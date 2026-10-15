//! The context panel: what the model received in its latest request.
//!
//! The worker sends a bounded snapshot of each assembled provider request.
//! This module keeps the latest one and renders it as text beside the
//! conversation. It never reads traces and never re-derives context.
use super::status_bar::compact_tokens;
use super::styled::{Role, Row, Styled};
use super::tool_lines::summarize;
use super::wrap::wrap;
use crate::context_snapshot::{ContextElement, ContextSnapshot, ElementKind, ElementResidency};
use std::cell::Cell;

/// Narrowest terminal, in columns, that shows the panel beside the conversation.
pub(super) const MIN_COLUMNS: u16 = 80;
/// Shortest terminal, in rows, that shows the panel.
pub(super) const MIN_LINES: u16 = 12;
/// The panel's share of the terminal width, in percent, and its least width.
const SHARE_PERCENT: u16 = 40;
const MIN_PANEL_COLUMNS: u16 = 34;
/// Rows moved by one panel scroll key.
const SCROLL_ROWS: usize = 8;

/// Where the panel sits when the terminal is large enough.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Split {
    /// Columns left of the separator, for the conversation.
    pub(super) left_columns: usize,
    /// The separator's column.
    pub(super) separator: usize,
    /// The first column of panel text, and how many columns it may use.
    pub(super) column: usize,
    pub(super) width: usize,
}

/// The conversation keeps the left side; the panel takes about 40% of the
/// width on the right. `None` when the terminal is too small to split.
pub(super) fn split(columns: u16, lines: u16) -> Option<Split> {
    if columns < MIN_COLUMNS || lines < MIN_LINES {
        return None;
    }
    let panel = (columns * SHARE_PERCENT / 100).max(MIN_PANEL_COLUMNS);
    let separator = usize::from(columns - panel);
    Some(Split {
        left_columns: separator,
        separator,
        column: separator + 2,
        // One blank column after the separator and one at the right edge.
        width: usize::from(panel) - 3,
    })
}

/// The hint shown instead of the panel on a small terminal.
pub(super) fn too_small_hint() -> String {
    format!("Context panel needs {MIN_COLUMNS}×{MIN_LINES} or larger · Ctrl-O hides it")
}

#[derive(Debug, Default)]
pub(super) struct ContextPanel {
    pub(super) visible: bool,
    latest: Option<(usize, ContextSnapshot)>,
    /// Exchanges sent in this conversation, for the header.
    exchanges: usize,
    /// Rows scrolled back from the newest, as in the conversation.
    scroll: usize,
    /// The furthest scroll the last render allowed, so scrolling back down
    /// starts moving at once.
    max_scroll: Cell<usize>,
    /// Changes whenever the panel's content does, for the redraw check.
    pub(super) revision: u64,
}

impl ContextPanel {
    pub(super) fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    /// A message was sent. A new conversation counts from one again.
    pub(super) fn begin_exchange(&mut self, new_conversation: bool) {
        if new_conversation {
            self.exchanges = 0;
        }
        self.exchanges += 1;
    }

    /// Show a newly assembled request, following it from the newest rows.
    pub(super) fn receive(&mut self, snapshot: ContextSnapshot) {
        self.latest = Some((self.exchanges, snapshot));
        self.scroll = 0;
        self.revision += 1;
    }

    /// The conversation ended, so its last request no longer applies.
    pub(super) fn forget(&mut self) {
        self.latest = None;
        self.exchanges = 0;
        self.scroll = 0;
        self.revision += 1;
    }

    pub(super) fn scroll_back(&mut self) {
        self.scroll = self
            .scroll
            .min(self.max_scroll.get())
            .saturating_add(SCROLL_ROWS)
            .min(self.max_scroll.get());
    }

    pub(super) fn scroll_forward(&mut self) {
        self.scroll = self
            .scroll
            .min(self.max_scroll.get())
            .saturating_sub(SCROLL_ROWS);
    }

    pub(super) fn scroll(&self) -> usize {
        self.scroll
    }

