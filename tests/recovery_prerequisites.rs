//! Positive telemetry prerequisite evidence for the exact reviewed Mojentic API.
//! These diagnostics deliberately do not claim recovery conformance.

use futures::StreamExt;
use mojentic::llm::gateway::{CompletionConfig, StreamChunk};
use mojentic::llm::gateways::{OllamaConfig, OllamaGateway};
use mojentic::llm::recovery::{RecoveryCall, RecoveryPolicy, RecoveryStreamEvent};
use mojentic::llm::{LlmGateway, LlmMessage};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

const BODY: &str = concat!(
    "{\"message\":{},\"done\":false}\n",
    "{\"message\":{\"thinking\":\"Synthetic reasoning.\"},\"done\":false}\n",
    "{\"message\":{\"content\":\"DONE\"},\"done\":false}\n",
    "{\"message\":{},\"done\":true,\"done_reason\":\"stop\",\"eval_count\":7,\"prompt_eval_count\":11,",
    "\"total_duration\":2000000000,\"eval_duration\":1000000000}\n",
);

fn server() -> (OllamaGateway, std::thread::JoinHandle<Vec<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..3 {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline, "no request arrived");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            let header_end = loop {
                let count = socket.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(index) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
            assert!(headers.starts_with("POST /api/chat HTTP/1.1"));
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            while bytes.len() < header_end + length {
                let count = socket.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                bytes.extend_from_slice(&buffer[..count]);
            }
            requests.push(bytes[header_end..header_end + length].to_vec());
            write!(socket,
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
                BODY.len()).unwrap();
        }
        requests
    });
    let gateway = OllamaGateway::with_config(OllamaConfig {
        host,
        timeout: None,
        headers: HashMap::new(),
    });
    (gateway, server)
}

