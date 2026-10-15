//! What the model received in one request, for the conversation view's
//! context panel.
//!
//! The worker builds a snapshot from the exact assembled provider request
//! (`llm.provider_request.assembled`) through the trace recorder's
//! conversation hook. The trace recorder has already credential-redacted that
//! payload. Each element keeps only a few short lines, so the view never holds
//! a whole request. Nothing here changes what is sent to the provider.
use crate::agent::{
    FILE_OBSERVATION_CALL_ID_PREFIX, FILE_OBSERVATION_PREFIX, REASONING_CHECKPOINT_PREFIX,
    TOOL_RESULT_SUMMARY_PREFIX, estimate_tokens,
};
use crate::guidance::{NESTED_MESSAGE_PREFIX, SECTION_HEADING};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

/// Non-blank lines of an element's text kept for its preview.
const PREVIEW_LINES: usize = 3;
/// Characters kept from each preview line.
const PREVIEW_LINE_CHARS: usize = 120;
/// Characters kept from each string in a tool call's arguments.
const ARGUMENT_CHARS: usize = 120;

/// One model request, as the context panel shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ContextSnapshot {
    /// This request's position in the exchange, from 1.
    pub(crate) request: usize,
    pub(crate) model: String,
    pub(crate) message_count: usize,
    /// The same four-characters-per-token estimate as the context ledger.
    pub(crate) estimated_tokens: usize,
    /// `None` when neither the model table nor the provider gives a size.
    pub(crate) window_tokens: Option<usize>,
    /// Whether the model's thinking from earlier requests is sent again.
    pub(crate) preserve_reasoning: bool,
    /// Oldest whole exchanges left out of this exchange's history.
    pub(crate) omitted_exchanges: usize,
    /// The request's messages in order. A message with two distinct parts
    /// (the system prompt and project guidance, or a prompt and a session
    /// notice) gives two elements with the same message number.
    pub(crate) elements: Vec<ContextElement>,
    /// The tool descriptors sent beside the messages.
    pub(crate) tools: Vec<ToolOffer>,
    /// The run records tool descriptors as pinned residents.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) tools_pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ContextElement {
    /// The message's position in the request, from 1.
    pub(crate) message: usize,
    pub(crate) role: String,
    pub(crate) kind: ElementKind,
    /// The calls an assistant message requests, or the one call a tool
    /// result answers, with long argument strings shortened.
    pub(crate) calls: Vec<CallSummary>,
    pub(crate) tokens: usize,
    /// At most a few lines, each shortened.
    pub(crate) preview: String,
    /// Characters of text after the preview.
    pub(crate) omitted_chars: usize,
    /// Characters of the model's own earlier reasoning sent with this message.
    pub(crate) reasoning_chars: usize,
    /// Not in the previous request of this exchange. For the first request,
    /// everything that is not retained conversation history.
    pub(crate) new: bool,
    /// Why the element is in the request and its residency state, when the
    /// run records residency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) residency: Option<ElementResidency>,
}

/// An element's residency, from the context ledger that preceded the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ElementResidency {
    pub(crate) reason: String,
    pub(crate) pinned: bool,
    pub(crate) activated_turn: u64,
    pub(crate) last_referenced_turn: Option<u64>,
    pub(crate) demoted: bool,
}

impl ElementResidency {
    fn from_item(item: &Value) -> Option<Self> {
        Some(Self {
            reason: item["inclusion_reason"].as_str()?.to_owned(),
            pinned: item["pinned"].as_bool().unwrap_or(false),
            activated_turn: item["activated_turn"].as_u64().unwrap_or(1),
            last_referenced_turn: item["last_referenced_turn"].as_u64(),
            demoted: item["demoted"].as_bool().unwrap_or(false),
        })
    }
}

/// Attach the ledger's residency records to the elements they describe. The
/// system message's base prompt and guidance section are separate elements;
/// any other message's record goes on its first element.
pub(crate) fn attach_residency(snapshot: &mut ContextSnapshot, items: &[Value]) {
    let at = |index: usize| {
        items
            .iter()
            .filter(move |item| item["component_index"].as_u64() == Some(index as u64))
    };
    let mut previous_message = 0;
    for element in &mut snapshot.elements {
        let index = element.message - 1;
        let first_part = element.message != previous_message;
        previous_message = element.message;
        element.residency = match element.kind {
            ElementKind::SystemPrompt => at(index)
                .find(|item| item["kind"] == "base_prompt")
                .and_then(ElementResidency::from_item),
            ElementKind::ProjectGuidance => at(index)
                .find(|item| item["kind"] == "root_guidance")
                .and_then(ElementResidency::from_item),
            _ if first_part => at(index).next().and_then(ElementResidency::from_item),
            _ => None,
        };
    }
    snapshot.tools_pinned = items.iter().any(|item| item["kind"] == "tool_descriptor");
}

