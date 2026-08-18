//! Reading mpv's `watch_later` files (CLAUDE.md's "mpv `watch_later`
//! reading, not IPC or launching mpv", design.md §4).
//!
//! Tsuzuki never launches mpv or holds an IPC socket — it only reads mpv's
//! own `watch_later` entries after the fact, from a fixed list of candidate
//! locations (or a user-configured override), merges duplicates
//! freshest-file-wins, filters to tracked roots, and classifies each
//! position against the resume/completion thresholds. No caller yet (Phase
//! 5 wires this into `resume`/`status`/`current`/...) —
//! `#[allow(dead_code)]`, matching `series.rs`/`anitomy.rs`'s established
//! not-yet-wired convention.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::Config;
use crate::duration::DurationOutcome;
use crate::roots::{expand_tilde, normalize};
use crate::series;

/// One candidate `watch_later` directory location, tagged with the OS it
/// applies to (matched against `std::env::consts::OS`) — a data table, not
/// scattered `if cfg!(target_os)` checks, so macOS/Windows entries can be
/// added later (once their real paths are verified — design.md §4 marks
/// them unverified) without restructuring this lookup.
struct WatchLaterCandidate {
    os: &'static str,
    /// Joined onto the user's home directory.
    relative: &'static str,
}

/// design.md §4's exact candidate list. v1 is Linux-only (CLAUDE.md §5);
/// macOS/Windows rows are deliberately not added yet.
const CANDIDATES: &[WatchLaterCandidate] = &[
    WatchLaterCandidate {
        os: "linux",
        relative: ".local/state/mpv/watch_later",
    },
    WatchLaterCandidate {
        os: "linux",
        relative: ".cache/mpv/watch_later",
    },
    WatchLaterCandidate {
        os: "linux",
        relative: ".var/app/io.mpv.Mpv/.local/state/mpv/watch_later",
    },
    WatchLaterCandidate {
        os: "linux",
        relative: ".var/app/io.mpv.Mpv/.cache/mpv/watch_later",
    },
    WatchLaterCandidate {
        os: "linux",
        relative: "snap/mpv/current/.local/state/mpv/watch_later",
    },
];

/// The OS-default candidate `watch_later` directories, joined onto `home`.
/// Selecting by `std::env::consts::OS` keeps this one call site as the only
/// platform check — no per-call `cfg!(target_os = ...)` branching.
#[allow(dead_code)]
fn candidate_watch_later_dirs(home: &Path) -> Vec<PathBuf> {
    CANDIDATES
        .iter()
        .filter(|c| c.os == std::env::consts::OS)
        .map(|c| home.join(c.relative))
        .collect()
}

/// Resolves the directories to scan: `watch_later_dir_override` (if
/// non-empty) REPLACES the OS-default candidate list entirely — only that
/// one directory is scanned, defaults are not also merged in (CLAUDE.md's
/// "`watch_later_dir` config override"). If `home` can't be resolved and no
/// override is set, returns an empty list rather than guessing.
#[allow(dead_code)]
fn resolve_watch_later_dirs(watch_later_dir_override: &str, home: Option<&Path>) -> Vec<PathBuf> {
    if !watch_later_dir_override.is_empty() {
        return vec![expand_tilde(Path::new(watch_later_dir_override), home)];
    }
    match home {
        Some(home) => candidate_watch_later_dirs(home),
        None => Vec::new(),
    }
}

/// One parsed, tracked-root-filtered `watch_later` entry.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct WatchLaterEntry {
    /// The path mpv wrote in the entry's comment line — the file the entry
    /// is for. Not necessarily canonicalized.
    pub source_path: PathBuf,
    /// Parsed from the entry's `start=` line.
    pub position_seconds: f64,
    /// The entry file's own mtime — used both for the per-path
    /// freshest-wins merge and for picking the single most recent entry.
    pub mtime: SystemTime,
}

/// Parses one `watch_later` entry file. Returns `None` for a `# redirect
/// entry` (no real position — an internal mpv pointer from opening a
/// directory/playlist, not playback state) or for any entry that doesn't
/// fit the expected shape (missing `start=`, first line isn't a `#`
/// comment, unparseable float, unreadable mtime) — malformed entries are
/// skipped, never abort the caller's scan.
fn parse_entry_file(path: &Path) -> Option<WatchLaterEntry> {
    let contents = fs::read_to_string(path).ok()?;
    let mut lines = contents.lines();
    let first_line = lines.next()?;

    if first_line == "# redirect entry" {
        return None;
    }

    let source_str = first_line.strip_prefix("# ")?;
    if source_str.is_empty() {
        return None;
    }
    let source_path = PathBuf::from(source_str);

    let position_seconds = lines
        .find_map(|line| line.strip_prefix("start="))
        .and_then(|rest| rest.trim().parse::<f64>().ok())?;

    let mtime = fs::metadata(path).ok()?.modified().ok()?;

    Some(WatchLaterEntry {
        source_path,
        position_seconds,
        mtime,
    })
}

