//! Series identity and episode ordering (CLAUDE.md §3, design.md §3/§5).
//!
//! Pure filesystem logic, zero external I/O beyond reading directory
//! entries — no mpv, no network, so this is fully testable against
//! `tempfile`-fabricated trees. Not wired into any user-facing command yet
//! (that's Phase 5); this module only needs to be callable and correct.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A series: the immediate child of a tracked root, named verbatim after
/// its directory (design.md §3 — no parsing, no bracket-stripping).
///
/// Unused outside tests until Phase 5 wires ordering into a user-facing
/// command (`list`, `status`, `start`, ...) — this phase only needs the
/// module to be callable and independently testable, per CLAUDE.md.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Series {
    pub name: String,
    pub path: PathBuf,
}

/// Enumerates the immediate child directories of `root` as series — depth
/// 1 only, no recursion into what counts as a series, verbatim directory
/// name. Non-directory entries directly under `root` are ignored; a series
/// is always a directory. Follows symlinks (stat semantics, not lstat) —
/// a symlinked series folder behaves exactly like a real one (CLAUDE.md's
/// "Symlinks are followed everywhere, with cycle protection"). No cycle
/// protection is needed here: this function never recurses, so a symlink
/// cycle among `root`'s immediate children can't cause unbounded work.
///
/// Results are naturally sorted by name for deterministic output, but this
/// ordering isn't meaningful yet — Phase 5's `list`/`start` decide how
/// series are actually presented.
#[allow(dead_code)]
pub fn series_in_root(root: &Path) -> io::Result<Vec<Series>> {
    let mut series = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if is_dir_following_symlinks(&path) {
            let name = entry.file_name().to_string_lossy().into_owned();
            series.push(Series { path, name });
        }
    }
    series.sort_by(|a, b| natord::compare(&a.name, &b.name));
    Ok(series)
}

/// Hierarchical natural sort over a series' full path tree (design.md §5):
/// at each directory level, subdirectories are compared naturally against
/// each other and recursed into depth-first, then this directory's own
/// video files are compared naturally against each other and appended.
/// Only files whose extension matches `video_extensions` (case-insensitive)
/// are included as leaves — directories are never filtered by extension.
///
/// Follows symlinks (stat semantics) for both directories and files, per
/// CLAUDE.md — a symlinked subdirectory is recursed into and a symlinked
/// episode file is included exactly like a real one. Because following
/// symlinks makes cycles possible, the walk tracks canonicalized real
/// paths already visited and stops descending into a repeat rather than
/// recursing forever.
#[allow(dead_code)]
pub fn ordered_episodes(
    series_path: &Path,
    video_extensions: &[String],
) -> io::Result<Vec<PathBuf>> {
    ordered_episodes_excluding(series_path, video_extensions, &[])
}

