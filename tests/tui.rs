//! End-to-end tests of the terminal interface, on a real pseudo-terminal.
#![cfg(unix)]

mod support;

use support::*;

/// The resting screen with no provider: animation off, an unreachable host.
fn open_idle(no_color: Option<&str>) -> (Workspace, Tui) {
    let workspace = Workspace::new();
    let mut launch = Launch::new(microgentic())
        .arg("--config")
        .arg(&workspace.config)
        .cwd(&workspace.project)
        .size(24, 90)
        .env("OMLX_HOST", "http://127.0.0.1:1")
        .env("MICROGENTIC_NO_ANIMATION", "1");
    if let Some(value) = no_color {
        launch = launch.env("NO_COLOR", value);
    }
    let tui = launch.spawn();
    (workspace, tui)
}

/// A crossterm foreground colour code in raw output; attributes are not colour.
fn has_colour_codes(raw: &[u8]) -> bool {
    String::from_utf8_lossy(raw).contains("\x1b[38;5;")
}

#[test]
fn tui_opens_without_a_provider_and_restores_the_terminal() {
    let (_workspace, mut tui) = open_idle(None);
    let screen = tui.wait_text("Enter send");
    assert!(screen.contains("Talk to Microgentic."), "{screen}");
    let (row, col) = screen.locate("● Ready").expect("the ready status");
    let marker = screen.cell(row, col);
    assert_eq!(marker.fgcolor(), ansi(2), "the ready marker is green");
    assert!(marker.bold());
    assert!(
        has_colour_codes(&tui.raw()),
        "the default view has no colour"
    );
    tui.assert_raw_mode_active();
    // A small resize and Unicode input exercise layout without submitting work.
    tui.resize(5, 20);
    tui.paste("世界");
    std::thread::sleep(std::time::Duration::from_millis(100));
    tui.quit();
}

#[test]
fn an_empty_no_color_keeps_colour() {
    let (_workspace, mut tui) = open_idle(Some(""));
    tui.wait_text("Enter send");
    assert!(
        has_colour_codes(&tui.raw()),
        "an empty NO_COLOR disabled colour"
    );
    tui.quit();
}

#[test]
fn no_color_removes_colour_but_keeps_labels_and_attributes() {
    let (_workspace, mut tui) = open_idle(Some("1"));
    let screen = tui.wait_text("Enter send");
    assert!(
        !has_colour_codes(&tui.raw()),
        "NO_COLOR still produced colour"
    );
    let (row, col) = screen
        .locate("● Ready")
        .expect("the monochrome status keeps its label");
    let marker = screen.cell(row, col);
    assert_eq!(marker.fgcolor(), vt100::Color::Default);
    assert!(marker.bold(), "attributes remain without colour");
    tui.quit();
}

// ---------------------------------------------------------------- sessions

/// A TUI session against a mock provider in a fresh workspace.
struct Session {
    workspace: Workspace,
    provider: MockProvider,
    tui: Tui,
}

/// Open the TUI on `binary` with saved preferences for `wire`.
fn open_session(
    binary: &str,
    workspace: Workspace,
    provider: MockProvider,
    rows: u16,
    cols: u16,
) -> Session {
    workspace.configure(provider.wire().name(), "test-model", Some("low"));
    let tui = launch_tui(binary, &workspace, &provider, rows, cols);
    Session {
        workspace,
        provider,
        tui,
    }
}

fn launch_tui(
    binary: &str,
    workspace: &Workspace,
    provider: &MockProvider,
    rows: u16,
    cols: u16,
) -> Tui {
    let mut launch = Launch::new(binary)
        .arg("--config")
        .arg(&workspace.config)
        .cwd(&workspace.project)
        .size(rows, cols)
        .env("XDG_STATE_HOME", &workspace.state)
        .env("MICROGENTIC_NO_ANIMATION", "1")
        // Fixtures build inside their own project, not a shared Cargo target.
        .env_remove("CARGO_TARGET_DIR");
    for (key, value) in provider.env() {
        launch = launch.env(key, value);
    }
    let tui = launch.spawn();
    tui.wait_text("Enter send");
    tui
}

