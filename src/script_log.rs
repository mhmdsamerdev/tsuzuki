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
//! The directory name itself is fixed and Tsuzuki-owned (no `[mpv]`
//! override for it, unlike `watch_later_dir`) — but which real filesystem
//! location that name resolves to does have install-method variance after
//! all, exactly like `watch_later` itself (see the correction below). mpv
//! never deletes a script-log entry once written, so presence here is
//! permanent, not a launcher-independent-but-load-deleted signal the way
//! `watch_later` entries are.
//!
//! Mirrors `mpv.rs`'s `scan_watch_later` shape deliberately, so the two read
//! side by side: parse-one-entry, skip malformed/redirect entries without
//! aborting the scan, filter to tracked roots via the same symlink-aware
//! `mpv::is_under_a_tracked_root` helper `watch_later` parsing already
//! relies on, and — as of the Flatpak correction below — a small
//! install-method candidate list merged freshest-file-wins, the exact same
//! shape `watch_later` scanning already needed.
//!
//! **Correction: this directory has install-method variance after all.**
//! CLAUDE.md's original Phase 6 decision assumed no candidate-list was
//! needed here — the directory name is Tsuzuki-owned and fixed, unlike
//! `watch_later`'s own install-method-dependent location. That reasoning
//! missed a real case, found by investigating a live "no logs ever appear"
//! report against a genuine Flatpak mpv install: Flatpak overrides
//! `XDG_DATA_HOME` for every sandboxed app to point into that app's own
//! private per-app data directory (`~/.var/app/io.mpv.Mpv/data`, verified
//! directly via `flatpak run --command=sh io.mpv.Mpv -c 'echo
//! $XDG_DATA_HOME'`), not the real `~/.local/share`. `tsuzuki.lua` reading
//! `os.getenv("XDG_DATA_HOME")` is exactly correct from inside the sandbox
//! — but it means a script running under Flatpak mpv writes to a
//! completely different real filesystem location than the one Tsuzuki
//! (running unsandboxed) computes for the *same nominal path* from its own
//! environment. The directory name being fixed and Tsuzuki-owned doesn't
//! matter if two different processes' environments resolve it to two
//! different real locations. Confirmed directly: real log entries were
//! found sitting at `~/.var/app/io.mpv.Mpv/data/tsuzuki/mpv-log/`,
//! genuinely written by the script, permanently invisible to Tsuzuki's own
//! single-fixed-path scan. The `host:ro` Flatpak permission also rules out
//! "just use `$HOME` instead of `$XDG_DATA_HOME`" as a fix on the script
//! side — that mount is read-only inside the sandbox, confirmed directly
//! (`mkdir` under it fails with "Read-only file system").
//!
//! Fix: `mpv_log_dirs()` returns a candidate list (native, resolved via
//! `dirs::data_dir()` exactly as before, plus the verified Flatpak-remapped
//! path) instead of one fixed directory, and `scan_script_log` merges
//! across all of them per source path, freshest-file-wins — the exact
//! precedent `mpv.rs`'s own cross-directory `watch_later` merge already
//! established. No Snap candidate is added: unlike the Flatpak case, Snap's
//! own `XDG_DATA_HOME` remapping behavior for a confined mpv has not been
//! verified against a real install, and this project doesn't add
//! candidate paths on a guess (CLAUDE.md §2). `[mpv]` still gets no new
//! config field for this — the candidate list, like `watch_later`'s own,
//! is a fixed table matched against install method, not a user-configured
//! override.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::mpv;
use crate::roots::normalize;

/// One candidate script-log directory beyond the native, `dirs::data_dir()`-
/// resolved one — tagged with the OS it applies to (matched against
/// `std::env::consts::OS`), mirroring `mpv.rs`'s own `WatchLaterCandidate`
/// table shape exactly, for the same reason: a data table, not scattered
/// `if cfg!(target_os)` checks, so a verified macOS/Windows entry can be
/// added later without restructuring this lookup.
struct FlatpakCandidate {
    os: &'static str,
    /// Joined onto the user's home directory.
    relative: &'static str,
}

/// Flatpak remaps `XDG_DATA_HOME` to this fixed, per-app path for every
/// sandboxed app — verified directly against a real `io.mpv.Mpv` Flatpak
/// install (see this module's own doc comment above). v1 is Linux-only
/// (CLAUDE.md §5); a macOS/Windows Flatpak equivalent is out of scope.
const FLATPAK_CANDIDATES: &[FlatpakCandidate] = &[FlatpakCandidate {
    os: "linux",
    relative: ".var/app/io.mpv.Mpv/data/tsuzuki/mpv-log",
}];

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

