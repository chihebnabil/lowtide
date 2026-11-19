//! Finds per-project build artifacts (node_modules, target, .next, venvs...)
//! under a root folder, so stale projects can be slimmed down.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::SystemTime;

use rayon::iter::{ParallelBridge, ParallelIterator};

use crate::fsutil::{self, CleanStats, Usage};

struct Rule {
    dir: &'static str,
    kind: &'static str,
    /// The parent must contain one of these for the folder to count. `*.ext` matches by extension.
    /// Empty means the folder name alone is unambiguous.
    markers: &'static [&'static str],
    restore: &'static str,
}

const NODE: &[&str] = &["package.json"];
const GRADLE: &[&str] = &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"];
const DOTNET: &[&str] = &["*.csproj", "*.fsproj", "*.vbproj", "*.sln"];

const RULES: &[Rule] = &[
    Rule { dir: "node_modules", kind: "node_modules", markers: NODE, restore: "npm / pnpm / yarn / bun install" },
    Rule { dir: "target", kind: "Rust target", markers: &["Cargo.toml"], restore: "cargo build" },
    Rule { dir: ".next", kind: "Next.js build", markers: NODE, restore: "next dev / next build" },
    Rule { dir: ".nuxt", kind: "Nuxt build", markers: NODE, restore: "nuxt dev / nuxt build" },
    Rule { dir: ".svelte-kit", kind: "SvelteKit build", markers: NODE, restore: "vite dev / vite build" },
    Rule { dir: ".turbo", kind: "Turborepo cache", markers: NODE, restore: "turbo run build" },
    Rule { dir: ".parcel-cache", kind: "Parcel cache", markers: NODE, restore: "parcel build" },
    Rule { dir: ".angular", kind: "Angular cache", markers: &["angular.json"], restore: "ng build" },
    Rule { dir: ".docusaurus", kind: "Docusaurus cache", markers: NODE, restore: "docusaurus start" },
    Rule { dir: ".gradle", kind: "Gradle project cache", markers: GRADLE, restore: "gradle build" },
    Rule { dir: "build", kind: "Gradle build", markers: GRADLE, restore: "gradle build" },
    Rule { dir: "build", kind: "Flutter build", markers: &["pubspec.yaml"], restore: "flutter build" },
    Rule { dir: ".dart_tool", kind: "Dart tool cache", markers: &["pubspec.yaml"], restore: "dart pub get" },
    Rule { dir: "bin", kind: ".NET bin", markers: DOTNET, restore: "dotnet build" },
    Rule { dir: "obj", kind: ".NET obj", markers: DOTNET, restore: "dotnet build" },
    Rule { dir: ".terraform", kind: "Terraform providers", markers: &["*.tf"], restore: "terraform init" },
    Rule { dir: ".zig-cache", kind: "Zig cache", markers: &["build.zig"], restore: "zig build" },
    Rule { dir: "zig-out", kind: "Zig output", markers: &["build.zig"], restore: "zig build" },
    Rule { dir: ".mypy_cache", kind: "mypy cache", markers: &[], restore: "runs again automatically" },
    Rule { dir: ".pytest_cache", kind: "pytest cache", markers: &[], restore: "runs again automatically" },
    Rule { dir: ".ruff_cache", kind: "Ruff cache", markers: &[], restore: "runs again automatically" },
    Rule { dir: ".tox", kind: "tox envs", markers: &[], restore: "tox" },
];

/// Folders never worth walking into when looking for projects.
const SKIP: &[&str] = &[
    "AppData", "$Recycle.Bin", "System Volume Information", "Windows", "Program Files",
    "Program Files (x86)", "ProgramData", "scoop", "site-packages",
];

#[derive(Debug, Clone)]
pub struct Artifact {
    pub path: PathBuf,
    pub kind: &'static str,
    pub restore: &'static str,
    pub usage: Usage,
    /// Last time the artifact folder changed: roughly the last install/build.
    pub modified: Option<SystemTime>,
}

impl Artifact {
    pub fn project(&self) -> &Path {
        self.path.parent().unwrap_or(&self.path)
    }

    pub fn age_days(&self) -> u64 {
        self.modified
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .map(|d| d.as_secs() / 86_400)
            .unwrap_or(0)
    }
}

