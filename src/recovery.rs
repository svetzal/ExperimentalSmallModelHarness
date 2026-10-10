//! Worker request recovery, operator admission, and credential-safe persistence.
//! The Mojentic engine owns retries; this module never replays an agent or a tool.
use crate::{sha256::sha256_hex, trace::TraceRecorder};
use anyhow::{Result, bail};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use mojentic::llm::{
    CompletionConfig, LlmGateway,
    gateway::StreamChunk,
    models::{LlmGatewayResponse, LlmMessage},
    recovery::*,
    tools::LlmTool,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::NonZeroU32,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

// The CLI uses a multi-thread runtime. Embedded current-thread callers still work:
// a synchronous Mojentic observer cannot await disk I/O on that runtime.
fn capture_io<T>(operation: impl FnOnce() -> T) -> T {
    if tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
    {
        tokio::task::block_in_place(operation)
    } else {
        operation()
    }
}

#[derive(Debug, Clone, serde::Serialize, clap::Args)]
pub struct RecoverySettings {
    /// Recover eligible wire requests. Never replays model interactions or tools.
    #[arg(long)]
    pub recover_requests: bool,
    /// Wire attempts per completion, including the first; separate from interaction/depth budgets.
    #[arg(long, default_value_t = 1)]
    pub recovery_max_attempts: u32,
    /// Explicit HTTP retry status selection (repeatable); permanent 400/401/403 remain ineligible.
    #[arg(long, value_parser = clap::value_parser!(u16).range(100..600))]
    pub recovery_retryable_status: Vec<u16>,
    /// Initial jitter ceiling in milliseconds.
    #[arg(long, default_value_t = 100)]
    pub recovery_base_delay_ms: u64,
    /// Maximum permissible delay, including Retry-After.
    #[arg(long, default_value_t = 30000)]
    pub recovery_delay_ceiling_ms: u64,
    /// Recovery budget after failure, in milliseconds; never a generation timeout.
    #[arg(long)]
    pub recovery_budget_ms: Option<u64>,
    /// Trusted executable outside the tool root; JSON proof on stdout, metadata on stdin.
    #[arg(long)]
    pub recovery_admission_command: Option<PathBuf>,
    /// Opt-in credential-filtered HTTP bodies outside the tool root; allowed with one attempt and recovery disabled.
    #[arg(long)]
    pub recovery_capture_dir: Option<PathBuf>,
}
impl Default for RecoverySettings {
    fn default() -> Self {
        Self {
            recover_requests: false,
            recovery_max_attempts: 1,
            recovery_retryable_status: Vec::new(),
            recovery_base_delay_ms: 100,
            recovery_delay_ceiling_ms: 30000,
            recovery_budget_ms: None,
            recovery_admission_command: None,
            recovery_capture_dir: None,
        }
    }
}
impl RecoverySettings {
    pub fn validate(&self, tool_root: &std::path::Path) -> Result<()> {
        if self.recovery_max_attempts == 0 {
            bail!("recovery-max-attempts must be positive");
        }
        if !self.recover_requests
            && (self.recovery_max_attempts != 1
                || !self.recovery_retryable_status.is_empty()
                || self.recovery_admission_command.is_some()
                || self.recovery_budget_ms.is_some()
                || self.recovery_base_delay_ms != 100
                || self.recovery_delay_ceiling_ms != 30000)
        {
            bail!(
                "recovery tuning requires --recover-requests; one-attempt capture is allowed independently"
            );
        }
        if self.recovery_base_delay_ms > self.recovery_delay_ceiling_ms
            || self.recovery_budget_ms == Some(0)
        {
            bail!("invalid recovery delay or budget");
        }
        let root = tool_root.canonicalize()?;
        if let Some(command) = &self.recovery_admission_command {
            let command = command.canonicalize()?;
            if !command.is_file() || command.starts_with(&root) {
                bail!("admission executable must be outside the model tool root");
            }
        }
        if let Some(dir) = &self.recovery_capture_dir {
            // Resolve existing ancestors before creating anything, including symlinks.
            let absolute = if dir.is_absolute() {
                dir.clone()
            } else {
                std::env::current_dir()?.join(dir)
            };
            let mut ancestor = absolute.as_path();
            while !ancestor.exists() {
                ancestor = ancestor.parent().ok_or_else(|| {
                    anyhow::anyhow!("capture directory needs an existing ancestor")
                })?;
            }
            let mut resolved = ancestor.canonicalize()?;
            for component in absolute.strip_prefix(ancestor)?.components() {
                match component {
                    std::path::Component::ParentDir => {
                        resolved.pop();
                    }
                    std::path::Component::CurDir => {}
                    _ => resolved.push(component),
                }
            }
            if resolved.starts_with(&root) {
                bail!("capture directory must be outside the model tool root");
            }
        }
        Ok(())
    }
}

pub(crate) fn credentials() -> Vec<String> {
    ["OMLX_API_KEY", "OPENAI_API_KEY", "OLLAMA_API_KEY"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .filter(|s| !s.is_empty())
        .collect()
}
pub(crate) fn redact_value(value: &mut Value, secrets: &[String]) -> bool {
    match value {
        Value::String(s) => {
            let mut changed = false;
            for secret in secrets {
                if s.contains(secret) {
                    *s = s.replace(secret, "[credential-redacted]");
                    changed = true;
                }
            }
            changed
        }
        Value::Array(values) => values
            .iter_mut()
            .fold(false, |changed, v| redact_value(v, secrets) | changed),
        Value::Object(values) => {
            let old = std::mem::take(values);
            let mut changed = false;
            for (mut key, mut v) in old {
                for secret in secrets {
                    if key.contains(secret) {
                        key = key.replace(secret, "[credential-redacted]");
                        changed = true;
                    }
                }
                changed |= redact_value(&mut v, secrets);
                values.insert(key, v);
            }
            changed
        }
        _ => false,
    }
}
fn filtered_bytes(bytes: &[u8], secrets: &[String]) -> (Vec<u8>, bool) {
    let mut result = bytes.to_vec();
    let mut redacted = false;
    for secret in secrets {
        let needle = secret.as_bytes();
        if needle.is_empty() {
            continue;
        }
        let replacement = b"[credential-redacted]";
        let mut cursor = 0;
        while let Some(relative) = result[cursor..]
            .windows(needle.len())
            .position(|w| w == needle)
        {
            let offset = cursor + relative;
            result.splice(offset..offset + needle.len(), replacement.iter().copied());
            cursor = offset + replacement.len();
            redacted = true;
        }
    }
    (result, redacted)
}
// Privacy verification only: decoding JSON strings does not interpret provider
// completion, retry, progress, tool execution, or terminal semantics. Check bytes
// before persistence even when the consumer drops before receiving every delta.
fn encoded_credential(bytes: &[u8], secrets: &[String]) -> bool {
    if filtered_bytes(bytes, secrets).1 {
        return true;
    }
    let mut filter = CredentialFilter::default();
    for line in bytes.split(|byte| *byte == b'\n') {
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        let line = line
            .trim()
            .strip_prefix("data:")
            .unwrap_or(line.trim())
            .trim();
        let Ok(mut value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if redact_value(&mut value, secrets) {
            return true;
        }
        if filter.frame(&value, secrets).is_err() {
            return true;
        }
    }
    if let Ok(mut value) = serde_json::from_slice::<Value>(bytes) {
        return redact_value(&mut value, secrets);
    }
    false
}
// Track each reconstructable field independently, as well as chronological
// cross-field echoes. Only potential prefixes need to survive between deltas.
#[derive(Default)]
struct CredentialFilter {
    prefixes: BTreeMap<String, String>,
    arguments: BTreeMap<String, String>,
    decoded_arguments: BTreeMap<String, String>,
}
impl CredentialFilter {
    fn push(&mut self, channel: &str, text: &str, secrets: &[String]) -> Result<()> {
        // Unrelated fields must not interrupt a reconstructable cross-field echo.
        let mut cross_prefix = String::new();
        for (other, prefix) in &self.prefixes {
            if other == channel || other == "chronological" || prefix.is_empty() {
                continue;
            }
            let combined = format!("{prefix}{text}");
            if secrets
                .iter()
                .filter(|secret| !secret.is_empty())
                .any(|secret| combined.contains(secret))
            {
                bail!("credential contamination across semantic fields");
            }
            let hold = credential_prefix_len(&combined, secrets);
            if hold > cross_prefix.len() {
                cross_prefix = combined[combined.len() - hold..].into();
            }
        }
        for key in [channel, "chronological"] {
            let pending = self.prefixes.entry(key.into()).or_default();
            pending.push_str(text);
            if secrets
                .iter()
                .filter(|secret| !secret.is_empty())
                .any(|secret| pending.contains(secret))
            {
                bail!("credential contamination in semantic output");
            }
            let hold = credential_prefix_len(pending, secrets);
            pending.drain(..pending.len() - hold);
        }
        let pending = self.prefixes.entry(channel.into()).or_default();
        if cross_prefix.len() > pending.len() {
            *pending = cross_prefix;
        }
        Ok(())
    }
    fn frame(&mut self, value: &Value, secrets: &[String]) -> Result<()> {
        if let Some(message) = value.get("message") {
            self.delta(message, secrets)?;
        }
        if let Some(choices) = value["choices"].as_array() {
            for choice in choices {
                self.delta(&choice["delta"], secrets)?;
            }
        }
        Ok(())
    }
    fn delta(&mut self, value: &Value, secrets: &[String]) -> Result<()> {
        for (field, channel) in [
            ("content", "content"),
            ("thinking", "reasoning"),
            ("reasoning_content", "reasoning"),
            ("reasoning", "reasoning"),
        ] {
            if let Some(text) = value[field].as_str() {
                self.push(channel, text, secrets)?;
            }
        }
        if let Some(calls) = value["tool_calls"].as_array() {
            for (position, call) in calls.iter().enumerate() {
                self.tool(call, position, secrets)?;
            }
        }
        Ok(())
    }
    fn tool(&mut self, call: &Value, position: usize, secrets: &[String]) -> Result<()> {
        let index = call["index"].as_u64().unwrap_or(position as u64);
        for (field, value) in [("id", &call["id"]), ("name", &call["function"]["name"])] {
            if let Some(text) = value.as_str() {
                self.push(&format!("tool/{index}/{field}"), text, secrets)?;
            }
        }
        let channel = format!("tool/{index}/arguments");
        match &call["function"]["arguments"] {
            Value::String(text) => {
                self.push(&channel, text, secrets)?;
                let accumulated = self.arguments.entry(channel.clone()).or_default();
                accumulated.push_str(text);
                // Decode complete and unfinished JSON string values for privacy only.
                // This does not interpret tool completion or execute a tool.
                let decoded = json_string_values(accumulated);
                for (index, text) in decoded.into_iter().enumerate() {
                    let decoded_channel = format!("{channel}/decoded/{index}");
                    let previous = self
                        .decoded_arguments
                        .entry(decoded_channel.clone())
                        .or_default();
                    let offset = previous.len();
                    *previous = text.clone();
                    if let Some(delta) = text.get(offset..) {
                        self.push(&decoded_channel, delta, secrets)?;
                    }
                }
            }
            value => self.argument_value(value, &channel, secrets)?,
        }
        Ok(())
    }
    fn argument_value(&mut self, value: &Value, channel: &str, secrets: &[String]) -> Result<()> {
        match value {
            Value::String(text) => self.push(channel, text, secrets)?,
            Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    self.argument_value(value, &format!("{channel}/{index}"), secrets)?;
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    self.argument_value(value, &format!("{channel}/{key}"), secrets)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
fn json_string_values(text: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut start = None;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if let Some(offset) = start {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                if !text[index + 1..].trim_start().starts_with(':')
                    && let Ok(value) = serde_json::from_str::<String>(&text[offset..=index])
                {
                    values.push(value);
                }
                start = None;
            }
        } else if character == '"' {
            start = Some(index);
        }
    }
    if let Some(offset) = start
        && let Ok(value) = serde_json::from_str::<String>(&format!("{}\"", &text[offset..]))
    {
        values.push(value);
    }
    values
}
fn credential_prefix_len(text: &str, secrets: &[String]) -> usize {
    secrets
        .iter()
        .flat_map(|secret| {
            (1..secret.len())
                .filter(|&n| secret.is_char_boundary(n))
                .map(|n| &secret[..n])
        })
        .filter(|prefix| text.ends_with(prefix))
        .map(str::len)
        .max()
        .unwrap_or(0)
}

#[derive(Default)]
struct WireRecord {
    request: Vec<u8>,
    response: Vec<u8>,
    digest: String,
    status: Option<u16>,
    redacted: bool,
    request_redacted: bool,
    identifying_metadata_redacted: bool,
}
#[derive(Default)]
struct EvidenceState {
    trace: Option<Arc<TraceRecorder>>,
    records: BTreeMap<String, WireRecord>,
    wire_attempts: u64,
    persistence_failed: bool,
    final_failure_recorded: bool,
    active_logical_requests: BTreeSet<String>,
}
pub(crate) struct Evidence {
    state: Mutex<EvidenceState>,
    secrets: Vec<String>,
    capture: Option<PathBuf>,
    terminal: tokio::sync::Notify,
}
impl Evidence {
    fn new(capture: Option<PathBuf>) -> Self {
        Self {
            state: Mutex::new(EvidenceState::default()),
            secrets: credentials(),
            capture,
            terminal: tokio::sync::Notify::new(),
        }
    }
    pub(crate) fn bind(&self, trace: Arc<TraceRecorder>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).trace = Some(trace);
    }
    fn event(&self, kind: &str, value: Value) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.final_failure_recorded |= kind == "llm.recovery.final_failure";
        if let Some(trace) = &state.trace
            && trace.event(kind, value).is_err()
        {
            state.persistence_failed = true;
        }
    }
    fn flush(&self, key: &str) -> std::io::Result<()> {
        let Some(dir) = &self.capture else {
            return Ok(());
        };
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(record) = state.records.get(key) else {
            return Ok(());
        };
        std::fs::create_dir_all(dir)?;
        let (request, rq_redacted) = if record.request_redacted {
            (b"[credential-contaminated request withheld]".to_vec(), true)
        } else {
            filtered_bytes(&record.request, &self.secrets)
        };
        let (response, rs_redacted) = if record.redacted {
            (
                b"[credential-contaminated response withheld]".to_vec(),
                true,
            )
        } else {
            filtered_bytes(&record.response, &self.secrets)
        };
        std::fs::write(dir.join(format!("{key}.request.body")), request)?;
        std::fs::write(dir.join(format!("{key}.response.body")), response)?;
        let mut metadata = json!({
            "attempt_id": key,
            "request_sha256": record.digest,
            "http_status": record.status,
            "credential_redacted": rq_redacted || rs_redacted || record.redacted,
            "exact": !(rq_redacted || rs_redacted || record.redacted),
            "headers_captured": false,
            "identifying_metadata_redacted": record.identifying_metadata_redacted,
        });
        if redact_value(&mut metadata, &self.secrets) {
            metadata["credential_redacted"] = json!(true);
            metadata["exact"] = json!(false);
        }
        std::fs::write(
            dir.join(format!("{key}.json")),
            serde_json::to_vec_pretty(&metadata)?,
        )?;
        let pending = dir.join(format!("{key}.pending.json"));
        if pending.exists() {
            std::fs::remove_file(pending)?;
        }
        Ok(())
    }
    fn contamination_detected(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .values()
            .any(|record| record.redacted)
    }
    fn contaminated(&self) {
        for record in self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .values_mut()
        {
            record.redacted = true;
        }
    }
    fn finish(&self) -> std::io::Result<()> {
        let keys: Vec<_> = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .keys()
            .cloned()
            .collect();
        // The synchronous wire-observer contract must not stall the CLI runtime.
        let result = capture_io(|| -> std::io::Result<()> {
            for key in keys {
                self.flush(&key)?;
            }
            Ok(())
        });
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.records.clear();
        state.persistence_failed |= result.is_err();
        result
    }
    fn request_finished(&self, identity: Option<&Identity>) {
        if let Some(identity) = identity {
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active_logical_requests
                .remove(&identity.logical_request_id.to_string());
        }
        self.terminal.notify_one();
    }
    pub(crate) async fn settle(&self) -> Result<()> {
        // This bounds cleanup after the worker has returned and dropped its streams,
        // never the duration of a generation or the existing reasoning controls.
        let drain = async {
            loop {
                let notified = self.terminal.notified();
                if self
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .active_logical_requests
                    .is_empty()
                {
                    break;
                }
                notified.await;
            }
        };
        if tokio::time::timeout(Duration::from_secs(5), drain)
            .await
            .is_err()
        {
            self.event(
                "llm.recovery.final_failure",
                json!({"outcome":"terminal_evidence_drain_failed"}),
            );
            bail!("request cancellation did not retain terminal lifecycle evidence");
        }
        let wire_attempts = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wire_attempts;
        self.event(
            "llm.recovery.accounting",
            json!({"wire_attempts": wire_attempts}),
        );
        if self.finish().is_err() || self.failed() {
            if !self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .final_failure_recorded
            {
                self.event(
                    "llm.recovery.final_failure",
                    json!({"outcome":"observer_failed"}),
                );
            }
            bail!("request evidence persistence failed");
        }
        Ok(())
    }

    fn failed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .persistence_failed
    }
}
impl LifecycleObserver for Evidence {
    fn observe(&self, event: &LifecycleEvent) {
        let mut value = serde_json::to_value(event).unwrap_or(Value::Null);
        if let Some(identity) = &event.identity {
            let redacted = self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .records
                .get(&identity.attempt_id.to_string())
                .is_some_and(|record| record.identifying_metadata_redacted);
            if redacted {
                value["identifying_metadata_redacted"] = json!(true);
            }
        }
        self.event("llm.recovery.lifecycle", value);
        if let Some(identity) = &event.identity {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(
                event.transition,
                Transition::AttemptSucceeded | Transition::AttemptFailed
            ) {
                state.wire_attempts += 1;
            }
            let logical = identity.logical_request_id.to_string();
            match event.transition {
                Transition::AttemptStarted => {
                    state.active_logical_requests.insert(logical);
                }
                Transition::AttemptSucceeded
                | Transition::Cancelled
                | Transition::Interrupted
                | Transition::Exhausted
                | Transition::AdmissionRequired
                | Transition::AdmissionRejected => {
                    state.active_logical_requests.remove(&logical);
                    self.terminal.notify_one();
                }
                _ => {}
            }
        }
    }
}
impl WireObserver for Evidence {
    fn observe(
        &self,
        event: WireEvent<'_>,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.failed() {
            return Err("recovery evidence persistence failed".into());
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (key, contaminated) = match event {
            WireEvent::Request { identity, request } => {
                let bytes = request
                    .body()
                    .and_then(|b| b.as_bytes())
                    .ok_or("encoded request body unavailable")?;
                let key = identity.attempt_id.to_string();
                let (_, mut redacted) = filtered_bytes(bytes, &self.secrets);
                // JSON escaping must not disguise a credential in the encoded request.
                if let Ok(mut body) = serde_json::from_slice::<Value>(bytes) {
                    redacted |= redact_value(&mut body, &self.secrets);
                }
                state.records.insert(
                    key.clone(),
                    WireRecord {
                        request: bytes.to_vec(),
                        digest: sha256_hex(bytes),
                        redacted,
                        request_redacted: redacted,
                        ..Default::default()
                    },
                );
                (key, redacted)
            }
            WireEvent::Headers {
                identity,
                status,
                headers,
            } => {
                let key = identity.attempt_id.to_string();
                if let Some(record) = state.records.get_mut(&key) {
                    record.status = Some(status);
                    record.identifying_metadata_redacted = headers
                        .get("x-request-id")
                        .and_then(|h| h.to_str().ok())
                        .is_some_and(|id| self.secrets.iter().any(|s| id.contains(s)));
                }
                (key, false)
            }
            WireEvent::Body { identity, bytes } => {
                let key = identity.attempt_id.to_string();
                let record = state
                    .records
                    .get_mut(&key)
                    .ok_or("missing request evidence")?;
                record.response.extend_from_slice(bytes);
                let redacted = encoded_credential(&record.response, &self.secrets);
                record.redacted |= redacted;
                (key, redacted)
            }
        };
        drop(state);
        // Validate the destination at the wire boundary, including before dispatch.
        if let Some(dir) = &self.capture {
            // Mojentic's observer is synchronous; yield the runtime thread for disk I/O.
            capture_io(|| -> std::io::Result<()> {
                std::fs::create_dir_all(dir)?;
                std::fs::write(
                    dir.join(format!("{key}.pending.json")),
                    b"{\"exact\":false,\"pending\":true}",
                )
            })?;
        }
        if contaminated {
            return Err("credential contamination at HTTP boundary".into());
        }
        Ok(())
    }
}
struct OperatorAdmission {
    command: PathBuf,
    evidence: Arc<Evidence>,
}
#[async_trait]
impl Admission for OperatorAdmission {
    async fn decide(&self, context: AdmissionContext) -> AdmissionDecision {
        let key = context.previous_identity.attempt_id.to_string();
        let digest = self
            .evidence
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .get(&key)
            .filter(|r| !r.identifying_metadata_redacted)
            .map(|r| r.digest.clone());
        let Some(digest) = digest else {
            return AdmissionDecision::Reject;
        };
        let mut input = json!({"failure": context.failure, "request_sha256": digest});
        // Removing identifying evidence invalidates admission, even if it was a UUID.
        if redact_value(&mut input, &self.evidence.secrets) {
            return AdmissionDecision::Reject;
        }
        let proof = async {
            let mut child = Command::new(&self.command)
                .kill_on_drop(true)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let mut stdin = child.stdin.take()?;
            stdin
                .write_all(&serde_json::to_vec(&input).ok()?)
                .await
                .ok()?;
            stdin.shutdown().await.ok()?;
            drop(stdin);
            let mut stdout = child.stdout.take()?;
            let mut bytes = Vec::new();
            (&mut stdout)
                .take(16_385)
                .read_to_end(&mut bytes)
                .await
                .ok()?;
            if bytes.len() > 16_384 || !child.wait().await.ok()?.success() {
                return None;
            }
            serde_json::from_slice::<Value>(&bytes).ok()
        };
        // Dropping either timeout or Mojentic's cancellable admission future kills our child.
        let Ok(Some(mut proof)) = tokio::time::timeout(Duration::from_secs(5), proof).await else {
            return AdmissionDecision::Reject;
        };
        if redact_value(&mut proof, &self.evidence.secrets) {
            return AdmissionDecision::Reject;
        }
        let identity = &context.previous_identity;
        let matches = proof["decision"] == "allow"
            && proof["previous_attempt_id"] == identity.attempt_id.to_string()
            && proof["logical_request_id"] == identity.logical_request_id.to_string()
            && proof["request_sha256"] == digest
            && proof["provider_request_id"] == input["failure"]["provider_request_id"]
            && matches!(
                proof["evidence_kind"].as_str(),
                Some("not_forwarded" | "exact_request_terminated")
            );
        if matches {
            AdmissionDecision::Allow
        } else {
            AdmissionDecision::Reject
        }
    }
}

pub(crate) struct WorkerGateway<G> {
    inner: RecoveryGateway<G>,
    evidence: Arc<Evidence>,
    call: RecoveryCall,
}
impl<G: RecoveryProvider> WorkerGateway<G> {
    pub(crate) fn new(
        inner: G,
        settings: &RecoverySettings,
        call: RecoveryCall,
        evidence: Arc<Evidence>,
    ) -> Result<Self> {
        let mut policy = RecoveryPolicy {
            max_attempts: NonZeroU32::new(settings.recovery_max_attempts)
                .ok_or_else(|| anyhow::anyhow!("invalid attempt limit"))?,
            base_delay: Duration::from_millis(settings.recovery_base_delay_ms),
            delay_ceiling: Duration::from_millis(settings.recovery_delay_ceiling_ms),
            budget: settings.recovery_budget_ms.map(Duration::from_millis),
            observer: Some(evidence.clone()),
            wire_observer: Some(evidence.clone()),
            ..Default::default()
        };
        if !settings.recovery_retryable_status.is_empty() {
            policy.retryable_statuses =
                settings.recovery_retryable_status.iter().copied().collect();
        }
        if let Some(command) = &settings.recovery_admission_command {
            policy.admission = Some(Arc::new(OperatorAdmission {
                command: command.canonicalize()?,
                evidence: evidence.clone(),
            }));
        }
        Ok(Self {
            inner: RecoveryGateway::new(inner, policy)?,
            evidence,
            call,
        })
    }
}
impl<G: RecoveryProvider> WorkerGateway<G> {
    fn finish_completion<T: serde::Serialize>(
        &self,
        result: std::result::Result<Recovered<LlmGatewayResponse<T>>, RecoveryError>,
    ) -> mojentic::error::Result<LlmGatewayResponse<T>> {
        match result {
            Ok(response) => {
                self.evidence
                    .request_finished(Some(&response.report.identity));
                let mut payload = json!({"content": response.value.content, "thinking": response.value.thinking, "tool_calls": response.value.tool_calls, "object": response.value.object, "metadata": response.value.evidence.metadata});
                let contaminated = redact_value(&mut payload, &self.evidence.secrets);
                if contaminated {
                    self.evidence.contaminated();
                }
                if self.evidence.finish().is_err()
                    || self.evidence.failed()
                    || contaminated
                    || self.call.cancellation.is_cancelled()
                {
                    self.evidence.event("llm.recovery.final_failure", json!({"outcome": if contaminated { "interrupted" } else if self.call.cancellation.is_cancelled() { "cancelled" } else { "observer_failed" }, "report": response.report, "credential_redacted": contaminated}));
                    return Err(mojentic::error::MojenticError::GatewayError(
                        "safe worker completion rejected".into(),
                    ));
                }
                Ok(response.value)
            }
            Err(error) => {
                self.evidence
                    .request_finished(error.failure.identity.as_ref().or_else(|| {
                        error
                            .history
                            .last()
                            .and_then(|failure| failure.identity.as_ref())
                    }));
                self.evidence
                    .event("llm.recovery.final_failure", json!({"error": error}));
                if self.evidence.finish().is_err() {
                    self.evidence.event(
                        "llm.recovery.capture_failure",
                        json!({"outcome":"observer_failed"}),
                    );
                }
                Err(mojentic::error::MojenticError::Recovery(Box::new(error)))
            }
        }
    }
}

pub(crate) fn evidence(settings: &RecoverySettings) -> Arc<Evidence> {
    Arc::new(Evidence::new(settings.recovery_capture_dir.clone()))
}

// Preserve stream order by holding the queue from its earliest unsafe prefix.
// A delta in another channel cannot disprove a prefix in the original channel.
#[derive(Default)]
struct SemanticFilter {
    pending: VecDeque<(bool, String)>,
    credentials: CredentialFilter,
}
impl SemanticFilter {
    fn push(
        &mut self,
        reasoning: bool,
        text: String,
        secrets: &[String],
    ) -> Result<Vec<StreamChunk>> {
        let channel = if reasoning { "reasoning" } else { "content" };
        if let Err(error) = self.credentials.push(channel, &text, secrets) {
            self.pending.clear();
            return Err(error);
        }
        self.pending.push_back((reasoning, text));
        let combined: String = self.pending.iter().map(|(_, text)| text.as_str()).collect();
        let mut safe = combined.len() - credential_prefix_len(&combined, secrets);
        for channel in [false, true] {
            let projected: String = self
                .pending
                .iter()
                .filter(|(reasoning, _)| *reasoning == channel)
                .map(|(_, text)| text.as_str())
                .collect();
            let mut hold = credential_prefix_len(&projected, secrets);
            let mut offset = combined.len();
            for (reasoning, text) in self.pending.iter().rev() {
                offset -= text.len();
                if *reasoning == channel && hold > 0 {
                    let take = hold.min(text.len());
                    safe = safe.min(offset + text.len() - take);
                    hold -= take;
                }
            }
        }
        Ok(self.release(safe))
    }
    fn release(&mut self, mut bytes: usize) -> Vec<StreamChunk> {
        let mut output = Vec::new();
        while bytes > 0 {
            let Some((reasoning, text)) = self.pending.pop_front() else {
                break;
            };
            let take = bytes.min(text.len());
            let (send, keep) = text.split_at(take);
            if !send.is_empty() {
                output.push(if reasoning {
                    StreamChunk::Thinking(send.into())
                } else {
                    StreamChunk::Content(send.into())
                });
            }
            if !keep.is_empty() {
                self.pending.push_front((reasoning, keep.into()));
            }
            bytes -= take;
        }
        output
    }
}
#[async_trait]
impl<G: RecoveryProvider> LlmGateway for WorkerGateway<G> {
    async fn complete(
        &self,
        model: &str,
        messages: &[LlmMessage],
        tools: Option<&[Box<dyn LlmTool>]>,
        config: &CompletionConfig,
    ) -> mojentic::error::Result<LlmGatewayResponse> {
        self.finish_completion(
            self.inner
                .complete_with_recovery(model, messages, tools, config, &self.call)
                .await,
        )
    }
    async fn complete_json(
        &self,
        model: &str,
        messages: &[LlmMessage],
        schema: Value,
        config: &CompletionConfig,
    ) -> mojentic::error::Result<Value> {
        Ok(self
            .complete_json_response(model, messages, schema, config)
            .await?
            .object
            .unwrap_or(Value::Null))
    }
    async fn complete_json_response(
        &self,
        model: &str,
        messages: &[LlmMessage],
        schema: Value,
        config: &CompletionConfig,
    ) -> mojentic::error::Result<LlmGatewayResponse<Value>> {
        self.finish_completion(
            self.inner
                .complete_json_with_recovery(model, messages, schema, config, &self.call)
                .await,
        )
    }
    async fn get_available_models(&self) -> mojentic::error::Result<Vec<String>> {
        self.inner.get_available_models().await
    }
    async fn calculate_embeddings(
        &self,
        text: &str,
        model: Option<&str>,
    ) -> mojentic::error::Result<Vec<f32>> {
        self.inner.calculate_embeddings(text, model).await
    }
    fn complete_stream<'a>(
        &'a self,
        model: &'a str,
        messages: &'a [LlmMessage],
        tools: Option<&'a [Box<dyn LlmTool>]>,
        config: &'a CompletionConfig,
    ) -> Pin<Box<dyn Stream<Item = mojentic::error::Result<StreamChunk>> + Send + 'a>> {
        let stream = self
            .inner
            .complete_stream_with_recovery(model, messages, tools, config, &self.call);
        let evidence = self.evidence.clone();
        let cancellation = self.call.cancellation.clone();
        Box::pin(futures::stream::unfold(
            (
                stream,
                SemanticFilter::default(),
                VecDeque::new(),
                false,
                false,
            ),
            move |(mut stream, mut filter, mut ready, mut done, mut contaminated)| {
                let evidence = evidence.clone();
                let cancellation = cancellation.clone();
                async move {
                    loop {
                        if let Some(chunk) = ready.pop_front() {
                            return Some((chunk, (stream, filter, ready, done, contaminated)));
                        }
                        if done {
                            return None;
                        }
                        match stream.next().await {
                            Some(RecoveryStreamEvent::Content(text))
                            | Some(RecoveryStreamEvent::Reasoning(text))
                                if contaminated =>
                            {
                                let _ = text;
                            }
                            Some(RecoveryStreamEvent::Content(text)) => {
                                match filter.push(false, text, &evidence.secrets) {
                                    Ok(chunks) => ready.extend(chunks.into_iter().map(Ok)),
                                    Err(_) => {
                                        contaminated = true;
                                        evidence.contaminated();
                                        cancellation.cancel();
                                    }
                                }
                            }
                            Some(RecoveryStreamEvent::Reasoning(text)) => {
                                match filter.push(true, text, &evidence.secrets) {
                                    Ok(chunks) => ready.extend(chunks.into_iter().map(Ok)),
                                    Err(_) => {
                                        contaminated = true;
                                        evidence.contaminated();
                                        cancellation.cancel();
                                    }
                                }
                            }
                            Some(RecoveryStreamEvent::Progress { progress, .. }) => {
                                ready.push_back(Ok(StreamChunk::Progress(progress)))
                            }
                            Some(RecoveryStreamEvent::Metrics { metrics, .. }) => {
                                ready.push_back(Ok(StreamChunk::Metrics(metrics)))
                            }
                            Some(RecoveryStreamEvent::ToolFragment(value)) => {
                                if !contaminated
                                    && filter
                                        .credentials
                                        .tool(&value, 0, &evidence.secrets)
                                        .is_err()
                                {
                                    contaminated = true;
                                    filter.pending.clear();
                                    evidence.contaminated();
                                    cancellation.cancel();
                                }
                            }
                            Some(RecoveryStreamEvent::Completed(response)) => {
                                done = true;
                                let mut tool_json =
                                    serde_json::to_value(&response.value.tool_calls)
                                        .unwrap_or(Value::Null);
                                contaminated |= redact_value(&mut tool_json, &evidence.secrets);
                                if contaminated {
                                    evidence.contaminated();
                                }
                                let capture_failed = evidence.finish().is_err();
                                if contaminated
                                    || capture_failed
                                    || evidence.failed()
                                    || cancellation.is_cancelled()
                                {
                                    evidence.event("llm.recovery.final_failure", json!({"outcome": if contaminated { "interrupted" } else if cancellation.is_cancelled() { "cancelled" } else { "observer_failed" }, "report": response.report, "credential_redacted": contaminated}));
                                    ready.push_back(Err(
                                        mojentic::error::MojenticError::GatewayError(
                                            "safe worker completion rejected".into(),
                                        ),
                                    ));
                                } else {
                                    ready.extend(filter.release(usize::MAX).into_iter().map(Ok));
                                    if !response.value.tool_calls.is_empty() {
                                        ready.push_back(Ok(StreamChunk::ToolCalls(
                                            response.value.tool_calls,
                                        )));
                                    }
                                }
                            }
                            Some(RecoveryStreamEvent::Failed(error)) => {
                                done = true;
                                contaminated |= evidence.contamination_detected();
                                evidence.request_finished(error.failure.identity.as_ref().or_else(
                                    || {
                                        error
                                            .history
                                            .last()
                                            .and_then(|failure| failure.identity.as_ref())
                                    },
                                ));
                                evidence.event(
                                    "llm.recovery.final_failure",
                                    json!({"error": error, "credential_redacted": contaminated}),
                                );
                                if evidence.finish().is_err() {
                                    evidence.event(
                                        "llm.recovery.capture_failure",
                                        json!({"outcome":"observer_failed"}),
                                    );
                                }
                                // A possible credential prefix is withheld even on interruption.
                                ready.push_back(Err(mojentic::error::MojenticError::Recovery(
                                    Box::new(error),
                                )));
                            }
                            None => {
                                done = true;
                            }
                        }
                    }
                }
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_credentials_split_across_reasoning_and_content_are_withheld() {
        let secret = "11111111-2222-4333-8444-555555555555".to_string();
        let secrets = vec![secret.clone()];
        let mut filter = SemanticFilter::default();
        let harmless = filter
            .push(true, format!("safe: {}", &secret[..12]), &secrets)
            .unwrap();
        assert!(matches!(&harmless[..], [StreamChunk::Thinking(text)] if text == "safe: "));
        assert!(
            filter
                .push(false, secret[12..24].into(), &secrets)
                .unwrap()
                .is_empty()
        );
        assert!(filter.push(true, secret[24..].into(), &secrets).is_err());
        assert!(filter.release(usize::MAX).is_empty());
    }

    #[test]
    fn ordinary_stream_payloads_and_utf8_are_preserved_after_prefix_mismatch() {
        let mut filter = SemanticFilter::default();
        let secrets = vec!["abcdef".into()];
        assert!(
            filter
                .push(true, "abc".into(), &secrets)
                .unwrap()
                .is_empty()
        );
        assert!(
            filter
                .push(false, "界 normal".into(), &secrets)
                .unwrap()
                .is_empty()
        );
        let released = filter.push(true, " mismatch".into(), &secrets).unwrap();
        assert!(
            matches!(&released[..], [StreamChunk::Thinking(thinking), StreamChunk::Content(content), StreamChunk::Thinking(last)] if thinking == "abc" && content == "界 normal" && last == " mismatch")
        );
    }

    #[test]
    fn unrelated_channel_cannot_release_a_held_credential_prefix() {
        let secrets = vec!["abcdef".into()];
        for reasoning in [false, true] {
            let mut filter = SemanticFilter::default();
            assert!(
                filter
                    .push(reasoning, "abc".into(), &secrets)
                    .unwrap()
                    .is_empty()
            );
            assert!(
                filter
                    .push(!reasoning, "harmless".into(), &secrets)
                    .unwrap()
                    .is_empty()
            );
            assert!(filter.push(reasoning, "def".into(), &secrets).is_err());
            assert!(filter.release(usize::MAX).is_empty());
        }
    }

    #[test]
    fn partial_tool_fields_are_checked_without_completed_calls() {
        let secrets = vec!["abcdef".into()];
        for field in ["id", "name", "arguments"] {
            let mut filter = CredentialFilter::default();
            for (piece, contaminated) in [("abc", false), ("def", true)] {
                let mut call = json!({"index": 2, "function": {}});
                if field == "id" {
                    call["id"] = json!(piece);
                } else {
                    call["function"][field] = json!(piece);
                }
                assert_eq!(filter.tool(&call, 0, &secrets).is_err(), contaminated);
                if !contaminated {
                    filter.push("content", "harmless", &secrets).unwrap();
                    filter
                        .tool(
                            &json!({"index": 3, "function":{"arguments":"unrelated"}}),
                            0,
                            &secrets,
                        )
                        .unwrap();
                }
            }
        }
        let mut filter = CredentialFilter::default();
        filter
            .tool(
                &json!({"function":{"name":"shell_command","arguments":{"command":"abc"}}}),
                0,
                &secrets,
            )
            .unwrap();
        filter.push("content", "harmless", &secrets).unwrap();
        assert!(
            filter
                .tool(
                    &json!({"function":{"name":"shell_command","arguments":{"command":"def"}}}),
                    0,
                    &secrets
                )
                .is_err()
        );
    }

    #[test]
    fn escaped_unfinished_tool_arguments_and_cross_field_echoes_are_detected() {
        let secrets = vec!["abcdef".into()];
        let mut filter = CredentialFilter::default();
        filter
            .tool(
                &json!({"index":0,"function":{"arguments":r#"{"command":"\u0061\u0062\u0063"#}}),
                0,
                &secrets,
            )
            .unwrap();
        filter.push("content", "harmless", &secrets).unwrap();
        assert!(
            filter
                .tool(
                    &json!({"index":0,"function":{"arguments":r#"\u0064\u0065\u0066"#}}),
                    0,
                    &secrets
                )
                .is_err()
        );
        let mut filter = CredentialFilter::default();
        filter.push("reasoning", "ab", &secrets).unwrap();
        filter
            .tool(
                &json!({"index":0,"id":"cd","function":{"name":"unrelated"}}),
                0,
                &secrets,
            )
            .unwrap();
        assert!(
            filter
                .tool(
                    &json!({"index":0,"function":{"arguments":{"command":"ef"}}}),
                    0,
                    &secrets
                )
                .is_err()
        );
    }

    #[test]
    fn raw_captures_detect_channel_projection_and_tool_fragments() {
        let secrets = vec!["abcdef".into()];
        for provider in ["ollama", "omlx"] {
            for field in ["reasoning", "content", "arguments", "name", "id"] {
                let frames: Vec<_> = ["abc", "harmless", "def"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, text)| {
                        let mut delta = json!({});
                        if index == 1 {
                            let key = if field == "content" {
                                if provider == "ollama" {
                                    "thinking"
                                } else {
                                    "reasoning_content"
                                }
                            } else {
                                "content"
                            };
                            delta[key] = json!(text);
                        } else if field == "reasoning" || field == "content" {
                            let key = if field == "reasoning" {
                                if provider == "ollama" {
                                    "thinking"
                                } else {
                                    "reasoning_content"
                                }
                            } else {
                                "content"
                            };
                            delta[key] = json!(text);
                        } else {
                            let mut call = json!({"index":0,"function":{}});
                            if field == "id" {
                                call["id"] = json!(text);
                            } else {
                                call["function"][field] = json!(text);
                            }
                            delta["tool_calls"] = json!([call]);
                        }
                        if provider == "ollama" {
                            json!({"message":delta}).to_string()
                        } else {
                            format!("data: {}", json!({"choices":[{"delta":delta}]}))
                        }
                    })
                    .collect();
                let frames = frames.join("\n");
                assert!(
                    encoded_credential(frames.as_bytes(), &secrets),
                    "{provider}/{field}"
                );
            }
        }
    }

    #[test]
    fn active_credentials_are_redacted_from_metadata_values_and_keys() {
        let secrets = vec!["11111111-2222-4333-8444-555555555555".to_string()];
        let mut metadata = json!({secrets[0].clone(): [{"request_id": secrets[0]}]});
        assert!(redact_value(&mut metadata, &secrets));
        assert!(!metadata.to_string().contains(&secrets[0]));
    }
    #[test]
    fn encoded_privacy_checks_cover_unconsumed_semantic_deltas_and_escaped_errors() {
        let secret = "11111111-2222-4333-8444-555555555555".to_string();
        let frames = format!(
            "{}\n{}\n{}\n",
            json!({"message":{"thinking":&secret[..12]}}),
            json!({"message":{"content":&secret[12..24]}}),
            json!({"message":{"thinking":&secret[24..]}})
        );
        assert!(encoded_credential(frames.as_bytes(), &[secret]));
        let secret = "synthetic-secret\"/unsafe".to_string();
        let error = serde_json::to_vec(&json!({"error":secret})).unwrap();
        assert!(encoded_credential(&error, &[secret]));
        assert!(!encoded_credential(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"normal\"}}]}\n\n",
            &["unrelated-secret".into()]
        ));
    }
    #[test]
    fn redaction_does_not_rescan_its_own_replacement() {
        let (body, redacted) = filtered_bytes(b"credential-credential", &["credential".into()]);
        assert!(redacted);
        assert_eq!(body, b"[credential-redacted]-[credential-redacted]");
    }
}
