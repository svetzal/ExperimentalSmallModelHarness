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
    style::{Attribute, Color, ContentStyle, PrintStyledContent},
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
    used_project_tools: bool,
    terminal_reported: bool,
}

/// What a run of text means on screen. Every role stays readable without colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Plain,
    Title,
    Muted,
    Value,
    UserLabel,
    AgentLabel,
    Warning,
    Error,
    Prompt,
    Ready,
    Working,
    Stopped,
}

/// Terminal styling, decided once at startup.
///
/// Named ANSI colours follow the user's terminal theme, so the palette works on
/// light and dark backgrounds. The normal-intensity colours are used because the
/// bright variants (bright yellow, bright cyan) wash out on light backgrounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Palette {
    colour: bool,
}

impl Palette {
    /// `NO_COLOR` (<https://no-color.org>) disables colour when present and
    /// non-empty. Attributes such as bold and dim remain, as the convention allows.
    fn from_no_color(value: Option<&std::ffi::OsStr>) -> Self {
        Self {
            colour: value.is_none_or(|value| value.is_empty()),
        }
    }

    fn style(self, role: Role) -> ContentStyle {
        let (colour, attribute) = match role {
            Role::Plain => (None, None),
            Role::Value => (None, Some(Attribute::Bold)),
            Role::Title | Role::AgentLabel => (Some(Color::DarkMagenta), Some(Attribute::Bold)),
            Role::UserLabel | Role::Prompt => (Some(Color::DarkCyan), Some(Attribute::Bold)),
            Role::Muted => (None, Some(Attribute::Dim)),
            Role::Warning => (Some(Color::DarkYellow), None),
            Role::Error => (Some(Color::DarkRed), None),
            Role::Ready => (Some(Color::DarkGreen), Some(Attribute::Bold)),
            Role::Working => (Some(Color::DarkYellow), Some(Attribute::Bold)),
            Role::Stopped => (Some(Color::DarkRed), Some(Attribute::Bold)),
        };
        let mut style = ContentStyle::new();
        if self.colour {
            style.foreground_color = colour;
        }
        if let Some(attribute) = attribute {
            style.attributes.set(attribute);
        }
        style
    }
}

/// Text made of runs that each carry a role. Adjacent text with the same role
/// shares one run, so streamed reply chunks extend the reply in place.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Styled {
    runs: Vec<(Role, String)>,
}

impl Styled {
    fn push(&mut self, role: Role, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.runs.last_mut() {
            Some((last, existing)) if *last == role => existing.push_str(text),
            _ => self.runs.push((role, text.to_owned())),
        }
    }

    fn with(mut self, role: Role, text: &str) -> Self {
        self.push(role, text);
        self
    }
}

/// One screen row of styled runs.
type Row = Vec<(Role, String)>;

/// Conversation history with a revision that changes on every edit, so the
/// redraw check never needs to compare or clone the whole history.
#[derive(Default)]
struct History {
    text: Styled,
    revision: usize,
}

impl History {
    fn push(&mut self, role: Role, text: &str) {
        self.text.push(role, text);
        self.revision += 1;
    }
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

/// Split styled text into rows of at most `width` terminal cells. Newlines end
/// rows, other control characters are removed, and roles stay attached to
/// their characters across row breaks.
fn wrap(text: &Styled, width: usize) -> Vec<Row> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = Row::new();
    let mut cells = 0;
    for (role, run) in &text.runs {
        for ch in run.chars() {
            if ch == '\n' {
                rows.push(std::mem::take(&mut row));
                cells = 0;
                continue;
            }
            if ch.is_control() {
                continue;
            }
            let size = ch.width().unwrap_or(0);
            if cells + size > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                cells = 0;
            }
            match row.last_mut() {
                Some((last, existing)) if last == role => existing.push(ch),
                _ => row.push((*role, ch.to_string())),
            }
            cells += size;
        }
    }
    rows.push(row);
    rows
}

fn first_row(text: &Styled, width: usize) -> Row {
    wrap(text, width).swap_remove(0)
}

