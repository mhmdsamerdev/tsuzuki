//! `~/.local/share/tsuzuki/state.toml` — machine-written runtime state.
//!
//! Single flat TOML file, always written atomically (temp file in the same
//! directory, then renamed over the original) — see CLAUDE.md §3. No write
//! path in this module may write in place.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A series' status in the resume picker (design.md §8). Real transitions
/// (pause/drop/resume) land in Phase 5 — this phase only needs the shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    InProgress,
    Paused,
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesState {
    pub status: Status,
    pub current_path: String,
    pub position_seconds: f64,
    pub episode_number: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub series: BTreeMap<String, SeriesState>,
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
///
/// Unused until a later phase's commands actually mutate state (pause,
/// drop, sync, ...) — no Phase 0 stub calls this, and reading a missing
/// state.toml must not write one, so nothing exercises it yet.
#[allow(dead_code)]
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
        let mut series = BTreeMap::new();
        series.insert(
            "One Piece".to_string(),
            SeriesState {
                status: Status::InProgress,
                current_path: "One Piece/Season 2/ep01.mkv".to_string(),
                position_seconds: 340.5,
                episode_number: Some(27),
            },
        );
        State { series }
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
}
