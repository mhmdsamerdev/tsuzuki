//! Interactive resume/start picker.
//!
//! Arrow-key selector on a TTY. Without one it never blocks: it lists the
//! choices with an index and reports that nothing was selected, so the
//! caller can exit non-zero.

use std::io::IsTerminal;

use crate::overview;

#[derive(Debug, PartialEq, Eq)]
pub enum PickerOutcome<T> {
    /// The user picked an item.
    Selected(T),
    /// No TTY: every choice was printed with its index and nothing was selected.
    Listed,
    /// The prompt was aborted (e.g. Ctrl-C) without a selection.
    Cancelled,
}

/// True if stdin is a terminal; decides interactive vs. listing mode.
pub fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

/// Presents `items` for selection. With a TTY, passes the labels to `select`
/// (`None` = cancelled). Otherwise prints each item with a 1-based index
/// through `print_line` and returns `Listed`.
/// `is_tty` and `select` are injected so tests can drive both paths.
///
/// When `items` span more than one `route` (callers pass them route-contiguous),
/// the non-TTY listing gets a route header per group and the TTY labels get an
/// aligned `[route]` tag; with one route the output is untouched.
#[allow(clippy::too_many_arguments)]
pub fn pick<T: Clone>(
    items: &[T],
    label: impl Fn(&T) -> String,
    route: impl Fn(&T) -> &str,
    is_tty: bool,
    width: Option<usize>,
    prompt: &str,
    mut print_line: impl FnMut(&str),
    select: impl FnOnce(&str, &[String]) -> Option<usize>,
) -> PickerOutcome<T> {
    let multi_route = overview::is_multi_route(items.iter().map(&route));

    if !is_tty {
        let routes: Vec<&str> = items.iter().map(&route).collect();
        let shown = overview::route_display(&routes);
        let mut current: Option<&str> = None;
        for (i, item) in items.iter().enumerate() {
            if multi_route && current != Some(routes[i]) {
                if current.is_some() {
                    print_line("");
                }
                print_line(&shown[i]);
                current = Some(routes[i]);
            }
            print_line(&format!("  {}. {}", i + 1, label(item)));
        }
        return PickerOutcome::Listed;
    }

    let mut labels: Vec<String> = items.iter().map(&label).collect();
    if multi_route {
        let routes: Vec<&str> = items.iter().map(&route).collect();
        labels = overview::tag_with_routes(&labels, &routes, width);
    } else if let Some(w) = width {
        labels = labels
            .iter()
            .map(|l| overview::truncate_label(l, w))
            .collect();
    }
    match select(prompt, &labels) {
        Some(index) => PickerOutcome::Selected(items[index].clone()),
        None => PickerOutcome::Cancelled,
    }
}

/// Width available to a picker label: the terminal width minus dialoguer's
/// 2-char row prefix and a 1-char margin. `None` if unknown.
pub fn row_width() -> Option<usize> {
    let (_, cols) = console::Term::stdout().size_checked()?;
    Some((cols as usize).saturating_sub(3))
}