fn header(root: &std::path::Path, prefs: &crate::preferences::Preferences) -> Styled {
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
    put(1, &first_row(header, width))?;
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
        &first_row(
            &Styled::default().with(
                Role::Muted,
                "Enter send | Alt-Enter newline | PgUp/PgDn scroll | Ctrl-C cancel | Esc quit",
            ),
            width,
        ),
    )?;
    out.flush()?;
    Ok(())
}

pub async fn run(config: Option<PathBuf>) -> Result<()> {
    let path = crate::preferences::config_path(config.as_deref())?;
    let prefs = crate::preferences::load(&path)?.unwrap_or_default();
    let root = std::env::current_dir()?;
    let header = header(&root, &prefs);
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
    view.history.push(Role::Muted, "Talk to Microgentic. Ask for work when you're ready.\nWhen requested, Microgentic can edit files and run project commands.\nConfigure defaults with microgentic setup outside this view.\n");
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
            let signature = (
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
                    KeyCode::Enter if active.is_none() && !view.input.trim().is_empty() => {
                        let prompt = std::mem::take(&mut view.input);
                        view.history.push(Role::Plain, "\n");
                        view.history.push(Role::UserLabel, "You:");
                        view.history.push(Role::Plain, &format!(" {prompt}\n"));
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
                                    used_project_tools: false,
                                    terminal_reported: false,
                                })
                            }
                            Err(error) => view
                                .history
                                .push(Role::Error, &format!("\nCould not start run: {error}\n")),
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

    /// Row text without roles, for layout assertions.
    fn wrapped(text: &str, width: usize) -> Vec<String> {
        rows_text(&wrap(&Styled::default().with(Role::Plain, text), width))
    }

    fn rows_text(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| row.iter().map(|(_, text)| text.as_str()).collect())
            .collect()
    }

    #[test]
    fn wrapping_tracks_terminal_cells_and_removes_control_sequences() {
        assert_eq!(wrapped("界a界", 3), ["界a", "界"]);
        assert_eq!(wrapped("abc\ndef", 2), ["ab", "c", "de", "f"]);
        assert!(!wrapped("\u{1b}[2J", 10)[0].contains('\u{1b}'));
    }

    #[test]
    fn wrapping_counts_cells_across_runs_and_keeps_each_role() {
        let text = Styled::default()
            .with(Role::UserLabel, "You:")
            .with(Role::Plain, " 界界\nok");
        let rows = wrap(&text, 7);
        assert_eq!(rows_text(&rows), ["You: 界", "界", "ok"]);
        assert_eq!(
            rows[0],
            [
                (Role::UserLabel, "You:".to_owned()),
                (Role::Plain, " 界".to_owned())
            ]
        );
        assert_eq!(rows[1], [(Role::Plain, "界".to_owned())]);
    }

    #[test]
    fn styled_text_extends_the_last_run_for_streamed_content() {
        let mut history = History::default();
        history.push(Role::AgentLabel, "Microgentic:");
        history.push(Role::Plain, " Hel");
        history.push(Role::Plain, "lo");
        history.push(Role::Plain, "");
        assert_eq!(
            history.text.runs,
            [
                (Role::AgentLabel, "Microgentic:".to_owned()),
                (Role::Plain, " Hello".to_owned())
            ]
        );
        // Every edit changes the redraw signature, even one that adds nothing visible.
        assert_eq!(history.revision, 4);
    }

    #[test]
    fn no_color_disables_colour_only_when_present_and_nonempty() {
        use std::ffi::OsStr;
        assert!(Palette::from_no_color(None).colour);
        assert!(Palette::from_no_color(Some(OsStr::new(""))).colour);
        assert!(!Palette::from_no_color(Some(OsStr::new("1"))).colour);
    }

    #[test]
    fn monochrome_styles_keep_attributes_without_colour() {
        let mono = Palette { colour: false };
        let colour = Palette { colour: true };
        for role in [Role::Title, Role::UserLabel, Role::Error, Role::Ready] {
            assert!(colour.style(role).foreground_color.is_some(), "{role:?}");
            assert!(mono.style(role).foreground_color.is_none(), "{role:?}");
        }
        assert!(mono.style(Role::Title).attributes.has(Attribute::Bold));
        assert!(mono.style(Role::Muted).attributes.has(Attribute::Dim));
        assert_eq!(colour.style(Role::Plain), ContentStyle::new());
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