/// Same as [`ordered_episodes`], but any path in `excluded` (files or
/// directories, given as real filesystem paths — not necessarily
/// canonicalized by the caller, this function does that) is removed from
/// the tree entirely before the natural sort runs, never as a post-filter
/// on the sorted result (CLAUDE.md's "Exclusions run before ordering,
/// always"). Excluding a directory excludes everything under it
/// recursively, simply by never descending into it. Kept as a distinct
/// function — rather than folding exclusion-filtering and sorting into one
/// pass — so exclusion behavior stays independently testable from the sort
/// itself.
#[allow(dead_code)]
pub fn ordered_episodes_excluding(
    series_path: &Path,
    video_extensions: &[String],
    excluded: &[PathBuf],
) -> io::Result<Vec<PathBuf>> {
    let excluded_real: HashSet<PathBuf> = excluded
        .iter()
        .map(|p| fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();

    let mut episodes = Vec::new();
    let mut visited = HashSet::new();
    // Seed with the series root's own real path so a symlink anywhere in
    // the tree that points back to the root is caught immediately, rather
    // than causing one full duplicate pass over the root's own files.
    if let Ok(real_root) = fs::canonicalize(series_path) {
        visited.insert(real_root);
    }
    walk(
        series_path,
        video_extensions,
        &excluded_real,
        &mut episodes,
        &mut visited,
    )?;
    Ok(episodes)
}

fn walk(
    dir: &Path,
    video_extensions: &[String],
    excluded: &HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    let mut subdirs = Vec::new();
    let mut files = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // `fs::metadata` follows symlinks (unlike `DirEntry::file_type`,
        // which reports the symlink itself). A broken symlink or a
        // read/permission race makes this fail — such an entry is
        // silently skipped rather than aborting the whole walk, the same
        // way a non-matching extension is silently excluded.
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };

        // Exclusion filtering happens here, at collection time — before
        // this level's entries are even handed to `sort_by` below, not as
        // a post-filter on the sorted result. Skipping an excluded
        // directory here means `walk` never recurses into it, which is
        // what makes a directory exclusion recursive for free.
        //
        // The check is against this entry's *resolved* (symlink-followed)
        // real path, and against exclusion *ancestry* — not just exact
        // membership — so a symlink located outside an excluded directory
        // but pointing into excluded content still resolves onto (i.e. is
        // a descendant of) an excluded real path and is correctly caught,
        // rather than silently bypassing the exclusion (CLAUDE.md's
        // "Exclusion enforcement checks the resolved path").
        let real_path = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if is_excluded(&real_path, excluded) {
            continue;
        }

        if metadata.is_dir() {
            subdirs.push(path);
        } else if metadata.is_file() && is_video_file(&path, video_extensions) {
            files.push(path);
        }
    }

    subdirs.sort_by(|a, b| natord::compare(&entry_name(a), &entry_name(b)));
    files.sort_by(|a, b| natord::compare(&entry_name(a), &entry_name(b)));

    for subdir in &subdirs {
        // Cycle protection: only descend if this subdirectory's real
        // (symlink-resolved) path hasn't been visited yet in this walk.
        // Also naturally deduplicates two different aliases (e.g. two
        // symlinks) that resolve to the same real directory, which would
        // otherwise list the same episodes twice.
        let real_path = fs::canonicalize(subdir).unwrap_or_else(|_| subdir.clone());
        if visited.insert(real_path) {
            walk(subdir, video_extensions, excluded, out, visited)?;
        }
    }
    out.extend(files);

    Ok(())
}

/// True if `real_path` is itself an excluded real path, or a descendant of
/// one — an ancestry check, not exact-set membership, since an excluded
/// directory's own real path is the only entry recorded for it (nothing
/// pre-expands it into every descendant path underneath).
fn is_excluded(real_path: &Path, excluded: &HashSet<PathBuf>) -> bool {
    excluded
        .iter()
        .any(|e| real_path == e || real_path.starts_with(e))
}