/// Keeps only entries whose `source_path` falls under one of `tracked_roots`
/// (reusing `roots::normalize`, the same tilde-expand-then-canonicalize
/// comparison `add`/`exclude` already use — not a second comparison
/// helper).
fn filter_to_tracked_roots(
    entries: Vec<WatchLaterEntry>,
    tracked_roots: &[String],
    home: Option<&Path>,
) -> Vec<WatchLaterEntry> {
    entries
        .into_iter()
        .filter(|entry| is_under_a_tracked_root(&entry.source_path, tracked_roots, home))
        .collect()
}

/// True if `source_path` resolves under any tracked root. Checks a cheap
/// whole-root prefix match first (the common case); only falls back to
/// canonicalizing the root's own depth-1 children individually when that
/// fails. The fallback is required, not an edge-case nicety: a depth-1
/// series folder can itself be a symlink pointing entirely outside the
/// root's own real location (CLAUDE.md's "Symlinks are followed
/// everywhere... a symlinked series folder behaves exactly like a real
/// one") — `fs::canonicalize`-ing the *whole* entry path resolves straight
/// through such a symlink to wherever it actually points, so a plain
/// root-vs-source prefix comparison can never match, and every
/// `watch_later` entry for that series would be silently invisible to
/// Tsuzuki forever. Confirmed as a real, reported bug, not a hypothetical:
/// a real-world tracked root with one symlinked series folder never
/// reconciled at all, while an ordinary (non-symlinked) sibling series
/// under the same root worked correctly.
pub(crate) fn is_under_a_tracked_root(
    source_path: &Path,
    tracked_roots: &[String],
    home: Option<&Path>,
) -> bool {
    let real_source = normalize(source_path, home);
    let tracked_root_real_paths: Vec<_> = tracked_roots
        .iter()
        .map(|r| normalize(Path::new(r), home))
        .collect();

    tracked_roots.iter().any(|root| {
        let real_root = normalize(Path::new(root), home);
        if real_source.starts_with(&real_root) {
            return true;
        }

        let expanded_root = expand_tilde(Path::new(root), home);
        let Ok(children) = series::series_in_root(&expanded_root, &tracked_root_real_paths) else {
            return false;
        };
        children.into_iter().any(|child| {
            let real_child = normalize(&child.path, home);
            real_source == real_child || real_source.starts_with(&real_child)
        })
    })
}

/// Merges entries by `source_path`, per-path freshest-file-wins: when the
/// same source path has an entry in more than one candidate directory, only
/// the one whose entry file has the newer mtime survives. Deliberately
/// per-path, not per-directory — a single fresh entry in an otherwise-stale
/// directory must not cause every other genuinely-valid entry in that
/// directory to be dropped.
fn merge_freshest_wins(entries: Vec<WatchLaterEntry>, home: Option<&Path>) -> Vec<WatchLaterEntry> {
    let mut by_path: HashMap<PathBuf, WatchLaterEntry> = HashMap::new();
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

/// A non-fatal issue surfaced by [`scan_watch_later`] alongside its
/// entries. Never printed by this module — it has no command-facing
/// output of its own yet (same as everything else in this phase); the
/// warning is returned as data for a future phase to actually display.
/// See CLAUDE.md's "An absent OS-default candidate directory stays
/// silent — an explicit `watch_later_dir` override that doesn't resolve
/// must be surfaced, not silent."
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanWarning {
    /// The configured `watch_later_dir` override does not exist on disk.
    /// Carries the value exactly as stored in `config.toml` (e.g.
    /// `~/mywatch`), not the expanded/resolved form — consistent with
    /// `AddError::NotFound`'s and the rest of the codebase's convention
    /// of showing the user the path they actually typed, not a form they
    /// may not recognize.
    ///
    /// Deliberately never produced for an absent OS-default candidate
    /// directory (e.g. no Flatpak mpv installed) — that case is normal
    /// and expected, and must stay exactly as silent as it is today.
    WatchLaterDirNotFound(PathBuf),
}

/// The result of [`scan_watch_later`]: the merged, filtered entries, plus
/// any non-fatal warning about the scan itself. `warning` is `None` in the
/// overwhelmingly common case; it's `Some` only when an explicit
/// `watch_later_dir` override was configured and didn't resolve to a real
/// directory on disk.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct ScanResult {
    pub entries: Vec<WatchLaterEntry>,
    pub warning: Option<ScanWarning>,
}

