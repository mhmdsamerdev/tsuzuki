//! `~/.config/tsuzuki/config.toml` — human-tunable configuration.
//!
//! Read/write via serde+toml. A missing file on read is written with defaults
//! and returned rather than erroring.

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
    #[serde(default)]
    pub sync_exclusions: Vec<SyncExclusion>,
}

/// `120`: below this a saved position is noise, not a bookmark.
fn default_resume_seconds() -> u64 {
    120
}

/// `180`: this close to a file's end counts as watched.
fn default_completion_remaining_seconds() -> u64 {
    180
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    /// Per-field defaults cover a `[thresholds]` table that exists but lacks
    /// this key; `Config.thresholds`'s own default covers a missing table. A
    /// bare `#[serde(default)]` would give `0`, disabling noise filtering and
    /// duration-based completion. Named functions because `serde(default = ..)`
    /// takes a path; `Thresholds::default()` reuses them.
    #[serde(default = "default_resume_seconds")]
    pub resume_seconds: u64,
    #[serde(default = "default_completion_remaining_seconds")]
    pub completion_remaining_seconds: u64,
    /// `0` means never auto-pause.
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

/// `["mkv", "mp4", "avi", "webm", "mov"]`. Needs a named default: a bare
/// `#[serde(default)]` would give an empty list that matches no video files.
fn default_video_extensions() -> Vec<String> {
    ["mkv", "mp4", "avi", "webm", "mov"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Roots {
    /// Empty by default: a fresh install tracks no roots.
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

/// Both fields default to `""`, so a derived `Default` is correct.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Mpv {
    #[serde(default)]
    pub watch_later_dir: String,
    /// Explicit launch command (e.g. `"flatpak run io.mpv.Mpv"`) that skips
    /// native/Flatpak auto-detection. Empty means auto-detect.
    #[serde(default)]
    pub command_override: String,
}

/// `8734`: the local OAuth-callback listener port for `tsuzuki sync login`.
/// Configurable in case something else already uses it.
fn default_mal_redirect_port() -> u16 {
    8734
}

/// `redirect_port` needs a named default: `u16::default()` is `0`, not a valid port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mal {
    #[serde(default)]
    pub client_id: String,
    #[serde(default = "default_mal_redirect_port")]
    pub redirect_port: u16,
    /// Minimum `sync::title_similarity` (`0.0` to `1.0`) a fuzzy candidate must
    /// reach to be suggested as "did you mean...?". Needs a named default:
    /// `f64::default()` is `0.0`, which would suggest anything.
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

/// `0.8`: keeps unrelated titles (e.g. "One Piece" vs. "One Piece Special",
/// well under 0.6) out of suggestions while still matching minor formatting
/// or synonym differences.
fn default_fuzzy_match_threshold() -> f64 {
    0.8
}

/// One excluded file or directory. `root` is the tracked root as stored in
/// `roots.paths` (e.g. `~/Anime`), never resolved, so it stays portable.
/// `path` is relative to that root, as passed to `exclude`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exclusion {
    pub root: String,
    pub path: String,
}

/// One series, or a whole root, kept out of MAL sync. `root` is the tracked
/// root as stored in `roots.paths`; `series` is a depth-1 directory name under
/// it, or `None` for every series under the root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncExclusion {
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
}

/// Resolves `config.toml` under the XDG config dir via `dirs::config_dir()`.
pub fn config_path() -> io::Result<PathBuf> {
    let base = dirs::config_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not resolve config directory",
        )
    })?;
    Ok(base.join("tsuzuki").join("config.toml"))
}

/// Reads config from `path`, writing and returning defaults if the file is missing.
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

/// Strips a leading `.` from each `video_extensions` entry so `.mkv` and `mkv`
/// behave the same. In-memory only; the file on disk is not rewritten.
fn normalize_video_extensions(config: &mut Config) {
    for ext in &mut config.roots.video_extensions {
        if let Some(stripped) = ext.strip_prefix('.') {
            *ext = stripped.to_string();
        }
    }
}

/// Writes `config` to `path`, creating parent directories as needed.
/// A plain write, not the atomic temp-file-then-rename path `state.rs` uses.
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
        // A failed write returns `Err` rather than panicking, and
        // `create_dir_all` failing means the colliding file is never touched.
        let dir = tempfile::tempdir().unwrap();
        let occupied = dir.path().join("not-a-directory");
        fs::write(&occupied, b"unrelated file, blocks the parent dir").unwrap();
        let config_path = occupied.join("config.toml");

        let result = write(&config_path, &Config::default());
        assert!(result.is_err());
        // The blocking file must be untouched.
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
        assert!(config.sync_exclusions.is_empty());
    }

    #[test]
    fn sync_exclusions_round_trip_through_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = Config::default();
        config.sync_exclusions.push(SyncExclusion {
            root: "~/Anime".to_string(),
            series: Some("Monster".to_string()),
        });
        config.sync_exclusions.push(SyncExclusion {
            root: "~/Movies".to_string(),
            series: None,
        });

        write(&path, &config).unwrap();
        let reread = read(&path).unwrap();

        assert_eq!(
            reread.sync_exclusions,
            vec![
                SyncExclusion {
                    root: "~/Anime".to_string(),
                    series: Some("Monster".to_string()),
                },
                SyncExclusion {
                    root: "~/Movies".to_string(),
                    series: None,
                },
            ]
        );
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
        // A missing `video_extensions` must give the default list, not an empty one
        // (which would match zero video files).
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
            "must default to the documented list, not an empty one"
        );
        assert_eq!(config.roots.paths, vec!["~/Anime".to_string()]);
    }

    #[test]
    fn missing_thresholds_table_and_missing_individual_threshold_keys_both_default_correctly() {
        // Covers both layers: `[thresholds]` table absent (config_a), and table
        // present but missing keys (config_b). A bare `#[serde(default)]` gives `0`.
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
        // The key that was present must not be overridden by defaults.
        assert_eq!(config_b.thresholds.auto_pause_days, 7);
    }

    #[test]
    fn missing_exclusions_key_defaults_to_empty_on_read() {
        // A config without an `[[exclusions]]` section must read cleanly.
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
        // A missing `command_override` key under `[mpv]` must read cleanly.
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
        // A missing `redirect_port` must default to 8734; a bare `#[serde(default)]` gives `0`.
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
        // A bare `#[serde(default)]` would give `0.0`, suggesting any candidate.
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
