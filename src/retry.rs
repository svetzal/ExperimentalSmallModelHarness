//! Bounded sequential retry coordination for unattended local-model runs.
//!
//! Each attempt is an ordinary [`crate::agent::run_agent`] invocation and is
//! always awaited to its natural terminal state. The coordinator changes no
//! within-attempt prompt, budget, tool, or repair policy. It only decides,
//! between completed attempts, whether another run may use the retained
//! artifact.

use crate::agent::{AgentRunConfig, run_agent};
use crate::runtime_events;
use crate::trace_analysis::analyze_trace;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SequentialRunConfig {
    pub agent: AgentRunConfig,
    pub artifact: PathBuf,
    pub expected_artifact: PathBuf,
    pub max_attempts: usize,
    pub max_failed_repair_cycles: usize,
    pub max_consecutive_unchanged_attempts: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SequentialStopReason {
    ConfirmedExactSuccess,
    ExplicitFail,
    ExactArtifactWithoutTerminalEvidence,
    ConsecutiveUnchangedAttempts,
    FailedRepairCycleLimit,
    AttemptLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSnapshot {
    pub exists: bool,
    pub bytes: usize,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequentialAttemptRecord {
    pub attempt: usize,
    pub trace_file: PathBuf,
    pub pre_artifact: ArtifactSnapshot,
    pub post_artifact: ArtifactSnapshot,
    pub artifact_unchanged: bool,
    pub consecutive_unchanged_attempts: usize,
    pub failed_repair_cycles: usize,
    pub cumulative_failed_repair_cycles: usize,
    pub passing_probes: usize,
    pub accepted_done: bool,
    pub explicit_fail: bool,
    pub independent_exact: bool,
    pub runtime_seconds: Option<f64>,
    pub observed_output_tokens: usize,
    pub final_summary: String,
    pub terminal_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequentialRunSummary {
    pub schema_version: String,
    pub artifact: PathBuf,
    pub expected_artifact: PathBuf,
    pub max_attempts: usize,
    pub max_failed_repair_cycles: usize,
    pub max_consecutive_unchanged_attempts: usize,
    pub attempts: Vec<SequentialAttemptRecord>,
    pub cumulative_failed_repair_cycles: usize,
    pub cumulative_runtime_seconds: f64,
    pub cumulative_observed_output_tokens: usize,
    pub stop_reason: SequentialStopReason,
    pub confirmed_exact_success: bool,
    pub summary_file: PathBuf,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct TraceRetryEvidence {
    failed_repair_cycles: usize,
    passing_probes: usize,
    accepted_done: bool,
    explicit_fail: bool,
}

#[derive(Debug, Clone, Copy)]
struct RetryLimits {
    max_attempts: usize,
    max_failed_repair_cycles: usize,
    max_consecutive_unchanged_attempts: usize,
}

pub async fn run_sequential(config: SequentialRunConfig) -> Result<SequentialRunSummary> {
    validate_limits(&config)?;
    let tool_root = std::env::current_dir()
        .context("reading harness cwd")?
        .canonicalize()
        .context("canonicalizing harness cwd")?;
    let artifact = scoped_worker_path(&tool_root, &config.artifact)?;
    let experiment_dir = config
        .agent
        .experiment_dir
        .canonicalize()
        .with_context(|| {
            format!(
                "canonicalizing experiment directory {}",
                config.agent.experiment_dir.display()
            )
        })?;
    let expected_artifact = scoped_experiment_file(
        &experiment_dir,
        &config.expected_artifact,
        "expected artifact",
    )?;
    let expected_bytes = std::fs::read(&expected_artifact)
        .with_context(|| format!("reading expected artifact {}", expected_artifact.display()))?;
    let limits = RetryLimits {
        max_attempts: config.max_attempts,
        max_failed_repair_cycles: config.max_failed_repair_cycles,
        max_consecutive_unchanged_attempts: config.max_consecutive_unchanged_attempts,
    };

    let mut attempts = Vec::new();
    let mut cumulative_failed_repair_cycles = 0usize;
    let mut cumulative_runtime_seconds = 0.0f64;
    let mut cumulative_observed_output_tokens = 0usize;
    let mut consecutive_unchanged_attempts = 0usize;
    let stop_reason;

    loop {
        let attempt = attempts.len() + 1;
        let pre_artifact = snapshot(&artifact)?;
        let trace_dir = experiment_dir.join("traces");
        let traces_before = trace_paths(&trace_dir)?;
        let run_result = run_agent(config.agent.clone()).await;
        let (trace_file, final_summary, terminal_error) = match run_result {
            Ok(summary) => (summary.trace_file, summary.final_summary, None),
            Err(error) => {
                let trace_file = newly_failed_trace(&trace_dir, &traces_before, &tool_root)
                    .with_context(|| format!("adopting trace after inner run error: {error:#}"))?;
                let error = format!("{error:#}");
                (
                    trace_file,
                    format!("inner run failed: {error}"),
                    Some(error),
                )
            }
        };
        let post_artifact = snapshot(&artifact)?;
        let artifact_unchanged = pre_artifact == post_artifact;
        consecutive_unchanged_attempts = if artifact_unchanged {
            consecutive_unchanged_attempts + 1
        } else {
            0
        };
        let evidence = inspect_trace(&trace_file)?;
        cumulative_failed_repair_cycles += evidence.failed_repair_cycles;
        let analysis = analyze_trace(&trace_file)?;
        cumulative_runtime_seconds += analysis.runtime_seconds.unwrap_or_default();
        cumulative_observed_output_tokens += analysis.observed_output_tokens;
        let independent_exact = artifact_bytes_equal(&artifact, &expected_bytes)?;

        let record = SequentialAttemptRecord {
            attempt,
            trace_file,
            pre_artifact,
            post_artifact,
            artifact_unchanged,
            consecutive_unchanged_attempts,
            failed_repair_cycles: evidence.failed_repair_cycles,
            cumulative_failed_repair_cycles,
            passing_probes: evidence.passing_probes,
            accepted_done: evidence.accepted_done,
            explicit_fail: evidence.explicit_fail,
            independent_exact,
            runtime_seconds: analysis.runtime_seconds,
            observed_output_tokens: analysis.observed_output_tokens,
            final_summary,
            terminal_error,
        };
        let decision = stop_after_attempt(&record, &attempts, limits);
        attempts.push(record);
        if let Some(reason) = decision {
            stop_reason = reason;
            break;
        }
    }

    let confirmed_exact_success = stop_reason == SequentialStopReason::ConfirmedExactSuccess;
    let summary_file = next_summary_path(&experiment_dir.join("retry"))?;
    let summary = SequentialRunSummary {
        schema_version: "sequential_retry.v2".to_string(),
        artifact: config.artifact,
        expected_artifact: config.expected_artifact,
        max_attempts: config.max_attempts,
        max_failed_repair_cycles: config.max_failed_repair_cycles,
        max_consecutive_unchanged_attempts: config.max_consecutive_unchanged_attempts,
        attempts,
        cumulative_failed_repair_cycles,
        cumulative_runtime_seconds,
        cumulative_observed_output_tokens,
        stop_reason,
        confirmed_exact_success,
        summary_file: summary_file.clone(),
    };
    persist_summary(&summary_file, &summary)?;
    Ok(summary)
}

fn validate_limits(config: &SequentialRunConfig) -> Result<()> {
    if config.max_attempts == 0 {
        bail!("max attempts must be greater than zero");
    }
    if config.max_failed_repair_cycles == 0 {
        bail!("max failed repair cycles must be greater than zero");
    }
    if config.max_consecutive_unchanged_attempts == 0 {
        bail!("max consecutive unchanged attempts must be greater than zero");
    }
    Ok(())
}

fn stop_after_attempt(
    current: &SequentialAttemptRecord,
    previous: &[SequentialAttemptRecord],
    limits: RetryLimits,
) -> Option<SequentialStopReason> {
    if current.independent_exact && current.passing_probes > 0 && current.accepted_done {
        return Some(SequentialStopReason::ConfirmedExactSuccess);
    }
    if current.explicit_fail {
        return Some(SequentialStopReason::ExplicitFail);
    }
    if current.independent_exact {
        return Some(SequentialStopReason::ExactArtifactWithoutTerminalEvidence);
    }
    if current.consecutive_unchanged_attempts >= limits.max_consecutive_unchanged_attempts {
        return Some(SequentialStopReason::ConsecutiveUnchangedAttempts);
    }
    if current.cumulative_failed_repair_cycles >= limits.max_failed_repair_cycles {
        return Some(SequentialStopReason::FailedRepairCycleLimit);
    }
    if previous.len() + 1 >= limits.max_attempts {
        return Some(SequentialStopReason::AttemptLimit);
    }
    None
}

fn inspect_trace(path: &Path) -> Result<TraceRetryEvidence> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading retry trace {}", path.display()))?;
    let mut evidence = TraceRetryEvidence::default();
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(line)
            .with_context(|| format!("parsing {} line {}", path.display(), index + 1))?;
        let kind = record["kind"].as_str().unwrap_or_default();
        let payload = &record["payload"];
        match kind {
            runtime_events::AGENT_VALIDATION_PROBE_OBSERVED => {
                if payload["success"].as_bool() == Some(true) {
                    evidence.passing_probes += 1;
                } else if payload["had_pending_source_writes"].as_bool() == Some(true)
                    && payload["assertion_kind"].as_str() == Some("file_text_equals")
                {
                    evidence.failed_repair_cycles += 1;
                }
            }
            "agent.terminal.done_observed" => evidence.accepted_done = true,
            "agent.terminal.fail_observed" => evidence.explicit_fail = true,
            _ => {}
        }
    }
    Ok(evidence)
}

fn trace_paths(dir: &Path) -> Result<HashSet<PathBuf>> {
    if !dir.exists() {
        return Ok(HashSet::new());
    }
    let mut paths = HashSet::new();
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("reading trace directory {}", dir.display()))?
    {
        let path = entry
            .with_context(|| format!("reading entry in {}", dir.display()))?
            .path();
        if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            paths.insert(path);
        }
    }
    Ok(paths)
}

fn newly_failed_trace(dir: &Path, before: &HashSet<PathBuf>, tool_root: &Path) -> Result<PathBuf> {
    let mut matches = Vec::new();
    for path in trace_paths(dir)?.difference(before) {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading candidate failed trace {}", path.display()))?;
        let mut matching_root = false;
        let mut failed = false;
        for (index, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record: Value = serde_json::from_str(line)
                .with_context(|| format!("parsing {} line {}", path.display(), index + 1))?;
            if record["payload"]["tool_root"].as_str() == tool_root.to_str() {
                matching_root = true;
            }
            if record["kind"].as_str() == Some("run.failed") {
                failed = true;
            }
        }
        if matching_root && failed {
            matches.push(path.clone());
        }
    }
    match matches.as_slice() {
        [path] => Ok(path.clone()),
        [] => bail!(
            "inner run failed without one new matching run.failed trace under {}",
            dir.display()
        ),
        _ => bail!(
            "inner run failure produced {} matching traces under {}",
            matches.len(),
            dir.display()
        ),
    }
}

fn snapshot(path: &Path) -> Result<ArtifactSnapshot> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(ArtifactSnapshot {
            exists: true,
            bytes: bytes.len(),
            sha256: Some(sha256(&bytes)),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ArtifactSnapshot {
            exists: false,
            bytes: 0,
            sha256: None,
        }),
        Err(error) => Err(error).with_context(|| format!("reading artifact {}", path.display())),
    }
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut result = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut result, "{byte:02x}").expect("writing to String cannot fail");
    }
    result
}

