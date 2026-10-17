//! `microgentic run` ends on a termination signal. The model request is
//! cancelled, a project command that is still running is killed with its
//! process group, and the process exits with an error instead of waiting
//! for work that nothing will finish.
#![cfg(unix)]

mod support;

use serde_json::json;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use support::*;

/// Start an everyday run with `prompt` and leave it running.
fn start_run(provider: &MockProvider, workspace: &Workspace, prompt: &str) -> Child {
    run_command(provider, workspace, prompt)
        .spawn()
        .expect("starting the run")
}

fn run_command(provider: &MockProvider, workspace: &Workspace, prompt: &str) -> Command {
    let mut command = Command::new(microgentic());
    command
        .arg("--config")
        .arg(&workspace.config)
        .args([
            "run",
            "--provider",
            provider.wire().name(),
            "--model",
            "test-model",
            "--prompt",
            prompt,
        ])
        .current_dir(&workspace.project)
        .env_remove("NO_COLOR")
        .env("XDG_STATE_HOME", &workspace.state)
        .env("XDG_CONFIG_HOME", workspace.base.join("config"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in provider.env() {
        command.env(key, value);
    }
    command
}

/// Wait up to ten seconds for `test` to hold.
fn wait_until(what: &str, test: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !test() {
        assert!(Instant::now() < deadline, "{what} did not happen in time");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn signal(child: &Child, signal: i32) {
    // SAFETY: kill(2) takes a pid and a signal number and touches no memory.
    let sent = unsafe { libc::kill(child.id() as libc::pid_t, signal) };
    assert_eq!(
        sent,
        0,
        "signalling the run: {}",
        std::io::Error::last_os_error()
    );
}

/// The run's exit status, if it ends within `patience`.
fn exit_within(child: &mut Child, patience: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + patience;
    loop {
        if let Some(status) = child.try_wait().expect("polling the run") {
            return Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn stderr_of(child: Child) -> String {
    let output = child
        .wait_with_output()
        .expect("collecting the run's output");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Processes whose command line carries `marker`.
fn processes_marked(marker: &str) -> Vec<String> {
    let output = Command::new("pgrep")
        .args(["-f", marker])
        .output()
        .expect("running pgrep");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn run_stops_while_the_model_is_answering(wire: Wire, signal_number: i32) {
    let workspace = Workspace::new();
    let hold = Gate::default();
    let provider = MockProvider::replying(
        wire,
        vec![content("The reply begins"), Step::Hold(hold.clone())],
    );
    let mut child = start_run(&provider, &workspace, "Say something long.");
    wait_until("the model request", || provider.request_count() >= 1);
    signal(&child, signal_number);
    let status = exit_within(&mut child, Duration::from_secs(3))
        .unwrap_or_else(|| panic!("signal {signal_number} did not end the run"));
    hold.open();
    let stderr = stderr_of(child);
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("Cancelled"),
        "the run did not report its cancellation: {stderr}"
    );
}

#[test]
fn run_stops_on_sigterm_while_the_model_is_answering_omlx() {
    run_stops_while_the_model_is_answering(Wire::Omlx, libc::SIGTERM);
}

#[test]
fn run_stops_on_sigterm_while_the_model_is_answering_ollama() {
    run_stops_while_the_model_is_answering(Wire::Ollama, libc::SIGTERM);
}

#[test]
fn run_stops_on_sighup_while_the_model_is_answering() {
    run_stops_while_the_model_is_answering(Wire::Omlx, libc::SIGHUP);
}

/// A project command the model started is killed with the run, process
/// group and all, rather than left to run out its timeout.
fn run_stops_while_a_command_runs(wire: Wire) {
    let workspace = Workspace::new();
    let marker = format!("mg-termination-{}-{}", std::process::id(), wire.name());
    let command_marker = marker.clone();
    let provider = MockProvider::start(wire, move |request| {
        Reply::Stream(match request.number {
            1 => vec![
                content("I will run it."),
                // The marker rides in the interpreter's own arguments, where
                // pgrep sees it; a shell comment would vanish when the shell
                // execs its last command.
                tool(
                    "shell_command",
                    json!({"command": format!(
                        "python3 -c 'import time; time.sleep(30)  # {command_marker}'"
                    )}),
                ),
            ],
            _ => vec![content("Done.")],
        })
    });
    let mut child = start_run(&provider, &workspace, "Run the long command.");
    let deadline = Instant::now() + Duration::from_secs(10);
    while processes_marked(&marker).is_empty() {
        if let Some(status) = child.try_wait().expect("polling the run") {
            panic!(
                "the run ended ({status}) before its command started: {}",
                stderr_of(child)
            );
        }
        if Instant::now() > deadline {
            let traces = workspace
                .traces()
                .iter()
                .map(|path| std::fs::read_to_string(path).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n");
            let processes = Command::new("ps").args(["-ef"]).output().expect("ps");
            panic!(
                "the command did not start in time; shell events:\n{}\nprocesses:\n{}",
                traces
                    .lines()
                    .filter(|line| line.contains("shell"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                String::from_utf8_lossy(&processes.stdout)
                    .lines()
                    .filter(|line| line.contains("sleep"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    signal(&child, libc::SIGTERM);
    let status = exit_within(&mut child, Duration::from_secs(3))
        .expect("SIGTERM did not end the run while its command ran");
    let stderr = stderr_of(child);
    assert!(!status.success(), "{stderr}");
    wait_until("the command to be killed", || {
        processes_marked(&marker).is_empty()
    });
}

#[test]
fn run_stops_on_sigterm_while_a_command_runs_omlx() {
    run_stops_while_a_command_runs(Wire::Omlx);
}

#[test]
fn run_stops_on_sigterm_while_a_command_runs_ollama() {
    run_stops_while_a_command_runs(Wire::Ollama);
}

/// A run started with SIGHUP ignored, as `nohup` does, keeps ignoring it
/// and still ends on SIGTERM.
#[test]
fn run_started_under_nohup_keeps_ignoring_sighup() {
    let workspace = Workspace::new();
    let hold = Gate::default();
    let provider = MockProvider::replying(
        Wire::Omlx,
        vec![content("The reply begins"), Step::Hold(hold.clone())],
    );
    let mut command = run_command(&provider, &workspace, "Say something long.");
    // SAFETY: signal(2) only changes this child's disposition table, which
    // is what nohup does before exec; nothing is allocated or shared.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    let mut child = command.spawn().expect("starting the run");
    wait_until("the model request", || provider.request_count() >= 1);
    signal(&child, libc::SIGHUP);
    let watched = Instant::now() + Duration::from_secs(1);
    while Instant::now() < watched {
        assert!(
            child.try_wait().expect("polling the run").is_none(),
            "SIGHUP ended a run that was started with it ignored"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    signal(&child, libc::SIGTERM);
    let status =
        exit_within(&mut child, Duration::from_secs(3)).expect("SIGTERM did not end the run");
    hold.open();
    assert_eq!(status.code(), Some(1), "{}", stderr_of(child));
}
