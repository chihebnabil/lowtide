//! lowtide desktop app: a thin Tauri shell over `lowtide-core`.
//! Long work runs on background threads and streams results to the UI as events.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use lowtide_core::settings::Settings;
use lowtide_core::{Artifact, CleanStats, Measure, Risk, Target, projects, shell, targets};

#[derive(Default)]
struct AppState {
    targets: Mutex<Vec<Target>>,
    /// Artifacts found by the latest project scan. The UI may only delete these.
    artifacts: Mutex<HashMap<PathBuf, Artifact>>,
    project_scan: AtomicU64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TargetDto {
    id: String,
    name: String,
    category: &'static str,
    risk: Risk,
    what: &'static str,
    after: &'static str,
    paths: Vec<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MeasureDto {
    id: String,
    needs_admin: bool,
    bytes: u64,
    files: u64,
}

impl MeasureDto {
    fn new(id: &str, m: Measure) -> Self {
        match m {
            Measure::Ready(u) => MeasureDto { id: id.into(), needs_admin: false, bytes: u.bytes, files: u.files },
            Measure::NeedsAdmin => MeasureDto { id: id.into(), needs_admin: true, bytes: 0, files: 0 },
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CacheCleaned {
    stats: CleanStats,
    after: MeasureDto,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ArtifactDto {
    path: String,
    project: String,
    name: String,
    kind: &'static str,
    restore: &'static str,
    bytes: u64,
    files: u64,
    modified_ms: Option<u64>,
    age_days: u64,
}

impl From<&Artifact> for ArtifactDto {
    fn from(a: &Artifact) -> Self {
        let project = a.project();
        ArtifactDto {
            path: a.path.display().to_string(),
            project: project.display().to_string(),
            name: project.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            kind: a.kind,
            restore: a.restore,
            bytes: a.usage.bytes,
            files: a.usage.files,
            modified_ms: a.modified.and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64),
            age_days: a.age_days(),
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProjectCleaned {
    path: String,
    stats: CleanStats,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Info {
    version: &'static str,
    elevated: bool,
    /// False in the Microsoft Store (MSIX) build, which must not elevate.
    can_elevate: bool,
    projects_root: String,
    recycle_projects: bool,
}

#[tauri::command]
fn get_info() -> Info {
    let s = Settings::load();
    Info {
        version: env!("CARGO_PKG_VERSION"),
        elevated: shell::is_elevated(),
        can_elevate: !shell::is_packaged(),
        projects_root: s.projects_root().display().to_string(),
        recycle_projects: s.recycle_projects,
    }
}

/// Returns the catalog immediately and measures every target in the background,
/// emitting `cache:measured` for each one.
#[tauri::command]
fn scan_caches(app: AppHandle, state: State<AppState>) -> Vec<TargetDto> {
    let catalog = targets::catalog();
    let dtos = catalog
        .iter()
        .map(|t| TargetDto {
            id: t.id.clone(),
            name: t.name.clone(),
            category: t.category,
            risk: t.risk,
            what: t.what,
            after: t.after,
            paths: t.paths().iter().map(|p| p.display().to_string()).collect(),
        })
        .collect();
    *state.targets.lock().unwrap() = catalog.clone();
    std::thread::spawn(move || {
        catalog.into_par_iter().for_each(|t| {
            let _ = app.emit("cache:measured", MeasureDto::new(&t.id, t.measure()));
        });
        let _ = app.emit("cache:done", ());
    });
    dtos
}

#[tauri::command]
fn clean_caches(app: AppHandle, state: State<AppState>, ids: Vec<String>) -> usize {
    let picked: Vec<Target> = state.targets.lock().unwrap().iter().filter(|t| ids.contains(&t.id)).cloned().collect();
    let n = picked.len();
    for t in picked {
        let app = app.clone();
        rayon::spawn(move || {
            let stats = t.clean();
            let after = MeasureDto::new(&t.id, t.measure());
            let _ = app.emit("cache:cleaned", CacheCleaned { stats, after });
        });
    }
    n
}

/// Starts a project scan; emits `project:found` per artifact and `projects:done` at the end.
#[tauri::command]
fn scan_projects(app: AppHandle, state: State<AppState>, root: String) -> u64 {
    let scan_id = state.project_scan.fetch_add(1, Ordering::SeqCst) + 1;
    state.artifacts.lock().unwrap().clear();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        projects::scan(&PathBuf::from(root), |a| {
            // A newer scan was started: drop results from this one.
            if state.project_scan.load(Ordering::SeqCst) != scan_id {
                return;
            }
            let dto = ArtifactDto::from(&a);
            state.artifacts.lock().unwrap().insert(a.path.clone(), a);
            let _ = app.emit("project:found", dto);
        });
        let _ = app.emit("projects:done", scan_id);
    });
    scan_id
}

#[tauri::command]
fn clean_projects(app: AppHandle, state: State<AppState>, paths: Vec<String>, recycle: bool) -> usize {
    let picked: Vec<Artifact> = {
        let mut known = state.artifacts.lock().unwrap();
        paths.iter().filter_map(|p| known.remove(&PathBuf::from(p))).collect()
    };
    let n = picked.len();
    for a in picked {
        let app = app.clone();
        rayon::spawn(move || {
            let stats = projects::remove(&a, recycle);
            let _ = app.emit("project:cleaned", ProjectCleaned { path: a.path.display().to_string(), stats });
        });
    }
    n
}

#[tauri::command]
async fn pick_folder(app: AppHandle, current: String) -> Option<String> {
    app.dialog()
        .file()
        .set_directory(current)
        .set_title("Folder with your projects")
        .blocking_pick_folder()
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string())
}

#[tauri::command]
fn save_settings(projects_root: String, recycle_projects: bool) -> Result<(), String> {
    let s = Settings { projects_root: Some(PathBuf::from(projects_root)), recycle_projects };
    s.save().map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Disk {
    drive: String,
    free: u64,
    total: u64,
}

/// Free/total space of the system drive (usually C:).
#[tauri::command]
fn disk_space() -> Option<Disk> {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let (free, total) = shell::disk_space(&PathBuf::from(format!("{drive}\\")))?;
    Some(Disk { drive, free, total })
}

#[tauri::command]
async fn virtual_disks() -> Vec<lowtide_core::vdisk::VirtualDisk> {
    lowtide_core::vdisk::find()
}

#[tauri::command]
fn reveal(path: String) -> bool {
    shell::reveal(&PathBuf::from(path))
}

#[tauri::command]
fn open_url(url: String) -> bool {
    shell::open_url(&url)
}

#[tauri::command]
fn relaunch_as_admin(app: AppHandle) -> bool {
    let ok = shell::relaunch_elevated("");
    if ok {
        app.exit(0);
    }
    ok
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_info,
            scan_caches,
            clean_caches,
            scan_projects,
            clean_projects,
            pick_folder,
            save_settings,
            reveal,
            open_url,
            disk_space,
            virtual_disks,
            relaunch_as_admin,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start lowtide");
}
