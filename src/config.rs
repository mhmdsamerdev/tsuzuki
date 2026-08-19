//! `~/.config/tsuzuki/config.toml` — human-tunable configuration.
//!
//! Read/write via serde+toml. If the file doesn't exist on read, defaults
//! are written and returned rather than erroring (see CLAUDE.md §3).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub thresholds: Thresholds,
    #[serde(default)]
    pub roots: Roots,
    #[serde(default)]
    pub mpv: Mpv,
    #[serde(default)]
    pub mal: Mal,
    #[serde(default)]
    pub exclusions: Vec<Exclusion>,
}

/// `120`, per design.md §8 — below this a saved position is noise, not a
/// real bookmark.
fn default_resume_seconds() -> u64 {
    120
}

/// `180`, per design.md §8 — this close to a file's end counts as watched.
fn default_completion_remaining_seconds() -> u64 {
    180
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    /// Two defaulting layers, both needed and both covering a genuinely
    /// different missing-file shape: `Config.thresholds`'s own
    /// `#[serde(default)]` (via `Thresholds: Default`, below) covers a
    /// config.toml with no `[thresholds]` table at all; these per-field
    /// defaults cover a `[thresholds]` table that's present but missing
    /// this one key — e.g. an old config.toml from before
    /// `completion_remaining_seconds` existed. A bare `#[serde(default)]`
    /// would be silently *wrong* here (defaults to `0`, not the documented
    /// `120`/`180`), not just imprecise — `0` for `resume_seconds` means
    /// literally no noise filtering at all, and `0` for
    /// `completion_remaining_seconds` means nothing is ever completed via
    /// the duration-proximity signal. Named functions, not inline closures,
    /// since `serde(default = "...")` requires a path, and reused by
    /// `Thresholds::default()` below so the two layers can't drift apart.
    #[serde(default = "default_resume_seconds")]
    pub resume_seconds: u64,
    #[serde(default = "default_completion_remaining_seconds")]
    pub completion_remaining_seconds: u64,
    /// `0` is both the documented default (design.md §8: "never
    /// auto-pause") and `u64`'s own `Default::default()` — a bare
    /// `#[serde(default)]` is correct here, unlike its two siblings above.
    #[serde(default)]
    pub auto_pause_days: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            resume_seconds: default_resume_seconds(),
            completion_remaining_seconds: default_completion_remaining_seconds(),
            auto_pause_days: 0,
        }
    }
}

/// `["mkv", "mp4", "avi", "webm", "mov"]`, per design.md §8. A bare
/// `#[serde(default)]` on `video_extensions` would silently produce an
/// *empty* list instead — not a parse failure, but a config that now
/// matches zero video files and excludes an entire library. This is the
/// exact bug found during Phase 5's human dogfood pass: an old config.toml
/// from before this field existed failed to parse at all (no default of
/// any kind), which a bare `#[serde(default)]` would have "fixed" into a
/// different, quieter failure instead of the documented list.
fn default_video_extensions() -> Vec<String> {
    ["mkv", "mp4", "avi", "webm", "mov"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Roots {
    /// Empty is the genuinely correct default (a fresh install has no
    /// tracked roots yet) — `Vec::default()` already matches, so a bare
    /// `#[serde(default)]` is correct, unlike `video_extensions` below.
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default = "default_video_extensions")]
    pub video_extensions: Vec<String>,
}

impl Default for Roots {
    fn default() -> Self {
        Roots {
            paths: Vec::new(),
            video_extensions: default_video_extensions(),
        }
    }
}

/// Both fields correctly default to `""` (`String::default()`), so
/// `#[derive(Default)]` needs no field-level overrides — unlike
/// `Thresholds`/`Roots`, nothing here would be silently wrong under a bare
/// `#[serde(default)]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Mpv {
    #[serde(default)]
    pub watch_later_dir: String,
    /// Explicit launch command (e.g. `"flatpak run io.mpv.Mpv"`), skipping
    /// native/Flatpak auto-detection entirely. Empty = auto-detect — same
    /// empty-means-default shape as `watch_later_dir` (CLAUDE.md's "Launching
    /// mpv" decision).
    #[serde(default)]
    pub command_override: String,
}

/// `8734`, the local OAuth-callback listener's port (`tsuzuki sync login`,
/// Phase 8a) — arbitrary but fixed, matching this project's "every tunable
/// is genuinely configurable" rule for the case where the default happens to
/// collide with something else already running on the user's machine.
fn default_mal_redirect_port() -> u16 {
    8734
}

