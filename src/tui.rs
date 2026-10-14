//! Terminal conversation with native history and a live worker event stream.
use crate::conversation::ConversationEvent;
use anyhow::{Context, Result};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute, queue,
    style::Print,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::{Write, stdout};
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::JoinHandle;
use unicode_width::UnicodeWidthChar;

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
    terminal_reported: bool,
}

#[derive(Default)]
struct View {
    history: String,
    input: String,
    scroll: usize,
    status: String,
}

fn wrapped(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        let mut cells = 0;
        for ch in line.chars().filter(|ch| !ch.is_control()) {
            let size = ch.width().unwrap_or(0);
            if cells + size > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                cells = 0;
            }
            row.push(ch);
            cells += size;
        }
        rows.push(row);
    }
    rows
}

fn draw(view: &View, header: &str) -> Result<()> {
    let (columns, lines) = terminal::size()?;
    let width = usize::from(columns.saturating_sub(2)).max(1);
    let mut out = stdout();
    queue!(out, MoveTo(0, 0), Clear(ClearType::All))?;
    let mut put = |row: u16, text: &str| -> Result<()> {
        if row < lines {
            queue!(
                out,
                MoveTo(1.min(columns.saturating_sub(1)), row),
                Print(wrapped(text, width)[0].as_str())
            )?;
        }
        Ok(())
    };
    put(0, "Microgentic")?;
    put(1, header)?;
    let input_rows = wrapped(&format!("> {}", view.input), width);
    let input_height = input_rows.len().min(4);
    let body_height = usize::from(lines.saturating_sub(5 + input_height as u16));
    let history = wrapped(&view.history, width);
    let end = history.len().saturating_sub(view.scroll.min(history.len()));
    let start = end.saturating_sub(body_height);
    for (index, row) in history[start..end].iter().enumerate() {
        put(3 + index as u16, row)?;
    }
    put(lines.saturating_sub(input_height as u16 + 2), &view.status)?;
    for (index, row) in input_rows
        .iter()
        .rev()
        .take(input_height)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .enumerate()
    {
        put(
            lines.saturating_sub(input_height as u16 + 1) + index as u16,
            row,
        )?;
    }
    put(
        lines.saturating_sub(1),
        "Enter send | Alt-Enter newline | PgUp/PgDn scroll | Ctrl-C cancel | Esc quit",
    )?;
    out.flush()?;
    Ok(())
}

pub async fn run(config: Option<PathBuf>) -> Result<()> {
    let path = crate::preferences::config_path(config.as_deref())?;
    let prefs = crate::preferences::load(&path)?.unwrap_or_default();
    let root = std::env::current_dir()?;
    let header = format!(
        "{} | {} | {} | effort: {}",
        root.display(),
        prefs.provider.as_str(),
        prefs.model,
        prefs.reasoning_effort.as_deref().unwrap_or("model default")
    );
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
        history: "Talk to Microgentic. Ask for work when you're ready.\nWhen requested, Microgentic can edit files and run project commands.\nConfigure defaults with microgentic setup outside this view.\n".into(),
        status: "Ready".into(),
        ..View::default()
    };
    let mut active: Option<ActiveRun> = None;
    let mut session_state = None;
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
                        view.status = "Ready for your next message. Esc to quit.".into();
                    }
                    Ok(output) => {
                        if !run.terminal_reported {
                            view.history.push_str(&format!(
                                "\n[Request stopped; any unfinished reply is incomplete]\nRun error: {}\n",
                                String::from_utf8_lossy(&output.stderr)
                            ));
                        }
                        view.status = "Stopped. You can follow up or Esc to quit.".into();
                    }
                    Err(error) => {
                        view.history.push_str(&format!("\nRun error: {error}\n"));
                        view.status = "Run could not start.".into();
                    }
                }
                view.scroll = 0;
            }
            if let Some(run) = &active {
                view.status = format!(
                    "{} · {}s · Ctrl-C to cancel",
                    if run.cancelled {
                        "Cancellation requested"
                    } else {
                        &run.activity
                    },
                    run.started.elapsed().as_secs()
                );
            }
            let signature = (
                view.history.len(),
                view.input.clone(),
                view.scroll,
                view.status.clone(),
                terminal::size()?,
            );
            if last_draw.as_ref() != Some(&signature) {
                draw(&view, &header)?;
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
                    KeyCode::Enter if active.is_none() && !view.input.trim().is_empty() => {
                        let prompt = std::mem::take(&mut view.input);
                        view.history.push_str(&format!("\nYou: {prompt}\n"));
                        view.scroll = 0;
                        let mut command = tokio::process::Command::new(std::env::current_exe()?);
                        let state = match &session_state {
                            Some(state) => state,
                            None => session_state.insert(create_session(&root)?),
                        };
                        command
                            .args(["conversation-worker", "--prompt", &prompt, "--session-state"])
                            .arg(state)
                            .args([
                                "--provider",
                                prefs.provider.as_str(),
                                "--model",
                                &prefs.model,
                            ])
                            .current_dir(&root)
                            .stdin(Stdio::null())
                            .stdout(Stdio::piped())
                            .stderr(Stdio::piped());
                        if let Some(effort) = &prefs.reasoning_effort {
                            command.args(["--reasoning-effort", effort]);
                        }
                        match command.spawn() {
                            Ok(mut child) => {
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
                                active = Some(ActiveRun {
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
                                    terminal_reported: false,
                                })
                            }
                            Err(error) => view
                                .history
                                .push_str(&format!("\nCould not start run: {error}\n")),
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

fn apply_event(view: &mut View, run: &mut ActiveRun, event: ConversationEvent) {
    match event {
        ConversationEvent::ReplyStarted => {
            view.history.push_str("\nMicrogentic: ");
            if !run.streamed_text.is_empty() && !run.streamed_text.ends_with('\n') {
                run.streamed_text.push('\n');
            }
            run.activity = "Waiting for response".into();
        }
        ConversationEvent::Content { text } => {
            view.history.push_str(&text);
            run.streamed_text.push_str(&text);
            run.activity = "Replying".into();
        }
        ConversationEvent::Activity { message } => {
            if message.starts_with("Using ")
                || message.ends_with("finished")
                || message.ends_with("reported an error")
                || message.starts_with("Context shortened")
            {
                view.history.push_str(&format!("\n  {message}\n"));
            }
            run.activity = message;
        }
        ConversationEvent::Finished { summary } => {
            let final_summary = summary["final_summary"].as_str().unwrap_or("No summary");
            if run.streamed_text.is_empty() || !run.streamed_text.contains(final_summary) {
                view.history
                    .push_str(&format!("\nResult: {final_summary}\n"));
            }
            view.history.push_str(&format!(
                "\nEvidence: {}\n",
                summary["trace_file"].as_str().unwrap_or("unknown")
            ));
            view.status = "Ready for your next message. Esc to quit.".into();
            run.terminal_reported = true;
        }
        ConversationEvent::Failed { message } => {
            view.history.push_str(&format!(
                "\n[Request stopped; unfinished reply is incomplete]\n{message}\n"
            ));
            view.status = "Stopped. You can follow up or Esc to quit.".into();
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
    #[test]
    fn wrapping_tracks_terminal_cells_and_removes_control_sequences() {
        assert_eq!(wrapped("界a界", 3), ["界a", "界"]);
        assert_eq!(wrapped("abc\ndef", 2), ["ab", "c", "de", "f"]);
        assert!(!wrapped("\u{1b}[2J", 10)[0].contains('\u{1b}'));
    }
}
