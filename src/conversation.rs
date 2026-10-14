//! TUI-only continuity and presentation. Ordinary runs never create this state.
use crate::agent::{AgentRunConfig, AgentRunSummary, Provider};
use anyhow::{Context, Result, bail};
use mojentic::llm::gateway::ReasoningEffort;
use mojentic::llm::models::{LlmMessage, MessageRole};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

/// Private worker protocol; never emitted by `run` or benchmark adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub(crate) enum ConversationEvent {
    ReplyStarted,
    Content { text: String },
    ToolStarted { name: String },
    Activity { message: String },
    Finished { summary: Value },
    Failed { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionState {
    schema_version: String,
    root: PathBuf,
    provider: Provider,
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
    preserve_reasoning: bool,
    messages: Vec<LlmMessage>,
    last_error: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ConversationRun {
    state: Mutex<SessionState>,
    prompt: String,
    protected_users: usize,
    events: UnboundedSender<ConversationEvent>,
    pub(crate) omitted_exchanges: usize,
    reply_completed: AtomicBool,
}

pub(crate) fn system_guidance() -> String {
    [
        "You are Microgentic, a helpful conversational assistant with coding tools available.",
        "Respond to the user's message in the context of this conversation. No engineering task is assumed.",
        "Greetings, questions, explanations, and discussion can receive ordinary conversational replies without tools or validation.",
        "Perform work only when the user requests it, including clear follow-up authorization in the conversation. Discussed possibilities and quoted examples are not instructions to execute. Ask a concise question if the intended work is unclear.",
        "Use tools when the requested work or answer requires them. Do not inspect the project, create files, change code, or run commands just because this conversation is open.",
        "All file tool paths and shell commands are rooted at the current project directory.",
        "When working on code, inspect relevant files before guessing their contents. Use bounded file reads and focused edits; preserve unrelated work and existing Git stashes.",
        "Use edit_file for existing source edits. Use write_file for new files, or for existing source only after reading the complete current file and preserving unrelated content.",
        "After source changes, obtain fresh deterministic validation before claiming the work is complete. Repair reported failures narrowly and rerun the appropriate check. Earlier conversation observations are not fresh validation.",
        "For requested new projects, use the requested language and layout, and create an appropriate .gitignore. Do not assume Rust or create Cargo files without a request for that project.",
        "Ignore generated build, dependency, cache, and virtual-environment directories unless the request requires them.",
        "Answer naturally with useful results, explanations, or concrete blockers. A visible conversational reply ends the exchange; DONE and FAIL tokens are not required.",
    ].join("\n")
}

pub(crate) fn message_document(message: String) -> String {
    // Conversation examples are data, not declared acceptance probes.
    json!({"guidance": message, "probes": []}).to_string()
}

impl ConversationRun {
    fn open(
        path: &Path,
        config: &AgentRunConfig,
        prompt: String,
        events: UnboundedSender<ConversationEvent>,
    ) -> Result<Self> {
        let root = std::env::current_dir()?.canonicalize()?;
        let parent = path
            .parent()
            .context("session state needs a parent directory")?;
        if parent.canonicalize()?.starts_with(&root) {
            bail!("conversation state must be outside the project directory");
        }
        let mut state = match std::fs::File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                if bytes.len() > 64 * 1024 * 1024 {
                    bail!("conversation state exceeds the 64 MiB limit");
                }
                serde_json::from_slice::<SessionState>(&bytes)
                    .context("reading conversation state")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => SessionState {
                schema_version: "microgentic_conversation.v1".into(),
                root: root.clone(),
                provider: config.provider,
                model: config.model.clone(),
                reasoning_effort: config.reasoning_effort,
                preserve_reasoning: config.preserve_reasoning,
                messages: Vec::new(),
                last_error: None,
            },
            Err(error) => return Err(error.into()),
        };
        if state.schema_version != "microgentic_conversation.v1"
            || state.root != root
            || state.provider != config.provider
            || state.model != config.model
            || state.reasoning_effort != config.reasoning_effort
            || state.preserve_reasoning != config.preserve_reasoning
        {
            bail!(
                "conversation belongs to different project or model settings; start a new TUI session"
            );
        }
        validate_history(&state.messages)?;
        // Reserve half the input allowance for the new task, tools and repairs.
        // Remove complete oldest exchanges only; never split a tool-call batch.
        let allowance = config
            .context_window_tokens
            .unwrap_or(32768)
            .saturating_sub(config.max_tokens.unwrap_or(16384))
            .saturating_mul(4)
            / 2;
        let omitted_exchanges = trim_history(&mut state.messages, allowance);
        let protected_users = state
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .count()
            + 1;
        Ok(Self {
            state: Mutex::new(state),
            prompt,
            protected_users,
            events,
            omitted_exchanges,
            reply_completed: AtomicBool::new(false),
        })
    }

    pub(crate) fn protected_users(&self) -> usize {
        self.protected_users
    }

    pub(crate) fn complete_reply(&self) {
        self.reply_completed.store(true, Ordering::Relaxed);
    }

    pub(crate) fn initial_messages(&self, system: String, mut task: String) -> Vec<LlmMessage> {
        let state = self.state.lock().expect("conversation mutex poisoned");
        if let Some(error) = &state.last_error {
            task.push_str(&format!("\n\n[Microgentic session notice: the previous request stopped: {error}. Only completed responses and tool batches were retained. Inspect current project state before relying on earlier tool observations.]"));
        }
        let mut messages = vec![LlmMessage::system(system)];
        messages.extend(state.messages.clone());
        messages.push(LlmMessage::user(task));
        messages
    }

    /// Save only complete native responses/tool batches, not partial streams or
    /// old task-scoped system/probe/repair instructions. Keep actual user prompts.
    pub(crate) fn checkpoint(&self, messages: &[LlmMessage]) {
        let mut users = 0;
        let history: Vec<_> = messages
            .iter()
            .filter_map(|message| match message.role {
                MessageRole::System => None,
                MessageRole::User => {
                    users += 1;
                    if users > self.protected_users {
                        None
                    } else if users == self.protected_users {
                        Some(LlmMessage::user(&self.prompt))
                    } else {
                        Some(message.clone())
                    }
                }
                _ => Some(message.clone()),
            })
            .collect();
        // A partially completed multi-tool batch must never become replayable.
        if validate_history(&history).is_ok() {
            self.state
                .lock()
                .expect("conversation mutex poisoned")
                .messages = history;
        }
    }

    pub(crate) fn emit(&self, event: ConversationEvent) {
        let _ = self.events.send(event);
    }

    pub(crate) fn observe(&self, kind: &str, payload: &Value) {
        if kind == "llm.response.tool_call.normalized" {
            self.emit(ConversationEvent::ToolStarted {
                name: payload["tool_name"]
                    .as_str()
                    .unwrap_or("project tool")
                    .into(),
            });
            return;
        }
        let message = match kind {
            "llm.stream.thinking" => {
                format!("Thinking · {} characters", payload["total_thinking_chars"])
            }
            "llm.context_assembly.appended" if payload["component"] == "tool_result" => format!(
                "{} {}",
                payload["tool_name"].as_str().unwrap_or("Tool"),
                if payload["ok"] == true {
                    "finished"
                } else {
                    "reported an error"
                }
            ),
            "llm.context_assembly.ledger" => format!(
                "Requesting response · context {}",
                payload["pressure_band"].as_str().unwrap_or("unknown")
            ),
            "llm.stream.progress"
                if payload["accumulated_tool_call_count"].as_u64().unwrap_or(0) > 0 =>
            {
                "Preparing tool request".into()
            }
            "llm.progress.status" => format!(
                "Model progress: {}",
                payload["progress_state"].as_str().unwrap_or("unknown")
            ),
            _ => return,
        };
        self.emit(ConversationEvent::Activity { message });
    }

    pub(crate) fn label_ledger(&self, payload: &mut Value) {
        let mut users = 0;
        if let Some(components) = payload["components"].as_array_mut() {
            for component in components {
                if component["role"] == "user" {
                    users += 1;
                    component["inclusion_reason"] = json!(if users < self.protected_users {
                        "retained_conversation_user_prompt"
                    } else if users == self.protected_users {
                        "authoritative_initial_context_packet"
                    } else {
                        "agent_loop_instruction_or_repair_prompt"
                    });
                }
            }
        }
    }

    fn save(&self, path: &Path, error: Option<String>) -> Result<()> {
        let mut state = self.state.lock().expect("conversation mutex poisoned");
        state.last_error = error;
        let mut file = tempfile::NamedTempFile::new_in(path.parent().context("session parent")?)?;
        serde_json::to_writer(&mut file, &*state)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path).context("saving conversation state")?;
        Ok(())
    }
}

