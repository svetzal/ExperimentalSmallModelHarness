//! Everyday runs against a mock provider, on both providers: natural reply
//! completion, the calculate tool and project guidance. No terminal and no
//! model.
mod support;

use serde_json::{Value, json};
use std::path::Path;
use support::*;

/// Run `microgentic` against `provider` and return its parsed JSON summary.
fn run_json(provider: &MockProvider, cwd: &Path, env: &[(&str, String)], args: &[&str]) -> Value {
    let mut full_env = provider.env();
    full_env.extend(env.iter().cloned());
    let output = run_binary(microgentic(), args, cwd, &full_env, b"");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("the summary is JSON")
}

fn trace_of(summary: &Value) -> std::path::PathBuf {
    summary["trace_file"]
        .as_str()
        .expect("the summary names its trace")
        .into()
}

/// Everyday requests carry the literal prompt, never the task template or
/// task continuation prompts.
fn assert_everyday(requests: &[Value]) {
    for (number, request) in requests.iter().enumerate() {
        let text = request["messages"].to_string();
        for task_text in [
            "benchmark task",
            "Continue from the current experiment state",
            "reply exactly DONE",
        ] {
            assert!(
                !text.contains(task_text),
                "request {} contains {task_text:?}: {text}",
                number + 1
            );
        }
    }
}

fn tool_json(request: &Value) -> Value {
    serde_json::from_str(&last_tool_result(request)).expect("the tool result is JSON")
}

// ---------------------------------------------------------------- calculate

#[test]
fn calculate_answers_batches_and_stays_out_of_benchmark_runs_omlx() {
    calculate(Wire::Omlx);
}

#[test]
fn calculate_answers_batches_and_stays_out_of_benchmark_runs_ollama() {
    calculate(Wire::Ollama);
}

