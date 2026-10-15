//! Exercise production transports and native reasoning continuity without loading a model.

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
        .unwrap_or(0);
    while bytes.len() < header_end + length {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "connection ended before request body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    let body = &bytes[header_end..header_end + length];
    (
        headers,
        if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(body).unwrap()
        },
    )
}

/// Everyday runs ask for the model's context window before the model
/// request. Answer without a window, so the run falls back as before.
fn is_model_metadata(headers: &str) -> bool {
    headers.starts_with("GET /v1/models ") || headers.starts_with("POST /api/show ")
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
            let (mut stream, headers, request) = loop {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "no provider request arrived");
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("accept failed: {error}"),
                };
                let (headers, request) = read_request(&mut stream);
                if is_model_metadata(&headers) {
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
                    continue;
                }
                break (stream, headers, request);
            };
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

#[test]
fn microgentic_and_legacy_entry_points_preserve_exact_provider_requests() {
    for provider in ["omlx", "ollama"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let prompt = "Inspect this project, then finish.\nUnicode: 世界\n";
        let contract = temp.path().join("contract.json");
        std::fs::write(&contract, json!({"profile":{"id":"terminal_work","version":"terminal_work_profile.v1"},"guidance":prompt,"read_scope":["./**"],"write_scope":["./**"],"probes":[]}).to_string()).unwrap();
        let config = temp.path().join("invalid-user-config.json");
        std::fs::write(&config, "malformed personal configuration").unwrap();
        let mut captured = Vec::new();
        for modern in [false, true] {
            let response = if provider == "omlx" {
                final_response()
            } else {
                format!(
                    "{}\n{}\n",
                    json!({"message":{"role":"assistant","content":"DONE"},"done":false}),
                    json!({"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop"})
                )
            };
            let inspect = if provider == "omlx" {
                format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[{"delta":{"content":"Inspecting.","tool_calls":[{"index":0,"id":"inspect-1","type":"function","function":{"name":"list_tree","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})
                )
            } else {
                format!(
                    "{}\n{}\n",
                    json!({"message":{"role":"assistant","content":"Inspecting.","tool_calls":[{"function":{"name":"list_tree","arguments":{}}}]},"done":false}),
                    json!({"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop"})
                )
            };
            let (host, server) = mock_provider_server(
                vec![inspect, response],
                if provider == "omlx" {
                    "/v1/chat/completions"
                } else {
                    "/api/chat"
                },
                provider == "omlx",
            );
            let trace = temp.path().join(if modern {
                "modern-traces"
            } else {
                "legacy-traces"
            });
            let mut command = Command::new(if modern {
                env!("CARGO_BIN_EXE_microgentic")
            } else {
                env!("CARGO_BIN_EXE_adaptive-agent-harness")
            });
            command
                .current_dir(if modern { temp.path() } else { &root })
                .env("OMLX_HOST", &host)
                .env("OLLAMA_HOST", &host)
                .env("OMLX_API_KEY", "test-key")
                .args(["--config"])
                .arg(&config)
                .args(["run", "--experiment"])
                .arg(temp.path())
                .arg("--contract")
                .arg(&contract)
                .arg("--trace-dir")
                .arg(&trace)
                .args([
                    "--provider",
                    provider,
                    "--reasoning-effort",
                    "low",
                    "--max-iterations",
                    "3",
                ]);
            if modern {
                command
                    .args(["--prompt", prompt, "--dir"])
                    .arg(&root)
                    .args([
                        "--json",
                        "--max-tokens",
                        "8192",
                        "--max-model-interactions",
                        "75",
                    ]);
            } else {
                command.args(["--num-predict", "8192", "--max-tool-iterations", "75"]);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                summary["tool_root"],
                root.canonicalize().unwrap().to_str().unwrap()
            );
            assert!(
                summary["final_summary"]
                    .as_str()
                    .unwrap()
                    .lines()
                    .any(|line| line == "DONE")
            );
            let requests = server.join().unwrap();
            assert_eq!(requests.len(), 2);
            captured.push(requests);
        }
        assert_eq!(
            captured[0], captured[1],
            "provider request changed for {provider}"
        );
    }
}

#[test]
fn saved_defaults_and_stdin_prompt_reach_the_public_worker_without_stdout_noise() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let config = temp.path().join("config.json");
    let setup = Command::new(env!("CARGO_BIN_EXE_microgentic"))
        .arg("--config")
        .arg(&config)
        .args([
            "setup",
            "--provider",
            "ollama",
            "--model",
            "custom-model",
            "--reasoning-effort",
            "low",
            "--apply",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let response = format!(
        "{}\n{}\n",
        json!({"message":{"role":"assistant","content":"DONE"},"done":false}),
        json!({"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop"})
    );
    let (host, server) = mock_provider_server(vec![response], "/api/chat", false);
    let mut child = Command::new(env!("CARGO_BIN_EXE_microgentic"))
        .arg("--config")
        .arg(&config)
        .current_dir(temp.path())
        .env("OLLAMA_HOST", &host)
        .env("XDG_STATE_HOME", temp.path().join("state"))
        .args(["run", "--dir"])
        .arg(&root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let prompt = "Hello!\nUnicode: 世界\n";
    child
        .stdin
        .take()
        .unwrap()
        .write_all(prompt.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"DONE\n");
    let requests = server.join().unwrap();
    assert_eq!(requests[0]["model"], "custom-model");
    assert_eq!(requests[0]["think"], "low");
    assert!(
        requests[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["content"]
                .as_str()
                .is_some_and(|text| text.contains(prompt)))
    );
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        0,
        "management artifacts entered the project"
    );
}

#[test]
fn built_worker_preserves_entry_stash_and_rejects_new_stash_both_providers() {
    for provider in ["ollama", "omlx"] {
        for create_new in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let workspace = temp.path().join("workspace");
            std::fs::create_dir(&workspace).unwrap();
            std::fs::write(
                temp.path().join("task.md"),
                "Check stash ownership and finish.",
            )
            .unwrap();
            let git = |args: &[&str]| {
                assert!(
                    Command::new("git")
                        .args(args)
                        .current_dir(&workspace)
                        .output()
                        .unwrap()
                        .status
                        .success()
                );
            };
            git(&["init"]);
            git(&["config", "user.email", "fixture@example.invalid"]);
            git(&["config", "user.name", "Fixture"]);
            std::fs::write(workspace.join("tracked.txt"), "seed").unwrap();
            git(&["add", "tracked.txt"]);
            git(&["commit", "-m", "seed"]);
            std::fs::write(workspace.join("tracked.txt"), "prior").unwrap();
            git(&["stash", "push"]);
            let before = Command::new("git")
                .args(["stash", "list", "--format=%H"])
                .current_dir(&workspace)
                .output()
                .unwrap()
                .stdout;
            let message = |content: &str| json!({"role":"assistant","content":content});
            let mut messages = vec![];
            if create_new {
                messages.push(json!({"role":"assistant","content":"","tool_calls":[{"id":"stash-tool","type":"function","function":{"name":"shell_command","arguments":{"command":"printf new > tracked.txt && git stash push"}}}]}));
            }
            messages.push(message("DONE"));
            if create_new {
                messages.push(message("FAIL unresolved stash"));
            }
            let responses = messages
                .into_iter()
                .map(|mut m| {
                    if provider == "ollama" {
                        format!(
                            "{}\n{}\n",
                            json!({"message":m,"done":false}),
                            json!({"message":{"content":""},"done":true,"done_reason":"stop"})
                        )
                    } else {
                        if let Some(calls) = m.get_mut("tool_calls").and_then(Value::as_array_mut) {
                            for call in calls {
                                call["function"]["arguments"] =
                                    json!(call["function"]["arguments"].to_string());
                                call["index"] = json!(0);
                            }
                        }
                        format!(
                            "data: {}\n\ndata: [DONE]\n\n",
                            json!({"choices":[{"delta":m,"finish_reason":"stop"}]})
                        )
                    }
                })
                .collect();
            let (host, server) = mock_provider_server(
                responses,
                if provider == "ollama" {
                    "/api/chat"
                } else {
                    "/v1/chat/completions"
                },
                provider == "omlx",
            );
            let output = Command::new(env!("CARGO_BIN_EXE_adaptive-agent-harness"))
                .current_dir(&workspace)
                .env("OLLAMA_HOST", &host)
                .env("OMLX_HOST", &host)
                .env("OMLX_API_KEY", "test-key")
                .args(["run", "--experiment"])
                .arg(temp.path())
                .args(["--provider", provider, "--max-iterations", "2"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                summary["final_summary"],
                if create_new {
                    "FAIL unresolved stash"
                } else {
                    "DONE"
                }
            );
            let requests = server.join().unwrap();
            assert!(requests[0]["messages"].as_array().unwrap().iter().any(|m| {
                m["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Git stash inventory at entry")
            }));
            if create_new {
                assert!(
                    requests.last().unwrap()["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|m| m["content"]
                            .as_str()
                            .unwrap_or("")
                            .contains("DONE withheld: Git stash state"))
                );
            }
            let after = Command::new("git")
                .args(["stash", "list", "--format=%H"])
                .current_dir(&workspace)
                .output()
                .unwrap()
                .stdout;
            assert!(after.ends_with(&before));
            assert_eq!(
                String::from_utf8(after).unwrap().lines().count(),
                if create_new { 2 } else { 1 }
            );
            let trace = std::fs::read_to_string(summary["trace_file"].as_str().unwrap()).unwrap();
            assert_eq!(trace.contains("agent.git_stash.done_rejected"), create_new);
        }
    }
}

fn run_cli(host: &str, effort: Option<&str>, budget_flag: &str) -> (tempfile::TempDir, Value) {
    run_cli_options(host, effort, budget_flag, &[])
}

fn run_cli_options(
    host: &str,
    effort: Option<&str>,
    budget_flag: &str,
    options: &[&str],
) -> (tempfile::TempDir, Value) {
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
        .env("OLLAMA_HOST", host)
        .env("OMLX_API_KEY", "test-key")
        .args(["run", "--experiment"])
        .arg(temp.path())
        .args([budget_flag, "8192", "--max-iterations", "3"]);
    if let Some(effort) = effort {
        command.args(["--reasoning-effort", effort]);
    }
    command.args(options);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        summary["final_summary"]
            .as_str()
            .unwrap()
            .lines()
            .any(|line| line.trim() == "DONE")
    );
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
        Some("xhigh"),
    ] {
        let (host, server) = mock_server(vec![final_response()]);
        let (_temp, summary) = run_cli(&host, effort, "--max-tokens");
        let requests = server.join().unwrap();
        let request = &requests[0];
        assert_eq!(request["model"], "Qwen3.8-27B-MLX-8bit");
        assert_eq!(request["stream"], true);
        assert_eq!(request["max_tokens"], 8192);
        assert_eq!(request["temperature"], 1.0);
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
    // Retention remains off by default, independently of reasoning effort.
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
async fn ollama_named_efforts_reach_the_wire_without_collapsing() {
    use mojentic::llm::gateway::{CompletionConfig, LlmGateway, ReasoningEffort};
    use mojentic::llm::gateways::{OllamaConfig, OllamaGateway};
    use mojentic::llm::models::LlmMessage;

    // Named effort and boolean enablement have distinct wire representations.
    for effort in [
        None,
        Some(ReasoningEffort::Disabled),
        Some(ReasoningEffort::Low),
        Some(ReasoningEffort::Medium),
        Some(ReasoningEffort::High),
        Some(ReasoningEffort::XHigh),
        Some(ReasoningEffort::Enabled),
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
                .map(|level| match level {
                    ReasoningEffort::Disabled => json!(false),
                    ReasoningEffort::Enabled => json!(true),
                    named => json!(named),
                })
                .as_ref()
        );
    }
}

#[test]
fn native_reasoning_history_and_xhigh_round_trip() {
    use mojentic::llm::gateway::ReasoningEffort;
    use mojentic::llm::models::LlmMessage;

    let message: LlmMessage = serde_json::from_value(json!({
        "role": "assistant", "content": "Visible answer",
        "reasoning_content": "Reasoning sentinel"
    }))
    .unwrap();
    let serialized = serde_json::to_value(message).unwrap();
    assert_eq!(serialized["content"], "Visible answer");
    assert_eq!(serialized["reasoning_content"], "Reasoning sentinel");
    assert!(serialized.get("thinking").is_none());
    assert_eq!(
        serde_json::from_value::<ReasoningEffort>(json!("xhigh")).unwrap(),
        ReasoningEffort::XHigh
    );
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

#[test]
fn native_reasoning_survives_tools_outer_turns_and_context_accounting() {
    let first = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{
        "reasoning_content":"FIRST_REASONING_SENTINEL", "content":"Inspecting now.",
        "tool_calls":[{"index":0,"id":"inspect-1","type":"function","function":{"name":"list_tree","arguments":"{}"}}]
    },"finish_reason":"tool_calls"}]})
    );
    let second = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{
        "reasoning_content":"SECOND_REASONING_SENTINEL", "content":"The inspection is complete; one more step."
    },"finish_reason":"stop"}]})
    );
    let (host, server) = mock_server(vec![first, second, final_response()]);
    let (_temp, summary) = run_cli_options(
        &host,
        Some("medium"),
        "--max-tokens",
        &[
            "--preserve-reasoning",
            "--reasoning-profile",
            "qwen3.8",
            "--transcript-policy",
            "full-transcript",
        ],
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0]["chat_template_kwargs"]["preserve_thinking"],
        true
    );
    assert_eq!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "assistant")
            .unwrap()["reasoning_content"],
        "FIRST_REASONING_SENTINEL"
    );
    let assistants: Vec<_> = requests[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "assistant")
        .collect();
    assert_eq!(
        assistants.len(),
        2,
        "no aggregate assistant duplicate across outer turns"
    );
    assert_eq!(
        assistants[1]["reasoning_content"],
        "SECOND_REASONING_SENTINEL"
    );
    let trace = std::fs::read_to_string(summary["trace_file"].as_str().unwrap()).unwrap();
    assert!(trace.contains("\"reasoning_chars\":24"));
    assert!(trace.contains("\"preserve_reasoning\":true"));
    assert!(trace.contains("qwen3.8"));
    assert_eq!(summary["preserve_reasoning"], true);
}

