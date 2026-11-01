//! Per-series display data shared by `list`, `status`, `start` and the resume
//! picker.
//!
//! Built from a live directory walk of every tracked root cross-referenced
//! with `state.toml`, so a deleted series never appears. ani-cli series have
//! no folder and come from their records alone.

use std::path::Path;

use crate::ani_cli;
use crate::anitomy;
use crate::config::Config;
use crate::exclude;
use crate::roots::{expand_tilde, normalize};
use crate::series;
use crate::state::{SeriesRecord, State, Status};

/// One tracked series' display state. `status: None` means never started
/// (no record in `state.toml`).
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesOverview {
    pub root: String,
    pub name: String,
    pub status: Option<Status>,
    pub episode_number: Option<u32>,
    pub season_number: Option<u32>,
    pub position_seconds: Option<f64>,
    pub file_count: usize,
}

impl SeriesOverview {
    /// `ep. 1`, or `S02E01` past season 1; `None` without an episode.
    pub fn episode_label(&self) -> Option<String> {
        self.episode_number
            .map(|ep| anitomy::episode_label(self.season_number, ep))
    }

    /// What follows the name in a listing: the file count, or `ani-cli` for a
    /// series that has no files.
    pub fn size_label(&self) -> String {
        if self.root == ani_cli::ROOT {
            ani_cli::SOURCE_ARG.to_string()
        } else {
            format!("{} files", self.file_count)
        }
    }
}

/// Every series under every tracked root, matched against `state`.
/// A root that can't be read (unmounted, permission denied) is skipped.
pub fn all_series(config: &Config, state: &State, home: Option<&Path>) -> Vec<SeriesOverview> {
    let mut overviews = Vec::new();
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
        let excluded = exclude::excluded_paths_for_root(config, root, home);

        for s in series_list {
            let file_count = series::ordered_episodes_excluding(
                &s.path,
                &config.roots.video_extensions,
                &excluded,
            )
            .map(|files| files.len())
            .unwrap_or(0);

            let record = state.find(root, &s.name);
            overviews.push(overview_for(root, &s.name, record, file_count));
        }
    }

    if config.ani_cli.enabled {
        for record in state.series.iter().filter(|r| r.root == ani_cli::ROOT) {
            overviews.push(overview_for(ani_cli::ROOT, &record.name, Some(record), 0));
        }
    }

    overviews
}

fn overview_for(
    root: &str,
    name: &str,
    record: Option<&SeriesRecord>,
    file_count: usize,
) -> SeriesOverview {
    match record {
        Some(r) => SeriesOverview {
            root: root.to_string(),
            name: name.to_string(),
            status: Some(r.status),
            episode_number: r.episode_number,
            season_number: r.season_number,
            position_seconds: Some(r.position_seconds),
            file_count,
        },
        None => SeriesOverview {
            root: root.to_string(),
            name: name.to_string(),
            status: None,
            episode_number: None,
            season_number: None,
            position_seconds: None,
            file_count,
        },
    }
}

/// Group rank for `list`: in progress, paused, dropped, completed, never started.
fn status_rank(status: Option<Status>) -> u8 {
    match status {
        Some(Status::InProgress) => 0,
        Some(Status::Paused) => 1,
        Some(Status::Dropped) => 2,
        Some(Status::Completed) => 3,
        None => 4,
    }
}

/// `list` order: by status rank, then series name, then root.
pub fn sorted_for_list(mut overviews: Vec<SeriesOverview>) -> Vec<SeriesOverview> {
    overviews.sort_by(|a, b| {
        status_rank(a.status)
            .cmp(&status_rank(b.status))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.root.cmp(&b.root))
    });
    overviews
}

/// Stable-sorts `overviews` so each root's series are contiguous, roots in
/// the order of `roots` (the config's); a root not in `roots` sorts last.
pub fn group_by_root(mut overviews: Vec<SeriesOverview>, roots: &[String]) -> Vec<SeriesOverview> {
    overviews.sort_by_key(|o| {
        roots
            .iter()
            .position(|r| *r == o.root)
            .unwrap_or(roots.len())
    });
    overviews
}

/// True when `routes` holds at least two distinct routes.
pub fn is_multi_route<'a>(routes: impl IntoIterator<Item = &'a str>) -> bool {
    let mut routes = routes.into_iter();
    match routes.next() {
        Some(first) => routes.any(|r| r != first),
        None => false,
    }
}

