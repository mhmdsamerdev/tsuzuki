//! Interactive resume/start picker.
//!
//! Arrow-key selector on a TTY, drawn on the alternate screen and fully
//! repainted from the top on every key, so a resized or short terminal can
//! never leave stale lines behind. Without a TTY it never blocks: it lists the
//! choices with an index and reports that nothing was selected, so the
//! caller can exit non-zero.

use std::io::IsTerminal;

use console::{Key, Term, measure_text_width, truncate_str};

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

/// One selectable line: its text and, when choices span several roots, a
/// `[route]` tag shown after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub tag: Option<String>,
}

/// True if stdin is a terminal; decides interactive vs. listing mode.
pub fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

/// Presents `items` for selection. With a TTY, passes the rows to `select`
/// (`None` = cancelled). Otherwise prints each item with a 1-based index
/// through `print_line` and returns `Listed`.
/// `is_tty` and `select` are injected so tests can drive both paths.
///
/// When `items` span more than one `route` (callers pass them route-contiguous),
/// the non-TTY listing gets a route header per group and the TTY rows get a
/// `[route]` tag; with one route the output is untouched.
pub fn pick<T: Clone>(
    items: &[T],
    label: impl Fn(&T) -> String,
    route: impl Fn(&T) -> &str,
    is_tty: bool,
    prompt: &str,
    mut print_line: impl FnMut(&str),
    select: impl FnOnce(&str, &[Row]) -> Option<usize>,
) -> PickerOutcome<T> {
    let routes: Vec<&str> = items.iter().map(&route).collect();
    let multi_route = overview::is_multi_route(routes.iter().copied());
    let shown = overview::route_display(&routes);

    if !is_tty {
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

    let rows: Vec<Row> = items
        .iter()
        .enumerate()
        .map(|(i, item)| Row {
            label: label(item),
            tag: multi_route.then(|| format!("[{}]", shown[i])),
        })
        .collect();
    match select(prompt, &rows) {
        Some(index) => PickerOutcome::Selected(items[index].clone()),
        None => PickerOutcome::Cancelled,
    }
}

/// Columns taken by the `> ` marker in front of every row.
const MARKER_WIDTH: usize = 2;
/// Narrowest label column worth keeping; below it the tags are dropped.
const MIN_LABEL_WIDTH: usize = 12;
const HINT: &str = "↑/↓ move · enter select · esc cancel";

/// Rows of the terminal usable for choices: all but the prompt and hint lines.
fn visible_rows(term_rows: usize) -> usize {
    if term_rows >= 3 {
        term_rows - 2
    } else {
        term_rows.max(1)
    }
}

fn fit(s: &str, width: usize) -> String {
    truncate_str(s, width, "…").into_owned()
}

/// The screen for one frame: prompt, the `visible_rows` choices from `top`,
/// then a hint. Pure; every line is at most `term_cols - 1` columns wide so
/// nothing ever wraps. Labels shrink with `…` before the tag column does, and
/// tags are dropped on terminals too narrow to keep a useful label.
fn render(
    rows: &[Row],
    prompt: &str,
    sel: usize,
    top: usize,
    term_rows: usize,
    term_cols: usize,
) -> Vec<String> {
    let max_w = term_cols.saturating_sub(1).max(1);
    let visible = visible_rows(term_rows);
    let end = (top + visible).min(rows.len());

    let mut tag_w = rows
        .iter()
        .filter_map(|r| r.tag.as_deref())
        .map(measure_text_width)
        .max()
        .unwrap_or(0);
    let mut budget = max_w.saturating_sub(MARKER_WIDTH);
    if tag_w > 0 {
        let with_tags = budget.saturating_sub(tag_w + 2);
        if with_tags >= MIN_LABEL_WIDTH {
            budget = with_tags;
        } else {
            tag_w = 0;
        }
    }
    let label_w = rows
        .iter()
        .map(|r| measure_text_width(&r.label))
        .max()
        .unwrap_or(0)
        .min(budget);

    let mut lines = Vec::new();
    if term_rows >= 3 {
        lines.push(fit(prompt, max_w));
    }
    for (i, row) in rows.iter().enumerate().take(end).skip(top) {
        let marker = if i == sel { "> " } else { "  " };
        let label = fit(&row.label, label_w);
        let mut line = format!("{marker}{label}");
        if let (true, Some(tag)) = (tag_w > 0, row.tag.as_deref()) {
            let pad = label_w.saturating_sub(measure_text_width(&label));
            line.push_str(&" ".repeat(pad));
            line.push_str("  ");
            line.push_str(tag);
        }
        lines.push(fit(&line, max_w));
    }
    if term_rows >= 3 {
        let hint = if rows.len() > visible {
            format!("{}/{} · {HINT}", sel + 1, rows.len())
        } else {
            HINT.to_string()
        };
        lines.push(fit(&hint, max_w));
    }
    lines
}

/// First visible row so `sel` is on screen, moving `top` as little as
/// possible. `visible` is at least 1.
fn scroll_top(sel: usize, top: usize, visible: usize, len: usize) -> usize {
    let top = top.min(len.saturating_sub(visible));
    if sel < top {
        sel
    } else if sel >= top + visible {
        sel + 1 - visible
    } else {
        top
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Move(usize),
    Choose,
    Cancel,
    Ignore,
}

/// What `key` does with the selection `sel` of `len` (> 0) choices.
fn apply_key(sel: usize, len: usize, visible: usize, key: &Key) -> Step {
    match key {
        Key::ArrowUp | Key::BackTab | Key::Char('k') => Step::Move((sel + len - 1) % len),
        Key::ArrowDown | Key::Tab | Key::Char('j') => Step::Move((sel + 1) % len),
        Key::Home => Step::Move(0),
        Key::End => Step::Move(len - 1),
        Key::PageUp => Step::Move(sel.saturating_sub(visible)),
        Key::PageDown => Step::Move((sel + visible).min(len - 1)),
        Key::Enter | Key::Char(' ') => Step::Choose,
        Key::Escape | Key::Char('q') | Key::CtrlC => Step::Cancel,
        _ => Step::Ignore,
    }
}

/// Switches to the alternate screen with the cursor hidden and restores both
/// on drop, so every exit path (cancel, error, panic) leaves the terminal sane.
struct AltScreen(Term);

impl AltScreen {
    fn enter(term: &Term) -> std::io::Result<Self> {
        term.write_str("\x1b[?1049h\x1b[?25l")?;
        Ok(Self(term.clone()))
    }
}

impl Drop for AltScreen {
    fn drop(&mut self) {
        let _ = self.0.write_str("\x1b[?25h\x1b[?1049l");
    }
}

/// The production `select`: arrow-key selector repainted in full on each key.
/// Draws on stderr so a piped stdout stays clean.
pub fn interactive_select(prompt: &str, rows: &[Row]) -> Option<usize> {
    let term = Term::stderr();
    if !term.is_term() || rows.is_empty() {
        return None;
    }
    let _screen = AltScreen::enter(&term).ok()?;
    let (mut sel, mut top) = (0, 0);
    loop {
        let (term_rows, term_cols) = term.size();
        let (term_rows, term_cols) = (term_rows as usize, term_cols as usize);
        let visible = visible_rows(term_rows);
        top = scroll_top(sel, top, visible, rows.len());
        let frame = render(rows, prompt, sel, top, term_rows, term_cols).join("\x1b[K\r\n");
        term.write_str(&format!("\x1b[H{frame}\x1b[K\x1b[J")).ok()?;
        // Raw read: Ctrl-C arrives as a key instead of killing us mid-screen.
        match apply_key(sel, rows.len(), visible, &term.read_key_raw().ok()?) {
            Step::Move(to) => sel = to,
            Step::Choose => return Some(sel),
            Step::Cancel => return None,
            Step::Ignore => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(label: &str, tag: Option<&str>) -> Row {
        Row {
            label: label.to_string(),
            tag: tag.map(str::to_string),
        }
    }

    #[test]
    fn non_tty_lists_every_choice_with_a_one_based_index_and_selects_nothing() {
        let items = vec!["Monster".to_string(), "One Piece".to_string()];
        let mut printed = Vec::new();

        let outcome = pick(
            &items,
            |s| s.clone(),
            |_| "~/Anime",
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
            |_| "~/Anime",
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
        // A fake selector stands in for a terminal.
        let items = vec!["Monster".to_string(), "One Piece".to_string()];

        let outcome = pick(
            &items,
            |s| s.clone(),
            |_| "~/Anime",
            true,
            "Resume which series?",
            |_| panic!("print_line must never be called in the TTY branch"),
            |prompt, rows| {
                assert_eq!(prompt, "Resume which series?");
                assert_eq!(rows, &[row("Monster", None), row("One Piece", None)]);
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
            "Pick",
            |_| {},
            |_, rows| {
                assert_eq!(
                    rows,
                    &[
                        row("item #1", None),
                        row("item #2", None),
                        row("item #3", None)
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
        let outcome = pick(
            &items,
            |s| s.clone(),
            |_| "~/Anime",
            true,
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
            "Pick",
            |line| printed.push(line.to_string()),
            |_, _| None,
        );

        assert_eq!(printed, vec!["  1. Monster", "  2. Naruto"]);
    }

    #[test]
    fn tty_multi_route_rows_carry_their_route_tag() {
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
            "Pick",
            |_| panic!("print_line must never be called in the TTY branch"),
            |_, rows| {
                assert_eq!(
                    rows,
                    &[
                        row("Monster", Some("[Anime]")),
                        row("Frieren", Some("[Shows]")),
                        row("Nana", Some("[Shows]")),
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
    fn tty_single_route_rows_are_untagged() {
        let items = items(&[("Monster", "~/Anime"), ("Naruto", "~/Anime")]);

        pick(
            &items,
            name_of,
            route_of,
            true,
            "Pick",
            |_| {},
            |_, rows| {
                assert_eq!(rows, &[row("Monster", None), row("Naruto", None)]);
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
            "Pick",
            |line| printed.push(line.to_string()),
            |_, _| None,
        );

        assert_eq!(
            printed,
            vec!["~/Anime", "  1. Monster", "", "/mnt/Anime", "  2. Frieren"]
        );
    }

    fn tagged(rows: &[(&str, &str)]) -> Vec<Row> {
        rows.iter().map(|(l, t)| row(l, Some(t))).collect()
    }

    fn user_list() -> Vec<Row> {
        tagged(&[
            ("Liar Game — Start (26 files)", "[anime]"),
            ("Re:Zero — Start (73 files)", "[anime]"),
            ("Fuufu Ijou, Koibito Miman — Start (12 files)", "[anime]"),
            ("Peaky Blinders S01 (2013) — Start (6 files)", "[tv-series]"),
            (
                "SHIMONETA: A Boring World Where the Concept of Dirty Jokes Doesn't Exist — Resume (ep. 2, ani-cli)",
                "[ani-cli]",
            ),
            ("Undead Unluck — Resume (ep. 3, ani-cli)", "[ani-cli]"),
        ])
    }

    #[test]
    fn render_wide_terminal_aligns_tags_right_after_the_longest_label() {
        let rows = tagged(&[("Monster", "[Anime]"), ("Frieren — Start", "[tv]")]);
        assert_eq!(
            render(&rows, "Pick?", 1, 0, 10, 80),
            vec![
                "Pick?",
                "  Monster          [Anime]",
                "> Frieren — Start  [tv]",
                "↑/↓ move · enter select · esc cancel",
            ]
        );
    }

    #[test]
    fn render_narrow_terminal_cuts_labels_but_never_the_tag() {
        let rows = tagged(&[("abcdefghijklmnopqrstuvwxyz", "[anime]"), ("short", "[tv]")]);
        // 30 cols: 29 usable - 2 marker - 9 tag column = 18 label columns.
        assert_eq!(
            render(&rows, "P", 0, 0, 10, 30),
            vec![
                "P",
                "> abcdefghijklmnopq…  [anime]",
                "  short               [tv]",
                "↑/↓ move · enter select · es…",
            ]
        );
    }

    #[test]
    fn render_tiny_terminal_drops_tags_instead_of_starving_the_label() {
        let rows = tagged(&[("abcdefghijklmnopqrstuvwxyz", "[anime]"), ("short", "[tv]")]);
        // 20 cols: tags would leave only 8 label columns (< 12), so they go.
        assert_eq!(
            render(&rows, "P", 1, 0, 10, 20)[1..3],
            ["  abcdefghijklmnop…", "> short"]
        );
    }

    #[test]
    fn render_label_width_boundary_fits_exactly_then_cuts_one_over() {
        // 12 label columns + marker 2 + tag col 9 = 23 usable, so 24 cols.
        let fits = tagged(&[("123456789012", "[anime]")]);
        assert_eq!(
            render(&fits, "P", 0, 0, 5, 24)[1],
            "> 123456789012  [anime]"
        );
        let over = tagged(&[("1234567890123", "[anime]")]);
        assert_eq!(
            render(&over, "P", 0, 0, 5, 24)[1],
            "> 12345678901…  [anime]"
        );
    }

    #[test]
    fn render_pads_double_width_labels_by_display_width() {
        let rows = tagged(&[("進撃の巨人", "[anime]"), ("ab", "[tv]")]);
        assert_eq!(
            render(&rows, "P", 0, 0, 5, 60)[1..3],
            ["> 進撃の巨人  [anime]", "  ab          [tv]"]
        );
    }

    #[test]
    fn render_never_exceeds_the_terminal_width_or_height() {
        let rows = user_list();
        for cols in 1..130 {
            for term_rows in 1..12 {
                let lines = render(&rows, "Start which one?", 2, 0, term_rows, cols);
                assert!(
                    lines.len() <= term_rows.max(1),
                    "{term_rows} rows: {lines:?}"
                );
                for line in &lines {
                    assert!(
                        measure_text_width(line) < cols.max(2),
                        "{cols} cols: {line:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn render_marks_only_the_selected_row_and_shows_the_window_from_top() {
        let rows = tagged(&[("a", "[x]"), ("b", "[x]"), ("c", "[x]"), ("d", "[x]")]);
        let lines = render(&rows, "P", 2, 1, 5, 60);
        assert_eq!(
            lines,
            vec![
                "P",
                "  b  [x]",
                "> c  [x]",
                "  d  [x]",
                "3/4 · ↑/↓ move · enter select · esc cancel",
            ]
        );
    }

    #[test]
    fn render_without_tags_uses_the_full_width_for_labels() {
        let rows = vec![row("1234567890123456", None)];
        // 20 cols: 19 usable - 2 marker = 17, so 16 chars fit untouched.
        assert_eq!(render(&rows, "P", 0, 0, 5, 20)[1], "> 1234567890123456");
        assert_eq!(render(&rows, "P", 0, 0, 5, 18)[1], "> 12345678901234…");
    }

    #[test]
    fn render_shows_the_position_only_when_the_list_is_clipped() {
        let rows = vec![row("a", None), row("b", None), row("c", None)];
        // 5 rows leave 3 for choices: all fit, no position.
        assert_eq!(render(&rows, "P", 0, 0, 5, 60)[4], HINT);
        // 4 rows leave 2: clipped.
        assert_eq!(render(&rows, "P", 0, 0, 4, 60)[3], format!("1/3 · {HINT}"));
    }

    #[test]
    fn render_without_tags_does_not_pad() {
        let rows = vec![row("Monster", None), row("Nana", None)];
        assert_eq!(
            render(&rows, "P", 0, 0, 6, 40)[1..3],
            ["> Monster", "  Nana"]
        );
    }

    #[test]
    fn scroll_top_keeps_the_selection_visible_for_every_position_and_window() {
        for len in 1..12 {
            for visible in 1..8 {
                for sel in 0..len {
                    for top in 0..14 {
                        let t = scroll_top(sel, top, visible, len);
                        assert!(t <= sel && sel < t + visible, "{len} {visible} {sel} {top}");
                        assert!(
                            t + visible <= len.max(visible),
                            "{len} {visible} {sel} {top}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn scroll_top_moves_only_as_far_as_needed() {
        assert_eq!(scroll_top(4, 2, 3, 10), 2);
        assert_eq!(scroll_top(5, 2, 3, 10), 3);
        assert_eq!(scroll_top(1, 2, 3, 10), 1);
        assert_eq!(scroll_top(0, 7, 3, 10), 0);
    }

    #[test]
    fn apply_key_maps_every_key_to_its_step() {
        assert_eq!(apply_key(0, 4, 3, &Key::ArrowUp), Step::Move(3));
        assert_eq!(apply_key(2, 4, 3, &Key::Char('k')), Step::Move(1));
        assert_eq!(apply_key(2, 4, 3, &Key::BackTab), Step::Move(1));
        assert_eq!(apply_key(3, 4, 3, &Key::ArrowDown), Step::Move(0));
        assert_eq!(apply_key(1, 4, 3, &Key::Char('j')), Step::Move(2));
        assert_eq!(apply_key(1, 4, 3, &Key::Tab), Step::Move(2));
        assert_eq!(apply_key(2, 9, 3, &Key::Home), Step::Move(0));
        assert_eq!(apply_key(2, 9, 3, &Key::End), Step::Move(8));
        assert_eq!(apply_key(5, 9, 3, &Key::PageUp), Step::Move(2));
        assert_eq!(apply_key(1, 9, 3, &Key::PageUp), Step::Move(0));
        assert_eq!(apply_key(5, 9, 3, &Key::PageDown), Step::Move(8));
        assert_eq!(apply_key(7, 9, 3, &Key::PageDown), Step::Move(8));
        assert_eq!(apply_key(1, 9, 3, &Key::PageDown), Step::Move(4));
        assert_eq!(apply_key(0, 1, 3, &Key::ArrowDown), Step::Move(0));
        assert_eq!(apply_key(0, 4, 3, &Key::Enter), Step::Choose);
        assert_eq!(apply_key(0, 4, 3, &Key::Char(' ')), Step::Choose);
        assert_eq!(apply_key(0, 4, 3, &Key::Escape), Step::Cancel);
        assert_eq!(apply_key(0, 4, 3, &Key::Char('q')), Step::Cancel);
        assert_eq!(apply_key(0, 4, 3, &Key::CtrlC), Step::Cancel);
        assert_eq!(apply_key(0, 4, 3, &Key::Char('x')), Step::Ignore);
        assert_eq!(apply_key(0, 4, 3, &Key::ArrowLeft), Step::Ignore);
    }
}
