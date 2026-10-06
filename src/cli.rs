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
        /// Series name, which bypasses the interactive picker entirely, TTY
        /// or not (a tracked root's immediate child directory name); or a
        /// tracked root's folder name, which narrows the picker to that root
        series: Option<String>,

        /// Tracked root to resolve `series` against, when it exists under
        /// more than one; without `series`, narrows the picker to that root
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

    /// List series outside the automatic resume picker (never-started,
    /// dropped, paused)
    Start {
        /// Series name, which bypasses the interactive picker entirely, TTY
        /// or not (a tracked root's immediate child directory name); or a
        /// tracked root's folder name, which narrows the picker to that root
        series: Option<String>,

        /// Tracked root to resolve `series` against, when it exists under
        /// more than one; without `series`, narrows the picker to that root
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Remove series from the resume picker, keeping their saved positions
    Pause {
        /// Series names (a tracked root's immediate child directory names)
        #[arg(required = true)]
        series: Vec<String>,

        /// Tracked root to resolve `series` against, when it exists under
        /// more than one
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Reset series' progress; they reappear in `start` as if new
    Drop {
        /// Series names (a tracked root's immediate child directory names)
        #[arg(required = true)]
        series: Vec<String>,

        /// Tracked root to resolve `series` against, when it exists under
        /// more than one
        #[arg(long)]
        root: Option<PathBuf>,
    },

    /// Exclude files or directories from ordering. Bare invocation surfaces
    /// Anitomy-suggested candidates for confirmation
    Exclude {
        /// Paths relative to a tracked root; omit to see suggested candidates
        paths: Vec<String>,

        /// Tracked root to resolve `paths` against, when they exist under
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

    /// Check for a newer published GitHub release, and install it directly
    /// if one is found, via the same installer script README.md
    /// documents. The network is only ever touched here because this
    /// command was explicitly invoked — never from `doctor` or any other
    /// command.
    Update,
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
