//! Reads mpv's `watch_later` entries: scans the candidate directories,
//! merges duplicates freshest-file-wins, filters to tracked roots, and
//! classifies positions against the resume/completion thresholds.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::Config;
use crate::duration::DurationOutcome;
use crate::roots::{expand_tilde, normalize};
use crate::series;

/// One candidate `watch_later` directory, tagged with the OS it applies to
/// (matched against `std::env::consts::OS`). A data table, so other OSes can
/// be added without restructuring the lookup.
struct WatchLaterCandidate {
    os: &'static str,
    /// Joined onto the user's home directory.
    relative: &'static str,
}

/// Linux candidates only; macOS/Windows paths are unverified.
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
/// `pub(crate)` so `doctor.rs` can report per-candidate reachability.
pub(crate) fn candidate_watch_later_dirs(home: &Path) -> Vec<PathBuf> {
    CANDIDATES
        .iter()
        .filter(|c| c.os == std::env::consts::OS)
        .map(|c| home.join(c.relative))
        .collect()
}

/// Resolves the directories to scan. A non-empty `watch_later_dir_override`
/// replaces the OS-default list entirely. With no override and no `home`,
/// returns an empty list rather than guessing.
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

/// Parses one `watch_later` entry file. Returns `None` for a redirect entry
/// (no real position) and for malformed entries (missing `start=`, first line
/// not a `#` comment, unparseable float, unreadable mtime), so one bad entry
/// never aborts a scan.
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

/// Keeps only entries whose `source_path` falls under one of `tracked_roots`.
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

/// True if `source_path` resolves under any tracked root. Tries a whole-root
/// prefix match first, then falls back to each depth-1 series folder
/// individually: a symlinked series folder canonicalizes to a path outside
/// the root, so the prefix match alone would never see its entries.
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

/// Merges entries by `source_path`, keeping the one with the newest mtime per
/// path. Per-path, not per-directory: a fresh entry in one directory must not
/// drop the other entries there.
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

/// A non-fatal issue surfaced by [`scan_watch_later`] alongside its entries,
/// returned as data for the caller to display.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanWarning {
    /// The configured `watch_later_dir` override does not exist on disk.
    /// Carries the value as typed in `config.toml`, not the expanded form.
    /// Never produced for an absent OS-default directory, which is normal.
    WatchLaterDirNotFound(PathBuf),
}

/// The result of [`scan_watch_later`]. `warning` is `Some` only when an
/// explicit `watch_later_dir` override doesn't resolve to a directory.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct ScanResult {
    pub entries: Vec<WatchLaterEntry>,
    pub warning: Option<ScanWarning>,
}

/// Scans every resolved `watch_later` directory, parses entries, filters to
/// tracked roots, and merges duplicates freshest-file-wins. A missing
/// directory is skipped silently, except an explicit override, which yields
/// [`ScanWarning::WatchLaterDirNotFound`]. Never errors or prints.
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

/// `Some` only when the override is set and its expanded form doesn't exist.
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

/// The entry with the newest mtime among `entries`.
#[allow(dead_code)]
pub fn most_recent_entry(entries: &[WatchLaterEntry]) -> Option<&WatchLaterEntry> {
    entries.iter().max_by_key(|e| e.mtime)
}

/// What a saved position (and, if known, the file's duration) means for
/// playback.
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

/// The result of classifying one entry's position. `duration` is carried
/// through, including the `DurationError` reason when unknown, so callers can
/// see why the completion check was skipped.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub action: PlaybackAction,
    pub duration: DurationOutcome,
}

/// Classifies `position_seconds` against the thresholds. The `resume_seconds`
/// check comes first, so a short file never classifies as `Completed` from a
/// position below it. With an unknown `duration` the completion check is
/// skipped.
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
        // The redirect check runs on the first line before any `start=` scan.
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
        // `normalize` resolves the whole entry path through the symlink, so a
        // plain root-prefix match fails. Needs real symlinks: `normalize` is a
        // no-op on paths that don't exist.
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

    // --- Missing root/file on disk ---

    #[test]
    fn filtering_does_not_panic_when_a_tracked_root_no_longer_exists_on_disk() {
        // `normalize` falls back to the uncanonicalized path when the root
        // is gone.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime"); // deliberately never created

        let entries = vec![WatchLaterEntry {
            source_path: root.join("Show").join("ep01.mkv"),
            position_seconds: 10.0,
            mtime: SystemTime::now(),
        }];

        let filtered =
            filter_to_tracked_roots(entries, &[root.to_string_lossy().into_owned()], None);
        // Both sides fall back to literal paths, so the prefix still matches.
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filtering_does_not_panic_when_the_entrys_source_file_no_longer_exists_on_disk() {
        // The root canonicalizes but the missing file falls back to its
        // literal path; the prefix still matches.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();
        // The source file is never created.

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

        // A missing root yields no entries.
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
        // Scan both directories directly; `watch_later_dir` points at one.
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
        // dir_b holds a fresher X; dir_a's undisputed Y must still survive.
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

    // --- merge_freshest_wins with 3+ duplicates and exact ties ---
    //
    // Called directly with explicit input order, independent of `read_dir`
    // enumeration order.

    #[test]
    fn three_way_duplicate_the_freshest_wins_even_when_processed_first() {
        // The freshest entry comes first, ruling out a last-seen-wins bug.
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
        // `existing.mtime >= entry.mtime` keeps the first entry on a tie.
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
        // Unlike the override case below, an absent default candidate must
        // never warn.
        let home = tempdir().unwrap();
        let root = home.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();
        // None of the candidate directories exist; `watch_later_dir` is unset.

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
        // An explicit override that doesn't resolve is surfaced as a warning,
        // not an error; the scan still returns zero entries.
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
        // The warning shows the value as typed in config.toml.
        let home = tempdir().unwrap();
        let root = home.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();
        // "~/this_never_exists" is never created under `home`.

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
    fn position_exactly_at_resume_threshold_resumes_from_that_position() {
        let result = classify(
            120.0,
            DurationOutcome::Unknown(DurationError::Unreadable),
            120,
            180,
        );
        assert_eq!(
            result,
            Classification {
                action: PlaybackAction::Resume(120.0),
                duration: DurationOutcome::Unknown(DurationError::Unreadable),
            }
        );
    }

    #[test]
    fn position_just_above_resume_threshold_resumes_from_that_position() {
        let result = classify(
            120.1,
            DurationOutcome::Unknown(DurationError::Unreadable),
            120,
            180,
        );
        assert_eq!(
            result,
            Classification {
                action: PlaybackAction::Resume(120.1),
                duration: DurationOutcome::Unknown(DurationError::Unreadable),
            }
        );
    }

    #[test]
    fn remaining_just_above_completion_threshold_resumes() {
        // remaining = 180.1, above the 180 threshold.
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
        // duration 100, position 50: below `resume_seconds` (120) and within
        // `completion_remaining_seconds` (180) of the end. The position check
        // wins.
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
