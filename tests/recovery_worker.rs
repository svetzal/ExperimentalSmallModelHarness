//! Conformance is measured through the built public CLI and real provider HTTP adapters.
#[test]
fn actual_worker_recovery_matrix_both_adapters() {
    let destination = std::env::var_os("HARNESS_RECOVERY_EVIDENCE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| tempfile::tempdir().unwrap().keep())
        .join(format!("cargo-worker-matrix-{}", std::process::id()));
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/recovery/worker_conformance.py"
        ))
        .arg("--worker")
        .arg(env!("CARGO_BIN_EXE_adaptive-agent-harness"))
        .arg("--artifacts")
        .arg(&destination)
        .output()
        .unwrap();
    println!("worker assertion artifacts: {}", destination.display());
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
