//! Context residency recording for everyday runs and the conversation.
//!
//! This is measurement only: it records what each request carries, why, and
//! since when, and it never changes what is sent. Benchmark runs have no
//! residency recorder, so their traces are unchanged.
//!
//! Every item in a request gets a record on its context ledger: base prompt,
//! tool descriptors, root guidance, nested guidance, conversation turns,
//! harness instructions, tool results and file observations. Discrete events
//! mark changes of state:
//!
//! - `context.residency.activated`: root and ancestor guidance at the start
//!   of a run, nested guidance when it is delivered.
//! - `context.residency.referenced`: delivered nested guidance, when a tool
//!   call names a path inside its directory (the same `path`, `cwd` and change
//!   paths that trigger delivery), or when the model's visible output names
//!   the guidance file's path.
//! - `context.residency.demoted`: whenever an existing compaction pass
//!   summarizes a tool result or merges or drops file observations.
//!
//! Item identities are stable within one run. A tool result is identified by
//! its tool call ID, or by its message position when the provider gave none.
use crate::agent::{
    FILE_OBSERVATION_PREFIX, REASONING_CHECKPOINT_PREFIX, TOOL_RESULT_SUMMARY_PREFIX,
    estimate_tokens,
};
use crate::context_window::ContextWindow;
use crate::guidance::{
    Discovery, GuidanceFile, NESTED_MESSAGE_PREFIX, SECTION_HEADING, Scope, Status,
};
use crate::trace::TraceRecorder;
use anyhow::Result;
use mojentic::llm::models::{LlmMessage, MessageRole};
use mojentic::llm::tools::LlmTool;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

/// The window assumed for standing-context accounting when none is known.
pub(crate) const ASSUMED_WINDOW_TOKENS: usize = 32_000;

/// Standing-context settings. The limit is recorded, not enforced.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub(crate) struct Settings {
    /// All standing context (guidance now, skills later) as a fraction of
    /// the window.
    pub(crate) standing_context_limit: f64,
    /// The guidance budget as a fraction of the window, between the existing
    /// floor and ceiling.
    pub(crate) guidance_fraction: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            standing_context_limit: 0.15,
            guidance_fraction: 0.125,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    BasePrompt,
    ToolDescriptor,
    RootGuidance,
    NestedGuidance,
    ConversationTurn,
    HarnessInstruction,
    ToolResult,
    FileObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Position {
    StablePrefix,
    VolatileTail,
}

/// One resident item of one request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Item {
    pub(crate) id: String,
    pub(crate) kind: Kind,
    pub(crate) owner: &'static str,
    pub(crate) inclusion_reason: String,
    pub(crate) estimated_tokens: usize,
    position: Position,
    pub(crate) activated_turn: usize,
    pub(crate) last_referenced_turn: Option<usize>,
    pub(crate) pinned: bool,
    pub(crate) demoted: bool,
    /// The ledger component (message) that carries it; `None` for tools.
    pub(crate) component_index: Option<usize>,
}

/// A guidance file in the system context.
#[derive(Debug, Clone)]
struct RootGuidance {
    shown: String,
    owner: &'static str,
    tokens: usize,
}

/// A nested guidance file delivered during this run.
#[derive(Debug, Clone)]
struct Nested {
    shown: String,
    directory: String,
    activated_turn: usize,
    last_referenced_turn: Option<usize>,
}

#[derive(Debug, Default)]
struct State {
    root: Vec<RootGuidance>,
    nested: Vec<Nested>,
    /// When each message item was first seen.
    activated: BTreeMap<String, usize>,
    /// User messages up to and including the current prompt.
    protected_users: usize,
}

/// What everyday runs and the conversation record about their context.
#[derive(Debug)]
pub(crate) struct Residency {
    window: ContextWindow,
    settings: Settings,
    state: Mutex<State>,
}