/// Scans every resolved `watch_later` directory, parses entries, filters to
/// tracked roots, and merges duplicates freshest-file-wins (design.md §4
/// steps 1-4, CLAUDE.md's cross-directory merge). A candidate directory
/// that doesn't exist on disk is silently skipped, not an error — except
/// when it's the sole result of an explicit `watch_later_dir` override,
/// in which case [`ScanResult::warning`] carries a
/// [`ScanWarning::WatchLaterDirNotFound`] alongside the (still empty)
/// entries. This function itself never errors and never prints anything;
/// the command still runs to completion either way.
#[allow(dead_code)]
pub fn scan_watch_later(config: &Config, home: Option<&Path>) -> ScanResult {
    let dirs = resolve_watch_later_dirs(&config.mpv.watch_later_dir, home);

    let mut raw = Vec::new();
    for dir in &dirs {
        if !dir.exists() {
            continue;
        }
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

    let filtered = filter_to_tracked_roots(raw, &config.roots.paths, home);
    let entries = merge_freshest_wins(filtered, home);
    let warning = override_not_found_warning(&config.mpv.watch_later_dir, home);

    ScanResult { entries, warning }
}

/// `Some` only when `watch_later_dir_override` is set and its expanded
/// form doesn't exist on disk — never for the OS-default candidate list,
/// where an absent directory is normal and expected (CLAUDE.md).
fn override_not_found_warning(
    watch_later_dir_override: &str,
    home: Option<&Path>,
) -> Option<ScanWarning> {
    if watch_later_dir_override.is_empty() {
        return None;
    }
    let expanded = expand_tilde(Path::new(watch_later_dir_override), home);
    if expanded.exists() {
        None
    } else {
        Some(ScanWarning::WatchLaterDirNotFound(PathBuf::from(
            watch_later_dir_override,
        )))
    }
}

/// The single most-recently-modified entry (by its entry file's own mtime)
/// among `entries` — design.md §4 step 5, "what you were watching."
#[allow(dead_code)]
pub fn most_recent_entry(entries: &[WatchLaterEntry]) -> Option<&WatchLaterEntry> {
    entries.iter().max_by_key(|e| e.mtime)
}

/// What a saved position (and, if known, the file's total duration) means
/// for playback, per CLAUDE.md's fixed resume/completion thresholds.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlaybackAction {
    /// Below `resume_seconds` — treated as noise, not a real bookmark.
    StartFromZero,
    /// At or above `resume_seconds`, and not within `completion_remaining_seconds` of the end.
    Resume(f64),
    /// Within `completion_remaining_seconds` of the end.
    Completed,
}

/// The result of classifying one entry's position against the configured
/// thresholds. `duration` is always carried through — including the
/// `DurationError` reason when unknown — rather than being dropped once
/// `action` is decided, so a caller (`doctor`, or a future diagnostic) can
/// still inspect why a completion check was skipped.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub action: PlaybackAction,
    pub duration: DurationOutcome,
}

