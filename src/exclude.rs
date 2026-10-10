//! `tsuzuki exclude <path> [--root <root>]`: resolves a relative path against
//! tracked roots and records it. Bare `tsuzuki exclude` lists
//! Anitomy-suggested candidates.
//!
//! Only mutates the in-memory `Config`; the caller writes it to disk.

use std::fmt;
use std::path::{Path, PathBuf};

use tsuzuki_anitomy::ElementKind;

use crate::config::{Config, Exclusion};
use crate::roots::{expand_tilde, normalize};
use crate::series;

/// One tracked root under which the requested relative path was found to
/// exist on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// The root as stored (e.g. `~/Anime`), never a resolved absolute path.
    pub root: String,
    /// The expanded, joined path. For display and validation only.
    pub resolved: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExcludeOutcome {
    /// Appended to `config.exclusions`; the caller must persist `Config`.
    Excluded { root: String, relative_path: String },
    /// Already in `config.exclusions`; not an error.
    AlreadyExcluded { root: String, relative_path: String },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExcludeError {
    /// The path doesn't exist under any tracked root, or under the given
    /// `--root`.
    NotFound(PathBuf),
    /// `--root` doesn't match any tracked root.
    RootNotFound(String),
    /// The path exists under more than one tracked root and no `--root` was
    /// given.
    Ambiguous {
        relative_path: String,
        matches: Vec<Match>,
    },
    /// The path is a depth-1 directory, i.e. a whole series; untrack or
    /// drop it instead.
    WholeSeries { relative_path: String },
    /// The path is absolute. Checked before any `join`, which would discard
    /// the root and resolve against the filesystem root.
    NotRelative(PathBuf),
}

impl fmt::Display for ExcludeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExcludeError::NotFound(p) => {
                write!(
                    f,
                    "path does not exist under any tracked root: {}",
                    p.display()
                )
            }
            ExcludeError::RootNotFound(r) => write!(f, "not a tracked root: {r}"),
            ExcludeError::WholeSeries { relative_path } => write!(
                f,
                "cannot exclude \"{relative_path}\" — it's an entire series (a depth-1 \
                 directory). Don't track it, or use a future `drop`, instead of excluding it."
            ),
            ExcludeError::NotRelative(p) => write!(
                f,
                "path must be relative to a tracked root, not absolute: {}",
                p.display()
            ),
            // The CLI renders this variant via the shared `print_ambiguous`;
            // this text is the fallback for other callers.
            ExcludeError::Ambiguous {
                relative_path,
                matches,
            } => {
                writeln!(
                    f,
                    "Ambiguous: \"{relative_path}\" exists under more than one tracked root:"
                )?;
                writeln!(f)?;
                for (i, m) in matches.iter().enumerate() {
                    writeln!(f, "  {}. {}", i + 1, m.resolved.display())?;
                }
                writeln!(f)?;
                writeln!(f, "Re-run with --root to specify which one:")?;
                for (i, m) in matches.iter().enumerate() {
                    write!(f, "  tsuzuki exclude \"{relative_path}\" --root {}", m.root)?;
                    if i + 1 < matches.len() {
                        writeln!(f)?;
                    }
                }
                Ok(())
            }
        }
    }
}

/// Resolves `relative_path` against tracked roots and records the exclusion
/// in `config`.
pub fn exclude(
    config: &mut Config,
    relative_path: &Path,
    root_arg: Option<&Path>,
) -> Result<ExcludeOutcome, ExcludeError> {
    exclude_with_home(config, relative_path, root_arg, dirs::home_dir().as_deref())
}