impl Residency {
    pub(crate) fn new(window: ContextWindow, settings: Settings) -> Self {
        Self {
            window,
            settings,
            state: Mutex::new(State {
                protected_users: 1,
                ..State::default()
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("residency mutex poisoned")
    }

    /// The conversation keeps earlier exchanges ahead of the current prompt.
    pub(crate) fn set_protected_users(&self, protected_users: usize) {
        self.state().protected_users = protected_users.max(1);
    }

    /// Root, ancestor and user guidance enter at the start of the run.
    pub(crate) fn activate_guidance(
        &self,
        trace: &TraceRecorder,
        discovery: &Discovery,
    ) -> Result<()> {
        let included: Vec<RootGuidance> = discovery
            .files
            .iter()
            .filter(|file| matches!(file.status, Status::Included | Status::Truncated { .. }))
            .map(|file| RootGuidance {
                shown: file.shown.clone(),
                owner: if matches!(file.scope, Scope::User) {
                    "user"
                } else {
                    "project"
                },
                tokens: estimate_tokens(file.text.len()),
            })
            .collect();
        for file in &included {
            trace.event(
                "context.residency.activated",
                json!({
                    "item": format!("guidance:{}", file.shown),
                    "kind": Kind::RootGuidance,
                    "owner": file.owner,
                    "estimated_tokens": file.tokens,
                    "reason": "discovered at run start",
                    "turn": 1,
                    "pinned": true,
                }),
            )?;
        }
        self.state().root = included;
        Ok(())
    }

    /// A nested guidance file was delivered in `message`.
    pub(crate) fn activate_nested(
        &self,
        trace: &TraceRecorder,
        file: &GuidanceFile,
        message: &str,
        turn: usize,
    ) -> Result<()> {
        let directory = match &file.scope {
            Scope::Nested { directory } => directory.clone(),
            _ => ".".into(),
        };
        trace.event(
            "context.residency.activated",
            json!({
                "item": format!("guidance:{}", file.shown),
                "kind": Kind::NestedGuidance,
                "owner": "project",
                "estimated_tokens": estimate_tokens(message.len()),
                "reason": format!("first tool call under {directory}/"),
                "turn": turn,
                "pinned": false,
            }),
        )?;
        self.state().nested.push(Nested {
            shown: file.shown.clone(),
            directory,
            activated_turn: turn,
            last_referenced_turn: None,
        });
        Ok(())
    }

    /// Delivered nested guidance is referenced when a tool call names a
    /// path inside its directory.
    pub(crate) fn reference_tool_calls(
        &self,
        trace: &TraceRecorder,
        arguments: &[&HashMap<String, Value>],
        turn: usize,
    ) -> Result<()> {
        let touched: Vec<String> = arguments
            .iter()
            .flat_map(|args| crate::guidance::touched_paths(args))
            .collect();
        let referenced = self.mark(turn, |nested| {
            touched
                .iter()
                .find(|path| crate::guidance::applies(&nested.directory, path))
                .cloned()
        });
        for (item, path) in referenced {
            trace.event(
                "context.residency.referenced",
                json!({"item": item, "kind": Kind::NestedGuidance, "turn": turn, "via": "tool_call", "path": path}),
            )?;
        }
        Ok(())
    }

    /// Delivered nested guidance is referenced when the model's visible
    /// output names the guidance file's path.
    pub(crate) fn reference_text(
        &self,
        trace: &TraceRecorder,
        text: &str,
        turn: usize,
    ) -> Result<()> {
        let referenced = self.mark(turn, |nested| {
            text.contains(&nested.shown).then(|| nested.shown.clone())
        });
        for (item, path) in referenced {
            trace.event(
                "context.residency.referenced",
                json!({"item": item, "kind": Kind::NestedGuidance, "turn": turn, "via": "visible_output", "path": path}),
            )?;
        }
        Ok(())
    }

    /// Mark each delivered nested file that `matches` as referenced.
    fn mark(
        &self,
        turn: usize,
        matches: impl Fn(&Nested) -> Option<String>,
    ) -> Vec<(String, String)> {
        let mut state = self.state();
        let mut referenced = Vec::new();
        for nested in &mut state.nested {
            if let Some(path) = matches(nested) {
                nested.last_referenced_turn = Some(turn);
                referenced.push((format!("guidance:{}", nested.shown), path));
            }
        }
        referenced
    }

    /// Demotion events for the existing compaction passes' trace events.
    pub(crate) fn observe(&self, kind: &str, payload: &Value) -> Vec<(&'static str, Value)> {
        let common = |item_kind: Kind, mechanism: &str| {
            json!({
                "kind": item_kind,
                "mechanism": mechanism,
                "turn": payload["turn"],
                "llm_call_depth": payload["llm_call_depth"],
            })
        };
        let demoted = |mut event: Value, extra: Value| {
            if let (Some(event), Some(extra)) = (event.as_object_mut(), extra.as_object()) {
                event.extend(extra.clone());
            }
            ("context.residency.demoted", event)
        };
        match kind {
            "llm.context_assembly.tool_result_compacted" => {
                let mechanism = if payload["effective_repair_handoff"] == true {
                    "repair_packet_compaction"
                } else {
                    "tool_result_compaction"
                };
                vec![demoted(
                    common(Kind::ToolResult, mechanism),
                    json!({
                        "message_index": payload["message_index"],
                        "tool_name": payload["tool_name"],
                        "estimated_tokens_before": payload["original_estimated_tokens"],
                        "estimated_tokens_after": payload["retained_estimated_tokens"],
                    }),
                )]
            }
            "llm.context_assembly.file_observations_coalesced" => payload["files"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|file| {
                    demoted(
                        common(Kind::FileObservation, "file_observation_coalescing"),
                        json!({
                            "path": file["path"],
                            "source_read_count": file["source_read_count"],
                            "epoch": payload["epoch"],
                        }),
                    )
                })
                .collect(),
            "llm.context_assembly.superseded_file_observations_removed" => vec![demoted(
                common(Kind::FileObservation, "superseded_file_observation_removal"),
                json!({"removed_projection_count": payload["removed_projection_count"]}),
            )],
            _ => Vec::new(),
        }
    }

    /// Add residency records, standing context and the known window's
    /// accounting to a context ledger. The ledger's own window, utilization
    /// and pressure band stay on the configured window, because run policies
    /// read them.
    pub(crate) fn annotate_ledger(
        &self,
        ledger: &mut Value,
        messages: &[LlmMessage],
        tools: &[Box<dyn LlmTool>],
        turn: usize,
    ) {
        let estimated = ledger["estimated_tokens"].as_u64().unwrap_or(0) as usize;
        let utilization = self
            .window
            .tokens
            .filter(|tokens| *tokens > 0)
            .map(|tokens| estimated as f64 / tokens as f64);
        ledger["known_window"] = json!({
            "tokens": self.window.tokens,
            "source": self.window.source,
            "utilization": utilization,
            "pressure_band": crate::agent::pressure_band(utilization),
        });
        let reasons: Vec<String> = ledger["components"]
            .as_array()
            .map(|components| {
                components
                    .iter()
                    .map(|component| {
                        component["inclusion_reason"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let items = self.items(messages, tools, &reasons, turn);
        ledger["standing_context"] = self.standing(&items);
        ledger["residency_settings"] = json!(self.settings);
        ledger["residency"] = json!(items);
    }

    fn items(
        &self,
        messages: &[LlmMessage],
        tools: &[Box<dyn LlmTool>],
        reasons: &[String],
        turn: usize,
    ) -> Vec<Item> {
        let mut state = self.state();
        let mut items = Vec::new();
        let mut users = 0;
        for (index, message) in messages.iter().enumerate() {
            let content = message.content.as_deref().unwrap_or_default();
            let tokens = estimate_tokens(crate::agent::message_chars(message));
            let reason = reasons.get(index).cloned().unwrap_or_default();
            match message.role {
                MessageRole::System if index == 0 => {
                    let section = content
                        .find(SECTION_HEADING)
                        .map_or(0, |at| estimate_tokens(content.len() - at));
                    items.push(fixed(
                        "base_prompt",
                        Kind::BasePrompt,
                        "harness",
                        reason,
                        tokens.saturating_sub(section),
                        index,
                    ));
                    let mut framing = section;
                    for file in &state.root {
                        framing = framing.saturating_sub(file.tokens);
                        items.push(fixed(
                            &format!("guidance:{}", file.shown),
                            Kind::RootGuidance,
                            file.owner,
                            "project_guidance_discovered".into(),
                            file.tokens,
                            index,
                        ));
                    }
                    if section > 0 {
                        items.push(fixed(
                            "guidance:framing",
                            Kind::RootGuidance,
                            "harness",
                            "project_guidance_framing".into(),
                            framing,
                            index,
                        ));
                    }
                }
                MessageRole::User => {
                    users += 1;
                    let (id, kind, owner, reason, pinned) = if let Some(nested) = content
                        .starts_with(NESTED_MESSAGE_PREFIX)
                        .then(|| {
                            state
                                .nested
                                .iter()
                                .find(|n| content.contains(&format!(" from {}.", n.shown)))
                        })
                        .flatten()
                    {
                        (
                            format!("guidance:{}", nested.shown),
                            Kind::NestedGuidance,
                            "project",
                            "nested_guidance_delivered".to_owned(),
                            false,
                        )
                    } else if users == state.protected_users {
                        (
                            "prompt".into(),
                            Kind::ConversationTurn,
                            "user",
                            "current_user_prompt".into(),
                            true,
                        )
                    } else if users < state.protected_users {
                        (
                            format!("history:#{index}"),
                            Kind::ConversationTurn,
                            "user",
                            "retained_conversation_user_prompt".into(),
                            false,
                        )
                    } else {
                        let reason = if content.contains(REASONING_CHECKPOINT_PREFIX) {
                            "reasoning_checkpoint".into()
                        } else {
                            reason
                        };
                        (
                            format!("instruction:#{index}"),
                            Kind::HarnessInstruction,
                            "harness",
                            reason,
                            false,
                        )
                    };
                    items.push(self.tracked(
                        &mut state, id, kind, owner, reason, tokens, pinned, false, index, turn,
                    ));
                }
                MessageRole::Assistant => {
                    let history = users < state.protected_users;
                    let id = if history {
                        format!("history:#{index}")
                    } else {
                        format!("assistant:#{index}")
                    };
                    items.push(self.tracked(
                        &mut state,
                        id,
                        Kind::ConversationTurn,
                        "model",
                        reason,
                        tokens,
                        false,
                        false,
                        index,
                        turn,
                    ));
                }
                MessageRole::Tool => {
                    let call_id = message
                        .tool_calls
                        .as_ref()
                        .and_then(|calls| calls.first())
                        .and_then(|call| call.id.clone())
                        .filter(|id| !id.is_empty());
                    let tool_name = message
                        .tool_calls
                        .as_ref()
                        .and_then(|calls| calls.first())
                        .map(|call| call.name.as_str())
                        .unwrap_or_default();
                    let observation =
                        content.starts_with(FILE_OBSERVATION_PREFIX) || tool_name == "read_file";
                    let kind = if observation {
                        Kind::FileObservation
                    } else {
                        Kind::ToolResult
                    };
                    let id = match call_id {
                        Some(id) => format!("tool_result:{id}"),
                        None => format!("tool_result:#{index}"),
                    };
                    let demoted = content.starts_with(TOOL_RESULT_SUMMARY_PREFIX)
                        || content.starts_with(FILE_OBSERVATION_PREFIX);
                    items.push(self.tracked(
                        &mut state, id, kind, "tool", reason, tokens, false, demoted, index, turn,
                    ));
                }
                MessageRole::System => {
                    let id = format!("instruction:#{index}");
                    items.push(self.tracked(
                        &mut state,
                        id,
                        Kind::HarnessInstruction,
                        "harness",
                        reason,
                        tokens,
                        false,
                        false,
                        index,
                        turn,
                    ));
                }
            }
        }
        for tool in tools {
            let descriptor = tool.descriptor();
            let chars = serde_json::to_string(&descriptor).unwrap_or_default().len();
            items.push(Item {
                id: format!("tool:{}", descriptor.function.name),
                kind: Kind::ToolDescriptor,
                owner: "harness",
                inclusion_reason: "active_tool_surface".into(),
                estimated_tokens: estimate_tokens(chars),
                position: Position::StablePrefix,
                activated_turn: 1,
                last_referenced_turn: None,
                pinned: true,
                demoted: false,
                component_index: None,
            });
        }
        // Nested guidance keeps its own activation and reference turns.
        for item in &mut items {
            if let Some(nested) = state
                .nested
                .iter()
                .find(|nested| item.id == format!("guidance:{}", nested.shown))
            {
                item.activated_turn = nested.activated_turn;
                item.last_referenced_turn = nested.last_referenced_turn;
            }
        }
        items
    }

    #[allow(clippy::too_many_arguments)]
    fn tracked(
        &self,
        state: &mut State,
        id: String,
        kind: Kind,
        owner: &'static str,
        inclusion_reason: String,
        estimated_tokens: usize,
        pinned: bool,
        demoted: bool,
        index: usize,
        turn: usize,
    ) -> Item {
        let activated_turn = *state.activated.entry(id.clone()).or_insert(turn);
        Item {
            id,
            kind,
            owner,
            inclusion_reason,
            estimated_tokens,
            position: Position::VolatileTail,
            activated_turn,
            last_referenced_turn: None,
            pinned,
            demoted,
            component_index: Some(index),
        }
    }

    /// Standing context (guidance now; skills later) against the combined
    /// limit. Recorded, not enforced.
    fn standing(&self, items: &[Item]) -> Value {
        let standing: Vec<&Item> = items
            .iter()
            .filter(|item| matches!(item.kind, Kind::RootGuidance | Kind::NestedGuidance))
            .collect();
        let tokens: usize = standing.iter().map(|item| item.estimated_tokens).sum();
        let window = self.window.tokens.unwrap_or(ASSUMED_WINDOW_TOKENS);
        let limit = (window as f64 * self.settings.standing_context_limit).floor() as usize;
        json!({
            "estimated_tokens": tokens,
            "items": standing.iter().filter(|item| item.id != "guidance:framing").count(),
            "limit_tokens": limit,
            "limit_fraction": self.settings.standing_context_limit,
            "window_tokens": window,
            "window_assumed": self.window.tokens.is_none(),
            "headroom_tokens": limit as i64 - tokens as i64,
            "over_limit": tokens > limit,
            "enforced": false,
        })
    }
}

fn fixed(
    id: &str,
    kind: Kind,
    owner: &'static str,
    inclusion_reason: String,
    estimated_tokens: usize,
    index: usize,
) -> Item {
    Item {
        id: id.into(),
        kind,
        owner,
        inclusion_reason,
        estimated_tokens,
        position: Position::StablePrefix,
        activated_turn: 1,
        last_referenced_turn: None,
        pinned: true,
        demoted: false,
        component_index: Some(index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_window::WindowSource;
    use mojentic::llm::models::LlmToolCall;

    fn window(tokens: Option<usize>) -> ContextWindow {
        ContextWindow {
            tokens,
            source: if tokens.is_some() {
                WindowSource::Provider
            } else {
                WindowSource::Unknown
            },
            field: None,
            discovery_error: None,
            discovery_ms: None,
            cached: false,
        }
    }

    fn file(shown: &str, scope: Scope, status: Status, text: &str) -> GuidanceFile {
        GuidanceFile {
            shown: shown.into(),
            scope,
            sha256: "0".repeat(64),
            bytes: text.len() as u64,
            status,
            text: text.into(),
        }
    }

    fn nested_file() -> GuidanceFile {
        file(
            "sub/AGENTS.md",
            Scope::Nested {
                directory: "sub".into(),
            },
            Status::Pending,
            "SUB-RULE",
        )
    }

    fn discovery() -> Discovery {
        Discovery {
            files: vec![
                file(
                    "~/.config/microgentic/AGENTS.md",
                    Scope::User,
                    Status::Included,
                    &"u".repeat(400),
                ),
                file(
                    "AGENTS.md",
                    Scope::Directory {
                        levels_up: 0,
                        repository_root: true,
                    },
                    Status::Included,
                    &"r".repeat(800),
                ),
                file(
                    "../AGENTS.md",
                    Scope::Directory {
                        levels_up: 1,
                        repository_root: false,
                    },
                    Status::Excluded {
                        reason: "over budget".into(),
                    },
                    "x",
                ),
            ],
            nested: vec![nested_file()],
            budget_tokens: 8_000,
        }
    }

    fn trace() -> (tempfile::TempDir, TraceRecorder) {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(temp.path()).unwrap();
        (temp, trace)
    }

    fn events(trace: &TraceRecorder, kind: &str) -> Vec<Value> {
        std::fs::read_to_string(trace.path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|event| event["kind"] == kind)
            .map(|event| event["payload"].clone())
            .collect()
    }

    fn call(id: Option<&str>, name: &str, path: &str) -> LlmToolCall {
        LlmToolCall {
            id: id.map(str::to_owned),
            name: name.into(),
            arguments: HashMap::from([("path".to_string(), json!(path))]),
        }
    }

    fn tool_message(call: LlmToolCall, content: &str) -> LlmMessage {
        LlmMessage {
            role: MessageRole::Tool,
            content: Some(content.into()),
            tool_calls: Some(vec![call]),
            image_paths: None,
            reasoning_content: None,
        }
    }

    fn residency(tokens: Option<usize>) -> Residency {
        Residency::new(window(tokens), Settings::default())
    }

    #[test]
    fn every_kind_of_item_gets_a_residency_record() {
        let (_temp, trace) = trace();
        let residency = residency(Some(262_144));
        residency.activate_guidance(&trace, &discovery()).unwrap();
        let nested = crate::guidance::nested_message(&nested_file());
        residency
            .activate_nested(&trace, &nested_file(), &nested, 2)
            .unwrap();
        let system = format!("Base prompt.{SECTION_HEADING}{}", "g".repeat(1_400));
        let assistant = LlmMessage {
            role: MessageRole::Assistant,
            content: Some(String::new()),
            tool_calls: Some(vec![call(Some("c1"), "read_file", "sub/a.txt")]),
            image_paths: None,
            reasoning_content: None,
        };
        let messages = vec![
            LlmMessage::system(&system),
            LlmMessage::user("Do the work."),
            assistant,
            tool_message(call(Some("c1"), "read_file", "sub/a.txt"), "contents"),
            tool_message(call(None, "shell_command", "."), "ok"),
            tool_message(
                call(Some("c3"), "shell_command", "."),
                &format!("{TOOL_RESULT_SUMMARY_PREFIX}\nsummary"),
            ),
            LlmMessage::user(&nested),
            LlmMessage::user("Continue."),
        ];
        let tools: Vec<Box<dyn LlmTool>> = vec![Box::new(crate::calculator::CalculateTool)];
        let reasons = vec!["base_system_prompt".to_owned(); messages.len()];
        let items = residency.items(&messages, &tools, &reasons, 3);
        let find = |id: &str| {
            items
                .iter()
                .find(|item| item.id == id)
                .unwrap_or_else(|| panic!("{id} missing: {items:#?}"))
        };
        let base = find("base_prompt");
        assert_eq!(
            (base.kind, base.pinned, base.position),
            (Kind::BasePrompt, true, Position::StablePrefix)
        );
        let root = find("guidance:AGENTS.md");
        assert_eq!(
            (root.kind, root.owner, root.estimated_tokens),
            (Kind::RootGuidance, "project", 200)
        );
        assert_eq!(
            find("guidance:~/.config/microgentic/AGENTS.md").owner,
            "user"
        );
        assert!(
            !items.iter().any(|item| item.id == "guidance:../AGENTS.md"),
            "excluded files are not resident"
        );
        let framing = find("guidance:framing");
        assert_eq!(
            framing.estimated_tokens + 200 + 100,
            estimate_tokens(1_400 + SECTION_HEADING.len())
        );
        let prompt = find("prompt");
        assert_eq!(
            (prompt.kind, prompt.pinned, prompt.inclusion_reason.as_str()),
            (Kind::ConversationTurn, true, "current_user_prompt")
        );
        assert_eq!(find("assistant:#2").kind, Kind::ConversationTurn);
        let read = find("tool_result:c1");
        assert_eq!(
            (read.kind, read.owner, read.demoted),
            (Kind::FileObservation, "tool", false)
        );
        assert_eq!(
            find("tool_result:#4").kind,
            Kind::ToolResult,
            "a call without an ID uses its position"
        );
        assert!(
            find("tool_result:c3").demoted,
            "a summarized result is demoted"
        );
        let guidance = find("guidance:sub/AGENTS.md");
        assert_eq!(
            (guidance.kind, guidance.activated_turn, guidance.pinned),
            (Kind::NestedGuidance, 2, false)
        );
        assert_eq!(find("instruction:#7").kind, Kind::HarnessInstruction);
        let tool = find("tool:calculate");
        assert_eq!(
            (tool.kind, tool.pinned, tool.component_index),
            (Kind::ToolDescriptor, true, None)
        );
        assert!(tool.estimated_tokens > 0);
        // Message items remember the turn they were first seen.
        let later = residency.items(&messages, &tools, &reasons, 5);
        assert_eq!(
            later
                .iter()
                .find(|item| item.id == "tool_result:c1")
                .unwrap()
                .activated_turn,
            3
        );

        let activated = events(&trace, "context.residency.activated");
        let ids: Vec<&str> = activated
            .iter()
            .map(|event| event["item"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "guidance:~/.config/microgentic/AGENTS.md",
                "guidance:AGENTS.md",
                "guidance:sub/AGENTS.md"
            ]
        );
        assert_eq!(activated[2]["kind"], "nested_guidance");
        assert_eq!(activated[2]["turn"], 2);
    }

    #[test]
    fn nested_guidance_is_referenced_by_paths_inside_it_or_by_naming_it() {
        let (_temp, trace) = trace();
        let residency = residency(None);
        let inside = HashMap::from([("path".to_string(), json!("sub/deep/file.rs"))]);
        let outside = HashMap::from([("path".to_string(), json!("subway/file.rs"))]);
        // Not yet delivered: a touch delivers it, it does not reference it.
        residency
            .reference_tool_calls(&trace, &[&inside], 1)
            .unwrap();
        assert!(events(&trace, "context.residency.referenced").is_empty());
        let message = crate::guidance::nested_message(&nested_file());
        residency
            .activate_nested(&trace, &nested_file(), &message, 1)
            .unwrap();
        residency
            .reference_tool_calls(&trace, &[&outside], 2)
            .unwrap();
        residency
            .reference_text(&trace, "I followed the sub rules.", 2)
            .unwrap();
        assert!(events(&trace, "context.residency.referenced").is_empty());
        residency
            .reference_tool_calls(&trace, &[&inside], 3)
            .unwrap();
        residency
            .reference_text(&trace, "Per sub/AGENTS.md, French.", 4)
            .unwrap();
        let referenced = events(&trace, "context.residency.referenced");
        assert_eq!(referenced.len(), 2);
        assert_eq!(referenced[0]["via"], "tool_call");
        assert_eq!(referenced[0]["path"], "sub/deep/file.rs");
        assert_eq!(referenced[1]["via"], "visible_output");
        assert_eq!(referenced[1]["turn"], 4);
        let items = residency.items(&[LlmMessage::user(&message)], &[], &[String::new()], 5);
        assert_eq!(items[0].last_referenced_turn, Some(4));
        assert_eq!(items[0].activated_turn, 1);
    }

    #[test]
    fn each_compaction_pass_is_a_demotion() {
        let residency = residency(None);
        let compacted = residency.observe(
            "llm.context_assembly.tool_result_compacted",
            &json!({"turn": 2, "llm_call_depth": 1, "message_index": 4, "tool_name": "shell_command",
                    "original_estimated_tokens": 900, "retained_estimated_tokens": 120,
                    "effective_repair_handoff": false}),
        );
        assert_eq!(compacted.len(), 1);
        assert_eq!(compacted[0].0, "context.residency.demoted");
        assert_eq!(compacted[0].1["mechanism"], "tool_result_compaction");
        assert_eq!(compacted[0].1["kind"], "tool_result");
        assert_eq!(compacted[0].1["estimated_tokens_after"], 120);
        let repair = residency.observe(
            "llm.context_assembly.tool_result_compacted",
            &json!({"turn": 2, "llm_call_depth": 1, "effective_repair_handoff": true}),
        );
        assert_eq!(repair[0].1["mechanism"], "repair_packet_compaction");
        let coalesced = residency.observe(
            "llm.context_assembly.file_observations_coalesced",
            &json!({"turn": 3, "llm_call_depth": 0, "epoch": 1,
                    "files": [{"path": "a.rs", "source_read_count": 2}, {"path": "b.rs", "source_read_count": 3}]}),
        );
        assert_eq!(coalesced.len(), 2);
        assert_eq!(coalesced[1].1["path"], "b.rs");
        assert_eq!(coalesced[1].1["mechanism"], "file_observation_coalescing");
        assert_eq!(coalesced[1].1["kind"], "file_observation");
        let superseded = residency.observe(
            "llm.context_assembly.superseded_file_observations_removed",
            &json!({"turn": 3, "llm_call_depth": 0, "removed_projection_count": 2}),
        );
        assert_eq!(
            superseded[0].1["mechanism"],
            "superseded_file_observation_removal"
        );
        assert!(
            residency
                .observe("context.residency.demoted", &json!({}))
                .is_empty()
        );
    }

    #[test]
    fn standing_context_is_measured_against_the_combined_limit() {
        let (_temp, trace) = trace();
        let system = format!("Base.{SECTION_HEADING}{}", "g".repeat(32_000));
        for (tokens, limit, over) in [(Some(262_144), 39_321, false), (None, 4_800, true)] {
            let residency = residency(tokens);
            let mut big = discovery();
            big.files[1].text = "r".repeat(31_000);
            residency.activate_guidance(&trace, &big).unwrap();
            let mut ledger = json!({"estimated_tokens": 8_100, "components": [{"inclusion_reason": "base_system_prompt"}]});
            residency.annotate_ledger(&mut ledger, &[LlmMessage::system(&system)], &[], 1);
            let standing = &ledger["standing_context"];
            assert_eq!(standing["limit_tokens"], limit);
            assert_eq!(
                standing["estimated_tokens"],
                estimate_tokens(32_000 + SECTION_HEADING.len())
            );
            assert_eq!(standing["items"], 2);
            assert_eq!(standing["over_limit"], over);
            assert_eq!(standing["window_assumed"], tokens.is_none());
            assert_eq!(standing["enforced"], false);
            assert_eq!(ledger["residency_settings"]["standing_context_limit"], 0.15);
            assert!(ledger["residency"].as_array().unwrap().len() >= 3);
        }
    }

    #[test]
    fn ledgers_gain_the_known_window_beside_the_configured_one() {
        let residency = residency(Some(1_000));
        let mut ledger = json!({"estimated_tokens": 200, "context_window_tokens": null, "pressure_band": "unknown", "components": []});
        residency.annotate_ledger(&mut ledger, &[], &[], 1);
        assert_eq!(
            ledger["pressure_band"], "unknown",
            "the policy band is unchanged"
        );
        assert_eq!(
            ledger["known_window"],
            json!({"tokens": 1_000, "source": "provider", "utilization": 0.2, "pressure_band": "yellow"})
        );
    }
}
