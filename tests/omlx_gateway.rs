//! Exercise production transports without loading a model, including known Mojentic 2.1 gaps.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};

fn read_request(stream: &mut TcpStream) -> (String, Value) {
    // Accepted sockets inherit nonblocking mode on macOS.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    let header_end = loop {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "connection ended before request headers");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    while bytes.len() < header_end + length {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "connection ended before request body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    (
        headers,
        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap(),
    )
}

fn mock_server(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<Value>>) {
    mock_provider_server(responses, "/v1/chat/completions", true)
}

fn mock_provider_server(
    responses: Vec<String>,
    route: &'static str,
    require_api_key: bool,
) -> (String, std::thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for body in responses {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "no provider request arrived");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            let (headers, request) = read_request(&mut stream);
            assert!(headers.starts_with(&format!("POST {route} HTTP/1.1")));
            if require_api_key {
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer test-key")
                );
            }
            requests.push(request);
            let content_type = if body.starts_with("data:") {
                "text/event-stream"
            } else {
                "application/json"
            };
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    (host, handle)
}

fn final_response() -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices": [{"delta": {"content": "DONE"}, "finish_reason": "stop"}]
        })
    )
}

fn run_cli(host: &str, effort: Option<&str>, budget_flag: &str) -> (tempfile::TempDir, Value) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(
        temp.path().join("task.md"),
        "Inspect the workspace if needed, then finish with DONE.\n",
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_adaptive-agent-harness"));
    command
        .current_dir(&workspace)
        .env("OMLX_HOST", host)
        .env("OMLX_API_KEY", "test-key")
        .args(["run", "--experiment"])
        .arg(temp.path())
        .args([budget_flag, "8192", "--max-iterations", "3"]);
    if let Some(effort) = effort {
        command.args(["--reasoning-effort", effort]);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary["final_summary"], "DONE");
    (temp, summary)
}

#[test]
fn worker_effort_and_token_budgets_reach_omlx_on_the_wire() {
    for effort in [
        None,
        Some("disabled"),
        Some("low"),
        Some("medium"),
        Some("high"),
    ] {
        let (host, server) = mock_server(vec![final_response()]);
        let (_temp, summary) = run_cli(&host, effort, "--max-tokens");
        let requests = server.join().unwrap();
        let request = &requests[0];
        assert_eq!(request["model"], "Qwen3.8-27B-MLX-8bit");
        assert_eq!(request["stream"], true);
        assert_eq!(request["max_tokens"], 8192);
        assert!(request.get("num_predict").is_none());
        assert!(request.get("num_ctx").is_none());
        assert!(
            request["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty())
        );
        if effort == Some("disabled") {
            assert_eq!(request["enable_thinking"], false);
            assert!(request.get("reasoning_effort").is_none());
        } else {
            assert!(request.get("enable_thinking").is_none());
            assert_eq!(
                request.get("reasoning_effort").and_then(Value::as_str),
                effort
            );
        }
        let trace = std::fs::read_to_string(summary["trace_file"].as_str().unwrap()).unwrap();
        assert!(!trace.contains("test-key"));
        assert!(trace.contains("\"provider\":\"omlx\""));
    }
}

#[test]
fn omlx_tool_results_survive_but_native_reasoning_is_only_traced() {
    let tool_response = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices": [{"delta": {
                "reasoning_content": "Inspect the workspace.",
                "tool_calls": [{"index": 0, "id": "inspect-1", "type": "function",
                    "function": {"name": "list_tree", "arguments": "{}"}}]
            }, "finish_reason": "tool_calls"}]
        })
    );
    let (host, server) = mock_server(vec![tool_response, final_response()]);
    let (_temp, summary) = run_cli(&host, Some("low"), "--num-predict");
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["max_tokens"], 8192);
    let messages = requests[1]["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == "inspect-1")
    );
    // Characterize the current gap, not the desired continuity contract. Mojentic
    // 2.1 has no assistant reasoning field; update this test when support lands.
    assert!(
        !serde_json::to_string(messages)
            .unwrap()
            .contains("Inspect the workspace.")
    );
    assert!(messages.iter().all(|message| {
        message.get("reasoning_content").is_none() && message.get("thinking").is_none()
    }));
    let trace = std::fs::read_to_string(summary["trace_file"].as_str().unwrap()).unwrap();
    assert!(trace.contains("\"kind\":\"llm.stream.thinking\""));
    assert!(trace.contains("Inspect the workspace."));
}

