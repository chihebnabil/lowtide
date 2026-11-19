//! Filesystem primitives: sizing, deleting, path expansion and safety guards.
//!
//! Rules that every walk here follows:
//! - Never follow symlinks or junctions (pnpm, OneDrive and npm links use them).
//!   A link is removed as a link; its target is never touched or counted.
//! - Locked / in-use files are skipped and counted, never fatal.

use std::fs::{self, Metadata};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Usage {
    pub bytes: u64,
    pub files: u64,
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, o: Self) {
        self.bytes += o.bytes;
        self.files += o.files;
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct CleanStats {
    pub freed: u64,
    pub deleted: u64,
    /// Files that could not be removed (in use, access denied...).
    pub skipped: u64,
    /// Bytes moved to the Recycle Bin: off your projects, but not freed until the bin is emptied.
    pub recycled: u64,
}

impl std::ops::AddAssign for CleanStats {
    fn add_assign(&mut self, o: Self) {
        self.freed += o.freed;
        self.deleted += o.deleted;
        self.skipped += o.skipped;
        self.recycled += o.recycled;
    }
}

#[cfg(windows)]
fn attrs(m: &Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes()
}

#[cfg(not(windows))]
fn attrs(m: &Metadata) -> u32 {
    let mut a = 0;
    if m.is_dir() {
        a |= FILE_ATTRIBUTE_DIRECTORY;
    }
    if m.file_type().is_symlink() {
        a |= FILE_ATTRIBUTE_REPARSE_POINT;
    }
    a
}

pub fn is_link(m: &Metadata) -> bool {
    m.file_type().is_symlink() || attrs(m) & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn is_old_enough(m: &Metadata, cutoff: Option<SystemTime>) -> bool {
    match cutoff {
        None => true,
        Some(c) => m.modified().map(|t| t <= c).unwrap_or(true),
    }
}

pub fn cutoff_for(min_age: Option<Duration>) -> Option<SystemTime> {
    min_age.and_then(|age| SystemTime::now().checked_sub(age))
}

/// Total size of the files under `path` that would be deleted with the given cutoff.
/// Returns `PermissionDenied` only if the root itself cannot be listed.
pub fn dir_usage(path: &Path, cutoff: Option<SystemTime>) -> io::Result<Usage> {
    let mut usage = Usage::default();
    let entries = fs::read_dir(path)?;
    for entry in entries.flatten() {
        let Ok(meta) = fs::symlink_metadata(entry.path()) else { continue };
        if is_link(&meta) {
            continue;
        }
        if meta.is_dir() {
            if let Ok(u) = dir_usage(&entry.path(), cutoff) {
                usage += u;
            }
        } else if is_old_enough(&meta, cutoff) {
            usage += Usage { bytes: meta.len(), files: 1 };
        }
    }
    Ok(usage)
}

/// Delete the contents of `path` (and `path` itself when `remove_self`).
/// Files newer than `cutoff` are kept. Returns true if the directory ended up removed.
pub fn purge(path: &Path, cutoff: Option<SystemTime>, remove_self: bool, stats: &mut CleanStats) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        stats.skipped += 1;
        return false;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        let Ok(meta) = fs::symlink_metadata(&child) else { continue };

        if is_link(&meta) {
            if !is_old_enough(&meta, cutoff) {
                continue;
            }
            // Remove the link itself, never what it points to.
            let res = if attrs(&meta) & FILE_ATTRIBUTE_DIRECTORY != 0 {
                fs::remove_dir(&child)
            } else {
                fs::remove_file(&child)
            };
            match res {
                Ok(()) => stats.deleted += 1,
                Err(_) => stats.skipped += 1,
            }
        } else if meta.is_dir() {
            let old = is_old_enough(&meta, cutoff);
            purge(&child, cutoff, old, stats);
        } else if is_old_enough(&meta, cutoff) {
            match remove_file(&child, &meta) {
                Ok(()) => {
                    stats.freed += meta.len();
                    stats.deleted += 1;
                }
                Err(_) => stats.skipped += 1,
            }
        }
    }
    // Only succeeds if the directory is empty, which is exactly what we want.
    remove_self && fs::remove_dir(path).is_ok()
}