fn artifact_bytes_equal(path: &Path, expected: &[u8]) -> Result<bool> {
    match std::fs::read(path) {
        Ok(actual) => Ok(actual == expected),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("reading artifact {}", path.display())),
    }
}

fn scoped_worker_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    validate_relative_path(relative, "artifact")?;
    let candidate = root.join(relative);
    if candidate.exists() {
        let canonical = candidate
            .canonicalize()
            .with_context(|| format!("canonicalizing artifact {}", candidate.display()))?;
        if !canonical.starts_with(root) {
            bail!("artifact path escapes worker root: {}", relative.display());
        }
        Ok(canonical)
    } else {
        let existing_ancestor = candidate
            .ancestors()
            .find(|ancestor| ancestor.exists())
            .ok_or_else(|| anyhow::anyhow!("artifact path has no existing ancestor"))?;
        let canonical_ancestor = existing_ancestor.canonicalize().with_context(|| {
            format!(
                "canonicalizing artifact ancestor {}",
                existing_ancestor.display()
            )
        })?;
        if !canonical_ancestor.starts_with(root) {
            bail!("artifact path escapes worker root: {}", relative.display());
        }
        Ok(candidate)
    }
}

fn scoped_experiment_file(root: &Path, path: &Path, label: &str) -> Result<PathBuf> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("canonicalizing {label} {}", candidate.display()))?;
    if !canonical.starts_with(root) {
        bail!("{label} escapes experiment root: {}", path.display());
    }
    if !canonical.is_file() {
        bail!("{label} is not a file: {}", canonical.display());
    }
    Ok(canonical)
}

