//! `~/.local/share/tsuzuki/state.toml` — machine-written runtime state.
//!
//! Single flat TOML file, always written atomically (temp file in the same
//! directory, then renamed over the original) — see CLAUDE.md §3. No write
//! path in this module may write in place.
//!
//! A record is keyed by **both** its tracked root's own persisted literal
//! form and the series' verbatim directory name — never name alone, since
//! composable roots (CLAUDE.md §3) let the same series name exist under more
//! than one tracked root at once with independent progress. This is why
//! `series` is a `Vec`, not a map keyed by name: TOML has no natural
//! composite-key map representation, and `[[series]]` array-of-tables
//! mirrors the same shape `config.rs`'s `[[exclusions]]` already uses.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::roots::normalize;

/// A series' status in the resume picker (design.md §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    InProgress,
    Paused,
    Dropped,
}

/// One tracked series' persisted progress. `root` stores the tracked root's
/// own persisted literal form (e.g. `~/Anime`) — the same mechanism
/// exclusions already use (CLAUDE.md's "Exclusion persistence and
/// disambiguation") — never a resolved absolute path, so a record stays
/// portable across machines exactly like a tracked root or exclusion does.
/// `name` is the series' verbatim directory name (CLAUDE.md §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesRecord {
    pub root: String,
    pub name: String,
    pub status: Status,
    /// Relative to `root` (not the series directory) — e.g.
    /// `"One Piece/Season 2/ep01.mkv"` — same relative-to-root convention
    /// exclusions use, for the same portability reason.
    pub current_relative_path: String,
    pub position_seconds: f64,
    pub episode_number: Option<u32>,
    /// Unix seconds. Only moves on a genuine, persisted state change — never
    /// on a mere scan/invocation, and never on a sub-`resume_seconds`
    /// observation (CLAUDE.md's "Cursor movement"). Exists to give
    /// `auto_pause_days` real meaning later; nothing in this phase acts on
    /// it yet beyond keeping it accurate.
    pub updated_at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub series: Vec<SeriesRecord>,
}

impl State {
    pub fn find(&self, root: &str, name: &str) -> Option<&SeriesRecord> {
        self.series
            .iter()
            .find(|r| r.root == root && r.name == name)
    }

    pub fn find_mut(&mut self, root: &str, name: &str) -> Option<&mut SeriesRecord> {
        self.series
            .iter_mut()
            .find(|r| r.root == root && r.name == name)
    }
}

/// Current Unix time in whole seconds. Used for `SeriesRecord::updated_at`.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Resolves a `--root` argument (as accepted by `pause`/`drop`/`resume`/
/// `start`, same mechanism `exclude`'s `--root` already uses) to the
/// matching tracked root's own stored literal form, by expand-then-
/// canonicalize comparison — so `--root ~/Anime` and
/// `--root /home/user/Anime` both resolve to whichever form `config.toml`
/// actually has on file. `exclude.rs`'s own equivalent helper is private and
/// off-limits to modify, so this is a small, independent reimplementation
/// over the same public `roots::normalize` both already share.
pub fn resolve_root_arg(config: &Config, root_arg: &Path, home: Option<&Path>) -> Option<String> {
    let target = normalize(root_arg, home);
    config
        .roots
        .paths
        .iter()
        .find(|r| normalize(Path::new(r), home) == target)
        .cloned()
}

/// Resolves `~/.local/share/tsuzuki/state.toml` via `dirs::data_dir()`.
pub fn state_path() -> io::Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "could not resolve data directory")
    })?;
    Ok(base.join("tsuzuki").join("state.toml"))
}

/// Reads state from `path`. A missing file is treated as empty state, not
/// an error.
pub fn read(path: &Path) -> io::Result<State> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            toml::from_str(&contents).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e),
    }
}