const READY: &str = "● Ready for your next message";

impl Session {
    /// Send a message and wait until `answer` shows and the run has settled.
    fn exchange(&mut self, prompt: &str, answer: &str) -> Snapshot {
        self.tui.submit(prompt);
        self.tui
            .wait_for(&format!("{answer:?} and the ready status"), |screen| {
                screen.contains(answer) && screen.contains(READY)
            })
    }

    fn session_state(&self) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(self.workspace.session_file()).unwrap())
            .unwrap()
    }
}

/// The rendered rows after the last row containing `marker`.
fn after_last(screen: &Snapshot, marker: &str) -> String {
    let rows = screen.rows();
    let start = rows
        .iter()
        .rposition(|row| row.contains(marker))
        .expect("marker on screen");
    rows[start..].join("\n")
}

// ---------------------------------------------------------------- dialogue

/// No engineering task is assumed; requested work and checks stay available.
fn dialogue(wire: Wire) {
    let workspace = Workspace::new();
    workspace.write(
        "Cargo.toml",
        "[package]\nname=\"dialogue_fixture\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
    );
    workspace.write(".gitignore", "target/\n");
    std::fs::create_dir_all(workspace.project.join("src")).unwrap();
    let provider = MockProvider::start(wire, |request| {
        Reply::Stream(match request.number {
            1 => vec![content("Hello! What would you like to talk about?")],
            2 => vec![content("")],
            3 => vec![content("I am ready to chat.")],
            4 => vec![content(
                "That is an example we can discuss without running it.",
            )],
            5 => vec![
                content("I will add the function you requested."),
                tool(
                    "write_file",
                    serde_json::json!({"path": "src/lib.rs", "content": "pub fn answer() -> u8 { 42 }\n"}),
                ),
            ],
            6 => vec![
                content("I will check the source change."),
                tool(
                    "shell_command",
                    serde_json::json!({"command": "cargo check --offline", "timeout_secs": 30}),
                ),
            ],
            7 => vec![content(
                "Added answer() returning 42 and cargo check passed.",
            )],
            _ => vec![content("You are welcome!")],
        })
    });
    let mut session = open_session(microgentic(), workspace, provider, 36, 200);
    // The fixture builds inside its own project, not a shared Cargo target.
    assert_eq!(
        session.provider.request_count(),
        0,
        "opening a TUI assumed a task"
    );

    let greeting = session.exchange("Hello!", "Hello! What would you like to talk about?");
    assert!(
        !after_last(&greeting, "You: Hello!").contains("Evidence:"),
        "ordinary chat received a work footer"
    );
    let requests = session.provider.requests();
    assert_eq!(requests.len(), 1, "a natural reply was forced to continue");
    assert_eq!(
        requests[0]["messages"].as_array().unwrap().last().unwrap()["content"],
        "Hello!"
    );
    assert!(system_message(&requests[0]).contains("No engineering task is assumed"));
    assert!(
        !requests[0]["messages"]
            .to_string()
            .contains("Complete this benchmark task")
    );
    assert!(session.session_state()["last_error"].is_null());

    let ready = session.exchange("Are you ready to chat?", "I am ready to chat.");
    assert!(
        !after_last(&ready, "You: Are you ready").contains("Evidence:"),
        "empty-reply recovery received a work footer"
    );
    let requests = session.provider.requests();
    assert!(
        user_messages(&requests[2])
            .last()
            .unwrap()
            .contains("Answer the user's message naturally")
    );

    let example =
        "Let's discuss this example, without executing it:\n```sh\ntouch SHOULD_NOT_EXIST\n```";
    let discussion = session.exchange(
        example,
        "That is an example we can discuss without running it.",
    );
    assert!(
        !after_last(&discussion, "You: Let's discuss").contains("Evidence:"),
        "quoted examples triggered a work footer"
    );
    assert_eq!(
        user_messages(&session.provider.requests()[3])
            .last()
            .unwrap(),
        example
    );
    assert!(!session.workspace.project.join("src/lib.rs").exists());
    assert!(!session.workspace.project.join("SHOULD_NOT_EXIST").exists());
    assert!(
        !session.workspace.project.join("target").exists(),
        "chat ran a validation command"
    );
    for trace in session.workspace.traces() {
        let resolved = &trace_payloads(&trace, "agent.contract.resolved")[0]["resolved"];
        assert_eq!(
            resolved["probes"],
            serde_json::json!([]),
            "a conversation example became a required probe"
        );
    }

    let work = session.exchange(
        "Please add src/lib.rs with an answer function returning 42 and check it.",
        "Added answer() returning 42 and cargo check passed.",
    );
    assert!(
        after_last(&work, "You: Please add").contains("Evidence:"),
        "source edits and checks lost their trace footer"
    );
    let requests = session.provider.requests();
    assert_eq!(requests.len(), 7);
    let observed = requests[6]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(observed["role"], "tool");
    let result: serde_json::Value =
        serde_json::from_str(observed["content"].as_str().unwrap()).unwrap();
    assert!(result["success"].as_bool().unwrap() && result["status"] == 0);
    assert_eq!(
        result["validation_probe"], false,
        "chat checks acquired benchmark acceptance authority"
    );
    assert_eq!(
        session.workspace.read("src/lib.rs"),
        "pub fn answer() -> u8 { 42 }\n"
    );
    assert!(
        session.workspace.project.join("target").exists(),
        "post-edit validation did not run"
    );

    let thanks = session.exchange("Thanks!", "You are welcome!");
    assert!(
        !after_last(&thanks, "You: Thanks!").contains("Evidence:"),
        "work footer leaked into a later chat reply"
    );
    let requests = session.provider.requests();
    assert_eq!(requests.len(), 8);
    assert_eq!(user_messages(&requests[7]).last().unwrap(), "Thanks!");
    assert!(
        !requests[7]["messages"]
            .to_string()
            .contains("previous request stopped")
    );
    assert_eq!(
        session.workspace.traces().len(),
        5,
        "hiding chat footers disabled trace retention"
    );
    session.tui.quit();
}

