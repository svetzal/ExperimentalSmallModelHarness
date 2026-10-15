//! Word wrapping for the conversation view.
//!
//! Text wraps at word boundaries, measured in terminal cells. The whitespace
//! at a break is dropped, so no row starts with a stray space. A logical
//! line's leading indentation stays on its first row, and its continuation
//! rows hang at that indentation, or after a list marker. A token wider than
//! the row is split after `/`, `-`, `_`, `.` or `,` when that fits it on two
//! rows; otherwise it is shortened in the middle, so its start and end stay
//! visible. Text in the `Evidence` role is a path to copy: it wraps at `/`,
//! or by character as a last resort, but is never shortened.
use super::styled::{Role, Row, Styled};
use unicode_width::UnicodeWidthChar;

type Cell = (char, Role);

const TAB: &str = "    ";
const BREAK_AFTER: [char; 5] = ['/', '-', '_', '.', ','];

/// Wrap styled text into rows of at most `width` cells.
pub(super) fn wrap(text: &Styled, width: usize) -> Vec<Row> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in logical_lines(text) {
        wrap_line(&line, width, &mut rows);
    }
    rows
}

/// The first row of wrapped text, for one-line items such as the status line.
pub(super) fn first_row(text: &Styled, width: usize) -> Row {
    wrap(text, width).swap_remove(0)
}

/// Split at newlines, expand tabs, and drop other control characters.
fn logical_lines(text: &Styled) -> Vec<Vec<Cell>> {
    let mut lines = vec![Vec::new()];
    for (role, run) in &text.runs {
        for ch in run.chars() {
            match ch {
                '\n' => lines.push(Vec::new()),
                '\t' => lines
                    .last_mut()
                    .expect("there is always a line")
                    .extend(TAB.chars().map(|space| (space, *role))),
                ch if ch.is_control() => {}
                ch => lines
                    .last_mut()
                    .expect("there is always a line")
                    .push((ch, *role)),
            }
        }
    }
    lines
}

fn width_of(ch: char) -> usize {
    ch.width().unwrap_or(0)
}

fn cells(text: &[Cell]) -> usize {
    text.iter().map(|(ch, _)| width_of(*ch)).sum()
}

/// A list item's marker and its following space, in cells: `- `, `* `,
/// `+ `, `12. `, `3) `, and the view's own line markers (tool states and
/// collapsed thinking), so their continuation rows align under the text.
fn list_marker(line: &[Cell]) -> usize {
    let text: String = line.iter().take(8).map(|(ch, _)| *ch).collect();
    let mut chars = text.chars();
    if let (Some(first), Some(' ')) = (chars.next(), chars.next())
        && ("-*+•✓✗∴–".contains(first) || ('⠀'..='⣿').contains(&first))
    {
        return width_of(first) + 1;
    }
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    let rest = &text[digits..];
    if digits > 0 && (rest.starts_with(". ") || rest.starts_with(") ")) {
        return digits + 2;
    }
    0
}

/// Words with the whitespace before each. East Asian wide characters, such
/// as CJK and emoji, are words of their own, so text without spaces can
/// still break between them.
fn tokens(line: &[Cell]) -> Vec<(Vec<Cell>, Vec<Cell>)> {
    let mut tokens = Vec::new();
    let mut gap = Vec::new();
    let mut word: Vec<Cell> = Vec::new();
    for &(ch, role) in line {
        if ch == ' ' || ch.is_whitespace() {
            if !word.is_empty() {
                tokens.push((std::mem::take(&mut gap), std::mem::take(&mut word)));
            }
            gap.push((ch, role));
        } else if width_of(ch) == 2 {
            if !word.is_empty() {
                tokens.push((std::mem::take(&mut gap), std::mem::take(&mut word)));
            }
            tokens.push((std::mem::take(&mut gap), vec![(ch, role)]));
        } else {
            word.push((ch, role));
        }
    }
    if !word.is_empty() {
        tokens.push((gap, word));
    }
    tokens
}