/// The native `$XDG_DATA_HOME/tsuzuki/mpv-log`, via `dirs::data_dir()` — the
/// same base directory `state::state_path()` resolves `state.toml` under,
/// just a different subdirectory. This is the directory *Tsuzuki's own*
/// environment resolves — correct for a native or Snap mpv (which shares
/// Tsuzuki's real `XDG_DATA_HOME`), but not for Flatpak mpv, which has its
/// own, different one (see [`mpv_log_dirs`]).
fn native_mpv_log_dir() -> io::Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "could not resolve data directory")
    })?;
    Ok(base.join("tsuzuki").join("mpv-log"))
}

/// Every directory that might hold a real script-log entry: the native one
/// plus any verified install-method-specific remap (currently just
/// Flatpak's). Order doesn't matter — [`scan_script_log`] merges across all
/// of them per source path, freshest-file-wins, exactly like `watch_later`'s
/// own cross-directory merge. A missing/unresolvable native directory
/// (`dirs::data_dir()` failing) simply isn't included — the Flatpak
/// candidate is still checked on its own; this function never errors.
pub fn mpv_log_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(native) = native_mpv_log_dir() {
        dirs.push(native);
    }
    if let Some(home) = home {
        dirs.extend(
            FLATPAK_CANDIDATES
                .iter()
                .filter(|c| c.os == std::env::consts::OS)
                .map(|c| home.join(c.relative)),
        );
    }
    dirs
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

