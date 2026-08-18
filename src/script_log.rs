//! Reading the optional mpv companion script's log directory (CLAUDE.md's
//! "Optional mpv companion script — per-file preferred source when present,
//! watch_later fallback always intact").
//!
//! `contrib/mpv/tsuzuki.lua` writes one flat text file per played source
//! path to `$XDG_DATA_HOME/tsuzuki/mpv-log`, atomically (temp file +
//! rename), named by an 8-hex-char djb2 hash of the path:
//!
//! ```text
//! # <original path>
//! duration=<float or empty>
//! position=<float or empty>
//! reason=<eof|stop|quit|error|redirect|unknown>
//! ```
//!
//! The comment-line path is the actual identity check — the hash is never
//! trusted on its own, exactly like `mpv.rs`'s own `watch_later` parsing.
//! Unlike `watch_later`, this directory is entirely Tsuzuki-owned (fixed,
//! not configurable — CLAUDE.md's decision not to add a `[mpv]` override for
//! it) and mpv never deletes an entry once written, so presence here is
//! permanent, not a launcher-independent-but-load-deleted signal the way
//! `watch_later` entries are.
//!
//! Mirrors `mpv.rs`'s `scan_watch_later` shape deliberately, so the two read
//! side by side: parse-one-entry, skip malformed/redirect entries without
//! aborting the scan, filter to tracked roots via the same symlink-aware
//! `mpv::is_under_a_tracked_root` helper `watch_later` parsing already
//! relies on.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::mpv;

/// One parsed, tracked-root-filtered script-log entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptLogEntry {
    /// The path the script wrote in the entry's comment line — the file the
    /// entry is for. Not necessarily canonicalized.
    pub source_path: PathBuf,
    /// `None` when the script's own `duration=` line was empty (mpv never
    /// observed a `duration` property value for this file).
    pub duration: Option<f64>,
    /// `None` when the script's own `position=` line was empty.
    pub position: Option<f64>,
    /// One of `eof`/`stop`/`quit`/`error`/`unknown` — `redirect` entries are
    /// discarded at parse time and never produce a `ScriptLogEntry` at all
    /// (same treatment as `watch_later`'s own redirect-entry filtering).
    pub reason: String,
    /// The entry file's own mtime — used for the per-path source-selection
    /// mtime comparison against a same-path `watch_later` entry.
    pub mtime: SystemTime,
}

/// Resolves `$XDG_DATA_HOME/tsuzuki/mpv-log` via `dirs::data_dir()` — the
/// same base directory `state::state_path()` resolves `state.toml` under,
/// just a different subdirectory. Fixed, never configurable (CLAUDE.md: "No
/// new config field for the script-log directory").
pub fn mpv_log_dir() -> io::Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "could not resolve data directory")
    })?;
    Ok(base.join("tsuzuki").join("mpv-log"))
}

/// Parses one script-log entry file. Returns `None` for a `reason=redirect`
/// entry (an internal mpv pointer from opening a directory/playlist, not
/// real playback state — same treatment as `watch_later`'s own redirect
/// filtering) or for any entry that doesn't fit the expected shape (missing
/// or empty comment-line path, missing `reason=` line, an unparseable
/// non-empty `duration=`/`position=` value, unreadable mtime) — malformed
/// entries are skipped, never abort the caller's scan.
fn parse_entry_file(path: &Path) -> Option<ScriptLogEntry> {
    let contents = fs::read_to_string(path).ok()?;
    let mut lines = contents.lines();

    let first_line = lines.next()?;
    let source_str = first_line.strip_prefix("# ")?;
    if source_str.is_empty() {
        return None;
    }
    let source_path = PathBuf::from(source_str);

    let mut duration: Option<f64> = None;
    let mut position: Option<f64> = None;
    let mut reason: Option<String> = None;

    for line in lines {
        if let Some(rest) = line.strip_prefix("duration=") {
            if !rest.is_empty() {
                duration = Some(rest.trim().parse::<f64>().ok()?);
            }
        } else if let Some(rest) = line.strip_prefix("position=") {
            if !rest.is_empty() {
                position = Some(rest.trim().parse::<f64>().ok()?);
            }
        } else if let Some(rest) = line.strip_prefix("reason=") {
            reason = Some(rest.trim().to_string());
        }
    }

    let reason = reason?;
    if reason.is_empty() || reason == "redirect" {
        return None;
    }

    let mtime = fs::metadata(path).ok()?.modified().ok()?;

    Some(ScriptLogEntry {
        source_path,
        duration,
        position,
        reason,
        mtime,
    })
}