    /// The panel's rows for `height` rows of `width` columns: a fixed
    /// header, then the request's elements, newest at the bottom unless
    /// scrolled back.
    pub(super) fn rows(&self, width: usize, height: usize) -> Vec<Row> {
        let Some((exchange, snapshot)) = &self.latest else {
            let mut rows = wrap(&title(), width);
            rows.extend(wrap(
                &Styled::default().with(
                    Role::Muted,
                    "No request yet. After you send a message, this shows everything the model receives.",
                ),
                width,
            ));
            rows.truncate(height);
            return rows;
        };
        let mut rows = wrap(&header(*exchange, snapshot), width);
        let body = wrap(&body(snapshot), width);
        let visible = height.saturating_sub(rows.len() + 1);
        let max_scroll = body.len().saturating_sub(visible);
        self.max_scroll.set(max_scroll);
        let scroll = self.scroll.min(max_scroll);
        let end = body.len() - scroll;
        let start = end.saturating_sub(visible);
        rows.extend(wrap(&rule(start, scroll, width), width));
        rows.extend_from_slice(&body[start..end]);
        rows.truncate(height);
        rows
    }
}

fn title() -> Styled {
    Styled::default().with(Role::Folder, "Context of the latest request")
}

fn header(exchange: usize, snapshot: &ContextSnapshot) -> Styled {
    let mut out = title();
    out.push(Role::Plain, "\n");
    out.push(
        Role::Plain,
        &format!(
            "Exchange {exchange} · request {} · {}\n",
            snapshot.request, snapshot.model
        ),
    );
    let size = match snapshot.window_tokens {
        Some(window) => format!(
            "~{} of {} tokens",
            compact_tokens(snapshot.estimated_tokens),
            compact_tokens(window)
        ),
        None => format!(
            "~{} tokens, window unknown",
            compact_tokens(snapshot.estimated_tokens)
        ),
    };
    out.push(
        Role::Plain,
        &format!(
            "{} · {size}\n",
            count(snapshot.message_count, "message", "messages")
        ),
    );
    if snapshot.preserve_reasoning {
        out.push(
            Role::Plain,
            "Earlier reasoning: retained. Thinking from earlier requests is sent again.\n",
        );
    } else {
        out.push(
            Role::Rewritten,
            "Earlier reasoning: not retained. Thinking from earlier requests is not sent; only replies and tool calls are.\n",
        );
    }
    match snapshot.omitted_exchanges {
        0 => out.push(Role::Plain, "Older exchanges omitted: none"),
        omitted => out.push(
            Role::Rewritten,
            &format!("Older exchanges omitted: {omitted}, to fit the history allowance"),
        ),
    }
    out
}

/// The line between header and elements. It tells how much is out of view.
fn rule(above: usize, below: usize, width: usize) -> Styled {
    let text = match (above, below) {
        (0, 0) => "─".repeat(width),
        (above, 0) => format!("↑ {above} more rows · Alt-PgUp"),
        (0, below) => format!("↓ {below} more rows · Alt-PgDn"),
        (above, below) => format!("↑ {above} more rows · ↓ {below} more rows"),
    };
    Styled::default().with(Role::Muted, &text)
}

fn body(snapshot: &ContextSnapshot) -> Styled {
    let mut out = Styled::default();
    let mut previous_message = 0;
    for element in &snapshot.elements {
        element_text(element, element.message == previous_message, &mut out);
        previous_message = element.message;
    }
    let tool_tokens: usize = snapshot.tools.iter().map(|tool| tool.tokens).sum();
    out.push(
        Role::Folder,
        &format!(
            "Tools offered: {} · ~{} tok{}\n",
            snapshot.tools.len(),
            compact_tokens(tool_tokens),
            if snapshot.tools_pinned {
                " · pinned"
            } else {
                ""
            }
        ),
    );
    if snapshot.tools.is_empty() {
        out.push(Role::Muted, "  none in this request");
    } else {
        let names: Vec<String> = snapshot
            .tools
            .iter()
            .map(|tool| format!("{} ~{}", tool.name, compact_tokens(tool.tokens)))
            .collect();
        out.push(Role::Plain, &format!("  {}", names.join(", ")));
    }
    out
}

