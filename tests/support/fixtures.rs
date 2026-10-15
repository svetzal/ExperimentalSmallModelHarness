//! Temporary projects, configuration, state, and trace lookup.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A temporary workspace: a project directory with state and config beside it.
pub struct Workspace {
    _temp: tempfile::TempDir,
    pub base: PathBuf,
    pub project: PathBuf,
    pub state: PathBuf,
    pub config: PathBuf,
}

impl Workspace {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let base = temp
            .path()
            .canonicalize()
            .expect("canonical temporary path");
        let project = base.join("project");
        std::fs::create_dir_all(&project).expect("project directory");
        Self {
            state: base.join("state"),
            config: base.join("config.json"),
            project,
            base,
            _temp: temp,
        }
    }

    /// Write a project file, creating its directories.
    pub fn write(&self, path: &str, text: &str) -> PathBuf {
        let full = self.project.join(path);
        std::fs::create_dir_all(full.parent().expect("file parent")).expect("file directory");
        std::fs::write(&full, text).expect("writing a project file");
        full
    }

    pub fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.project.join(path)).expect("reading a project file")
    }

    /// Saved preferences for the TUI and everyday `run`.
    pub fn configure(&self, provider: &str, model: &str, effort: Option<&str>) {
        let config = json!({
            "schema_version": "microgentic_config.v1",
            "provider": provider,
            "model": model,
            "reasoning_effort": effort,
        });
        std::fs::write(&self.config, config.to_string()).expect("writing the config");
    }

    /// Make the project a Git repository with one commit of its files.
    pub fn git_init(&self) {
        git(&self.project, &["init", "-q", "-b", "main"]);
        git(&self.project, &["add", "-A"]);
        git(
            &self.project,
            &["commit", "-q", "--allow-empty", "-m", "start"],
        );
    }

    /// Run trace files, oldest first by name.
    pub fn traces(&self) -> Vec<PathBuf> {
        let mut traces: Vec<PathBuf> = glob(&self.state.join("microgentic/runs"), "run-", "traces")
            .into_iter()
            .flat_map(|traces| list(&traces, ".jsonl"))
            .collect();
        traces.sort_by_key(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok());
        traces
    }

    /// The single conversation session file.
    pub fn session_file(&self) -> PathBuf {
        let sessions: Vec<PathBuf> = glob(&self.state.join("microgentic/runs"), "session-", "")
            .into_iter()
            .map(|session| session.join("conversation.json"))
            .filter(|file| file.exists())
            .collect();
        assert_eq!(sessions.len(), 1, "expected one session file: {sessions:?}");
        sessions.into_iter().next().expect("one session")
    }

    pub fn session_files(&self) -> usize {
        glob(&self.state.join("microgentic/runs"), "session-", "")
            .into_iter()
            .filter(|session| session.join("conversation.json").exists())
            .count()
    }
}

/// Directories under `root` whose names start with `prefix`, joined with `child`.
fn glob(root: &Path, prefix: &str, child: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
        .map(|entry| {
            if child.is_empty() {
                entry.path()
            } else {
                entry.path().join(child)
            }
        })
        .collect();
    found.sort();
    found
}

fn list(directory: &Path, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.to_string_lossy().ends_with(suffix))
        .collect()
}

/// The records of one JSONL trace.
pub fn trace_records(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .expect("reading a trace")
        .lines()
        .map(|line| serde_json::from_str(line).expect("trace line is JSON"))
        .collect()
}

/// Payloads of every record of `kind` in a trace.
pub fn trace_payloads(path: &Path, kind: &str) -> Vec<Value> {
    trace_records(path)
        .into_iter()
        .filter(|record| record["kind"] == kind)
        .map(|record| record["payload"].clone())
        .collect()
}

pub fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("running git");
    assert!(status.success(), "git {args:?} failed");
}

/// Run a binary to completion without a terminal.
pub fn run_binary(
    binary: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, String)],
    stdin: &[u8],
) -> Output {
    use std::io::Write;
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(cwd)
        .env_remove("NO_COLOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("starting the binary");
    let mut input = child.stdin.take().expect("stdin");
    input.write_all(stdin).expect("writing stdin");
    drop(input);
    child.wait_with_output().expect("waiting for the binary")
}
