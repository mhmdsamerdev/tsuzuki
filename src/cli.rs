//! Clap CLI skeleton. Every subcommand here is a stub in this phase — see
//! `main.rs` for dispatch. Structure and help text follow design.md §9
//! verbatim; nothing here decides ordering, thresholds, or sync behavior.

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
    Resume,

    /// Show what's currently in progress
    Status,

    /// Show which series bare `tsuzuki` would resume right now
    Current,

    /// List all tracked series and their progress/status
    List,

    /// Register a tracked root
    Add {
        /// Directory to track; its immediate children become series
        path: PathBuf,
    },

    /// List series outside the automatic resume picker (never-started,
    /// dropped, paused)
    Start,

    /// Remove a series from the resume picker, keeping its saved position
    Pause {
        /// Series name (a tracked root's immediate child directory name)
        series: String,
    },

    /// Reset a series' progress; it reappears in `start` as if new
    Drop {
        /// Series name (a tracked root's immediate child directory name)
        series: String,
    },

    /// Exclude a file or directory from ordering. Bare invocation surfaces
    /// Anitomy-suggested candidates for confirmation
    Exclude {
        /// Path relative to a tracked root; omit to see suggested candidates
        path: Option<String>,
    },

    /// Run or configure MyAnimeList sync
    Sync {
        #[command(subcommand)]
        action: Option<SyncCommand>,
    },

    /// Read or write config values, or open the config file directly
    ///
    /// `get`/`set`/`list`/`edit` are all equally valid — there's no single
    /// default action, so bare `tsuzuki config` prints this help instead of
    /// guessing one.
    Config {
        #[command(subcommand)]
        action: Option<ConfigCommand>,
    },

    /// Environment health checks (mpv config, watch_later reachability,
    /// tracked-root validity, MAL auth status)
    Doctor,
}

#[derive(Debug, Subcommand)]
pub enum SyncCommand {
    /// One-time MAL OAuth authorization
    Login,

    /// Manually resolve a series-to-MAL-ID mapping when exact-title
    /// matching fails
    Link {
        /// Series name (a tracked root's immediate child directory name)
        series: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print a single config value
    Get {
        /// Dotted key, e.g. `thresholds.resume_seconds`
        key: String,
    },

    /// Set a single config value
    Set {
        /// Dotted key, e.g. `thresholds.resume_seconds`
        key: String,
        /// New value
        value: String,
    },

    /// Print the full resolved config
    List,

    /// Open the config file in `$EDITOR`
    Edit,
}