fn element_text(element: &ContextElement, same_message: bool, out: &mut Styled) {
    out.push(
        Role::Folder,
        &format!("{}. {}", element.message, element.role),
    );
    out.push(Role::Plain, " · ");
    out.push(kind_role(element.kind), kind_label(element.kind));
    if same_message {
        out.push(Role::Muted, " (same message)");
    }
    out.push(
        Role::Plain,
        &format!(" · ~{} tok · ", compact_tokens(element.tokens)),
    );
    if element.new {
        out.push(Role::Fresh, "new");
    } else {
        out.push(Role::Muted, "kept");
    }
    out.push(Role::Plain, "\n");
    if let Some(residency) = &element.residency {
        out.push(Role::Muted, &format!("  {}\n", residency_line(residency)));
    }
    let call_verb = if element.role == "tool" {
        "answers"
    } else {
        "calls"
    };
    for call in &element.calls {
        out.push(
            Role::Muted,
            &format!(
                "  {call_verb} {} {}\n",
                call.name,
                summarize(&call.name, &call.arguments)
            ),
        );
    }
    if element.reasoning_chars > 0 {
        out.push(
            Role::Muted,
            &format!(
                "  with earlier reasoning, {} chars\n",
                thousands(element.reasoning_chars)
            ),
        );
    }
    if element.preview.is_empty() {
        out.push(Role::Muted, "  (no text)\n");
    } else {
        for line in element.preview.lines() {
            out.push(Role::Plain, &format!("  {line}\n"));
        }
    }
    if element.omitted_chars > 0 {
        out.push(
            Role::Muted,
            &format!("  … {} more chars\n", thousands(element.omitted_chars)),
        );
    }
}

/// Why an element is in the request and its residency state, in words.
fn residency_line(residency: &ElementResidency) -> String {
    let mut parts = vec![residency.reason.replace('_', " ")];
    if residency.pinned {
        parts.push("pinned".into());
    }
    parts.push(format!("since turn {}", residency.activated_turn));
    if let Some(turn) = residency.last_referenced_turn {
        parts.push(format!("last referenced turn {turn}"));
    }
    if residency.demoted {
        parts.push("demoted".into());
    }
    parts.join(" · ")
}

fn kind_label(kind: ElementKind) -> &'static str {
    match kind {
        ElementKind::SystemPrompt => "system prompt",
        ElementKind::ProjectGuidance => "project guidance",
        ElementKind::NestedGuidance => "nested project guidance",
        ElementKind::SystemInstruction => "system instruction",
        ElementKind::RetainedUserPrompt => "retained user prompt",
        ElementKind::CurrentUserPrompt => "current user prompt",
        ElementKind::SessionNotice => "session notice",
        ElementKind::HarnessInstruction => "harness instruction (repair or loop)",
        ElementKind::ReasoningCheckpoint => "reasoning checkpoint, not verbatim",
        ElementKind::AssistantReply => "assistant reply",
        ElementKind::ToolRequest => "tool request",
        ElementKind::ConsolidatedReadRequest => "consolidated reads, written by the harness",
        ElementKind::ToolResult => "tool result",
        ElementKind::SummarizedToolResult => "summarized tool result, not verbatim",
        ElementKind::ConsolidatedFileObservation => "consolidated file observation, not verbatim",
        ElementKind::Other => "other",
    }
}

