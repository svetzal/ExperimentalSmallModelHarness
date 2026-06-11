use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Default)]
pub struct TraceAnalysis {
    pub trace_file: PathBuf,
    pub model: Option<String>,
    pub goal_file: Option<String>,
    pub tool_root: Option<String>,
    pub assembly_policy: Option<String>,
    pub context_window_tokens: Option<usize>,
    pub max_tool_iterations: Option<usize>,
    pub packet_type: Option<String>,
    pub expected_output_tokens: Option<usize>,
    pub status: String,
    pub runtime_seconds: Option<f64>,
    pub llm_call_count: usize,
    pub max_llm_call_estimated_tokens: usize,
    pub max_llm_call_utilization: Option<f64>,
    pub max_pressure_band: String,
    pub pressure_band_counts: BTreeMap<String, usize>,
    pub context_delta_chars_total: isize,
    pub appended_message_count: usize,
    pub appended_chars_by_component: BTreeMap<String, usize>,
    pub cumulative_tool_result_estimated_tokens: usize,
    pub largest_tool_result_estimated_tokens: usize,
    pub largest_tool_result_kind: Option<String>,
    pub tool_result_estimated_tokens_by_kind: BTreeMap<String, usize>,
    pub stream_progress_events: usize,
    pub stream_metrics_events: usize,
    pub progress_status_events: usize,
    pub progress_state_counts: BTreeMap<String, usize>,
    pub runner_activity_evidence_events: usize,
    pub gpu_utilization_sample_events: usize,
    pub max_gpu_utilization_percent: Option<f64>,
    pub observed_output_tokens: usize,
    pub max_observed_tokens_per_second: Option<f64>,
    pub failed_tool_events: usize,
    pub validation_commands: Vec<ValidationCommandSummary>,
    pub final_summary_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ValidationCommandSummary {
    pub command: String,
    pub status: Option<i64>,
    pub success: Option<bool>,
    pub repair_required: bool,
}

pub fn analyze_trace(path: impl AsRef<Path>) -> Result<TraceAnalysis> {
    let path = path.as_ref();
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut analysis = TraceAnalysis {
        trace_file: path.to_path_buf(),
        status: "unknown".to_string(),
        max_pressure_band: "unknown".to_string(),
        ..Default::default()
    };
    let mut first_timestamp = None;
    let mut last_timestamp = None;

    for (line_number, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(line)
            .with_context(|| format!("parsing {} line {}", path.display(), line_number + 1))?;
        let kind = record["kind"].as_str().unwrap_or_default();
        let payload = &record["payload"];
        if let Some(timestamp) = record["timestamp"].as_str().and_then(parse_timestamp) {
            first_timestamp.get_or_insert(timestamp);
            last_timestamp = Some(timestamp);
        }

        match kind {
            "run.started" => apply_run_started(&mut analysis, payload),
            "run.finished" => apply_run_finished(&mut analysis, payload),
            "run.failed" => {
                analysis.status = "failed".to_string();
            }
            "llm.context_assembly.ledger" => apply_context_ledger(&mut analysis, payload),
            "llm.context_assembly.appended" => apply_context_append(&mut analysis, payload),
            "llm.stream.progress" => {
                analysis.stream_progress_events += 1;
            }
            "llm.progress.status" => apply_progress_status(&mut analysis, payload),
            "llm.stream.metrics" => apply_stream_metrics(&mut analysis, payload),
            "tool.payload.measured" => apply_tool_payload(&mut analysis, payload),
            "tool.shell_command" => apply_shell_command(&mut analysis, payload),
            kind if kind.starts_with("tool.") && is_failed_tool_payload(payload) => {
                analysis.failed_tool_events += 1;
            }
            _ => {}
        }
    }

    if analysis.status == "unknown" {
        analysis.status = "unfinished".to_string();
    }
    analysis.runtime_seconds = first_timestamp
        .zip(last_timestamp)
        .map(|(first, last)| (last - first).num_milliseconds() as f64 / 1000.0);
    Ok(analysis)
}

fn apply_run_started(analysis: &mut TraceAnalysis, payload: &Value) {
    analysis.model = value_string(payload, "model");
    analysis.goal_file = value_string(payload, "goal_file");
    analysis.tool_root = value_string(payload, "tool_root");
    analysis.context_window_tokens = value_usize(payload, "context_window_tokens");
    analysis.max_tool_iterations = value_usize(payload, "max_tool_iterations");
    analysis.assembly_policy = value_string(payload, "assembly_policy");
    analysis.packet_type = value_string(payload, "packet_type");
    analysis.expected_output_tokens = value_usize(payload, "expected_output_tokens");
}

fn apply_run_finished(analysis: &mut TraceAnalysis, payload: &Value) {
    analysis.status = "finished".to_string();
    analysis.final_summary_preview = value_string(payload, "final_summary")
        .map(|summary| summary.chars().take(240).collect::<String>());
}

fn apply_context_ledger(analysis: &mut TraceAnalysis, payload: &Value) {
    analysis.llm_call_count += 1;
    if let Some(tokens) = value_usize(payload, "estimated_tokens") {
        analysis.max_llm_call_estimated_tokens = analysis.max_llm_call_estimated_tokens.max(tokens);
    }
    if let Some(utilization) = payload["utilization"].as_f64() {
        analysis.max_llm_call_utilization = Some(
            analysis
                .max_llm_call_utilization
                .unwrap_or(0.0)
                .max(utilization),
        );
    }
    if let Some(band) = payload["pressure_band"].as_str() {
        *analysis
            .pressure_band_counts
            .entry(band.to_string())
            .or_insert(0) += 1;
        if pressure_rank(band) > pressure_rank(&analysis.max_pressure_band) {
            analysis.max_pressure_band = band.to_string();
        }
    }
    if let Some(delta) = payload["delta_chars_from_previous_call"].as_i64() {
        analysis.context_delta_chars_total += delta as isize;
    }
    if analysis.assembly_policy.is_none() {
        analysis.assembly_policy = value_string(payload, "assembly_policy");
    }
}

fn apply_context_append(analysis: &mut TraceAnalysis, payload: &Value) {
    analysis.appended_message_count += 1;
    let component = payload["component"].as_str().unwrap_or("unknown");
    let chars = value_usize(payload, "message_chars")
        .or_else(|| value_usize(payload, "content_chars"))
        .unwrap_or_default();
    *analysis
        .appended_chars_by_component
        .entry(component.to_string())
        .or_insert(0) += chars;
}

fn apply_tool_payload(analysis: &mut TraceAnalysis, payload: &Value) {
    if let Some(tokens) = value_usize(payload, "total_tool_result_estimated_tokens") {
        analysis.cumulative_tool_result_estimated_tokens =
            analysis.cumulative_tool_result_estimated_tokens.max(tokens);
    }
    if let Some(tokens) = value_usize(payload, "max_tool_result_estimated_tokens") {
        analysis.largest_tool_result_estimated_tokens =
            analysis.largest_tool_result_estimated_tokens.max(tokens);
    }
    if let Some(kind) = value_string(payload, "max_tool_result_kind") {
        analysis.largest_tool_result_kind = Some(kind);
    }
    if let Some(kind) = value_string(payload, "kind") {
        *analysis
            .tool_result_estimated_tokens_by_kind
            .entry(kind)
            .or_insert(0) += value_usize(payload, "result_estimated_tokens").unwrap_or_default();
    }
}

fn apply_progress_status(analysis: &mut TraceAnalysis, payload: &Value) {
    analysis.progress_status_events += 1;
    if let Some(state) = value_string(payload, "progress_state") {
        *analysis.progress_state_counts.entry(state).or_insert(0) += 1;
    }
    if payload["runner_activity_evidence"].as_bool() == Some(true) {
        analysis.runner_activity_evidence_events += 1;
    }
    if let Some(utilization) = payload["runner_activity"]["gpu_utilization_percent"].as_f64() {
        analysis.gpu_utilization_sample_events += 1;
        analysis.max_gpu_utilization_percent = Some(
            analysis
                .max_gpu_utilization_percent
                .unwrap_or(0.0)
                .max(utilization),
        );
    }
}

fn apply_stream_metrics(analysis: &mut TraceAnalysis, payload: &Value) {
    analysis.stream_metrics_events += 1;
    analysis.observed_output_tokens += value_usize(payload, "eval_count").unwrap_or_default();
    if let Some(tokens_per_second) = payload["tokens_per_second"].as_f64() {
        analysis.max_observed_tokens_per_second = Some(
            analysis
                .max_observed_tokens_per_second
                .unwrap_or(0.0)
                .max(tokens_per_second),
        );
    }
    if analysis.packet_type.is_none() {
        analysis.packet_type = value_string(payload, "packet_type");
    }
    if analysis.expected_output_tokens.is_none() {
        analysis.expected_output_tokens = value_usize(payload, "expected_output_tokens");
    }
}

fn apply_shell_command(analysis: &mut TraceAnalysis, payload: &Value) {
    if payload["validation_probe"].as_bool().unwrap_or(false) {
        analysis.validation_commands.push(ValidationCommandSummary {
            command: payload["command"].as_str().unwrap_or_default().to_string(),
            status: payload["status"].as_i64(),
            success: payload["success"].as_bool(),
            repair_required: !payload["repair_required"].is_null(),
        });
    }
    if is_failed_tool_payload(payload) {
        analysis.failed_tool_events += 1;
    }
}

fn is_failed_tool_payload(payload: &Value) -> bool {
    payload.get("error").is_some() || payload["success"].as_bool() == Some(false)
}

fn value_string(payload: &Value, key: &str) -> Option<String> {
    payload[key].as_str().map(ToString::to_string)
}

fn value_usize(payload: &Value, key: &str) -> Option<usize> {
    payload[key].as_u64().map(|value| value as usize)
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn pressure_rank(band: &str) -> u8 {
    match band {
        "green" => 1,
        "yellow" => 2,
        "orange" => 3,
        "red" => 4,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzes_context_assembly_trace_events() {
        let temp = tempfile::tempdir().unwrap();
        let trace = temp.path().join("run-test.jsonl");
        std::fs::write(
            &trace,
            r#"{"timestamp":"2026-06-11T00:00:00Z","kind":"run.started","payload":{"model":"qwen","goal_file":"/tmp/task.md","max_tool_iterations":50,"context_window_tokens":1000,"packet_type":"narrow-patch","expected_output_tokens":2048,"assembly_policy":"append_summarized_tool_transcript"}}
{"timestamp":"2026-06-11T00:00:01Z","kind":"llm.context_assembly.ledger","payload":{"estimated_tokens":100,"utilization":0.10,"pressure_band":"green","delta_chars_from_previous_call":null,"assembly_policy":"append_summarized_tool_transcript"}}
{"timestamp":"2026-06-11T00:00:02Z","kind":"llm.context_assembly.appended","payload":{"component":"tool_result","message_chars":400}}
{"timestamp":"2026-06-11T00:00:03Z","kind":"tool.payload.measured","payload":{"kind":"tool.read_file","result_estimated_tokens":100,"total_tool_result_estimated_tokens":100,"max_tool_result_estimated_tokens":100,"max_tool_result_kind":"tool.read_file"}}
{"timestamp":"2026-06-11T00:00:04Z","kind":"llm.context_assembly.ledger","payload":{"estimated_tokens":250,"utilization":0.25,"pressure_band":"orange","delta_chars_from_previous_call":600,"assembly_policy":"append_summarized_tool_transcript"}}
{"timestamp":"2026-06-11T00:00:04Z","kind":"llm.stream.progress","payload":{"frame_index":1}}
{"timestamp":"2026-06-11T00:00:04Z","kind":"llm.progress.status","payload":{"progress_state":"ProgressUnknown","runner_activity_evidence":true,"runner_activity":{"gpu_utilization_percent":38.5}}}
{"timestamp":"2026-06-11T00:00:04Z","kind":"llm.stream.metrics","payload":{"eval_count":200,"tokens_per_second":4.5,"packet_type":"narrow-patch","expected_output_tokens":2048}}
{"timestamp":"2026-06-11T00:00:05Z","kind":"tool.shell_command","payload":{"command":"cargo test","validation_probe":true,"status":101,"success":false,"repair_required":{"active":true}}}
{"timestamp":"2026-06-11T00:00:06Z","kind":"run.finished","payload":{"final_summary":"DONE"}}
"#,
        )
        .unwrap();

        let analysis = analyze_trace(&trace).unwrap();

        assert_eq!(analysis.status, "finished");
        assert_eq!(analysis.model.as_deref(), Some("qwen"));
        assert_eq!(analysis.llm_call_count, 2);
        assert_eq!(analysis.max_llm_call_estimated_tokens, 250);
        assert_eq!(analysis.max_pressure_band, "orange");
        assert_eq!(analysis.appended_message_count, 1);
        assert_eq!(analysis.cumulative_tool_result_estimated_tokens, 100);
        assert_eq!(analysis.packet_type.as_deref(), Some("narrow-patch"));
        assert_eq!(analysis.expected_output_tokens, Some(2048));
        assert_eq!(analysis.stream_progress_events, 1);
        assert_eq!(analysis.progress_status_events, 1);
        assert_eq!(
            analysis.progress_state_counts.get("ProgressUnknown"),
            Some(&1)
        );
        assert_eq!(analysis.runner_activity_evidence_events, 1);
        assert_eq!(analysis.gpu_utilization_sample_events, 1);
        assert_eq!(analysis.max_gpu_utilization_percent, Some(38.5));
        assert_eq!(analysis.stream_metrics_events, 1);
        assert_eq!(analysis.observed_output_tokens, 200);
        assert_eq!(analysis.max_observed_tokens_per_second, Some(4.5));
        assert_eq!(analysis.validation_commands.len(), 1);
        assert!(analysis.validation_commands[0].repair_required);
        assert_eq!(analysis.runtime_seconds, Some(6.0));
    }
}
