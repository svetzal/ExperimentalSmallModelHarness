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

#[test]
fn calculate_answers_batches_and_stays_out_of_benchmark_runs() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/calculator_tool.py"
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

#[test]
fn project_guidance_reaches_everyday_runs_and_conversations() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/project_guidance.py"
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