/// Classifies a saved `position_seconds` against `resume_seconds` and
/// `completion_remaining_seconds` (CLAUDE.md's "Threshold classification").
/// Position is checked against `resume_seconds` first: a position below the
/// resume threshold is always `StartFromZero`, even in the edge case of a
/// very short file where the remaining time would also independently
/// qualify as `Completed` — see this phase's report for why this ordering
/// was chosen. When `duration` is `Unknown`, the completion check is
/// skipped for this entry only; the position-based classification still
/// applies.
#[allow(dead_code)]
pub fn classify(
    position_seconds: f64,
    duration: DurationOutcome,
    resume_seconds: u64,
    completion_remaining_seconds: u64,
) -> Classification {
    let action = if position_seconds < resume_seconds as f64 {
        PlaybackAction::StartFromZero
    } else if let DurationOutcome::Known(total_seconds) = duration {
        let remaining = total_seconds - position_seconds;
        if remaining <= completion_remaining_seconds as f64 {
            PlaybackAction::Completed
        } else {
            PlaybackAction::Resume(position_seconds)
        }
    } else {
        PlaybackAction::Resume(position_seconds)
    };

    Classification { action, duration }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duration::DurationError;
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

    fn set_mtime(path: &Path, when: SystemTime) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    // --- Entry parsing ---

    #[test]
    fn a_normal_entry_parses_path_and_position() {
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("abc123.mkv");
        write_entry(
            &entry_path,
            "# /home/user/Anime/Show/ep01.mkv\nstart=565.231000\n",
        );

        let entry = parse_entry_file(&entry_path).unwrap();
        assert_eq!(
            entry.source_path,
            PathBuf::from("/home/user/Anime/Show/ep01.mkv")
        );
        assert_eq!(entry.position_seconds, 565.231);
    }

    #[test]
    fn a_redirect_entry_is_discarded() {
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("redirect.mkv");
        write_entry(&entry_path, "# redirect entry\n");

        assert!(parse_entry_file(&entry_path).is_none());
    }

    #[test]
    fn a_redirect_entry_with_a_trailing_start_line_is_still_discarded() {
        // Gap-audit: the redirect check happens unconditionally on the
        // first line, before any `start=` scanning — so even a
        // hand-edited (or future-mpv-version) redirect entry that also
        // carries a `start=` line must still be discarded as a redirect,
        // not mistaken for a real entry just because a position happens
        // to be present.
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("redirect-with-start.mkv");
        write_entry(&entry_path, "# redirect entry\nstart=123.0\n");

        assert!(parse_entry_file(&entry_path).is_none());
    }

    #[test]
    fn a_malformed_entry_missing_start_is_skipped() {
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("no-start.mkv");
        write_entry(&entry_path, "# /home/user/Anime/Show/ep01.mkv\n");

        assert!(parse_entry_file(&entry_path).is_none());
    }

    #[test]
    fn a_malformed_entry_with_garbage_first_line_is_skipped() {
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("garbage.mkv");
        write_entry(&entry_path, "not a comment line at all\nstart=10.0\n");

        assert!(parse_entry_file(&entry_path).is_none());
    }

    #[test]
    fn a_malformed_entry_with_unparseable_start_value_is_skipped() {
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("bad-float.mkv");
        write_entry(
            &entry_path,
            "# /home/user/Anime/Show/ep01.mkv\nstart=not-a-number\n",
        );

        assert!(parse_entry_file(&entry_path).is_none());
    }

    #[test]
    fn a_malformed_entry_alongside_a_valid_one_does_not_abort_the_scan() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let watch_later = dir.path().join("watch_later");
        std_fs::create_dir_all(root.join("Show")).unwrap();
        touch(&root.join("Show").join("ep01.mkv"));
        touch(&root.join("Show").join("ep02.mkv"));

        write_entry(
            &watch_later.join("valid"),
            &format!(
                "# {}\nstart=42.0\n",
                root.join("Show").join("ep01.mkv").display()
            ),
        );
        write_entry(&watch_later.join("malformed"), "garbage, no comment line\n");

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.mpv.watch_later_dir = watch_later.to_string_lossy().into_owned();

        let result = scan_watch_later(&config, None);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].position_seconds, 42.0);
        assert_eq!(result.warning, None, "the override dir exists here");
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std_fs::create_dir_all(parent).unwrap();
        }
        File::create(path).unwrap();
    }

    // --- Tracked-root filtering ---

    #[test]
    fn an_entry_outside_every_tracked_root_is_filtered_out() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();

        let entries = vec![WatchLaterEntry {
            source_path: dir.path().join("SomewhereElse").join("clip.mkv"),
            position_seconds: 10.0,
            mtime: SystemTime::now(),
        }];

        let filtered =
            filter_to_tracked_roots(entries, &[root.to_string_lossy().into_owned()], None);
        assert!(filtered.is_empty());
    }

    #[test]
    fn an_entry_under_a_tracked_root_survives_filtering() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();

        let entries = vec![WatchLaterEntry {
            source_path: root.join("Show").join("ep01.mkv"),
            position_seconds: 10.0,
            mtime: SystemTime::now(),
        }];

        let filtered =
            filter_to_tracked_roots(entries, &[root.to_string_lossy().into_owned()], None);
        assert_eq!(filtered.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_under_a_symlinked_series_folder_survives_filtering() {
        // Regression test for a real, reported bug: a depth-1 series
        // folder that's itself a symlink pointing entirely outside the
        // root's own real location (a normal, expected library shape per
        // CLAUDE.md's "Symlinks are followed everywhere") was silently
        // dropped by this filter. `normalize` canonicalizes the *whole*
        // entry path, resolving straight through the symlink to
        // `real_storage`'s location — a plain prefix comparison against
        // the root's own (non-symlink-descended) canonical form can never
        // match that, so every watch_later entry for that series was
        // invisible, permanently. Uses real files/symlinks, not fabricated
        // paths, since `normalize`'s canonicalize-with-fallback silently
        // no-ops on paths that don't exist on disk at all — this bug is
        // only reachable with real symlink resolution.
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();

        let real_storage = dir.path().join("real_storage").join("Dungeon Meshi");
        std_fs::create_dir_all(&real_storage).unwrap();
        std_fs::write(real_storage.join("ep01.mkv"), b"").unwrap();
        symlink(&real_storage, root.join("Dungeon Meshi")).unwrap();

        let entries = vec![WatchLaterEntry {
            source_path: root.join("Dungeon Meshi").join("ep01.mkv"),
            position_seconds: 685.852,
            mtime: SystemTime::now(),
        }];

        let filtered =
            filter_to_tracked_roots(entries, &[root.to_string_lossy().into_owned()], None);
        assert_eq!(
            filtered.len(),
            1,
            "an entry for a file reached through a symlinked series folder must not be \
             silently filtered out"
        );
    }

    // --- Gap-audit: real-world-shaped states (root/file missing on disk) ---

    #[test]
    fn filtering_does_not_panic_when_a_tracked_root_no_longer_exists_on_disk() {
        // A root can be removed from the filesystem after being tracked
        // (before Phase 6's `doctor` would ever flag it) — `normalize`
        // falls back to the tilde-expanded, uncanonicalized form when
        // `fs::canonicalize` fails, so this must filter cleanly rather
        // than panicking or erroring.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime"); // deliberately never created

        let entries = vec![WatchLaterEntry {
            source_path: root.join("Show").join("ep01.mkv"),
            position_seconds: 10.0,
            mtime: SystemTime::now(),
        }];

        let filtered =
            filter_to_tracked_roots(entries, &[root.to_string_lossy().into_owned()], None);
        // Neither side canonicalizes (root doesn't exist, so its
        // `normalize` also falls back to the literal expanded form) — the
        // literal prefix relationship still holds, so the entry survives.
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filtering_does_not_panic_when_the_entrys_source_file_no_longer_exists_on_disk() {
        // The video file a watch_later entry points at may have been
        // deleted or moved since mpv wrote the entry. The tracked root
        // itself still exists here (and so *does* get canonicalized by
        // `normalize`), while the missing source file falls back to its
        // uncanonicalized literal form — this must not panic, and in the
        // common no-symlink case (true for a plain tempdir on this
        // environment) the literal prefix relationship still holds.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();
        // The source file is never created — it's "since deleted."

        let entries = vec![WatchLaterEntry {
            source_path: root.join("Show").join("ep01.mkv"),
            position_seconds: 10.0,
            mtime: SystemTime::now(),
        }];

        let filtered =
            filter_to_tracked_roots(entries, &[root.to_string_lossy().into_owned()], None);
        assert_eq!(
            filtered.len(),
            1,
            "a deleted source file's entry must still filter cleanly, not panic or vanish"
        );
    }

    #[test]
    fn scan_watch_later_does_not_panic_when_a_tracked_root_is_missing_from_disk() {
        let home = tempdir().unwrap();
        let root = home.path().join("Anime"); // deliberately never created

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        // Must not panic; a tracked root with nothing on disk simply
        // yields no matching entries.
        let result = scan_watch_later(&config, Some(home.path()));
        assert!(result.entries.is_empty());
        assert_eq!(result.warning, None);
    }

    // --- Cross-directory merge: per-path freshest-file-wins ---

    #[test]
    fn same_source_path_in_two_directories_the_newer_mtime_wins() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("Show").join("ep01.mkv"));

        let dir_a = dir.path().join("watch_later_a");
        let dir_b = dir.path().join("watch_later_b");
        let source = root.join("Show").join("ep01.mkv");

        write_entry(
            &dir_a.join("entry"),
            &format!("# {}\nstart=100.0\n", source.display()),
        );
        write_entry(
            &dir_b.join("entry"),
            &format!("# {}\nstart=200.0\n", source.display()),
        );

        let now = SystemTime::now();
        set_mtime(&dir_a.join("entry"), now - Duration::from_secs(60));
        set_mtime(&dir_b.join("entry"), now);

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        // Scan both directories directly via the lower-level pieces, since
        // `watch_later_dir` only ever points at one directory.
        let mut raw = Vec::new();
        for dir in [&dir_a, &dir_b] {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                if let Some(parsed) = parse_entry_file(&entry.path()) {
                    raw.push(parsed);
                }
            }
        }
        let filtered = filter_to_tracked_roots(raw, &config.roots.paths, None);
        let merged = merge_freshest_wins(filtered, None);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].position_seconds, 200.0);
    }

    #[test]
    fn merge_is_per_path_not_per_directory() {
        // dir_a has a stale entry for X and a fresh entry for Y (no
        // duplicate of Y anywhere else). dir_b has a fresher duplicate of
        // X. Both X (from dir_b) and Y (from dir_a) must survive the
        // merge — a fresh entry in dir_b must not cause dir_a's genuinely
        // valid, undisputed Y entry to be dropped.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("Show").join("x.mkv"));
        touch(&root.join("Show").join("y.mkv"));

        let dir_a = dir.path().join("watch_later_a");
        let dir_b = dir.path().join("watch_later_b");
        let x = root.join("Show").join("x.mkv");
        let y = root.join("Show").join("y.mkv");

        write_entry(
            &dir_a.join("x-entry"),
            &format!("# {}\nstart=10.0\n", x.display()),
        );
        write_entry(
            &dir_a.join("y-entry"),
            &format!("# {}\nstart=20.0\n", y.display()),
        );
        write_entry(
            &dir_b.join("x-entry"),
            &format!("# {}\nstart=99.0\n", x.display()),
        );

        let now = SystemTime::now();
        set_mtime(&dir_a.join("x-entry"), now - Duration::from_secs(120));
        set_mtime(&dir_a.join("y-entry"), now - Duration::from_secs(120));
        set_mtime(&dir_b.join("x-entry"), now);

        let mut raw = Vec::new();
        for dir in [&dir_a, &dir_b] {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                if let Some(parsed) = parse_entry_file(&entry.path()) {
                    raw.push(parsed);
                }
            }
        }
        let filtered = filter_to_tracked_roots(raw, &[root.to_string_lossy().into_owned()], None);
        let merged = merge_freshest_wins(filtered, None);

        assert_eq!(merged.len(), 2);
        let x_entry = merged.iter().find(|e| e.source_path == x).unwrap();
        let y_entry = merged.iter().find(|e| e.source_path == y).unwrap();
        assert_eq!(x_entry.position_seconds, 99.0, "dir_b's fresher X must win");
        assert_eq!(
            y_entry.position_seconds, 20.0,
            "dir_a's undisputed Y must survive even though dir_a also lost its X"
        );
    }

    // --- Gap-audit: merge_freshest_wins with 3+ duplicates and exact ties ---
    //
    // These call `merge_freshest_wins` directly with an explicit `Vec`
    // input order, rather than going through a filesystem scan — the
    // function's own fold-over-a-HashMap semantics are what's under test
    // here (independent of `fs::read_dir`'s unspecified enumeration
    // order across directories, which is a separate, filesystem-level
    // non-guarantee, not something `merge_freshest_wins` controls).

    #[test]
    fn three_way_duplicate_the_freshest_wins_even_when_processed_first() {
        // Ordering the freshest entry FIRST (not last, not in the middle)
        // specifically rules out an implementation that accidentally
        // tracks "last one seen for this key" instead of genuinely
        // comparing every duplicate's mtime — that bug would silently
        // pick the last-processed (stalest) entry here instead of the
        // truly freshest one.
        let now = SystemTime::now();
        let path = PathBuf::from("/Anime/Show/ep01.mkv");
        let entries = vec![
            WatchLaterEntry {
                source_path: path.clone(),
                position_seconds: 999.0,
                mtime: now, // freshest, processed first
            },
            WatchLaterEntry {
                source_path: path.clone(),
                position_seconds: 10.0,
                mtime: now - Duration::from_secs(100),
            },
            WatchLaterEntry {
                source_path: path.clone(),
                position_seconds: 20.0,
                mtime: now - Duration::from_secs(50),
            },
        ];

        let merged = merge_freshest_wins(entries, None);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].position_seconds, 999.0);
    }

    #[test]
    fn merge_on_an_exact_mtime_tie_is_deterministic() {
        // Two entries for the same path with an identical mtime. Given
        // `merge_freshest_wins`'s own semantics (`existing.mtime >=
        // entry.mtime` keeps whatever's already in the map), the first
        // entry in input order wins a tie, deterministically — this test
        // locks that behavior in explicitly rather than leaving it
        // implicit. (A genuine exact-mtime tie between two *different*
        // watch_later files for the same source is an extremely rare
        // real-world case; either choice would be equally defensible —
        // what matters is that the function itself isn't order-random.)
        let now = SystemTime::now();
        let path = PathBuf::from("/Anime/Show/ep01.mkv");
        let entries = vec![
            WatchLaterEntry {
                source_path: path.clone(),
                position_seconds: 111.0,
                mtime: now,
            },
            WatchLaterEntry {
                source_path: path.clone(),
                position_seconds: 222.0,
                mtime: now,
            },
        ];

        let merged = merge_freshest_wins(entries, None);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].position_seconds, 111.0,
            "on an exact tie, the first entry in input order wins"
        );
    }

    // --- watch_later_dir override ---

    #[test]
    fn override_replaces_the_default_candidate_list_entirely() {
        let home = tempdir().unwrap();
        let root = home.path().join("Anime");
        touch(&root.join("Show").join("ep01.mkv"));

        // A valid entry sitting only in a default-candidate path.
        let default_candidate = home.path().join(".local/state/mpv/watch_later");
        write_entry(
            &default_candidate.join("entry"),
            &format!(
                "# {}\nstart=42.0\n",
                root.join("Show").join("ep01.mkv").display()
            ),
        );

        // The override points elsewhere entirely, with nothing in it.
        let override_dir = home.path().join("custom_watch_later");
        std_fs::create_dir_all(&override_dir).unwrap();

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.mpv.watch_later_dir = override_dir.to_string_lossy().into_owned();

        let result = scan_watch_later(&config, Some(home.path()));
        assert!(
            result.entries.is_empty(),
            "the default-candidate entry must be absent once an override is set"
        );
        assert_eq!(
            result.warning, None,
            "the override dir exists here, just empty"
        );
    }

    #[test]
    fn empty_override_falls_back_to_os_default_candidates() {
        let home = tempdir().unwrap();
        let root = home.path().join("Anime");
        touch(&root.join("Show").join("ep01.mkv"));

        let default_candidate = home.path().join(".local/state/mpv/watch_later");
        write_entry(
            &default_candidate.join("entry"),
            &format!(
                "# {}\nstart=42.0\n",
                root.join("Show").join("ep01.mkv").display()
            ),
        );

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        // watch_later_dir left at its default "" value.

        let result = scan_watch_later(&config, Some(home.path()));
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].position_seconds, 42.0);
        assert_eq!(result.warning, None);
    }

    #[test]
    fn os_default_mode_with_every_candidate_directory_absent_yields_zero_entries_and_zero_warnings()
    {
        // The asymmetry CLAUDE.md requires: an absent OS-default candidate
        // is normal/expected and must stay exactly as silent as before —
        // unlike the override case (see the test right below this one),
        // this must produce NO warning no matter how many (or all) of the
        // candidate directories are missing. Proven as its own explicit
        // test rather than inferred from the override test, since the two
        // cases must behave differently, not identically.
        let home = tempdir().unwrap();
        let root = home.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();
        // Nothing created under any watch_later candidate path at all —
        // every one of the 5 Linux candidates is absent.
        // watch_later_dir left at its default "" value (OS-default mode).

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let result = scan_watch_later(&config, Some(home.path()));
        assert!(result.entries.is_empty());
        assert_eq!(
            result.warning, None,
            "an absent OS-default candidate directory must never produce a warning"
        );
    }

    #[test]
    fn a_watch_later_dir_override_pointing_at_a_nonexistent_path_surfaces_a_warning() {
        // CLAUDE.md's "An absent OS-default candidate directory stays
        // silent — an explicit watch_later_dir override that doesn't
        // resolve must be surfaced, not silent." Unlike the OS-default
        // case above, `watch_later_dir` is something the user explicitly
        // typed — a mistake there must be discoverable. This still isn't
        // an error: the scan completes and returns zero entries exactly
        // as before, but now also carries a warning naming the configured
        // (as-typed, unexpanded) path.
        let home = tempdir().unwrap();
        let root = home.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        let configured_override = home
            .path()
            .join("this_path_was_never_created")
            .to_string_lossy()
            .into_owned();
        config.mpv.watch_later_dir = configured_override.clone();

        let result = scan_watch_later(&config, Some(home.path()));
        assert!(result.entries.is_empty());
        assert_eq!(
            result.warning,
            Some(ScanWarning::WatchLaterDirNotFound(PathBuf::from(
                configured_override
            )))
        );
    }

    #[test]
    fn the_warning_carries_the_literal_tilde_form_not_the_expanded_path() {
        // Matches the rest of the codebase's convention (e.g.
        // `AddError::NotFound`, roots persisted "in the form the user
        // typed") — the warning shows the user the value they actually
        // put in config.toml, not an expanded form they may not
        // recognize.
        let home = tempdir().unwrap();
        let root = home.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();
        // Deliberately never create "~/this_never_exists" under `home`.

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.mpv.watch_later_dir = "~/this_never_exists".to_string();

        let result = scan_watch_later(&config, Some(home.path()));
        assert_eq!(
            result.warning,
            Some(ScanWarning::WatchLaterDirNotFound(PathBuf::from(
                "~/this_never_exists"
            )))
        );
    }

    // --- Candidate path table ---

    #[test]
    fn linux_candidates_match_design_doc_exactly() {
        let home = PathBuf::from("/home/user");
        let dirs = candidate_watch_later_dirs(&home);

        if std::env::consts::OS == "linux" {
            assert_eq!(
                dirs,
                vec![
                    home.join(".local/state/mpv/watch_later"),
                    home.join(".cache/mpv/watch_later"),
                    home.join(".var/app/io.mpv.Mpv/.local/state/mpv/watch_later"),
                    home.join(".var/app/io.mpv.Mpv/.cache/mpv/watch_later"),
                    home.join("snap/mpv/current/.local/state/mpv/watch_later"),
                ]
            );
        }
    }

    #[test]
    fn resolve_with_no_home_and_no_override_returns_empty() {
        let dirs = resolve_watch_later_dirs("", None);
        assert!(dirs.is_empty());
    }

    // --- most_recent_entry ---

    #[test]
    fn most_recent_entry_picks_the_newest_mtime() {
        let now = SystemTime::now();
        let entries = vec![
            WatchLaterEntry {
                source_path: PathBuf::from("/a.mkv"),
                position_seconds: 1.0,
                mtime: now - Duration::from_secs(100),
            },
            WatchLaterEntry {
                source_path: PathBuf::from("/b.mkv"),
                position_seconds: 2.0,
                mtime: now,
            },
        ];

        let most_recent = most_recent_entry(&entries).unwrap();
        assert_eq!(most_recent.source_path, PathBuf::from("/b.mkv"));
    }

    #[test]
    fn most_recent_entry_on_empty_slice_returns_none() {
        assert!(most_recent_entry(&[]).is_none());
    }

    // --- Threshold classification ---

    #[test]
    fn position_just_below_resume_threshold_starts_from_zero() {
        let result = classify(
            119.9,
            DurationOutcome::Unknown(DurationError::Unreadable),
            120,
            180,
        );
        assert_eq!(result.action, PlaybackAction::StartFromZero);
    }

    #[test]
    fn position_exactly_at_resume_threshold_is_not_start_from_zero() {
        let result = classify(
            120.0,
            DurationOutcome::Unknown(DurationError::Unreadable),
            120,
            180,
        );
        assert_ne!(result.action, PlaybackAction::StartFromZero);
    }

    #[test]
    fn position_just_above_resume_threshold_is_not_start_from_zero() {
        let result = classify(
            120.1,
            DurationOutcome::Unknown(DurationError::Unreadable),
            120,
            180,
        );
        assert_ne!(result.action, PlaybackAction::StartFromZero);
    }

    #[test]
    fn remaining_just_above_completion_threshold_resumes() {
        // duration 1000, position such that remaining = 180.1 (> 180 threshold)
        let result = classify(819.9, DurationOutcome::Known(1000.0), 120, 180);
        assert_eq!(result.action, PlaybackAction::Resume(819.9));
    }

    #[test]
    fn remaining_exactly_at_completion_threshold_is_completed() {
        // remaining = 180.0, at the threshold.
        let result = classify(820.0, DurationOutcome::Known(1000.0), 120, 180);
        assert_eq!(result.action, PlaybackAction::Completed);
    }

    #[test]
    fn remaining_just_below_completion_threshold_is_completed() {
        let result = classify(820.1, DurationOutcome::Known(1000.0), 120, 180);
        assert_eq!(result.action, PlaybackAction::Completed);
    }

    #[test]
    fn overlap_case_on_a_short_file_start_from_zero_wins_over_completed() {
        // CLAUDE.md's "Threshold precedence when position + completion
        // checks overlap (short files)": on a short enough file, a single
        // saved position can satisfy both `resume_seconds` (small enough
        // to count as noise) and `completion_remaining_seconds` (close
        // enough to that file's own ending) simultaneously. Here:
        // duration = 100s, position = 50s. position (50) < resume_seconds
        // (120) -> satisfies the StartFromZero condition. remaining
        // (100 - 50 = 50) <= completion_remaining_seconds (180) ->
        // independently satisfies the Completed condition too. The
        // documented, deliberate resolution is that the position check
        // wins: StartFromZero, never Completed.
        let result = classify(50.0, DurationOutcome::Known(100.0), 120, 180);
        assert_eq!(result.action, PlaybackAction::StartFromZero);
    }

    #[test]
    fn unknown_duration_skips_completion_check_but_still_resumes_from_position() {
        let outcome = DurationOutcome::Unknown(DurationError::Unreadable);
        let result = classify(500.0, outcome.clone(), 120, 180);
        assert_eq!(result.action, PlaybackAction::Resume(500.0));
        assert_eq!(result.duration, outcome);
    }

    #[test]
    fn unknown_duration_reason_is_carried_through_even_when_position_based_classification_applies()
    {
        let outcome = DurationOutcome::Unknown(DurationError::Io("permission denied".to_string()));
        let result = classify(500.0, outcome.clone(), 120, 180);
        assert_eq!(result.duration, outcome);
    }
}
