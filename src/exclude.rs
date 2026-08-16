//! `tsuzuki exclude <path> [--root <root>]` — resolving a relative path
//! against tracked roots and persisting the result (CLAUDE.md's
//! "Exclusion persistence and disambiguation", design.md §3) — plus bare
//! `tsuzuki exclude`'s Anitomy-suggested candidate listing (design.md §5's
//! Phase 3 paragraph, CLAUDE.md's "Anitomy-candidate confirmation stays
//! list-only, not interactive").
//!
//! This module only mutates the in-memory `Config`; the caller (main.rs)
//! is responsible for writing it to disk, same pattern as `roots.rs`.

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
    /// The root's own persisted literal form (e.g. `~/Anime`) — what an
    /// exclusion referencing this root would store, never a resolved
    /// absolute path.
    pub root: String,
    /// The expanded, joined path — for display and on-disk validation
    /// only, never persisted.
    pub resolved: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExcludeOutcome {
    /// Newly appended to `config.exclusions`. Caller must persist `Config`.
    Excluded { root: String, relative_path: String },
    /// Already present in `config.exclusions` — idempotent, not an error.
    AlreadyExcluded { root: String, relative_path: String },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExcludeError {
    /// `relative_path` doesn't exist under any tracked root (or, when
    /// `--root` was given, doesn't exist under that specific root).
    NotFound(PathBuf),
    /// `--root` was given but doesn't match any currently tracked root.
    RootNotFound(String),
    /// The relative path exists under more than one tracked root and no
    /// `--root` was given to disambiguate.
    Ambiguous {
        relative_path: String,
        matches: Vec<Match>,
    },
    /// The relative path resolves to a depth-1 directory — a whole series
    /// folder — which `exclude` refuses to touch (CLAUDE.md: that's what
    /// not tracking it, or a future `drop`, is for).
    WholeSeries { relative_path: String },
    /// `relative_path` was given as an absolute path. Rejected before it
    /// ever reaches a `.join()` call — `PathBuf::join` silently discards
    /// its base entirely when given an absolute argument, which would
    /// otherwise resolve against the filesystem root instead of the
    /// tracked root (CLAUDE.md's "must be validated as relative before
    /// any join").
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

/// Resolves `relative_path` against tracked roots and, on success, records
/// the exclusion in `config`. See module docs and CLAUDE.md's "Exclusion
/// persistence and disambiguation" for the full decision table.
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
    // Validated before any `.join()` involving `relative_path` — see
    // `ExcludeError::NotRelative`'s doc comment for why this has to come
    // first, not just be "one more check somewhere in here".
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

    // Redundant with the lookup above in the unambiguous case, but kept as
    // its own explicit, independently-testable check per CLAUDE.md's
    // validate-now rule (matching `add`'s behavior) — and it's the only
    // check exercised at all in the `--root`-given path.
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

    // Idempotency is checked against the normalized (tilde-expanded,
    // canonicalized) path, not a raw string comparison on `path` — the
    // same reused `roots::normalize` helper `add`'s own idempotency check
    // uses (CLAUDE.md: "if these two checks ever diverge again, that's a
    // bug, not an acceptable inconsistency"). This is what makes a
    // trailing slash, a `..` that resolves to the same location, or a
    // symlink alias all correctly collapse to the same comparison target.
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

/// A depth-1 directory directly under its root is a whole series folder
/// (CLAUDE.md §3: "Series identity = depth-1 directory under a tracked
/// root") — `exclude` rejects removing one of those entirely, distinct
/// from excluding a file or a deeper subdirectory within one.
fn is_whole_series_exclusion(relative_path: &Path, resolved: &Path) -> bool {
    relative_path.components().count() == 1 && resolved.is_dir()
}

/// Every tracked root under which `relative_path` currently exists on
/// disk. Follows symlinks (`Path::exists` uses stat semantics), consistent
/// with CLAUDE.md's "Symlinks are followed everywhere".
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

/// Finds the tracked root (in its stored literal form) that `root_arg`
/// refers to, using the same expand-then-canonicalize comparison `add`
/// uses for its own idempotency check — so `--root ~/Anime` and
/// `--root /home/user/Anime` are recognized as the same tracked root
/// regardless of which form was originally passed to `add`.
fn find_tracked_root(config: &Config, root_arg: &Path, home: Option<&Path>) -> Option<String> {
    let target = normalize(root_arg, home);
    config
        .roots
        .paths
        .iter()
        .find(|r| normalize(Path::new(r), home) == target)
        .cloned()
}

/// Every currently-stored exclusion under `root` (matched by its literal
/// stored form), expanded to an absolute-ish filesystem path — ready to
/// hand to `series::ordered_episodes_excluding`. A path outside a given
/// series' own subtree simply never matches anything during that series'
/// walk, so this doesn't need to be pre-filtered per series.
pub fn excluded_paths_for_root(config: &Config, root: &str, home: Option<&Path>) -> Vec<PathBuf> {
    config
        .exclusions
        .iter()
        .filter(|e| e.root == root)
        .map(|e| expand_tilde(Path::new(root), home).join(&e.path))
        .collect()
}

/// Anitomy `Type` values that count as suggest-worthy for bare `tsuzuki
/// exclude` — matched case-insensitively, since Anitomy preserves the
/// filename's original casing in the parsed value (confirmed by direct
/// testing: `"ova"` in a filename parses to `Type = "ova"`, not a
/// normalized `"OVA"`).
///
/// Every `Type` keyword in Anitomy's own table is marked ambiguous
/// (requires bracket-enclosure, or specific surrounding context, to be
/// recognized at all — see `vendor/anitomy/src/keyword.rs`), so "ambiguous
/// per the keyword table" doesn't by itself distinguish any of these from
/// each other. `"TV"` and `"SP"` are excluded here on different, more
/// concrete grounds: `"TV"` denotes the ordinary/mainline broadcast
/// category — the opposite of what an exclude-candidate should flag — and
/// Anitomy's own keyword table carries an inline comment documenting a
/// real `"SP"`-in-an-ordinary-title collision ("Yumeiro Patissiere SP
/// Professional"). `"Gekijouban"` is added on top of the literal list
/// given in this phase's task brief: it's a confirmed real `Type` value
/// (verified by direct testing) carrying the exact same ambiguity
/// classification as `"Movie"`, and is semantically its direct synonym
/// (theatrical release) — there's no principled reason to suggest one and
/// not the other.
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

/// One Anitomy-suggested exclude candidate: a file under a tracked root
/// whose parsed `Type` element looks like an OVA/Special/Movie/etc, not
/// already excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The root's own persisted literal form (e.g. `~/Anime`) — what the
    /// suggested `exclude` command's `--root`-free form resolves against.
    pub root: String,
    /// Relative to `root`, ready to hand straight to `exclude`.
    pub relative_path: String,
}

/// Scans every tracked root's series trees — respecting existing
/// exclusions, so an already-excluded file is never suggested again — and
/// returns every file whose filename Anitomy parses a suggest-worthy
/// `Type` element from. Unreadable roots/series (e.g. a root that's
/// currently unmounted) are silently skipped, consistent with `series.rs`'s
/// own failure-tolerant walk — a bare `exclude` scan isn't the place to
/// surface an I/O error for one bad root out of many.
pub fn suggest_candidates(config: &Config, home: Option<&Path>) -> Vec<Candidate> {
    let mut candidates = Vec::new();

    for root in &config.roots.paths {
        let expanded_root = expand_tilde(Path::new(root), home);
        let Ok(series_list) = series::series_in_root(&expanded_root) else {
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

/// True if `path`'s filename (never the full path — see CLAUDE.md's
/// bare-filename calling rule) parses at least one `Type` element whose
/// value case-insensitively matches `SUGGEST_WORTHY_TYPES`.
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
        // A depth-1 *file* isn't a series (a series is always a
        // directory) — only a depth-1 directory triggers the
        // whole-series rejection.
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
        // A root with zero exclusions of its own (while a *different*
        // root does have some) must not panic and must not pick up
        // entries that belong to that other root.
        let mut config = Config::default();
        config.exclusions.push(Exclusion {
            root: "~/AltAnime".to_string(),
            path: "Monster/stray.mkv".to_string(),
        });

        let paths = excluded_paths_for_root(&config, "~/Anime", None);
        assert!(paths.is_empty());
    }

    // --- Gap-audit: overlapping/redundant exclusions (never deduped or
    // merged across granularities — consistent with "never infer",
    // CLAUDE.md §2: Tsuzuki doesn't decide two exclusions overlap, it only
    // recognizes the *exact same* path being re-excluded as idempotent) ---

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

        // Not recognized as already-covered — a distinct, narrower path
        // string is a distinct exclusion entry, not an error and not
        // idempotent-with the broader one.
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
        // The now-redundant file-level entry is left in place, not
        // cleaned up — nothing in `exclude` reasons about one exclusion
        // shadowing another.
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

    // --- Gap-audit: `--root` in the unambiguous case ---

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
        // Distinct from `RootNotFound`: the root itself is valid and
        // tracked, it just doesn't contain this particular relative path
        // (it exists only under a different tracked root).
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

    // --- Gap-audit: symlink interaction ---

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

        // The symlink resolves to a directory (stat semantics), so this
        // must succeed exactly like excluding a real directory would —
        // not be treated as a file, not silently rejected.
        let outcome =
            exclude_with_home(&mut config, Path::new("One Piece/Specials"), None, None).unwrap();
        assert!(matches!(outcome, ExcludeOutcome::Excluded { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn excluding_a_symlinked_top_level_entry_is_still_rejected_as_a_whole_series() {
        use std::os::unix::fs::symlink;

        // A depth-1 entry that's a symlink to a real directory elsewhere
        // must still be recognized as a directory (and thus a whole
        // series) by `is_whole_series_exclusion`'s `resolved.is_dir()`
        // check, which follows symlinks.
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

    // --- Correction pass: these two used to be `#[ignore]`d bug
    // reproductions (gap-audit). Now fixed — un-ignored and asserting the
    // correct behavior is the proof. See CLAUDE.md's "Exclusion
    // persistence and disambiguation" for the fix reasoning. ---

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

        // "/etc" is used only because it reliably exists on any Linux box
        // this test runs on — the point is that an absolute path must be
        // rejected outright, never silently resolved against it.
        let err = exclude_with_home(&mut config, Path::new("/etc"), None, None).unwrap_err();

        assert_eq!(err, ExcludeError::NotRelative(PathBuf::from("/etc")));
        assert!(config.exclusions.is_empty());
    }

    #[test]
    fn an_absolute_path_argument_is_rejected_even_with_root_given() {
        // The guard runs before the `root_arg`/`find_matches` branch
        // entirely, so this must be rejected the same way regardless of
        // whether `--root` was also passed.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let err = exclude_with_home(&mut config, Path::new("/etc"), Some(&root), None).unwrap_err();

        assert_eq!(err, ExcludeError::NotRelative(PathBuf::from("/etc")));
    }

    // --- Phase 3: Anitomy-suggested candidates for bare `exclude` ---

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

        // Exclude the OVA's containing directory first, exactly like a
        // user running the suggested `exclude` command would.
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

    // --- Gap audit: candidate-suggestion completeness ---

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
        // Candidate suggestion only cares about the `Type` element — it
        // must not implicitly depend on an `Episode` element also being
        // present. A pure OVA release with no number in its filename at
        // all (confirmed by direct testing: this parses `Type = "OVA"`
        // and no `Episode` element whatsoever) is exactly the case where
        // episode-identity parsing (a separate, independent feature) would
        // fall back to ordinal — that must have zero effect on whether
        // this file gets suggested.
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
        // The case-insensitivity claim documented on `SUGGEST_WORTHY_TYPES`
        // was previously only exercised indirectly (via `is_suggest_worthy`
        // unit-level reasoning) — this proves it end-to-end through
        // `suggest_candidates`, using both a fully-lowercase and a
        // mixed-case `Type` value, both confirmed by direct testing to be
        // exactly what Anitomy hands back for these filenames (it
        // preserves the filename's original casing verbatim).
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
        // Locks in the "no directory-name inference" design decision as
        // an explicit regression test, not just an implicit consequence
        // of "only ever pass the bare filename to Anitomy" (CLAUDE.md's
        // bare-filename calling rule). A file sitting inside a
        // `Specials/` folder whose own filename carries no OVA/Special/
        // Movie keyword must NOT be suggested just because of where it
        // lives — that would be exactly the folder-based content
        // classification CLAUDE.md §2/§3 rules out everywhere else.
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
        // Direct negative-case coverage for CLAUDE.md's documented
        // exclusion of "TV" and "SP" from the suggest-worthy list — up to
        // now only the *positive* suggest-worthy values had any test
        // coverage at all. Both filenames are confirmed by direct testing
        // to parse `Type = "TV"` / `Type = "SP"` respectively (proving
        // this isn't just "these never get recognized as Type in the
        // first place" — they do, and are still correctly filtered out).
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

    // --- Gap audit: symlink interaction with candidate suggestion ---

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
        // Same ancestry-based, resolved-real-path exclusion check Phase 2
        // fixed for `series.rs`'s ordering walk (CLAUDE.md's "Exclusion
        // enforcement checks the resolved path") — `suggest_candidates`
        // reuses `ordered_episodes_excluding` directly, so this proves
        // that reuse actually carries the fix through, rather than
        // assuming it does because the two features share code.
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

    // --- Gap audit: vendor workspace-dependency soundness (item 5) ---

    #[test]
    fn the_tsuzuki_binary_crate_can_call_into_the_vendored_workspace_dependency() {
        // Not testing Anitomy's parsing correctness (that's
        // `tsuzuki-anitomy`'s own regression suite, `cargo test -p
        // tsuzuki-anitomy`) — this is a deliberately named, minimal smoke
        // test proving the *dependency edge itself* is sound: that code
        // living in the `tsuzuki` crate can call `tsuzuki_anitomy::parse`
        // and get real element data back, through the workspace path
        // dependency the vendor/anitomy/Cargo.toml dev-dependency rename
        // fix was needed to unblock. Every other test in this file and in
        // `anitomy.rs` already exercises this same edge incidentally;
        // this one exists so the guarantee is explicit and self-
        // documenting rather than a side effect nobody named.
        let elements = tsuzuki_anitomy::parse("[Group] Show - 05 [1080p].mkv");
        assert!(
            elements
                .iter()
                .any(|e| e.kind() == ElementKind::Episode && e.value() == "05")
        );
    }
}