fn exclude_with_home(
    config: &mut Config,
    relative_path: &Path,
    root_arg: Option<&Path>,
    home: Option<&Path>,
) -> Result<ExcludeOutcome, ExcludeError> {
    // Must precede every `join` with `relative_path`.
    if relative_path.is_absolute() {
        return Err(ExcludeError::NotRelative(relative_path.to_path_buf()));
    }

    let chosen_root = match root_arg {
        Some(root_arg) => find_tracked_root(config, root_arg, home)
            .ok_or_else(|| ExcludeError::RootNotFound(root_arg.to_string_lossy().into_owned()))?,
        None => {
            let matches = find_matches(config, relative_path, home);
            match matches.len() {
                0 => return Err(ExcludeError::NotFound(relative_path.to_path_buf())),
                1 => matches.into_iter().next().unwrap().root,
                _ => {
                    return Err(ExcludeError::Ambiguous {
                        relative_path: relative_path.to_string_lossy().into_owned(),
                        matches,
                    });
                }
            }
        }
    };

    // Only check on the `--root` path; redundant after `find_matches`.
    let expanded_root = expand_tilde(Path::new(&chosen_root), home);
    let resolved = expanded_root.join(relative_path);
    if !resolved.exists() {
        return Err(ExcludeError::NotFound(relative_path.to_path_buf()));
    }

    let relative_str = relative_path.to_string_lossy().into_owned();

    if is_whole_series_exclusion(relative_path, &resolved) {
        return Err(ExcludeError::WholeSeries {
            relative_path: relative_str,
        });
    }

    // Compare normalized paths, as `add` does, so a trailing slash, `..` or
    // symlink alias counts as the same exclusion.
    let new_normalized = normalize(&resolved, home);
    let already_excluded = config
        .exclusions
        .iter()
        .any(|e| normalize(&Path::new(&e.root).join(&e.path), home) == new_normalized);
    if already_excluded {
        return Ok(ExcludeOutcome::AlreadyExcluded {
            root: chosen_root,
            relative_path: relative_str,
        });
    }

    config.exclusions.push(Exclusion {
        root: chosen_root.clone(),
        path: relative_str.clone(),
    });
    Ok(ExcludeOutcome::Excluded {
        root: chosen_root,
        relative_path: relative_str,
    })
}

/// True for a depth-1 directory, which is a whole series.
fn is_whole_series_exclusion(relative_path: &Path, resolved: &Path) -> bool {
    relative_path.components().count() == 1 && resolved.is_dir()
}

/// Every tracked root under which `relative_path` exists on disk, following
/// symlinks.
fn find_matches(config: &Config, relative_path: &Path, home: Option<&Path>) -> Vec<Match> {
    let mut matches = Vec::new();
    for root in &config.roots.paths {
        let expanded_root = expand_tilde(Path::new(root), home);
        let candidate = expanded_root.join(relative_path);
        if candidate.exists() {
            matches.push(Match {
                root: root.clone(),
                resolved: candidate,
            });
        }
    }
    matches
}

/// Finds the tracked root (as stored) that `root_arg` refers to, comparing
/// normalized paths so `~/Anime` and `/home/user/Anime` match.
fn find_tracked_root(config: &Config, root_arg: &Path, home: Option<&Path>) -> Option<String> {
    let target = normalize(root_arg, home);
    config
        .roots
        .paths
        .iter()
        .find(|r| normalize(Path::new(r), home) == target)
        .cloned()
}

/// Every stored exclusion under `root`, expanded to a filesystem path for
/// `series::ordered_episodes_excluding`.
pub fn excluded_paths_for_root(config: &Config, root: &str, home: Option<&Path>) -> Vec<PathBuf> {
    config
        .exclusions
        .iter()
        .filter(|e| e.root == root)
        .map(|e| expand_tilde(Path::new(root), home).join(&e.path))
        .collect()
}

/// Anitomy `Type` values worth suggesting for exclusion, matched
/// case-insensitively because Anitomy keeps the filename's casing.
///
/// `"TV"` is left out (mainline broadcast) and so is `"SP"`, which collides
/// with ordinary titles like "Yumeiro Patissiere SP Professional".
const SUGGEST_WORTHY_TYPES: &[&str] = &[
    "OVA",
    "Special",
    "Specials",
    "Movie",
    "Gekijouban",
    "ONA",
    "OAD",
    "OAV",
];