/// What an element is. The view names each kind in words, so the panel
/// reads the same without colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ElementKind {
    SystemPrompt,
    /// The project guidance section of the system message.
    ProjectGuidance,
    /// A nested `AGENTS.md` delivered when work reached its directory.
    NestedGuidance,
    /// A later system message, which the conversation runtime does not
    /// normally send.
    SystemInstruction,
    RetainedUserPrompt,
    CurrentUserPrompt,
    /// A notice the harness appended to the current prompt, such as a
    /// stopped previous request or changed guidance.
    SessionNotice,
    /// A repair, loop, or follow-up instruction from the harness.
    HarnessInstruction,
    /// A bounded tail of the model's earlier thinking, resent by the harness.
    ReasoningCheckpoint,
    AssistantReply,
    ToolRequest,
    /// The request the harness writes when it consolidates earlier reads.
    ConsolidatedReadRequest,
    ToolResult,
    /// An older tool result the harness replaced with a short summary.
    SummarizedToolResult,
    /// Earlier reads of one file, merged by the harness.
    ConsolidatedFileObservation,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CallSummary {
    /// Empty when the provider gave none.
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolOffer {
    pub(crate) name: String,
    pub(crate) tokens: usize,
}

/// What the request alone does not say, from the conversation run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RequestFacts<'a> {
    pub(crate) request: usize,
    /// User messages up to and including the current prompt.
    pub(crate) protected_users: usize,
    /// The user's own words for this exchange, before any notice.
    pub(crate) current_prompt: &'a str,
    pub(crate) omitted_exchanges: usize,
    pub(crate) window_tokens: Option<usize>,
}

/// A stable identity for one message within this process, used to mark what
/// is new since the previous request. Never persisted.
pub(crate) fn fingerprint(message: &Value) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    message.to_string().hash(&mut hasher);
    hasher.finish()
}