fn validate_history(messages: &[LlmMessage]) -> Result<()> {
    let mut pending = Vec::new();
    for message in messages {
        if message.role == MessageRole::System {
            bail!("conversation cannot retain system instructions");
        }
        if message.role == MessageRole::Tool {
            let calls = message.tool_calls.as_deref().unwrap_or_default();
            if calls.len() != 1 || pending.is_empty() {
                bail!("conversation has an orphaned tool result");
            }
            let expected: &mojentic::llm::models::LlmToolCall = pending.remove(0);
            if calls[0].id != expected.id || calls[0].name != expected.name {
                bail!("conversation tool result does not match its request");
            }
        } else {
            if !pending.is_empty() {
                bail!("conversation has an incomplete tool batch");
            }
            if let Some(calls) = &message.tool_calls {
                if message.role != MessageRole::Assistant && !calls.is_empty() {
                    bail!("conversation tool requests must be assistant messages");
                }
                pending.extend(calls);
            }
        }
    }
    if !pending.is_empty() {
        bail!("conversation has an incomplete tool batch");
    }
    Ok(())
}

fn trim_history(messages: &mut Vec<LlmMessage>, max_chars: usize) -> usize {
    let mut omitted = 0;
    while serde_json::to_vec(messages).map_or(usize::MAX, |v| v.len()) > max_chars
        && !messages.is_empty()
    {
        let end = messages
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, m)| m.role == MessageRole::User)
            .map_or(messages.len(), |(i, _)| i);
        messages.drain(..end);
        omitted += 1;
    }
    omitted
}