/// The production `select`: a `dialoguer` arrow-key selector.
pub fn dialoguer_select(prompt: &str, labels: &[String]) -> Option<usize> {
    dialoguer::Select::new()
        .with_prompt(prompt)
        .items(labels)
        .default(0)
        .interact_opt()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_tty_lists_every_choice_with_a_one_based_index_and_selects_nothing() {
        let items = vec!["Monster".to_string(), "One Piece".to_string()];
        let mut printed = Vec::new();

        let outcome = pick(
            &items,
            |s| s.clone(),
            |_| "~/Anime",
            false,
            None,
            "Pick one",
            |line| printed.push(line.to_string()),
            |_, _| panic!("select must never be called in the non-TTY branch"),
        );

        assert_eq!(outcome, PickerOutcome::Listed);
        assert_eq!(printed, vec!["  1. Monster", "  2. One Piece"]);
    }

    #[test]
    fn non_tty_with_zero_items_prints_nothing_and_still_reports_listed() {
        let items: Vec<String> = Vec::new();
        let mut printed = Vec::new();

        let outcome = pick(
            &items,
            |s| s.clone(),
            |_| "~/Anime",
            false,
            None,
            "Pick one",
            |line| printed.push(line.to_string()),
            |_, _| panic!("select must never be called in the non-TTY branch"),
        );

        assert_eq!(outcome, PickerOutcome::Listed);
        assert!(printed.is_empty());
    }

    #[test]
    fn tty_selection_path_drives_a_fabricated_choice_through_to_selected() {
        // A fake selector stands in for a terminal.
        let items = vec!["Monster".to_string(), "One Piece".to_string()];

        let outcome = pick(
            &items,
            |s| s.clone(),
            |_| "~/Anime",
            true,
            None,
            "Resume which series?",
            |_| panic!("print_line must never be called in the TTY branch"),
            |prompt, labels| {
                assert_eq!(prompt, "Resume which series?");
                assert_eq!(labels, &["Monster".to_string(), "One Piece".to_string()]);
                Some(1)
            },
        );

        assert_eq!(outcome, PickerOutcome::Selected("One Piece".to_string()));
    }

    #[test]
    fn tty_selection_labels_are_built_from_the_label_closure_not_the_raw_items() {
        let items = vec![1, 2, 3];
        let outcome = pick(
            &items,
            |n| format!("item #{n}"),
            |_| "~/Anime",
            true,
            None,
            "Pick",
            |_| {},
            |_, labels| {
                assert_eq!(
                    labels,
                    &[
                        "item #1".to_string(),
                        "item #2".to_string(),
                        "item #3".to_string()
                    ]
                );
                Some(2)
            },
        );
        assert_eq!(outcome, PickerOutcome::Selected(3));
    }

    #[test]
    fn tty_single_route_labels_are_truncated_to_the_width() {
        let items = vec!["abcdefghij".to_string(), "abc".to_string()];
        let outcome = pick(
            &items,
            |s| s.clone(),
            |_| "~/Anime",
            true,
            Some(6),
            "Pick",
            |_| {},
            |_, labels| {
                assert_eq!(labels, &["abcde…".to_string(), "abc".to_string()]);
                Some(0)
            },
        );
        assert_eq!(outcome, PickerOutcome::Selected("abcdefghij".to_string()));
    }

    #[test]
    fn tty_selection_cancelled_reports_cancelled_not_a_default_selection() {
        let items = vec!["Monster".to_string()];
        let outcome = pick(
            &items,
            |s| s.clone(),
            |_| "~/Anime",
            true,
            None,
            "Pick one",
            |_| {},
            |_, _| None,
        );
        assert_eq!(outcome, PickerOutcome::Cancelled);
    }

    #[test]
    fn stdin_is_tty_does_not_panic_in_a_test_harness() {
        // Stdin is usually redirected under a harness; only the call is checked.
        let _ = stdin_is_tty();
    }

    fn route_of(item: &(String, String)) -> &str {
        &item.1
    }

    fn items(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(n, r)| (n.to_string(), r.to_string()))
            .collect()
    }

    fn name_of(item: &(String, String)) -> String {
        item.0.clone()
    }

    #[test]
    fn non_tty_multi_route_prints_group_headers_with_continuous_numbering() {
        let items = items(&[
            ("Monster", "~/Anime"),
            ("Naruto", "~/Anime"),
            ("Frieren", "/mnt/nas/Shows"),
        ]);
        let mut printed = Vec::new();

        let outcome = pick(
            &items,
            name_of,
            route_of,
            false,
            None,
            "Pick",
            |line| printed.push(line.to_string()),
            |_, _| panic!("select must never be called in the non-TTY branch"),
        );

        assert_eq!(outcome, PickerOutcome::Listed);
        assert_eq!(
            printed,
            vec![
                "Anime",
                "  1. Monster",
                "  2. Naruto",
                "",
                "Shows",
                "  3. Frieren",
            ]
        );
    }

    #[test]
    fn non_tty_single_route_has_no_headers() {
        let items = items(&[("Monster", "~/Anime"), ("Naruto", "~/Anime")]);
        let mut printed = Vec::new();

        pick(
            &items,
            name_of,
            route_of,
            false,
            None,
            "Pick",
            |line| printed.push(line.to_string()),
            |_, _| None,
        );

        assert_eq!(printed, vec!["  1. Monster", "  2. Naruto"]);
    }

    #[test]
    fn tty_multi_route_tags_labels_with_aligned_route() {
        let items = items(&[
            ("Monster", "~/Anime"),
            ("Frieren", "/mnt/nas/Shows"),
            ("Nana", "/mnt/nas/Shows"),
        ]);

        let outcome = pick(
            &items,
            name_of,
            route_of,
            true,
            None,
            "Pick",
            |_| panic!("print_line must never be called in the TTY branch"),
            |_, labels| {
                assert_eq!(
                    labels,
                    &[
                        "Monster  [Anime]".to_string(),
                        "Frieren  [Shows]".to_string(),
                        "Nana     [Shows]".to_string(),
                    ]
                );
                Some(1)
            },
        );

        assert_eq!(
            outcome,
            PickerOutcome::Selected(("Frieren".to_string(), "/mnt/nas/Shows".to_string()))
        );
    }

    #[test]
    fn tty_single_route_labels_are_untagged() {
        let items = items(&[("Monster", "~/Anime"), ("Naruto", "~/Anime")]);

        pick(
            &items,
            name_of,
            route_of,
            true,
            None,
            "Pick",
            |_| {},
            |_, labels| {
                assert_eq!(labels, &["Monster".to_string(), "Naruto".to_string()]);
                None
            },
        );
    }

    #[test]
    fn tty_same_name_under_two_routes_selects_the_chosen_route() {
        let items = items(&[("Monster", "~/Anime"), ("Monster", "/mnt/nas/Shows")]);

        let outcome = pick(
            &items,
            name_of,
            route_of,
            true,
            None,
            "Pick",
            |_| {},
            |_, _| Some(1),
        );

        assert_eq!(
            outcome,
            PickerOutcome::Selected(("Monster".to_string(), "/mnt/nas/Shows".to_string()))
        );
    }

    #[test]
    fn non_tty_routes_sharing_a_folder_name_are_headed_by_their_full_root() {
        let items = items(&[("Monster", "~/Anime"), ("Frieren", "/mnt/Anime")]);
        let mut printed = Vec::new();

        pick(
            &items,
            name_of,
            route_of,
            false,
            None,
            "Pick",
            |line| printed.push(line.to_string()),
            |_, _| None,
        );

        assert_eq!(
            printed,
            vec!["~/Anime", "  1. Monster", "", "/mnt/Anime", "  2. Frieren"]
        );
    }
}
