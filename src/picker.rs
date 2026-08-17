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

/// Presents `items` for selection. When `is_tty` is true, uses a real
/// arrow-key selector (`dialoguer`); otherwise prints each item (via
/// `label`) with a 1-based index through `print_line` and returns
/// `Listed` without ever blocking on input. `is_tty` is passed in rather
/// than queried internally so this stays fully unit-testable without a
/// real terminal.
pub fn pick<T: Clone>(
    items: &[T],
    label: impl Fn(&T) -> String,
    is_tty: bool,
    prompt: &str,
    mut print_line: impl FnMut(&str),
) -> PickerOutcome<T> {
    if !is_tty {
        for (i, item) in items.iter().enumerate() {
            print_line(&format!("  {}. {}", i + 1, label(item)));
        }
        return PickerOutcome::Listed;
    }

    let labels: Vec<String> = items.iter().map(&label).collect();
    match dialoguer::Select::new()
        .with_prompt(prompt)
        .items(&labels)
        .default(0)
        .interact_opt()
    {
        Ok(Some(index)) => PickerOutcome::Selected(items[index].clone()),
        Ok(None) | Err(_) => PickerOutcome::Cancelled,
    }
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
        );

        assert_eq!(outcome, PickerOutcome::Listed);
        assert!(printed.is_empty());
    }

    #[test]
    fn stdin_is_tty_does_not_panic_in_a_test_harness() {
        // Test harnesses typically run with stdin redirected — this just
        // proves the check itself is safe to call, not any particular
        // result.
        let _ = stdin_is_tty();
    }
}