/// The short name shown for each route (parallel to `routes`): the root's
/// own folder name. Distinct routes that share a folder name keep their
/// full stored string instead, so two rows never look the same.
pub fn route_display(routes: &[&str]) -> Vec<String> {
    let short = |r: &str| {
        if r == ani_cli::ROOT {
            return ani_cli::SOURCE_ARG.to_string();
        }
        crate::roots::root_folder_name(r).unwrap_or_else(|| r.to_string())
    };
    routes
        .iter()
        .map(|r| {
            let name = short(r);
            let clash = routes.iter().any(|o| o != r && short(o) == name);
            if clash { r.to_string() } else { name }
        })
        .collect()
}

/// Appends an aligned `[route]` tag to each label; `labels` and `routes` are
/// parallel.
pub fn tag_with_routes(labels: &[String], routes: &[&str]) -> Vec<String> {
    let width = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    labels
        .iter()
        .zip(route_display(routes))
        .map(|(l, r)| format!("{l:<width$}  [{r}]"))
        .collect()
}

/// `start`'s display word: never started -> `"Start"`, paused -> `"Resume"`,
/// dropped or completed -> `"Rewatch?"`.
/// `in_progress` series belong to the resume picker, not `start`'s listing;
/// they map to `"Resume"` so the function stays total.
pub fn start_display_word(status: Option<Status>) -> &'static str {
    match status {
        None => "Start",
        Some(Status::Paused) => "Resume",
        Some(Status::Dropped | Status::Completed) => "Rewatch?",
        Some(Status::InProgress) => "Resume",
    }
}

/// Series offered by `start`: everything not `in_progress`. ani-cli shows are
/// never started from here; they appear only once watched in ani-cli.
pub fn eligible_for_start(overviews: &[SeriesOverview]) -> Vec<SeriesOverview> {
    overviews
        .iter()
        .filter(|o| o.status != Some(Status::InProgress) && o.root != ani_cli::ROOT)
        .cloned()
        .collect()
}

