//! The catalog of known junk locations. Each target explains itself in plain
//! language so people who don't know what `%LOCALAPPDATA%\uv\cache` is can still
//! make an informed decision.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::fsutil::{self, CleanStats, Usage, env_path};
use crate::recycle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    /// Rebuilt automatically, nothing to download.
    Safe,
    /// Comes back on demand, but has to be downloaded again.
    Redownload,
    /// Permanent: look before you delete.
    Review,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::Safe => "safe",
            Risk::Redownload => "re-download",
            Risk::Review => "review",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Kind {
    /// Delete the contents of these directories, keeping the directories themselves.
    Dirs { paths: Vec<PathBuf>, min_age: Option<Duration> },
    RecycleBin,
}

#[derive(Debug, Clone)]
pub struct Target {
    pub id: String,
    pub name: String,
    pub category: &'static str,
    pub risk: Risk,
    pub what: &'static str,
    pub after: &'static str,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy)]
pub enum Measure {
    Ready(Usage),
    NeedsAdmin,
}

impl Target {
    pub fn paths(&self) -> &[PathBuf] {
        match &self.kind {
            Kind::Dirs { paths, .. } => paths,
            Kind::RecycleBin => &[],
        }
    }

    pub fn measure(&self) -> Measure {
        match &self.kind {
            Kind::RecycleBin => Measure::Ready(recycle::query().unwrap_or_default()),
            Kind::Dirs { paths, min_age } => {
                let cutoff = fsutil::cutoff_for(*min_age);
                let mut total = Usage::default();
                let mut denied = 0;
                for p in paths {
                    match fsutil::dir_usage(p, cutoff) {
                        Ok(u) => total += u,
                        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => denied += 1,
                        Err(_) => {}
                    }
                }
                if denied == paths.len() { Measure::NeedsAdmin } else { Measure::Ready(total) }
            }
        }
    }

    pub fn clean(&self) -> CleanStats {
        let mut stats = CleanStats::default();
        match &self.kind {
            Kind::RecycleBin => {
                let before = recycle::query().unwrap_or_default();
                if recycle::empty() {
                    stats.freed = before.bytes;
                    stats.deleted = before.files;
                }
            }
            Kind::Dirs { paths, min_age } => {
                let cutoff = fsutil::cutoff_for(*min_age);
                for p in paths.iter().filter(|p| fsutil::is_safe_to_clean(p)) {
                    fsutil::purge(p, cutoff, false, &mut stats);
                }
            }
        }
        stats
    }
}

struct Env {
    local: Option<PathBuf>,
    roaming: Option<PathBuf>,
    home: Option<PathBuf>,
    sysroot: Option<PathBuf>,
    progdata: Option<PathBuf>,
}

impl Env {
    fn under(base: &Option<PathBuf>, rel: &str) -> Option<PathBuf> {
        base.as_ref().map(|b| b.join(rel))
    }
    fn local(&self, rel: &str) -> Option<PathBuf> {
        Self::under(&self.local, rel)
    }
    fn roaming(&self, rel: &str) -> Option<PathBuf> {
        Self::under(&self.roaming, rel)
    }
    fn home(&self, rel: &str) -> Option<PathBuf> {
        Self::under(&self.home, rel)
    }
    fn sys(&self, rel: &str) -> Option<PathBuf> {
        Self::under(&self.sysroot, rel)
    }
    fn progdata(&self, rel: &str) -> Option<PathBuf> {
        Self::under(&self.progdata, rel)
    }
}

struct Builder {
    targets: Vec<Target>,
}

impl Builder {
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        id: &str,
        name: &str,
        category: &'static str,
        risk: Risk,
        what: &'static str,
        after: &'static str,
        min_age: Option<Duration>,
        patterns: impl IntoIterator<Item = Option<PathBuf>>,
    ) {
        let mut paths: Vec<PathBuf> = Vec::new();
        for pat in patterns.into_iter().flatten() {
            for p in fsutil::expand(&pat) {
                // %TEMP% and %LOCALAPPDATA%\Temp are usually the same folder.
                if !paths.iter().any(|q| q.as_os_str().eq_ignore_ascii_case(p.as_os_str())) {
                    paths.push(p);
                }
            }
        }
        paths.retain(|p| fsutil::is_safe_to_clean(p));
        if paths.is_empty() {
            return; // Tool not installed: don't clutter the list.
        }
        self.targets.push(Target {
            id: id.into(),
            name: name.into(),
            category,
            risk,
            what,
            after,
            kind: Kind::Dirs { paths, min_age },
        });
    }
}