#[tokio::test]
async fn mojentic_21_ollama_named_efforts_collapse_to_boolean_on_the_wire() {
    use mojentic::llm::gateway::{CompletionConfig, LlmGateway, ReasoningEffort};
    use mojentic::llm::gateways::{OllamaConfig, OllamaGateway};
    use mojentic::llm::models::LlmMessage;

    // This dependency limitation makes a low/medium/high Ollama experiment
    // uninterpretable until the gateway preserves the requested level.
    for effort in [
        None,
        Some(ReasoningEffort::Disabled),
        Some(ReasoningEffort::Low),
        Some(ReasoningEffort::Medium),
        Some(ReasoningEffort::High),
    ] {
        let response = json!({"message": {"role": "assistant", "content": "DONE"}, "done": true});
        let (host, server) = mock_provider_server(vec![response.to_string()], "/api/chat", false);
        let gateway = OllamaGateway::with_config(OllamaConfig {
            host,
            timeout: Some(Duration::from_secs(10)),
            headers: Default::default(),
        });
        gateway
            .complete(
                "test-qwen",
                &[LlmMessage::user("Finish.")],
                None,
                &CompletionConfig {
                    reasoning_effort: effort,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let requests = server.join().unwrap();
        assert_eq!(
            requests[0].get("think"),
            effort
                .map(|level| json!(level != ReasoningEffort::Disabled))
                .as_ref()
        );
    }
}

#[test]
fn mojentic_21_cannot_represent_native_reasoning_history_or_xhigh() {
    use mojentic::llm::gateway::ReasoningEffort;
    use mojentic::llm::models::LlmMessage;

    let message: LlmMessage = serde_json::from_value(json!({
        "role": "assistant", "content": "Visible answer",
        "reasoning_content": "Reasoning sentinel", "thinking": "Reasoning sentinel"
    }))
    .unwrap();
    let serialized = serde_json::to_value(message).unwrap();
    assert_eq!(serialized["content"], "Visible answer");
    assert!(serialized.get("reasoning_content").is_none());
    assert!(serialized.get("thinking").is_none());
    assert!(serde_json::from_value::<ReasoningEffort>(json!("xhigh")).is_err());
}

#[tokio::test]
async fn structured_advisories_use_omlx_output_budgets() {
    use adaptive_agent_harness::semantic_advisory::{
        SemanticAdvisoryKind, SemanticAdvisoryRequest, request_semantic_advisory,
    };
    use adaptive_agent_harness::trace::TraceRecorder;
    use mojentic::llm::gateways::{OmlxConfig, OmlxGateway};
    use mojentic::llm::models::LlmMessage;

    let response = json!({"choices": [{"message": {"content": "{\"accepted\":true}"}, "finish_reason": "stop"}]}).to_string();
    let (host, server) = mock_server(vec![response]);
    let gateway = OmlxGateway::with_config(OmlxConfig {
        host,
        api_key: Some("test-key".into()),
        timeout: Some(Duration::from_secs(10)),
    });
    let temp = tempfile::tempdir().unwrap();
    let trace = TraceRecorder::create(temp.path()).unwrap();
    let messages = [LlmMessage::user("Assess the proposal.")];
    let response = request_semantic_advisory(&gateway, SemanticAdvisoryRequest {
        advisory_kind: SemanticAdvisoryKind::AcceptancePlanning,
        model: "test-qwen",
        messages: &messages,
        response_schema: json!({"type": "object", "properties": {"accepted": {"type": "boolean"}}, "required": ["accepted"], "additionalProperties": false}),
        max_input_chars: 1024,
        max_output_tokens: 256,
        temperature: 0.2,
        capture_reasoning: false,
    }, &trace).await.unwrap();
    assert_eq!(response.raw_proposal, json!({"accepted": true}));
    let requests = server.join().unwrap();
    assert_eq!(requests[0]["max_tokens"], 256);
    assert_eq!(requests[0]["response_format"]["type"], "json_schema");
    assert!(requests[0].get("num_predict").is_none());
    assert!(requests[0].get("reasoning_effort").is_none());
    assert!(requests[0].get("tools").is_none());
}
