//! Reads the mpv mpv script's log directory, tsuzuki's only view of mpv.
//!
//! `contrib/mpv/tsuzuki.lua` atomically writes one text file per played path
//! to `$XDG_DATA_HOME/tsuzuki/mpv-log`, named by an 8-hex-char djb2 hash of
//! the path:
//!
//! ```text
//! # <original path>
//! duration=<float or empty>
//! position=<float or empty>
//! reason=<eof|stop|quit|error|redirect|unknown>
//! ```
//!
//! The comment-line path is the identity check; the hash is never trusted.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use crate::roots::{expand_tilde, normalize};
use crate::series;

/// A script-log directory besides the native one, tagged with the OS it
/// applies to (`std::env::consts::OS`).
struct FlatpakCandidate {
    os: &'static str,
    /// Joined onto the user's home directory.
    relative: &'static str,
}

/// Flatpak mpv runs with `XDG_DATA_HOME` set to its per-app data dir, so the
/// script logs here instead of the native path. Linux-only; Snap isn't
/// supported (see `mpv`).
const FLATPAK_CANDIDATES: &[FlatpakCandidate] = &[FlatpakCandidate {
    os: "linux",
    relative: ".var/app/io.mpv.Mpv/data/tsuzuki/mpv-log",
}];

/// One parsed script-log entry under a tracked root.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptLogEntry {
    /// The file the entry is for, as written in the comment line.
    pub source_path: PathBuf,
    /// `None` when the `duration=` line was empty.
    pub duration: Option<f64>,
    /// `None` when the `position=` line was empty.
    pub position: Option<f64>,
    /// One of `eof`/`stop`/`quit`/`error`/`unknown`; redirects never get an entry.
    pub reason: String,
    /// The `title=` line: mpv's media title, written for stream URLs only.
    pub title: Option<String>,
    /// The entry file's mtime: when playback of this file last ended.
    pub mtime: SystemTime,
}

/// Native `$XDG_DATA_HOME/tsuzuki/mpv-log`, next to `state.toml`. Correct for
/// native mpv, not Flatpak (see [`mpv_log_dirs`]).
fn native_mpv_log_dir() -> io::Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "could not resolve data directory")
    })?;
    Ok(base.join("tsuzuki").join("mpv-log"))
}

/// Every directory that might hold script-log entries: the native one (if
/// it resolves) plus the Flatpak remap when `home` is known. Independent of
/// the mpv target on purpose, so entries from any mpv the user ran are read.
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

/// Parses one entry file. `None` for `reason=redirect` entries and for
/// malformed ones (missing or empty path line, missing `reason=`, unparseable
/// `duration=`/`position=`, unreadable mtime).
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
    let mut title: Option<String> = None;

    for line in lines {
        if let Some(rest) = line.strip_prefix("duration=") {
            if !rest.is_empty() {
                duration = Some(rest.trim().parse::<f64>().ok()?);
            }
        } else if let Some(rest) = line.strip_prefix("position=") {
            if !rest.is_empty() {
                position = Some(rest.trim().parse::<f64>().ok()?);
            }
        } else if let Some(rest) = line.strip_prefix("title=") {
            if !rest.is_empty() {
                title = Some(rest.to_string());
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
        title,
        mtime,
    })
}

/// What one pass over the script-log directories yields.
pub(crate) struct ScriptLogScan {
    /// Local-file entries under a tracked root, keyed by canonical path.
    pub files: BTreeMap<PathBuf, ScriptLogEntry>,
    /// Stream entries (those with a `title=` line), the freshest per title.
    pub streams: Vec<ScriptLogEntry>,
}

/// Parses every entry file in `dirs`, drops redirects and malformed files,
/// keeps the freshest entry per source path, and filters to entries under
/// `tracked_roots`. Keyed by the source path's canonical form, resolved once
/// here. Missing directories are skipped.
#[cfg(test)]
pub fn scan_script_log(
    dirs: &[PathBuf],
    tracked_roots: &[String],
    home: Option<&Path>,
) -> BTreeMap<PathBuf, ScriptLogEntry> {
    scan_script_log_full(dirs, &resolve_roots(tracked_roots, home), home).files
}