#[test]
fn capped_partial_reasoning_is_not_retained_as_completed_native_history() {
    let first = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{
        "reasoning_content":"PARTIAL_REASONING_SENTINEL that exceeds the protective cap"
    },"finish_reason":"stop"}]})
    );
    let (host, server) = mock_server(vec![first, final_response()]);
    let (_temp, summary) = run_cli_options(
        &host,
        Some("medium"),
        "--max-tokens",
        &["--preserve-reasoning", "--max-thinking-only-tokens", "1"],
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        !requests[1]["messages"]
            .to_string()
            .contains("PARTIAL_REASONING_SENTINEL")
    );
    let trace = std::fs::read_to_string(summary["trace_file"].as_str().unwrap()).unwrap();
    assert!(trace.contains("PARTIAL_REASONING_SENTINEL"));
    assert!(trace.contains("llm.thinking_only_stream.action_transitioned"));
}

#[test]
fn qwen_profile_rejects_high_before_connecting() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("task.md"), "Finish with DONE.").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_adaptive-agent-harness"))
        .current_dir(temp.path())
        .env("OMLX_HOST", "http://127.0.0.1:1")
        .args(["run", "--experiment"])
        .arg(temp.path())
        .args([
            "--reasoning-profile",
            "qwen3.8",
            "--reasoning-effort",
            "high",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported by profile qwen3.8"));
}

#[test]
fn ollama_worker_preserves_native_reasoning_and_records_provider_controls() {
    for preserve in [false, true] {
        let reply = |message: Value| {
            format!(
                "{}\n{}\n",
                json!({"message": message, "done": false}),
                json!({"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop"})
            )
        };
        let (host, server) = mock_provider_server(
            vec![
                reply(
                    json!({"role":"assistant","content":"Inspecting.","thinking":"NATIVE_SENTINEL","tool_calls":[{"function":{"name":"list_tree","arguments":{}}}]}),
                ),
                reply(json!({"role":"assistant","content":"DONE"})),
            ],
            "/api/chat",
            false,
        );
        let mut options = vec!["--provider", "ollama", "--reasoning-profile", "qwen3.8"];
        if preserve {
            options.push("--preserve-reasoning");
        }
        let (_temp, summary) = run_cli_options(&host, Some("medium"), "--max-tokens", &options);
        let requests = server.join().unwrap();
        assert_eq!(summary["provider"], "ollama");
        assert_eq!(requests.len(), 2);
        for request in &requests {
            assert_eq!(request["model"], "qwen3.8:27b-mxfp8");
            assert_eq!(request["think"], "medium");
            assert_eq!(request["options"]["num_predict"], 8192);
            assert_eq!(request["options"]["temperature"], 1.0);
            assert_eq!(request["options"]["num_ctx"], 262144);
            assert!(request.get("enable_thinking").is_none());
        }
        let assistants: Vec<_> = requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "assistant")
            .collect();
        assert_eq!(assistants.len(), 1);
        assert_eq!(
            assistants[0]["thinking"],
            if preserve {
                json!("NATIVE_SENTINEL")
            } else {
                Value::Null
            }
        );
        let events: Vec<Value> = std::fs::read_to_string(summary["trace_file"].as_str().unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let snapshots: Vec<_> = events
            .iter()
            .filter(|e| e["kind"] == "llm.provider_request.assembled")
            .collect();
        assert_eq!(snapshots.len(), 2);
        for snapshot in snapshots {
            assert_eq!(snapshot["payload"]["provider"], "ollama");
            assert_eq!(snapshot["payload"]["provider_controls"]["think"], "medium");
            assert_eq!(
                snapshot["payload"]["provider_controls"]["num_predict"],
                8192
            );
            assert_eq!(snapshot["payload"]["provider_controls"]["num_ctx"], 262144);
        }
    }
}