/// Scans every directory in `dirs` for script-log entries, parses each,
/// discards redirects and malformed files, merges duplicates across
/// directories per source path (freshest entry-file mtime wins — the same
/// cross-directory precedent `mpv::scan_watch_later` already established),
/// and filters to entries whose source path falls under one of
/// `tracked_roots`. A directory that doesn't exist on disk is silently
/// skipped, not an error (the script may never have been installed, or a
/// given install method may simply not apply on this machine).
pub fn scan_script_log(
    dirs: &[PathBuf],
    tracked_roots: &[String],
    home: Option<&Path>,
) -> Vec<ScriptLogEntry> {
    let mut raw = Vec::new();
    for dir in dirs {
        let Ok(read_dir) = fs::read_dir(dir) else {
            continue;
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
    }

    merge_freshest_wins(raw, home)
        .into_iter()
        .filter(|entry| mpv::is_under_a_tracked_root(&entry.source_path, tracked_roots, home))
        .collect()
}

/// Merges entries by `source_path`, per-path freshest-file-wins: when the
/// same source path has an entry in more than one candidate directory (e.g.
/// a stale native-path entry from before switching to Flatpak mpv,
/// alongside a fresh Flatpak-path one), only the one whose entry file has
/// the newer mtime survives. Mirrors `mpv.rs`'s own `merge_freshest_wins`
/// exactly (a small, type-specific mirror rather than a shared generic —
/// the logic is a few lines, and the two entry types otherwise have nothing
/// in common worth abstracting over).
fn merge_freshest_wins(entries: Vec<ScriptLogEntry>, home: Option<&Path>) -> Vec<ScriptLogEntry> {
    let mut by_path: HashMap<PathBuf, ScriptLogEntry> = HashMap::new();
    for entry in entries {
        let key = normalize(&entry.source_path, home);
        match by_path.get(&key) {
            Some(existing) if existing.mtime >= entry.mtime => {}
            _ => {
                by_path.insert(key, entry);
            }
        }
    }
    by_path.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self as std_fs, File};
    use std::io::Write;
    use std::time::Duration;
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
        let entries = scan_script_log(std::slice::from_ref(&log_dir), &tracked_roots, None);

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
        let entries = scan_script_log(std::slice::from_ref(&log_dir), &tracked_roots, None);
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
        let entries = scan_script_log(std::slice::from_ref(&log_dir), &tracked_roots, None);
        assert!(entries.is_empty());
    }

    #[test]
    fn scanning_a_missing_directory_returns_an_empty_result_not_an_error() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let entries = scan_script_log(std::slice::from_ref(&missing), &[], None);
        assert!(entries.is_empty());
    }

    // --- Candidate directory list (the Flatpak correction) ---

    #[test]
    fn mpv_log_dirs_includes_the_flatpak_remapped_path_when_home_is_known() {
        // Regression test for a real, reported bug: Flatpak overrides
        // XDG_DATA_HOME for every sandboxed app to point into that app's
        // own private per-app data directory, so a script running under
        // Flatpak mpv never writes to the plain XDG_DATA_HOME-resolved
        // native path at all — confirmed directly against a real
        // `io.mpv.Mpv` Flatpak install. This candidate must always be
        // included whenever a home directory is known, regardless of
        // whether the native `dirs::data_dir()` call succeeds.
        let home = PathBuf::from("/home/user");
        let dirs = mpv_log_dirs(Some(&home));

        if std::env::consts::OS == "linux" {
            assert!(
                dirs.contains(&home.join(".var/app/io.mpv.Mpv/data/tsuzuki/mpv-log")),
                "expected the Flatpak-remapped candidate in {dirs:?}"
            );
        }
    }

    #[test]
    fn mpv_log_dirs_with_no_home_still_returns_the_native_candidate_only() {
        // No Flatpak candidate can be computed without a home directory to
        // join it onto — this must degrade to just the native candidate
        // (if `dirs::data_dir()` itself resolves), not panic or error.
        let dirs = mpv_log_dirs(None);
        assert!(
            dirs.len() <= 1,
            "with no home, only the native candidate (if any) may appear, got {dirs:?}"
        );
    }

    // --- Cross-directory merge: per-path freshest-file-wins ---

    fn set_mtime(path: &Path, when: SystemTime) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    #[test]
    fn same_source_path_across_two_candidate_directories_the_newer_mtime_wins() {
        // Simulates a user who used to have entries at the native path
        // (e.g. before switching to Flatpak mpv) and now also has a fresher
        // one at the Flatpak-remapped path — the fresher one must win,
        // mirroring `mpv.rs`'s own cross-directory `watch_later` merge.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("Show").join("ep01.mkv"));

        let native_dir = dir.path().join("native-log");
        let flatpak_dir = dir.path().join("flatpak-log");
        let source = root.join("Show").join("ep01.mkv");

        write_entry(
            &native_dir.join("entry"),
            &format!(
                "# {}\nduration=\nposition=100.0\nreason=quit\n",
                source.display()
            ),
        );
        write_entry(
            &flatpak_dir.join("entry"),
            &format!(
                "# {}\nduration=\nposition=200.0\nreason=quit\n",
                source.display()
            ),
        );

        let now = SystemTime::now();
        set_mtime(&native_dir.join("entry"), now - Duration::from_secs(60));
        set_mtime(&flatpak_dir.join("entry"), now);

        let tracked_roots = vec![root.to_string_lossy().into_owned()];
        let entries = scan_script_log(&[native_dir, flatpak_dir], &tracked_roots, None);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].position, Some(200.0));
    }

    #[test]
    fn merge_is_per_path_not_per_directory() {
        // A fresher duplicate of X in the second directory must not cause
        // the first directory's genuinely undisputed Y entry to be dropped
        // — same precedent as `mpv.rs`'s own equivalent test.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("Show").join("x.mkv"));
        touch(&root.join("Show").join("y.mkv"));

        let dir_a = dir.path().join("log_a");
        let dir_b = dir.path().join("log_b");
        let x = root.join("Show").join("x.mkv");
        let y = root.join("Show").join("y.mkv");

        write_entry(
            &dir_a.join("x-entry"),
            &format!("# {}\nduration=\nposition=10.0\nreason=quit\n", x.display()),
        );
        write_entry(
            &dir_a.join("y-entry"),
            &format!("# {}\nduration=\nposition=20.0\nreason=quit\n", y.display()),
        );
        write_entry(
            &dir_b.join("x-entry"),
            &format!("# {}\nduration=\nposition=99.0\nreason=quit\n", x.display()),
        );

        let now = SystemTime::now();
        set_mtime(&dir_a.join("x-entry"), now - Duration::from_secs(120));
        set_mtime(&dir_a.join("y-entry"), now - Duration::from_secs(120));
        set_mtime(&dir_b.join("x-entry"), now);

        let tracked_roots = vec![root.to_string_lossy().into_owned()];
        let entries = scan_script_log(&[dir_a, dir_b], &tracked_roots, None);

        assert_eq!(entries.len(), 2);
        let x_entry = entries.iter().find(|e| e.source_path == x).unwrap();
        let y_entry = entries.iter().find(|e| e.source_path == y).unwrap();
        assert_eq!(x_entry.position, Some(99.0), "dir_b's fresher X must win");
        assert_eq!(
            y_entry.position,
            Some(20.0),
            "dir_a's undisputed Y must survive even though dir_a also lost its X"
        );
    }
}