#[tokio::test]
async fn reviewed_recovery_stream_preserves_worker_frame_progress() {
    if std::env::var_os("HARNESS_RECOVERY_DIAGNOSTIC_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "reviewed_recovery_stream_preserves_worker_frame_progress",
                "--nocapture",
            ])
            .env_clear()
            .env("HARNESS_RECOVERY_DIAGNOSTIC_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        println!("{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let (gateway, server) = server();
    let messages = [LlmMessage::user("Synthetic diagnostic")];
    let config = CompletionConfig::default();
    let mut stream = gateway.complete_stream("synthetic", &messages, None, &config);
    let mut legacy_progress = Vec::new();
    let mut legacy_metrics = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk.unwrap() {
            StreamChunk::Progress(progress) => legacy_progress.push(progress),
            StreamChunk::Metrics(metrics) => legacy_metrics.push(metrics),
            _ => {}
        }
    }
    drop(stream);
    assert_eq!(legacy_progress.len(), 4);
    assert_eq!(legacy_progress[0].content_chars, 0);
    assert_eq!(legacy_progress[0].thinking_chars, 0);
    assert_eq!(legacy_progress[0].tool_call_count, 0);
    assert!(!legacy_progress[0].done);
    assert_eq!(legacy_metrics.len(), 1);
    assert_eq!(legacy_metrics[0].eval_count, Some(7));
    assert_eq!(legacy_metrics[0].tokens_per_second, Some(7.0));

    let recovery = gateway.with_recovery(RecoveryPolicy::default()).unwrap();
    let call = RecoveryCall::default();
    let mut stream =
        recovery.complete_stream_with_recovery("synthetic", &messages, None, &config, &call);
    let mut events = Vec::new();
    let mut raw_progress = Vec::new();
    let mut raw_metrics = Vec::new();
    let mut raw_identity = None;
    while let Some(event) = stream.next().await {
        // Exhaustive match preserves semantic events and nonsemantic telemetry.
        match event {
            RecoveryStreamEvent::Progress { identity, progress } => {
                assert_eq!(identity.wire_attempt, 1);
                assert_eq!(raw_identity.get_or_insert(identity.clone()), &identity);
                raw_progress.push(progress);
                events.push("progress");
            }
            RecoveryStreamEvent::Metrics { identity, metrics } => {
                assert_eq!(raw_identity.as_ref(), Some(&identity));
                raw_metrics.push(metrics);
                events.push("metrics");
            }
            RecoveryStreamEvent::Content(content) => {
                assert_eq!(content, "DONE");
                events.push("content");
            }
            RecoveryStreamEvent::Reasoning(reasoning) => {
                assert_eq!(reasoning, "Synthetic reasoning.");
                events.push("reasoning");
            }
            RecoveryStreamEvent::ToolFragment(_) => panic!("no tool was requested"),
            RecoveryStreamEvent::Completed(response) => {
                assert_eq!(response.report.identity.wire_attempt, 1);
                assert_eq!(raw_identity.as_ref(), Some(&response.report.identity));
                assert_eq!(response.report.progress.raw_bytes, BODY.len() as u64);
                // Terminal evidence and exposed metadata retain the same actual metrics.
                let usage = response.value.evidence.usage.unwrap();
                assert_eq!(usage["eval_count"], 7);
                assert_eq!(usage["prompt_eval_count"], 11);
                assert_eq!(
                    response.value.evidence.metadata["eval_duration"],
                    1_000_000_000
                );
                events.push("completed");
            }
            RecoveryStreamEvent::Failed(error) => panic!("unexpected recovery failure: {error}"),
        }
    }
    assert_eq!(
        events,
        [
            "progress",
            "progress",
            "reasoning",
            "progress",
            "content",
            "progress",
            "metrics",
            "completed"
        ]
    );
    assert_eq!(raw_progress, legacy_progress);
    assert_eq!(raw_metrics, legacy_metrics);
    drop(stream);

    let mut stream = recovery.complete_stream("synthetic", &messages, None, &config);
    let mut convenience_progress = Vec::new();
    let mut convenience_metrics = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk.unwrap() {
            StreamChunk::Progress(progress) => convenience_progress.push(progress),
            StreamChunk::Metrics(metrics) => convenience_metrics.push(metrics),
            _ => {}
        }
    }
    assert_eq!(convenience_progress, legacy_progress);
    assert_eq!(convenience_metrics, legacy_metrics);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0], requests[1]);
    assert_eq!(requests[1], requests[2]);
    let artifacts = tempfile::tempdir().unwrap().keep();
    std::fs::write(artifacts.join("response.body"), BODY).unwrap();
    for (index, request) in requests.iter().enumerate() {
        std::fs::write(artifacts.join(format!("request-{index}.body")), request).unwrap();
    }
    std::fs::write(artifacts.join("assertions.json"), serde_json::to_vec_pretty(
        &serde_json::json!({
            "reviewed_revision": "4ca1ed279c02eab37827a1ed07c30e961155ecf3",
            "legacy_progress_frames": legacy_progress.len(),
            "legacy_metrics_frames": legacy_metrics.len(),
            "raw_recovery_events": events,
            "convenience_progress_frames": convenience_progress.len(),
            "convenience_metrics_frames": convenience_metrics.len(),
            "terminal_metrics_retained": true,
            "metadata_fields_and_order_preserved": true,
            "encoded_requests_identical": true,
            "recovery_conformance": false,
            "response_body": artifacts.join("response.body"),
            "request_bodies": (0..3).map(|index| artifacts.join(format!("request-{index}.body"))).collect::<Vec<_>>(),
        })
    ).unwrap()).unwrap();
    println!(
        "reviewed API prerequisite artifacts: {}",
        artifacts.display()
    );
}

#[test]
fn actual_worker_provider_paths_preserve_progress_prerequisites() {
    let artifacts = tempfile::tempdir().unwrap().keep();
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/recovery/worker_prerequisites.py"
        ))
        .arg("--worker")
        .arg(env!("CARGO_BIN_EXE_adaptive-agent-harness"))
        .arg("--artifacts")
        .arg(artifacts.join("recorded"))
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}