fn calculate(wire: Wire) {
    let workspace = Workspace::new();
    let env = [
        ("XDG_STATE_HOME", workspace.state.display().to_string()),
        (
            "XDG_CONFIG_HOME",
            workspace.base.join("config").display().to_string(),
        ),
    ];

    // An everyday run offers calculate; a batch comes back one line each.
    let provider = MockProvider::turns(
        wire,
        vec![
            vec![tool(
                "calculate",
                json!({"expression": "140+90+70+120+350+60+60+154+60+280+90\n1474/1.1"}),
            )],
            vec![tool("calculate", json!({"expression": "1354 / (1.1"}))],
            vec![tool(
                "calculate",
                json!({"expression": "total = 140 + 90\ntotal / 1.1\n(total / 1000) as secs"}),
            )],
            vec![content(
                "The total is 1474 ms and the new length is 1340 ms.",
            )],
        ],
    );
    let summary = run_json(
        &provider,
        &workspace.project,
        &env,
        &[
            "run",
            "--prompt",
            "Add up the stage holds, then divide by 1.1.",
            "--json",
            "--provider",
            wire.name(),
            "--model",
            "test-model",
        ],
    );
    let requests = provider.requests();
    // Three calculations, then the reply ends the run.
    assert_eq!(requests.len(), 4, "one request per turn");
    assert_everyday(&requests);
    assert_eq!(
        summary["final_summary"],
        "The total is 1474 ms and the new length is 1340 ms."
    );
    assert!(
        tool_names(&requests[0]).contains(&"calculate".to_owned()),
        "{:?}",
        tool_names(&requests[0])
    );
    assert_eq!(
        tool_json(&requests[1]),
        json!({"results": [
            "140 + 90 + 70 + 120 + 350 + 60 + 60 + 154 + 60 + 280 + 90 = 1474",
            "1474 / 1.1 = 1340",
        ]})
    );
    // The error names the column and the unclosed bracket.
    assert_eq!(
        tool_json(&requests[2]),
        json!({"error": "Tool error: 1354 / (1.1  ->  error at column 12: missing \")\"; \
                         the \"(\" at column 8 is never closed"})
    );
    // A batch with some good lines still answers; the bad line says why.
    let mixed = tool_json(&requests[3]);
    let mixed = mixed["results"].as_array().expect("results");
    assert_eq!(
        mixed,
        &[
            json!("total = 140 + 90 = 230"),
            json!("total / 1.1 = 209.090909091"),
            json!(
                "(total / 1000) as secs  ->  error at column 16: unexpected \"as\" after the \
                 expression; write only arithmetic, or name the value first, such as \
                 secs = 1474 / 1000"
            ),
        ]
    );
    let results: Vec<Value> = trace_payloads(&trace_of(&summary), "llm.context_assembly.appended")
        .into_iter()
        .filter(|payload| payload["component"] == "tool_result")
        .map(|payload| payload["ok"].clone())
        .collect();
    assert_eq!(results, [json!(true), json!(false), json!(true)]);

    // Benchmark runs keep their exact tool set.
    let experiment = &workspace.base;
    let contract = workspace.base.join("contract.json");
    std::fs::write(
        &contract,
        json!({
            "profile": {"id": "terminal_work", "version": "terminal_work_profile.v1"},
            "guidance": "Finish.",
            "read_scope": ["./**"],
            "write_scope": ["./**"],
            "probes": [],
        })
        .to_string(),
    )
    .expect("writing the contract");
    let provider = MockProvider::replying(wire, vec![content("DONE")]);
    let mut full_env = provider.env();
    full_env.extend(env.iter().cloned());
    let traces = workspace.base.join("bench-traces");
    let output = run_binary(
        microgentic(),
        &[
            "benchmark",
            "run",
            "--experiment",
            &experiment.display().to_string(),
            "--contract",
            &contract.display().to_string(),
            "--trace-dir",
            &traces.display().to_string(),
            "--provider",
            wire.name(),
            "--dir",
            &workspace.project.display().to_string(),
            "--max-model-interactions",
            "2",
            "--max-iterations",
            "1",
        ],
        &workspace.project,
        &full_env,
        b"",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = provider.requests();
    assert!(!requests.is_empty(), "the benchmark never asked the model");
    assert!(
        !tool_names(&requests[0]).contains(&"calculate".to_owned()),
        "{:?}",
        tool_names(&requests[0])
    );
}

// ---------------------------------------------------------------- guidance

#[test]
fn project_guidance_reaches_everyday_runs_and_conversations_omlx() {
    project_guidance(Wire::Omlx);
}

#[test]
fn project_guidance_reaches_everyday_runs_and_conversations_ollama() {
    project_guidance(Wire::Ollama);
}

fn project_guidance(wire: Wire) {
    let workspace = Workspace::new();
    let root = &workspace.project;
    std::fs::create_dir_all(root.join(".git")).expect("a repository marker");
    workspace.write("AGENTS.md", "ROOT-RULE: run tests with make check.\n");
    workspace.write("sub/AGENTS.md", "SUB-RULE: in sub, answer in French.\n");
    workspace.write("sub/notes.txt", "notes\n");
    let config_home = workspace.base.join("config");
    let user_guidance = config_home.join("microgentic").join("AGENTS.md");
    std::fs::create_dir_all(user_guidance.parent().unwrap()).expect("user config directory");
    std::fs::write(&user_guidance, "USER-RULE: be brief.\n").expect("user guidance");
    let env = [
        ("XDG_STATE_HOME", workspace.state.display().to_string()),
        ("XDG_CONFIG_HOME", config_home.display().to_string()),
    ];
    let run_args = |extra: &[&'static str]| -> Vec<String> {
        let mut args: Vec<String> = [
            "run",
            "--prompt",
            "Check the notes.",
            "--json",
            "--provider",
            wire.name(),
            "--model",
            "test-model",
        ]
        .map(str::to_owned)
        .to_vec();
        args.extend(extra.iter().map(|arg| (*arg).to_owned()));
        args
    };
    let read_notes = || tool("read_file", json!({"path": "sub/notes.txt"}));

    let provider = MockProvider::turns(
        wire,
        vec![
            vec![read_notes()],
            vec![read_notes()],
            vec![content("Done.")],
        ],
    );
    let args = run_args(&[]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let summary = run_json(&provider, root, &env, &args);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3, "two reads, then the reply ends the run");
    assert_everyday(&requests);
    let prompt = system_message(&requests[0]);
    assert!(prompt.contains("# Project guidance"), "{prompt}");
    let user_header = format!("## {} (user guidance)", user_guidance.display());
    assert!(prompt.contains(&user_header), "{prompt}");
    assert!(
        prompt.contains("## AGENTS.md (project directory and repository root)"),
        "{prompt}"
    );
    assert!(
        prompt.find("USER-RULE").unwrap() < prompt.find("ROOT-RULE").unwrap(),
        "user guidance must come first"
    );
    assert!(prompt.contains("- sub/ (sub/AGENTS.md)"), "{prompt}");
    assert!(!prompt.contains("SUB-RULE"), "{prompt}");
    // The nested file arrives after the first read under sub/, once.
    assert!(
        !user_messages(&requests[0])
            .iter()
            .any(|text| text.contains("SUB-RULE"))
    );
    let injected: Vec<String> = user_messages(&requests[1])
        .into_iter()
        .filter(|text| text.starts_with("[Project guidance for sub/ from sub/AGENTS.md."))
        .collect();
    assert_eq!(injected.len(), 1, "{:?}", user_messages(&requests[1]));
    assert!(injected[0].contains("SUB-RULE"));
    assert_eq!(
        user_messages(&requests[2])
            .iter()
            .filter(|text| text.contains("SUB-RULE"))
            .count(),
        1,
        "nested guidance was injected twice"
    );
    let trace = trace_of(&summary);
    let discovered = trace_payloads(&trace, "context.guidance.discovered");
    assert_eq!(discovered.len(), 1, "guidance is discovered once per run");
    let candidates = discovered[0]["candidates"].as_array().expect("candidates");
    let statuses: std::collections::BTreeMap<String, String> = candidates
        .iter()
        .map(|candidate| {
            (
                candidate["shown"].as_str().unwrap().to_owned(),
                candidate["status"]["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let expected: std::collections::BTreeMap<String, String> = [
        (user_guidance.display().to_string(), "included"),
        ("AGENTS.md".into(), "included"),
        ("sub/AGENTS.md".into(), "pending"),
    ]
    .into_iter()
    .map(|(shown, status)| (shown, status.to_owned()))
    .collect();
    assert_eq!(statuses, expected);
    assert_eq!(candidates.len(), 3, "a candidate appeared twice");
    assert!(candidates.iter().all(|candidate| {
        candidate["sha256"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64)
    }));
    assert_eq!(
        trace_payloads(&trace, "context.guidance.nested_injected").len(),
        1,
        "the nested file was injected once"
    );
    // Residency: root guidance at the start, nested guidance on delivery,
    // and the second read under sub/ references it.
    let activated: Vec<(String, String)> = trace_payloads(&trace, "context.residency.activated")
        .iter()
        .map(|event| {
            (
                event["item"].as_str().unwrap().to_owned(),
                event["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        activated,
        [
            (
                format!("guidance:{}", user_guidance.display()),
                "root_guidance".to_owned()
            ),
            ("guidance:AGENTS.md".to_owned(), "root_guidance".to_owned()),
            (
                "guidance:sub/AGENTS.md".to_owned(),
                "nested_guidance".to_owned()
            ),
        ]
    );
    let referenced = trace_payloads(&trace, "context.residency.referenced");
    assert_eq!(referenced.len(), 1, "{referenced:?}");
    assert_eq!(referenced[0]["item"], "guidance:sub/AGENTS.md");
    assert_eq!(referenced[0]["via"], "tool_call");
    assert_eq!(referenced[0]["path"], "sub/notes.txt");
    let ledgers = trace_payloads(&trace, "llm.context_assembly.ledger");
    let last = ledgers.last().unwrap();
    let item = |id: &str| {
        last["residency"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing from {}", last["residency"]))
            .clone()
    };
    // Tool results continue the same turn at a deeper call.
    assert_eq!(item("guidance:sub/AGENTS.md")["last_referenced_turn"], 1);
    assert_eq!(item("guidance:AGENTS.md")["pinned"], true);
    assert_eq!(item("tool:read_file")["kind"], "tool_descriptor");
    assert_eq!(item("tool:read_file")["pinned"], true);
    assert_eq!(item("prompt")["inclusion_reason"], "current_user_prompt");
    assert_eq!(last["standing_context"]["items"], 3);
    assert_eq!(last["standing_context"]["enforced"], false);

    // Guidance never changes the tools; the flag removes it entirely.
    let plain = MockProvider::replying(wire, vec![content("Done.")]);
    let args = run_args(&["--no-project-guidance"]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let summary = run_json(&plain, root, &env, &args);
    let plain_requests = plain.requests();
    assert_eq!(plain_requests.len(), 1, "a plain reply ends the run");
    assert!(!plain_requests[0].to_string().contains("Project guidance"));
    assert_eq!(tool_names(&plain_requests[0]), tool_names(&requests[0]));
    assert!(!trace_records(&trace_of(&summary)).iter().any(|record| {
        record["kind"]
            .as_str()
            .is_some_and(|kind| kind.starts_with("context.guidance"))
    }));

    // The conversation worker reads guidance too, and notices changes between exchanges.
    let session = workspace.base.join("session").join("conversation.json");
    std::fs::create_dir_all(session.parent().unwrap()).expect("session directory");
    let worker = MockProvider::replying(wire, vec![content("Hello.")]);
    let mut full_env = worker.env();
    full_env.extend(env.iter().cloned());
    for prompt in ["Hi.", "Again."] {
        let output = run_binary(
            microgentic(),
            &[
                "conversation-worker",
                "--prompt",
                prompt,
                "--session-state",
                &session.display().to_string(),
                "--provider",
                wire.name(),
                "--model",
                "test-model",
            ],
            root,
            &full_env,
            b"",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        workspace.write("AGENTS.md", "ROOT-RULE: run tests with make verify.\n");
    }
    let requests = worker.requests();
    assert_eq!(requests.len(), 2);
    assert!(system_message(&requests[0]).contains("ROOT-RULE: run tests with make check."));
    assert!(system_message(&requests[1]).contains("ROOT-RULE: run tests with make verify."));
    let first = user_messages(&requests[0]);
    assert!(
        !first
            .last()
            .unwrap()
            .contains("project guidance was updated")
    );
    let second = user_messages(&requests[1]);
    assert!(
        second
            .last()
            .unwrap()
            .contains("project guidance was updated since the last message (AGENTS.md changed)"),
        "{second:?}"
    );
}

// ---------------------------------------------------------------- replies

/// Run an everyday `microgentic run` and require a clean exit.
fn everyday(
    provider: &MockProvider,
    workspace: &Workspace,
    args: &[&str],
    stdin: &[u8],
) -> std::process::Output {
    let mut env = provider.env();
    env.push(("XDG_STATE_HOME", workspace.state.display().to_string()));
    env.push((
        "XDG_CONFIG_HOME",
        workspace.base.join("config").display().to_string(),
    ));
    let mut full = vec![
        "run",
        "--provider",
        provider.wire().name(),
        "--model",
        "test-model",
    ];
    full.extend_from_slice(args);
    let output = run_binary(microgentic(), &full, &workspace.project, &env, stdin);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

/// A plain question takes one request, through `--prompt` or stdin, and
/// stdout holds only the reply.
fn plain_question(wire: Wire) {
    let workspace = Workspace::new();
    let reply = "Hello! A crate is a compilation unit in Rust.";
    for (args, stdin) in [
        (&["--prompt", "Hello! What is a crate?"][..], &b""[..]),
        (&[][..], &b"Hello! What is a crate?"[..]),
    ] {
        // Replies often open with blank lines; stdout holds the reply alone.
        let provider = MockProvider::replying(wire, vec![content("\n\n"), content(reply)]);
        let output = everyday(&provider, &workspace, args, stdin);
        assert_eq!(stdout(&output), format!("{reply}\n"));
        let requests = provider.requests();
        assert_eq!(requests.len(), 1, "a natural reply was forced to continue");
        assert_everyday(&requests);
        assert_eq!(
            user_messages(&requests[0]),
            ["Hello! What is a crate?"],
            "the prompt is delivered literally"
        );
        assert!(system_message(&requests[0]).contains("No engineering task is assumed"));
        assert!(
            tool_names(&requests[0]).contains(&"read_file".to_owned()),
            "tools stay available"
        );
    }
}

#[test]
fn a_plain_question_takes_one_request_omlx() {
    plain_question(Wire::Omlx);
}

#[test]
fn a_plain_question_takes_one_request_ollama() {
    plain_question(Wire::Ollama);
}

/// A question that needs one tool takes two requests: the call, then the reply.
fn one_tool_question(wire: Wire) {
    let workspace = Workspace::new();
    workspace.write("notes.txt", "PACE = 1.1\n");
    let reply = "The pace is 1.1.";
    let provider = MockProvider::turns(
        wire,
        vec![
            vec![tool("read_file", json!({"path": "notes.txt"}))],
            vec![content(reply)],
        ],
    );
    let output = everyday(
        &provider,
        &workspace,
        &["--prompt", "What is the pace in notes.txt?"],
        b"",
    );
    assert_eq!(stdout(&output), format!("{reply}\n"));
    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "the tool call, then the reply");
    assert_everyday(&requests);
    assert!(last_tool_result(&requests[1]).contains("PACE = 1.1"));
}

#[test]
fn a_question_needing_one_tool_takes_two_requests_omlx() {
    one_tool_question(Wire::Omlx);
}

#[test]
fn a_question_needing_one_tool_takes_two_requests_ollama() {
    one_tool_question(Wire::Ollama);
}

/// A request for work still does the work, checks it, and reports naturally.
fn edit_and_check(wire: Wire) {
    let workspace = Workspace::new();
    workspace.write("greeting.txt", "Hi there\n");
    let reply = "Changed the greeting to Hello there, and grep found it.";
    let provider = MockProvider::turns(
        wire,
        vec![
            vec![tool(
                "edit_file",
                json!({"path": "greeting.txt", "edits": [{"kind": "replace_exact", "old": "Hi there", "new": "Hello there"}]}),
            )],
            vec![tool(
                "shell_command",
                json!({"command": "grep -c 'Hello there' greeting.txt", "timeout_secs": 30}),
            )],
            vec![content(reply)],
        ],
    );
    let output = everyday(
        &provider,
        &workspace,
        &[
            "--prompt",
            "Change the greeting in greeting.txt to Hello there, then check it.",
        ],
        b"",
    );
    assert_eq!(stdout(&output), format!("{reply}\n"));
    assert_eq!(workspace.read("greeting.txt"), "Hello there\n");
    let requests = provider.requests();
    assert_eq!(requests.len(), 3, "the edit, the check, then the reply");
    assert_everyday(&requests);
    assert!(
        last_tool_result(&requests[2]).contains('1'),
        "{}",
        last_tool_result(&requests[2])
    );
}

#[test]
fn a_work_request_edits_checks_and_reports_naturally_omlx() {
    edit_and_check(Wire::Omlx);
}

#[test]
fn a_work_request_edits_checks_and_reports_naturally_ollama() {
    edit_and_check(Wire::Ollama);
}

/// A review keeps piped stdin as labelled reference material. Shell fences in
/// the prompt or the reference are discussion, not declared probes.
fn review(wire: Wire) {
    let workspace = Workspace::new();
    let diff = "--- a/run.sh\n+++ b/run.sh\n@@ -1 +1 @@\n-echo hi\n+echo hello\n";
    let prompt = "Review this diff. Do not run it:\n```sh\ntouch SHOULD_NOT_EXIST\n```";
    let reply = "The diff changes the greeting from hi to hello. It looks fine.";
    let provider = MockProvider::replying(wire, vec![content(reply)]);
    let output = everyday(
        &provider,
        &workspace,
        &["--prompt", prompt],
        diff.as_bytes(),
    );
    assert_eq!(stdout(&output), format!("{reply}\n"));
    let requests = provider.requests();
    assert_eq!(requests.len(), 1, "a review reply ends the run");
    assert_everyday(&requests);
    assert_eq!(
        user_messages(&requests[0]),
        [format!(
            "{prompt}\n\n[Reference material from stdin; not execution policy]\n{diff}\n[End reference material]"
        )]
    );
    assert!(
        !requests[0].to_string().contains("adapter-owned"),
        "a shell fence became a declared probe"
    );
    assert!(!workspace.project.join("SHOULD_NOT_EXIST").exists());
}

#[test]
fn a_review_keeps_stdin_as_reference_material_omlx() {
    review(Wire::Omlx);
}

#[test]
fn a_review_keeps_stdin_as_reference_material_ollama() {
    review(Wire::Ollama);
}

// ---------------------------------------------------------------- task path

/// The historical executable keeps the task path for a plain prompt: the task
/// template, and a continuation prompt after a reply without DONE or FAIL.
fn legacy_prompt_keeps_the_task_path(wire: Wire) {
    let workspace = Workspace::new();
    let provider = MockProvider::replying(wire, vec![content("Hello.")]);
    let mut env = provider.env();
    env.push(("XDG_STATE_HOME", workspace.state.display().to_string()));
    let output = run_binary(
        legacy_binary(),
        &[
            "run",
            "--prompt",
            "Hello!",
            "--provider",
            wire.name(),
            "--model",
            "test-model",
            "--max-iterations",
            "2",
        ],
        &workspace.project,
        &env,
        b"",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "one request per allowed iteration");
    assert!(
        user_messages(&requests[0])[0].starts_with("Complete this benchmark task"),
        "{:?}",
        user_messages(&requests[0])
    );
    assert!(
        user_messages(&requests[1])
            .last()
            .unwrap()
            .starts_with("Continue from the current experiment state"),
        "{:?}",
        user_messages(&requests[1])
    );
    assert!(!tool_names(&requests[0]).contains(&"calculate".to_owned()));
}

#[test]
fn the_legacy_binary_keeps_the_task_path_for_a_plain_prompt_omlx() {
    legacy_prompt_keeps_the_task_path(Wire::Omlx);
}

#[test]
fn the_legacy_binary_keeps_the_task_path_for_a_plain_prompt_ollama() {
    legacy_prompt_keeps_the_task_path(Wire::Ollama);
}

// ---------------------------------------------------------------- context window

/// The JSON summary of an everyday run of `prompt`.
fn everyday_json(provider: &MockProvider, workspace: &Workspace, prompt: &str) -> Value {
    let output = everyday(provider, workspace, &["--prompt", prompt, "--json"], b"");
    serde_json::from_slice(&output.stdout).expect("the summary is JSON")
}

/// An everyday run asks the provider for the model's window, traces it with
/// its source, and accounts against it, without changing the request.
fn discovered_window(wire: Wire) {
    let workspace = Workspace::new();
    let unknown = MockProvider::replying(wire, vec![content("Hello.")]);
    let before = everyday_json(&unknown, &workspace, "Hello!");
    let known = MockProvider::replying(wire, vec![content("Hello.")]);
    known.advertise_window(Some(262_144));
    let after = everyday_json(&known, &workspace, "Hello!");

    let resolved = |summary: &Value| {
        trace_payloads(&trace_of(summary), "context.window.resolved")
            .pop()
            .expect("the window is traced")
    };
    let window = resolved(&after);
    assert_eq!(window["tokens"], 262_144);
    assert_eq!(window["source"], "provider");
    assert_eq!(
        window["field"],
        match wire {
            Wire::Omlx => "max_model_len",
            Wire::Ollama => "model_info.test.context_length",
        }
    );
    let fallback = resolved(&before);
    assert_eq!(fallback["source"], "unknown");
    assert!(fallback["tokens"].is_null());
    assert!(
        fallback["discovery_error"]
            .as_str()
            .unwrap()
            .contains("does not advertise"),
        "{fallback}"
    );
    let ledger = trace_payloads(&trace_of(&after), "llm.context_assembly.ledger").remove(0);
    assert_eq!(ledger["known_window"]["tokens"], 262_144);
    assert_eq!(ledger["known_window"]["pressure_band"], "green");
    assert!(
        ledger["context_window_tokens"].is_null(),
        "the policy window is unchanged"
    );
    match wire {
        Wire::Omlx => assert_eq!(known.gets(), ["/v1/models"]),
        Wire::Ollama => assert_eq!(known.shows(), [json!({"model": "test-model"})]),
    }
    let response = trace_payloads(&trace_of(&after), "llm.context_assembly.response").remove(0);
    assert_eq!(response["first_token_kind"], "content");
    assert!(response["time_to_first_token_ms"].is_u64(), "{response}");
    assert!(response["request_duration_ms"].is_u64(), "{response}");
    // The discovered window changes nothing the provider receives, num_ctx
    // included.
    assert_eq!(unknown.requests(), known.requests());
}

#[test]
fn an_everyday_run_discovers_the_context_window_omlx() {
    discovered_window(Wire::Omlx);
}

#[test]
fn an_everyday_run_discovers_the_context_window_ollama() {
    discovered_window(Wire::Ollama);
}
