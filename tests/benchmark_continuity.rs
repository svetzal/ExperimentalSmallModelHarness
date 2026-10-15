//! Benchmark continuity: two binaries given the same provider replies must
//! send the same provider requests, record the same packets and context
//! ledgers, and print summaries with the same schema.
//!
//! The permanent tests compare the current binary with itself: the legacy
//! top-level verbs against `benchmark <verb>` with ambient intake (a personal
//! config, stdin, project guidance and an unused goal) that must change
//! nothing. To compare against an older binary, build or keep one and run:
//!
//! ```text
//! MICROGENTIC_CONTINUITY_BASELINE=/path/to/old/microgentic \
//!     cargo test --test benchmark_continuity -- --ignored --nocapture
//! ```
//!
//! `MICROGENTIC_CONTINUITY_MODE=conversation` runs the narrower conversation
//! isolation check (contract runs through `run --prompt` on both binaries)
//! instead of the default benchmark mode. `MICROGENTIC_CONTINUITY_CURRENT`
//! replaces the current side, for example with this build's
//! `adaptive-agent-harness` to compare it with an older one.
mod support;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use support::*;

const PROMPT: &str = "Inspect the project with list_tree, then finish. Unicode 世界.";
const SENTINEL: &str = "PROJECT-GUIDANCE-SENTINEL";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Legacy verbs on the baseline, `benchmark <verb>` with ambient intake on
    /// the current binary, for every case.
    Benchmark,
    /// `run --prompt` with a contract on both binaries.
    Conversation,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Case {
    Contract,
    Goal,
    Sequential,
}

impl Case {
    fn name(self) -> &'static str {
        match self {
            Case::Contract => "contract",
            Case::Goal => "goal",
            Case::Sequential => "sequential",
        }
    }

    fn expected_requests(self) -> usize {
        if self == Case::Sequential { 4 } else { 2 }
    }
}

/// An experiment shared by both binaries in one comparison.
struct Experiment {
    _temp: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    contract: PathBuf,
    config: PathBuf,
}

impl Experiment {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("microgentic-continuity-")
            .tempdir()
            .expect("temporary directory");
        let base = temp.path().canonicalize().expect("canonical path");
        let root = base.join("project");
        std::fs::create_dir(&root).expect("project directory");
        // Project guidance must never reach benchmark or contract runs.
        std::fs::write(
            root.join("AGENTS.md"),
            format!("{SENTINEL}: benchmark runs must never see this.\n"),
        )
        .expect("project guidance");
        let contract = base.join("contract.json");
        std::fs::write(
            &contract,
            json!({
                "profile": {"id": "terminal_work", "version": "terminal_work_profile.v1"},
                "guidance": PROMPT,
                "read_scope": ["./**"],
                "write_scope": ["./**"],
                "probes": [],
            })
            .to_string(),
        )
        .expect("contract");
        let config = base.join("personal-config.json");
        std::fs::write(
            &config,
            "invalid personal config: must never be read by benchmarks",
        )
        .expect("personal config");
        std::fs::write(base.join("task.md"), PROMPT).expect("goal");
        std::fs::write(base.join("expected.txt"), "expected artifact\n")
            .expect("expected artifact");
        Self {
            _temp: temp,
            base,
            root,
            contract,
            config,
        }
    }
}

/// What one binary sent and recorded for one case.
struct Capture {
    requests: Vec<Value>,
    packets: Vec<Value>,
    ledgers: Vec<Value>,
    summary: Value,
}

/// Each odd request inspects the project with one tool call; each even one
/// finishes. Sequential runs pace their first requests because historical
/// trace file names have one-second precision.
fn inspect_then_finish(wire: Wire, case: Case) -> MockProvider {
    MockProvider::start(wire, move |request| {
        if request.number % 2 == 1 {
            let mut steps = Vec::new();
            if case == Case::Sequential {
                steps.push(Step::Pause(Duration::from_millis(1050)));
            }
            steps.push(content("Inspecting."));
            steps.push(tool_with_id("inspection", "list_tree", json!({})));
            Reply::Stream(steps)
        } else {
            Reply::Stream(vec![content("DONE")])
        }
    })
}

/// Whether `binary verb --help` lists `flag`.
fn accepts_flag(binary: &Path, verb: &str, flag: &str) -> bool {
    let help = std::process::Command::new(binary)
        .args([verb, "--help"])
        .output()
        .expect("asking a binary for help");
    String::from_utf8_lossy(&help.stdout)
        .split_whitespace()
        .any(|word| word.trim_end_matches(',') == flag)
}

fn path(path: &Path) -> String {
    path.display().to_string()
}

