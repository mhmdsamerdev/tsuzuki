//! Resolves a series name (optionally with `--root`) or a root's folder name
//! to the tracked root it belongs to. Shared by `resume`, `start`, `pause`,
//! and `drop`.

use std::path::Path;

use crate::ani_cli;
use crate::config::Config;
use crate::roots::{expand_tilde, root_folder_name, tracked_real_paths};
use crate::series::is_series_dir;
use crate::state::{State, resolve_root_arg};

#[derive(Debug, PartialEq, Eq)]
pub enum TargetError {
    /// `series` is not a depth-1 directory under any tracked root, or under
    /// the given `--root`.
    NotFound(String),
    /// `--root` was given but doesn't match any currently tracked root.
    RootNotFound(String),
    /// `series` exists as a depth-1 directory under more than one tracked
    /// root and no `--root` was given to disambiguate.
    Ambiguous { series: String, roots: Vec<String> },
}

/// True when the ani-cli source is on and `state` holds a series by that title.
fn has_ani_cli_series(config: &Config, state: &State, series: &str) -> bool {
    config.ani_cli.enabled
        && state
            .series
            .iter()
            .any(|r| r.root == ani_cli::ROOT && r.name == series)
}

/// True when `root_arg` is the `ani-cli` source word and the source is on.
fn is_ani_cli_arg(config: &Config, root_arg: &Path) -> bool {
    config.ani_cli.enabled && root_arg == Path::new(ani_cli::SOURCE_ARG)
}

/// Resolves a `--root` used to narrow a picker: a tracked root's stored form,
/// or the ani-cli source's reserved root for the word `ani-cli`.
pub fn resolve_scope_root(config: &Config, root_arg: &Path, home: Option<&Path>) -> Option<String> {
    if is_ani_cli_arg(config, root_arg) {
        return Some(ani_cli::ROOT.to_string());
    }
    resolve_root_arg(config, root_arg, home)
}

