//! User preferences shared by the CLI and the desktop app,
//! stored in `%APPDATA%\lowtide\settings.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::fsutil::env_path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Folder scanned for project build artifacts. `None` means the home folder.
    pub projects_root: Option<PathBuf>,
    /// Move project folders to the Recycle Bin instead of deleting them.
    pub recycle_projects: bool,
}

fn file() -> Option<PathBuf> {
    env_path("APPDATA").map(|p| p.join("Lowtide").join("settings.json"))
}

impl Settings {
    pub fn load() -> Self {
        file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(f) = file() else { return Ok(()) };
        if let Some(dir) = f.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(f, serde_json::to_string_pretty(self).expect("settings serialize"))
    }

    pub fn projects_root(&self) -> PathBuf {
        self.projects_root
            .clone()
            .filter(|p| p.is_dir())
            .or_else(|| env_path("USERPROFILE"))
            .unwrap_or_else(|| PathBuf::from("."))
    }
}