#[test]
fn tui_chats_naturally_and_can_perform_work_when_requested_omlx() {
    dialogue(Wire::Omlx);
}

#[test]
fn tui_chats_naturally_and_can_perform_work_when_requested_ollama() {
    dialogue(Wire::Ollama);
}

// ---------------------------------------------------------------- conversation

/// The real TUI, stream transport and conversation, for one binary.
fn conversation(binary: &str, wire: Wire) {
    let workspace = Workspace::new();
    workspace.write("memo.txt", "Codename copperfern. Unicode 世界.\n");
    let (release_first, release_cancel) = (Gate::default(), Gate::default());
    let live = format!("FIRST-LIVE-{}-END", "x".repeat(150));
    let (first, cancel, chunk) = (release_first.clone(), release_cancel.clone(), live.clone());
    let provider = MockProvider::start(wire, move |request| {
        Reply::Stream(match request.number {
            1 => vec![
                thinking("Looking at the memo"),
                content(&chunk),
                Step::Hold(first.clone()),
                tool("read_file", serde_json::json!({"path": "memo.txt"})),
            ],
            2 => vec![content("Copperfern stored.\nDONE")],
            3 => vec![content("FOLLOWUP-LIVE copperfern 世界\n"), content("DONE")],
            // Deliberately close without completing the native response.
            4 => vec![
                content("UNFINISHED-REPLY"),
                Step::Hold(cancel.clone()),
                Step::Abort,
            ],
            5 => vec![content("RECOVERED\nDONE")],
            6 | 7 => vec![content("DONE")],
            number => panic!("unexpected provider request {number}"),
        })
    });
    let mut session = open_session(binary, workspace, provider, 30, 240);

    session
        .tui
        .submit("Remember codename copperfern. Read memo.txt, then finish.");
    let screen = session.tui.wait_text(&live);
    assert_eq!(
        session.provider.request_count(),
        1,
        "stream only appeared after task completion"
    );
    assert!(
        screen.rows().iter().any(|row| row.contains(&live)),
        "stream used truncated previews"
    );
    release_first.open();
    let completed = session
        .tui
        .wait_for("the first exchange to settle", |screen| {
            screen.contains("Copperfern stored.") && screen.contains(READY)
        });
    assert!(
        completed.contains("✓ read_file memo.txt"),
        "tool activity missing\n{completed}"
    );
    assert!(
        completed.contains("Evidence:"),
        "read-only project inspection lost its trace footer"
    );

    session.exchange(
        "What codename and Unicode did you see? Do not reread the file.",
        "FOLLOWUP-LIVE",
    );
    let requests = session.provider.requests();
    assert_eq!(requests.len(), 3);
    let messages = requests[2]["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages.iter().filter(|m| m["role"] == "system").count(), 1);
    assert!(messages.iter().any(|m| m["role"] == "user"
        && m["content"] == "Remember codename copperfern. Read memo.txt, then finish."));
    assert!(
        messages
            .iter()
            .any(|m| m["role"] == "assistant" && !m["tool_calls"].is_null())
    );
    assert!(messages.iter().any(|m| {
        m["role"] == "tool"
            && m["content"]
                .as_str()
                .unwrap_or_default()
                .contains("copperfern")
    }));
    assert!(messages.iter().any(|m| {
        m["role"] == "assistant"
            && m["content"]
                .as_str()
                .unwrap_or_default()
                .contains("Copperfern stored.")
    }));
    assert!(
        messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("What codename")
    );

    session.tui.submit("Start a request I will cancel.");
    session.tui.wait_text("UNFINISHED-REPLY");
    session.tui.ctrl_c();
    session.tui.wait_text("You can follow up");
    release_cancel.open();
    let saved = session.session_state();
    assert!(
        !saved["last_error"].is_null(),
        "cancellation was treated as completed conversation"
    );
    assert!(!saved["messages"].to_string().contains("UNFINISHED-REPLY"));
    session.exchange(
        "Continue after cancellation; keep our codename.",
        "RECOVERED",
    );
    let requests = session.provider.requests();
    assert!(
        user_messages(&requests[4])
            .last()
            .unwrap()
            .contains("previous request stopped")
    );
    assert!(requests[4]["messages"].to_string().contains("copperfern"));
    assert!(
        !requests[4]["messages"]
            .to_string()
            .contains("UNFINISHED-REPLY")
    );
    session.tui.quit();

    // Reusing state with another model or project fails before any provider call.
    let session_file = session.workspace.session_file();
    let other = session.workspace.base.join("other-project");
    std::fs::create_dir_all(&other).unwrap();
    for (directory, model) in [
        (&session.workspace.project, "another-model"),
        (&other, "test-model"),
    ] {
        let output = run_binary(
            binary,
            &[
                "conversation-worker",
                "--prompt",
                "wrong session",
                "--session-state",
                session_file.to_str().unwrap(),
                "--provider",
                wire.name(),
                "--model",
                model,
                "--reasoning-effort",
                "low",
            ],
            directory,
            &env_for(&session),
            b"",
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("different project or model"));
    }
    assert_eq!(session.provider.request_count(), 5);

    // A normal run ignores all conversation state, even in the same project.
    let plain = run_binary(
        binary,
        &[
            "--config",
            session.workspace.config.to_str().unwrap(),
            "run",
            "--prompt",
            "A fresh isolated request",
            "--json",
            "--provider",
            wire.name(),
            "--model",
            "test-model",
            "--reasoning-effort",
            "low",
        ],
        &session.workspace.project,
        &env_for(&session),
        b"",
    );
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&plain.stdout).unwrap();
    assert_eq!(summary["final_summary"], "DONE");
    let requests = session.provider.requests();
    assert_eq!(requests[5]["messages"].as_array().unwrap().len(), 2);
    assert!(!requests[5]["messages"].to_string().contains("copperfern"));
    let controls: &[&str] = match wire {
        Wire::Omlx => &["temperature", "max_tokens"],
        Wire::Ollama => &["options", "think"],
    };
    for control in controls {
        assert_eq!(requests[0][control], requests[5][control], "{control}");
    }
    let mut conversation_tools = requests[0]["tools"].as_array().unwrap().clone();
    if binary == legacy_binary() {
        // The compatibility executable's run keeps its historical tool set;
        // only everyday runs and conversations add calculate.
        conversation_tools.retain(|tool| tool["function"]["name"] != "calculate");
    }
    assert_eq!(
        serde_json::Value::Array(conversation_tools),
        requests[5]["tools"]
    );

    // Opening a second TUI starts a new conversation and never resumes.
    let mut second = launch_tui(binary, &session.workspace, &session.provider, 30, 240);
    second.submit("Start a separate conversation.");
    second.wait_for("the separate conversation to settle", |_| {
        session.provider.request_count() == 7
    });
    second.wait_text(READY);
    assert_eq!(
        session.provider.requests()[6]["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    second.quit();
}

fn env_for(session: &Session) -> Vec<(&'static str, String)> {
    let mut env = session.provider.env();
    env.push((
        "XDG_STATE_HOME",
        session.workspace.state.display().to_string(),
    ));
    env.push(("MICROGENTIC_NO_ANIMATION", "1".into()));
    env
}

#[test]
fn tui_streams_and_keeps_native_history_omlx() {
    conversation(microgentic(), Wire::Omlx);
}

#[test]
fn tui_streams_and_keeps_native_history_ollama() {
    conversation(microgentic(), Wire::Ollama);
}

#[test]
fn legacy_tui_streams_and_keeps_native_history_omlx() {
    conversation(legacy_binary(), Wire::Omlx);
}

#[test]
fn legacy_tui_streams_and_keeps_native_history_ollama() {
    conversation(legacy_binary(), Wire::Ollama);
}

/// History rows above the rule, without blank rows at the end.
fn history_rows(screen: &Snapshot) -> Vec<String> {
    let rows = screen.rows();
    let rule = rows
        .iter()
        .rposition(|row| row.trim_start().starts_with("────"))
        .expect("the rule");
    let mut history = rows[..rule].to_vec();
    while history.last().is_some_and(|row| row.trim().is_empty()) {
        history.pop();
    }
    history
}

// ---------------------------------------------------------------- slash commands

/// Slash commands stay local and change only later provider requests.
fn commands(wire: Wire) {
    let workspace = Workspace::new();
    workspace.write("AGENTS.md", "Run tests with make check.\n");
    let release_slow = Gate::default();
    let slow = release_slow.clone();
    let provider = MockProvider::start(wire, move |request| {
        let mut steps = Vec::new();
        if request.last_message() == "Take your time." {
            // Hold the reply open so commands arrive while a run is active.
            steps.push(Step::Hold(slow.clone()));
        }
        steps.push(content(&format!("REPLY-{}", request.number)));
        Reply::Stream(steps)
    });
    let mut session = open_session(microgentic(), workspace, provider, 40, 160);
    let saved_config = std::fs::read_to_string(&session.workspace.config).unwrap();
    let effort = |request: &serde_json::Value| request[wire.effort_field()].clone();
    let exchange = |session: &mut Session, prompt: &str| {
        let before = session.provider.request_count();
        session.exchange(prompt, &format!("REPLY-{}", before + 1));
        assert_eq!(session.provider.request_count(), before + 1);
        session.provider.requests().last().unwrap().clone()
    };
    let hint_row = |screen: &Snapshot| screen.rows().last().cloned().unwrap_or_default();

    session.tui.submit("/help");
    session.tui.wait_text("Start a new conversation");
    session.tui.submit("/guidance");
    let screen = session
        .tui
        .wait_text("Project guidance, most general first:");
    assert!(screen.contains("  AGENTS.md  27 B  included"), "{screen}");
    session.tui.wait_text("📜 1");
    session.tui.submit("/nope");
    session
        .tui
        .wait_text("Unknown command /nope. Type /help to see commands.");
    session.tui.submit("/effort extreme");
    session.tui.wait_text("Unknown effort \"extreme\"");
    session.tui.submit("/model");
    session
        .tui
        .wait_text(&format!("Model: test-model on {}", wire.name()));
    let picker = session.tui.wait_for("the model picker", |screen| {
        screen.contains(&format!("Choose a model from {}", wire.name()))
            && hint_row(screen).contains("Enter choose | Esc cancel")
    });
    let (row, _) = picker
        .locate("› test-model (current)")
        .expect("the current model selected");
    assert!(
        picker.row(row + 1).contains("  other-model")
            || picker.row(row - 1).contains("  other-model"),
        "{picker}"
    );
    session.tui.esc(); // Esc closes the picker without a change.
    session.tui.wait_for("the picker to close", |screen| {
        hint_row(screen).contains("Enter send")
    });
    session.tui.submit("/effort");
    session.tui.wait_text("› low (current)");
    session.tui.send(b"\x1b[B\x1b"); // Down, then Esc: still no change.
    session.tui.wait_for("the picker to close", |screen| {
        hint_row(screen).contains("Enter send")
    });
    assert_eq!(
        session.provider.request_count(),
        0,
        "a command reached the provider"
    );
    assert_eq!(
        session.provider.gets().len(),
        1,
        "{:?}",
        session.provider.gets()
    );

    let first = exchange(&mut session, "First message.");
    assert_eq!(first["model"], "test-model");
    assert_eq!(effort(&first), "low");
    let follow = exchange(&mut session, "Second message.");
    assert!(
        follow["messages"].to_string().contains("First message."),
        "ordinary follow-ups lost their history"
    );

    session.tui.submit("/model");
    session.tui.wait_text("› test-model (current)");
    session.tui.send(b"\x1b[B\r"); // Down to other-model, then Enter.
    session
        .tui
        .wait_text("Model set to other-model for this session.");
    session.tui.wait_text("🤖 other-model  🧠 low");
    let switched = exchange(&mut session, "After the model switch.");
    assert_eq!(switched["model"], "other-model");
    assert_eq!(effort(&switched), "low");
    assert!(
        !switched["messages"].to_string().contains("First message."),
        "history crossed a model change"
    );

    session.tui.submit("/effort");
    session.tui.wait_text("› low (current)");
    session.tui.send(b"\x1b[B\x1b[B\r"); // low, medium, high, then Enter.
    session
        .tui
        .wait_text("Effort set to high for this session.");
    session.tui.wait_text("🤖 other-model  🧠 high");
    let raised = exchange(&mut session, "After the effort change.");
    assert_eq!(raised["model"], "other-model");
    assert_eq!(effort(&raised), "high");
    let kept = exchange(&mut session, "Still the same settings.");
    assert!(
        kept["messages"]
            .to_string()
            .contains("After the effort change.")
    );

    session.tui.submit("/clear");
    session.tui.wait_text("Started a new conversation.");
    let cleared = exchange(&mut session, "After clearing.");
    assert_eq!(
        cleared["messages"].as_array().unwrap().len(),
        2,
        "{}",
        cleared["messages"]
    );
    assert!(
        !cleared["messages"]
            .to_string()
            .contains("Still the same settings.")
    );
    assert_eq!(
        session.workspace.session_files(),
        4,
        "clearing or switching deleted retained session state"
    );

    session.tui.submit("/effort default");
    session
        .tui
        .wait_text("Effort set to model default for this session.");
    let unset = exchange(&mut session, "With the model default effort.");
    assert!(effort(&unset).is_null(), "{unset}");

    let escaped = exchange(&mut session, "//etc/hosts is a path, not a command.");
    assert_eq!(
        user_messages(&escaped).last().unwrap(),
        "/etc/hosts is a path, not a command."
    );

    session.tui.submit("Take your time.");
    session.tui.wait_text("Ctrl-C to cancel");
    session.tui.submit("/model test-model");
    session
        .tui
        .wait_text("/model is unavailable while Microgentic is replying.");
    session.tui.submit("/help");
    session
        .tui
        .wait_for("help shown during the reply", |screen| {
            let history = history_rows(screen);
            history
                .last()
                .is_some_and(|row| row.contains("Start a message with // to send it"))
                && history
                    .iter()
                    .any(|row| row.contains("/model is unavailable"))
        });
    release_slow.open();
    session.tui.wait_for("the held reply to settle", |screen| {
        screen.contains("REPLY-9") && screen.contains(READY)
    });
    let busy = session.provider.requests().last().unwrap().clone();
    assert_eq!(
        busy["model"], "other-model",
        "a rejected command changed the model"
    );
    assert_eq!(
        std::fs::read_to_string(&session.workspace.config).unwrap(),
        saved_config,
        "session commands wrote the configuration"
    );
    session.tui.quit();
}

#[test]
fn tui_slash_commands_stay_local_omlx() {
    commands(Wire::Omlx);
}

#[test]
fn tui_slash_commands_stay_local_ollama() {
    commands(Wire::Ollama);
}

// ---------------------------------------------------------------- tool lines

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// A tool line still running: a spinner glyph, a space, then `call`.
fn running(frame: &str, call: &str) -> bool {
    SPINNER
        .iter()
        .any(|glyph| frame.contains(&format!("{glyph} {call}")))
}

/// Tool calls show as one live line each, resolving in place.
fn tool_lines(wire: Wire) {
    let workspace = Workspace::new();
    workspace.write("notes.txt", "first line\nsecond line\nthird line\n");
    let provider = MockProvider::turns(
        wire,
        vec![
            vec![
                content("Checking two things."),
                tools(&[
                    (
                        "shell_command",
                        serde_json::json!({"command": "sleep 1", "timeout_secs": 30}),
                    ),
                    (
                        "read_file",
                        serde_json::json!({"path": "notes.txt", "line_start": 1, "line_end": 2}),
                    ),
                ]),
            ],
            vec![
                content(""),
                tool("read_file", serde_json::json!({"path": "missing.txt"})),
            ],
            vec![
                content(""),
                tool("calculate", serde_json::json!({"expression": "1474 / 1.1"})),
            ],
            vec![content("All done.")],
        ],
    );
    let mut session = open_session(microgentic(), workspace, provider, 36, 160);
    session.tui.mark_frames();
    let finished = session.exchange("Check the notes.", "All done.");

    let (shell, read) = ("shell_command sleep 1", "read_file notes.txt:1-2");
    let frames = session.tui.frames_since_mark();
    assert!(
        frames
            .iter()
            .any(|frame| running(frame, shell) && running(frame, read)),
        "the batch never showed both calls running at once"
    );
    let glyphs: std::collections::BTreeSet<char> = frames
        .iter()
        .flat_map(|frame| {
            SPINNER
                .iter()
                .filter(move |glyph| frame.contains(&format!("{glyph} {shell}")))
        })
        .copied()
        .collect();
    assert!(glyphs.len() >= 2, "the spinner did not animate: {glyphs:?}");

    for line in [
        "  ✓ shell_command sleep 1",
        "  ✓ read_file notes.txt:1-2",
        "  ✗ read_file missing.txt",
        "  ✓ calculate 1474 / 1.1 = 1340",
    ] {
        assert!(
            finished.rows().iter().any(|row| row.ends_with(line)),
            "{line} missing\n{finished}"
        );
    }
    let (row, col) = finished.locate("✓ shell_command").unwrap();
    assert_eq!(
        finished.cell(row, col).fgcolor(),
        ansi(2),
        "the check is green"
    );
    let (row, col) = finished.locate("✗ read_file").unwrap();
    assert_eq!(
        finished.cell(row, col).fgcolor(),
        ansi(1),
        "the cross is red"
    );
    let history = history_rows(&finished).join("\n");
    assert!(
        !SPINNER.iter().any(|glyph| history.contains(*glyph)),
        "a call still spins after the run finished"
    );
    let raw = String::from_utf8_lossy(&session.tui.raw()).into_owned();
    assert!(
        !raw.contains("finished"),
        "a separate finished line appeared"
    );
    assert!(
        !history.contains("Using "),
        "a separate Using line appeared in the history"
    );
    assert!(finished.contains("Evidence:"));
    assert_eq!(session.provider.request_count(), 4);
    session.tui.quit();
}

#[test]
fn tui_shows_each_tool_call_as_one_live_line_omlx() {
    tool_lines(Wire::Omlx);
}

#[test]
fn tui_shows_each_tool_call_as_one_live_line_ollama() {
    tool_lines(Wire::Ollama);
}

// ---------------------------------------------------------------- status bar

fn compact_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1_000.0),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.0),
    }
}