/// Resolves `series` to the stored form of the tracked root it lives under.
/// With `root_arg`, only that root is considered: an untracked one is
/// `RootNotFound`, one without `series` is `NotFound`. Without it, zero
/// matching roots is `NotFound` and more than one is `Ambiguous`.
pub fn resolve_series_root(
    config: &Config,
    state: &State,
    series: &str,
    root_arg: Option<&Path>,
    home: Option<&Path>,
) -> Result<String, TargetError> {
    match root_arg {
        Some(root_arg) if is_ani_cli_arg(config, root_arg) => {
            if has_ani_cli_series(config, state, series) {
                Ok(ani_cli::ROOT.to_string())
            } else {
                Err(TargetError::NotFound(series.to_string()))
            }
        }
        Some(root_arg) => {
            let root = resolve_root_arg(config, root_arg, home).ok_or_else(|| {
                TargetError::RootNotFound(root_arg.to_string_lossy().into_owned())
            })?;
            let real_roots = tracked_real_paths(config, home);
            if is_series_dir(
                &expand_tilde(Path::new(&root), home).join(series),
                &real_roots,
            ) {
                Ok(root)
            } else {
                Err(TargetError::NotFound(series.to_string()))
            }
        }
        None => {
            let real_roots = tracked_real_paths(config, home);
            let mut matches: Vec<String> = config
                .roots
                .paths
                .iter()
                .filter(|root| {
                    is_series_dir(
                        &expand_tilde(Path::new(root), home).join(series),
                        &real_roots,
                    )
                })
                .cloned()
                .collect();
            if has_ani_cli_series(config, state, series) {
                matches.push(ani_cli::ROOT.to_string());
            }
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

/// What a bare `start`/`resume` argument names.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    /// A series under exactly one tracked root (that root's stored form).
    Series(String),
    /// A tracked root, by folder name (its stored form).
    Root(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum PositionalError {
    /// Neither a series under any root nor a root's folder name.
    NotFound(String),
    /// A series that exists under more than one tracked root.
    AmbiguousSeries { series: String, roots: Vec<String> },
    /// A folder name shared by more than one tracked root.
    AmbiguousRoot { name: String, roots: Vec<String> },
    /// Both a series name and a root's folder name.
    Collision {
        name: String,
        series_roots: Vec<String>,
        roots: Vec<String>,
    },
}

/// Resolves `arg` as a series or, failing that, a root's folder name. Both
/// lookups are exact and case-sensitive; matching both is a `Collision`,
/// never a silent pick.
pub fn resolve_positional(
    config: &Config,
    state: &State,
    arg: &str,
    home: Option<&Path>,
) -> Result<Resolved, PositionalError> {
    let real_roots = tracked_real_paths(config, home);
    let mut series_roots: Vec<String> = config
        .roots
        .paths
        .iter()
        .filter(|root| is_series_dir(&expand_tilde(Path::new(root), home).join(arg), &real_roots))
        .cloned()
        .collect();
    if has_ani_cli_series(config, state, arg) {
        series_roots.push(ani_cli::ROOT.to_string());
    }
    let mut roots: Vec<String> = config
        .roots
        .paths
        .iter()
        .filter(|root| root_folder_name(root).as_deref() == Some(arg))
        .cloned()
        .collect();
    if is_ani_cli_arg(config, Path::new(arg)) {
        roots.push(ani_cli::ROOT.to_string());
    }
    match (series_roots.len(), roots.len()) {
        (0, 0) => Err(PositionalError::NotFound(arg.to_string())),
        (1, 0) => Ok(Resolved::Series(series_roots.into_iter().next().unwrap())),
        (_, 0) => Err(PositionalError::AmbiguousSeries {
            series: arg.to_string(),
            roots: series_roots,
        }),
        (0, 1) => Ok(Resolved::Root(roots.into_iter().next().unwrap())),
        (0, _) => Err(PositionalError::AmbiguousRoot {
            name: arg.to_string(),
            roots,
        }),
        _ => Err(PositionalError::Collision {
            name: arg.to_string(),
            series_roots,
            roots,
        }),
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
        let resolved =
            resolve_series_root(&config, &State::default(), "Monster", None, None).unwrap();
        assert_eq!(resolved, root.to_string_lossy());
    }

    #[test]
    fn errors_not_found_when_no_tracked_root_has_that_series() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(&root).unwrap();

        let config = tracked_config(&[&root]);
        let err =
            resolve_series_root(&config, &State::default(), "Nonexistent", None, None).unwrap_err();
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
        let err =
            resolve_series_root(&config, &State::default(), "Monster", None, None).unwrap_err();
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
        let resolved =
            resolve_series_root(&config, &State::default(), "Monster", Some(&root_b), None)
                .unwrap();
        assert_eq!(resolved, root_b.to_string_lossy());
    }

    #[test]
    fn unknown_root_arg_errors_root_not_found() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(root.join("Monster")).unwrap();

        let config = tracked_config(&[&root]);
        let unrelated = dir.path().join("Unrelated");
        let err = resolve_series_root(
            &config,
            &State::default(),
            "Monster",
            Some(&unrelated),
            None,
        )
        .unwrap_err();
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
        let err = resolve_series_root(&config, &State::default(), "Monster", Some(&root_b), None)
            .unwrap_err();
        assert_eq!(err, TargetError::NotFound("Monster".to_string()));
    }
    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn positional_naming_only_a_series_resolves_to_its_root() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("anime");
        let re = dir.path().join("re");
        fs::create_dir_all(anime.join("Monster")).unwrap();
        fs::create_dir_all(&re).unwrap();
        let config = tracked_config(&[&anime, &re]);

        assert_eq!(
            resolve_positional(&config, &State::default(), "Monster", None),
            Ok(Resolved::Series(s(&anime)))
        );
    }

    #[test]
    fn positional_naming_only_a_root_folder_resolves_to_that_root() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("anime");
        let re = dir.path().join("re");
        fs::create_dir_all(anime.join("Monster")).unwrap();
        fs::create_dir_all(&re).unwrap();
        let config = tracked_config(&[&anime, &re]);

        assert_eq!(
            resolve_positional(&config, &State::default(), "re", None),
            Ok(Resolved::Root(s(&re)))
        );
    }

    #[test]
    fn positional_naming_nothing_is_not_found() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("anime");
        fs::create_dir_all(anime.join("Monster")).unwrap();
        let config = tracked_config(&[&anime]);

        assert_eq!(
            resolve_positional(&config, &State::default(), "nope", None),
            Err(PositionalError::NotFound("nope".to_string()))
        );
    }

    #[test]
    fn positional_naming_both_a_series_and_a_root_is_a_collision_listing_both() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("anime");
        let re = dir.path().join("re");
        fs::create_dir_all(anime.join("re")).unwrap();
        fs::create_dir_all(&re).unwrap();
        let config = tracked_config(&[&anime, &re]);

        assert_eq!(
            resolve_positional(&config, &State::default(), "re", None),
            Err(PositionalError::Collision {
                name: "re".to_string(),
                series_roots: vec![s(&anime)],
                roots: vec![s(&re)],
            })
        );
    }

    #[test]
    fn positional_collision_beats_an_ambiguous_series_and_an_ambiguous_root() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a").join("re");
        let b = dir.path().join("b").join("re");
        fs::create_dir_all(a.join("re")).unwrap();
        fs::create_dir_all(b.join("re")).unwrap();
        let config = tracked_config(&[&a, &b]);

        assert_eq!(
            resolve_positional(&config, &State::default(), "re", None),
            Err(PositionalError::Collision {
                name: "re".to_string(),
                series_roots: vec![s(&a), s(&b)],
                roots: vec![s(&a), s(&b)],
            })
        );
    }

    #[test]
    fn positional_series_under_two_roots_is_ambiguous_series() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir_all(a.join("Monster")).unwrap();
        fs::create_dir_all(b.join("Monster")).unwrap();
        let config = tracked_config(&[&a, &b]);

        assert_eq!(
            resolve_positional(&config, &State::default(), "Monster", None),
            Err(PositionalError::AmbiguousSeries {
                series: "Monster".to_string(),
                roots: vec![s(&a), s(&b)],
            })
        );
    }

    #[test]
    fn positional_folder_name_shared_by_two_roots_is_ambiguous_root() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("x").join("anime");
        let b = dir.path().join("y").join("anime");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let config = tracked_config(&[&a, &b]);

        assert_eq!(
            resolve_positional(&config, &State::default(), "anime", None),
            Err(PositionalError::AmbiguousRoot {
                name: "anime".to_string(),
                roots: vec![s(&a), s(&b)],
            })
        );
    }

    #[test]
    fn positional_root_name_matching_ignores_a_trailing_slash_on_the_stored_root() {
        let dir = tempdir().unwrap();
        let re = dir.path().join("re");
        fs::create_dir_all(&re).unwrap();
        let mut config = Config::default();
        config.roots.paths.push(format!("{}/", re.display()));

        assert_eq!(
            resolve_positional(&config, &State::default(), "re", None),
            Ok(Resolved::Root(format!("{}/", re.display())))
        );
    }

    #[test]
    fn positional_root_name_matching_is_case_sensitive() {
        let dir = tempdir().unwrap();
        let re = dir.path().join("re");
        fs::create_dir_all(&re).unwrap();
        let config = tracked_config(&[&re]);

        assert_eq!(
            resolve_positional(&config, &State::default(), "RE", None),
            Err(PositionalError::NotFound("RE".to_string()))
        );
    }

    #[test]
    fn positional_root_name_matching_handles_non_ascii_folder_names() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("アニメ");
        fs::create_dir_all(&root).unwrap();
        let config = tracked_config(&[&root]);

        assert_eq!(
            resolve_positional(&config, &State::default(), "アニメ", None),
            Ok(Resolved::Root(s(&root)))
        );
    }

    #[test]
    fn positional_root_name_matches_the_stored_form_not_a_symlink_target() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("real");
        let link = dir.path().join("re");
        fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let config = tracked_config(&[&link]);

        assert_eq!(
            resolve_positional(&config, &State::default(), "re", None),
            Ok(Resolved::Root(s(&link)))
        );
        assert_eq!(
            resolve_positional(&config, &State::default(), "real", None),
            Err(PositionalError::NotFound("real".to_string()))
        );
    }

    #[test]
    fn positional_tilde_root_name_matches_and_series_lookup_expands_the_tilde() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        fs::create_dir_all(home.join("re").join("Bleach")).unwrap();
        let mut config = Config::default();
        config.roots.paths.push("~/re".to_string());

        assert_eq!(
            resolve_positional(&config, &State::default(), "re", Some(home)),
            Ok(Resolved::Root("~/re".to_string()))
        );
        assert_eq!(
            resolve_positional(&config, &State::default(), "Bleach", Some(home)),
            Ok(Resolved::Series("~/re".to_string()))
        );
    }

    #[test]
    fn positional_nested_root_name_resolves_as_root_not_series() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("anime");
        let rewatch = anime.join("rewatch");
        fs::create_dir_all(rewatch.join("Bleach")).unwrap();
        fs::create_dir_all(anime.join("Monster")).unwrap();
        let config = tracked_config(&[&anime, &rewatch]);

        assert_eq!(
            resolve_positional(&config, &State::default(), "rewatch", None),
            Ok(Resolved::Root(s(&rewatch)))
        );
        assert_eq!(
            resolve_positional(&config, &State::default(), "Monster", None),
            Ok(Resolved::Series(s(&anime)))
        );
    }

    #[test]
    fn series_root_with_root_arg_rejects_a_nested_root_as_series() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("anime");
        let rewatch = anime.join("rewatch");
        fs::create_dir_all(&rewatch).unwrap();
        let config = tracked_config(&[&anime, &rewatch]);

        assert_eq!(
            resolve_series_root(&config, &State::default(), "rewatch", Some(&anime), None),
            Err(TargetError::NotFound("rewatch".to_string()))
        );
    }

    #[test]
    fn series_root_without_root_arg_ignores_a_nested_root_in_its_parent() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("anime");
        let rewatch = anime.join("rewatch");
        fs::create_dir_all(rewatch.join("Bleach")).unwrap();
        let config = tracked_config(&[&anime, &rewatch]);

        assert_eq!(
            resolve_series_root(&config, &State::default(), "rewatch", None, None),
            Err(TargetError::NotFound("rewatch".to_string()))
        );
        assert_eq!(
            resolve_series_root(&config, &State::default(), "Bleach", None, None),
            Ok(s(&rewatch))
        );
    }

    // --- ani-cli series ---

    fn ani_config() -> Config {
        let mut config = Config::default();
        config.ani_cli.enabled = true;
        config
    }

    fn ani_state(names: &[&str]) -> State {
        State {
            series: names
                .iter()
                .map(|name| crate::state::SeriesRecord {
                    root: ani_cli::ROOT.to_string(),
                    name: name.to_string(),
                    status: crate::state::Status::InProgress,
                    current_relative_path: String::new(),
                    position_seconds: 300.0,
                    episode_number: Some(9),
                    updated_at: 1,
                    mal_episode_awareness: None,
                    season_number: None,
                })
                .collect(),
            mal_links: Vec::new(),
        }
    }

    #[test]
    fn an_ani_cli_series_resolves_to_the_reserved_root_by_name_or_with_root_ani_cli() {
        let config = ani_config();
        let state = ani_state(&["Blue Lock"]);

        assert_eq!(
            resolve_series_root(&config, &state, "Blue Lock", None, None),
            Ok(ani_cli::ROOT.to_string())
        );
        assert_eq!(
            resolve_series_root(
                &config,
                &state,
                "Blue Lock",
                Some(Path::new("ani-cli")),
                None
            ),
            Ok(ani_cli::ROOT.to_string())
        );
        assert_eq!(
            resolve_series_root(&config, &state, "Other", Some(Path::new("ani-cli")), None),
            Err(TargetError::NotFound("Other".to_string()))
        );
    }

    #[test]
    fn an_ani_cli_series_does_not_resolve_while_the_source_is_off() {
        let state = ani_state(&["Blue Lock"]);

        assert_eq!(
            resolve_series_root(&Config::default(), &state, "Blue Lock", None, None),
            Err(TargetError::NotFound("Blue Lock".to_string()))
        );
        assert_eq!(
            resolve_positional(&Config::default(), &state, "ani-cli", None),
            Err(PositionalError::NotFound("ani-cli".to_string()))
        );
    }

    #[test]
    fn a_title_shared_by_a_folder_series_and_an_ani_cli_show_is_ambiguous_until_root_is_given() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(root.join("Blue Lock")).unwrap();
        let mut config = tracked_config(&[&root]);
        config.ani_cli.enabled = true;
        let state = ani_state(&["Blue Lock"]);
        let stored = root.to_string_lossy().into_owned();

        assert_eq!(
            resolve_series_root(&config, &state, "Blue Lock", None, None),
            Err(TargetError::Ambiguous {
                series: "Blue Lock".to_string(),
                roots: vec![stored.clone(), ani_cli::ROOT.to_string()],
            })
        );
        assert_eq!(
            resolve_series_root(&config, &state, "Blue Lock", Some(&root), None),
            Ok(stored)
        );
        assert_eq!(
            resolve_series_root(
                &config,
                &state,
                "Blue Lock",
                Some(Path::new("ani-cli")),
                None
            ),
            Ok(ani_cli::ROOT.to_string())
        );
    }

    #[test]
    fn positional_resolves_an_ani_cli_title_as_a_series_and_the_word_ani_cli_as_a_root() {
        let config = ani_config();
        let state = ani_state(&["Blue Lock"]);

        assert_eq!(
            resolve_positional(&config, &state, "Blue Lock", None),
            Ok(Resolved::Series(ani_cli::ROOT.to_string()))
        );
        assert_eq!(
            resolve_positional(&config, &state, "ani-cli", None),
            Ok(Resolved::Root(ani_cli::ROOT.to_string()))
        );
    }

    #[test]
    fn positional_ani_cli_word_that_is_also_a_folder_root_name_is_an_ambiguous_root() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("ani-cli");
        fs::create_dir_all(&root).unwrap();
        let mut config = tracked_config(&[&root]);
        config.ani_cli.enabled = true;

        assert_eq!(
            resolve_positional(&config, &State::default(), "ani-cli", None),
            Err(PositionalError::AmbiguousRoot {
                name: "ani-cli".to_string(),
                roots: vec![
                    root.to_string_lossy().into_owned(),
                    ani_cli::ROOT.to_string()
                ],
            })
        );
    }

    #[test]
    fn scope_root_maps_the_ani_cli_word_only_when_enabled() {
        let state_root = Path::new("ani-cli");

        assert_eq!(
            resolve_scope_root(&ani_config(), state_root, None),
            Some(ani_cli::ROOT.to_string())
        );
        assert_eq!(
            resolve_scope_root(&Config::default(), state_root, None),
            None
        );
    }
}