/// A not-yet-excluded file whose filename parses as an OVA, special, movie
/// or similar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The root as stored (e.g. `~/Anime`).
    pub root: String,
    /// Relative to `root`, ready to hand straight to `exclude`.
    pub relative_path: String,
}

/// Returns every non-excluded file under the tracked roots whose filename
/// parses a suggest-worthy `Type`. Unreadable roots and series are skipped.
pub fn suggest_candidates(config: &Config, home: Option<&Path>) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let tracked_root_real_paths: Vec<_> = config
        .roots
        .paths
        .iter()
        .map(|r| normalize(Path::new(r), home))
        .collect();

    for root in &config.roots.paths {
        let expanded_root = expand_tilde(Path::new(root), home);
        let Ok(series_list) = series::series_in_root(&expanded_root, &tracked_root_real_paths)
        else {
            continue;
        };
        let excluded = excluded_paths_for_root(config, root, home);

        for s in series_list {
            let Ok(files) = series::ordered_episodes_excluding(
                &s.path,
                &config.roots.video_extensions,
                &excluded,
            ) else {
                continue;
            };

            for file in files {
                if !is_suggest_worthy(&file) {
                    continue;
                }
                let Ok(relative) = file.strip_prefix(&expanded_root) else {
                    continue;
                };
                candidates.push(Candidate {
                    root: root.clone(),
                    relative_path: relative.to_string_lossy().into_owned(),
                });
            }
        }
    }

    candidates
}

