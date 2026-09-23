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
    /// observation (CLAUDE.md's "Cursor movement"). This is exactly the
    /// idleness clock `reconcile::apply_idle_auto_pause` reads against
    /// `[thresholds] auto_pause_days`.
    pub updated_at: u64,
    /// Phase 8b: the highest episode number a MAL sync merge has confirmed
    /// either side already knows about for this series — set only by
    /// `sync.rs`'s merge step, never by `reconcile.rs`. Deliberately
    /// **separate** from `episode_number`: `reconcile.rs` unconditionally
    /// overwrites `episode_number` from the local file's own Anitomy/
    /// ordinal identity on every single reconcile pass (which runs before
    /// every state-touching command, CLAUDE.md's "every state-touching
    /// command reconciles first") — reusing that field for a MAL-driven
    /// number would silently lose it the very next time any command ran.
    /// `None` means no sync merge has ever touched this series. This field
    /// never implies a real playback position. After a successful merge,
    /// sync may separately advance `current_relative_path` to the first
    /// unwatched local episode at position zero; it never fabricates a
    /// position within an episode Tsuzuki did not observe locally.
    #[serde(default)]
    pub mal_episode_awareness: Option<u32>,
}

/// One series' link to a MyAnimeList anime ID (design.md §7's linking
/// section). Stored here in `state.toml`, not `config.toml` — Phase 8b's
/// judgment call (see `sync.rs`'s module doc for the full reasoning): a
/// link is Tsuzuki-derived/written identity data, populated either by
/// exact-match auto-linking or by the explicit `sync link` command, the
/// same category as `SeriesRecord` itself (which already lives here) —
/// not hand-typed human configuration a user would maintain in a dotfiles
/// repo the way tracked roots or exclusions are. Keyed by the same
/// `(root, name)` composite key as `SeriesRecord`, for the same composable-
/// roots reason (CLAUDE.md §3): the same series name can exist under more
/// than one tracked root, each with its own independent MAL link.
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
    let contents =
        toml::to_string_pretty(state).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    atomic_write(path, contents.as_bytes(), None)
}

/// The shared temp-file-then-rename primitive behind every atomic write in
/// this project (CLAUDE.md §3: "Any new state-writing code path must go
/// through the same atomic-write helper; don't add a second write path that
/// writes in place 'just this once.'"). `unix_mode`, when given, restricts
/// the written file's permissions on Unix (e.g. `0o600` for `mal_token.toml`,
/// which holds live credentials) — set on the temp file before the rename so
/// the final path is never briefly world-readable.
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
        // Gap-audit sweep item: is a pre-Phase-5 state.toml reachable in
        // practice, and if one somehow existed, what happens reading it
        // now? In practice: no command ever called `write()` before this
        // phase (confirmed via `git log -p -- src/main.rs` — the one write
        // call that briefly existed in the very first scaffolding commit
        // wrote nothing but an empty default and was removed the same day,
        // before this phase, and no real `~/.local/share/tsuzuki/` exists
        // on this development machine at all), so no real installation can
        // have a genuinely old-format file. This test covers the
        // hypothetical anyway: the old schema (`BTreeMap<String,
        // SeriesState>` under a bare `[series."name"]` table, keyed by name
        // alone, fields `current_path`/no `root`/no `updated_at`) is a
        // fundamentally different TOML shape from today's `[[series]]`
        // array-of-tables — deserialization must fail with a typed error,
        // never panic and never silently produce a wrong/partial `State`.
        // `main.rs`'s `reconcile_state` already turns any `Err` here into a
        // plain command error, not a crash — no migration path exists or is
        // needed for a format that was never actually shipped.
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

    // --- Phase 8b: mal_episode_awareness / MalLink migration + lookups ---

    #[test]
    fn a_pre_phase_8b_series_record_missing_mal_episode_awareness_still_parses() {
        // A real state.toml written by any Phase 5-8a build has no
        // `mal_episode_awareness` key under `[[series]]` at all — must
        // still read cleanly, defaulting to `None`, not fail to parse.
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
}