const DAY: Duration = Duration::from_secs(86_400);

/// Chromium/Electron cache folders inside an app's data dir. Never cookies, logins or settings.
const CHROMIUM_CACHES: &[&str] = &[
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "GrShaderCache",
    "ShaderCache",
];

fn chromium(base: Option<PathBuf>, extra: &[&str]) -> Vec<Option<PathBuf>> {
    CHROMIUM_CACHES
        .iter()
        .chain(extra)
        .map(|sub| base.as_ref().map(|b| b.join(sub)))
        .collect()
}

pub fn catalog() -> Vec<Target> {
    let e = Env {
        local: env_path("LOCALAPPDATA"),
        roaming: env_path("APPDATA"),
        home: env_path("USERPROFILE"),
        sysroot: env_path("SystemRoot"),
        progdata: env_path("ProgramData"),
    };
    let cargo_home = env_path("CARGO_HOME").or_else(|| e.home(".cargo"));
    let gradle_home = env_path("GRADLE_USER_HOME").or_else(|| e.home(".gradle"));
    let hf_home = env_path("HF_HOME").or_else(|| e.home(".cache\\huggingface"));
    let go_mod = env_path("GOMODCACHE")
        .or_else(|| env_path("GOPATH").map(|p| p.join("pkg\\mod")))
        .or_else(|| e.home("go\\pkg\\mod"));
    let go_build = env_path("GOCACHE").or_else(|| e.local("go-build"));

    let mut b = Builder { targets: Vec::new() };
    use Risk::*;

    // ── System ────────────────────────────────────────────────────────────
    let temp = env_path("TEMP").filter(|p| {
        // Only trust %TEMP% if it actually looks like a temp folder.
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        name.contains("temp") || name.contains("tmp")
    });
    b.add("temp", "Your temp files", "System", Safe,
        "Scratch files apps leave behind: installers, extracted archives, build leftovers.",
        "Nothing. Files touched in the last 24h and files in use are left alone.",
        Some(DAY), [temp, e.local("Temp")]);
    b.add("windows-temp", "Windows temp files", "System", Safe,
        "System-wide scratch folder used by services and installers.",
        "Nothing. Files from the last 24h are kept. Needs admin.",
        Some(DAY), [e.sys("Temp")]);
    b.add("crash-dumps", "Crash dumps", "System", Safe,
        "Memory snapshots written when an app crashes. Often hundreds of MB each.",
        "Nothing, unless you were about to send them to a developer.",
        None, [e.local("CrashDumps")]);
    b.add("error-reports", "Windows error reports", "System", Safe,
        "Queued and archived crash reports from Windows Error Reporting.",
        "Nothing.",
        None, [
            e.local("Microsoft\\Windows\\WER\\ReportArchive"),
            e.local("Microsoft\\Windows\\WER\\ReportQueue"),
            e.progdata("Microsoft\\Windows\\WER\\ReportArchive"),
            e.progdata("Microsoft\\Windows\\WER\\ReportQueue"),
        ]);
    b.add("inet-cache", "Internet cache (legacy)", "System", Safe,
        "Web cache used by old IE components, Office and some Windows apps.",
        "Nothing. Refills as needed.",
        None, [e.local("Microsoft\\Windows\\INetCache")]);
    b.add("shader-cache", "GPU shader caches", "System", Safe,
        "Compiled shaders from DirectX, NVIDIA, AMD and Intel drivers.",
        "Games may stutter briefly the first time while shaders are rebuilt.",
        None, [
            e.local("D3DSCache"),
            e.local("NVIDIA\\DXCache"),
            e.local("NVIDIA\\GLCache"),
            e.local("AMD\\DxCache"),
            e.local("AMD\\DxcCache"),
            e.local("AMD\\VkCache"),
            e.local("Intel\\ShaderCache"),
        ]);
    b.add("windows-update", "Windows Update downloads", "System", Safe,
        "Update packages that were already installed.",
        "Nothing. Windows re-downloads only if an update is still pending. Needs admin.",
        None, [e.sys("SoftwareDistribution\\Download")]);
    b.add("delivery-opt", "Delivery Optimization cache", "System", Safe,
        "Update chunks Windows keeps to share with other PCs.",
        "Nothing. Needs admin.",
        None, [e.sys("ServiceProfiles\\NetworkService\\AppData\\Local\\Microsoft\\Windows\\DeliveryOptimization\\Cache")]);
    if cfg!(windows) {
        b.targets.push(Target {
            id: "recycle-bin".into(),
            name: "Recycle Bin".into(),
            category: "System",
            risk: Review,
            what: "Files you deleted, on every drive.",
            after: "They are gone for good. Peek inside first if unsure.",
            kind: Kind::RecycleBin,
        });
    }

    // ── JavaScript ────────────────────────────────────────────────────────
    b.add("npm", "npm cache", "JavaScript", Safe,
        "Every package tarball npm ever downloaded, plus npx's temporary installs.",
        "Next `npm install` fetches from the registry instead of the local cache.",
        None, [e.local("npm-cache")]);
    b.add("pnpm-store", "pnpm store", "JavaScript", Redownload,
        "pnpm's global content-addressable store that projects hard-link into.",
        "Existing node_modules keep working (hard links). New installs re-download.",
        None, [e.local("pnpm\\store")]);
    b.add("pnpm-cache", "pnpm metadata cache", "JavaScript", Safe,
        "Cached package metadata from the registry.",
        "Nothing. Refetched on next install.",
        None, [e.local("pnpm-cache")]);
    b.add("yarn", "Yarn cache", "JavaScript", Safe,
        "Package archives cached by Yarn v1 and Yarn Berry.",
        "Next `yarn install` downloads again.",
        None, [e.local("Yarn\\Cache"), e.local("Yarn\\Berry\\cache")]);
    b.add("bun", "Bun install cache", "JavaScript", Safe,
        "Packages cached by `bun install`.",
        "Next `bun install` downloads again.",
        None, [e.home(".bun\\install\\cache")]);
    b.add("deno", "Deno cache", "JavaScript", Redownload,
        "Remote modules and compiled code cached by Deno.",
        "Deno re-downloads dependencies on next run.",
        None, ["deps", "gen", "npm", "remote", "registries"].map(|s| e.local(&format!("deno\\{s}"))));
    b.add("corepack", "Corepack cache", "JavaScript", Redownload,
        "pnpm/yarn binaries downloaded by Corepack.",
        "Re-downloaded the next time a project asks for that version.",
        None, [e.local("node\\corepack")]);
    b.add("node-gyp", "node-gyp headers", "JavaScript", Redownload,
        "Node.js headers used to compile native addons.",
        "Re-downloaded next time a native module builds.",
        None, [e.local("node-gyp\\Cache")]);
    b.add("electron", "Electron downloads", "JavaScript", Redownload,
        "Electron binaries cached by electron and electron-builder.",
        "Re-downloaded on next install or build.",
        None, [e.local("electron\\Cache"), e.local("electron-builder\\Cache")]);
    b.add("playwright", "Playwright browsers", "JavaScript", Redownload,
        "Chromium, Firefox and WebKit builds for Playwright. Old versions pile up.",
        "Run `npx playwright install` to get them back.",
        None, [e.local("ms-playwright")]);
    b.add("puppeteer", "Puppeteer browsers", "JavaScript", Redownload,
        "Chrome builds downloaded by Puppeteer.",
        "Re-downloaded on next `npm install` of puppeteer.",
        None, [e.home(".cache\\puppeteer")]);
    b.add("cypress", "Cypress binaries", "JavaScript", Redownload,
        "Cypress app versions, one per version you ever installed.",
        "Run `npx cypress install` to get them back.",
        None, [e.local("Cypress\\Cache")]);

    b.add("typescript", "TypeScript typings cache", "JavaScript", Safe,
        "Type definitions VS Code downloads for JavaScript IntelliSense.",
        "Re-fetched in the background when you open a JS project.",
        None, [e.local("Microsoft\\TypeScript")]);
    b.add("selenium", "Selenium drivers", "JavaScript", Redownload,
        "Browser drivers and browsers fetched by Selenium Manager.",
        "Re-downloaded on the next test run.",
        None, [e.home(".cache\\selenium")]);

    // ── Python ────────────────────────────────────────────────────────────
    b.add("pip", "pip cache", "Python", Safe,
        "Wheels and downloads cached by pip.",
        "Next `pip install` downloads again.",
        None, [e.local("pip\\cache")]);
    b.add("uv", "uv cache", "Python", Safe,
        "Packages cached by uv. Grows fast.",
        "Next `uv sync` downloads again. Your Python installs are not touched.",
        None, [e.local("uv\\cache")]);
    b.add("poetry", "Poetry cache", "Python", Safe,
        "Packages and metadata cached by Poetry.",
        "Next `poetry install` downloads again.",
        None, [e.local("pypoetry\\Cache")]);
    b.add("pipenv", "Pipenv cache", "Python", Safe,
        "Packages cached by Pipenv.",
        "Next install downloads again.",
        None, [e.local("pipenv\\pipenv\\Cache")]);
    b.add("pre-commit", "pre-commit hook envs", "Python", Redownload,
        "Isolated environments pre-commit builds for each hook repo.",
        "Rebuilt the next time your git hooks run.",
        None, [e.home(".cache\\pre-commit")]);

    // ── AI / ML ───────────────────────────────────────────────────────────
    b.add("huggingface", "Hugging Face models & datasets", "AI / ML", Redownload,
        "Every model and dataset pulled by transformers, diffusers, datasets... Often tens of GB.",
        "Models re-download on next use. Your HF login token is NOT deleted.",
        None, [
            hf_home.as_ref().map(|h| h.join("hub")),
            hf_home.as_ref().map(|h| h.join("datasets")),
            hf_home.as_ref().map(|h| h.join("xet")),
        ]);
    b.add("torch", "PyTorch hub cache", "AI / ML", Redownload,
        "Pretrained weights downloaded through torch.hub and torchvision.",
        "Re-downloaded on next use.",
        None, [e.home(".cache\\torch")]);
    b.add("whisper", "Whisper models", "AI / ML", Redownload,
        "Speech-to-text models downloaded by openai-whisper.",
        "Re-downloaded on next use.",
        None, [e.home(".cache\\whisper")]);

    // ── Rust / Go / .NET / Java / PHP ─────────────────────────────────────
    b.add("cargo", "Cargo registry & git cache", "Rust", Redownload,
        "Downloaded and unpacked crates plus git dependencies.",
        "Next `cargo build` downloads what it needs. Installed binaries stay.",
        None, [
            cargo_home.as_ref().map(|c| c.join("registry\\cache")),
            cargo_home.as_ref().map(|c| c.join("registry\\src")),
            cargo_home.as_ref().map(|c| c.join("git\\checkouts")),
        ]);
    b.add("go-build", "Go build cache", "Go", Safe,
        "Compiled packages cached by `go build` and `go test`.",
        "Next build is slower once. Same as `go clean -cache`.",
        None, [go_build]);
    b.add("go-mod", "Go module cache", "Go", Redownload,
        "Source of every Go module you've depended on.",
        "Re-downloaded on next build. Same as `go clean -modcache`.",
        None, [go_mod]);
    b.add("nuget-http", "NuGet HTTP cache", "C# / .NET", Safe,
        "Cached NuGet API responses and package downloads.",
        "Nothing. Refetched on restore.",
        None, [e.local("NuGet\\v3-cache"), e.local("NuGet\\plugins-cache")]);
    b.add("nuget", "NuGet packages", "C# / .NET", Redownload,
        "Global folder of every NuGet package version you've restored.",
        "Next `dotnet restore` downloads again.",
        None, [e.home(".nuget\\packages")]);
    b.add("gradle", "Gradle caches", "Java", Redownload,
        "Dependencies, build caches and Gradle distributions. Android projects make this huge.",
        "Next build downloads again (slow the first time).",
        None, [
            gradle_home.as_ref().map(|g| g.join("caches")),
            gradle_home.as_ref().map(|g| g.join("wrapper\\dists")),
        ]);
    b.add("maven", "Maven repository", "Java", Redownload,
        "Every Maven artifact you've ever resolved.",
        "Next build downloads again.",
        None, [e.home(".m2\\repository")]);
    b.add("composer", "Composer cache", "PHP", Safe,
        "Packages cached by Composer.",
        "Next `composer install` downloads again.",
        None, [e.local("Composer")]);

    b.add("android", "Android build cache", "Java", Safe,
        "Build cache from the Android Gradle plugin and emulator temp files.",
        "Next Android build is slower once. Your SDK and emulators are not touched.",
        None, [e.home(".android\\cache"), e.home(".android\\build-cache")]);
    b.add("unity", "Unity cache", "Game dev", Redownload,
        "Packages and assets cached by Unity and the Asset Store.",
        "Unity re-downloads packages when a project needs them.",
        None, [e.local("Unity\\cache")]);

    // ── Editors ───────────────────────────────────────────────────────────
    for (id, name, dir) in [
        ("vscode", "VS Code cache", "Code"),
        ("vscode-insiders", "VS Code Insiders cache", "Code - Insiders"),
        ("cursor", "Cursor cache", "Cursor"),
        ("windsurf", "Windsurf cache", "Windsurf"),
    ] {
        b.add(id, name, "Editors", Safe,
            "Electron caches, compiled extension code, old extension installers and logs. \
             Settings, extensions and chat history are not touched.",
            "Editor starts a bit slower once. Close it first for a full clean.",
            None, chromium(e.roaming(dir), &["CachedData", "CachedExtensionVSIXs", "logs"]));
    }
    b.add("jetbrains", "JetBrains caches & indexes", "Editors", Safe,
        "Caches, indexes and logs for every IntelliJ/WebStorm/PyCharm/Rider version, including old ones.",
        "The IDE re-indexes your project on next open.",
        None, ["caches", "index", "log"].map(|s| e.local(&format!("JetBrains\\*\\{s}"))));

    // ── Browsers ──────────────────────────────────────────────────────────
    for (id, name, dir) in [
        ("chrome", "Chrome cache", "Google\\Chrome\\User Data"),
        ("edge", "Edge cache", "Microsoft\\Edge\\User Data"),
        ("brave", "Brave cache", "BraveSoftware\\Brave-Browser\\User Data"),
        ("vivaldi", "Vivaldi cache", "Vivaldi\\User Data"),
    ] {
        let mut pats = chromium(e.local(dir), &[]);
        pats.extend(chromium(e.local(&format!("{dir}\\*")), &[]));
        b.add(id, name, "Browsers", Safe,
            "Cached web pages, scripts and images for all profiles.",
            "Sites load a bit slower once. You stay logged in: cookies are not touched.",
            None, pats);
    }
    b.add("firefox", "Firefox cache", "Browsers", Safe,
        "Cached web pages, scripts and images for all profiles.",
        "Sites load a bit slower once. You stay logged in: cookies are not touched.",
        None, [e.local("Mozilla\\Firefox\\Profiles\\*\\cache2")]);

    // ── Apps ──────────────────────────────────────────────────────────────
    for (id, name, dir) in [
        ("discord", "Discord cache", "discord"),
        ("slack", "Slack cache", "Slack"),
        ("teams-classic", "Teams (classic) cache", "Microsoft\\Teams"),
        ("postman", "Postman cache", "Postman"),
        ("notion", "Notion cache", "Notion"),
        ("obsidian", "Obsidian cache", "obsidian"),
        ("claude-desktop", "Claude desktop cache", "Claude"),
    ] {
        b.add(id, name, "Apps", Safe,
            "Electron caches: images, scripts and GPU data. No messages, accounts or settings.",
            "App starts a bit slower once. Close it first for a full clean.",
            None, chromium(e.roaming(dir), &[]));
    }

    b.targets
}
