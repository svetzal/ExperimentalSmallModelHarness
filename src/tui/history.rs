//! Conversation history shown in the TUI.
//!
//! History is a list of entries: styled text, and tool-call lines that change
//! state in place. Vertical rhythm is one blank line between exchanges and
//! commands, with replies, tool lines, and footers tight inside an exchange.
//! Whitespace-only reply content never opens blank rows: it is held until
//! visible text follows it in the same reply segment, and dropped otherwise.
use super::styled::{Role, Styled};
use super::thinking::{Ending, Thinking, ThinkingState};
use super::tool_lines::{CallState, ToolLine};

#[derive(Debug)]
enum Entry {
    Text(Styled),
    Tool(ToolLine),
    Thinking(Thinking),
}

/// Conversation history with a revision that changes on every edit, so the
/// redraw check never needs to compare or clone the whole history.
#[derive(Default)]
pub(super) struct History {
    entries: Vec<Entry>,
    pub(super) revision: usize,
    /// Reply whitespace waiting for visible text to follow it.
    held: String,
    /// Whether the current reply segment has shown any visible text.
    reply_visible: bool,
}

impl History {
    pub(super) fn push(&mut self, role: Role, text: &str) {
        match self.entries.last_mut() {
            Some(Entry::Text(styled)) => styled.push(role, text),
            _ => self
                .entries
                .push(Entry::Text(Styled::default().with(role, text))),
        }
        self.revision += 1;
    }

    pub(super) fn reset(&mut self) {
        self.entries.clear();
        self.revision += 1;
        self.push(Role::Muted, WELCOME);
    }

    fn at_line_start(&self) -> bool {
        match self.entries.last() {
            None | Some(Entry::Tool(_) | Entry::Thinking(_)) => true,
            Some(Entry::Text(styled)) => styled
                .runs
                .last()
                .is_none_or(|(_, text)| text.ends_with('\n')),
        }
    }

    /// Continue on a fresh line within the current exchange.
    pub(super) fn start_line(&mut self) {
        if !self.at_line_start() {
            self.push(Role::Plain, "\n");
        }
    }

    fn ends_with_blank_row(&self) -> bool {
        match self.entries.last() {
            Some(Entry::Text(styled)) => {
                let tail: String = styled
                    .runs
                    .iter()
                    .rev()
                    .take(2)
                    .map(|(_, text)| text.as_str())
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                tail.ends_with("\n\n")
            }
            _ => false,
        }
    }

    /// Start an exchange or command output, one blank line below the last.
    /// There is never more than one blank row in a row.
    pub(super) fn start_block(&mut self) {
        self.start_line();
        if !self.entries.is_empty() && !self.ends_with_blank_row() {
            self.push(Role::Plain, "\n");
        }
    }

    /// Begin a reply segment: a model turn's text, or text after a tool block.
    pub(super) fn begin_reply(&mut self) {
        self.held.clear();
        self.reply_visible = false;
    }

    /// Add streamed reply text. Leading whitespace in a segment is dropped,
    /// whitespace is held until visible text follows it, and runs of blank
    /// lines shrink to one. Whitespace inside visible text, such as code
    /// indentation, is kept.
    pub(super) fn push_reply(&mut self, text: &str) {
        self.held.push_str(text);
        let Some((last, ch)) = self
            .held
            .char_indices()
            .rev()
            .find(|(_, ch)| !ch.is_whitespace())
        else {
            return;
        };
        let visible: String = self.held.drain(..last + ch.len_utf8()).collect();
        // Visible reply text means the thinking before it is no longer needed.
        self.end_thinking(Ending::Reply);
        let visible = if self.reply_visible {
            visible.as_str()
        } else {
            self.reply_visible = true;
            visible.trim_start()
        };
        self.push(Role::Plain, &clamp_blank_lines(visible));
    }

    /// Add a running call, or refresh one already shown with the same ID.
    pub(super) fn start_tool(&mut self, line: ToolLine) {
        // A tool block ends the reply segment; its held whitespace is dropped.
        self.begin_reply();
        self.end_thinking(Ending::Tools);
        let existing = self.entries.iter_mut().rev().find_map(|entry| match entry {
            Entry::Tool(shown) if !line.call_id.is_empty() && shown.call_id == line.call_id => {
                Some(shown)
            }
            _ => None,
        });
        match existing {
            Some(shown) => *shown = line,
            None => {
                self.start_line();
                self.entries.push(Entry::Tool(line));
            }
        }
        self.revision += 1;
    }

