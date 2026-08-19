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
    Resume {
        /// Series name; bypasses the interactive picker entirely, TTY or
        /// not (a tracked root's immediate child directory name)
        series: Option<String>,

        /// Tracked root to resolve `series` against, when it exists under
        /// more than one
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Show what's currently in progress
    Status,

    /// List all tracked series and their progress/status
    List,

    /// Register a tracked root
    Add {
        /// Directory to track; its immediate children become series
        path: PathBuf,
    },

    /// List series outside the automatic resume picker (never-started,
    /// dropped, paused)
    Start {
        /// Series name; bypasses the interactive picker entirely, TTY or
        /// not (a tracked root's immediate child directory name)
        series: Option<String>,

        /// Tracked root to resolve `series` against, when it exists under
        /// more than one
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Remove a series from the resume picker, keeping its saved position
    Pause {
        /// Series name (a tracked root's immediate child directory name)
        series: String,

        /// Tracked root to resolve `series` against, when it exists under
        /// more than one
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Reset a series' progress; it reappears in `start` as if new
    Drop {
        /// Series name (a tracked root's immediate child directory name)
        series: String,

        /// Tracked root to resolve `series` against, when it exists under
        /// more than one
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Exclude a file or directory from ordering. Bare invocation surfaces
    /// Anitomy-suggested candidates for confirmation
    Exclude {
        /// Path relative to a tracked root; omit to see suggested candidates
        path: Option<String>,

        /// Tracked root to resolve `path` against, when it exists under
        /// more than one (see the ambiguous-match error for the exact
        /// invocation to use)
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
    /// Bare `tsuzuki config` prints the config Tsuzuki is actually running
    /// with (defaults filled in for anything missing from the file) plus
    /// where that file lives — config.toml is meant to be hand-edited
    /// directly, so `edit` is the only other action here.
    Config {
        #[command(subcommand)]
        action: Option<ConfigCommand>,
    },

    /// Environment health checks (mpv availability, mpv.conf, watch_later
    /// reachability, Flatpak permissions, the tsuzuki.lua companion script,
    /// tracked-root validity, orphaned state.toml records)
    Doctor {
        /// Apply every currently-safe, non-destructive fix (today: only
        /// installing/updating the tsuzuki.lua companion script). Never
        /// edits mpv.conf and never clears orphaned records on its own.
        #[arg(long)]
        fix: bool,

        /// Permanently clear every currently-confirmed orphaned
        /// state.toml record (a series deleted/moved/renamed out from
        /// under a still-tracked root). Requires --fix. Prints exactly
        /// what it's about to delete before doing it.
        #[arg(long, requires = "fix")]
        forget_orphans: bool,
    },
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

        /// The MyAnimeList anime ID to link this series to. Verified
        /// against your MAL list before being saved — a bad id is
        /// rejected, never silently linked.
        #[arg(long = "mal-id")]
        mal_id: u32,

        /// Tracked root to resolve `series` against, when it exists under
        /// more than one
        #[arg(long)]
        root: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Open config.toml in $EDITOR, then re-validate it
    Edit,
}