/// `client_id`'s correct default is `""`, same reasoning as `Mpv`.
/// `redirect_port` needs the same two-defaulting-layers treatment
/// `Thresholds` uses — its correct default (`8734`) isn't `u16::default()`
/// (`0`, which isn't even a valid listener port), so a bare
/// `#[serde(default)]` would be silently wrong the same way it would for
/// `resume_seconds`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mal {
    #[serde(default)]
    pub client_id: String,
    #[serde(default = "default_mal_redirect_port")]
    pub redirect_port: u16,
    /// The minimum title-similarity score (`sync::title_similarity`, `0.0`
    /// to `1.0`) a fuzzy candidate must clear before `tsuzuki sync` will
    /// suggest it as a "did you mean...?" — below this, `sync`'s own output
    /// shows a plain "no matching series found" instead of a low-confidence
    /// guess (CLAUDE.md's "sync output only ever suggests a confident fuzzy
    /// match"). Same two-defaulting-layers reasoning as `redirect_port`: the
    /// correct default (`0.8`) isn't `f64::default()` (`0.0`, which would
    /// suggest literally anything on the user's list).
    #[serde(default = "default_fuzzy_match_threshold")]
    pub fuzzy_match_threshold: f64,
}

impl Default for Mal {
    fn default() -> Self {
        Mal {
            client_id: String::new(),
            redirect_port: default_mal_redirect_port(),
            fuzzy_match_threshold: default_fuzzy_match_threshold(),
        }
    }
}

/// `0.8`, chosen so an unrelated series (typically well under 0.6 similarity
/// even for same-franchise titles like "One Piece" vs. "One Piece Special")
/// never surfaces as a suggestion, while a genuine minor-formatting/
/// synonym-level difference (the common real case) still does.
fn default_fuzzy_match_threshold() -> f64 {
    0.8
}

/// One `tsuzuki exclude`d file or directory (CLAUDE.md's "Exclusion
/// persistence and disambiguation"). `root` stores the tracked root's own
/// persisted literal form (whatever's in `roots.paths`, e.g. `~/Anime`),
/// never a resolved absolute path — this is what keeps an exclusion
/// portable across machines the same way a tracked root is. `path` is
/// relative to that root, exactly as passed to `exclude`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exclusion {
    pub root: String,
    pub path: String,
}

/// Resolves `~/.config/tsuzuki/config.toml` via the `dirs` crate's
/// XDG-aware `config_dir()` — never a hand-rolled `$HOME` join.
pub fn config_path() -> io::Result<PathBuf> {
    let base = dirs::config_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not resolve config directory",
        )
    })?;
    Ok(base.join("tsuzuki").join("config.toml"))
}

/// Reads config from `path`. If the file doesn't exist, writes and returns
/// defaults instead of erroring.
pub fn read(path: &Path) -> io::Result<Config> {
    let mut config = match fs::read_to_string(path) {
        Ok(contents) => {
            toml::from_str(&contents).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let config = Config::default();
            write(path, &config)?;
            config
        }
        Err(e) => return Err(e),
    };
    normalize_video_extensions(&mut config);
    Ok(config)
}

/// Strips a leading `.` from each `video_extensions` entry, so a
/// hand-typed `.mkv` behaves identically to the documented bare `mkv`
/// form (CLAUDE.md's "video_extensions entries are normalized on read").
/// In-memory only — the on-disk file is never silently rewritten just
/// because it was read; only an explicit `write` changes it.
fn normalize_video_extensions(config: &mut Config) {
    for ext in &mut config.roots.video_extensions {
        if let Some(stripped) = ext.strip_prefix('.') {
            *ext = stripped.to_string();
        }
    }
}