/// Writes `state` to `path` atomically: serialize to a temp file created in
/// the same directory as `path`, then rename over it. A kill mid-write
/// leaves either the old file or the new one, never a half-written one.
pub fn write(path: &Path, state: &State) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "state path has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;

    let contents =
        toml::to_string_pretty(state).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    io::Write::write_all(&mut temp, contents.as_bytes())?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> State {
        State {
            series: vec![SeriesRecord {
                root: "~/Anime".to_string(),
                name: "One Piece".to_string(),
                status: Status::InProgress,
                current_relative_path: "One Piece/Season 2/ep01.mkv".to_string(),
                position_seconds: 340.5,
                episode_number: Some(27),
                updated_at: 1_700_000_000,
            }],
        }
    }

    #[test]
    fn read_missing_file_returns_empty_state_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");

        assert!(!path.exists());
        let state = read(&path).unwrap();
        assert_eq!(state, State::default());

        assert!(
            !path.exists(),
            "read() must not write state.toml on a missing file — only an explicit write() call may"
        );
        assert!(
            fs::read_dir(dir.path()).unwrap().next().is_none(),
            "read() must not create any file in the state directory on a missing file"
        );
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tsuzuki").join("state.toml");

        let state = sample_state();
        write(&path, &state).unwrap();
        let reread = read(&path).unwrap();
        assert_eq!(reread, state);
    }

    #[test]
    fn write_leaves_no_stray_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");

        write(&path, &sample_state()).unwrap();

        let entries: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(
            entries.len(),
            1,
            "exactly one file should remain after an atomic write"
        );
        assert_eq!(entries[0].as_ref().unwrap().path(), path);
    }

    /// Proves the write path actually swaps in a new inode via rename
    /// rather than truncating and rewriting the existing file in place —
    /// the property that makes a kill mid-write safe. An in-place write
    /// would keep the same inode; rename always produces a new one.
    #[cfg(unix)]
    #[test]
    fn write_replaces_file_via_rename_not_in_place_mutation() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");

        write(&path, &State::default()).unwrap();
        let inode_before = fs::metadata(&path).unwrap().ino();

        write(&path, &sample_state()).unwrap();
        let inode_after = fs::metadata(&path).unwrap().ino();

        assert_ne!(
            inode_before, inode_after,
            "expected a new inode from temp-file-then-rename, got the same file mutated in place"
        );
    }

    #[test]
    fn empty_state_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");

        write(&path, &State::default()).unwrap();
        let reread = read(&path).unwrap();
        assert_eq!(reread, State::default());
    }

    #[test]
    fn composite_key_keeps_same_named_series_under_different_roots_as_separate_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");

        let state = State {
            series: vec![
                SeriesRecord {
                    root: "~/Anime".to_string(),
                    name: "Monster".to_string(),
                    status: Status::InProgress,
                    current_relative_path: "Monster/ep05.mkv".to_string(),
                    position_seconds: 100.0,
                    episode_number: Some(5),
                    updated_at: 1,
                },
                SeriesRecord {
                    root: "~/Anime/Rewatches".to_string(),
                    name: "Monster".to_string(),
                    status: Status::Paused,
                    current_relative_path: "Monster/ep02.mkv".to_string(),
                    position_seconds: 50.0,
                    episode_number: Some(2),
                    updated_at: 2,
                },
            ],
        };

        write(&path, &state).unwrap();
        let reread = read(&path).unwrap();
        assert_eq!(reread.series.len(), 2);

        let first = reread.find("~/Anime", "Monster").unwrap();
        assert_eq!(first.status, Status::InProgress);
        assert_eq!(first.position_seconds, 100.0);

        let second = reread.find("~/Anime/Rewatches", "Monster").unwrap();
        assert_eq!(second.status, Status::Paused);
        assert_eq!(second.position_seconds, 50.0);
    }

    #[test]
    fn find_and_find_mut_look_up_by_composite_key_not_name_alone() {
        let mut state = sample_state();
        assert!(state.find("~/Anime", "One Piece").is_some());
        assert!(state.find("~/Elsewhere", "One Piece").is_none());
        assert!(state.find("~/Anime", "Nonexistent").is_none());

        let record = state.find_mut("~/Anime", "One Piece").unwrap();
        record.position_seconds = 999.0;
        assert_eq!(
            state.find("~/Anime", "One Piece").unwrap().position_seconds,
            999.0
        );
    }

    #[test]
    fn resolve_root_arg_matches_across_tilde_and_absolute_forms() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("Anime")).unwrap();

        let mut config = Config::default();
        config.roots.paths.push("~/Anime".to_string());

        let resolved = resolve_root_arg(&config, &home.path().join("Anime"), Some(home.path()));
        assert_eq!(resolved, Some("~/Anime".to_string()));
    }

    #[test]
    fn resolve_root_arg_returns_none_for_an_untracked_root() {
        let config = Config::default();
        let resolved = resolve_root_arg(&config, Path::new("/not/tracked"), None);
        assert_eq!(resolved, None);
    }
}
