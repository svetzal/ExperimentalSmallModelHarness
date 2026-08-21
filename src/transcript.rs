use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const TEMPLATE: &str = include_str!("transcript_template.html");
const EVIDENCE_SCHEMA: &str = "transcript_evidence.v1";

#[derive(Debug, Clone)]
pub struct RenderTranscriptConfig {
    pub inputs: Vec<PathBuf>,
    pub output: PathBuf,
    pub title: String,
    pub evidence_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderTranscriptSummary {
    pub output: PathBuf,
    pub session_count: usize,
    pub model_call_count: usize,
    pub exact_request_snapshot_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct EvidenceFile {
    schema_version: String,
    #[serde(default)]
    sessions: Vec<IndependentEvidence>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct IndependentEvidence {
    #[serde(default)]
    trace: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    reward: Option<String>,
    #[serde(default)]
    passed: usize,
    #[serde(default)]
    failed: usize,
    #[serde(default)]
    output: String,
}

#[derive(Debug, Clone, Serialize)]
struct TranscriptDocument<'a> {
    title: &'a str,
    sessions: &'a [Session],
}

#[derive(Debug, Clone, Serialize)]
struct Session {
    id: String,
    trace: String,
    model: Option<String>,
    profile: Option<String>,
    runtime_seconds: Option<f64>,
    turns: usize,
    calls: Vec<ModelCall>,
    reasoning_chars: usize,
    request_snapshot_count: usize,
    exact_request_trace: bool,
    final_summary: String,
    hard_stop: Option<EventCard>,
    evidence: IndependentEvidence,
    legacy_initial_user_message: Option<String>,
    fidelity_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ModelCall {
    index: usize,
    turn: usize,
    depth: usize,
    at: Option<f64>,
    context: Value,
    request: Option<Value>,
    request_messages: Vec<Value>,
    reasoning: String,
    reasoning_reported_chars: usize,
    reasoning_complete: bool,
    assistant: String,
    assistant_reported_chars: usize,
    assistant_complete: bool,
    response: Value,
    response_tool_calls: Vec<Value>,
    tools: Vec<ToolCard>,
    harness: Vec<EventCard>,
}

#[derive(Debug, Clone, Serialize)]
struct ToolCard {
    kind: String,
    name: String,
    at: Option<f64>,
    summary: String,
    success: Option<bool>,
    payload: Value,
}

#[derive(Debug, Clone, Serialize)]
struct EventCard {
    kind: String,
    at: Option<f64>,
    title: String,
    summary: String,
    severity: &'static str,
    payload: Value,
}

#[derive(Debug)]
struct TraceRecord {
    timestamp: Option<DateTime<Utc>>,
    kind: String,
    payload: Value,
}

pub fn render_transcript(config: RenderTranscriptConfig) -> Result<RenderTranscriptSummary> {
    if config.inputs.is_empty() {
        bail!("at least one trace file or directory is required");
    }
    let evidence = load_evidence(config.evidence_file.as_deref())?;
    let trace_files = discover_trace_files(&config.inputs)?;
    if trace_files.is_empty() {
        bail!("no harness JSONL traces found");
    }

    let mut sessions = Vec::new();
    for path in trace_files {
        if let Some(mut session) = parse_session(&path)? {
            session.evidence = match_evidence(&evidence, &path, &session.id);
            sessions.push(session);
        }
    }
    if sessions.is_empty() {
        bail!("no input contained a run.started harness event");
    }
    sessions.sort_by(|left, right| left.id.cmp(&right.id));

    let data = serde_json::to_string(&TranscriptDocument {
        title: &config.title,
        sessions: &sessions,
    })?
    .replace("</", "<\\/");
    let html = TEMPLATE
        .replace("__TRANSCRIPT_TITLE__", &escape_html(&config.title))
        .replace("__TRANSCRIPT_DATA__", &data);
    if let Some(parent) = config
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating transcript directory {}", parent.display()))?;
    }
    std::fs::write(&config.output, html)
        .with_context(|| format!("writing transcript {}", config.output.display()))?;

    Ok(RenderTranscriptSummary {
        output: config.output,
        session_count: sessions.len(),
        model_call_count: sessions.iter().map(|session| session.calls.len()).sum(),
        exact_request_snapshot_count: sessions
            .iter()
            .map(|session| session.request_snapshot_count)
            .sum(),
    })
}

fn discover_trace_files(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut traces = BTreeSet::new();
    for input in inputs {
        if input.is_file() {
            traces.insert(
                input
                    .canonicalize()
                    .with_context(|| format!("canonicalizing trace file {}", input.display()))?,
            );
            continue;
        }
        if !input.is_dir() {
            bail!("trace input does not exist: {}", input.display());
        }
        for entry in WalkBuilder::new(input).hidden(false).build() {
            let entry = entry.with_context(|| format!("walking {}", input.display()))?;
            let path = entry.path();
            if entry.file_type().is_some_and(|kind| kind.is_file())
                && path.extension().and_then(|value| value.to_str()) == Some("jsonl")
            {
                traces
                    .insert(path.canonicalize().with_context(|| {
                        format!("canonicalizing trace file {}", path.display())
                    })?);
            }
        }
    }
    Ok(traces.into_iter().collect())
}

fn load_evidence(path: Option<&Path>) -> Result<Vec<IndependentEvidence>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading transcript evidence {}", path.display()))?;
    let evidence: EvidenceFile = serde_json::from_str(&text)
        .with_context(|| format!("decoding transcript evidence {}", path.display()))?;
    if evidence.schema_version != EVIDENCE_SCHEMA {
        bail!(
            "unsupported transcript evidence schema {:?}; expected {EVIDENCE_SCHEMA}",
            evidence.schema_version
        );
    }
    for item in &evidence.sessions {
        if item.trace.is_none() && item.label.is_none() {
            bail!("each transcript evidence entry requires trace or label");
        }
    }
    Ok(evidence.sessions)
}

fn match_evidence(items: &[IndependentEvidence], trace: &Path, label: &str) -> IndependentEvidence {
    let trace_text = trace.display().to_string();
    items
        .iter()
        .find(|item| {
            item.trace.as_deref() == Some(trace_text.as_str())
                || item.label.as_deref() == Some(label)
        })
        .cloned()
        .unwrap_or_else(|| IndependentEvidence {
            output: "No independent evidence supplied for this session.".to_string(),
            ..Default::default()
        })
}

fn parse_session(path: &Path) -> Result<Option<Session>> {
    let records = read_records(path)?;
    if !records.iter().any(|record| record.kind == "run.started") {
        return Ok(None);
    }
    let started = records.first().and_then(|record| record.timestamp);
    let last = records.last().and_then(|record| record.timestamp);
    let mut run_started = Value::Null;
    let mut profile = None;
    let mut legacy_initial_user_message = None;
    let mut calls: Vec<ModelCall> = Vec::new();
    let mut current_call = None;
    let mut final_summary = String::new();
    let mut hard_stop = None;

    for record in records {
        let at = elapsed_seconds(started, record.timestamp);
        match record.kind.as_str() {
            "run.started" => run_started = record.payload.clone(),
            "agent.contract.resolved" => {
                profile = record
                    .payload
                    .pointer("/resolved/profile/id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "initial_context.assembled" => {
                legacy_initial_user_message = record
                    .payload
                    .get("worker_message")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "llm.context_assembly.ledger" => {
                let call = ModelCall {
                    index: calls.len() + 1,
                    turn: value_usize(&record.payload, "turn"),
                    depth: value_usize(&record.payload, "llm_call_depth"),
                    at,
                    context: record.payload.clone(),
                    request: None,
                    request_messages: Vec::new(),
                    reasoning: String::new(),
                    reasoning_reported_chars: 0,
                    reasoning_complete: true,
                    assistant: String::new(),
                    assistant_reported_chars: 0,
                    assistant_complete: true,
                    response: Value::Null,
                    response_tool_calls: Vec::new(),
                    tools: Vec::new(),
                    harness: Vec::new(),
                };
                calls.push(call);
                current_call = Some(calls.len() - 1);
            }
            "llm.provider_request.assembled" => {
                if let Some(index) = current_call
                    && value_usize(&record.payload, "turn") == calls[index].turn
                    && value_usize(&record.payload, "llm_call_depth") == calls[index].depth
                {
                    calls[index].request = Some(record.payload.clone());
                }
            }
            "llm.stream.thinking" => {
                if let Some(index) = current_call {
                    let call = &mut calls[index];
                    append_stream_text(
                        &mut call.reasoning,
                        &mut call.reasoning_reported_chars,
                        &mut call.reasoning_complete,
                        &record.payload,
                    );
                }
            }
            "llm.stream.content" => {
                if let Some(index) = current_call {
                    let call = &mut calls[index];
                    append_stream_text(
                        &mut call.assistant,
                        &mut call.assistant_reported_chars,
                        &mut call.assistant_complete,
                        &record.payload,
                    );
                }
            }
            "llm.context_assembly.response" => {
                if let Some(index) = current_call {
                    calls[index].response = record.payload.clone();
                }
            }
            crate::runtime_events::LLM_RESPONSE_TOOL_CALL_NORMALIZED => {
                if let Some(index) = current_call
                    && value_usize(&record.payload, "turn") == calls[index].turn
                    && value_usize(&record.payload, "llm_call_depth") == calls[index].depth
                {
                    calls[index]
                        .response_tool_calls
                        .push(record.payload.clone());
                }
            }
            "agent.turn.finished" => {
                if let Some(index) = current_call
                    && calls[index].assistant.is_empty()
                    && let Some(response) = record.payload.get("response").and_then(Value::as_str)
                {
                    calls[index].assistant = response.to_string();
                    calls[index].assistant_reported_chars = response.chars().count();
                }
            }
            kind if is_tool_event(kind) => {
                if let Some(index) = current_call {
                    calls[index].tools.push(ToolCard {
                        kind: kind.to_string(),
                        name: kind.trim_start_matches("tool.").to_string(),
                        at,
                        summary: tool_summary(kind, &record.payload),
                        success: record.payload.get("success").and_then(Value::as_bool),
                        payload: record.payload.clone(),
                    });
                }
            }
            kind if is_harness_event(kind) => {
                let (mut title, summary) = harness_summary(kind, &record.payload);
                let severity = if kind == "run.finished" && hard_stop.is_some() {
                    title = "Harness stopped".to_string();
                    "danger"
                } else {
                    severity_for(kind, &record.payload)
                };
                let card = EventCard {
                    kind: kind.to_string(),
                    at,
                    title,
                    summary,
                    severity,
                    payload: record.payload.clone(),
                };
                if kind == "run.finished" {
                    final_summary = record
                        .payload
                        .get("final_summary")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                }
                if kind.contains("hard_failed") {
                    hard_stop = Some(card.clone());
                }
                if let Some(index) = current_call {
                    calls[index].harness.push(card);
                }
            }
            _ => {}
        }
    }

    annotate_request_messages(&mut calls);
    let request_snapshot_count = calls.iter().filter(|call| call.request.is_some()).count();
    let exact_request_trace = !calls.is_empty() && request_snapshot_count == calls.len();
    let turns = calls.iter().map(|call| call.turn).max().unwrap_or_default();
    let reasoning_chars = calls.iter().map(|call| call.reasoning_reported_chars).sum();
    let model = run_started
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let fidelity_notes = if exact_request_trace {
        vec![format!(
            "Exact provider-bound requests retained for all {} model calls.",
            calls.len()
        )]
    } else {
        vec![
            format!(
                "Legacy or partial trace: exact provider-bound requests retained for {request_snapshot_count} of {} model calls.",
                calls.len()
            ),
            "Missing request text cannot be reconstructed from component measurements or policy events.".to_string(),
        ]
    };

    Ok(Some(Session {
        id: label_for_trace(path),
        trace: path.display().to_string(),
        model,
        profile,
        runtime_seconds: elapsed_seconds(started, last),
        turns,
        calls,
        reasoning_chars,
        request_snapshot_count,
        exact_request_trace,
        final_summary,
        hard_stop,
        evidence: IndependentEvidence::default(),
        legacy_initial_user_message,
        fidelity_notes,
    }))
}

fn read_records(path: &Path) -> Result<Vec<TraceRecord>> {
    let file = File::open(path).with_context(|| format!("opening trace {}", path.display()))?;
    let mut records = Vec::new();
    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("reading {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line)
            .with_context(|| format!("decoding {} line {}", path.display(), line_index + 1))?;
        records.push(TraceRecord {
            timestamp: value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_time),
            kind: value
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            payload: value.get("payload").cloned().unwrap_or(Value::Null),
        });
    }
    Ok(records)
}

fn annotate_request_messages(calls: &mut [ModelCall]) {
    let mut previous = Vec::new();
    for call in calls {
        let messages = call
            .request
            .as_ref()
            .and_then(|request| request.get("messages"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let components = call
            .context
            .get("components")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        call.request_messages = messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                let continuity = if previous.is_empty() {
                    "initial"
                } else if index >= previous.len() {
                    "new"
                } else if previous[index] == *message {
                    "retained"
                } else {
                    "changed"
                };
                let mut annotated = message
                    .as_object()
                    .cloned()
                    .unwrap_or_else(|| Map::from_iter([("content".to_string(), message.clone())]));
                annotated.insert("index".to_string(), json!(index));
                annotated.insert("continuity".to_string(), json!(continuity));
                annotated.insert(
                    "inclusion_reason".to_string(),
                    components
                        .get(index)
                        .and_then(|component| component.get("inclusion_reason"))
                        .cloned()
                        .unwrap_or_else(|| json!("provider_bound_message")),
                );
                Value::Object(annotated)
            })
            .collect();
        if !messages.is_empty() {
            previous = messages;
        }
    }
}

fn append_stream_text(
    target: &mut String,
    reported_chars: &mut usize,
    complete: &mut bool,
    payload: &Value,
) {
    let preview = payload
        .get("preview")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let chars = payload
        .get("chars")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| preview.chars().count());
    target.push_str(preview);
    *reported_chars += chars;
    if preview.chars().count() != chars {
        *complete = false;
    }
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn elapsed_seconds(start: Option<DateTime<Utc>>, end: Option<DateTime<Utc>>) -> Option<f64> {
    start
        .zip(end)
        .map(|(start, end)| (end - start).num_milliseconds() as f64 / 1_000.0)
}

fn value_usize(value: &Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_default()
}

fn label_for_trace(path: &Path) -> String {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .find(|component| {
            let bytes = component.as_bytes();
            bytes.len() > 4
                && bytes.first() == Some(&b'r')
                && bytes.get(1).is_some_and(u8::is_ascii_digit)
                && bytes.get(2).is_some_and(u8::is_ascii_digit)
                && bytes.get(3) == Some(&b'-')
        })
        .map(str::to_string)
        .or_else(|| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "session".to_string())
}

fn is_tool_event(kind: &str) -> bool {
    matches!(
        kind,
        "tool.list_tree"
            | "tool.read_file"
            | "tool.write_file"
            | "tool.edit_file"
            | "tool.patch_file"
            | "tool.shell_command"
            | "tool.run_probe"
    )
}

fn is_harness_event(kind: &str) -> bool {
    kind == "run.finished"
        || kind == "run.failed"
        || kind.starts_with("agent.validation")
        || kind.starts_with("agent.stage")
        || kind.starts_with("agent.terminal")
        || kind.starts_with("agent.action_boundary")
        || kind.starts_with("agent.pre_source_action_only")
        || kind.starts_with("agent.turn.empty")
        || kind.starts_with("agent.turn.hidden")
        || kind.starts_with("agent.contract.probes")
        || kind.starts_with("llm.thinking_only_stream")
}

fn tool_summary(kind: &str, payload: &Value) -> String {
    match kind {
        "tool.shell_command" => {
            let command = payload
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default();
            let status = payload
                .get("status")
                .map_or("?".to_string(), Value::to_string);
            format!("{} · exit {status}", truncate(command, 96))
        }
        "tool.write_file" => format!(
            "{} · {} bytes written",
            payload.get("path").and_then(Value::as_str).unwrap_or("?"),
            value_usize(payload, "bytes_written")
        ),
        "tool.list_tree" => format!("{} entries", value_usize(payload, "entry_count")),
        _ => payload
            .get("path")
            .or_else(|| payload.get("probe_id"))
            .and_then(Value::as_str)
            .unwrap_or(kind)
            .to_string(),
    }
}

fn harness_summary(kind: &str, payload: &Value) -> (String, String) {
    match kind {
        "agent.validation.repair_required" => (
            "Repair mode entered".to_string(),
            payload
                .get("failure_text")
                .and_then(Value::as_str)
                .unwrap_or("Validation failed")
                .to_string(),
        ),
        "agent.validation.repair_no_action" => (
            "Repair turn produced no action".to_string(),
            format!(
                "Consecutive no-action turns: {}",
                value_usize(payload, "consecutive_no_action_turns")
            ),
        ),
        "agent.stage.first_source_mutation" => (
            "First source mutation".to_string(),
            payload
                .get("paths")
                .and_then(Value::as_array)
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default(),
        ),
        "agent.validation_probe.observed" => (
            format!(
                "Validation {}",
                if payload.get("success").and_then(Value::as_bool) == Some(true) {
                    "passed"
                } else {
                    "failed"
                }
            ),
            truncate(
                payload
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                180,
            ),
        ),
        "llm.thinking_only_stream.action_transitioned" => (
            "Thinking cap transitioned to action".to_string(),
            "The next call was constrained toward a concrete action.".to_string(),
        ),
        "agent.pre_source_action_only.scheduled" => (
            "Pre-source action-only handoff scheduled".to_string(),
            "The next outer turn will disable reasoning and expose action tools only.".to_string(),
        ),
        "agent.pre_source_action_only.started" => (
            "Pre-source action-only handoff started".to_string(),
            "Native reasoning is disabled; read, list, and arbitrary shell tools are withheld."
                .to_string(),
        ),
        "agent.pre_source_action_only.completed" => (
            "Pre-source action-only handoff completed".to_string(),
            format!(
                "Source mutation: {}; validation probe: {}",
                payload
                    .get("source_mutated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                payload
                    .get("validation_probed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            ),
        ),
        "agent.pre_source_action_only.aborted" => (
            "Pre-source action-only aborted".to_string(),
            format!(
                "Reason: {}; source mutation: {}; validation probe: {}",
                payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                payload
                    .get("source_mutated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                payload
                    .get("validation_probed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ),
        ),
        "run.finished" => (
            "Harness finished".to_string(),
            payload
                .get("final_summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        "run.failed" => (
            "Harness failed".to_string(),
            payload
                .get("error")
                .or_else(|| payload.get("stage"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        _ if kind.contains("repair_hard_failed") => (
            "Repair hard stop".to_string(),
            "The harness stopped repeated repair reasoning without an edit or probe.".to_string(),
        ),
        _ => (kind.replace(['.', '_'], " "), String::new()),
    }
}

fn severity_for(kind: &str, payload: &Value) -> &'static str {
    if kind.contains("hard_failed") || kind == "run.failed" {
        "danger"
    } else if payload.get("success").and_then(Value::as_bool) == Some(false)
        || kind.contains("repair")
        || kind.contains("no_action")
    {
        "warning"
    } else if payload.get("success").and_then(Value::as_bool) == Some(true)
        || kind.ends_with("resolved")
        || kind == "run.finished"
    {
        "success"
    } else {
        "neutral"
    }
}

fn truncate(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_exact_context_continuity_and_generic_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let trace_dir = temp.path().join("r01-adaptive-test/traces");
        std::fs::create_dir_all(&trace_dir).unwrap();
        let trace = trace_dir.join("run.jsonl");
        let records = [
            json!({"timestamp":"2026-01-01T00:00:00Z","kind":"run.started","payload":{"model":"qwen"}}),
            json!({"timestamp":"2026-01-01T00:00:01Z","kind":"llm.context_assembly.ledger","payload":{"turn":1,"llm_call_depth":0,"utilization":0.1}}),
            json!({"timestamp":"2026-01-01T00:00:01Z","kind":"llm.provider_request.assembled","payload":{"turn":1,"llm_call_depth":0,"messages":[{"role":"system","content":"system"},{"role":"user","content":"task"}],"tools":[],"completion":{}}}),
            json!({"timestamp":"2026-01-01T00:00:02Z","kind":"llm.stream.thinking","payload":{"preview":"inspect","chars":7}}),
            json!({"timestamp":"2026-01-01T00:00:02Z","kind":"llm.response.tool_call.normalized","payload":{"schema_version":"response_tool_call.v1","turn":1,"llm_call_depth":0,"response_index":0,"response_tool_call_count":1,"tool_call_id":"call-0","tool_name":"read_file","arguments_json":"{\"path\":\"src/lib.rs\"}","arguments_complete":true}}),
            json!({"timestamp":"2026-01-01T00:00:03Z","kind":"tool.list_tree","payload":{"entry_count":0}}),
            json!({"timestamp":"2026-01-01T00:00:04Z","kind":"llm.context_assembly.ledger","payload":{"turn":1,"llm_call_depth":1,"utilization":0.2}}),
            json!({"timestamp":"2026-01-01T00:00:04Z","kind":"llm.provider_request.assembled","payload":{"turn":1,"llm_call_depth":1,"messages":[{"role":"system","content":"system"},{"role":"user","content":"task"},{"role":"tool","content":"empty"}],"tools":[],"completion":{}}}),
            json!({"timestamp":"2026-01-01T00:00:04Z","kind":"agent.validation.repair_hard_failed","payload":{}}),
            json!({"timestamp":"2026-01-01T00:00:05Z","kind":"run.finished","payload":{"final_summary":"DONE"}}),
        ];
        std::fs::write(
            &trace,
            records
                .iter()
                .map(|record| format!("{record}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let evidence = temp.path().join("evidence.json");
        std::fs::write(
            &evidence,
            json!({"schema_version":EVIDENCE_SCHEMA,"sessions":[{"label":"r01-adaptive-test","reward":"1","passed":2,"failed":0,"output":"PASSED"}]}).to_string(),
        )
        .unwrap();
        let output = temp.path().join("transcript.html");

        let summary = render_transcript(RenderTranscriptConfig {
            inputs: vec![trace_dir],
            output: output.clone(),
            title: "Test </script>".to_string(),
            evidence_file: Some(evidence),
        })
        .unwrap();

        assert_eq!(summary.session_count, 1);
        assert_eq!(summary.model_call_count, 2);
        assert_eq!(summary.exact_request_snapshot_count, 2);
        let html = std::fs::read_to_string(output).unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Causal exchange timeline"));
        assert!(html.contains("\\/script>"));
        assert!(html.contains("\"continuity\":\"retained\""));
        assert!(html.contains("\"continuity\":\"new\""));
        assert!(html.contains("\"response_tool_calls\":[{"));
        assert!(html.contains("src/lib.rs"));
        assert!(html.contains("\"title\":\"Harness stopped\""));
        assert!(html.contains("\"severity\":\"danger\""));
        assert!(!html.contains("<script src="));
        assert!(!html.contains("<link"));
    }

    #[test]
    fn trace_label_accepts_generic_replicate_arm_names() {
        let path = Path::new("/benchmark/runs/cell/r02-repair-2k/job/trial/agent/traces/run.jsonl");

        assert_eq!(label_for_trace(path), "r02-repair-2k");
    }

    #[test]
    fn rejects_unknown_evidence_schema() {
        let temp = tempfile::tempdir().unwrap();
        let evidence = temp.path().join("evidence.json");
        std::fs::write(&evidence, r#"{"schema_version":"unknown","sessions":[]}"#).unwrap();

        let error = load_evidence(Some(&evidence)).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unsupported transcript evidence schema")
        );
    }
}