/// [`scan_script_log`] against roots the caller already resolved, also
/// returning the stream entries.
pub(crate) fn scan_script_log_full(
    dirs: &[PathBuf],
    resolved: &[ResolvedRoot],
    home: Option<&Path>,
) -> ScriptLogScan {
    let mut files = Vec::new();
    let mut streams = Vec::new();
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
                if parsed.title.is_some() {
                    streams.push(parsed);
                } else {
                    files.push(parsed);
                }
            }
        }
    }

    let mut files = merge_freshest_wins(files, home);
    files.retain(|real_source, entry| {
        is_under_a_tracked_root(real_source, resolved)
            || logged_series(resolved, &entry.source_path).is_some()
    });
    ScriptLogScan {
        files,
        streams: merge_streams_freshest_wins(streams),
    }
}

/// A tracked root resolved once per run, so matching log entries never
/// rescans the root.
pub(crate) struct ResolvedRoot {
    /// The root as stored in `config.toml`.
    pub root: String,
    /// The root with `~` expanded, symlinks not resolved: the prefix a logged
    /// path carries.
    pub expanded_root: PathBuf,
    pub real_root: PathBuf,
    /// `(series name, canonical series path)` for series folders that resolve
    /// outside `real_root` (symlinked out): the only ones a prefix match on
    /// `real_root` can't see. Empty when the root can't be read.
    pub outside_children: Vec<(String, PathBuf)>,
}

/// Resolves every tracked root and its series folders.
pub(crate) fn resolve_roots(tracked_roots: &[String], home: Option<&Path>) -> Vec<ResolvedRoot> {
    let tracked_root_real_paths: Vec<_> = tracked_roots
        .iter()
        .map(|r| normalize(Path::new(r), home))
        .collect();

    tracked_roots
        .iter()
        .zip(&tracked_root_real_paths)
        .map(|(root, real_root)| {
            let expanded_root = expand_tilde(Path::new(root), home);
            let outside_children = series::series_in_root(&expanded_root, &tracked_root_real_paths)
                .map(|list| {
                    list.into_iter()
                        .map(|child| (child.name, normalize(&child.path, home)))
                        .filter(|(_, real_child)| !real_child.starts_with(real_root))
                        .collect()
                })
                .unwrap_or_default();
            ResolvedRoot {
                root: root.clone(),
                expanded_root,
                real_root: real_root.clone(),
                outside_children,
            }
        })
        .collect()
}

/// The `(root, series name)` a logged path belongs to by where it sits under a
/// tracked root as typed, before symlinks resolve. It catches files reached
/// through symlinks below the series folder, which resolve outside every root.
/// The series name is the depth-1 directory; the match is a claim only, since
/// reconcile still requires the file in that series' own walk. The most
/// specific root wins. Paths with `..` or no directory under the root never
/// match.
pub(crate) fn logged_series(resolved: &[ResolvedRoot], logged: &Path) -> Option<(String, String)> {
    let mut best: Option<(&ResolvedRoot, String)> = None;
    for r in resolved {
        let Ok(rel) = logged.strip_prefix(&r.expanded_root) else {
            continue;
        };
        let parts: Vec<_> = rel.components().collect();
        if parts.len() < 2 || !parts.iter().all(|c| matches!(c, Component::Normal(_))) {
            continue;
        }
        let name = parts[0].as_os_str().to_string_lossy().into_owned();
        let more_specific = best.as_ref().is_none_or(|(b, _)| {
            r.expanded_root.as_os_str().len() > b.expanded_root.as_os_str().len()
        });
        if more_specific {
            best = Some((r, name));
        }
    }
    best.map(|(r, name)| (r.root.clone(), name))
}

/// True if the canonical `real_source` is under any tracked root. Tries a
/// whole-root prefix match first, then falls back to the series folders
/// symlinked outside the root, whose entries the prefix match never sees.
pub(crate) fn is_under_a_tracked_root(real_source: &Path, resolved: &[ResolvedRoot]) -> bool {
    resolved.iter().any(|r| {
        real_source.starts_with(&r.real_root)
            || r.outside_children
                .iter()
                .any(|(_, real_child)| real_source.starts_with(real_child))
    })
}

