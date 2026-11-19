mod tui;

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use clap::{Parser, Subcommand};
use rayon::prelude::*;
use ratatui::crossterm::style::Stylize;

use lowtide_core::{fsutil, projects, settings::Settings, targets, shell};
use fsutil::{CleanStats, human};
use targets::{Measure, Risk, Target};

/// Lowtide: see what your dev tools left behind, and wash it away.
/// Dev caches, build artifacts and temp files on Windows.
///
/// Run with no arguments for the interactive UI.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Folder the Projects tab scans for node_modules, target, .next, venvs... [default: your home folder]
    #[arg(long)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show how much space every known cache uses (read-only)
    Scan,
    /// Clean caches. Defaults to the "safe" ones; asks before deleting
    Clean {
        /// Include caches that must be re-downloaded later (models, browsers, package stores)
        #[arg(long)]
        all: bool,
        /// Only clean these target ids (see `lowtide scan`), e.g. --only npm,uv,temp
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
    /// Find build artifacts in your projects (node_modules, target, .next, venv...)
    Projects {
        /// Folder to scan [default: your home folder]
        path: Option<PathBuf>,
        /// Only list artifacts not rebuilt for this many days
        #[arg(long, default_value_t = 0)]
        older_than: u64,
        /// Delete what was found (asks first unless --yes)
        #[arg(long)]
        clean: bool,
        /// Move folders to the Recycle Bin instead of deleting them
        #[arg(long)]
        recycle: bool,
        #[arg(short, long)]
        yes: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let settings = Settings::load();
    let home = settings.projects_root();
    match cli.cmd {
        None => tui::run(cli.root.unwrap_or(home), settings.recycle_projects),
        Some(Cmd::Scan) => {
            scan_caches();
            Ok(())
        }
        Some(Cmd::Clean { all, only, yes }) => clean_caches(all, &only, yes),
        Some(Cmd::Projects { path, older_than, clean, recycle, yes }) => {
            projects(path.unwrap_or(home), older_than, clean, recycle || settings.recycle_projects, yes)
        }
    }
}

fn risk_badge(r: Risk) -> String {
    let s = format!("{:<11}", r.label());
    match r {
        Risk::Safe => s.green().to_string(),
        Risk::Redownload => s.yellow().to_string(),
        Risk::Review => s.red().to_string(),
    }
}

fn measure_all() -> Vec<(Target, Measure)> {
    eprint!("{}", "Scanning caches...\r".dim());
    let mut rows: Vec<_> = targets::catalog().into_par_iter().map(|t| {
        let m = t.measure();
        (t, m)
    }).collect();
    eprint!("{}\r", " ".repeat(20));
    let size = |m: &Measure| match m {
        Measure::Ready(u) => u.bytes,
        Measure::NeedsAdmin => 0,
    };
    rows.sort_by_key(|(_, m)| std::cmp::Reverse(size(m)));
    rows
}

fn scan_caches() {
    let rows = measure_all();
    let mut total = 0;
    let mut admin = 0;
    println!("{}", format!("  {:<18} {:<32} {:<12} {:<11} {:>10}", "ID", "NAME", "CATEGORY", "RISK", "SIZE").bold());
    for (t, m) in &rows {
        let size = match m {
            Measure::Ready(u) if u.bytes == 0 => continue,
            Measure::Ready(u) => {
                total += u.bytes;
                human(u.bytes)
            }
            Measure::NeedsAdmin => {
                admin += 1;
                "admin".into()
            }
        };
        println!("  {} {:<32} {:<12} {} {:>10}", format!("{:<18}", t.id).cyan(), t.name, t.category, risk_badge(t.risk), size);
    }
    println!("\n  {} {}", "Washed up on this PC:".bold(), human(total).green().bold());
    if admin > 0 && !shell::is_elevated() {
        println!("  {}", format!("{admin} location(s) need an elevated terminal to scan.").dim());
    }
    println!("  {}", "Clean with `lowtide clean` (safe only) or `lowtide` for the interactive UI.".dim());
}

fn confirm(prompt: &str) -> bool {
    print!("{prompt} [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().lock().read_line(&mut line);
    matches!(line.trim(), "y" | "Y" | "yes")
}

fn clean_caches(all: bool, only: &[String], yes: bool) -> anyhow::Result<()> {
    let rows = measure_all();
    if let Some(bad) = only.iter().find(|id| !rows.iter().any(|(t, _)| &t.id == *id)) {
        anyhow::bail!("unknown or not-present target `{bad}` (see `lowtide scan`)");
    }
    let picked: Vec<_> = rows
        .into_iter()
        .filter_map(|(t, m)| match m {
            Measure::Ready(u) if u.bytes > 0 => Some((t, u)),
            _ => None,
        })
        .filter(|(t, _)| {
            if only.is_empty() { t.risk == Risk::Safe || (all && t.risk == Risk::Redownload) } else { only.contains(&t.id) }
        })
        .collect();
    if picked.is_empty() {
        println!("Nothing to clean.");
        return Ok(());
    }
    let total: u64 = picked.iter().map(|(_, u)| u.bytes).sum();
    for (t, u) in &picked {
        println!("  {:<32} {} {:>10}", t.name, risk_badge(t.risk), human(u.bytes));
    }
    if !yes && !confirm(&format!("\nDelete {} from {} location(s)?", human(total), picked.len())) {
        println!("Cancelled.");
        return Ok(());
    }
    let done = Mutex::new(CleanStats::default());
    picked.par_iter().for_each(|(t, _)| {
        let s = t.clean();
        println!("  {} {:<32} {}", "✓".green(), t.name, human(s.freed));
        *done.lock().unwrap() += s;
    });
    report(done.into_inner().unwrap());
    Ok(())
}

fn report(s: CleanStats) {
    println!("\n  {} {}", "Freed".bold(), human(s.freed).green().bold());
    if s.skipped > 0 {
        println!("  {}", format!("{} item(s) skipped because they are in use. Close apps and re-run to get them.", s.skipped).dim());
    }
}

fn projects(root: PathBuf, older_than: u64, clean: bool, recycle: bool, yes: bool) -> anyhow::Result<()> {
    eprintln!("{}", format!("Scanning {} ...", root.display()).dim());
    let found = Mutex::new(Vec::new());
    projects::scan(&root, |a| {
        if a.age_days() >= older_than {
            found.lock().unwrap().push(a);
        }
    });
    let mut found = found.into_inner().unwrap();
    found.sort_by_key(|a| std::cmp::Reverse(a.usage.bytes));
    if found.is_empty() {
        println!("No build artifacts found.");
        return Ok(());
    }
    for a in &found {
        let when = a.modified.map(fsutil::age).unwrap_or_default();
        println!("  {}  {:<20} {} {}", format!("{:>10}", human(a.usage.bytes)).bold(), a.kind, format!("{when:<14}").dim(), a.path.display());
    }
    let total: u64 = found.iter().map(|a| a.usage.bytes).sum();
    println!("\n  {} {} in {} folder(s)", "Total".bold(), human(total).green().bold(), found.len());
    if !clean {
        println!("  {}", "Add --clean to delete them, or use --older-than 30 to target stale projects only.".dim());
        return Ok(());
    }
    if !yes && !confirm(&format!("\nDelete {} folder(s), {}?", found.len(), human(total))) {
        println!("Cancelled.");
        return Ok(());
    }
    let done = Mutex::new(CleanStats::default());
    found.par_iter().for_each(|a| {
        let s = projects::remove(a, recycle);
        *done.lock().unwrap() += s;
    });
    report(done.into_inner().unwrap());
    Ok(())
}
