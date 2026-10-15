//! Terminal conversation with native history and a live worker event stream.
mod commands;
mod history;
mod session;
mod styled;

use crate::conversation::ConversationEvent;
use crate::preferences::Preferences;
use anyhow::{Context, Result};
use commands::{Command, Submission, complete, footer, parse_submission};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute, queue,
    style::PrintStyledContent,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use history::History;
use session::{Session, finish_model_listing, run_command};
use std::io::{Write, stdout};
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use styled::{Palette, Role, Styled, first_row, fit_from_end, wrap};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::JoinHandle;

struct Screen;
impl Screen {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let screen = Self;
        execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste, Hide)?;
        Ok(screen)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableBracketedPaste, Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

struct ActiveRun {
    pid: Option<u32>,
    started: Instant,
    cancelled: bool,
    result: JoinHandle<std::io::Result<Output>>,
    events: UnboundedReceiver<ConversationEvent>,
    activity: String,
    streamed_text: String,
    used_project_tools: bool,
    terminal_reported: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Ready,
    Working,
    Stopped,
}

/// The status line. The label carries the meaning; the marker colour repeats it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Status {
    phase: Phase,
    label: String,
}

impl Status {
    fn new(phase: Phase, label: impl Into<String>) -> Self {
        Self {
            phase,
            label: label.into(),
        }
    }

    fn line(&self) -> Styled {
        let marker = match self.phase {
            Phase::Ready => Role::Ready,
            Phase::Working => Role::Working,
            Phase::Stopped => Role::Stopped,
        };
        Styled::default()
            .with(marker, "●")
            .with(Role::Plain, " ")
            .with(Role::Plain, &self.label)
    }
}

struct View {
    history: History,
    input: String,
    scroll: usize,
    status: Status,
}

fn header(root: &std::path::Path, prefs: &Preferences) -> Styled {
    Styled::default()
        .with(
            Role::Muted,
            &format!("{} | {} | ", root.display(), prefs.provider.as_str()),
        )
        .with(Role::Value, &prefs.model)
        .with(Role::Muted, " | effort: ")
        .with(
            Role::Value,
            prefs.reasoning_effort.as_deref().unwrap_or("model default"),
        )
}

/// Act on Enter. Messages wait while a reply is active; commands other than
/// `/help` are refused then, so a run never changes settings underneath it.
fn submit(view: &mut View, session: &mut Session, active: &mut Option<ActiveRun>) -> Result<()> {
    let submission = parse_submission(&view.input);
    if active.is_some() && matches!(submission, Submission::Message(_)) {
        // The draft stays in the input until the current reply settles.
        return Ok(());
    }
    let typed = std::mem::take(&mut view.input);
    view.scroll = 0;
    match submission {
        Submission::Message(prompt) => {
            view.history.push(Role::Plain, "\n");
            view.history.push(Role::UserLabel, "You:");
            view.history.push(Role::Plain, &format!(" {prompt}\n"));
            *active = start_reply(view, session, &prompt)?;
        }
        Submission::Command(command) => {
            view.history
                .push(Role::Muted, &format!("\n{}\n", typed.trim()));
            if active.is_some() && command != Command::Help {
                let name = typed.split_whitespace().next().unwrap_or_default();
                view.history.push(Role::Error, &format!("{name} is unavailable while Microgentic is replying. Wait for the reply or press Ctrl-C to cancel it, then try again.\n"));
            } else {
                run_command(view, session, command);
            }
        }
        Submission::Invalid(message) => {
            view.history
                .push(Role::Muted, &format!("\n{}\n", typed.trim()));
            view.history.push(Role::Error, &format!("{message}\n"));
        }
    }
    Ok(())
}

/// Start a worker for one message with the session's current settings.
fn start_reply(view: &mut View, session: &mut Session, prompt: &str) -> Result<Option<ActiveRun>> {
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    let state = match &session.state {
        Some(state) => state,
        None => session.state.insert(create_session(&session.root)?),
    };
    let settings = &session.settings;
    command
        .args(["conversation-worker", "--prompt", prompt, "--session-state"])
        .arg(state)
        .args([
            "--provider",
            settings.provider.as_str(),
            "--model",
            &settings.model,
        ])
        .current_dir(&session.root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(effort) = &settings.reasoning_effort {
        command.args(["--reasoning-effort", effort]);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            view.history
                .push(Role::Error, &format!("\nCould not start run: {error}\n"));
            return Ok(None);
        }
    };
    let (sender, events) = unbounded_channel();
    let stdout = child.stdout.take().context("opening worker stream")?;
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await? {
            let event = serde_json::from_str(&line).map_err(std::io::Error::other)?;
            let _ = sender.send(event);
        }
        Ok::<_, std::io::Error>(())
    });
    let pid = child.id();
    Ok(Some(ActiveRun {
        pid,
        started: Instant::now(),
        cancelled: false,
        result: tokio::spawn(async move {
            let output = child.wait_with_output().await?;
            reader.await.map_err(std::io::Error::other)??;
            Ok(output)
        }),
        events,
        activity: "Working".into(),
        streamed_text: String::new(),
        used_project_tools: false,
        terminal_reported: false,
    }))
}

