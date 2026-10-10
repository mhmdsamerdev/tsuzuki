//! `state.toml`: machine-written runtime state.
//!
//! One flat TOML file, always written atomically; no write path here may
//! write in place.
//!
//! Records are keyed by `(root, name)`, since the same series name can exist
//! under several tracked roots. Hence `series` is a `Vec` (`[[series]]`), not
//! a map keyed by name.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::roots::normalize;

/// A series' status in the resume picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    InProgress,
    Paused,
    /// Reached only by reconcile confirming the series' final episode
    /// finished; no command sets it.
    Completed,
    Dropped,
}

/// One tracked series' persisted progress. `root` is the tracked root's
/// stored literal form (e.g. `~/Anime`), never a resolved path. `name` is the
/// series' verbatim directory name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesRecord {
    pub root: String,
    pub name: String,
    pub status: Status,
    /// Relative to `root`, not the series directory, e.g. `One Piece/Season 2/ep01.mkv`.
    pub current_relative_path: String,
    pub position_seconds: f64,
    pub episode_number: Option<u32>,
    /// Unix seconds. Moves only on a persisted state change, never on a scan or
    /// a sub-`resume_seconds` observation. Idle auto-pause reads it.
    pub updated_at: u64,
    /// Highest episode number a MAL sync merge has confirmed either side knows.
    /// Set only by `sync.rs`, never reconcile, which overwrites `episode_number`
    /// on every pass. `None` means no merge has touched this series. Never
    /// implies a playback position.
    #[serde(default)]
    pub mal_episode_awareness: Option<u32>,
}

/// One series' link to a MyAnimeList anime ID. Lives here rather than in
/// `config.toml` because Tsuzuki writes it (auto-link or `sync link`). Keyed by
/// `(root, name)` like `SeriesRecord`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MalLink {
    pub root: String,
    pub name: String,
    pub mal_id: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub series: Vec<SeriesRecord>,
    #[serde(default)]
    pub mal_links: Vec<MalLink>,
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

    pub fn find_mal_link(&self, root: &str, name: &str) -> Option<&MalLink> {
        self.mal_links
            .iter()
            .find(|l| l.root == root && l.name == name)
    }

    pub fn find_mal_link_mut(&mut self, root: &str, name: &str) -> Option<&mut MalLink> {
        self.mal_links
            .iter_mut()
            .find(|l| l.root == root && l.name == name)
    }
}

/// Current Unix time in whole seconds.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Resolves a `--root` argument to the tracked root's stored literal form,
/// comparing normalized paths so `~/Anime` and its absolute form both match.
pub fn resolve_root_arg(config: &Config, root_arg: &Path, home: Option<&Path>) -> Option<String> {
    let target = normalize(root_arg, home);
    config
        .roots
        .paths
        .iter()
        .find(|r| normalize(Path::new(r), home) == target)
        .cloned()
}

/// Path of `state.toml` under the platform data dir.
pub fn state_path() -> io::Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "could not resolve data directory")
    })?;
    Ok(base.join("tsuzuki").join("state.toml"))
}

/// Reads state from `path`; a missing file is empty state, not an error.
pub fn read(path: &Path) -> io::Result<State> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            toml::from_str(&contents).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e),
    }
}

/// Writes `state` to `path` atomically: a temp file in the same directory,
/// then a rename over `path`, so a kill mid-write never leaves a partial file.
pub fn write(path: &Path, state: &State) -> io::Result<()> {
    let contents =
        toml::to_string_pretty(state).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    atomic_write(path, contents.as_bytes(), None)
}