/// Remove an artifact folder, either for good or into the Recycle Bin.
pub fn remove(a: &Artifact, to_recycle_bin: bool) -> CleanStats {
    let mut stats = CleanStats::default();
    if !fsutil::is_safe_to_clean(&a.path) {
        stats.skipped += 1;
    } else if to_recycle_bin {
        match trash::delete(&a.path) {
            Ok(()) => {
                stats.recycled = a.usage.bytes;
                stats.deleted = a.usage.files;
            }
            Err(_) => stats.skipped += 1,
        }
    } else {
        fsutil::purge(&a.path, None, true, &mut stats);
    }
    stats
}

fn has_marker(names: &[String], markers: &[&str]) -> bool {
    markers.is_empty()
        || markers.iter().any(|m| match m.strip_prefix('*') {
            Some(ext) => names.iter().any(|n| n.to_lowercase().ends_with(ext)),
            None => names.iter().any(|n| n.eq_ignore_ascii_case(m)),
        })
}

/// Walks `root`, calling `found` for each artifact (sized in parallel) as soon as it's known.
pub fn scan(root: &Path, found: impl Fn(Artifact) + Sync) {
    let (tx, rx) = mpsc::channel::<(PathBuf, &'static Rule)>();
    let root = root.to_path_buf();
    let walker = std::thread::spawn(move || walk(&root, &tx));

    rx.into_iter().par_bridge().for_each(|(path, rule)| {
        let usage = fsutil::dir_usage(&path, None).unwrap_or_default();
        if usage.bytes == 0 {
            return;
        }
        let modified = fs::symlink_metadata(&path).and_then(|m| m.modified()).ok();
        found(Artifact { path, kind: rule.kind, restore: rule.restore, usage, modified });
    });
    let _ = walker.join();
}

fn walk(root: &Path, tx: &mpsc::Sender<(PathBuf, &'static Rule)>) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        let mut names = Vec::new();
        let mut subdirs = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(meta) = fs::symlink_metadata(e.path()) else { continue };
            if meta.is_dir() && !fsutil::is_link(&meta) {
                subdirs.push(name.clone());
            }
            names.push(name);
        }

        for name in subdirs {
            let path = dir.join(&name);
            if let Some(rule) = RULES.iter().find(|r| r.dir.eq_ignore_ascii_case(&name) && has_marker(&names, r.markers)) {
                let _ = tx.send((path, rule));
                continue;
            }
            if path.join("pyvenv.cfg").is_file() {
                let _ = tx.send((path, &VENV));
                continue;
            }
            let skip = name.starts_with('.')
                || name.eq_ignore_ascii_case("node_modules")
                || SKIP.iter().any(|s| s.eq_ignore_ascii_case(&name));
            if !skip {
                stack.push(path);
            }
        }
    }
}

static VENV: Rule = Rule { dir: "", kind: "Python venv", markers: &[], restore: "python -m venv + pip install / uv sync" };

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn finds_artifacts_only_next_to_markers() {
        let root = std::env::temp_dir().join(format!("lowtide-proj-{}", std::process::id()));
        let web = root.join("web");
        let lonely = root.join("notes");
        fs::create_dir_all(web.join("node_modules/react/node_modules/x")).unwrap();
        fs::write(web.join("package.json"), "{}").unwrap();
        fs::write(web.join("node_modules/react/index.js"), "x".repeat(100)).unwrap();
        // A folder named `target` without Cargo.toml must be ignored.
        fs::create_dir_all(lonely.join("target")).unwrap();
        fs::write(lonely.join("target/important.txt"), "data").unwrap();
        fs::create_dir_all(root.join("py/.venv")).unwrap();
        fs::write(root.join("py/.venv/pyvenv.cfg"), "home = x").unwrap();

        let found = Mutex::new(Vec::new());
        scan(&root, |a| found.lock().unwrap().push((a.kind, a.path)));
        let mut found = found.into_inner().unwrap();
        found.sort();
        assert_eq!(found, vec![("Python venv", root.join("py/.venv")), ("node_modules", web.join("node_modules"))]);
        fs::remove_dir_all(&root).unwrap();
    }
}
