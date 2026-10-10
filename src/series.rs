//! Series identity and episode ordering. Pure filesystem logic: no mpv, no network.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A series: the immediate child of a tracked root, named verbatim after its directory.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Series {
    pub name: String,
    pub path: PathBuf,
}

/// Lists the depth-1 directories of `root` as series, naturally sorted by name.
/// Symlinked directories count; loose files are ignored.
///
/// A child whose real path is in `tracked_root_real_paths` is skipped: it is its own
/// root and contributes its series independently.
#[allow(dead_code)]
pub fn series_in_root(root: &Path, tracked_root_real_paths: &[PathBuf]) -> io::Result<Vec<Series>> {
    let mut series = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if is_dir_following_symlinks(&path) && !is_a_tracked_root(&path, tracked_root_real_paths) {
            let name = entry.file_name().to_string_lossy().into_owned();
            series.push(Series { path, name });
        }
    }
    series.sort_by(|a, b| natord::compare(&a.name, &b.name));
    Ok(series)
}

fn is_a_tracked_root(path: &Path, tracked_root_real_paths: &[PathBuf]) -> bool {
    let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    tracked_root_real_paths.contains(&real)
}

/// Episodes of a series in natural order: at each level, subdirectories (depth-first)
/// come before that level's own video files.
/// Only files matching `video_extensions` (case-insensitive) are listed. Symlinks are
/// followed; a directory whose real path was already visited is not entered again.
#[allow(dead_code)]
pub fn ordered_episodes(
    series_path: &Path,
    video_extensions: &[String],
) -> io::Result<Vec<PathBuf>> {
    ordered_episodes_excluding(series_path, video_extensions, &[])
}

/// Like [`ordered_episodes`], but paths in `excluded` (files or directories) are
/// dropped before sorting. Excluding a directory drops everything under it.
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
    // Seed with the root so a symlink back to it is not walked as a second copy.
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
        // Follows symlinks; broken links and unreadable entries are skipped.
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };

        // Checked on the resolved path so a symlink into excluded content is caught too.
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
        // Cycle protection; also dedupes aliases of the same real directory.
        let real_path = fs::canonicalize(subdir).unwrap_or_else(|_| subdir.clone());
        if visited.insert(real_path) {
            walk(subdir, video_extensions, excluded, out, visited)?;
        }
    }
    out.extend(files);

    Ok(())
}

