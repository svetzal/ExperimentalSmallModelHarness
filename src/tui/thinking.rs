//! The model's thinking: streamed live, then collapsed.
//!
//! While the model thinks, the view shows a rolling tail of the thinking in a
//! dim, italic style, so the user can follow along without the screen
//! scrolling away. When thinking leads to tool calls it collapses to its last
//! line above them; when it leads to a reply it is removed, because the reply
//! speaks for itself.
use super::styled::{Role, Styled};
use super::wrap::wrap;
use unicode_width::UnicodeWidthChar;

/// Wrapped rows of live thinking kept on screen.
const LIVE_ROWS: usize = 6;
const MARKER: &str = "∴";
const INDENT: &str = "  ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ThinkingState {
    Live,
    /// Reduced to its last line. `stopped` when the run ended mid-thought.
    Collapsed {
        stopped: bool,
    },
}

/// What followed the thinking, which decides what remains of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ending {
    /// Tool calls: keep the last line above them.
    Tools,
    /// Visible reply text: remove the thinking.
    Reply,
    /// The model turn or exchange ended with neither: keep the last line.
    TurnEnded,
    /// The run was cancelled or failed: keep the last line, marked stopped.
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Thinking {
    pub(super) text: String,
    pub(super) state: ThinkingState,
}

impl Thinking {
    pub(super) fn live(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            state: ThinkingState::Live,
        }
    }

    /// The state after `ending`, or `None` when nothing should remain.
    pub(super) fn end(&self, ending: Ending) -> Option<ThinkingState> {
        if self.text.trim().is_empty() {
            return None;
        }
        match ending {
            Ending::Reply => None,
            Ending::Tools | Ending::TurnEnded => Some(ThinkingState::Collapsed { stopped: false }),
            Ending::Stopped => Some(ThinkingState::Collapsed { stopped: true }),
        }
    }

    /// Append the block's rows, each ending in a newline, for `width` cells.
    pub(super) fn render(&self, width: usize, out: &mut Styled) {
        let inner = width.saturating_sub(INDENT.len()).max(1);
        match self.state {
            ThinkingState::Live => {
                let text = self.text.trim();
                if text.is_empty() {
                    return;
                }
                let rows = wrap(&Styled::default().with(Role::Thinking, text), inner);
                let hidden = rows.len().saturating_sub(LIVE_ROWS);
                if hidden > 0 {
                    out.push(Role::Thinking, &format!("{INDENT}…\n"));
                }
                for row in &rows[hidden..] {
                    let line: String = row.iter().map(|(_, text)| text.as_str()).collect();
                    out.push(Role::Thinking, &format!("{INDENT}{line}\n"));
                }
            }
            ThinkingState::Collapsed { stopped } => {
                let suffix = if stopped { " · stopped" } else { "" };
                let room =
                    inner.saturating_sub(MARKER.chars().count() + 1 + suffix.chars().count());
                if let Some(line) = last_line(&self.text, room) {
                    out.push(
                        Role::Thinking,
                        &format!("{INDENT}{MARKER} {line}{suffix}\n"),
                    );
                }
            }
        }
    }
}

/// The last non-empty line of `text`, trimmed and cut with `…` to fit
/// `max_cells`. `None` when the text is only whitespace.
pub(super) fn last_line(text: &str, max_cells: usize) -> Option<String> {
    let line = text.lines().map(str::trim).rfind(|line| !line.is_empty())?;
    let cells: usize = line.chars().map(|ch| ch.width().unwrap_or(0)).sum();
    if cells <= max_cells {
        return Some(line.to_owned());
    }
    let mut kept = String::new();
    let mut used = 1;
    for ch in line.chars() {
        let size = ch.width().unwrap_or(0);
        if used + size > max_cells {
            break;
        }
        used += size;
        kept.push(ch);
    }
    Some(format!("{}…", kept.trim_end()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(thinking: &Thinking, width: usize) -> String {
        let mut out = Styled::default();
        thinking.render(width, &mut out);
        assert!(out.runs.iter().all(|(role, _)| *role == Role::Thinking));
        out.runs.iter().map(|(_, text)| text.as_str()).collect()
    }

    #[test]
    fn live_thinking_shows_a_rolling_tail_of_six_rows() {
        let short = Thinking::live("\nThe user wants the pacing constant.");
        assert_eq!(
            rendered(&short, 80),
            "  The user wants the pacing constant.\n"
        );
        let long = Thinking::live(
            &(1..=9)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assert_eq!(
            rendered(&long, 80),
            "  …\n  line 4\n  line 5\n  line 6\n  line 7\n  line 8\n  line 9\n"
        );
        // Wrapping counts toward the six rows.
        let wide = Thinking::live(&"word ".repeat(40));
        let shown = rendered(&wide, 22);
        assert_eq!(shown.lines().count(), 7);
        assert!(shown.starts_with("  …\n"));
        assert_eq!(rendered(&Thinking::live(" \n\t "), 80), "");
    }

    #[test]
    fn the_ending_decides_what_remains() {
        let thinking = Thinking::live("Plan.\nI'll search first.");
        assert_eq!(thinking.end(Ending::Reply), None);
        assert_eq!(
            thinking.end(Ending::Tools),
            Some(ThinkingState::Collapsed { stopped: false })
        );
        assert_eq!(
            thinking.end(Ending::TurnEnded),
            Some(ThinkingState::Collapsed { stopped: false })
        );
        assert_eq!(
            thinking.end(Ending::Stopped),
            Some(ThinkingState::Collapsed { stopped: true })
        );
        assert_eq!(
            Thinking::live("\n  \n").end(Ending::Tools),
            None,
            "blank thinking leaves nothing"
        );
    }

    #[test]
    fn collapsed_thinking_is_one_marked_line() {
        let mut thinking = Thinking::live("Plan.\nI'll search for the pacing constant first.\n\n");
        thinking.state = ThinkingState::Collapsed { stopped: false };
        assert_eq!(
            rendered(&thinking, 80),
            "  ∴ I'll search for the pacing constant first.\n"
        );
        thinking.state = ThinkingState::Collapsed { stopped: true };
        assert_eq!(
            rendered(&thinking, 80),
            "  ∴ I'll search for the pacing constant first. · stopped\n"
        );
        thinking.state = ThinkingState::Collapsed { stopped: false };
        let narrow = rendered(&thinking, 24);
        assert_eq!(narrow, "  ∴ I'll search for the…\n");
        assert!(narrow.trim_end().chars().count() <= 24);
    }

    #[test]
    fn the_last_line_skips_blank_lines_and_fits_its_room() {
        assert_eq!(
            last_line("first\nsecond\n\n  \n", 80).as_deref(),
            Some("second")
        );
        assert_eq!(
            last_line("  padded line  ", 80).as_deref(),
            Some("padded line")
        );
        assert_eq!(last_line(" \n\t\n", 80), None);
        let long = last_line(&format!("start {}", "x".repeat(200)), 20).unwrap();
        assert_eq!(long.chars().count(), 20);
        assert!(long.ends_with('…'));
    }
}