/// Scans `dir` for script-log entries, parses each, discards redirects and
/// malformed files, and filters to entries whose source path falls under one
/// of `tracked_roots` — mirroring `mpv::scan_watch_later`'s own
/// parse-then-filter shape. A `dir` that doesn't exist on disk yields an
/// empty result, not an error (the script may never have been installed).
pub fn scan_script_log(
    dir: &Path,
    tracked_roots: &[String],
    home: Option<&Path>,
) -> Vec<ScriptLogEntry> {
    let mut raw = Vec::new();
    let Ok(read_dir) = fs::read_dir(dir) else {
        return raw;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Some(parsed) = parse_entry_file(&path) {
            raw.push(parsed);
        }
    }

    raw.into_iter()
        .filter(|entry| mpv::is_under_a_tracked_root(&entry.source_path, tracked_roots, home))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self as std_fs, File};
    use std::io::Write;
    use tempfile::tempdir;

    fn write_entry(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std_fs::create_dir_all(parent).unwrap();
        }
        let mut f = File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std_fs::create_dir_all(parent).unwrap();
        }
        File::create(path).unwrap();
    }

    #[test]
    fn a_normal_eof_entry_parses_all_fields() {
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("abc123");
        write_entry(
            &entry_path,
            "# /home/user/Anime/Show/ep01.mkv\nduration=1420.500000\nposition=1418.250000\nreason=eof\n",
        );

        let entry = parse_entry_file(&entry_path).unwrap();
        assert_eq!(
            entry.source_path,
            PathBuf::from("/home/user/Anime/Show/ep01.mkv")
        );
        assert_eq!(entry.duration, Some(1420.5));
        assert_eq!(entry.position, Some(1418.25));
        assert_eq!(entry.reason, "eof");
    }

    #[test]
    fn empty_duration_and_position_values_parse_as_none() {
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("noduration");
        write_entry(
            &entry_path,
            "# /home/user/Anime/Show/ep01.mkv\nduration=\nposition=\nreason=quit\n",
        );

        let entry = parse_entry_file(&entry_path).unwrap();
        assert_eq!(entry.duration, None);
        assert_eq!(entry.position, None);
        assert_eq!(entry.reason, "quit");
    }

    #[test]
    fn a_redirect_entry_is_discarded() {
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("redirect");
        write_entry(
            &entry_path,
            "# /home/user/Anime/Show/ep01.mkv\nduration=\nposition=\nreason=redirect\n",
        );

        assert!(parse_entry_file(&entry_path).is_none());
    }

    #[test]
    fn a_malformed_entry_missing_the_comment_line_is_skipped() {
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("garbage");
        write_entry(&entry_path, "not a comment line at all\nreason=eof\n");

        assert!(parse_entry_file(&entry_path).is_none());
    }

    #[test]
    fn a_malformed_entry_missing_reason_is_skipped() {
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("noreason");
        write_entry(
            &entry_path,
            "# /home/user/Anime/Show/ep01.mkv\nduration=5.0\nposition=5.0\n",
        );

        assert!(parse_entry_file(&entry_path).is_none());
    }

    #[test]
    fn a_malformed_entry_with_unparseable_duration_is_skipped() {
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("baddur");
        write_entry(
            &entry_path,
            "# /home/user/Anime/Show/ep01.mkv\nduration=not-a-number\nposition=\nreason=eof\n",
        );

        assert!(parse_entry_file(&entry_path).is_none());
    }

    #[test]
    fn a_malformed_entry_alongside_a_valid_one_does_not_abort_the_scan() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let log_dir = dir.path().join("mpv-log");
        touch(&root.join("Show").join("ep01.mkv"));
        touch(&root.join("Show").join("ep02.mkv"));

        write_entry(
            &log_dir.join("valid"),
            &format!(
                "# {}\nduration=1000.0\nposition=990.0\nreason=eof\n",
                root.join("Show").join("ep01.mkv").display()
            ),
        );
        write_entry(&log_dir.join("malformed"), "garbage, no comment line\n");

        let tracked_roots = vec![root.to_string_lossy().into_owned()];
        let entries = scan_script_log(&log_dir, &tracked_roots, None);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].reason, "eof");
    }

    #[test]
    fn an_entry_outside_every_tracked_root_is_filtered_out() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();
        let log_dir = dir.path().join("mpv-log");

        write_entry(
            &log_dir.join("entry"),
            &format!(
                "# {}\nduration=\nposition=\nreason=eof\n",
                dir.path().join("SomewhereElse").join("clip.mkv").display()
            ),
        );

        let tracked_roots = vec![root.to_string_lossy().into_owned()];
        let entries = scan_script_log(&log_dir, &tracked_roots, None);
        assert!(entries.is_empty());
    }

    #[test]
    fn a_redirect_entry_never_survives_a_full_directory_scan() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let log_dir = dir.path().join("mpv-log");
        touch(&root.join("Show").join("ep01.mkv"));

        write_entry(
            &log_dir.join("entry"),
            &format!(
                "# {}\nduration=1000.0\nposition=999.0\nreason=redirect\n",
                root.join("Show").join("ep01.mkv").display()
            ),
        );

        let tracked_roots = vec![root.to_string_lossy().into_owned()];
        let entries = scan_script_log(&log_dir, &tracked_roots, None);
        assert!(entries.is_empty());
    }

    #[test]
    fn scanning_a_missing_directory_returns_an_empty_result_not_an_error() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let entries = scan_script_log(&missing, &[], None);
        assert!(entries.is_empty());
    }
}