/// Build the panel's view of an assembled provider request. `previous` holds
/// the fingerprints of the messages the model saw before; the returned set
/// replaces it for the next request.
pub(crate) fn build(
    payload: &Value,
    facts: &RequestFacts,
    previous: &HashSet<u64>,
) -> (ContextSnapshot, HashSet<u64>) {
    let messages = payload["messages"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut seen = HashSet::with_capacity(messages.len());
    let mut elements = Vec::with_capacity(messages.len());
    let mut users = 0;
    let mut message_chars = 0;
    for (index, message) in messages.iter().enumerate() {
        let print = fingerprint(message);
        let new = !previous.contains(&print);
        seen.insert(print);
        let role = message["role"].as_str().unwrap_or("unknown");
        if role == "user" {
            users += 1;
        }
        let content = message["content"].as_str().unwrap_or_default();
        // Sizes follow the context ledger: bytes of text, reasoning, and the
        // serialized tool calls.
        let reasoning_chars = message["reasoning_content"].as_str().map_or(0, str::len);
        let call_chars = match &message["tool_calls"] {
            Value::Null => 0,
            calls => calls.to_string().len(),
        };
        message_chars += content.len() + reasoning_chars + call_chars;
        let calls = call_summaries(&message["tool_calls"]);
        for (part, (kind, text)) in parts(index, role, content, &calls, users, facts)
            .into_iter()
            .enumerate()
        {
            // The first part carries the message's reasoning and calls.
            let extra = if part == 0 {
                reasoning_chars + call_chars
            } else {
                0
            };
            let (preview, omitted_chars) = preview(strip_marker(kind, text));
            elements.push(ContextElement {
                message: index + 1,
                role: role.to_owned(),
                kind,
                calls: if part == 0 { calls.clone() } else { Vec::new() },
                tokens: estimate_tokens(text.len() + extra),
                preview,
                omitted_chars,
                reasoning_chars: if part == 0 { reasoning_chars } else { 0 },
                new,
                residency: None,
            });
        }
    }
    let tools: Vec<ToolOffer> = payload["tools"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|tool| ToolOffer {
            name: tool["function"]["name"]
                .as_str()
                .unwrap_or("unnamed")
                .to_owned(),
            tokens: estimate_tokens(tool.to_string().len()),
        })
        .collect();
    let tool_chars: usize = payload["tools"].as_array().map_or(0, |tools| {
        tools.iter().map(|tool| tool.to_string().len()).sum()
    });
    let snapshot = ContextSnapshot {
        request: facts.request,
        model: payload["model"].as_str().unwrap_or_default().to_owned(),
        message_count: messages.len(),
        estimated_tokens: estimate_tokens(message_chars + tool_chars),
        window_tokens: facts.window_tokens,
        preserve_reasoning: payload["completion"]["preserve_reasoning"]
            .as_bool()
            .unwrap_or(false),
        omitted_exchanges: facts.omitted_exchanges,
        elements,
        tools,
        tools_pinned: false,
    };
    (snapshot, seen)
}

/// Split a message into what it is made of. Most messages are one part.
fn parts<'a>(
    index: usize,
    role: &str,
    content: &'a str,
    calls: &[CallSummary],
    user_number: usize,
    facts: &RequestFacts,
) -> Vec<(ElementKind, &'a str)> {
    use ElementKind::*;
    match role {
        "system" if index == 0 => match content.find(SECTION_HEADING) {
            Some(at) => vec![
                (SystemPrompt, &content[..at]),
                (ProjectGuidance, &content[at..]),
            ],
            None => vec![(SystemPrompt, content)],
        },
        "system" => vec![(SystemInstruction, content)],
        // Earlier prompts, then the current one, come first; the harness adds
        // its own user messages after the current prompt.
        "user" if user_number < facts.protected_users => vec![(RetainedUserPrompt, content)],
        "user" if user_number == facts.protected_users => {
            // Notices are appended after the user's own words.
            match content.strip_prefix(facts.current_prompt) {
                Some(notice) if !facts.current_prompt.is_empty() && !notice.trim().is_empty() => {
                    let prompt = &content[..facts.current_prompt.len()];
                    vec![(CurrentUserPrompt, prompt), (SessionNotice, notice)]
                }
                _ => vec![(CurrentUserPrompt, content)],
            }
        }
        "user" if content.starts_with(NESTED_MESSAGE_PREFIX) => vec![(NestedGuidance, content)],
        "user" if content.starts_with(REASONING_CHECKPOINT_PREFIX) => {
            vec![(ReasoningCheckpoint, content)]
        }
        "user" => vec![(HarnessInstruction, content)],
        "assistant" if !calls.is_empty() && calls_are_consolidated(calls) => {
            vec![(ConsolidatedReadRequest, content)]
        }
        "assistant" if !calls.is_empty() => vec![(ToolRequest, content)],
        "assistant" => vec![(AssistantReply, content)],
        "tool" if content.starts_with(TOOL_RESULT_SUMMARY_PREFIX) => {
            vec![(SummarizedToolResult, content)]
        }
        "tool" if content.starts_with(FILE_OBSERVATION_PREFIX) => {
            vec![(ConsolidatedFileObservation, content)]
        }
        "tool" => vec![(ToolResult, content)],
        _ => vec![(Other, content)],
    }
}

/// The consolidation request is the only one whose call IDs the harness
/// writes itself.
fn calls_are_consolidated(calls: &[CallSummary]) -> bool {
    calls
        .iter()
        .all(|call| call.id.starts_with(FILE_OBSERVATION_CALL_ID_PREFIX))
}

fn call_summaries(calls: &Value) -> Vec<CallSummary> {
    calls
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|call| CallSummary {
            id: call["id"].as_str().unwrap_or_default().to_owned(),
            name: call["name"].as_str().unwrap_or("unnamed").to_owned(),
            arguments: shorten_strings(&call["arguments"]),
        })
        .collect()
}