/// True if `real_path` is an excluded path or lies under one.
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
    use std::time::Duration;
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
        // A loose file is not a series.
        touch(&root.join("readme.txt"));

        let series = series_in_root(root, &[]).unwrap();
        let names: Vec<_> = series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Monster", "One Piece"]);
        assert_eq!(series[0].path, root.join("Monster"));
    }

    #[test]
    fn a_child_not_separately_tracked_as_its_own_root_is_still_listed_as_a_series() {
        let dir = tempdir().unwrap();
        let anime_root = dir.path().join("Anime");
        let rewatches_root = anime_root.join("Rewatches");
        std_fs::create_dir_all(anime_root.join("Monster")).unwrap();
        std_fs::create_dir_all(rewatches_root.join("Monster")).unwrap();

        // Not a tracked root here, so it is a plain series.
        let anime_series = series_in_root(&anime_root, &[]).unwrap();
        let anime_names: Vec<_> = anime_series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(anime_names, vec!["Monster", "Rewatches"]);
    }

    #[test]
    fn a_child_that_is_itself_a_tracked_root_is_excluded_from_its_parents_series_list() {
        // `Rewatches` is its own root, so it must not also appear as a series of `Anime`.
        let dir = tempdir().unwrap();
        let anime_root = dir.path().join("Anime");
        let rewatches_root = anime_root.join("Rewatches");
        std_fs::create_dir_all(anime_root.join("Monster")).unwrap();
        std_fs::create_dir_all(rewatches_root.join("Monster")).unwrap();

        let tracked_root_real_paths = vec![
            std_fs::canonicalize(&anime_root).unwrap(),
            std_fs::canonicalize(&rewatches_root).unwrap(),
        ];

        let anime_series = series_in_root(&anime_root, &tracked_root_real_paths).unwrap();
        let anime_names: Vec<_> = anime_series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(anime_names, vec!["Monster"]);

        let rewatches_series = series_in_root(&rewatches_root, &tracked_root_real_paths).unwrap();
        let rewatches_names: Vec<_> = rewatches_series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(rewatches_names, vec!["Monster"]);

        // The two `Monster` series are distinct paths.
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
        // Loose files only, no directories.
        touch(&root.join("readme.txt"));
        touch(&root.join("ep01.mkv"));

        let series = series_in_root(&root, &[]).unwrap();
        assert!(series.is_empty());
    }

    #[test]
    fn ordering_recurses_genuinely_arbitrarily_deep_not_just_two_levels() {
        // Three directories deep, branching at the third level.
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
        // An empty allowlist is explicit: no fallback to defaults.
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("No Extensions Configured");
        touch(&series_path.join("ep01.mkv"));
        touch(&series_path.join("ep02.mkv"));

        let episodes = ordered_episodes(&series_path, &[]).unwrap();
        assert!(episodes.is_empty());
    }

    #[test]
    fn leading_dot_in_configured_extension_does_not_match_bare_file_extension() {
        // Normalizing a leading dot is config.rs's job; this layer compares literally.
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Dotted Config");
        touch(&series_path.join("ep01.mkv"));

        let dotted_extensions = vec![".mkv".to_string()];
        let episodes = ordered_episodes(&series_path, &dotted_extensions).unwrap();
        assert!(episodes.is_empty());
    }

    #[test]
    fn directories_are_never_filtered_by_extension() {
        // A directory named like a video file is still recursed into.
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
        // Excluding one series' `Specials` must not affect another series' `Specials`.
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

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_an_excluded_directory_pointing_out_is_never_reached() {
        // The excluded directory is never read, so its outbound symlink is never found.
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

        // The alias resolves to the excluded file, so it is dropped too.
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

        let series = series_in_root(&root, &[]).unwrap();
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
    fn ordered_episodes_symlink_back_to_root_terminates_and_lists_each_episode_once() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Cycle Series");
        let nested = series_path.join("nested");
        std_fs::create_dir_all(&nested).unwrap();
        touch(&series_path.join("ep01.mkv"));

        // `nested/back` points back at the series root.
        symlink(&series_path, nested.join("back")).unwrap();

        let episodes = walk_with_timeout(&series_path);
        assert_eq!(relative_order(&series_path, &episodes), vec!["ep01.mkv"]);
    }

    fn walk_with_timeout(series_path: &Path) -> Vec<PathBuf> {
        let (tx, rx) = std::sync::mpsc::channel();
        let path = series_path.to_path_buf();
        let extensions = default_extensions();
        std::thread::spawn(move || {
            let _ = tx.send(ordered_episodes(&path, &extensions));
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect(
                "ordered_episodes did not return — likely infinite recursion on a symlink cycle",
            )
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn ordered_episodes_sibling_symlink_cycle_lists_each_episode_once() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Cycle Series");
        touch(&series_path.join("a").join("ep01.mkv"));
        touch(&series_path.join("b").join("ep02.mkv"));
        symlink(series_path.join("b"), series_path.join("a").join("to_b")).unwrap();
        symlink(series_path.join("a"), series_path.join("b").join("to_a")).unwrap();

        let episodes = walk_with_timeout(&series_path);

        let mut file_names: Vec<String> = episodes.iter().map(|p| entry_name(p)).collect();
        file_names.sort();
        assert_eq!(file_names, vec!["ep01.mkv", "ep02.mkv"]);
    }

    #[cfg(unix)]
    #[test]
    fn ordered_episodes_two_aliases_of_one_directory_list_its_episodes_once() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Alias Series");
        touch(&series_path.join("real").join("ep01.mkv"));
        symlink(series_path.join("real"), series_path.join("alias")).unwrap();

        let episodes = walk_with_timeout(&series_path);

        assert_eq!(episodes.len(), 1);
    }

    #[test]
    fn ordered_episodes_lists_season_dirs_before_loose_files_at_the_same_level() {
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Show");
        // "Aaa" sorts before "Season", so a files-first walk would list it first.
        touch(&series_path.join("Aaa OVA.mkv"));
        touch(&series_path.join("Season 1").join("ep01.mkv"));
        touch(&series_path.join("Season 1").join("ep02.mkv"));
        touch(&series_path.join("Season 2").join("ep01.mkv"));
        touch(&series_path.join("Zzz OVA.mkv"));

        let episodes = ordered_episodes(&series_path, &default_extensions()).unwrap();

        assert_eq!(
            relative_order(&series_path, &episodes),
            vec![
                "Season 1/ep01.mkv",
                "Season 1/ep02.mkv",
                "Season 2/ep01.mkv",
                "Aaa OVA.mkv",
                "Zzz OVA.mkv",
            ]
        );
    }

    #[test]
    fn ordered_episodes_sorts_unpadded_numbers_naturally_files_and_dirs() {
        let dir = tempdir().unwrap();
        let series_path = dir.path().join("Show");
        for name in ["ep10.mkv", "ep2.mkv", "ep1.mkv"] {
            touch(&series_path.join(name));
        }
        for season in ["Season 10", "Season 2"] {
            touch(&series_path.join(season).join("ep1.mkv"));
        }

        let episodes = ordered_episodes(&series_path, &default_extensions()).unwrap();

        assert_eq!(
            relative_order(&series_path, &episodes),
            vec![
                "Season 2/ep1.mkv",
                "Season 10/ep1.mkv",
                "ep1.mkv",
                "ep2.mkv",
                "ep10.mkv",
            ]
        );
    }

    #[test]
    fn series_in_root_sorts_unpadded_numbers_naturally() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        for name in ["Show 10", "Show 2", "Show 1"] {
            std_fs::create_dir_all(root.join(name)).unwrap();
        }

        let names: Vec<String> = series_in_root(&root, &[])
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();

        assert_eq!(names, vec!["Show 1", "Show 2", "Show 10"]);
    }
}