fn wrap_line(line: &[Cell], width: usize, rows: &mut Vec<Row>) {
    let indent_len = line.iter().take_while(|(ch, _)| *ch == ' ').count();
    let (indent, rest) = line.split_at(indent_len);
    // Indentation that leaves no room for text is dropped rather than wrapped.
    let indent = if indent_len * 2 > width {
        &[][..]
    } else {
        indent
    };
    let hanging = indent.len() + list_marker(rest);
    let hanging = if hanging * 2 > width { 0 } else { hanging };
    let words = tokens(rest);
    let mut row: Vec<Cell> = indent.to_vec();
    if words.is_empty() {
        rows.push(to_row(&row));
        return;
    }
    let mut used = cells(&row);
    let mut has_word = false;
    for (gap, word) in words {
        let size = cells(&word);
        if has_word && used + cells(&gap) + size <= width {
            row.extend(gap);
            row.extend(word);
            used = cells(&row);
            continue;
        }
        if has_word {
            rows.push(to_row(&row));
            row = vec![(' ', Role::Plain); hanging];
            used = hanging;
        }
        let room = width - used;
        if size <= room {
            row.extend(word);
        } else {
            let mut pieces = split_long(&word, room, width - hanging).into_iter();
            row.extend(pieces.next().unwrap_or_default());
            for piece in pieces {
                rows.push(to_row(&row));
                row = vec![(' ', Role::Plain); hanging];
                row.extend(piece);
            }
        }
        used = cells(&row);
        has_word = true;
    }
    rows.push(to_row(&row));
}

/// Fit a word wider than its row: wrap a copyable path, split once at a
/// punctuation break when two rows hold it, or shorten it in the middle.
fn split_long(word: &[Cell], first: usize, next: usize) -> Vec<Vec<Cell>> {
    if word
        .first()
        .is_some_and(|(_, role)| *role == Role::Evidence)
    {
        return split_copyable(word, first, next);
    }
    if cells(word) <= first + next {
        let mut used = 0;
        let mut best = None;
        for (index, (ch, _)) in word.iter().enumerate() {
            used += width_of(*ch);
            if used > first {
                break;
            }
            if BREAK_AFTER.contains(ch) && cells(&word[index + 1..]) <= next {
                best = Some(index + 1);
            }
        }
        if let Some(at) = best {
            return vec![word[..at].to_vec(), word[at..].to_vec()];
        }
    }
    vec![middle_ellipsis(word, first)]
}

/// Wrap a path at `/` where possible, by character otherwise. Never shortened.
fn split_copyable(word: &[Cell], first: usize, next: usize) -> Vec<Vec<Cell>> {
    let mut pieces = Vec::new();
    let mut rest = word;
    let mut room = first.max(1);
    while cells(rest) > room {
        let mut used = 0;
        let mut fit = 0;
        let mut slash = None;
        for (index, (ch, _)) in rest.iter().enumerate() {
            if used + width_of(*ch) > room {
                break;
            }
            used += width_of(*ch);
            fit = index + 1;
            if *ch == '/' {
                slash = Some(index + 1);
            }
        }
        let at = slash.unwrap_or(fit).max(1);
        pieces.push(rest[..at].to_vec());
        rest = &rest[at..];
        room = next.max(1);
    }
    pieces.push(rest.to_vec());
    pieces
}

/// Keep the start and end of `word` within `room` cells, joined by `…`.
fn middle_ellipsis(word: &[Cell], room: usize) -> Vec<Cell> {
    let role = word.first().map_or(Role::Plain, |(_, role)| *role);
    if room <= 1 {
        return vec![('…', role)];
    }
    let budget = room - 1;
    let mut head = Vec::new();
    let mut used = 0;
    for &(ch, role) in word {
        if used + width_of(ch) > budget.div_ceil(2) {
            break;
        }
        used += width_of(ch);
        head.push((ch, role));
    }
    let mut tail = Vec::new();
    let mut tail_used = 0;
    for &(ch, role) in word.iter().rev() {
        if used + tail_used + width_of(ch) > budget {
            break;
        }
        tail_used += width_of(ch);
        tail.push((ch, role));
    }
    tail.reverse();
    let ellipsis_role = head.last().map_or(role, |(_, role)| *role);
    head.push(('…', ellipsis_role));
    head.extend(tail);
    head
}

