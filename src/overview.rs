//! Per-series display data shared by `list`/`status`/`start`/the resume
//! picker (design.md §6/§9, CLAUDE.md's "`list` ordering and episode-number
//! display" and "`start`'s listing has three distinct display words").
//!
//! Built from a live directory walk of every tracked root cross-referenced
//! with `state.toml` — never from `state.toml` alone — so a deleted series
//! simply never appears here, structurally, matching the same
//! `series_in_root`-vs-`state.toml` precedent CLAUDE.md already established
//! for `start`/`list`.

use std::path::Path;

use crate::config::Config;
use crate::exclude;
use crate::roots::expand_tilde;
use crate::series;
use crate::state::{SeriesRecord, State, Status};

/// One tracked series' current display state. `status: None` means
/// never-started — no `state.toml` record exists for it at all.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesOverview {
    pub root: String,
    pub name: String,
    pub status: Option<Status>,
    pub episode_number: Option<u32>,
    pub position_seconds: Option<f64>,
    pub file_count: usize,
}

/// Enumerates every series under every tracked root, cross-referenced with
/// `state`. A root that currently can't be read (unmounted, permission
/// denied) is silently skipped — consistent with `exclude.rs`'s
/// `suggest_candidates` precedent for the same situation, since this isn't
/// the place to surface a filesystem error for one bad root out of many.
pub fn all_series(config: &Config, state: &State, home: Option<&Path>) -> Vec<SeriesOverview> {
    let mut overviews = Vec::new();

    for root in &config.roots.paths {
        let expanded_root = expand_tilde(Path::new(root), home);
        let Ok(series_list) = series::series_in_root(&expanded_root) else {
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
            position_seconds: Some(r.position_seconds),
            file_count,
        },
        None => SeriesOverview {
            root: root.to_string(),
            name: name.to_string(),
            status: None,
            episode_number: None,
            position_seconds: None,
            file_count,
        },
    }
}

/// Sort rank for `list`'s grouping: `in_progress` -> `paused` -> `dropped`
/// -> never-started, per CLAUDE.md.
fn status_rank(status: Option<Status>) -> u8 {
    match status {
        Some(Status::InProgress) => 0,
        Some(Status::Paused) => 1,
        Some(Status::Dropped) => 2,
        None => 3,
    }
}

/// `list`'s fixed ordering: grouped by status rank first, alphabetical by
/// series name within each group; `root` breaks ties for the same name
/// across composable roots, keeping output deterministic.
pub fn sorted_for_list(mut overviews: Vec<SeriesOverview>) -> Vec<SeriesOverview> {
    overviews.sort_by(|a, b| {
        status_rank(a.status)
            .cmp(&status_rank(b.status))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.root.cmp(&b.root))
    });
    overviews
}

/// `start`'s per-state display word, exactly per CLAUDE.md: never-started
/// (no record) -> `"Start"`; paused -> `"Resume"`; dropped (manual or
/// detected-completion — no distinction is made) -> `"Rewatch?"`.
/// `in_progress` never legitimately reaches this function — those series
/// belong to the automatic resume picker, not `start`'s listing — but it
/// stays total (falls back to `"Resume"`, the same word a real resumable
/// position uses) rather than panicking on an unexpected input.
pub fn start_display_word(status: Option<Status>) -> &'static str {
    match status {
        None => "Start",
        Some(Status::Paused) => "Resume",
        Some(Status::Dropped) => "Rewatch?",
        Some(Status::InProgress) => "Resume",
    }
}

/// Series eligible for `start`'s listing/picker: everything *not* currently
/// `in_progress` (design.md §6: "not currently in the automatic in_progress
/// picker").
pub fn eligible_for_start(overviews: &[SeriesOverview]) -> Vec<SeriesOverview> {
    overviews
        .iter()
        .filter(|o| o.status != Some(Status::InProgress))
        .cloned()
        .collect()
}

/// One human-readable line for `start`'s listing/picker, e.g.
/// `"Monster — Resume (ep. 9, 74 files)"` or
/// `"Apothecary Diaries — Start (12 files)"`.
pub fn start_line(overview: &SeriesOverview) -> String {
    let word = start_display_word(overview.status);
    match (overview.status, overview.episode_number) {
        (Some(Status::Paused), Some(ep)) => format!(
            "{} — {} (ep. {ep}, {} files)",
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
            position_seconds: None,
            file_count: 1,
        }
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
    fn paused_display_word_never_contains_rewatch() {
        // The bug CLAUDE.md specifically flags as having been gotten wrong
        // once already: a paused series must never be mislabeled "Rewatch?".
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
        // Gap-audit sweep item: four separate single-state tests could each
        // individually pass while still being wrong relative to each other
        // (e.g. two states accidentally sharing a string). This constructs
        // one series per real state — never-started, paused, dropped,
        // in_progress — in a single test and asserts on all four outputs
        // together, so a regression that makes any two collide is caught
        // here even if each one still "looks right" in isolation.
        let never_started = start_display_word(None);
        let paused = start_display_word(Some(Status::Paused));
        let dropped = start_display_word(Some(Status::Dropped));
        let in_progress = start_display_word(Some(Status::InProgress));

        assert_eq!(never_started, "Start");
        assert_eq!(paused, "Resume");
        assert_eq!(dropped, "Rewatch?");
        // in_progress never legitimately reaches start's listing (filtered
        // out by eligible_for_start), but the function stays total — it
        // falls back to the same word a real resumable position uses.
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
            }],
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
    fn episode_number_falls_back_to_ordinal_when_anitomy_cannot_parse() {
        // A record's episode_number is populated entirely by reconcile (via
        // anitomy::episode_identity's own ordinal fallback) — this proves
        // overview.rs surfaces exactly what's stored, verbatim, without a
        // second normalization pass of its own.
        let record = SeriesRecord {
            root: "~/Anime".to_string(),
            name: "No Numbers".to_string(),
            status: Status::InProgress,
            current_relative_path: "No Numbers/beta.mkv".to_string(),
            position_seconds: 200.0,
            episode_number: Some(2), // ordinal fallback position, not a parsed number
            updated_at: 1,
        };
        let o = overview_for("~/Anime", "No Numbers", Some(&record), 3);
        assert_eq!(o.episode_number, Some(2));
    }
}
