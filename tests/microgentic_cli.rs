use std::process::{Command, Stdio};

fn worker() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_microgentic"));
    command.stdin(Stdio::null());
    command
}

#[test]
fn missing_or_invalid_intake_never_starts_a_provider() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.json");
    for args in [
        vec![],
        vec!["run"],
        vec!["run", "--prompt", ""],
        vec!["run", "positional"],
        vec!["run", "--prompt", "one", "--prompt", "two"],
    ] {
        let output = worker()
            .arg("--config")
            .arg(&config)
            .args(args)
            .env("OMLX_HOST", "http://127.0.0.1:1")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Connection refused"));
    }
    assert!(!config.exists());
}

#[test]
fn benchmark_help_and_errors_explain_isolated_file_intake() {
    let help = worker()
        .args(["benchmark", "run", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("--experiment <EXPERIMENT>"));
    assert!(text.contains("--goal <GOAL>"));
    assert!(text.contains("stdin, --config, and TUI history are ignored"));
    assert!(!text.contains("echo 'Hello!'"));
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec!["benchmark", "run"],
        vec!["benchmark", "run", "--experiment", ".", "--prompt", "hello"],
    ] {
        let output = worker()
            .current_dir(temp.path())
            .args(args)
            .env("OMLX_HOST", "http://127.0.0.1:1")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Connection refused"));
        assert!(!temp.path().join("traces").exists());
    }
}

#[test]
fn benchmark_packets_match_task_runs_and_ignore_ambient_intake() {
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/benchmark_continuity.py"
        ))
        .arg(env!("CARGO_BIN_EXE_microgentic"))
        .arg(env!("CARGO_BIN_EXE_microgentic"))
        .arg("benchmark")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn setup_previews_before_saving_and_persists_model_default_effort() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("nested/config.json");
    let args = [
        "setup",
        "--provider",
        "ollama",
        "--model",
        "test-model",
        "--reasoning-effort",
        "default",
        "--json",
    ];
    let preview = worker()
        .arg("--config")
        .arg(&config)
        .args(args)
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert!(!config.exists());
    let save = worker()
        .arg("--config")
        .arg(&config)
        .args(args)
        .arg("--apply")
        .output()
        .unwrap();
    assert!(save.status.success());
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
    assert!(saved["reasoning_effort"].is_null());
    assert_eq!(saved["model"], "test-model");
}

#[test]
fn prompt_must_match_an_authoritative_contract_before_a_run_is_created() {
    let temp = tempfile::tempdir().unwrap();
    let contract = temp.path().join("contract.json");
    std::fs::write(&contract, r#"{"guidance":"original prompt","probes":[]}"#).unwrap();
    let traces = temp.path().join("traces");
    let output = worker()
        .args(["run", "--prompt", "changed prompt", "--experiment"])
        .arg(temp.path())
        .arg("--contract")
        .arg(&contract)
        .arg("--trace-dir")
        .arg(&traces)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must exactly match"));
    assert!(!traces.exists());
}

#[cfg(unix)]
#[test]
fn tui_opens_without_a_provider_and_restores_the_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/tui_smoke.py"))
        .arg(env!("CARGO_BIN_EXE_microgentic"))
        .arg(temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn tui_streams_and_keeps_native_history_without_leaking_into_runs() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/tui_conversation.py"
        ))
        .arg(env!("CARGO_BIN_EXE_microgentic"))
        .arg(temp.path())
        .arg(env!("CARGO_BIN_EXE_adaptive-agent-harness"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn tui_chats_naturally_and_can_perform_work_when_requested() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/tui_dialogue.py"
        ))
        .arg(env!("CARGO_BIN_EXE_microgentic"))
        .arg(temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn tui_slash_commands_stay_local_and_change_later_requests() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/tui_commands.py"
        ))
        .arg(env!("CARGO_BIN_EXE_microgentic"))
        .arg(temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn tui_shows_each_tool_call_as_one_live_line() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/tui_tools.py"))
        .arg(env!("CARGO_BIN_EXE_microgentic"))
        .arg(temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