    /// Resolve the matching running call in place. Calls may finish in any
    /// order; a report with no matching call is ignored.
    pub(super) fn finish_tool(&mut self, call_id: &str, name: &str, ok: bool) {
        let running = self.entries.iter_mut().find_map(|entry| match entry {
            Entry::Tool(line) if line.answers(call_id, name) => Some(line),
            _ => None,
        });
        if let Some(line) = running {
            line.state = if ok {
                CallState::Succeeded
            } else {
                CallState::Failed
            };
            self.revision += 1;
        }
    }

    /// Stream thinking text into the live thinking block, starting one if
    /// none is open.
    pub(super) fn think(&mut self, text: &str) {
        match self.entries.last_mut() {
            Some(Entry::Thinking(thinking)) if thinking.state == ThinkingState::Live => {
                thinking.text.push_str(text);
            }
            _ => {
                self.begin_reply();
                self.entries.push(Entry::Thinking(Thinking::live(text)));
            }
        }
        self.revision += 1;
    }

    /// Collapse or remove the live thinking block according to what followed.
    pub(super) fn end_thinking(&mut self, ending: Ending) {
        let Some(index) = self.entries.iter().rposition(
            |entry| matches!(entry, Entry::Thinking(thinking) if thinking.state == ThinkingState::Live),
        ) else {
            return;
        };
        let Entry::Thinking(thinking) = &mut self.entries[index] else {
            return;
        };
        match thinking.end(ending) {
            Some(state) => thinking.state = state,
            None => {
                self.entries.remove(index);
            }
        }
        self.revision += 1;
    }

    /// Mark calls that never reported a result, so nothing spins after a run.
    pub(super) fn stop_running(&mut self) {
        for entry in &mut self.entries {
            if let Entry::Tool(line) = entry
                && line.state == CallState::Running
            {
                line.state = CallState::Stopped;
                self.revision += 1;
            }
        }
    }

    /// The name of the oldest call still running, which is the one executing.
    pub(super) fn oldest_running(&self) -> Option<&str> {
        self.entries.iter().find_map(|entry| match entry {
            Entry::Tool(line) if line.state == CallState::Running => Some(line.name.as_str()),
            _ => None,
        })
    }

    pub(super) fn has_running(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry, Entry::Tool(line) if line.state == CallState::Running))
    }

    /// The whole history as styled text for `width` cells, with spinners at
    /// `frame`. Tool and thinking lines always start on a fresh line.
    pub(super) fn render(&self, frame: usize, width: usize) -> Styled {
        let mut out = Styled::default();
        for entry in &self.entries {
            if !matches!(entry, Entry::Text(_))
                && out
                    .runs
                    .last()
                    .is_some_and(|(_, text)| !text.ends_with('\n'))
            {
                out.push(Role::Plain, "\n");
            }
            match entry {
                Entry::Text(styled) => {
                    for (role, text) in &styled.runs {
                        out.push(*role, text);
                    }
                }
                Entry::Tool(line) => line.render(frame, &mut out),
                Entry::Thinking(thinking) => thinking.render(width, &mut out),
            }
        }
        out
    }
}

/// Shrink every run of blank lines to a single blank line, keeping the
/// indentation that follows the last newline in the run.
fn clamp_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            run.push(ch);
            continue;
        }
        out.push_str(&shrink_run(&run));
        run.clear();
        out.push(ch);
    }
    out.push_str(&shrink_run(&run));
    out
}

fn shrink_run(run: &str) -> String {
    if run.matches('\n').count() <= 2 {
        return run.to_owned();
    }
    let indent = &run[run.rfind('\n').map_or(0, |at| at + 1)..];
    format!("\n\n{indent}")
}