fn draw(view: &View, header: &Styled, palette: Palette) -> Result<()> {
    let (columns, lines) = terminal::size()?;
    let width = usize::from(columns.saturating_sub(2)).max(1);
    let mut out = stdout();
    queue!(out, MoveTo(0, 0), Clear(ClearType::All))?;
    let mut put = |row: u16, cells: &[(Role, String)]| -> Result<()> {
        if row < lines {
            queue!(out, MoveTo(1.min(columns.saturating_sub(1)), row))?;
            for (role, text) in cells {
                queue!(
                    out,
                    PrintStyledContent(palette.style(*role).apply(text.as_str()))
                )?;
            }
        }
        Ok(())
    };
    let rule = first_row(
        &Styled::default().with(Role::Muted, &"─".repeat(width)),
        width,
    );
    put(
        0,
        &first_row(&Styled::default().with(Role::Title, "Microgentic"), width),
    )?;
    put(1, &first_row(&fit_from_end(header, width), width))?;
    let input = Styled::default()
        .with(Role::Prompt, "> ")
        .with(Role::Plain, &view.input);
    let input_rows = wrap(&input, width);
    let input_height = input_rows.len().min(4);
    let status_row = lines.saturating_sub(input_height as u16 + 2);
    // Rules frame the conversation; very short terminals drop them first.
    let lower_rule_row = status_row.saturating_sub(1);
    if lower_rule_row > 2 {
        put(2, &rule)?;
        put(lower_rule_row, &rule)?;
    }
    let body_height = usize::from(lines.saturating_sub(6 + input_height as u16));
    let history = wrap(&view.history.text, width);
    let end = history.len().saturating_sub(view.scroll.min(history.len()));
    let start = end.saturating_sub(body_height);
    for (index, row) in history[start..end].iter().enumerate() {
        put(3 + index as u16, row)?;
    }
    put(status_row, &first_row(&view.status.line(), width))?;
    for (index, row) in input_rows[input_rows.len() - input_height..]
        .iter()
        .enumerate()
    {
        put(
            lines.saturating_sub(input_height as u16 + 1) + index as u16,
            row,
        )?;
    }
    put(
        lines.saturating_sub(1),
        &first_row(&footer(&view.input), width),
    )?;
    out.flush()?;
    Ok(())
}

