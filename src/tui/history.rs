//! Conversation history shown in the TUI.
use super::styled::{Role, Styled};

/// Conversation history with a revision that changes on every edit, so the
/// redraw check never needs to compare or clone the whole history.
#[derive(Default)]
pub(super) struct History {
    pub(super) text: Styled,
    pub(super) revision: usize,
}

impl History {
    pub(super) fn push(&mut self, role: Role, text: &str) {
        self.text.push(role, text);
        self.revision += 1;
    }

    pub(super) fn reset(&mut self) {
        self.text = Styled::default();
        self.revision += 1;
        self.push(Role::Muted, WELCOME);
    }
}

pub(super) const WELCOME: &str = "Talk to Microgentic. Ask for work when you're ready.\nWhen requested, Microgentic can edit files and run project commands.\nConfigure defaults with microgentic setup outside this view.\nType /help for commands.\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styled_text_extends_the_last_run_for_streamed_content() {
        let mut history = History::default();
        history.push(Role::AgentLabel, "Microgentic:");
        history.push(Role::Plain, " Hel");
        history.push(Role::Plain, "lo");
        history.push(Role::Plain, "");
        assert_eq!(
            history.text.runs,
            [
                (Role::AgentLabel, "Microgentic:".to_owned()),
                (Role::Plain, " Hello".to_owned())
            ]
        );
        // Every edit changes the redraw signature, even one that adds nothing visible.
        assert_eq!(history.revision, 4);
    }
}