/// Text the harness rewrote stands apart from what the model or user wrote.
fn kind_role(kind: ElementKind) -> Role {
    match kind {
        ElementKind::ReasoningCheckpoint
        | ElementKind::ConsolidatedReadRequest
        | ElementKind::SummarizedToolResult
        | ElementKind::ConsolidatedFileObservation => Role::Rewritten,
        _ => Role::Plain,
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `12,345`, for character counts.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::styled::rows_text;
    use super::*;
    use crate::context_snapshot::{CallSummary, ToolOffer};
    use serde_json::json;

    fn element(message: usize, role: &str, kind: ElementKind, preview: &str) -> ContextElement {
        ContextElement {
            message,
            role: role.into(),
            kind,
            calls: Vec::new(),
            tokens: 5,
            preview: preview.into(),
            omitted_chars: 0,
            reasoning_chars: 0,
            new: false,
            residency: None,
        }
    }

    fn snapshot() -> ContextSnapshot {
        let mut request = element(4, "assistant", ElementKind::ToolRequest, "");
        request.calls = vec![CallSummary {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: json!({"path": "src/lib.rs", "line_start": 1, "line_end": 9}),
        }];
        let mut summary = element(
            5,
            "tool",
            ElementKind::SummarizedToolResult,
            "read_file: 9 lines",
        );
        summary.calls = request.calls.clone();
        let mut current = element(6, "user", ElementKind::CurrentUserPrompt, "Write the file");
        current.new = true;
        let mut system = element(
            1,
            "system",
            ElementKind::SystemPrompt,
            "You are Microgentic.",
        );
        system.omitted_chars = 1_368;
        ContextSnapshot {
            request: 1,
            model: "qwen3.8:27b".into(),
            message_count: 6,
            estimated_tokens: 1_234,
            window_tokens: None,
            preserve_reasoning: false,
            omitted_exchanges: 0,
            elements: vec![
                system,
                element(
                    1,
                    "system",
                    ElementKind::ProjectGuidance,
                    "# Project guidance",
                ),
                element(2, "user", ElementKind::RetainedUserPrompt, "Add a test"),
                element(
                    3,
                    "assistant",
                    ElementKind::AssistantReply,
                    "Here is the test:",
                ),
                request,
                summary,
                current,
            ],
            tools: vec![ToolOffer {
                name: "read_file".into(),
                tokens: 150,
            }],
            tools_pinned: false,
        }
    }

    fn panel() -> ContextPanel {
        let mut panel = ContextPanel::default();
        panel.begin_exchange(true);
        panel.begin_exchange(false);
        panel.receive(snapshot());
        panel
    }

    #[test]
    fn the_panel_lists_every_element_with_labels_that_need_no_colour() {
        let text = rows_text(&panel().rows(60, 100)).join("\n");
        assert_eq!(
            text,
            "Context of the latest request
Exchange 2 · request 1 · qwen3.8:27b
6 messages · ~1.2k tokens, window unknown
Earlier reasoning: not retained. Thinking from earlier
requests is not sent; only replies and tool calls are.
Older exchanges omitted: none
────────────────────────────────────────────────────────────
1. system · system prompt · ~5 tok · kept
  You are Microgentic.
  … 1,368 more chars
1. system · project guidance (same message) · ~5 tok · kept
  # Project guidance
2. user · retained user prompt · ~5 tok · kept
  Add a test
3. assistant · assistant reply · ~5 tok · kept
  Here is the test:
4. assistant · tool request · ~5 tok · kept
  calls read_file src/lib.rs:1-9
  (no text)
5. tool · summarized tool result, not verbatim · ~5 tok ·
   kept
  answers read_file src/lib.rs:1-9
  read_file: 9 lines
6. user · current user prompt · ~5 tok · new
  Write the file
Tools offered: 1 · ~150 tok
  read_file ~150"
        );
    }

    #[test]
    fn rewritten_text_and_new_elements_carry_their_own_roles() {
        let rows = panel().rows(80, 100);
        let role_of = |needle: &str| {
            rows.iter()
                .flatten()
                .find(|(_, text)| text.contains(needle))
                .map(|(role, _)| *role)
        };
        assert_eq!(role_of("summarized tool result"), Some(Role::Rewritten));
        assert_eq!(
            role_of("Earlier reasoning: not retained"),
            Some(Role::Rewritten)
        );
        assert_eq!(role_of("new"), Some(Role::Fresh));
    }

    #[test]
    fn the_newest_rows_show_first_and_scrolling_reveals_older_ones() {
        let mut panel = panel();
        let rows = rows_text(&panel.rows(60, 12));
        assert_eq!(rows.len(), 12);
        assert!(rows[6].starts_with("↑ "), "{rows:?}");
        assert_eq!(rows.last().unwrap(), "  read_file ~150");
        panel.scroll_back();
        let rows = rows_text(&panel.rows(60, 12));
        assert!(rows[6].contains("↓ 8 more rows"), "{rows:?}");
        for _ in 0..10 {
            panel.scroll_back();
        }
        let rows = rows_text(&panel.rows(60, 12));
        assert!(rows[6].starts_with("↓ "), "{rows:?}");
        assert_eq!(rows[7], "1. system · system prompt · ~5 tok · kept");
        // One step forward moves at once, however far the keys went back.
        panel.scroll_forward();
        let rows = rows_text(&panel.rows(60, 12));
        assert!(rows[6].contains("↑ 8 more rows"), "{rows:?}");
        // A new request follows the newest rows again.
        panel.receive(snapshot());
        assert_eq!(panel.scroll(), 0);
    }

    #[test]
    fn before_any_request_the_panel_says_so() {
        let text = rows_text(&ContextPanel::default().rows(40, 20)).join("\n");
        assert!(text.starts_with("Context of the latest request\nNo request yet."));
        let mut panel = panel();
        panel.forget();
        assert!(rows_text(&panel.rows(40, 20))[1].starts_with("No request yet."));
    }

    #[test]
    fn a_known_window_and_retained_reasoning_read_plainly() {
        let mut snapshot = snapshot();
        snapshot.window_tokens = Some(32_768);
        snapshot.preserve_reasoning = true;
        snapshot.omitted_exchanges = 2;
        let mut panel = ContextPanel::default();
        panel.begin_exchange(true);
        panel.receive(snapshot);
        let text = rows_text(&panel.rows(200, 100)).join("\n");
        assert!(text.contains("Exchange 1 · request 1"), "{text}");
        assert!(
            text.contains("6 messages · ~1.2k of 32.8k tokens"),
            "{text}"
        );
        assert!(text.contains("Earlier reasoning: retained."), "{text}");
        assert!(text.contains("Older exchanges omitted: 2"), "{text}");
    }

    #[test]
    fn residency_shows_why_each_element_is_there_and_its_state() {
        let mut snapshot = snapshot();
        let residency = |reason: &str, pinned, last, demoted| ElementResidency {
            reason: reason.into(),
            pinned,
            activated_turn: 2,
            last_referenced_turn: last,
            demoted,
        };
        snapshot.elements[0].residency = Some(residency("base_system_prompt", true, None, false));
        snapshot.elements[5].residency = Some(residency(
            "retained_summarized_tool_result",
            false,
            Some(3),
            true,
        ));
        snapshot.tools_pinned = true;
        let mut panel = ContextPanel::default();
        panel.begin_exchange(true);
        panel.receive(snapshot);
        let text = rows_text(&panel.rows(200, 100)).join("\n");
        assert!(
            text.contains("1. system · system prompt · ~5 tok · kept\n  base system prompt · pinned · since turn 2\n"),
            "{text}"
        );
        assert!(
            text.contains("  retained summarized tool result · since turn 2 · last referenced turn 3 · demoted\n"),
            "{text}"
        );
        assert!(
            text.contains("Tools offered: 1 · ~150 tok · pinned"),
            "{text}"
        );
    }

    #[test]
    fn the_split_keeps_a_useful_conversation_width() {
        assert_eq!(split(79, 40), None);
        assert_eq!(split(120, 11), None);
        let at_minimum = split(80, 12).unwrap();
        assert_eq!(at_minimum.separator, 46);
        assert_eq!(at_minimum.column, 48);
        assert_eq!(at_minimum.width, 31);
        assert_eq!(at_minimum.column + at_minimum.width, 79);
        let wide = split(200, 50).unwrap();
        assert_eq!(wide.separator, 120);
        assert_eq!(wide.width, 77);
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(999), "999");
    }
}