/// Keeps one entry per canonical source path: the one whose file has the
/// newest mtime.
fn merge_freshest_wins(
    entries: Vec<ScriptLogEntry>,
    home: Option<&Path>,
) -> BTreeMap<PathBuf, ScriptLogEntry> {
    let mut by_path: BTreeMap<PathBuf, ScriptLogEntry> = BTreeMap::new();
    for entry in entries {
        let key = normalize(&entry.source_path, home);
        match by_path.get(&key) {
            Some(existing)
                if (existing.mtime, &existing.source_path) >= (entry.mtime, &entry.source_path) => {
            }
            _ => {
                by_path.insert(key, entry);
            }
        }
    }
    by_path
}

/// Keeps one stream entry per title: the one whose file has the newest mtime.
/// Equal mtimes go to the greater source path, so the choice never depends on
/// the order the log directories were read.
fn merge_streams_freshest_wins(entries: Vec<ScriptLogEntry>) -> Vec<ScriptLogEntry> {
    let mut by_title: BTreeMap<String, ScriptLogEntry> = BTreeMap::new();
    for entry in entries {
        let Some(title) = entry.title.clone() else {
            continue;
        };
        match by_title.get(&title) {
            Some(existing)
                if (existing.mtime, &existing.source_path) >= (entry.mtime, &entry.source_path) => {
            }
            _ => {
                by_title.insert(title, entry);
            }
        }
    }
    by_title.into_values().collect()
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

    /// An exact, second-aligned mtime for whole-value entry comparisons.
    fn fixed_mtime() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
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
    fn a_stream_entry_keeps_its_title_line_verbatim() {
        let dir = tempdir().unwrap();
        let entry_path = dir.path().join("stream");
        write_entry(
            &entry_path,
            "# https://cdn.example/x/index.m3u8?t=abc\ntitle=Blue Lock Episode 9\nduration=1400.5\nposition=312.25\nreason=quit\n",
        );

        let entry = parse_entry_file(&entry_path).unwrap();
        assert_eq!(
            (
                entry.source_path,
                entry.duration,
                entry.position,
                entry.reason,
                entry.title
            ),
            (
                PathBuf::from("https://cdn.example/x/index.m3u8?t=abc"),
                Some(1400.5),
                Some(312.25),
                "quit".to_string(),
                Some("Blue Lock Episode 9".to_string()),
            )
        );
    }

    #[test]
    fn an_entry_without_or_with_an_empty_title_line_has_no_title() {
        let dir = tempdir().unwrap();
        for (name, contents) in [
            (
                "plain",
                "# /home/user/Anime/Show/ep01.mkv\nduration=5.0\nposition=1.0\nreason=quit\n",
            ),
            (
                "empty",
                "# https://cdn.example/a\ntitle=\nduration=5.0\nposition=1.0\nreason=quit\n",
            ),
        ] {
            let entry_path = dir.path().join(name);
            write_entry(&entry_path, contents);
            assert_eq!(parse_entry_file(&entry_path).unwrap().title, None, "{name}");
        }
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
        let mtime = fixed_mtime();
        set_mtime(&log_dir.join("valid"), mtime);

        let tracked_roots = vec![root.to_string_lossy().into_owned()];
        let entries = scan_script_log(std::slice::from_ref(&log_dir), &tracked_roots, None);

        let source = root.join("Show").join("ep01.mkv");
        assert_eq!(
            entries,
            BTreeMap::from([(
                std_fs::canonicalize(&source).unwrap(),
                ScriptLogEntry {
                    source_path: source,
                    duration: Some(1000.0),
                    position: Some(990.0),
                    reason: "eof".to_string(),
                    title: None,
                    mtime,
                }
            )])
        );
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
    fn is_under_a_tracked_root_accepts_a_file_below_the_root_and_rejects_a_sibling_dir() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let tracked = vec![root.to_string_lossy().into_owned()];
        touch(&root.join("Show").join("ep01.mkv"));
        touch(&dir.path().join("Anime2").join("Show").join("ep01.mkv"));

        assert!(is_under_a_tracked_root(
            &normalize(&root.join("Show").join("ep01.mkv"), None),
            &resolve_roots(&tracked, None),
        ));
        assert!(!is_under_a_tracked_root(
            &normalize(
                &dir.path().join("Anime2").join("Show").join("ep01.mkv"),
                None
            ),
            &resolve_roots(&tracked, None),
        ));
    }

    #[test]
    fn is_under_a_tracked_root_with_a_root_and_file_missing_on_disk_matches_by_prefix() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime"); // never created
        let tracked = vec![root.to_string_lossy().into_owned()];

        assert!(is_under_a_tracked_root(
            &normalize(&root.join("Show").join("ep01.mkv"), None),
            &resolve_roots(&tracked, None),
        ));
    }

    #[cfg(unix)]
    #[test]
    fn is_under_a_tracked_root_accepts_a_file_under_a_symlinked_series_folder() {
        // `normalize` resolves the file through the symlink to a path outside
        // the root, so only the per-series fallback can match it.
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();
        let real_storage = dir.path().join("real_storage").join("Dungeon Meshi");
        touch(&real_storage.join("ep01.mkv"));
        symlink(&real_storage, root.join("Dungeon Meshi")).unwrap();
        let tracked = vec![root.to_string_lossy().into_owned()];

        assert!(is_under_a_tracked_root(
            &normalize(&root.join("Dungeon Meshi").join("ep01.mkv"), None),
            &resolve_roots(&tracked, None),
        ));
    }

    #[cfg(unix)]
    #[test]
    fn scan_keeps_entries_logged_through_symlinked_seasons_and_episode_files_inside_a_real_series()
    {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("tv");
        let log_dir = dir.path().join("mpv-log");
        let storage = dir.path().join("storage");
        touch(&storage.join("Lupin S02").join("ep01.mkv"));
        touch(&storage.join("ep02.mkv"));
        touch(&storage.join("stray.mkv"));
        std_fs::create_dir_all(root.join("Lupin")).unwrap();
        symlink(
            storage.join("Lupin S02"),
            root.join("Lupin").join("Lupin S02"),
        )
        .unwrap();
        std_fs::create_dir_all(root.join("Show")).unwrap();
        symlink(storage.join("ep02.mkv"), root.join("Show").join("ep02.mkv")).unwrap();

        let logged = [
            ("a", root.join("Lupin/Lupin S02/ep01.mkv")),
            ("b", root.join("Show/ep02.mkv")),
            ("c", storage.join("stray.mkv")),
        ];
        for (name, path) in &logged {
            write_entry(
                &log_dir.join(name),
                &format!(
                    "# {}\nduration=2400.0\nposition=500.0\nreason=quit\n",
                    path.display()
                ),
            );
        }

        let tracked = vec![root.to_string_lossy().into_owned()];
        let kept: Vec<PathBuf> = scan_script_log(std::slice::from_ref(&log_dir), &tracked, None)
            .into_values()
            .map(|e| e.source_path)
            .collect();
        let mut kept = kept;
        kept.sort();
        let mut expected = vec![logged[0].1.clone(), logged[1].1.clone()];
        expected.sort();
        assert_eq!(kept, expected);
    }

    fn resolved_for(roots: &[&Path]) -> Vec<ResolvedRoot> {
        let tracked: Vec<String> = roots
            .iter()
            .map(|r| r.to_string_lossy().into_owned())
            .collect();
        resolve_roots(&tracked, None)
    }

    #[test]
    fn logged_series_names_the_depth_one_directory_under_the_most_specific_root() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("Anime");
        let rewatches = anime.join("Rewatches");
        let resolved = resolved_for(&[&anime, &rewatches]);

        assert_eq!(
            logged_series(&resolved, &anime.join("Lupin/S02/ep01.mkv")),
            Some((anime.to_string_lossy().into_owned(), "Lupin".to_string()))
        );
        assert_eq!(
            logged_series(&resolved, &rewatches.join("Monster/ep01.mkv")),
            Some((
                rewatches.to_string_lossy().into_owned(),
                "Monster".to_string()
            ))
        );
    }

    #[test]
    fn logged_series_rejects_loose_files_parent_traversal_and_outside_paths() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("Anime");
        let resolved = resolved_for(&[&anime]);

        // A file directly in the root names no series.
        assert_eq!(logged_series(&resolved, &anime.join("loose.mkv")), None);
        assert_eq!(logged_series(&resolved, &anime), None);
        // `..` could name the root's parent as a series.
        assert_eq!(
            logged_series(&resolved, &anime.join("../Other/ep01.mkv")),
            None
        );
        assert_eq!(
            logged_series(&resolved, &anime.join("Show/../ep01.mkv")),
            None
        );
        // Textually outside the root, including a sibling that shares its name prefix.
        assert_eq!(
            logged_series(&resolved, &dir.path().join("Anime2/Show/ep01.mkv")),
            None
        );
        assert_eq!(
            logged_series(&resolved, &dir.path().join("Downloads/clip.mkv")),
            None
        );
    }

    #[test]
    fn logged_series_with_two_spellings_of_one_root_picks_the_first_listed() {
        let home = Path::new("/home/u");
        let resolved = resolve_roots(
            &["~/Anime".to_string(), "/home/u/Anime".to_string()],
            Some(home),
        );

        assert_eq!(
            logged_series(&resolved, Path::new("/home/u/Anime/Show/ep01.mkv")),
            Some(("~/Anime".to_string(), "Show".to_string()))
        );
    }

    #[test]
    fn logged_series_expands_a_tilde_root_and_ignores_a_trailing_slash() {
        let home = Path::new("/home/u");
        let resolved = resolve_roots(&["~/Anime/".to_string()], Some(home));

        assert_eq!(
            logged_series(&resolved, Path::new("/home/u/Anime/Show/ep01.mkv")),
            Some(("~/Anime/".to_string(), "Show".to_string()))
        );
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
        // Regression: Flatpak mpv writes under its own XDG_DATA_HOME, so the
        // Flatpak candidate must be listed whenever a home directory is known,
        // even if the native data dir fails to resolve.
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
    fn mpv_log_dirs_without_home_returns_only_the_native_candidate() {
        // Without a home directory only the native candidate remains.
        let expected: Vec<PathBuf> = dirs::data_dir()
            .map(|d| d.join("tsuzuki").join("mpv-log"))
            .into_iter()
            .collect();
        assert_eq!(mpv_log_dirs(None), expected);
    }

    // --- Cross-directory merge: per-path freshest-file-wins ---

    fn set_mtime(path: &Path, when: SystemTime) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    #[test]
    fn same_source_path_across_two_candidate_directories_the_newer_mtime_wins() {
        // A stale native-path entry and a fresher Flatpak-path entry for the
        // same source: the fresher one wins.
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

        let now = fixed_mtime();
        set_mtime(&native_dir.join("entry"), now - Duration::from_secs(60));
        set_mtime(&flatpak_dir.join("entry"), now);

        let tracked_roots = vec![root.to_string_lossy().into_owned()];
        let entries = scan_script_log(&[native_dir, flatpak_dir], &tracked_roots, None);

        assert_eq!(
            entries,
            BTreeMap::from([(
                std_fs::canonicalize(&source).unwrap(),
                ScriptLogEntry {
                    source_path: source,
                    duration: None,
                    position: Some(200.0),
                    reason: "quit".to_string(),
                    title: None,
                    mtime: now,
                }
            )])
        );
    }

    #[test]
    fn merge_is_per_path_not_per_directory() {
        // A fresher duplicate of X must not drop the undisputed Y entry from the
        // first directory.
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

        let now = fixed_mtime();
        let earlier = now - Duration::from_secs(120);
        set_mtime(&dir_a.join("x-entry"), earlier);
        set_mtime(&dir_a.join("y-entry"), earlier);
        set_mtime(&dir_b.join("x-entry"), now);

        let tracked_roots = vec![root.to_string_lossy().into_owned()];
        let entries = scan_script_log(&[dir_a, dir_b], &tracked_roots, None);

        // dir_b's fresher X wins; dir_a's undisputed Y survives.
        assert_eq!(
            entries,
            BTreeMap::from([
                (
                    std_fs::canonicalize(&x).unwrap(),
                    ScriptLogEntry {
                        source_path: x,
                        duration: None,
                        position: Some(99.0),
                        reason: "quit".to_string(),
                        title: None,
                        mtime: now,
                    }
                ),
                (
                    std_fs::canonicalize(&y).unwrap(),
                    ScriptLogEntry {
                        source_path: y,
                        duration: None,
                        position: Some(20.0),
                        reason: "quit".to_string(),
                        title: None,
                        mtime: earlier,
                    }
                ),
            ])
        );
    }

    #[test]
    fn scan_with_many_untracked_entries_keeps_exactly_the_tracked_one() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let log_dir = dir.path().join("mpv-log");
        for i in 0..5 {
            touch(&root.join(format!("Show {i}")).join("ep01.mkv"));
        }
        for n in 0..200 {
            write_entry(
                &log_dir.join(format!("u{n}")),
                &format!(
                    "# {}\nduration=\nposition=300.0\nreason=quit\n",
                    dir.path()
                        .join("Downloads")
                        .join(format!("clip{n}.mkv"))
                        .display()
                ),
            );
        }
        let tracked = root.join("Show 3").join("ep01.mkv");
        write_entry(
            &log_dir.join("t"),
            &format!(
                "# {}\nduration=1400.0\nposition=300.0\nreason=quit\n",
                tracked.display()
            ),
        );
        let mtime = fixed_mtime();
        set_mtime(&log_dir.join("t"), mtime);

        let tracked_roots = vec![root.to_string_lossy().into_owned()];
        let entries = scan_script_log(std::slice::from_ref(&log_dir), &tracked_roots, None);

        assert_eq!(
            entries,
            BTreeMap::from([(
                std_fs::canonicalize(&tracked).unwrap(),
                ScriptLogEntry {
                    source_path: tracked,
                    duration: Some(1400.0),
                    position: Some(300.0),
                    reason: "quit".to_string(),
                    title: None,
                    mtime,
                }
            )])
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_keys_one_file_spelled_through_a_symlinked_root_and_its_real_path_once() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let real_root = dir.path().join("disk").join("Anime");
        touch(&real_root.join("Show").join("ep01.mkv"));
        let linked_root = dir.path().join("Anime");
        symlink(&real_root, &linked_root).unwrap();

        let native_dir = dir.path().join("native-log");
        let flatpak_dir = dir.path().join("flatpak-log");
        let via_link = linked_root.join("Show").join("ep01.mkv");
        let via_real = real_root.join("Show").join("ep01.mkv");
        write_entry(
            &native_dir.join("entry"),
            &format!(
                "# {}\nduration=\nposition=100.0\nreason=quit\n",
                via_real.display()
            ),
        );
        write_entry(
            &flatpak_dir.join("entry"),
            &format!(
                "# {}\nduration=\nposition=200.0\nreason=quit\n",
                via_link.display()
            ),
        );
        let now = fixed_mtime();
        set_mtime(&native_dir.join("entry"), now - Duration::from_secs(60));
        set_mtime(&flatpak_dir.join("entry"), now);

        let tracked_roots = vec![linked_root.to_string_lossy().into_owned()];
        let entries = scan_script_log(&[native_dir, flatpak_dir], &tracked_roots, None);

        assert_eq!(
            entries,
            BTreeMap::from([(
                std_fs::canonicalize(&via_real).unwrap(),
                ScriptLogEntry {
                    source_path: via_link,
                    duration: None,
                    position: Some(200.0),
                    reason: "quit".to_string(),
                    title: None,
                    mtime: now,
                }
            )])
        );
    }

    #[test]
    fn scan_separates_stream_entries_and_keeps_the_freshest_per_title_across_dirs() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("Show").join("ep01.mkv"));
        let native = dir.path().join("native-log");
        let flatpak = dir.path().join("flatpak-log");
        write_entry(
            &native.join("local"),
            &format!(
                "# {}\nduration=\nposition=\nreason=eof\n",
                root.join("Show").join("ep01.mkv").display()
            ),
        );
        write_entry(
            &native.join("stream"),
            "# https://cdn.example/old\ntitle=Blue Lock Episode 9\nduration=1400.0\nposition=100.0\nreason=quit\n",
        );
        write_entry(
            &flatpak.join("stream"),
            "# https://cdn.example/new\ntitle=Blue Lock Episode 9\nduration=1400.0\nposition=900.0\nreason=quit\n",
        );
        write_entry(
            &flatpak.join("other"),
            "# https://cdn.example/o\ntitle=Blue Lock Episode 10\nduration=\nposition=\nreason=eof\n",
        );
        let older = fixed_mtime();
        let newer = older + Duration::from_secs(60);
        set_mtime(&native.join("stream"), older);
        set_mtime(&flatpak.join("stream"), newer);
        set_mtime(&flatpak.join("other"), older);
        let tracked_roots = vec![root.to_string_lossy().into_owned()];

        // Whichever directory is read first, the newer entry must win.
        for dirs in [[flatpak.clone(), native.clone()], [native, flatpak]] {
            let scan = scan_script_log_full(&dirs, &resolve_roots(&tracked_roots, None), None);
            check_scan(&scan, &root);
        }
    }

    #[test]
    fn scan_drops_a_redirect_stream_entry_and_skips_a_malformed_one_without_aborting() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path().join("mpv-log");
        write_entry(
            &log_dir.join("redirect"),
            "# https://cdn.example/r\ntitle=Blue Lock Episode 1\nduration=1400.0\nposition=900.0\nreason=redirect\n",
        );
        write_entry(
            &log_dir.join("malformed"),
            "# https://cdn.example/m\ntitle=Blue Lock Episode 2\nduration=1400.0\nposition=abc\nreason=quit\n",
        );
        write_entry(
            &log_dir.join("good"),
            "# https://cdn.example/g\ntitle=Blue Lock Episode 3\nduration=1400.0\nposition=60.0\nreason=quit\n",
        );

        let scan = scan_script_log_full(&[log_dir], &[], None);

        assert_eq!(
            scan.streams
                .iter()
                .map(|e| (e.title.clone().unwrap(), e.position))
                .collect::<Vec<_>>(),
            vec![("Blue Lock Episode 3".to_string(), Some(60.0))]
        );
    }

    #[test]
    fn scan_streams_with_equal_mtimes_pick_the_same_entry_whichever_dir_is_read_first() {
        let dir = tempdir().unwrap();
        let native = dir.path().join("native-log");
        let flatpak = dir.path().join("flatpak-log");
        write_entry(
            &native.join("stream"),
            "# https://cdn.example/a\ntitle=Blue Lock Episode 9\nduration=1400.0\nposition=100.0\nreason=quit\n",
        );
        write_entry(
            &flatpak.join("stream"),
            "# https://cdn.example/b\ntitle=Blue Lock Episode 9\nduration=1400.0\nposition=900.0\nreason=quit\n",
        );
        set_mtime(&native.join("stream"), fixed_mtime());
        set_mtime(&flatpak.join("stream"), fixed_mtime());

        let positions: Vec<Vec<Option<f64>>> =
            [[native.clone(), flatpak.clone()], [flatpak, native]]
                .iter()
                .map(|dirs| {
                    scan_script_log_full(dirs, &[], None)
                        .streams
                        .iter()
                        .map(|e| e.position)
                        .collect()
                })
                .collect();

        assert_eq!(positions[0], positions[1]);
    }

    fn check_scan(scan: &ScriptLogScan, root: &Path) {
        assert_eq!(
            scan.files
                .values()
                .map(|e| e.source_path.clone())
                .collect::<Vec<_>>(),
            vec![root.join("Show").join("ep01.mkv")]
        );
        assert_eq!(
            scan.streams
                .iter()
                .map(|e| (e.title.clone().unwrap(), e.position, e.source_path.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    "Blue Lock Episode 10".to_string(),
                    None,
                    PathBuf::from("https://cdn.example/o")
                ),
                (
                    "Blue Lock Episode 9".to_string(),
                    Some(900.0),
                    PathBuf::from("https://cdn.example/new")
                ),
            ]
        );
    }
}