/// The temp-file-then-rename primitive every atomic write goes through.
/// `unix_mode`, when given, is set on the temp file before the rename so the
/// final path is never briefly more permissive (e.g. `0o600` for the MAL token).
pub fn atomic_write(path: &Path, contents: &[u8], unix_mode: Option<u32>) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    fs::create_dir_all(parent)?;

    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    io::Write::write_all(&mut temp, contents)?;

    #[cfg(unix)]
    if let Some(mode) = unix_mode {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = unix_mode;

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
                mal_episode_awareness: None,
            }],
            mal_links: Vec::new(),
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

    /// Rename swaps in a new inode; an in-place rewrite would keep the same one.
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
                    mal_episode_awareness: None,
                },
                SeriesRecord {
                    root: "~/Anime/Rewatches".to_string(),
                    name: "Monster".to_string(),
                    status: Status::Paused,
                    current_relative_path: "Monster/ep02.mkv".to_string(),
                    position_seconds: 50.0,
                    episode_number: Some(2),
                    updated_at: 2,
                    mal_episode_awareness: None,
                },
            ],

            mal_links: Vec::new(),
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
    fn a_pre_phase_5_schema_state_toml_fails_cleanly_not_silently_or_with_a_panic() {
        // The old by-name `[series."name"]` schema must fail to parse, never yield partial state.
        let old_format = r#"
[series."One Piece"]
status = "in_progress"
current_path = "One Piece/Season 2/ep01.mkv"
position_seconds = 340.5
episode_number = 27
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        fs::write(&path, old_format).unwrap();

        let result = read(&path);
        assert!(
            result.is_err(),
            "an old-schema file must not silently parse as valid (possibly empty) new-schema state"
        );
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

    // --- mal_episode_awareness / MalLink ---

    #[test]
    fn a_pre_phase_8b_series_record_missing_mal_episode_awareness_still_parses() {
        // Older files have no `mal_episode_awareness` key; it must default to `None`.
        let old_format = r#"
[[series]]
root = "~/Anime"
name = "One Piece"
status = "in_progress"
current_relative_path = "One Piece/ep01.mkv"
position_seconds = 340.5
episode_number = 27
updated_at = 1700000000
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        fs::write(&path, old_format).unwrap();

        let state = read(&path).unwrap();
        assert_eq!(state.series.len(), 1);
        assert_eq!(state.series[0].mal_episode_awareness, None);
        assert!(
            state.mal_links.is_empty(),
            "a state.toml with no [[mal_links]] table at all must default to empty, not fail"
        );
    }

    #[test]
    fn mal_links_round_trip_through_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");

        let mut state = sample_state();
        state.mal_links.push(MalLink {
            root: "~/Anime".to_string(),
            name: "One Piece".to_string(),
            mal_id: 21,
        });

        write(&path, &state).unwrap();
        let reread = read(&path).unwrap();
        assert_eq!(reread.mal_links, state.mal_links);
    }

    #[test]
    fn find_mal_link_and_find_mal_link_mut_look_up_by_composite_key() {
        let mut state = State {
            series: Vec::new(),
            mal_links: vec![
                MalLink {
                    root: "~/Anime".to_string(),
                    name: "Monster".to_string(),
                    mal_id: 19,
                },
                MalLink {
                    root: "~/Anime/Rewatches".to_string(),
                    name: "Monster".to_string(),
                    mal_id: 19,
                },
            ],
        };

        assert!(state.find_mal_link("~/Anime", "Monster").is_some());
        assert!(state.find_mal_link("~/Elsewhere", "Monster").is_none());
        assert!(state.find_mal_link("~/Anime", "Nonexistent").is_none());

        let link = state.find_mal_link_mut("~/Anime", "Monster").unwrap();
        link.mal_id = 20;
        assert_eq!(
            state.find_mal_link("~/Anime", "Monster").unwrap().mal_id,
            20
        );
        assert_eq!(
            state
                .find_mal_link("~/Anime/Rewatches", "Monster")
                .unwrap()
                .mal_id,
            19,
            "the identically-named link under the OTHER root must be untouched"
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_with_unix_mode_sets_that_mode_on_the_final_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.toml");

        atomic_write(&path, b"secret", Some(0o600)).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"secret");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn atomic_write_overwrites_existing_content_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        fs::write(&path, b"old").unwrap();

        atomic_write(&path, b"new", None).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["state.toml"]);
    }

    #[test]
    fn atomic_write_when_rename_fails_returns_error_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        // A non-empty directory at the target makes the final rename fail.
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"x").unwrap();

        let result = atomic_write(&path, b"new", None);

        assert!(result.is_err());
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["state.toml"]);
        assert_eq!(fs::read(path.join("keep")).unwrap(), b"x");
    }

    #[test]
    fn state_round_trips_dropped_status_and_mal_episode_awareness() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        let mut state = sample_state();
        state.series[0].status = Status::Dropped;
        state.series[0].mal_episode_awareness = Some(12);
        write(&path, &state).unwrap();

        let read_back = read(&path).unwrap();

        assert_eq!(read_back, state);
    }

    #[test]
    fn state_round_trips_completed_status_as_snake_case() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        let mut state = sample_state();
        state.series[0].status = Status::Completed;
        write(&path, &state).unwrap();

        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("status = \"completed\"")
        );
        assert_eq!(read(&path).unwrap(), state);
    }
}