fn to_row(cells: &[Cell]) -> Row {
    let mut row = Row::new();
    for &(ch, role) in cells {
        match row.last_mut() {
            Some((last, text)) if *last == role => text.push(ch),
            _ => row.push((role, ch.to_string())),
        }
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::styled::rows_text;

    fn wrapped(text: &str, width: usize) -> Vec<String> {
        rows_text(&wrap(&Styled::default().with(Role::Plain, text), width))
    }

    fn row_cells(row: &str) -> usize {
        row.chars().map(width_of).sum()
    }

    /// Every row fits, and none starts with a space unless it carries the
    /// line's own indentation or its hanging indent.
    fn assert_well_formed(rows: &[String], width: usize, indents: &[usize]) {
        for row in rows {
            assert!(row_cells(row) <= width, "{row:?} is wider than {width}");
            let leading = row.chars().take_while(|ch| *ch == ' ').count();
            assert!(
                leading == 0 || indents.contains(&leading),
                "{row:?} starts with {leading} stray spaces"
            );
        }
    }

    #[test]
    fn prose_breaks_between_words_and_drops_the_space() {
        let rows = wrapped(
            "Found it. The pace is set by the PACE constant in launch.rs.",
            20,
        );
        assert_eq!(
            rows,
            [
                "Found it. The pace",
                "is set by the PACE",
                "constant in",
                "launch.rs."
            ]
        );
        assert_well_formed(&rows, 20, &[]);
    }

    #[test]
    fn words_that_fit_exactly_stay_on_the_row() {
        assert_eq!(wrapped("abcd efgh", 4), ["abcd", "efgh"]);
        assert_eq!(wrapped("ab cd ef", 5), ["ab cd", "ef"]);
    }

    #[test]
    fn spaces_inside_a_row_are_kept_but_never_start_a_row() {
        assert_eq!(wrapped("a  b   c", 20), ["a  b   c"]);
        assert_eq!(wrapped("alpha    beta", 7), ["alpha", "beta"]);
        assert_eq!(
            wrapped("   ", 10),
            ["   "],
            "an indented empty line keeps its indent"
        );
    }

    #[test]
    fn leading_indentation_stays_and_continuation_rows_hang_under_it() {
        let rows = wrapped("    let value = compute_the_answer(first, second);", 24);
        assert_eq!(
            rows,
            [
                // The long call breaks after "_": two rows then hold it whole.
                "    let value =",
                "    compute_the_",
                "    answer(first,",
                "    second);"
            ]
        );
        assert_well_formed(&rows, 24, &[4]);
        // Tabs expand to four spaces.
        assert_eq!(wrapped("\tx", 10), ["    x"]);
    }

    #[test]
    fn list_items_hang_after_their_marker() {
        let rows = wrapped("- Tag the release and build every artifact", 20);
        assert_eq!(
            rows,
            ["- Tag the release", "  and build every", "  artifact"]
        );
        let numbered = wrapped("  12. Smoke test outside the checkout", 18);
        assert_eq!(
            numbered,
            ["  12. Smoke test", "      outside the", "      checkout"]
        );
        assert_well_formed(&numbered, 18, &[2, 6]);
    }

    #[test]
    fn tool_and_thinking_lines_hang_under_their_text() {
        let rows = wrapped("  ✓ shell_command grep -rn animation src/tui", 24);
        assert_eq!(
            rows,
            ["  ✓ shell_command grep", "    -rn animation", "    src/tui"]
        );
        let rows = wrapped("  ⠋ read_file a very long summary here", 20);
        assert_eq!(
            rows,
            ["  ⠋ read_file a very", "    long summary", "    here"]
        );
    }

    #[test]
    fn a_long_word_at_a_row_end_moves_down_whole() {
        assert_eq!(wrapped("see configuration", 12), ["see", "config…ation"]);
        assert_eq!(wrapped("see configuration", 14), ["see", "configuration"]);
    }

    #[test]
    fn urls_wider_than_the_row_are_shortened_in_the_middle() {
        let url = "https://github.com/svetzal/microgentic/releases/download/v0.2.0/microgentic-aarch64.tar.gz";
        let rows = wrapped(&format!("Download {url} now"), 40);
        assert_eq!(rows[0], "Download");
        assert!(
            rows[1].starts_with("https://github.com/") && rows[1].contains('…'),
            "{rows:?}"
        );
        assert!(rows[1].ends_with("aarch64.tar.gz"), "{rows:?}");
        assert_eq!(row_cells(&rows[1]), 40);
        assert_eq!(rows[2], "now");
    }

    #[test]
    fn a_token_that_fits_on_two_rows_breaks_after_punctuation_instead() {
        let rows = wrapped("src/tui/history_rendering_module.rs", 20);
        assert_eq!(rows, ["src/tui/history_", "rendering_module.rs"]);
        // A token that needs three rows is shortened instead.
        let rows = wrapped(
            "src/tui/history_rendering_module_with_a_much_longer_name.rs",
            20,
        );
        assert_eq!(rows, ["src/tui/hi…r_name.rs"]);
        let rows = wrapped("src/tui/history/render.rs", 16);
        assert_eq!(rows, ["src/tui/history/", "render.rs"]);
    }

    #[test]
    fn evidence_paths_wrap_at_slashes_and_are_never_shortened() {
        let path = "~/.local/state/microgentic/runs/run-R1q399/traces/run-20261015T080403Z.jsonl";
        let text = Styled::default().with(Role::Evidence, &format!("Evidence: {path}"));
        let rows = rows_text(&wrap(&text, 30));
        assert_eq!(rows[0], "Evidence:");
        assert_eq!(rows[1..].concat(), path, "the path lost characters");
        assert!(rows.iter().all(|row| row_cells(row) <= 30));
        assert!(rows[1].ends_with('/') && !rows.concat().contains('…'));
        // With no slash to break at, it wraps by character.
        let hash = Styled::default().with(Role::Evidence, &"a".repeat(25));
        assert_eq!(
            rows_text(&wrap(&hash, 10)),
            ["aaaaaaaaaa", "aaaaaaaaaa", "aaaaa"]
        );
    }

    #[test]
    fn wide_characters_count_two_cells_and_can_break_between_them() {
        assert_eq!(wrapped("界a界", 3), ["界a", "界"]);
        assert_eq!(
            wrapped("這是一個很長的句子", 8),
            ["這是一個", "很長的句", "子"]
        );
        assert_eq!(wrapped("ok 🌿 main", 6), ["ok 🌿", "main"]);
    }

    #[test]
    fn styles_survive_a_change_mid_word_and_a_break() {
        let text = Styled::default()
            .with(Role::UserLabel, "You:")
            .with(Role::Plain, " hel")
            .with(Role::Error, "lo world");
        let rows = wrap(&text, 9);
        assert_eq!(rows_text(&rows), ["You:", "hello", "world"]);
        assert_eq!(
            rows[1],
            [
                (Role::Plain, "hel".to_owned()),
                (Role::Error, "lo".to_owned())
            ]
        );
        assert_eq!(rows[2], [(Role::Error, "world".to_owned())]);
    }

    #[test]
    fn tiny_widths_never_panic_and_still_show_something() {
        for width in 0..=3 {
            let rows = wrapped("Hello, wonderful world of terminals", width);
            assert!(!rows.is_empty());
            assert_well_formed(&rows, width.max(1), &[]);
        }
        assert_eq!(wrapped("hello", 1), ["…"]);
        assert_eq!(wrapped("hello", 3), ["h…o"]);
        assert_eq!(
            wrapped("        deep", 3),
            ["d…p"],
            "indentation wider than the row is dropped"
        );
    }

    #[test]
    fn control_characters_are_removed_and_newlines_end_rows() {
        assert_eq!(wrapped("\u{1b}[2J", 10), ["[2J"]);
        assert_eq!(wrapped("abc\n\ndef", 10), ["abc", "", "def"]);
        assert_eq!(wrapped("", 10), [""]);
    }

    #[test]
    fn no_row_starts_with_a_stray_space_in_mixed_text() {
        let text = "Found it — the pace is set in two places:\n\n  1. `PACE` in launch.rs, which divides every hold time by the speed factor\n  2. `LAUNCH_FRAME`, the 33 ms redraw\n\n```rust\n    const PACE: f64 = 1.1;\n```\nSo in short:   it plays   in about 1340 ms.";
        for width in [12, 20, 33, 47, 80] {
            let rows = wrapped(text, width);
            assert_well_formed(&rows, width, &[2, 4, 5]);
        }
    }
}