fn shorten_strings(value: &Value) -> Value {
    match value {
        Value::String(text) if text.chars().count() > ARGUMENT_CHARS => {
            Value::String(text.chars().take(ARGUMENT_CHARS).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(shorten_strings).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), shorten_strings(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The kind already says what a harness marker would say, so the preview
/// starts after it.
fn strip_marker(kind: ElementKind, text: &str) -> &str {
    let marker = match kind {
        ElementKind::SummarizedToolResult => TOOL_RESULT_SUMMARY_PREFIX,
        ElementKind::ConsolidatedFileObservation => FILE_OBSERVATION_PREFIX,
        ElementKind::ReasoningCheckpoint => REASONING_CHECKPOINT_PREFIX,
        _ => "",
    };
    text.strip_prefix(marker).unwrap_or(text)
}

/// The first few non-blank lines, each shortened, and how many characters
/// follow them. A shortened line ends the preview.
fn preview(text: &str) -> (String, usize) {
    let text = text.trim();
    let mut lines = Vec::new();
    let mut end = 0;
    for line in text.split('\n') {
        let start = end;
        end = start + line.len() + 1;
        if line.trim().is_empty() {
            continue;
        }
        if lines.len() == PREVIEW_LINES {
            end = start;
            break;
        }
        let kept: String = line.trim_end().chars().take(PREVIEW_LINE_CHARS).collect();
        if kept.len() < line.trim_end().len() {
            end = start + kept.len();
            lines.push(kept);
            break;
        }
        lines.push(kept);
    }
    let rest = text.get(end.min(text.len())..).unwrap_or_default();
    (lines.join("\n"), rest.trim().chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn facts(protected_users: usize, current_prompt: &str) -> RequestFacts<'_> {
        RequestFacts {
            request: 1,
            protected_users,
            current_prompt,
            omitted_exchanges: 0,
            window_tokens: None,
        }
    }

    fn kinds(snapshot: &ContextSnapshot) -> Vec<ElementKind> {
        snapshot.elements.iter().map(|e| e.kind).collect()
    }

    /// A second exchange shaped like a real oMLX request: system prompt with
    /// guidance, the earlier exchange's prompt, tool batch and reply, and the
    /// new prompt with a stop notice.
    fn second_exchange() -> Value {
        json!({
            "schema_version": "provider_request.v1",
            "model": "qwen3.8:27b",
            "turn": 1,
            "llm_call_depth": 0,
            "completion": {"preserve_reasoning": false, "num_ctx": null, "max_tokens": 16384},
            "messages": [
                {"role": "system", "content": "You are Microgentic.\n# Project guidance\n\nRun make check.\n"},
                {"role": "user", "content": "Slow down the opening animation by 25%"},
                {"role": "assistant", "content": "\n\n", "tool_calls": [
                    {"id": "call_1", "name": "shell_command", "arguments": {"command": "ls"}},
                    {"id": "call_2", "name": "read_file", "arguments": {"path": "src/tui/launch.rs"}}
                ]},
                {"role": "tool", "content": "[harness-retained-tool-result-summary]\nshell_command ls: 12 lines, success", "tool_calls": [
                    {"id": "call_1", "name": "shell_command", "arguments": {"command": "ls"}}
                ]},
                {"role": "tool", "content": "{\"path\":\"src/tui/launch.rs\",\"content\":\"const PACE: f64 = 1.1;\"}", "tool_calls": [
                    {"id": "call_2", "name": "read_file", "arguments": {"path": "src/tui/launch.rs"}}
                ]},
                {"role": "assistant", "content": "Consolidated prior read_file results for unchanged files in this epoch.", "tool_calls": [
                    {"id": "harness-file-observation-226774d6942aba7c", "name": "read_file", "arguments": {"path": "src/tui/mod.rs"}}
                ]},
                {"role": "tool", "content": "[harness-consolidated-file-observation]\nMetadata: {\"path\":\"src/tui/mod.rs\"}\n--- BEGIN LINE-NUMBERED CONTENT ---\n1: mod canvas;", "tool_calls": [
                    {"id": "harness-file-observation-226774d6942aba7c", "name": "read_file", "arguments": {"path": "src/tui/mod.rs"}}
                ]},
                {"role": "assistant", "content": "Set PACE to 0.88.", "tool_calls": [], "reasoning_content": "thinking"},
                {"role": "user", "content": "Write the file\n\n[Microgentic session notice: the previous request stopped: cancelled.]"},
                {"role": "user", "content": "[Project guidance for docs/ from docs/AGENTS.md. It applies...]\n\nUse plain words."},
                {"role": "user", "content": "[harness-reasoning-checkpoint]\nThe text below is a bounded tail."},
                {"role": "user", "content": "Validation passed. Answer the user's message."}
            ],
            "tools": [
                {"type": "function", "function": {"name": "read_file", "description": "Read a file", "parameters": {}}},
                {"type": "function", "function": {"name": "shell_command", "description": "Run a command", "parameters": {}}}
            ]
        })
    }

    #[test]
    fn every_message_is_classified_in_request_order() {
        let payload = second_exchange();
        let (snapshot, _) = build(&payload, &facts(2, "Write the file"), &HashSet::new());
        use ElementKind::*;
        assert_eq!(
            kinds(&snapshot),
            [
                SystemPrompt,
                ProjectGuidance,
                RetainedUserPrompt,
                ToolRequest,
                SummarizedToolResult,
                ToolResult,
                ConsolidatedReadRequest,
                ConsolidatedFileObservation,
                AssistantReply,
                CurrentUserPrompt,
                SessionNotice,
                NestedGuidance,
                ReasoningCheckpoint,
                HarnessInstruction,
            ]
        );
        assert_eq!(snapshot.message_count, 12);
        let numbers: Vec<_> = snapshot.elements.iter().map(|e| e.message).collect();
        assert_eq!(numbers, [1, 1, 2, 3, 4, 5, 6, 7, 8, 9, 9, 10, 11, 12]);
        assert_eq!(snapshot.model, "qwen3.8:27b");
        assert!(!snapshot.preserve_reasoning);
        assert_eq!(
            snapshot
                .tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["read_file", "shell_command"]
        );
    }

    #[test]
    fn split_parts_keep_their_own_text() {
        let payload = second_exchange();
        let (snapshot, _) = build(&payload, &facts(2, "Write the file"), &HashSet::new());
        let element = |kind| {
            snapshot
                .elements
                .iter()
                .find(|e| e.kind == kind)
                .unwrap()
                .clone()
        };
        assert_eq!(
            element(ElementKind::SystemPrompt).preview,
            "You are Microgentic."
        );
        assert_eq!(
            element(ElementKind::ProjectGuidance).preview,
            "# Project guidance\nRun make check."
        );
        assert_eq!(
            element(ElementKind::CurrentUserPrompt).preview,
            "Write the file"
        );
        assert_eq!(
            element(ElementKind::SessionNotice).preview,
            "[Microgentic session notice: the previous request stopped: cancelled.]"
        );
    }

    #[test]
    fn harness_markers_are_labelled_by_kind_not_shown_as_text() {
        let payload = second_exchange();
        let (snapshot, _) = build(&payload, &facts(2, "Write the file"), &HashSet::new());
        let summary = &snapshot.elements[4];
        assert_eq!(summary.preview, "shell_command ls: 12 lines, success");
        assert_eq!(summary.calls[0].name, "shell_command");
        let observation = &snapshot.elements[7];
        assert!(observation.preview.starts_with("Metadata: "));
        assert_eq!(observation.calls[0].arguments["path"], "src/tui/mod.rs");
        let request = &snapshot.elements[3];
        assert_eq!(
            request
                .calls
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["shell_command", "read_file"]
        );
        assert_eq!(request.preview, "", "whitespace-only content");
        assert_eq!(snapshot.elements[8].reasoning_chars, "thinking".len());
    }

    #[test]
    fn a_quoted_harness_marker_in_a_prompt_stays_a_prompt() {
        let payload = json!({"messages": [
            {"role": "system", "content": "s"},
            {"role": "user", "content": "[harness-reasoning-checkpoint] what is this?"},
            {"role": "user", "content": "[Project guidance for x/ is a phrase I saw"},
        ]});
        let (snapshot, _) = build(
            &payload,
            &facts(2, "[Project guidance for x/ is a phrase I saw"),
            &HashSet::new(),
        );
        assert_eq!(
            kinds(&snapshot),
            [
                ElementKind::SystemPrompt,
                ElementKind::RetainedUserPrompt,
                ElementKind::CurrentUserPrompt
            ]
        );
    }

    #[test]
    fn previews_keep_a_few_short_lines_and_count_the_rest() {
        assert_eq!(preview("\n\n  one\n\ntwo\n"), ("one\ntwo".into(), 0));
        assert_eq!(preview("a\nb\nc\nd\ne"), ("a\nb\nc".into(), 3));
        let long = "x".repeat(PREVIEW_LINE_CHARS + 30);
        assert_eq!(
            preview(&format!("{long}\nnext")),
            ("x".repeat(PREVIEW_LINE_CHARS), 35)
        );
        assert_eq!(preview("   "), (String::new(), 0));
        // Multi-byte text is counted in characters and never split.
        let (text, rest) = preview(&"é".repeat(PREVIEW_LINE_CHARS + 2));
        assert_eq!(text.chars().count(), PREVIEW_LINE_CHARS);
        assert_eq!(rest, 2);
    }

    #[test]
    fn long_arguments_are_shortened_in_calls() {
        let payload = json!({"messages": [
            {"role": "assistant", "content": "", "tool_calls": [
                {"id": "c", "name": "write_file", "arguments": {"path": "a.rs", "content": "y".repeat(5_000)}}
            ]}
        ]});
        let (snapshot, _) = build(&payload, &facts(1, ""), &HashSet::new());
        let arguments = &snapshot.elements[0].calls[0].arguments;
        assert_eq!(arguments["path"], "a.rs");
        assert_eq!(
            arguments["content"].as_str().unwrap().chars().count(),
            ARGUMENT_CHARS
        );
        // The token estimate still covers the whole call.
        assert!(snapshot.elements[0].tokens > 1_000);
    }

    #[test]
    fn new_marks_what_the_previous_request_did_not_have() {
        let first = json!({"messages": [
            {"role": "system", "content": "s"},
            {"role": "user", "content": "earlier"},
            {"role": "assistant", "content": "earlier reply", "tool_calls": []},
            {"role": "user", "content": "now"},
        ]});
        let history: HashSet<u64> = first["messages"].as_array().unwrap()[1..3]
            .iter()
            .map(fingerprint)
            .collect();
        let (snapshot, seen) = build(&first, &facts(2, "now"), &history);
        let new: Vec<bool> = snapshot.elements.iter().map(|e| e.new).collect();
        assert_eq!(new, [true, false, false, true]);

        let mut second = first.clone();
        second["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role": "assistant", "content": "", "tool_calls": [{"id": "1", "name": "read_file", "arguments": {}}]}));
        let mut facts = facts(2, "now");
        facts.request = 2;
        let (snapshot, _) = build(&second, &facts, &seen);
        let new: Vec<bool> = snapshot.elements.iter().map(|e| e.new).collect();
        assert_eq!(new, [false, false, false, false, true]);
        assert_eq!(snapshot.request, 2);
    }

    #[test]
    fn token_estimates_match_the_context_ledger_rule() {
        let payload = second_exchange();
        let (snapshot, _) = build(&payload, &facts(2, "Write the file"), &HashSet::new());
        let messages: usize = payload["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                m["content"].as_str().unwrap_or_default().len()
                    + m["reasoning_content"].as_str().map_or(0, str::len)
                    + match &m["tool_calls"] {
                        Value::Null => 0,
                        calls => calls.to_string().len(),
                    }
            })
            .sum();
        let tools: usize = payload["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.to_string().len())
            .sum();
        assert_eq!(snapshot.estimated_tokens, (messages + tools).div_ceil(4));
    }

    #[test]
    fn residency_records_attach_to_the_elements_they_describe() {
        let payload = json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": format!("Base.{SECTION_HEADING}Rules.")},
                {"role": "user", "content": "Do it."},
            ],
            "tools": [{"type": "function", "function": {"name": "read_file"}}],
        });
        let facts = RequestFacts {
            request: 1,
            protected_users: 1,
            current_prompt: "Do it.",
            omitted_exchanges: 0,
            window_tokens: None,
        };
        let (mut snapshot, _) = build(&payload, &facts, &HashSet::new());
        let items = vec![
            json!({"id": "base_prompt", "kind": "base_prompt", "component_index": 0, "inclusion_reason": "base_system_prompt", "pinned": true, "activated_turn": 1}),
            json!({"id": "guidance:AGENTS.md", "kind": "root_guidance", "component_index": 0, "inclusion_reason": "project_guidance_discovered", "pinned": true, "activated_turn": 1}),
            json!({"id": "prompt", "kind": "conversation_turn", "component_index": 1, "inclusion_reason": "current_user_prompt", "pinned": true, "activated_turn": 1}),
            json!({"id": "tool:read_file", "kind": "tool_descriptor", "component_index": null, "inclusion_reason": "active_tool_surface", "pinned": true, "activated_turn": 1}),
        ];
        attach_residency(&mut snapshot, &items);
        let reasons: Vec<_> = snapshot
            .elements
            .iter()
            .map(|element| element.residency.as_ref().map(|r| r.reason.as_str()))
            .collect();
        assert_eq!(
            reasons,
            [
                Some("base_system_prompt"),
                Some("project_guidance_discovered"),
                Some("current_user_prompt"),
            ]
        );
        assert!(snapshot.tools_pinned);
        assert!(snapshot.elements[2].residency.as_ref().unwrap().pinned);
    }
}