/// True if the filename (never the full path) parses a `Type` in
/// `SUGGEST_WORTHY_TYPES`.
fn is_suggest_worthy(path: &Path) -> bool {
    let Some(filename) = path.file_name().and_then(|f| f.to_str()) else {
        return false;
    };
    tsuzuki_anitomy::parse(filename).into_iter().any(|element| {
        element.kind() == ElementKind::Type
            && SUGGEST_WORTHY_TYPES
                .iter()
                .any(|t| t.eq_ignore_ascii_case(element.value()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::File::create(path).unwrap();
    }

    #[test]
    fn excludes_a_subdirectory_under_a_single_tracked_root() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("One Piece").join("Specials").join("sp01.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let outcome =
            exclude_with_home(&mut config, Path::new("One Piece/Specials"), None, None).unwrap();

        assert_eq!(
            outcome,
            ExcludeOutcome::Excluded {
                root: root.to_string_lossy().into_owned(),
                relative_path: "One Piece/Specials".to_string(),
            }
        );
        assert_eq!(config.exclusions.len(), 1);
        assert_eq!(config.exclusions[0].path, "One Piece/Specials");
    }

    #[test]
    fn excludes_a_single_file_under_a_single_tracked_root() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("Monster").join("stray-clip.mkv"));
        touch(&root.join("Monster").join("ep01.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let outcome =
            exclude_with_home(&mut config, Path::new("Monster/stray-clip.mkv"), None, None)
                .unwrap();

        assert!(matches!(outcome, ExcludeOutcome::Excluded { .. }));
        assert_eq!(config.exclusions[0].path, "Monster/stray-clip.mkv");
    }

    #[test]
    fn re_excluding_the_same_path_is_idempotent() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("One Piece").join("Specials").join("sp01.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        exclude_with_home(&mut config, Path::new("One Piece/Specials"), None, None).unwrap();
        let outcome =
            exclude_with_home(&mut config, Path::new("One Piece/Specials"), None, None).unwrap();

        assert_eq!(
            outcome,
            ExcludeOutcome::AlreadyExcluded {
                root: root.to_string_lossy().into_owned(),
                relative_path: "One Piece/Specials".to_string(),
            }
        );
        // No duplicate entry.
        assert_eq!(config.exclusions.len(), 1);
    }

    #[test]
    fn excluding_a_nonexistent_path_errors() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let err =
            exclude_with_home(&mut config, Path::new("Nope/Nothing"), None, None).unwrap_err();
        assert!(matches!(err, ExcludeError::NotFound(p) if p == Path::new("Nope/Nothing")));
        assert!(config.exclusions.is_empty());
    }

    #[test]
    fn excluding_a_whole_series_directory_is_rejected() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("One Piece").join("ep01.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let err = exclude_with_home(&mut config, Path::new("One Piece"), None, None).unwrap_err();
        assert_eq!(
            err,
            ExcludeError::WholeSeries {
                relative_path: "One Piece".to_string(),
            }
        );
        assert!(config.exclusions.is_empty());
    }

    #[test]
    fn excluding_a_lone_file_directly_under_root_is_not_rejected_as_a_whole_series() {
        // A depth-1 file isn't a series; only a directory is.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("loose-clip.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let outcome =
            exclude_with_home(&mut config, Path::new("loose-clip.mkv"), None, None).unwrap();
        assert!(matches!(outcome, ExcludeOutcome::Excluded { .. }));
    }

    #[test]
    fn ambiguous_match_across_two_roots_lists_both_and_errors_without_root() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        touch(&root_a.join("One Piece").join("Specials").join("sp01.mkv"));
        touch(&root_b.join("One Piece").join("Specials").join("sp01.mkv"));

        let mut config = Config::default();
        config
            .roots
            .paths
            .push(root_a.to_string_lossy().into_owned());
        config
            .roots
            .paths
            .push(root_b.to_string_lossy().into_owned());

        let err = exclude_with_home(&mut config, Path::new("One Piece/Specials"), None, None)
            .unwrap_err();

        let ExcludeError::Ambiguous {
            relative_path,
            matches,
        } = err
        else {
            panic!("expected Ambiguous, got a different error variant");
        };
        assert_eq!(relative_path, "One Piece/Specials");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].root, root_a.to_string_lossy());
        assert_eq!(matches[1].root, root_b.to_string_lossy());
        assert!(config.exclusions.is_empty());
    }

    #[test]
    fn ambiguous_display_format_matches_the_design_doc() {
        let matches = vec![
            Match {
                root: "~/Anime".to_string(),
                resolved: PathBuf::from("/home/user/Anime/One Piece/Specials"),
            },
            Match {
                root: "~/AltAnime".to_string(),
                resolved: PathBuf::from("/home/user/AltAnime/One Piece/Specials"),
            },
        ];
        let err = ExcludeError::Ambiguous {
            relative_path: "One Piece/Specials".to_string(),
            matches,
        };

        let expected = "Ambiguous: \"One Piece/Specials\" exists under more than one tracked root:\n\
             \n\
             \x20\x201. /home/user/Anime/One Piece/Specials\n\
             \x20\x202. /home/user/AltAnime/One Piece/Specials\n\
             \n\
             Re-run with --root to specify which one:\n\
             \x20\x20tsuzuki exclude \"One Piece/Specials\" --root ~/Anime\n\
             \x20\x20tsuzuki exclude \"One Piece/Specials\" --root ~/AltAnime";

        assert_eq!(err.to_string(), expected);
    }

    #[test]
    fn ambiguous_match_resolved_with_root_flag_picks_the_named_root() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        touch(&root_a.join("One Piece").join("Specials").join("sp01.mkv"));
        touch(&root_b.join("One Piece").join("Specials").join("sp01.mkv"));

        let mut config = Config::default();
        config
            .roots
            .paths
            .push(root_a.to_string_lossy().into_owned());
        config
            .roots
            .paths
            .push(root_b.to_string_lossy().into_owned());

        let outcome = exclude_with_home(
            &mut config,
            Path::new("One Piece/Specials"),
            Some(&root_b),
            None,
        )
        .unwrap();

        assert_eq!(
            outcome,
            ExcludeOutcome::Excluded {
                root: root_b.to_string_lossy().into_owned(),
                relative_path: "One Piece/Specials".to_string(),
            }
        );
        assert_eq!(config.exclusions.len(), 1);
        assert_eq!(config.exclusions[0].root, root_b.to_string_lossy());
    }

    #[test]
    fn root_flag_not_matching_any_tracked_root_errors() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let unrelated = dir.path().join("NotTracked");
        touch(&root.join("One Piece").join("ep01.mkv"));
        fs::create_dir_all(&unrelated).unwrap();

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let err = exclude_with_home(&mut config, Path::new("One Piece"), Some(&unrelated), None)
            .unwrap_err();
        assert_eq!(
            err,
            ExcludeError::RootNotFound(unrelated.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn exclusion_persists_the_roots_literal_tilde_form_not_an_expanded_path() {
        let home = tempdir().unwrap();
        touch(
            &home
                .path()
                .join("Anime")
                .join("One Piece")
                .join("Specials")
                .join("sp01.mkv"),
        );

        let mut config = Config::default();
        config.roots.paths.push("~/Anime".to_string());

        let outcome = exclude_with_home(
            &mut config,
            Path::new("One Piece/Specials"),
            None,
            Some(home.path()),
        )
        .unwrap();

        assert_eq!(
            outcome,
            ExcludeOutcome::Excluded {
                root: "~/Anime".to_string(),
                relative_path: "One Piece/Specials".to_string(),
            }
        );
        assert_eq!(config.exclusions[0].root, "~/Anime");
    }

    #[test]
    fn excluded_paths_for_root_expands_only_entries_matching_that_root() {
        let home = tempdir().unwrap();

        let mut config = Config::default();
        config.exclusions.push(Exclusion {
            root: "~/Anime".to_string(),
            path: "One Piece/Specials".to_string(),
        });
        config.exclusions.push(Exclusion {
            root: "~/AltAnime".to_string(),
            path: "Monster/stray.mkv".to_string(),
        });

        let paths = excluded_paths_for_root(&config, "~/Anime", Some(home.path()));
        assert_eq!(
            paths,
            vec![home.path().join("Anime").join("One Piece").join("Specials")]
        );
    }

    #[test]
    fn excluded_paths_for_root_is_empty_when_no_exclusions_match_that_root() {
        // A root without exclusions of its own must not pick up another root's.
        let mut config = Config::default();
        config.exclusions.push(Exclusion {
            root: "~/AltAnime".to_string(),
            path: "Monster/stray.mkv".to_string(),
        });

        let paths = excluded_paths_for_root(&config, "~/Anime", None);
        assert!(paths.is_empty());
    }

    // --- overlapping exclusions are never merged ---

    #[test]
    fn excluding_a_file_already_covered_by_a_prior_directory_exclusion_is_accepted_as_a_separate_entry()
     {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("One Piece").join("Specials").join("sp01.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        exclude_with_home(&mut config, Path::new("One Piece/Specials"), None, None).unwrap();
        let outcome = exclude_with_home(
            &mut config,
            Path::new("One Piece/Specials/sp01.mkv"),
            None,
            None,
        )
        .unwrap();

        // A narrower path is a distinct exclusion, not idempotent with the broader one.
        assert!(matches!(outcome, ExcludeOutcome::Excluded { .. }));
        assert_eq!(config.exclusions.len(), 2);
    }

    #[test]
    fn excluding_a_directory_after_a_file_within_it_was_already_excluded_keeps_both_entries() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("One Piece").join("Specials").join("sp01.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        exclude_with_home(
            &mut config,
            Path::new("One Piece/Specials/sp01.mkv"),
            None,
            None,
        )
        .unwrap();
        let outcome =
            exclude_with_home(&mut config, Path::new("One Piece/Specials"), None, None).unwrap();

        assert!(matches!(outcome, ExcludeOutcome::Excluded { .. }));
        assert_eq!(config.exclusions.len(), 2);
        // The redundant file-level entry is left in place.
        assert!(
            config
                .exclusions
                .iter()
                .any(|e| e.path == "One Piece/Specials/sp01.mkv")
        );
        assert!(
            config
                .exclusions
                .iter()
                .any(|e| e.path == "One Piece/Specials")
        );
    }

    // --- `--root` ---

    #[test]
    fn root_flag_works_when_only_one_tracked_root_exists_no_ambiguity_to_resolve() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("One Piece").join("Specials").join("sp01.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let outcome = exclude_with_home(
            &mut config,
            Path::new("One Piece/Specials"),
            Some(&root),
            None,
        )
        .unwrap();

        assert_eq!(
            outcome,
            ExcludeOutcome::Excluded {
                root: root.to_string_lossy().into_owned(),
                relative_path: "One Piece/Specials".to_string(),
            }
        );
    }

    #[test]
    fn root_flag_naming_a_tracked_root_that_does_not_contain_the_path_returns_not_found() {
        // The root is tracked but doesn't contain the path, unlike `RootNotFound`.
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        touch(&root_a.join("One Piece").join("ep01.mkv"));
        touch(&root_b.join("Monster").join("ep01.mkv"));

        let mut config = Config::default();
        config
            .roots
            .paths
            .push(root_a.to_string_lossy().into_owned());
        config
            .roots
            .paths
            .push(root_b.to_string_lossy().into_owned());

        let err =
            exclude_with_home(&mut config, Path::new("Monster"), Some(&root_a), None).unwrap_err();

        assert!(matches!(err, ExcludeError::NotFound(p) if p == Path::new("Monster")));
        assert!(config.exclusions.is_empty());
    }

    // --- symlinks ---

    #[cfg(unix)]
    #[test]
    fn excluding_a_symlinked_directory_follows_it_with_stat_semantics() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let real_specials = dir.path().join("real-storage").join("Specials");
        touch(&real_specials.join("sp01.mkv"));
        touch(&root.join("One Piece").join("ep01.mkv"));
        symlink(&real_specials, root.join("One Piece").join("Specials")).unwrap();

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        // A symlink to a directory is excluded like a real directory.
        let outcome =
            exclude_with_home(&mut config, Path::new("One Piece/Specials"), None, None).unwrap();
        assert!(matches!(outcome, ExcludeOutcome::Excluded { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn excluding_a_symlinked_top_level_entry_is_still_rejected_as_a_whole_series() {
        use std::os::unix::fs::symlink;

        // A depth-1 symlink to a directory is still a whole series.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let real_series = dir.path().join("real-storage").join("Monster");
        touch(&real_series.join("ep01.mkv"));
        fs::create_dir_all(&root).unwrap();
        symlink(&real_series, root.join("Monster")).unwrap();

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let err = exclude_with_home(&mut config, Path::new("Monster"), None, None).unwrap_err();
        assert_eq!(
            err,
            ExcludeError::WholeSeries {
                relative_path: "Monster".to_string(),
            }
        );
    }

    // --- idempotency and absolute paths ---

    #[test]
    fn a_trailing_slash_variant_of_an_already_excluded_path_is_recognized_as_idempotent() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("One Piece").join("Specials").join("sp01.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        exclude_with_home(&mut config, Path::new("One Piece/Specials"), None, None).unwrap();
        let outcome =
            exclude_with_home(&mut config, Path::new("One Piece/Specials/"), None, None).unwrap();

        assert_eq!(
            outcome,
            ExcludeOutcome::AlreadyExcluded {
                root: root.to_string_lossy().into_owned(),
                relative_path: "One Piece/Specials/".to_string(),
            }
        );
        // No redundant second entry.
        assert_eq!(config.exclusions.len(), 1);
    }

    #[test]
    fn an_absolute_path_argument_is_rejected_before_it_ever_reaches_join() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();

        let mut config = Config::default();
        config
            .roots
            .paths
            .push(root_a.to_string_lossy().into_owned());
        config
            .roots
            .paths
            .push(root_b.to_string_lossy().into_owned());

        // `/etc` exists on any Linux box; the absolute path must be rejected, not resolved.
        let err = exclude_with_home(&mut config, Path::new("/etc"), None, None).unwrap_err();

        assert_eq!(err, ExcludeError::NotRelative(PathBuf::from("/etc")));
        assert!(config.exclusions.is_empty());
    }

    #[test]
    fn an_absolute_path_argument_is_rejected_even_with_root_given() {
        // Rejected the same way with or without `--root`.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let err = exclude_with_home(&mut config, Path::new("/etc"), Some(&root), None).unwrap_err();

        assert_eq!(err, ExcludeError::NotRelative(PathBuf::from("/etc")));
    }

    // --- suggested candidates ---

    fn default_extensions() -> Vec<String> {
        vec!["mkv".to_string()]
    }

    #[test]
    fn suggests_only_ova_special_movie_style_files_not_regular_episodes() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(
            &root
                .join("One Piece")
                .join("[Group] One Piece - 05 [1080p].mkv"),
        );
        touch(
            &root
                .join("One Piece")
                .join("Specials")
                .join("[Group] One Piece - OVA 1 [1080p].mkv"),
        );
        touch(&root.join("One Piece").join("One Piece Movie 01.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.roots.video_extensions = default_extensions();

        let candidates = suggest_candidates(&config, None);
        let paths: Vec<&str> = candidates
            .iter()
            .map(|c| c.relative_path.as_str())
            .collect();

        assert!(
            paths.iter().any(|p| p.contains("OVA 1")),
            "expected the OVA file to be suggested, got: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.contains("Movie")),
            "expected the Movie file to be suggested, got: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.contains("- 05")),
            "regular episode must not be suggested, got: {paths:?}"
        );
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn an_already_excluded_candidate_is_never_suggested_again() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let ova_path = root
            .join("One Piece")
            .join("Specials")
            .join("[Group] One Piece - OVA 1 [1080p].mkv");
        touch(&ova_path);
        touch(
            &root
                .join("One Piece")
                .join("[Group] One Piece - 05 [1080p].mkv"),
        );

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.roots.video_extensions = default_extensions();

        // Exclude the OVA's directory, as the suggested command would.
        exclude_with_home(&mut config, Path::new("One Piece/Specials"), None, None).unwrap();

        let candidates = suggest_candidates(&config, None);
        assert!(
            candidates.is_empty(),
            "already-excluded candidate must not be suggested again, got: {candidates:?}"
        );
    }

    #[test]
    fn zero_candidates_when_nothing_matches_a_suggest_worthy_type() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(
            &root
                .join("One Piece")
                .join("[Group] One Piece - 01 [1080p].mkv"),
        );
        touch(
            &root
                .join("One Piece")
                .join("[Group] One Piece - 02 [1080p].mkv"),
        );

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.roots.video_extensions = default_extensions();

        let candidates = suggest_candidates(&config, None);
        assert!(candidates.is_empty());
    }

    #[test]
    fn suggest_candidates_reports_the_owning_root_for_each_candidate() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("One Piece").join("One Piece Movie 01.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.roots.video_extensions = default_extensions();

        let candidates = suggest_candidates(&config, None);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].root, root.to_string_lossy());
        assert_eq!(
            candidates[0].relative_path,
            "One Piece/One Piece Movie 01.mkv"
        );
    }

    // --- suggestion completeness ---

    #[test]
    fn a_suggest_worthy_file_several_directories_deep_is_still_found() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(
            &root
                .join("One Piece")
                .join("Extras")
                .join("Bonus")
                .join("Deep")
                .join("One Piece Movie 01.mkv"),
        );

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.roots.video_extensions = default_extensions();

        let candidates = suggest_candidates(&config, None);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].relative_path,
            "One Piece/Extras/Bonus/Deep/One Piece Movie 01.mkv"
        );
    }

    #[test]
    fn a_suggest_worthy_type_with_no_parseable_episode_number_is_still_suggested() {
        // A pure OVA filename with no `Episode` element is still suggested.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("One Piece").join("One Piece - OVA [1080p].mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.roots.video_extensions = default_extensions();

        let candidates = suggest_candidates(&config, None);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].relative_path,
            "One Piece/One Piece - OVA [1080p].mkv"
        );
    }

    #[test]
    fn suggestion_matching_is_case_insensitive_on_the_type_value_itself() {
        // Lowercase and mixed-case `Type` values both match.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("Show").join("[Group] Show - ova 1 [1080p].mkv"));
        touch(&root.join("Show").join("[Group] Show - Ova 2 [1080p].mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.roots.video_extensions = default_extensions();

        let candidates = suggest_candidates(&config, None);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn a_video_file_whose_own_filename_has_no_type_keyword_is_not_suggested_even_inside_a_specials_folder()
     {
        // A file in a `Specials/` folder is not suggested on folder name alone.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(
            &root
                .join("One Piece")
                .join("Specials")
                .join("[Group] One Piece - 01 [1080p].mkv"),
        );

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.roots.video_extensions = default_extensions();

        let candidates = suggest_candidates(&config, None);
        assert!(
            candidates.is_empty(),
            "a filename with no Type keyword must not be suggested just because of its \
             containing folder's name, got: {candidates:?}"
        );
    }

    #[test]
    fn tv_and_sp_type_values_are_never_suggested() {
        // `TV` and `SP` parse as `Type` but are not suggested.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("Show").join("[Group] Show TV - 05 [1080p].mkv"));
        touch(&root.join("Show").join("[Group] Show - SP01.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.roots.video_extensions = default_extensions();

        let candidates = suggest_candidates(&config, None);
        assert!(
            candidates.is_empty(),
            "TV/SP-typed files must never be suggested, got: {candidates:?}"
        );
    }

    // --- symlinks in suggestions ---

    #[cfg(unix)]
    #[test]
    fn a_suggest_worthy_file_reached_through_a_symlinked_directory_is_still_suggested() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let real_specials = dir.path().join("real-storage").join("Specials");
        touch(&real_specials.join("One Piece Movie 01.mkv"));
        fs::create_dir_all(root.join("One Piece")).unwrap();
        symlink(&real_specials, root.join("One Piece").join("Specials")).unwrap();

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.roots.video_extensions = default_extensions();

        let candidates = suggest_candidates(&config, None);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].relative_path,
            "One Piece/Specials/One Piece Movie 01.mkv"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_inside_an_excluded_symlinked_directory_is_never_suggested() {
        // Exclusion applies by resolved path, so symlinked files aren't suggested.
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        let real_specials = dir.path().join("real-storage").join("Specials");
        touch(&real_specials.join("One Piece Movie 01.mkv"));
        fs::create_dir_all(root.join("One Piece")).unwrap();
        symlink(&real_specials, root.join("One Piece").join("Specials")).unwrap();

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        config.roots.video_extensions = default_extensions();

        exclude_with_home(&mut config, Path::new("One Piece/Specials"), None, None).unwrap();

        let candidates = suggest_candidates(&config, None);
        assert!(
            candidates.is_empty(),
            "a file inside an excluded symlinked directory must not be suggested, got: {candidates:?}"
        );
    }

    // --- vendor dependency ---

    #[test]
    fn the_tsuzuki_binary_crate_can_call_into_the_vendored_workspace_dependency() {
        // Smoke test: `tsuzuki_anitomy::parse` is callable from this crate.
        let elements = tsuzuki_anitomy::parse("[Group] Show - 05 [1080p].mkv");
        assert!(
            elements
                .iter()
                .any(|e| e.kind() == ElementKind::Episode && e.value() == "05")
        );
    }
}