/// The bar shows the model, effort, folder, git state and context fill.
fn status_bar(wire: Wire, model: &str, in_git: bool) {
    let workspace = Workspace::new();
    workspace.write("tracked.txt", "one\n");
    if in_git {
        workspace.git_init();
        workspace.write("tracked.txt", "two\n");
        workspace.write("new.txt", "untracked\n");
    }
    let provider = MockProvider::start(wire, |request| {
        Reply::Stream(vec![content(&format!("REPLY-{}", request.number))])
    });
    workspace.configure(wire.name(), model, Some("low"));
    let mut tui = launch_tui(microgentic(), &workspace, &provider, 30, 160);
    let bar = |screen: &Snapshot| screen.row(28);
    let identity = format!(" 🤖 {model}  🧠 low  📁 project");
    let screen = if in_git {
        // A known window shows an empty gauge before the first request.
        tui.wait_for("the full status bar", |screen| {
            bar(screen) == format!("{identity}  🌿 main ●1 …1  ctx:[··········]0%")
        })
    } else {
        let screen = tui.snapshot();
        assert_eq!(bar(&screen), identity, "the bar showed more than it knows");
        screen
    };
    let colour_of = |screen: &Snapshot, needle: &str| {
        let (row, col) = screen
            .locate(needle)
            .unwrap_or_else(|| panic!("{needle} on screen"));
        screen.cell(row, col).fgcolor()
    };
    assert_eq!(colour_of(&screen, "🤖"), ansi(2), "the model is green");
    assert_eq!(colour_of(&screen, "🧠"), ansi(3), "the effort is yellow");
    if in_git {
        assert_eq!(colour_of(&screen, "🌿"), ansi(6), "the branch is cyan");
        assert_eq!(
            colour_of(&screen, "●1"),
            ansi(208),
            "the changed count is orange"
        );
    }
    let exchange = |tui: &mut Tui, prompt: &str, number: usize| {
        tui.submit(prompt);
        tui.wait_for("the reply to settle", |screen| {
            screen.contains(&format!("REPLY-{number}")) && screen.contains(READY)
        })
    };
    exchange(&mut tui, "Hello.", 1);
    let trace = workspace.traces().pop().expect("a trace");
    let ledger = trace_payloads(&trace, "llm.context_assembly.ledger")
        .pop()
        .expect("a ledger");
    let used = ledger["estimated_tokens"].as_u64().unwrap();
    let gauge = match ledger["context_window_tokens"].as_u64() {
        Some(window) => {
            assert_eq!(window, 262_144);
            format!("]{}%", (used as f64 * 100.0 / window as f64).round())
        }
        None => format!("ctx: {}", compact_tokens(used)),
    };
    tui.wait_for(&format!("the gauge {gauge}"), |screen| {
        bar(screen).ends_with(&gauge)
    });
    if in_git {
        // The bar refreshes git after each exchange, never on a timer.
        workspace.write("another.txt", "x\n");
        exchange(&mut tui, "Again.", 2);
        tui.wait_for("the refreshed git state", |screen| {
            bar(screen).contains("🌿 main ●1 …2")
        });
    } else {
        assert!(
            !String::from_utf8_lossy(&tui.raw()).contains('🌿'),
            "a project outside git showed a branch"
        );
    }
    tui.quit();
}

#[test]
fn tui_status_bar_shows_git_state_and_a_known_context_window() {
    status_bar(Wire::Ollama, "qwen3.8:27b-mxfp8", true);
}

#[test]
fn tui_status_bar_shows_only_what_it_knows_outside_git() {
    status_bar(Wire::Omlx, "test-model", false);
}
