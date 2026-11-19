//! lowtide engine: knows where dev junk lives on Windows, measures it and
//! removes it safely. Shared by the CLI/TUI and the desktop app.

pub mod fsutil;
pub mod projects;
pub mod recycle;
pub mod settings;
pub mod shell;
pub mod targets;
pub mod vdisk;

pub use fsutil::{CleanStats, Usage, human};
pub use projects::Artifact;
pub use targets::{Measure, Risk, Target};