fn remove_file(path: &Path, meta: &Metadata) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied && attrs(meta) & FILE_ATTRIBUTE_READONLY != 0 => {
            // Read-only files (git objects, Go module cache) need the flag cleared first.
            let mut perms = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            fs::set_permissions(path, perms)?;
            fs::remove_file(path)
        }
        other => other,
    }
}

/// Expand a path pattern where any component may contain `*` wildcards,
/// e.g. `C:\Users\me\AppData\Local\Google\Chrome\User Data\*\Cache`.
/// Only existing directories are returned. Links are never matched by wildcards.
pub fn expand(pattern: &Path) -> Vec<PathBuf> {
    let mut current = vec![PathBuf::new()];
    for comp in pattern.components() {
        let part = comp.as_os_str().to_string_lossy();
        if part.contains('*') {
            let mut next = Vec::new();
            for base in &current {
                let Ok(rd) = fs::read_dir(base) else { continue };
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
                    if m.is_dir() && !is_link(&m) && wildcard_match(&part, &name) {
                        next.push(e.path());
                    }
                }
            }
            current = next;
        } else {
            for p in &mut current {
                p.push(comp.as_os_str());
            }
        }
    }
    current.retain(|p| p.is_dir());
    current.sort();
    current
}

/// Case-insensitive glob match supporting only `*`.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Last line of defence against a misconfigured environment variable
/// (e.g. `TEMP=C:\`) turning a cache clean into a disaster.
pub fn is_safe_to_clean(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let depth = path.components().filter(|c| matches!(c, Component::Normal(_))).count();
    if depth < 2 {
        return false;
    }
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    let target = norm(path);
    let protected = ["USERPROFILE", "SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramData", "APPDATA", "LOCALAPPDATA", "OneDrive"];
    for var in protected {
        if let Some(v) = std::env::var_os(var) {
            let p = norm(Path::new(&v));
            // Refuse the protected dir itself or any of its ancestors.
            if !p.is_empty() && (p == target || p.starts_with(&format!("{target}\\"))) {
                return false;
            }
        }
    }
    true
}

pub fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

pub fn age(t: SystemTime) -> String {
    let secs = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    let days = secs / 86_400;
    match days {
        0 => "today".into(),
        1 => "1 day ago".into(),
        2..=59 => format!("{days} days ago"),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard() {
        assert!(wildcard_match("thumbcache_*", "thumbcache_256.db"));
        assert!(wildcard_match("*", "Profile 1"));
        assert!(wildcard_match("Profile*", "profile 2"));
        assert!(!wildcard_match("Profile*", "Default"));
    }

    #[test]
    fn guards() {
        assert!(!is_safe_to_clean(Path::new("C:\\")));
        assert!(!is_safe_to_clean(Path::new("C:\\Users")));
        assert!(!is_safe_to_clean(Path::new("relative\\path")));
        if let Some(home) = env_path("USERPROFILE") {
            assert!(!is_safe_to_clean(&home));
            assert!(is_safe_to_clean(&home.join("AppData\\Local\\npm-cache")));
        }
    }

    #[test]
    fn purge_respects_links_and_cutoff() {
        let root = std::env::temp_dir().join(format!("lowtide-test-{}", std::process::id()));
        let keep = root.join("outside");
        let junk = root.join("junk");
        fs::create_dir_all(junk.join("nested")).unwrap();
        fs::create_dir_all(&keep).unwrap();
        fs::write(keep.join("precious.txt"), b"keep me").unwrap();
        fs::write(junk.join("a.bin"), vec![0u8; 1000]).unwrap();
        fs::write(junk.join("nested").join("b.bin"), vec![0u8; 500]).unwrap();
        // A directory symlink pointing outside; may fail without dev mode, that's fine.
        #[cfg(windows)]
        let _ = std::os::windows::fs::symlink_dir(&keep, junk.join("link"));

        // Fresh files with a 1-day cutoff are kept.
        let mut s = CleanStats::default();
        purge(&junk, cutoff_for(Some(Duration::from_secs(86_400))), false, &mut s);
        assert_eq!(s.deleted, 0);
        assert_eq!(dir_usage(&junk, None).unwrap().bytes, 1500);

        let mut s = CleanStats::default();
        assert!(purge(&junk, None, true, &mut s));
        assert_eq!(s.freed, 1500);
        assert!(!junk.exists());
        assert!(keep.join("precious.txt").exists(), "link target must survive");
        fs::remove_dir_all(&root).unwrap();
    }
}
