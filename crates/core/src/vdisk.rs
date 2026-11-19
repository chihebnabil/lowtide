//! WSL and Docker Desktop virtual disks (`.vhdx`). They grow as Linux writes
//! data but never shrink on their own, so they're often the biggest hidden
//! space hog on a dev machine. Free builds only measure them.

use std::path::PathBuf;

use serde::Serialize;

use crate::fsutil::{self, env_path};

#[derive(Debug, Clone, Serialize)]
pub struct VirtualDisk {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
}

pub fn find() -> Vec<VirtualDisk> {
    let Some(local) = env_path("LOCALAPPDATA") else { return Vec::new() };
    let candidates = [
        (r"Packages\*\LocalState", "ext4.vhdx"),
        (r"wsl\*", "ext4.vhdx"),
        (r"Docker\wsl\data", "ext4.vhdx"),
        (r"Docker\wsl\main", "ext4.vhdx"),
        (r"Docker\wsl\disk", "docker_data.vhdx"),
    ];
    let mut disks = Vec::new();
    for (dir, file) in candidates {
        for d in fsutil::expand(&local.join(dir)) {
            let path = d.join(file);
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            disks.push(VirtualDisk { name: label(&path), path, bytes: meta.len() });
        }
    }
    disks.sort_by_key(|d| std::cmp::Reverse(d.bytes));
    disks
}

/// "CanonicalGroupLimited.Ubuntu22.04LTS_79rhkp1fndgsc" → "Ubuntu22.04LTS".
fn label(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    if s.contains(r"\Docker\") {
        return "Docker Desktop".into();
    }
    let pkg = path
        .ancestors()
        .find_map(|a| {
            let parent = a.parent()?;
            parent.file_name().filter(|n| n.eq_ignore_ascii_case("Packages")).map(|_| a)
        })
        .and_then(|a| a.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    match pkg {
        Some(p) => {
            let p = p.split('_').next().unwrap_or(&p);
            p.rsplit('.').next().unwrap_or(p).to_string()
        }
        None => "WSL distro".into(),
    }
}