pub async fn run(config: Option<PathBuf>) -> Result<()> {
    let path = crate::preferences::config_path(config.as_deref())?;
    let mut session = Session {
        root: std::env::current_dir()?,
        settings: crate::preferences::load(&path)?.unwrap_or_default(),
        state: None,
        model_listing: None,
    };
    let palette = Palette::from_no_color(std::env::var_os("NO_COLOR").as_deref());
    let _screen = Screen::enter()?;
    let leaving = Arc::new(AtomicBool::new(false));
    let requested_exit = leaving.clone();
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let signal = tokio::spawn(async move {
        #[cfg(unix)]
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        requested_exit.store(true, Ordering::Relaxed);
    });
    let mut view = View {
        history: History::default(),
        input: String::new(),
        scroll: 0,
        status: Status::new(Phase::Ready, "Ready"),
    };
    view.history.reset();
    let mut active: Option<ActiveRun> = None;
    let mut last_draw = None;
    let outcome: Result<()> = async {
        loop {
            if leaving.load(Ordering::Relaxed) {
                if let Some(run) = &mut active {
                    if !run.cancelled {
                        interrupt(run);
                    }
                } else {
                    break;
                }
            }
            if let Some(run) = &mut active {
                while let Ok(event) = run.events.try_recv() {
                    apply_event(&mut view, run, event);
                }
            }
            if active.as_ref().is_some_and(|run| run.result.is_finished()) {
                let run = active.take().expect("completed run exists");
                match run.result.await.context("waiting for agent run")? {
                    Ok(output) if output.status.success() && run.terminal_reported => {
                        view.status = ready_for_next();
                    }
                    Ok(output) => {
                        if !run.terminal_reported {
                            view.history.push(Role::Error, &format!(
                                "\n[Request stopped; any unfinished reply is incomplete]\nRun error: {}\n",
                                String::from_utf8_lossy(&output.stderr)
                            ));
                        }
                        view.status = stopped();
                    }
                    Err(error) => {
                        view.history.push(Role::Error, &format!("\nRun error: {error}\n"));
                        view.status = Status::new(Phase::Stopped, "Run could not start.");
                    }
                }
                view.scroll = 0;
            }
            if let Some(run) = &active {
                view.status = Status::new(
                    Phase::Working,
                    format!(
                        "{} · {}s · Ctrl-C to cancel",
                        if run.cancelled {
                            "Cancellation requested"
                        } else {
                            &run.activity
                        },
                        run.started.elapsed().as_secs()
                    ),
                );
            }
            finish_model_listing(&mut view, &mut session).await;
            let header = header(&session.root, &session.settings);
            let signature = (
                header.clone(),
                view.history.revision,
                view.input.clone(),
                view.scroll,
                view.status.clone(),
                terminal::size()?,
            );
            if last_draw.as_ref() != Some(&signature) {
                draw(&view, &header, palette)?;
                last_draw = Some(signature);
            }
            if !event::poll(Duration::from_millis(50))? {
                tokio::task::yield_now().await;
                continue;
            }
            match event::read()? {
                Event::Paste(text) => view
                    .input
                    .extend(text.chars().filter(|ch| !ch.is_control() || *ch == '\n')),
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Esc => {
                        if active.is_none() {
                            break;
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(run) = &mut active {
                            interrupt(run);
                        } else if view.input.is_empty() {
                            break;
                        } else {
                            view.input.clear();
                        }
                    }
                    KeyCode::PageUp => view.scroll = view.scroll.saturating_add(8),
                    KeyCode::PageDown => view.scroll = view.scroll.saturating_sub(8),
                    KeyCode::Enter
                        if key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        view.input.push('\n')
                    }
                    KeyCode::Enter if !view.input.trim().is_empty() => {
                        submit(&mut view, &mut session, &mut active)?;
                    }
                    KeyCode::Tab => {
                        if let Some(completed) = complete(&view.input) {
                            view.input = completed;
                        }
                    }
                    KeyCode::Backspace => {
                        view.input.pop();
                    }
                    KeyCode::Char(ch)
                        if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        view.input.push(ch)
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        Ok(())
    }
    .await;
    signal.abort();
    if let Some(mut run) = active {
        interrupt(&mut run);
        // A display/input error still lets the runtime settle its own cancellation.
        let _ = run.result.await;
    }
    outcome
}

fn create_session(root: &std::path::Path) -> Result<PathBuf> {
    let state = crate::preferences::state_root()?;
    std::fs::create_dir_all(&state)?;
    if state.canonicalize()?.starts_with(root) {
        anyhow::bail!(
            "Microgentic state must be outside the project; set XDG_STATE_HOME to another directory"
        );
    }
    Ok(tempfile::Builder::new()
        .prefix("session-")
        .tempdir_in(state)?
        .keep()
        .join("conversation.json"))
}

fn ready_for_next() -> Status {
    Status::new(Phase::Ready, "Ready for your next message. Esc to quit.")
}

fn stopped() -> Status {
    Status::new(Phase::Stopped, "Stopped. You can follow up or Esc to quit.")
}

fn apply_event(view: &mut View, run: &mut ActiveRun, event: ConversationEvent) {
    match event {
        ConversationEvent::ReplyStarted => {
            view.history.push(Role::Plain, "\n");
            view.history.push(Role::AgentLabel, "Microgentic:");
            view.history.push(Role::Plain, " ");
            if !run.streamed_text.is_empty() && !run.streamed_text.ends_with('\n') {
                run.streamed_text.push('\n');
            }
            run.activity = "Waiting for response".into();
        }
        ConversationEvent::Content { text } => {
            view.history.push(Role::Plain, &text);
            run.streamed_text.push_str(&text);
            run.activity = "Replying".into();
        }
        ConversationEvent::ToolStarted { name } => {
            run.used_project_tools = true;
            run.activity = format!("Using {name}");
            view.history
                .push(Role::Muted, &format!("\n  {}\n", run.activity));
        }
        ConversationEvent::Activity { message } => {
            if message.ends_with("reported an error") {
                view.history
                    .push(Role::Warning, &format!("\n  {message}\n"));
            } else if message.ends_with("finished") || message.starts_with("Context shortened") {
                view.history.push(Role::Muted, &format!("\n  {message}\n"));
            }
            run.activity = message;
        }
        ConversationEvent::Finished { summary } => {
            let final_summary = summary["final_summary"].as_str().unwrap_or("No summary");
            if run.streamed_text.is_empty() || !run.streamed_text.contains(final_summary) {
                view.history
                    .push(Role::Plain, &format!("\nResult: {final_summary}\n"));
            }
            // Trace visibility follows this exchange's actual work, never its
            // wording or tool history retained from an earlier prompt.
            if run.used_project_tools {
                view.history.push(
                    Role::Muted,
                    &format!(
                        "\nEvidence: {}\n",
                        summary["trace_file"].as_str().unwrap_or("unknown")
                    ),
                );
            }
            view.status = ready_for_next();
            run.terminal_reported = true;
        }
        ConversationEvent::Failed { message } => {
            view.history.push(
                Role::Error,
                &format!("\n[Request stopped; unfinished reply is incomplete]\n{message}\n"),
            );
            view.status = stopped();
            run.terminal_reported = true;
        }
    }
}

fn interrupt(run: &mut ActiveRun) {
    #[cfg(unix)]
    if let Some(pid) = run.pid {
        // Only our worker is signalled; its runtime owns cancellation and cleanup.
        unsafe {
            libc::kill(pid as i32, libc::SIGINT);
        }
    }
    run.cancelled = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use styled::rows_text;

    fn session() -> Session {
        Session {
            root: PathBuf::from("/p"),
            settings: Preferences {
                model: "first".into(),
                reasoning_effort: Some("low".into()),
                ..Default::default()
            },
            state: Some(PathBuf::from("/state/session-1/conversation.json")),
            model_listing: None,
        }
    }

    fn view() -> View {
        let mut view = View {
            history: History::default(),
            input: String::new(),
            scroll: 3,
            status: Status::new(Phase::Ready, "Ready"),
        };
        view.history.reset();
        view
    }

    fn history_text(view: &View) -> String {
        view.history
            .text
            .runs
            .iter()
            .map(|(_, text)| text.as_str())
            .collect()
    }

    #[test]
    fn status_labels_carry_meaning_without_colour() {
        let line = Status::new(Phase::Stopped, "Stopped. You can follow up or Esc to quit.").line();
        assert_eq!(
            rows_text(&wrap(&line, 80)),
            ["● Stopped. You can follow up or Esc to quit."]
        );
        assert_eq!(line.runs[0].0, Role::Stopped);
    }

    #[tokio::test]
    async fn active_runs_refuse_commands_except_help() {
        let mut view = view();
        let mut session = session();
        // A finished placeholder run stands in for an active reply.
        let (_sender, events) = unbounded_channel();
        let mut active = Some(ActiveRun {
            pid: None,
            started: Instant::now(),
            cancelled: false,
            result: tokio::spawn(async { Err(std::io::Error::other("unused")) }),
            events,
            activity: String::new(),
            streamed_text: String::new(),
            used_project_tools: false,
            terminal_reported: false,
        });
        view.input = "/model other".into();
        submit(&mut view, &mut session, &mut active).unwrap();
        assert_eq!(session.settings.model, "first");
        assert!(
            history_text(&view).contains("/model is unavailable while Microgentic is replying.")
        );
        view.input = "/help".into();
        submit(&mut view, &mut session, &mut active).unwrap();
        assert!(history_text(&view).contains("Commands:"));
        view.input = "A drafted message".into();
        submit(&mut view, &mut session, &mut active).unwrap();
        assert_eq!(
            view.input, "A drafted message",
            "a draft was consumed during a reply"
        );
    }

    #[test]
    fn header_emphasises_model_and_effort() {
        let prefs = crate::preferences::Preferences {
            model: "m1".into(),
            ..Default::default()
        };
        let header = header(std::path::Path::new("/p"), &prefs);
        assert_eq!(
            rows_text(&wrap(&header, 200)),
            ["/p | omlx | m1 | effort: model default"]
        );
        let values: Vec<_> = header
            .runs
            .iter()
            .filter(|(role, _)| *role == Role::Value)
            .map(|(_, text)| text.as_str())
            .collect();
        assert_eq!(values, ["m1", "model default"]);
    }
}
