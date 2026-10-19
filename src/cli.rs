//! Clap command definitions and help text. Dispatch is in `main.rs`; nothing
//! here decides ordering, thresholds, or sync behavior.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "tsuzuki", version, about = "Continue where you left off.")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Resume the current series (same as bare `tsuzuki`)
    Resume {
        /// Series name (skips the picker), or a tracked root's folder name
        /// (narrows the picker to that root)
        series: Option<String>,

        /// Tracked root to resolve `series` against when it exists under
        /// several; without `series`, narrows the picker to that root
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Show what's currently in progress
    Status,

    /// List all tracked series and their progress/status
    List,

    /// Register one or more tracked roots
    Add {
        /// Directories to track; their immediate children become series
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },

    /// List series outside the resume picker (never started, dropped, paused)
    Start {
        /// Series name (skips the picker), or a tracked root's folder name
        /// (narrows the picker to that root)
        series: Option<String>,

        /// Tracked root to resolve `series` against when it exists under
        /// several; without `series`, narrows the picker to that root
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Remove series from the resume picker, keeping their saved positions
    Pause {
        /// Series names
        #[arg(required = true)]
        series: Vec<String>,

        /// Tracked root to resolve `series` against when it exists under
        /// several
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Reset series' progress; they reappear in `start` as if new
    Drop {
        /// Series names
        #[arg(required = true)]
        series: Vec<String>,

        /// Tracked root to resolve `series` against when it exists under
        /// several
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Exclude files or directories from ordering; bare, suggests candidates
    Exclude {
        /// Paths relative to a tracked root; omit to see suggested candidates
        paths: Vec<String>,

        /// Tracked root to resolve `paths` against when they exist under
        /// several
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Run or configure MyAnimeList sync
    Sync {
        #[command(subcommand)]
        action: Option<SyncCommand>,
    },

    /// Show the resolved config, or open config.toml directly
    ///
    /// Bare `tsuzuki config` prints the effective config (defaults filled
    /// in) and the file's location.
    Config {
        #[command(subcommand)]
        action: Option<ConfigCommand>,
    },

    /// Check the environment: mpv, tsuzuki.lua, Flatpak
    /// permissions, tracked roots, orphaned records
    Doctor {
        /// Apply safe fixes (install/update tsuzuki.lua); never clears
        /// orphaned records unless asked
        #[arg(long)]
        fix: bool,

        /// Delete orphaned state.toml records (series deleted or moved out
        /// of a tracked root), listing them first. Requires --fix
        #[arg(long, requires = "fix")]
        forget_orphans: bool,
    },

    /// Install the newer published GitHub release, if there is one
    Update,
}

#[derive(Debug, Subcommand)]
pub enum SyncCommand {
    /// One-time MAL OAuth authorization
    Login,

    /// Link a series to a MAL ID when exact-title matching fails
    Link {
        /// Series name
        series: String,

        /// MyAnimeList anime ID, checked against your MAL list before saving
        #[arg(long = "mal-id")]
        mal_id: u32,

        /// Tracked root to resolve `series` against when it exists under
        /// several
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Keep series, or every series under a root, out of MAL sync
    Exclude {
        /// Series names, or tracked roots' folder names
        #[arg(required_unless_present = "root")]
        targets: Vec<String>,

        /// Tracked root to resolve `targets` against when they exist under
        /// several; alone, excludes the whole root
        #[arg(long)]
        root: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Open config.toml in $EDITOR, then re-validate it
    Edit,
}