fn capture(
    experiment: &Experiment,
    mode: Mode,
    case: Case,
    wire: Wire,
    side: &str,
    binary: &Path,
) -> Capture {
    let provider = inspect_then_finish(wire, case);
    let trace_dir = experiment
        .base
        .join(format!("{}-{}-{side}", case.name(), wire.name()));
    let mut controls: Vec<String> = [
        "--experiment",
        &path(&experiment.base),
        "--trace-dir",
        &path(&trace_dir),
        "--provider",
        wire.name(),
        "--reasoning-effort",
        "low",
        "--max-tokens",
        "2048",
        "--max-model-interactions",
        "4",
        "--max-iterations",
        "3",
    ]
    .map(str::to_owned)
    .to_vec();
    if case != Case::Goal {
        controls.extend(["--contract".into(), path(&experiment.contract)]);
    }
    let verb = if case == Case::Sequential {
        "run-sequential"
    } else {
        "run"
    };
    if case == Case::Sequential {
        controls.extend(
            [
                "--artifact",
                "answer.txt",
                "--expected-artifact",
                "expected.txt",
                "--max-attempts",
                "2",
                "--max-consecutive-unchanged-attempts",
                "2",
            ]
            .map(str::to_owned),
        );
    }
    let mut env = provider.env();
    let current = side == "current";
    let mut stdin: &[u8] = b"";
    let args: Vec<String> = match mode {
        Mode::Benchmark => {
            env.push(("XDG_CONFIG_HOME", path(&experiment.base.join("config"))));
            env.push(("XDG_STATE_HOME", path(&experiment.base.join("state"))));
            if current {
                // Both historical file inputs may be supplied; the contract wins.
                if case != Case::Goal {
                    controls.extend(["--goal".into(), "missing-unused-goal.md".into()]);
                }
                stdin = b"ignore this ambient stdin";
                let mut args = vec![
                    "--config".into(),
                    path(&experiment.config),
                    "benchmark".into(),
                    verb.into(),
                ];
                args.extend(controls);
                args.extend(["--dir".into(), path(&experiment.root)]);
                args
            } else {
                let mut args = vec![verb.to_owned()];
                args.extend(controls);
                // Binaries from before `--json` print JSON without being asked.
                if accepts_flag(binary, verb, "--json") {
                    args.push("--json".into());
                }
                args
            }
        }
        Mode::Conversation => {
            let mut args = vec!["run".to_owned()];
            args.extend(controls);
            args.extend(["--prompt".into(), PROMPT.into(), "--json".into()]);
            args
        }
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run_binary(
        binary.to_str().expect("binary path"),
        &args,
        &experiment.root,
        &env,
        stdin,
    );
    let label = format!("{} {} {side}", case.name(), wire.name());
    assert!(
        output.status.success(),
        "{label}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{label}: the summary is not JSON: {error}"));
    if mode == Mode::Benchmark {
        assert!(
            !experiment.base.join("state").exists(),
            "{label}: benchmark created personal run state"
        );
    }
    let trace_files: Vec<PathBuf> = if case == Case::Sequential {
        summary["attempts"]
            .as_array()
            .expect("sequential attempts")
            .iter()
            .map(|attempt| attempt["trace_file"].as_str().expect("trace file").into())
            .collect()
    } else {
        vec![summary["trace_file"].as_str().expect("trace file").into()]
    };
    let records: Vec<Value> = trace_files
        .iter()
        .flat_map(|file| trace_records(file))
        .collect();
    let of_kind = |kind: &str| -> Vec<Value> {
        records
            .iter()
            .filter(|record| record["kind"] == kind)
            .map(|record| record["payload"].clone())
            .collect()
    };
    let packets = of_kind("llm.provider_request.assembled");
    let ledgers = of_kind("llm.context_assembly.ledger");
    assert!(
        !records.iter().any(|record| record["kind"]
            .as_str()
            .is_some_and(|kind| kind.starts_with("session."))),
        "{label}: a benchmark trace recorded session events"
    );
    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        case.expected_requests(),
        "{label}: requests"
    );
    assert_eq!(packets.len(), case.expected_requests(), "{label}: packets");
    if case == Case::Sequential {
        assert_eq!(
            requests[..2],
            requests[2..],
            "{label}: context leaked between task attempts"
        );
        assert_eq!(summary["stop_reason"], "consecutive_unchanged_attempts");
        assert_eq!(summary["confirmed_exact_success"], false);
    }
    assert!(
        !Value::from(requests.clone()).to_string().contains(SENTINEL),
        "{label}: project guidance reached a benchmark request"
    );
    Capture {
        requests,
        packets,
        ledgers,
        summary,
    }
}