pub(super) const WELCOME: &str = "Talk to Microgentic. Ask for work when you're ready.\nWhen requested, Microgentic can edit files and run project commands.\nConfigure defaults with microgentic setup outside this view.\nType /help for commands.\n";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(history: &History) -> String {
        history
            .render(0, 200)
            .runs
            .iter()
            .map(|(_, text)| text.as_str())
            .collect()
    }

    fn call(id: &str, name: &str, path: &str) -> ToolLine {
        ToolLine::started(id, name, &json!({ "path": path }))
    }

    #[test]
    fn styled_text_extends_the_last_run_for_streamed_content() {
        let mut history = History::default();
        history.push(Role::AgentLabel, "Microgentic:");
        history.push(Role::Plain, " Hel");
        history.push(Role::Plain, "lo");
        history.push(Role::Plain, "");
        assert_eq!(
            history.render(0, 200).runs,
            [
                (Role::AgentLabel, "Microgentic:".to_owned()),
                (Role::Plain, " Hello".to_owned())
            ]
        );
        // Every edit changes the redraw signature, even one that adds nothing visible.
        assert_eq!(history.revision, 4);
    }

    fn think(history: &mut History, chunks: &[&str]) {
        for chunk in chunks {
            history.think(chunk);
        }
    }

    #[test]
    fn thinking_before_tools_collapses_to_its_last_line_above_them() {
        let mut history = History::default();
        history.push(Role::AgentLabel, "Microgentic: ");
        think(
            &mut history,
            &["\nThe user wants", " the pacing.\nI'll search", " first.\n"],
        );
        assert_eq!(
            text(&history),
            "Microgentic: \n  The user wants the pacing.\n  I'll search first.\n"
        );
        history.push_reply("\n\n");
        history.start_tool(call("c1", "shell_command", "x"));
        history.finish_tool("c1", "shell_command", true);
        think(&mut history, &["The constant is likely in launch.rs."]);
        history.start_tool(call("c2", "read_file", "src/tui/launch.rs"));
        history.finish_tool("c2", "read_file", true);
        think(&mut history, &["I have what I need."]);
        history.push_reply("\n\nThe pace is set by `PACE`.");
        assert_eq!(
            text(&history),
            "Microgentic: \n  ∴ I'll search first.\n  ✓ shell_command\n  ∴ The constant is likely in launch.rs.\n  ✓ read_file src/tui/launch.rs\nThe pace is set by `PACE`."
        );
    }

    #[test]
    fn thinking_before_a_reply_is_removed() {
        let mut history = History::default();
        history.push(Role::AgentLabel, "Microgentic: ");
        think(&mut history, &["Just a greeting."]);
        history.push_reply("Hello!");
        assert_eq!(text(&history), "Microgentic: Hello!");
    }

    #[test]
    fn a_stopped_run_never_leaves_thinking_expanded() {
        let mut history = History::default();
        think(&mut history, &["One.\nTwo, still going"]);
        history.end_thinking(Ending::Stopped);
        assert_eq!(text(&history), "  ∴ Two, still going · stopped\n");
        // Ending again changes nothing, and blank thinking leaves no trace.
        history.end_thinking(Ending::Reply);
        think(&mut history, &["\n \n"]);
        history.end_thinking(Ending::Tools);
        assert_eq!(text(&history), "  ∴ Two, still going · stopped\n");
    }

    #[test]
    fn whitespace_only_content_never_opens_blank_rows() {
        let mut history = History::default();
        history.begin_reply();
        for chunk in ["\n\n", "\n", "\n", "   ", "\t"] {
            history.push_reply(chunk);
        }
        assert_eq!(text(&history), "");
        history.start_tool(call("c1", "read_file", "a.rs"));
        assert_eq!(text(&history), "  ⠋ read_file a.rs\n");
    }

    #[test]
    fn held_whitespace_appears_only_before_following_text() {
        let mut history = History::default();
        history.begin_reply();
        for chunk in [
            "\n\nFirst",
            " ",
            "line.\n",
            "\n\n\n",
            "Second\n",
            "  indented",
        ] {
            history.push_reply(chunk);
        }
        assert_eq!(text(&history), "First line.\n\nSecond\n  indented");
        // Trailing whitespace before a tool block is dropped.
        history.push_reply("\n\n");
        history.start_tool(call("c1", "read_file", "a.rs"));
        assert_eq!(
            text(&history),
            "First line.\n\nSecond\n  indented\n  ⠋ read_file a.rs\n"
        );
    }

    #[test]
    fn code_blocks_keep_their_inner_whitespace() {
        let mut history = History::default();
        history.begin_reply();
        let code = "```rust\nfn main() {\n    let x = 1;\n\n    println!(\"{x}\");\n}\n```";
        for chunk in code.split_inclusive(' ') {
            history.push_reply(chunk);
        }
        assert_eq!(text(&history), code);
        assert_eq!(clamp_blank_lines("a\n\n\n\n    b"), "a\n\n    b");
        assert_eq!(clamp_blank_lines("a\n \n\t\nb"), "a\n\nb");
    }

    #[test]
    fn blocks_never_stack_two_blank_rows() {
        let mut history = History::default();
        history.push(Role::Plain, "text\n\n");
        history.start_block();
        history.push(Role::Plain, "next\n");
        assert_eq!(text(&history), "text\n\nnext\n");
    }

    #[test]
    fn exchanges_are_separated_by_one_blank_line_and_tools_sit_tight() {
        let mut history = History::default();
        history.reset();
        history.start_block();
        history.push(Role::Plain, "You: hi\n");
        history.push(Role::Plain, "Microgentic: Looking.");
        history.start_tool(call("c1", "read_file", "a.rs"));
        history.finish_tool("c1", "read_file", true);
        history.push(Role::Plain, "Done.");
        history.start_line();
        history.push(Role::Muted, "Evidence: trace\n");
        history.start_block();
        history.push(Role::Plain, "You: thanks\n");
        assert_eq!(
            text(&history),
            format!(
                "{WELCOME}\nYou: hi\nMicrogentic: Looking.\n  ✓ read_file a.rs\nDone.\nEvidence: trace\n\nYou: thanks\n"
            )
        );
    }

    #[test]
    fn concurrent_calls_resolve_in_place_in_any_order() {
        let mut history = History::default();
        history.start_tool(call("c1", "read_file", "a.rs"));
        history.start_tool(call("c2", "list_tree", "src"));
        assert!(history.has_running());
        assert_eq!(text(&history), "  ⠋ read_file a.rs\n  ⠋ list_tree src\n");
        history.finish_tool("c2", "list_tree", false);
        assert_eq!(text(&history), "  ⠋ read_file a.rs\n  ✗ list_tree src\n");
        history.finish_tool("c1", "read_file", true);
        assert_eq!(text(&history), "  ✓ read_file a.rs\n  ✗ list_tree src\n");
        assert!(!history.has_running());
        // A repeated or unknown report changes nothing.
        let revision = history.revision;
        history.finish_tool("c1", "read_file", false);
        history.finish_tool("zz", "other", true);
        assert_eq!(history.revision, revision);
        assert_eq!(text(&history), "  ✓ read_file a.rs\n  ✗ list_tree src\n");
    }

    #[test]
    fn the_oldest_running_call_is_the_one_executing() {
        let mut history = History::default();
        assert_eq!(history.oldest_running(), None);
        history.start_tool(call("c1", "shell_command", "x"));
        history.start_tool(call("c2", "read_file", "a.rs"));
        history.start_tool(call("c3", "list_tree", "src"));
        assert_eq!(history.oldest_running(), Some("shell_command"));
        history.finish_tool("c1", "shell_command", true);
        assert_eq!(history.oldest_running(), Some("read_file"));
        // A later call finishing first does not change which call is executing.
        history.finish_tool("c3", "list_tree", true);
        assert_eq!(history.oldest_running(), Some("read_file"));
        history.finish_tool("c2", "read_file", false);
        assert_eq!(history.oldest_running(), None);
    }

    #[test]
    fn calls_without_ids_resolve_oldest_first_by_name() {
        let mut history = History::default();
        history.start_tool(call("", "read_file", "a.rs"));
        history.start_tool(call("", "read_file", "b.rs"));
        history.finish_tool("", "read_file", true);
        assert_eq!(text(&history), "  ✓ read_file a.rs\n  ⠋ read_file b.rs\n");
    }

    #[test]
    fn a_repeated_start_refreshes_the_existing_line() {
        let mut history = History::default();
        history.start_tool(call("c1", "read_file", "a.rs"));
        history.start_tool(call("c1", "read_file", "b.rs"));
        assert_eq!(text(&history), "  ⠋ read_file b.rs\n");
    }

    #[test]
    fn stopping_resolves_only_running_calls() {
        let mut history = History::default();
        history.start_tool(call("c1", "read_file", "a.rs"));
        history.start_tool(call("c2", "read_file", "b.rs"));
        history.finish_tool("c1", "read_file", true);
        history.stop_running();
        assert!(!history.has_running());
        assert_eq!(
            text(&history),
            "  ✓ read_file a.rs\n  – read_file b.rs · stopped\n"
        );
    }
}
