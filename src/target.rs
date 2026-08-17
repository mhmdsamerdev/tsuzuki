//! Resolving an explicit series name (optionally with `--root`) to the
//! specific tracked root it lives under — the same disambiguation shape
//! `exclude.rs` already established for `--root` (CLAUDE.md §3), shared by
//! `resume`, `start`, `pause`, and `drop`. `exclude.rs`'s own equivalent
//! helper (`find_tracked_root`) is private and off-limits to modify, so
//! this is an independent, small reimplementation over the same public
//! `roots::expand_tilde`/`state::resolve_root_arg` both already share —
//! not a duplicated large surface.

use std::path::Path;

use crate::config::Config;
use crate::roots::expand_tilde;
use crate::state::resolve_root_arg;

#[derive(Debug, PartialEq, Eq)]
pub enum TargetError {
    /// `series` doesn't exist as a depth-1 directory under any tracked root
    /// (or, when `--root` was given, doesn't exist under that specific
    /// root).
    NotFound(String),
    /// `--root` was given but doesn't match any currently tracked root.
    RootNotFound(String),
    /// `series` exists as a depth-1 directory under more than one tracked
    /// root and no `--root` was given to disambiguate.
    Ambiguous { series: String, roots: Vec<String> },
}

/// Resolves `series` to the tracked root (its own stored literal form) it
/// lives under. With `root_arg` given, only that root is considered — a
/// non-matching `--root` is `RootNotFound`, and a `--root` that doesn't
/// contain `series` is `NotFound`. Without it, every tracked root
/// containing a depth-1 directory named `series` is a candidate: zero is
/// `NotFound`, exactly one resolves directly, more than one is `Ambiguous`.
pub fn resolve_series_root(
    config: &Config,
    series: &str,
    root_arg: Option<&Path>,
    home: Option<&Path>,
) -> Result<String, TargetError> {
    match root_arg {
        Some(root_arg) => {
            let root = resolve_root_arg(config, root_arg, home).ok_or_else(|| {
                TargetError::RootNotFound(root_arg.to_string_lossy().into_owned())
            })?;
            if expand_tilde(Path::new(&root), home).join(series).is_dir() {
                Ok(root)
            } else {
                Err(TargetError::NotFound(series.to_string()))
            }
        }
        None => {
            let matches: Vec<String> = config
                .roots
                .paths
                .iter()
                .filter(|root| expand_tilde(Path::new(root), home).join(series).is_dir())
                .cloned()
                .collect();
            match matches.len() {
                0 => Err(TargetError::NotFound(series.to_string())),
                1 => Ok(matches.into_iter().next().unwrap()),
                _ => Err(TargetError::Ambiguous {
                    series: series.to_string(),
                    roots: matches,
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn tracked_config(roots: &[&Path]) -> Config {
        let mut config = Config::default();
        for root in roots {
            config.roots.paths.push(root.to_string_lossy().into_owned());
        }
        config
    }

    #[test]
    fn resolves_a_unique_series_name_with_no_root_arg() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(root.join("Monster")).unwrap();

        let config = tracked_config(&[&root]);
        let resolved = resolve_series_root(&config, "Monster", None, None).unwrap();
        assert_eq!(resolved, root.to_string_lossy());
    }

    #[test]
    fn errors_not_found_when_no_tracked_root_has_that_series() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let config = tracked_config(&[&root]);
        let err = resolve_series_root(&config, "Nonexistent", None, None).unwrap_err();
        assert_eq!(err, TargetError::NotFound("Nonexistent".to_string()));
    }

    #[test]
    fn ambiguous_when_two_tracked_roots_share_the_same_series_name() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        fs::create_dir_all(root_a.join("Monster")).unwrap();
        fs::create_dir_all(root_b.join("Monster")).unwrap();

        let config = tracked_config(&[&root_a, &root_b]);
        let err = resolve_series_root(&config, "Monster", None, None).unwrap_err();
        match err {
            TargetError::Ambiguous { series, roots } => {
                assert_eq!(series, "Monster");
                assert_eq!(roots.len(), 2);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn root_arg_disambiguates_directly() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        fs::create_dir_all(root_a.join("Monster")).unwrap();
        fs::create_dir_all(root_b.join("Monster")).unwrap();

        let config = tracked_config(&[&root_a, &root_b]);
        let resolved = resolve_series_root(&config, "Monster", Some(&root_b), None).unwrap();
        assert_eq!(resolved, root_b.to_string_lossy());
    }

    #[test]
    fn unknown_root_arg_errors_root_not_found() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(root.join("Monster")).unwrap();

        let config = tracked_config(&[&root]);
        let unrelated = dir.path().join("Unrelated");
        let err = resolve_series_root(&config, "Monster", Some(&unrelated), None).unwrap_err();
        assert_eq!(
            err,
            TargetError::RootNotFound(unrelated.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn root_arg_given_but_series_not_under_that_specific_root_errors_not_found() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        fs::create_dir_all(root_a.join("Monster")).unwrap();
        fs::create_dir_all(&root_b).unwrap();

        let config = tracked_config(&[&root_a, &root_b]);
        let err = resolve_series_root(&config, "Monster", Some(&root_b), None).unwrap_err();
        assert_eq!(err, TargetError::NotFound("Monster".to_string()));
    }
}
