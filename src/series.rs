//! Series identity and episode ordering (CLAUDE.md §3, design.md §3/§5).
//!
//! Pure filesystem logic, zero external I/O beyond reading directory
//! entries — no mpv, no network, so this is fully testable against
//! `tempfile`-fabricated trees. Not wired into any user-facing command yet
//! (that's Phase 5); this module only needs to be callable and correct.

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
/// is always a directory.
///
/// Results are naturally sorted by name for deterministic output, but this
/// ordering isn't meaningful yet — Phase 5's `list`/`start` decide how
/// series are actually presented.
#[allow(dead_code)]
pub fn series_in_root(root: &Path) -> io::Result<Vec<Series>> {
    let mut series = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            series.push(Series {
                path: entry.path(),
                name,
            });
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
#[allow(dead_code)]
pub fn ordered_episodes(
    series_path: &Path,
    video_extensions: &[String],
) -> io::Result<Vec<PathBuf>> {
    let mut episodes = Vec::new();
    walk(series_path, video_extensions, &mut episodes)?;
    Ok(episodes)
}

fn walk(dir: &Path, video_extensions: &[String], out: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut subdirs = Vec::new();
    let mut files = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            subdirs.push(path);
        } else if file_type.is_file() && is_video_file(&path, video_extensions) {
            files.push(path);
        }
    }

    subdirs.sort_by(|a, b| natord::compare(&entry_name(a), &entry_name(b)));
    files.sort_by(|a, b| natord::compare(&entry_name(a), &entry_name(b)));

    for subdir in &subdirs {
        walk(subdir, video_extensions, out)?;
    }
    out.extend(files);

    Ok(())
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