pub(crate) async fn run_worker(
    config: AgentRunConfig,
    prompt: String,
    path: PathBuf,
) -> Result<()> {
    let (events, mut receiver) = unbounded_channel();
    let conversation = Arc::new(ConversationRun::open(&path, &config, prompt, events)?);
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(event) = receiver.recv().await {
            let mut line = serde_json::to_vec(&event)?;
            line.push(b'\n');
            stdout.write_all(&line).await?;
            stdout.flush().await?;
        }
        Ok::<_, anyhow::Error>(())
    });
    let result: Result<AgentRunSummary> =
        crate::agent::run_agent_in_conversation(config, conversation.clone()).await;
    let error = result.as_ref().err().map(|e| format!("{e:#}"));
    let continuity_warning = error.clone().or_else(|| {
        result.as_ref().ok().and_then(|summary| {
            (!conversation.reply_completed.load(Ordering::Relaxed)
                && crate::runtime::terminal_token(&summary.final_summary).is_none())
            .then(|| {
                format!(
                    "runtime returned without a terminal task status: {}",
                    summary.final_summary
                )
            })
        })
    });
    let saved = conversation.save(&path, continuity_warning);
    match (&result, &saved) {
        (Ok(summary), Ok(())) => conversation.emit(ConversationEvent::Finished {
            summary: json!(summary),
        }),
        (_, Err(error)) => conversation.emit(ConversationEvent::Failed {
            message: format!("Could not retain conversation: {error:#}"),
        }),
        (Err(_), Ok(())) => conversation.emit(ConversationEvent::Failed {
            message: error.unwrap_or_default(),
        }),
    }
    drop(conversation);
    let written = writer.await.context("waiting for conversation output")?;
    saved?;
    result?;
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use mojentic::llm::models::LlmToolCall;
    use std::collections::HashMap;

    fn conversation(history: Vec<LlmMessage>) -> ConversationRun {
        let protected_users = history
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .count()
            + 1;
        let (events, _) = unbounded_channel();
        ConversationRun {
            state: Mutex::new(SessionState {
                schema_version: "microgentic_conversation.v1".into(),
                root: "/project".into(),
                provider: Provider::Omlx,
                model: "test".into(),
                reasoning_effort: None,
                preserve_reasoning: true,
                messages: history,
                last_error: None,
            }),
            prompt: "new actual prompt".into(),
            protected_users,
            events,
            omitted_exchanges: 0,
            reply_completed: AtomicBool::new(false),
        }
    }

    fn tool_exchange() -> Vec<LlmMessage> {
        let calls = ["first", "second"].map(|id| LlmToolCall {
            id: Some(id.into()),
            name: "read_file".into(),
            arguments: HashMap::new(),
        });
        let mut assistant =
            LlmMessage::assistant("reading").with_reasoning("native completed reasoning");
        assistant.tool_calls = Some(calls.to_vec());
        let mut result = vec![assistant];
        for call in calls {
            result.push(LlmMessage {
                role: MessageRole::Tool,
                content: Some("observed data".into()),
                tool_calls: Some(vec![call]),
                image_paths: None,
                reasoning_content: None,
            });
        }
        result
    }

    #[test]
    fn checkpoint_keeps_native_batches_and_actual_prompts_without_old_task_policy() {
        let session = conversation(vec![
            LlmMessage::user("old actual prompt"),
            LlmMessage::assistant("old answer"),
        ]);
        let mut messages =
            session.initial_messages("fresh system".into(), "fresh task/probe packet".into());
        messages.extend(tool_exchange());
        messages.push(LlmMessage::user("automatic repair directive"));
        messages.push(LlmMessage::assistant("DONE"));
        session.checkpoint(&messages);
        let state = session.state.lock().unwrap();
        assert_eq!(
            state
                .messages
                .iter()
                .filter(|m| m.role == MessageRole::User)
                .count(),
            2
        );
        assert_eq!(
            state.messages[2].content.as_deref(),
            Some("new actual prompt")
        );
        assert_eq!(
            state.messages[3].reasoning_content.as_deref(),
            Some("native completed reasoning")
        );
        assert!(state.messages.iter().all(|m| m.role != MessageRole::System));
        assert!(
            !serde_json::to_string(&state.messages)
                .unwrap()
                .contains("automatic repair directive")
        );
        validate_history(&state.messages).unwrap();
    }

    #[test]
    fn interrupted_tool_batch_cannot_replace_a_complete_checkpoint() {
        let session = conversation(Vec::new());
        let mut messages = session.initial_messages("system".into(), "packet".into());
        session.checkpoint(&messages);
        messages.extend(tool_exchange().into_iter().take(2));
        session.checkpoint(&messages);
        assert_eq!(session.state.lock().unwrap().messages.len(), 1);
        assert!(validate_history(&messages[1..]).is_err());
    }

    #[test]
    fn context_trimming_drops_whole_exchanges_including_tool_pairs() {
        let mut messages = vec![LlmMessage::user("older task")];
        messages.extend(tool_exchange());
        let newest = vec![
            LlmMessage::user("newer task"),
            LlmMessage::assistant("answer"),
        ];
        messages.extend(newest.clone());
        let budget = serde_json::to_vec(&newest).unwrap().len();
        assert_eq!(trim_history(&mut messages, budget), 1);
        assert_eq!(
            serde_json::to_value(&messages).unwrap(),
            serde_json::to_value(&newest).unwrap()
        );
        validate_history(&messages).unwrap();
    }
}
