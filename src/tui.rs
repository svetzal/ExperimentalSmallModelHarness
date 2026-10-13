//! Small terminal front end. Each request invokes the unchanged public run path.
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
        history: "Give Microgentic a coding task. Each task runs independently; previous messages are shown for reference.\nTasks can edit files and execute project commands.\nConfigure defaults with microgentic setup outside this view.\n".into(),
        status: "Ready".into(),
        ..View::default()
    };
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
            if active.as_ref().is_some_and(|run| run.result.is_finished()) {
                let run = active.take().expect("completed run exists");
                match run.result.await.context("waiting for agent run")? {
                    Ok(output) if output.status.success() => {
                        let summary: serde_json::Value = serde_json::from_slice(&output.stdout)
                            .context("decoding agent result")?;
                        view.history.push_str(&format!(
                            "\nMicrogentic: {}\nEvidence: {}\n",
                            summary["final_summary"].as_str().unwrap_or("No summary"),
                            summary["trace_file"].as_str().unwrap_or("unknown")
                        ));
                        view.status = "Finished. Enter another task or Esc to quit.".into();
                    }
                    Ok(output) => {
                        view.history.push_str(&format!(
                            "\nRun error: {}\n",
                            String::from_utf8_lossy(&output.stderr)
                        ));
                        view.status = "Stopped; see the error above.".into();
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
                    "{} · {}s elapsed · Ctrl-C to cancel",
                    if run.cancelled {
                        "Cancellation requested"
                    } else {
                        "Working"
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
                Event::Paste(text) if active.is_none() => view
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
                        if active.is_none() && key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        view.input.push('\n')
                    }
                    KeyCode::Enter if active.is_none() && !view.input.trim().is_empty() => {
                        let prompt = std::mem::take(&mut view.input);
                        view.history.push_str(&format!("\nYou: {prompt}\n"));
                        view.scroll = 0;
                        let mut command = tokio::process::Command::new(std::env::current_exe()?);
                        command
                            .arg("--config")
                            .arg(&path)
                            .args(["run", "--prompt", &prompt, "--json"])
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
                            Ok(child) => {
                                active = Some(ActiveRun {
                                    pid: child.id(),
                                    started: Instant::now(),
                                    cancelled: false,
                                    result: tokio::spawn(child.wait_with_output()),
                                })
                            }
                            Err(error) => view
                                .history
                                .push_str(&format!("\nCould not start run: {error}\n")),
                        }
                    }
                    KeyCode::Backspace if active.is_none() => {
                        view.input.pop();
                    }
                    KeyCode::Char(ch)
                        if active.is_none()
                            && !key
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