/// One `start` listing line, e.g. `"Monster — Resume (ep. 9, 74 files)"` or
/// `"Apothecary Diaries — Start (12 files)"`.
pub fn start_line(overview: &SeriesOverview) -> String {
    let word = start_display_word(overview.status);
    match (overview.status, overview.episode_label()) {
        (Some(Status::Paused), Some(ep)) => format!(
            "{} — {} ({ep}, {} files)",
            overview.name, word, overview.file_count
        ),
        _ => format!(
            "{} — {} ({} files)",
            overview.name, word, overview.file_count
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overview(name: &str, status: Option<Status>) -> SeriesOverview {
        SeriesOverview {
            root: "~/Anime".to_string(),
            name: name.to_string(),
            status,
            episode_number: None,
            season_number: None,
            position_seconds: None,
            file_count: 1,
        }
    }

    #[test]
    fn paused_start_line_shows_season_past_season_one() {
        let mut o = overview("Bakuman", Some(Status::Paused));
        o.episode_number = Some(1);
        o.season_number = Some(2);
        assert_eq!(start_line(&o), "Bakuman — Resume (S02E01, 1 files)");
    }

    #[test]
    fn paused_start_line_without_season_keeps_ep_form() {
        let mut o = overview("Bakuman", Some(Status::Paused));
        o.episode_number = Some(9);
        assert_eq!(start_line(&o), "Bakuman — Resume (ep. 9, 1 files)");
    }

    #[test]
    fn overview_for_passes_the_stored_season_through() {
        let record = SeriesRecord {
            root: "~/Anime".to_string(),
            name: "Bakuman".to_string(),
            status: Status::InProgress,
            current_relative_path: "Bakuman/S02/e.mkv".to_string(),
            position_seconds: 5.0,
            episode_number: Some(1),
            updated_at: 1,
            mal_episode_awareness: None,
            season_number: Some(2),
        };
        let o = overview_for("~/Anime", "Bakuman", Some(&record), 3);
        assert_eq!(o.season_number, Some(2));
        assert_eq!(o.episode_label(), Some("S02E01".to_string()));
    }

    #[test]
    fn start_display_word_is_start_for_never_started() {
        assert_eq!(start_display_word(None), "Start");
    }

    #[test]
    fn start_display_word_is_resume_for_paused() {
        assert_eq!(start_display_word(Some(Status::Paused)), "Resume");
    }

    #[test]
    fn start_display_word_is_rewatch_for_dropped() {
        assert_eq!(start_display_word(Some(Status::Dropped)), "Rewatch?");
    }

    #[test]
    fn start_display_word_is_rewatch_for_completed() {
        assert_eq!(start_display_word(Some(Status::Completed)), "Rewatch?");
    }

    #[test]
    fn paused_display_word_never_contains_rewatch() {
        // A paused series must never be labeled "Rewatch?".
        let word = start_display_word(Some(Status::Paused));
        assert!(!word.contains("Rewatch"));
    }

    #[test]
    fn paused_start_line_never_contains_rewatch() {
        let mut o = overview("Monster", Some(Status::Paused));
        o.episode_number = Some(9);
        let line = start_line(&o);
        assert!(!line.contains("Rewatch"));
        assert!(line.contains("Resume"));
    }

    #[test]
    fn all_four_real_states_produce_correct_and_distinct_words_together() {
        // Asserted together so two states can't silently share a word.
        let never_started = start_display_word(None);
        let paused = start_display_word(Some(Status::Paused));
        let dropped = start_display_word(Some(Status::Dropped));
        let in_progress = start_display_word(Some(Status::InProgress));

        assert_eq!(never_started, "Start");
        assert_eq!(paused, "Resume");
        assert_eq!(dropped, "Rewatch?");
        // in_progress is filtered out by eligible_for_start; the word is a total fallback.
        assert_eq!(in_progress, "Resume");

        let words = [never_started, paused, dropped];
        for i in 0..words.len() {
            for j in (i + 1)..words.len() {
                assert_ne!(
                    words[i], words[j],
                    "the three real start-listing states must never share a display word"
                );
            }
        }
    }

    #[test]
    fn eligible_for_start_excludes_in_progress() {
        let overviews = vec![
            overview("A", Some(Status::InProgress)),
            overview("B", Some(Status::Paused)),
            overview("C", None),
        ];
        let eligible = eligible_for_start(&overviews);
        let names: Vec<_> = eligible.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["B", "C"]);
    }

    #[test]
    fn sorted_for_list_groups_by_status_then_alphabetical_within_group() {
        let overviews = vec![
            overview("Zeta", None),
            overview("Beta", Some(Status::Dropped)),
            overview("Alpha", Some(Status::InProgress)),
            overview("Delta", Some(Status::Paused)),
            overview("Charlie", Some(Status::InProgress)),
        ];
        let sorted = sorted_for_list(overviews);
        let names: Vec<_> = sorted.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Charlie", "Delta", "Beta", "Zeta"]);
    }

    #[test]
    fn sorted_for_list_places_completed_after_dropped_and_before_never_started() {
        let sorted = sorted_for_list(vec![
            overview("Zeta", None),
            overview("Alpha", Some(Status::Completed)),
            overview("Omega", Some(Status::Dropped)),
            overview("Delta", Some(Status::Paused)),
            overview("Yankee", Some(Status::InProgress)),
        ]);
        let names: Vec<_> = sorted.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["Yankee", "Delta", "Omega", "Alpha", "Zeta"]);
    }

    #[test]
    fn sorted_for_list_breaks_name_ties_by_root() {
        let mut a = overview("Monster", Some(Status::InProgress));
        a.root = "~/AltAnime".to_string();
        let mut b = overview("Monster", Some(Status::InProgress));
        b.root = "~/Anime".to_string();

        let sorted = sorted_for_list(vec![a.clone(), b.clone()]);
        assert_eq!(sorted[0].root, "~/AltAnime");
        assert_eq!(sorted[1].root, "~/Anime");
    }

    #[test]
    fn all_series_enumerates_across_tracked_roots_and_cross_references_state() {
        use crate::state::SeriesRecord;
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        fs::create_dir_all(root.join("Monster")).unwrap();
        fs::File::create(root.join("Monster").join("ep01.mkv")).unwrap();
        fs::create_dir_all(root.join("Fresh Show")).unwrap();
        fs::File::create(root.join("Fresh Show").join("ep01.mkv")).unwrap();

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let state = State {
            series: vec![SeriesRecord {
                root: root.to_string_lossy().into_owned(),
                name: "Monster".to_string(),
                status: Status::InProgress,
                current_relative_path: "Monster/ep01.mkv".to_string(),
                position_seconds: 100.0,
                episode_number: Some(1),
                updated_at: 1,
                mal_episode_awareness: None,
                season_number: None,
            }],

            mal_links: Vec::new(),
        };

        let overviews = all_series(&config, &state, None);
        assert_eq!(overviews.len(), 2);

        let monster = overviews.iter().find(|o| o.name == "Monster").unwrap();
        assert_eq!(monster.status, Some(Status::InProgress));
        assert_eq!(monster.file_count, 1);

        let fresh = overviews.iter().find(|o| o.name == "Fresh Show").unwrap();
        assert_eq!(fresh.status, None);
        assert_eq!(fresh.file_count, 1);
    }

    #[test]
    fn a_nested_tracked_root_does_not_also_appear_as_a_series_of_its_parent_root() {
        // With `Anime` and `Anime/Rewatches` both tracked, `Rewatches` is a
        // root, not a series of `Anime`.
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let anime_root = dir.path().join("Anime");
        let rewatches_root = anime_root.join("Rewatches");
        fs::create_dir_all(anime_root.join("Monster")).unwrap();
        fs::create_dir_all(rewatches_root.join("Cowboy Bebop")).unwrap();

        let mut config = Config::default();
        config
            .roots
            .paths
            .push(anime_root.to_string_lossy().into_owned());
        config
            .roots
            .paths
            .push(rewatches_root.to_string_lossy().into_owned());

        let state = State {
            series: vec![],
            mal_links: Vec::new(),
        };
        let overviews = all_series(&config, &state, None);

        let names: Vec<_> = overviews.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names.iter().filter(|n| **n == "Rewatches").count(), 0);
        assert!(names.contains(&"Monster"));
        assert!(names.contains(&"Cowboy Bebop"));
        assert_eq!(overviews.len(), 2);
    }

    #[test]
    fn episode_number_falls_back_to_ordinal_when_anitomy_cannot_parse() {
        // The stored episode_number is surfaced verbatim, with no re-parsing.
        let record = SeriesRecord {
            root: "~/Anime".to_string(),
            name: "No Numbers".to_string(),
            status: Status::InProgress,
            current_relative_path: "No Numbers/beta.mkv".to_string(),
            position_seconds: 200.0,
            episode_number: Some(2), // ordinal fallback position, not a parsed number
            updated_at: 1,
            mal_episode_awareness: None,
            season_number: None,
        };
        let o = overview_for("~/Anime", "No Numbers", Some(&record), 3);
        assert_eq!(o.episode_number, Some(2));
    }

    #[test]
    fn start_word_for_every_status_matches_the_hand_written_table() {
        // No wildcard: a new `Status` variant must be added here.
        fn expected(status: Status) -> &'static str {
            match status {
                Status::InProgress => "Resume",
                Status::Paused => "Resume",
                Status::Dropped => "Rewatch?",
                Status::Completed => "Rewatch?",
            }
        }
        for status in [
            Status::InProgress,
            Status::Paused,
            Status::Dropped,
            Status::Completed,
        ] {
            assert_eq!(
                start_display_word(Some(status)),
                expected(status),
                "{status:?}"
            );
        }
    }

    fn in_root(root: &str, name: &str) -> SeriesOverview {
        SeriesOverview {
            root: root.to_string(),
            ..overview(name, None)
        }
    }

    #[test]
    fn group_by_root_makes_roots_contiguous_in_config_order_keeping_inner_order() {
        let roots = vec!["~/Anime".to_string(), "/mnt/nas/Shows".to_string()];
        let input = vec![
            in_root("/mnt/nas/Shows", "Frieren"),
            in_root("~/Anime", "Monster"),
            in_root("/mnt/nas/Shows", "Nana"),
            in_root("~/Anime", "Naruto"),
        ];

        let got: Vec<(String, String)> = group_by_root(input, &roots)
            .into_iter()
            .map(|o| (o.root, o.name))
            .collect();

        let want = [
            ("~/Anime", "Monster"),
            ("~/Anime", "Naruto"),
            ("/mnt/nas/Shows", "Frieren"),
            ("/mnt/nas/Shows", "Nana"),
        ];
        let want: Vec<(String, String)> = want
            .iter()
            .map(|(r, n)| (r.to_string(), n.to_string()))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn group_by_root_puts_unknown_roots_last_and_handles_empty_input() {
        let roots = vec!["~/Anime".to_string()];
        let got = group_by_root(vec![in_root("/gone", "A"), in_root("~/Anime", "B")], &roots);
        let names: Vec<&str> = got.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["B", "A"]);
        assert_eq!(group_by_root(Vec::new(), &roots), Vec::new());
    }

    #[test]
    fn is_multi_route_needs_two_distinct_routes() {
        assert!(!is_multi_route([]));
        assert!(!is_multi_route(["~/Anime"]));
        assert!(!is_multi_route(["~/Anime", "~/Anime"]));
        assert!(is_multi_route(["~/Anime", "/mnt"]));
        assert!(is_multi_route(["~/Anime", "~/Anime", "/mnt"]));
    }

    #[test]
    fn tag_with_routes_pads_to_the_longest_label_plus_two_spaces() {
        let labels = vec!["ab".to_string(), "abcd".to_string()];
        assert_eq!(
            tag_with_routes(&labels, &["~/A", "/b"]),
            vec!["ab    [A]".to_string(), "abcd  [b]".to_string()]
        );
        assert_eq!(tag_with_routes(&[], &[]), Vec::<String>::new());
    }

    #[test]
    fn route_display_uses_the_root_folder_name() {
        assert_eq!(
            route_display(&["~/Anime", "/mnt/nas/Shows/", "Library"]),
            vec!["Anime", "Shows", "Library"]
        );
    }

    #[test]
    fn route_display_keeps_the_full_root_only_for_routes_sharing_a_folder_name() {
        assert_eq!(
            route_display(&["~/Anime", "/mnt/Anime", "/mnt/Shows"]),
            vec!["~/Anime", "/mnt/Anime", "Shows"]
        );
    }

    #[test]
    fn route_display_falls_back_to_the_stored_string_without_a_folder_name() {
        assert_eq!(route_display(&["/", "/mnt/Shows"]), vec!["/", "Shows"]);
        assert_eq!(route_display(&[]), Vec::<String>::new());
    }

    // --- ani-cli series ---

    fn ani_record(name: &str, status: Status) -> SeriesRecord {
        SeriesRecord {
            root: ani_cli::ROOT.to_string(),
            name: name.to_string(),
            status,
            current_relative_path: String::new(),
            position_seconds: 300.0,
            episode_number: Some(9),
            updated_at: 1,
            mal_episode_awareness: None,
            season_number: None,
        }
    }

    fn ani_overview(name: &str, status: Status) -> SeriesOverview {
        SeriesOverview {
            root: ani_cli::ROOT.to_string(),
            name: name.to_string(),
            status: Some(status),
            episode_number: Some(9),
            season_number: None,
            position_seconds: Some(300.0),
            file_count: 0,
        }
    }

    #[test]
    fn all_series_lists_ani_cli_records_only_when_the_source_is_enabled() {
        let state = State {
            series: vec![ani_record("Blue Lock", Status::Paused)],
            mal_links: Vec::new(),
        };
        let mut config = Config::default();

        assert_eq!(all_series(&config, &state, None), Vec::new());

        config.ani_cli.enabled = true;
        assert_eq!(
            all_series(&config, &state, None),
            vec![ani_overview("Blue Lock", Status::Paused)]
        );
    }

    #[test]
    fn all_series_never_invents_a_never_started_ani_cli_show() {
        let mut config = Config::default();
        config.ani_cli.enabled = true;

        assert_eq!(all_series(&config, &State::default(), None), Vec::new());
    }

    #[test]
    fn eligible_for_start_never_offers_an_ani_cli_show_at_any_status() {
        for status in [
            Status::InProgress,
            Status::Paused,
            Status::Dropped,
            Status::Completed,
        ] {
            assert_eq!(
                eligible_for_start(&[ani_overview("Blue Lock", status)]),
                Vec::new(),
                "{status:?}"
            );
        }
        let folder = overview("Monster", Some(Status::Paused));
        assert_eq!(
            eligible_for_start(&[ani_overview("Blue Lock", Status::Paused), folder.clone()]),
            vec![folder]
        );
    }

    #[test]
    fn size_label_is_the_file_count_for_folders_and_ani_cli_for_streams() {
        let mut folder = overview("Monster", None);
        folder.file_count = 74;

        assert_eq!(folder.size_label(), "74 files");
        assert_eq!(
            ani_overview("Blue Lock", Status::Paused).size_label(),
            "ani-cli"
        );
    }

    #[test]
    fn route_display_names_the_ani_cli_source_and_keeps_full_strings_on_a_folder_clash() {
        assert_eq!(
            route_display(&[ani_cli::ROOT, "~/Anime"]),
            vec!["ani-cli".to_string(), "Anime".to_string()]
        );
        assert_eq!(
            route_display(&[ani_cli::ROOT, "~/ani-cli"]),
            vec!["ani-cli://".to_string(), "~/ani-cli".to_string()]
        );
    }
}