/// Writes `config` to `path`, creating parent directories as needed.
/// A hand-edited config file is not state, so this is a plain write —
/// not the atomic temp-file-then-rename path `state.rs` uses.
pub fn write(path: &Path, config: &Config) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string_pretty(config)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_missing_file_writes_and_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tsuzuki").join("config.toml");

        assert!(!path.exists());
        let config = read(&path).unwrap();
        assert_eq!(config, Config::default());
        assert!(path.exists());

        let reread = read(&path).unwrap();
        assert_eq!(reread, config);
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = Config::default();
        config.thresholds.resume_seconds = 90;
        config.roots.paths.push("~/Anime".to_string());

        write(&path, &config).unwrap();
        let reread = read(&path).unwrap();
        assert_eq!(reread, config);
    }

    #[test]
    fn write_fails_gracefully_when_parent_path_is_occupied_by_a_file() {
        // config.toml is deliberately a plain (non-atomic) write — see the
        // doc comment on `write` — so there's no rename-swap guarantee to
        // test the way state.rs's write is tested. What's worth locking in
        // instead: a failure here returns `Err` rather than panicking, and
        // (since `fs::create_dir_all` fails before `fs::write` is ever
        // reached) never touches an existing file at a colliding path.
        let dir = tempfile::tempdir().unwrap();
        let occupied = dir.path().join("not-a-directory");
        fs::write(&occupied, b"unrelated file, blocks the parent dir").unwrap();
        let config_path = occupied.join("config.toml");

        let result = write(&config_path, &Config::default());
        assert!(result.is_err());
        // The blocking file itself must be untouched.
        assert_eq!(
            fs::read(&occupied).unwrap(),
            b"unrelated file, blocks the parent dir"
        );
    }

    #[test]
    fn video_extensions_with_leading_dots_are_normalized_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = Config::default();
        config.roots.video_extensions =
            vec![".mkv".to_string(), "mp4".to_string(), ".AVI".to_string()];
        write(&path, &config).unwrap();

        let reread = read(&path).unwrap();
        assert_eq!(
            reread.roots.video_extensions,
            vec!["mkv".to_string(), "mp4".to_string(), "AVI".to_string()]
        );
    }

    #[test]
    fn dotted_and_bare_video_extensions_normalize_to_the_same_result() {
        let dir = tempfile::tempdir().unwrap();

        let dotted_path = dir.path().join("dotted.toml");
        let mut dotted = Config::default();
        dotted.roots.video_extensions = vec![".mkv".to_string(), ".mp4".to_string()];
        write(&dotted_path, &dotted).unwrap();

        let bare_path = dir.path().join("bare.toml");
        let mut bare = Config::default();
        bare.roots.video_extensions = vec!["mkv".to_string(), "mp4".to_string()];
        write(&bare_path, &bare).unwrap();

        let dotted_read = read(&dotted_path).unwrap();
        let bare_read = read(&bare_path).unwrap();

        assert_eq!(
            dotted_read.roots.video_extensions,
            bare_read.roots.video_extensions
        );
    }

    #[test]
    fn defaults_match_design_doc() {
        let config = Config::default();
        assert_eq!(config.thresholds.resume_seconds, 120);
        assert_eq!(config.thresholds.completion_remaining_seconds, 180);
        assert_eq!(config.thresholds.auto_pause_days, 0);
        assert!(config.roots.paths.is_empty());
        assert_eq!(
            config.roots.video_extensions,
            vec!["mkv", "mp4", "avi", "webm", "mov"]
        );
        assert_eq!(config.mpv.watch_later_dir, "");
        assert_eq!(config.mpv.command_override, "");
        assert_eq!(config.mal.client_id, "");
        assert_eq!(config.mal.redirect_port, 8734);
        assert_eq!(config.mal.fuzzy_match_threshold, 0.8);
        assert!(config.exclusions.is_empty());
    }

    #[test]
    fn exclusions_round_trip_through_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = Config::default();
        config.roots.paths.push("~/Anime".to_string());
        config.exclusions.push(Exclusion {
            root: "~/Anime".to_string(),
            path: "One Piece/Specials".to_string(),
        });
        config.exclusions.push(Exclusion {
            root: "~/Anime".to_string(),
            path: "Monster/stray-clip.mkv".to_string(),
        });

        write(&path, &config).unwrap();
        let reread = read(&path).unwrap();

        assert_eq!(reread.exclusions, config.exclusions);
        assert_eq!(reread.exclusions[0].root, "~/Anime");
        assert_eq!(reread.exclusions[0].path, "One Piece/Specials");
    }

    #[test]
    fn missing_video_extensions_key_defaults_to_the_documented_list_not_empty() {
        // Regression test for the real bug found during Phase 5's human
        // dogfood pass: `tsuzuki add` against a real (if old) config.toml
        // missing `video_extensions` failed with "missing field" — this
        // field was added in Phase 1 with no serde default at all. Asserts
        // on the actual resulting value, not just "doesn't error" — a bare
        // `#[serde(default)]` would also "not error" while silently
        // producing an empty list (which matches zero video files), so
        // that alone wouldn't prove the fix is correct.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[roots]
paths = ["~/Anime"]
"#,
        )
        .unwrap();

        let config = read(&path).unwrap();
        assert_eq!(
            config.roots.video_extensions,
            vec!["mkv", "mp4", "avi", "webm", "mov"],
            "must default to the documented list from design.md §8, not an empty one"
        );
        assert_eq!(config.roots.paths, vec!["~/Anime".to_string()]);
    }

    #[test]
    fn missing_thresholds_table_and_missing_individual_threshold_keys_both_default_correctly() {
        // Same bug class, found during the gap-audit's field-by-field
        // sweep: `resume_seconds`/`completion_remaining_seconds` had no
        // serde default either, and their correct defaults (120/180) are
        // non-zero — a bare `#[serde(default)]` would silently produce `0`
        // for both, which is wrong in a different, quieter way (0 = no
        // noise filtering at all / nothing ever counts as near-complete).
        // Covers both defaulting layers: the whole `[thresholds]` table
        // absent (config_a), and the table present but missing one key
        // (config_b).
        let dir = tempfile::tempdir().unwrap();

        let path_a = dir.path().join("no_thresholds_table.toml");
        fs::write(&path_a, "[roots]\npaths = []\n").unwrap();
        let config_a = read(&path_a).unwrap();
        assert_eq!(config_a.thresholds.resume_seconds, 120);
        assert_eq!(config_a.thresholds.completion_remaining_seconds, 180);
        assert_eq!(config_a.thresholds.auto_pause_days, 0);

        let path_b = dir.path().join("partial_thresholds_table.toml");
        fs::write(
            &path_b,
            r#"
[thresholds]
auto_pause_days = 7

[roots]
paths = []
"#,
        )
        .unwrap();
        let config_b = read(&path_b).unwrap();
        assert_eq!(
            config_b.thresholds.resume_seconds, 120,
            "must default to 120, not 0"
        );
        assert_eq!(
            config_b.thresholds.completion_remaining_seconds, 180,
            "must default to 180, not 0"
        );
        // The one key that *was* present must still be read correctly, not
        // overridden by the default-filling logic.
        assert_eq!(config_b.thresholds.auto_pause_days, 7);
    }

    #[test]
    fn missing_exclusions_key_defaults_to_empty_on_read() {
        // A config.toml written before Phase 2 (or hand-edited without an
        // [[exclusions]] section) must still read cleanly rather than
        // erroring on a missing field.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[thresholds]
resume_seconds = 120
completion_remaining_seconds = 180
auto_pause_days = 0

[roots]
paths = []
video_extensions = ["mkv"]

[mpv]
watch_later_dir = ""

[mal]
client_id = ""
"#,
        )
        .unwrap();

        let config = read(&path).unwrap();
        assert!(config.exclusions.is_empty());
    }

    #[test]
    fn missing_command_override_key_defaults_to_empty_on_read() {
        // A config.toml written before Phase 5 has no `command_override`
        // key under `[mpv]` at all — must still read cleanly.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[thresholds]
resume_seconds = 120
completion_remaining_seconds = 180
auto_pause_days = 0

[roots]
paths = []
video_extensions = ["mkv"]

[mpv]
watch_later_dir = ""

[mal]
client_id = ""
"#,
        )
        .unwrap();

        let config = read(&path).unwrap();
        assert_eq!(config.mpv.command_override, "");
    }

    #[test]
    fn missing_redirect_port_key_defaults_to_8734_not_zero() {
        // Same defaulting-layer bug class as `resume_seconds`/
        // `completion_remaining_seconds`: a config.toml written before Phase
        // 8a has no `redirect_port` key under `[mal]` at all — a bare
        // `#[serde(default)]` would silently produce `0`, not a valid
        // listener port.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[thresholds]
resume_seconds = 120
completion_remaining_seconds = 180
auto_pause_days = 0

[roots]
paths = []
video_extensions = ["mkv"]

[mpv]
watch_later_dir = ""

[mal]
client_id = ""
"#,
        )
        .unwrap();

        let config = read(&path).unwrap();
        assert_eq!(config.mal.redirect_port, 8734);
    }

    #[test]
    fn missing_fuzzy_match_threshold_key_defaults_to_zero_point_eight_not_zero() {
        // Same defaulting-layer bug class again: a bare `#[serde(default)]`
        // would silently produce `0.0`, which would suggest literally any
        // candidate on the user's MAL list regardless of similarity.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[thresholds]
resume_seconds = 120
completion_remaining_seconds = 180
auto_pause_days = 0

[roots]
paths = []
video_extensions = ["mkv"]

[mpv]
watch_later_dir = ""

[mal]
client_id = ""
redirect_port = 8734
"#,
        )
        .unwrap();

        let config = read(&path).unwrap();
        assert_eq!(config.mal.fuzzy_match_threshold, 0.8);
    }
}