fn validate_relative_path(path: &Path, label: &str) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("{label} must be a non-empty root-relative path");
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!("{label} path may not escape its root: {}", path.display());
    }
    Ok(())
}

fn next_summary_path(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating retry summary directory {}", dir.display()))?;
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
    Ok(dir.join(format!("sequence-{timestamp}.json")))
}

fn persist_summary(path: &Path, summary: &SequentialRunSummary) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .with_context(|| format!("creating retry summary {}", path.display()))?;
    serde_json::to_writer_pretty(&mut file, summary)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn record(
        attempt: usize,
        exact: bool,
        pass: bool,
        done: bool,
        fail: bool,
        unchanged: usize,
        cycles: usize,
    ) -> SequentialAttemptRecord {
        SequentialAttemptRecord {
            attempt,
            trace_file: PathBuf::from("trace.jsonl"),
            pre_artifact: ArtifactSnapshot {
                exists: false,
                bytes: 0,
                sha256: None,
            },
            post_artifact: ArtifactSnapshot {
                exists: exact,
                bytes: usize::from(exact),
                sha256: exact.then(|| "hash".to_string()),
            },
            artifact_unchanged: unchanged > 0,
            consecutive_unchanged_attempts: unchanged,
            failed_repair_cycles: cycles,
            cumulative_failed_repair_cycles: cycles,
            passing_probes: usize::from(pass),
            accepted_done: done,
            explicit_fail: fail,
            independent_exact: exact,
            runtime_seconds: Some(1.0),
            observed_output_tokens: 10,
            final_summary: String::new(),
            terminal_error: None,
        }
    }

    fn limits() -> RetryLimits {
        RetryLimits {
            max_attempts: 8,
            max_failed_repair_cycles: 8,
            max_consecutive_unchanged_attempts: 2,
        }
    }

    #[test]
    fn confirmed_success_requires_exact_probe_and_accepted_done() {
        let exact_without_done = record(1, true, true, false, false, 0, 1);
        assert_eq!(
            stop_after_attempt(&exact_without_done, &[], limits()),
            Some(SequentialStopReason::ExactArtifactWithoutTerminalEvidence)
        );

        let confirmed = record(1, true, true, true, false, 0, 1);
        assert_eq!(
            stop_after_attempt(&confirmed, &[], limits()),
            Some(SequentialStopReason::ConfirmedExactSuccess)
        );
    }

    #[test]
    fn explicit_fail_precedes_unconfirmed_exact_classification() {
        let failed = record(1, true, false, false, true, 0, 0);
        assert_eq!(
            stop_after_attempt(&failed, &[], limits()),
            Some(SequentialStopReason::ExplicitFail)
        );
    }

    #[test]
    fn unchanged_cycle_and_attempt_limits_are_deterministic() {
        let unchanged = record(2, false, false, false, false, 2, 3);
        assert_eq!(
            stop_after_attempt(
                &unchanged,
                &[record(1, false, false, false, false, 1, 1)],
                limits()
            ),
            Some(SequentialStopReason::ConsecutiveUnchangedAttempts)
        );

        let mut cycles = record(2, false, false, false, false, 0, 8);
        cycles.cumulative_failed_repair_cycles = 8;
        assert_eq!(
            stop_after_attempt(
                &cycles,
                &[record(1, false, false, false, false, 0, 1)],
                limits()
            ),
            Some(SequentialStopReason::FailedRepairCycleLimit)
        );

        let final_attempt = record(8, false, false, false, false, 0, 1);
        let previous = (1..8)
            .map(|attempt| record(attempt, false, false, false, false, 0, 0))
            .collect::<Vec<_>>();
        assert_eq!(
            stop_after_attempt(&final_attempt, &previous, limits()),
            Some(SequentialStopReason::AttemptLimit)
        );
    }

    #[test]
    fn trace_evidence_counts_only_completed_failed_file_repair_cycles() {
        let temp = tempdir().unwrap();
        let trace = temp.path().join("trace.jsonl");
        let events = [
            json!({"kind": runtime_events::AGENT_VALIDATION_PROBE_OBSERVED, "payload": {"success": false, "had_pending_source_writes": false, "assertion_kind": "file_text_equals"}}),
            json!({"kind": runtime_events::AGENT_VALIDATION_PROBE_OBSERVED, "payload": {"success": false, "had_pending_source_writes": true, "assertion_kind": "file_text_equals"}}),
            json!({"kind": runtime_events::AGENT_VALIDATION_PROBE_OBSERVED, "payload": {"success": false, "had_pending_source_writes": true, "assertion_kind": "shell"}}),
            json!({"kind": runtime_events::AGENT_VALIDATION_PROBE_OBSERVED, "payload": {"success": true, "had_pending_source_writes": true, "assertion_kind": "file_text_equals"}}),
            json!({"kind": "agent.terminal.done_observed", "payload": {}}),
        ];
        let text = events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&trace, format!("{text}\n")).unwrap();

        assert_eq!(
            inspect_trace(&trace).unwrap(),
            TraceRetryEvidence {
                failed_repair_cycles: 1,
                passing_probes: 1,
                accepted_done: true,
                explicit_fail: false,
            }
        );
    }

    #[test]
    fn failed_inner_run_adopts_exactly_one_new_matching_trace() {
        let temp = tempdir().unwrap();
        let traces = temp.path().join("traces");
        std::fs::create_dir(&traces).unwrap();
        let root = temp.path().canonicalize().unwrap();
        let before = trace_paths(&traces).unwrap();
        let matching = traces.join("run-matching.jsonl");
        std::fs::write(
            &matching,
            format!(
                "{}\n{}\n",
                json!({"kind": "run.started", "payload": {"tool_root": root}}),
                json!({"kind": "run.failed", "payload": {"tool_root": root}})
            ),
        )
        .unwrap();
        std::fs::write(
            traces.join("run-other.jsonl"),
            format!(
                "{}\n",
                json!({"kind": "run.failed", "payload": {"tool_root": "/different/root"}})
            ),
        )
        .unwrap();

        assert_eq!(
            newly_failed_trace(&traces, &before, &root).unwrap(),
            matching
        );
    }

    #[test]
    fn failed_inner_run_trace_adoption_fails_closed_on_ambiguity() {
        let temp = tempdir().unwrap();
        let traces = temp.path().join("traces");
        std::fs::create_dir(&traces).unwrap();
        let root = temp.path().canonicalize().unwrap();
        let before = trace_paths(&traces).unwrap();
        for name in ["run-one.jsonl", "run-two.jsonl"] {
            std::fs::write(
                traces.join(name),
                format!(
                    "{}\n",
                    json!({"kind": "run.failed", "payload": {"tool_root": root}})
                ),
            )
            .unwrap();
        }

        assert!(newly_failed_trace(&traces, &before, &root).is_err());
    }

    #[test]
    fn artifact_snapshots_use_stable_sha256_and_distinguish_missing() {
        let temp = tempdir().unwrap();
        let missing = temp.path().join("missing.txt");
        assert_eq!(
            snapshot(&missing).unwrap(),
            ArtifactSnapshot {
                exists: false,
                bytes: 0,
                sha256: None,
            }
        );

        let artifact = temp.path().join("artifact.txt");
        std::fs::write(&artifact, b"abc").unwrap();
        assert_eq!(
            snapshot(&artifact).unwrap(),
            ArtifactSnapshot {
                exists: true,
                bytes: 3,
                sha256: Some(
                    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string()
                ),
            }
        );
    }

    #[test]
    fn worker_artifact_path_rejects_parent_and_symlink_escape() {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        assert_eq!(
            scoped_worker_path(&root, Path::new("missing.txt")).unwrap(),
            root.join("missing.txt")
        );
        assert!(scoped_worker_path(&root, Path::new("../escape")).is_err());

        #[cfg(unix)]
        {
            let outside = tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), root.join("linked")).unwrap();
            assert!(scoped_worker_path(&root, Path::new("linked")).is_err());
            assert!(scoped_worker_path(&root, Path::new("linked/missing.txt")).is_err());
        }
    }
}