fn is_dir_following_symlinks(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

fn entry_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn is_video_file(path: &Path, video_extensions: &[String]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| video_extensions.iter().any(|v| v.eq_ignore_ascii_case(ext)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self as std_fs, File};
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std_fs::create_dir_all(parent).unwrap();
        }
        File::create(path).unwrap();
    }

    fn default_extensions() -> Vec<String> {
        ["mkv", "mp4", "avi", "webm", "mov"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn relative_order(root: &Path, episodes: &[PathBuf]) -> Vec<String> {
        episodes
            .iter()
            .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn series_in_root_enumerates_depth_1_directories_verbatim() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std_fs::create_dir_all(root.join("Monster")).unwrap();
        std_fs::create_dir_all(root.join("One Piece")).unwrap();
        // A loose file directly under root is not a series.
        touch(&root.join("readme.txt"));

        let series = series_in_root(root).unwrap();
        let names: Vec<_> = series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Monster", "One Piece"]);
        assert_eq!(series[0].path, root.join("Monster"));
    }

    #[test]
    fn composable_roots_produce_independent_series_lists() {
        let dir = tempdir().unwrap();
        let anime_root = dir.path().join("Anime");
        let rewatches_root = anime_root.join("Rewatches");
        std_fs::create_dir_all(anime_root.join("Monster")).unwrap();
        std_fs::create_dir_all(rewatches_root.join("Monster")).unwrap();

        let anime_series = series_in_root(&anime_root).unwrap();
        let rewatches_series = series_in_root(&rewatches_root).unwrap();

        // "Rewatches" itself shows up as a series under the Anime root
        // (it's just another depth-1 directory there) — that's expected;
        // registering it separately is what makes it its own root too.
        let anime_names: Vec<_> = anime_series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(anime_names, vec!["Monster", "Rewatches"]);

        let rewatches_names: Vec<_> = rewatches_series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(rewatches_names, vec!["Monster"]);

        // Distinct paths -> independent ordering trees, per design.md §3.
        assert_ne!(
            anime_series
                .iter()
                .find(|s| s.name == "Monster")
                .unwrap()
                .path,
            rewatches_series
                .iter()
                .find(|s| s.name == "Monster")
                .unwrap()
                .path
        );
    }

    #[test]
    fn season_folders_sort_hierarchically_per_design_doc_example() {
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("One Piece");
        touch(&series_path.join("Season 1").join("ep01.mkv"));
        touch(&series_path.join("Season 1").join("ep02.mkv"));
        touch(&series_path.join("Season 2").join("ep01.mkv"));

        let episodes = ordered_episodes(&series_path, &default_extensions()).unwrap();
        assert_eq!(
            relative_order(&series_path, &episodes),
            vec![
                "Season 1/ep01.mkv",
                "Season 1/ep02.mkv",
                "Season 2/ep01.mkv",
            ]
        );
    }

    #[test]
    fn numbering_gap_preserves_order_without_inferring_the_gap() {
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Gapped Show");
        for n in [1, 2, 3, 5, 6] {
            touch(&series_path.join(format!("ep{n:02}.mkv")));
        }

        let episodes = ordered_episodes(&series_path, &default_extensions()).unwrap();
        assert_eq!(
            relative_order(&series_path, &episodes),
            vec!["ep01.mkv", "ep02.mkv", "ep03.mkv", "ep05.mkv", "ep06.mkv",]
        );
    }

    #[test]
    fn non_numeric_names_fall_back_to_alphabetical() {
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("No Numbers");
        touch(&series_path.join("beta.mkv"));
        touch(&series_path.join("alpha.mkv"));
        touch(&series_path.join("gamma.mkv"));

        let episodes = ordered_episodes(&series_path, &default_extensions()).unwrap();
        assert_eq!(
            relative_order(&series_path, &episodes),
            vec!["alpha.mkv", "beta.mkv", "gamma.mkv"]
        );
    }

    #[test]
    fn sidecar_files_are_excluded_from_the_ordering_tree() {
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Series With Sidecars");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep01.srt"));
        touch(&series_path.join("ep01.ass"));
        touch(&series_path.join("ep02.mkv"));
        touch(&series_path.join("series.nfo"));
        touch(&series_path.join("cover.jpg"));

        let episodes = ordered_episodes(&series_path, &default_extensions()).unwrap();
        assert_eq!(
            relative_order(&series_path, &episodes),
            vec!["ep01.mkv", "ep02.mkv"]
        );
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Mixed Case");
        touch(&series_path.join("ep01.MKV"));
        touch(&series_path.join("ep02.mkv"));

        let episodes = ordered_episodes(&series_path, &default_extensions()).unwrap();
        assert_eq!(
            relative_order(&series_path, &episodes),
            vec!["ep01.MKV", "ep02.mkv"]
        );
    }

    #[test]
    fn ordered_episodes_on_an_empty_directory_returns_an_empty_vec() {
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Empty Series");
        std_fs::create_dir_all(&series_path).unwrap();

        let episodes = ordered_episodes(&series_path, &default_extensions()).unwrap();
        assert!(episodes.is_empty());
    }

    #[test]
    fn ordered_episodes_with_only_non_video_files_returns_an_empty_vec() {
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Only Sidecars");
        touch(&series_path.join("series.nfo"));
        touch(&series_path.join("cover.jpg"));
        touch(&series_path.join("episode.srt"));

        let episodes = ordered_episodes(&series_path, &default_extensions()).unwrap();
        assert!(episodes.is_empty());
    }

    #[test]
    fn series_in_root_with_no_subdirectories_returns_an_empty_vec() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Root");
        // Only loose files directly under root, no series (directories) at
        // all.
        touch(&root.join("readme.txt"));
        touch(&root.join("ep01.mkv"));

        let series = series_in_root(&root).unwrap();
        assert!(series.is_empty());
    }

    #[test]
    fn ordering_recurses_genuinely_arbitrarily_deep_not_just_two_levels() {
        // Every other ordering test nests exactly 2 levels deep (series
        // root -> one subdirectory -> file). This fixture nests 3
        // directories deep, with branching at the third level, to prove
        // `walk` actually recurses rather than happening to work only for
        // the shallow fixtures used elsewhere.
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Long Runner");
        touch(
            &series_path
                .join("Arc 1")
                .join("Part 1")
                .join("Sub A")
                .join("ep01.mkv"),
        );
        touch(
            &series_path
                .join("Arc 1")
                .join("Part 1")
                .join("Sub B")
                .join("ep02.mkv"),
        );
        touch(&series_path.join("Arc 1").join("Part 2").join("ep03.mkv"));

        let episodes = ordered_episodes(&series_path, &default_extensions()).unwrap();
        assert_eq!(
            relative_order(&series_path, &episodes),
            vec![
                "Arc 1/Part 1/Sub A/ep01.mkv",
                "Arc 1/Part 1/Sub B/ep02.mkv",
                "Arc 1/Part 2/ep03.mkv",
            ]
        );
    }

    #[test]
    fn empty_video_extensions_list_excludes_every_file() {
        // An empty allowlist is an explicit user choice (however
        // self-defeating) — per CLAUDE.md's "never infer" principle there's
        // no fallback to the default list; every file is correctly excluded,
        // not an error.
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("No Extensions Configured");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let episodes = ordered_episodes(&series_path, &[]).unwrap();
        assert!(episodes.is_empty());
    }

    #[test]
    fn leading_dot_in_configured_extension_does_not_match_bare_file_extension() {
        // `Path::extension()` returns the extension without its leading
        // dot, and `is_video_file` compares against whatever list it's
        // given as-is, with no normalization of its own — normalizing a
        // leading dot is config.rs's job at load time (see
        // config.rs::video_extensions_with_leading_dots_are_normalized_on_read),
        // deliberately not repeated per-file-comparison here (CLAUDE.md).
        // This test documents that `ordered_episodes` itself stays
        // literal: a caller that bypasses `config::read` and hands it a
        // raw dotted extension still gets this behavior, not a silent
        // second normalization pass.
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Dotted Config");
        touch(&series_path.join("ep01.mkv"));

        let dotted_extensions = vec![".mkv".to_string()];
        let episodes = ordered_episodes(&series_path, &dotted_extensions).unwrap();
        assert!(episodes.is_empty());
    }

    #[test]
    fn directories_are_never_filtered_by_extension() {
        // A subdirectory literally named like a video file must still be
        // recursed into as a directory, not treated as (or excluded like) a
        // file — the allowlist only ever applies to files.
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Odd Names");
        touch(&series_path.join("ep01.mkv").join("nested.mkv"));

        let episodes = ordered_episodes(&series_path, &default_extensions()).unwrap();
        assert_eq!(
            relative_order(&series_path, &episodes),
            vec!["ep01.mkv/nested.mkv"]
        );
    }

    #[test]
    fn excluding_a_directory_removes_everything_under_it_recursively() {
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("One Piece");
        touch(&series_path.join("Season 1").join("ep01.mkv"));
        touch(&series_path.join("Season 1").join("ep02.mkv"));
        touch(&series_path.join("Specials").join("sp01.mkv"));
        touch(&series_path.join("Specials").join("sp02.mkv"));

        let excluded = vec![series_path.join("Specials")];
        let episodes =
            ordered_episodes_excluding(&series_path, &default_extensions(), &excluded).unwrap();

        assert_eq!(
            relative_order(&series_path, &episodes),
            vec!["Season 1/ep01.mkv", "Season 1/ep02.mkv"]
        );
    }

    #[test]
    fn excluding_a_single_file_removes_only_that_file() {
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Monster");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));
        touch(&series_path.join("stray-clip.mkv"));

        let excluded = vec![series_path.join("stray-clip.mkv")];
        let episodes =
            ordered_episodes_excluding(&series_path, &default_extensions(), &excluded).unwrap();

        assert_eq!(
            relative_order(&series_path, &episodes),
            vec!["ep01.mkv", "ep02.mkv"]
        );
    }

    #[test]
    fn an_unexcluded_series_with_the_same_directory_structure_is_unaffected() {
        // Proves exclusions don't leak across series (or roots): a second
        // series with an identically-named "Specials" subdirectory, given
        // an exclusion list that only names the *other* series' Specials
        // folder, must keep its own Specials content intact.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let excluded_series = root.join("One Piece");
        let unaffected_series = root.join("Monster");
        touch(&excluded_series.join("Specials").join("sp01.mkv"));
        touch(&excluded_series.join("Season 1").join("ep01.mkv"));
        touch(&unaffected_series.join("Specials").join("sp01.mkv"));
        touch(&unaffected_series.join("Season 1").join("ep01.mkv"));

        let excluded = vec![excluded_series.join("Specials")];

        let one_piece_episodes =
            ordered_episodes_excluding(&excluded_series, &default_extensions(), &excluded).unwrap();
        assert_eq!(
            relative_order(&excluded_series, &one_piece_episodes),
            vec!["Season 1/ep01.mkv"]
        );

        let monster_episodes =
            ordered_episodes_excluding(&unaffected_series, &default_extensions(), &excluded)
                .unwrap();
        assert_eq!(
            relative_order(&unaffected_series, &monster_episodes),
            vec!["Season 1/ep01.mkv", "Specials/sp01.mkv"]
        );
    }

    #[test]
    fn ordered_episodes_with_no_exclusions_matches_ordered_episodes_excluding_with_an_empty_list() {
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("One Piece");
        touch(&series_path.join("Season 1").join("ep01.mkv"));
        touch(&series_path.join("Specials").join("sp01.mkv"));

        let plain = ordered_episodes(&series_path, &default_extensions()).unwrap();
        let via_excluding =
            ordered_episodes_excluding(&series_path, &default_extensions(), &[]).unwrap();

        assert_eq!(plain, via_excluding);
    }

    // --- Gap-audit: symlink interaction with exclusion filtering ---

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_an_excluded_directory_pointing_out_is_never_reached() {
        // The excluded directory is skipped at collection time — `walk`
        // never even calls `read_dir` on it — so an internal symlink
        // that points back out to non-excluded content elsewhere can't
        // leak that content in via this direction: the symlink itself is
        // never discovered, let alone followed.
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let series_path = dir.path().join("One Piece");
        touch(&series_path.join("Season 1").join("ep01.mkv"));
        touch(&series_path.join("Real Content").join("ep02.mkv"));
        std_fs::create_dir_all(series_path.join("Specials")).unwrap();
        symlink(
            series_path.join("Real Content"),
            series_path.join("Specials").join("link-out"),
        )
        .unwrap();

        let excluded = vec![series_path.join("Specials")];
        let episodes =
            ordered_episodes_excluding(&series_path, &default_extensions(), &excluded).unwrap();

        assert_eq!(
            relative_order(&series_path, &episodes),
            vec!["Real Content/ep02.mkv", "Season 1/ep01.mkv"]
        );
    }

    // --- Correction pass: this used to be an `#[ignore]`d bug
    // reproduction (gap-audit). Now fixed — un-ignored and asserting the
    // correct behavior is the proof. See CLAUDE.md's "Exclusion
    // enforcement checks the resolved ... path" for the fix reasoning. ---

    #[cfg(unix)]
    #[test]
    fn a_symlink_alias_pointing_into_excluded_content_is_also_excluded() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let series_path = dir.path().join("One Piece");
        touch(&series_path.join("Specials").join("sp01.mkv"));
        touch(&series_path.join("Season 1").join("ep01.mkv"));
        symlink(
            series_path.join("Specials").join("sp01.mkv"),
            series_path.join("Season 1").join("sp01-alias.mkv"),
        )
        .unwrap();

        let excluded = vec![series_path.join("Specials")];
        let episodes =
            ordered_episodes_excluding(&series_path, &default_extensions(), &excluded).unwrap();

        // The alias resolves (via canonicalize) to the exact same real
        // file the exclusion targets, so it's excluded too, even though
        // its own path in the tree is outside "Specials".
        assert_eq!(
            relative_order(&series_path, &episodes),
            vec!["Season 1/ep01.mkv"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_subdirectory_under_a_tracked_root_is_enumerated_as_a_series() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();
        let real_series = dir.path().join("real-storage").join("Monster");
        std_fs::create_dir_all(&real_series).unwrap();

        symlink(&real_series, root.join("Monster")).unwrap();

        let series = series_in_root(&root).unwrap();
        let names: Vec<_> = series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Monster"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_episode_file_is_included_in_ordered_episodes() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Series");
        std_fs::create_dir_all(&series_path).unwrap();
        touch(&series_path.join("ep01.mkv"));

        let real_ep02 = dir.path().join("real-storage").join("ep02.mkv");
        touch(&real_ep02);
        symlink(&real_ep02, series_path.join("ep02.mkv")).unwrap();

        let episodes = ordered_episodes(&series_path, &default_extensions()).unwrap();
        assert_eq!(
            relative_order(&series_path, &episodes),
            vec!["ep01.mkv", "ep02.mkv"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cycle_does_not_hang_or_panic() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Cycle Series");
        let nested = series_path.join("nested");
        std_fs::create_dir_all(&nested).unwrap();
        touch(&series_path.join("ep01.mkv"));

        // `nested/back` symlinks back to the series root itself — a cycle
        // a naive recursive walk would follow forever once symlinks are
        // followed at all.
        symlink(&series_path, nested.join("back")).unwrap();

        let series_path_clone = series_path.clone();
        let extensions = default_extensions();
        let handle = std::thread::spawn(move || ordered_episodes(&series_path_clone, &extensions));

        let result = wait_with_timeout(handle, Duration::from_secs(5)).expect(
            "ordered_episodes did not return within the timeout — \
             likely an infinite loop from the symlink cycle",
        );

        let episodes = result.unwrap();
        // The cycle must not cause ep01.mkv to be listed more than once.
        assert_eq!(relative_order(&series_path, &episodes), vec!["ep01.mkv"]);
    }

    /// Polls a `JoinHandle` instead of blocking on `.join()`, so a
    /// regression that reintroduces infinite recursion fails this test
    /// loudly (assertion/panic) instead of hanging the whole test binary
    /// (and therefore CI) indefinitely. The spawned thread itself can't be
    /// killed if it never returns — it's leaked for the rest of the
    /// process — but the test polling it always returns within `timeout`.
    fn wait_with_timeout<T: Send + 'static>(
        handle: std::thread::JoinHandle<T>,
        timeout: Duration,
    ) -> Option<T> {
        let start = Instant::now();
        loop {
            if handle.is_finished() {
                return handle.join().ok();
            }
            if start.elapsed() > timeout {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Not a correctness assertion — ordering isn't wired to any command
    /// yet (Phase 5's job), so this exercises `series_in_root` +
    /// `ordered_episodes` end-to-end against a fabricated tree and prints
    /// the result for visual sanity-checking against design.md §5's
    /// example. Run with `cargo test ordering_demo -- --nocapture`.
    #[test]
    fn ordering_demo_visual_sanity_check() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");

        touch(
            &root
                .join("Monster")
                .join("[1080p HEVC][Crunchyroll]")
                .join("ep01.mkv"),
        );
        touch(&root.join("One Piece").join("Season 1").join("ep01.mkv"));
        touch(&root.join("One Piece").join("Season 1").join("ep02.mkv"));
        touch(&root.join("One Piece").join("Season 2").join("ep01.mkv"));
        touch(&root.join("One Piece").join("Season 1").join("ep01.srt"));

        println!(
            "\n=== ordering demo: series_in_root({}) ===",
            root.display()
        );
        let series = series_in_root(&root).unwrap();
        for s in &series {
            println!("series: {}", s.name);
            let episodes = ordered_episodes(&s.path, &default_extensions()).unwrap();
            for (i, ep) in episodes.iter().enumerate() {
                println!(
                    "  {:>2}. {}",
                    i + 1,
                    ep.strip_prefix(&s.path).unwrap().display()
                );
            }
        }
        println!("=== end ordering demo ===\n");
    }
}
