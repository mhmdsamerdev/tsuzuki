//! `~/.config/tsuzuki/config.toml` — human-tunable configuration.
//!
//! Read/write via serde+toml. If the file doesn't exist on read, defaults
//! are written and returned rather than erroring (see CLAUDE.md §3).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub thresholds: Thresholds,
    pub roots: Roots,
    pub mpv: Mpv,
    pub mal: Mal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    pub resume_seconds: u64,
    pub completion_remaining_seconds: u64,
    pub auto_pause_days: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Roots {
    pub paths: Vec<String>,
    pub video_extensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mpv {
    pub watch_later_dir: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mal {
    pub client_id: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            thresholds: Thresholds {
                resume_seconds: 120,
                completion_remaining_seconds: 180,
                auto_pause_days: 0,
            },
            roots: Roots {
                paths: Vec::new(),
                video_extensions: ["mkv", "mp4", "avi", "webm", "mov"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            },
            mpv: Mpv {
                watch_later_dir: String::new(),
            },
            mal: Mal {
                client_id: String::new(),
            },
        }
    }
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
    match fs::read_to_string(path) {
        Ok(contents) => {
            let config: Config = toml::from_str(&contents)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(config)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let config = Config::default();
            write(path, &config)?;
            Ok(config)
        }
        Err(e) => Err(e),
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
        assert_eq!(config.mal.client_id, "");
    }
}
