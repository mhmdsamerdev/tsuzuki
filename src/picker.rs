//! The interactive resume/start picker (CLAUDE.md's "Series selection:
//! interactive by default, explicit when scripted").
//!
//! A real arrow-key selector when a TTY is available; in a non-TTY context
//! (piped/scripted invocation), never hangs waiting for input — instead
//! lists the choices with an index and reports that no action was taken, so
//! the caller can exit non-zero (CLAUDE.md's "informed but did not complete
//! the action" precedent).

use std::io::IsTerminal;

#[derive(Debug, PartialEq, Eq)]
pub enum PickerOutcome<T> {
    /// The user picked (or there was only one choice worth auto-selecting —
    /// callers handle the true 0/1/many dispatch themselves; this type only
    /// covers "prompted and got an answer" vs. "could not prompt").
    Selected(T),
    /// No TTY was available — every choice was printed with its index and
    /// nothing was selected or acted on.
    Listed,
    /// A TTY was available but the prompt was aborted (e.g. Ctrl-C) without
    /// a selection.
    Cancelled,
}

/// True if stdin is a real terminal — the one signal this module uses to
/// decide interactive vs. listing mode. Kept as its own function (rather
/// than inlined at call sites) so production code has exactly one place
/// that makes this check, matching CLAUDE.md's "one call site" convention
/// used elsewhere (e.g. `mpv.rs`'s OS-candidate dispatch).
pub fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

/// Presents `items` for selection. When `is_tty` is true, calls `select`
/// with the rendered labels and returns whichever index it picks (`None` =
/// cancelled); otherwise prints each item (via `label`) with a 1-based
/// index through `print_line` and returns `Listed` without ever blocking on
/// input. Both `is_tty` and `select` are passed in rather than the real TTY
/// check / `dialoguer` call being hardcoded — this is what makes the actual
/// selection path (not just the non-TTY fallback) genuinely unit-testable:
/// a test can inject a fake `select` and drive a fabricated choice all the
/// way through to `PickerOutcome::Selected`, without a real terminal.
pub fn pick<T: Clone>(
    items: &[T],
    label: impl Fn(&T) -> String,
    is_tty: bool,
    prompt: &str,
    mut print_line: impl FnMut(&str),
    select: impl FnOnce(&str, &[String]) -> Option<usize>,
) -> PickerOutcome<T> {
    if !is_tty {
        for (i, item) in items.iter().enumerate() {
            print_line(&format!("  {}. {}", i + 1, label(item)));
        }
        return PickerOutcome::Listed;
    }

    let labels: Vec<String> = items.iter().map(&label).collect();
    match select(prompt, &labels) {
        Some(index) => PickerOutcome::Selected(items[index].clone()),
        None => PickerOutcome::Cancelled,
    }
}

/// The real, production `select` implementation: a `dialoguer` arrow-key
/// selector. This is the *only* place `dialoguer` is touched in this
/// module — `pick` itself never references it — so production code passes
/// this function pointer while tests pass a fake closure instead.
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
            false,
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
            false,
            "Pick one",
            |line| printed.push(line.to_string()),
            |_, _| panic!("select must never be called in the non-TTY branch"),
        );

        assert_eq!(outcome, PickerOutcome::Listed);
        assert!(printed.is_empty());
    }

    #[test]
    fn tty_selection_path_drives_a_fabricated_choice_through_to_selected() {
        // Sweep finding: the TTY branch used to call `dialoguer::Select`
        // directly, with no way to exercise it in a test at all. `select`
        // being injected is what makes this possible: a fake selector
        // stands in for a real terminal, proving `pick`'s own selection
        // logic (labels built correctly, chosen index mapped back to the
        // right item) independent of `dialoguer` or a real TTY.
        let items = vec!["Monster".to_string(), "One Piece".to_string()];

        let outcome = pick(
            &items,
            |s| s.clone(),
            true,
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
            true,
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
    fn tty_selection_cancelled_reports_cancelled_not_a_default_selection() {
        let items = vec!["Monster".to_string()];
        let outcome = pick(&items, |s| s.clone(), true, "Pick one", |_| {}, |_, _| None);
        assert_eq!(outcome, PickerOutcome::Cancelled);
    }

    #[test]
    fn stdin_is_tty_does_not_panic_in_a_test_harness() {
        // Test harnesses typically run with stdin redirected — this just
        // proves the check itself is safe to call, not any particular
        // result.
        let _ = stdin_is_tty();
    }
}
