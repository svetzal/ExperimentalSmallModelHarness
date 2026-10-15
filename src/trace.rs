use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct TraceRecorder {
    path: PathBuf,
    file: Mutex<File>,
    conversation: Option<Arc<crate::conversation::ConversationRun>>,
}

impl TraceRecorder {
    pub fn create(dir: &Path) -> Result<Self> {
        Self::create_for_conversation(dir, None)
    }

    pub(crate) fn create_for_conversation(
        dir: &Path,
        conversation: Option<Arc<crate::conversation::ConversationRun>>,
    ) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating trace directory {}", dir.display()))?;
        let filename = format!("run-{}.jsonl", Utc::now().format("%Y%m%dT%H%M%SZ"));
        let path = dir.join(filename);
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("creating trace file {}", path.display()))?;
        Ok(Self {
            path,
            file: Mutex::new(file),
            conversation,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn event(&self, kind: &str, payload: impl Serialize) -> Result<()> {
        let payload = serde_json::to_value(payload)?;
        self.value_event(kind, payload)
    }

    pub fn value_event(&self, kind: &str, mut payload: Value) -> Result<()> {
        if kind == "llm.context_assembly.ledger"
            && let Some(conversation) = &self.conversation
        {
            conversation.label_ledger(&mut payload);
        }
        let credential_redacted =
            crate::recovery::redact_value(&mut payload, &crate::recovery::credentials());
        let record = TraceEvent {
            timestamp: Utc::now(),
            kind,
            payload,
            credential_redacted: credential_redacted.then_some(true),
            exact: credential_redacted.then_some(false),
        };
        let mut file = self.file.lock().expect("trace mutex poisoned");
        serde_json::to_writer(&mut *file, &record)?;
        file.write_all(b"\n")?;
        file.flush()?;
        if let Some(conversation) = &self.conversation {
            conversation.observe(kind, &record.payload);
        }
        Ok(())
    }

    pub(crate) fn conversation_checkpoint(&self, messages: &[mojentic::llm::models::LlmMessage]) {
        if let Some(conversation) = &self.conversation {
            conversation.checkpoint(messages);
        }
    }

    /// Report a requested tool call to an open conversation view.
    pub(crate) fn tool_requested(&self, call: &mojentic::llm::models::LlmToolCall) {
        if let Some(conversation) = &self.conversation {
            conversation.tool_requested(call);
        }
    }

    pub(crate) fn is_conversation(&self) -> bool {
        self.conversation.is_some()
    }

    pub(crate) fn reply_started(&self) {
        if let Some(conversation) = &self.conversation {
            conversation.emit(crate::conversation::ConversationEvent::ReplyStarted);
        }
    }

    pub(crate) fn stream_content(&self, text: &str) {
        if let Some(conversation) = &self.conversation {
            conversation
                .emit(crate::conversation::ConversationEvent::Content { text: text.into() });
        } else {
            eprint!("{text}");
        }
    }
}

#[derive(Debug, Serialize)]
struct TraceEvent<'a> {
    timestamp: DateTime<Utc>,
    kind: &'a str,
    payload: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential_redacted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exact: Option<bool>,
}

pub fn error_payload(error: &anyhow::Error) -> Value {
    json!({ "error": error.to_string() })
}
