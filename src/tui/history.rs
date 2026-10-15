//! Conversation history shown in the TUI.
//!
//! History is a list of entries: styled text, and tool-call lines that change
//! state in place. Vertical rhythm is one blank line between exchanges and
//! commands, with replies, tool lines, and footers tight inside an exchange.
use super::styled::{Role, Styled};
use super::tool_lines::{CallState, ToolLine};

#[derive(Debug)]
enum Entry {
    Text(Styled),
    Tool(ToolLine),
}

/// Conversation history with a revision that changes on every edit, so the
/// redraw check never needs to compare or clone the whole history.
#[derive(Default)]
pub(super) struct History {
    entries: Vec<Entry>,
    pub(super) revision: usize,
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
            None | Some(Entry::Tool(_)) => true,
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

    /// Start an exchange or command output, one blank line below the last.
    pub(super) fn start_block(&mut self) {
        self.start_line();
        if !self.entries.is_empty() {
            self.push(Role::Plain, "\n");
        }
    }

    /// Add a running call, or refresh one already shown with the same ID.
    pub(super) fn start_tool(&mut self, line: ToolLine) {
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

    pub(super) fn has_running(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry, Entry::Tool(line) if line.state == CallState::Running))
    }

    /// The whole history as styled text, with spinners at `frame`.
    pub(super) fn render(&self, frame: usize) -> Styled {
        let mut out = Styled::default();
        for entry in &self.entries {
            match entry {
                Entry::Text(styled) => {
                    for (role, text) in &styled.runs {
                        out.push(*role, text);
                    }
                }
                Entry::Tool(line) => line.render(frame, &mut out),
            }
        }
        out
    }
}

pub(super) const WELCOME: &str = "Talk to Microgentic. Ask for work when you're ready.\nWhen requested, Microgentic can edit files and run project commands.\nConfigure defaults with microgentic setup outside this view.\nType /help for commands.\n";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(history: &History) -> String {
        history
            .render(0)
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
            history.render(0).runs,
            [
                (Role::AgentLabel, "Microgentic:".to_owned()),
                (Role::Plain, " Hello".to_owned())
            ]
        );
        // Every edit changes the redraw signature, even one that adds nothing visible.
        assert_eq!(history.revision, 4);
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