/// Run one case on both binaries, assert continuity, and describe the result.
fn compare(baseline: &Path, current: &Path, mode: Mode, case: Case, wire: Wire) -> Value {
    let experiment = Experiment::new();
    let old = capture(&experiment, mode, case, wire, "baseline", baseline);
    let new = capture(&experiment, mode, case, wire, "current", current);
    let label = format!("{} {}", case.name(), wire.name());
    assert_eq!(
        old.requests, new.requests,
        "{label}: provider requests changed"
    );
    assert_eq!(
        old.packets, new.packets,
        "{label}: provider packets changed"
    );
    assert_eq!(old.ledgers, new.ledgers, "{label}: context ledgers changed");
    let keys = |summary: &Value| -> Vec<String> {
        let mut keys: Vec<String> = summary
            .as_object()
            .expect("summary object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    };
    assert_eq!(
        keys(&old.summary),
        keys(&new.summary),
        "{label}: summary schema changed"
    );
    if case == Case::Sequential {
        let attempts =
            |summary: &Value| summary["attempts"].as_array().cloned().unwrap_or_default();
        let (left, right) = (attempts(&old.summary), attempts(&new.summary));
        assert_eq!(left.len(), right.len(), "{label}: attempt count changed");
        for (left, right) in left.iter().zip(&right) {
            for key in [
                "final_summary",
                "accepted_done",
                "independent_exact",
                "terminal_error",
                "pre_artifact",
                "post_artifact",
                "artifact_unchanged",
            ] {
                assert_eq!(
                    left[key], right[key],
                    "{label}: sequential attempt {key} changed"
                );
            }
        }
    } else {
        assert_eq!(
            old.summary["final_summary"], new.summary["final_summary"],
            "{label}: final summary changed"
        );
    }
    json!({
        "case": case.name(),
        "provider": wire.name(),
        "requests_compared": case.expected_requests(),
        "provider_json_equal": true,
        "exact_snapshots_equal": true,
        "context_ledgers_equal": true,
        "summary_schema_equal": true,
    })
}

fn current_binary() -> PathBuf {
    PathBuf::from(microgentic())
}

fn self_check(case: Case, wire: Wire) {
    let binary = current_binary();
    compare(&binary, &binary, Mode::Benchmark, case, wire);
}

#[test]
fn benchmark_packets_match_task_runs_and_ignore_ambient_intake_contract_omlx() {
    self_check(Case::Contract, Wire::Omlx);
}

#[test]
fn benchmark_packets_match_task_runs_and_ignore_ambient_intake_contract_ollama() {
    self_check(Case::Contract, Wire::Ollama);
}

#[test]
fn benchmark_packets_match_task_runs_and_ignore_ambient_intake_goal_omlx() {
    self_check(Case::Goal, Wire::Omlx);
}

#[test]
fn benchmark_packets_match_task_runs_and_ignore_ambient_intake_goal_ollama() {
    self_check(Case::Goal, Wire::Ollama);
}

#[test]
fn benchmark_packets_match_task_runs_and_ignore_ambient_intake_sequential_omlx() {
    self_check(Case::Sequential, Wire::Omlx);
}

#[test]
fn benchmark_packets_match_task_runs_and_ignore_ambient_intake_sequential_ollama() {
    self_check(Case::Sequential, Wire::Ollama);
}

fn sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("reading a binary");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Compare an older binary named by `MICROGENTIC_CONTINUITY_BASELINE` with
/// the current one, and print a report with both binaries' hashes.
#[test]
#[ignore = "needs MICROGENTIC_CONTINUITY_BASELINE, the path of an older binary"]
fn continuity_against_a_baseline_binary() {
    let baseline = std::env::var_os("MICROGENTIC_CONTINUITY_BASELINE")
        .map(PathBuf::from)
        .expect("set MICROGENTIC_CONTINUITY_BASELINE to the path of an older microgentic binary");
    let baseline = baseline
        .canonicalize()
        .unwrap_or_else(|error| panic!("{}: {error}", baseline.display()));
    let mode = match std::env::var("MICROGENTIC_CONTINUITY_MODE").as_deref() {
        Ok("benchmark") | Err(_) => Mode::Benchmark,
        Ok("conversation") => Mode::Conversation,
        Ok(other) => {
            panic!("MICROGENTIC_CONTINUITY_MODE must be benchmark or conversation, not {other}")
        }
    };
    let current = std::env::var_os("MICROGENTIC_CONTINUITY_CURRENT")
        .map_or_else(current_binary, PathBuf::from)
        .canonicalize()
        .expect("current binary");
    let cases: &[Case] = match mode {
        Mode::Benchmark => &[Case::Contract, Case::Goal, Case::Sequential],
        Mode::Conversation => &[Case::Contract],
    };
    let mut results = Vec::new();
    for &case in cases {
        for wire in [Wire::Omlx, Wire::Ollama] {
            results.push(compare(&baseline, &current, mode, case, wire));
        }
    }
    let report = json!({
        "schema_version": match mode {
            Mode::Benchmark => "microgentic_benchmark_mode_check.v1",
            Mode::Conversation => "microgentic_conversation_isolation_check.v1",
        },
        "baseline_sha256": sha256(&baseline),
        "current_sha256": sha256(&current),
        "results": results,
    });
    println!("{}", serde_json::to_string_pretty(&report).expect("report"));
}
