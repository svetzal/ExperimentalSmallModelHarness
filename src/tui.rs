//! Terminal conversation with native history and a live worker event stream.
use crate::agent::Provider;
use crate::conversation::ConversationEvent;
use crate::preferences::Preferences;
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
    System,
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
            Role::System => (None, Some(Attribute::Italic)),
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

    fn reset(&mut self) {
        self.text = Styled::default();
        self.revision += 1;
        self.push(Role::Muted, WELCOME);
    }
}

const WELCOME: &str = "Talk to Microgentic. Ask for work when you're ready.\nWhen requested, Microgentic can edit files and run project commands.\nConfigure defaults with microgentic setup outside this view.\nType /help for commands.\n";

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

/// Drop leading characters so one line fits `width` cells, marking the cut
/// with "…". A long project path then gives way before the model and effort.
fn fit_from_end(text: &Styled, width: usize) -> Styled {
    let cells = |run: &str| run.chars().map(|ch| ch.width().unwrap_or(0)).sum::<usize>();
    let total: usize = text.runs.iter().map(|(_, run)| cells(run)).sum();
    let Some((first_role, _)) = text.runs.first() else {
        return text.clone();
    };
    if total <= width || width < 2 {
        return text.clone();
    }
    let mut excess = total - (width - 1);
    let mut fitted = Styled::default().with(*first_role, "…");
    for (role, run) in &text.runs {
        let kept: String = run
            .chars()
            .skip_while(|ch| {
                let drop = excess > 0;
                excess = excess.saturating_sub(ch.width().unwrap_or(0));
                drop
            })
            .collect();
        fitted.push(*role, &kept);
    }
    fitted
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

/// A slash command. Commands act on this session and never reach the model.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    ShowModel,
    SetModel(String),
    ShowEffort,
    /// `None` returns to the model's own default.
    SetEffort(Option<String>),
    Clear,
}

/// What Enter does with the input.
#[derive(Debug, PartialEq, Eq)]
enum Submission {
    Message(String),
    Command(Command),
    Invalid(String),
}

struct CommandSpec {
    name: &'static str,
    argument: Option<&'static str>,
    summary: &'static str,
}

impl CommandSpec {
    fn usage(&self) -> String {
        match self.argument {
            Some(argument) => format!("{} {argument}", self.name),
            None => self.name.to_owned(),
        }
    }
}

/// One table drives help, footer hints, and Tab completion.
const COMMANDS: [CommandSpec; 4] = [
    CommandSpec {
        name: "/model",
        argument: Some("[id]"),
        summary: "Show the model and list available models, or switch for this session",
    },
    CommandSpec {
        name: "/effort",
        argument: Some("[level]"),
        summary: "Show or set reasoning effort for this session",
    },
    CommandSpec {
        name: "/clear",
        argument: None,
        summary: "Start a new conversation",
    },
    CommandSpec {
        name: "/help",
        argument: None,
        summary: "List commands",
    },
];

/// Accepted `/effort` values: `default` plus every level the runtime parses.
const EFFORT_CHOICES: [&str; 7] = [
    "default", "disabled", "enabled", "low", "medium", "high", "xhigh",
];

const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// Decide whether input is a message or a command. A leading `//` sends the
/// message with one leading `/`. Messages are otherwise kept literally.
fn parse_submission(input: &str) -> Submission {
    if let Some(rest) = input.strip_prefix("//") {
        return Submission::Message(format!("/{rest}"));
    }
    if !input.starts_with('/') {
        return Submission::Message(input.to_owned());
    }
    let line = input.trim();
    let (name, argument) = match line.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim()),
        None => (line, ""),
    };
    let command = match (name, argument) {
        ("/help", "") => Command::Help,
        ("/clear", "") => Command::Clear,
        ("/help" | "/clear", _) => {
            return Submission::Invalid(format!("{name} does not take an argument."));
        }
        ("/model", "") => Command::ShowModel,
        ("/model", id) if crate::preferences::valid_model_id(id) => {
            Command::SetModel(id.to_owned())
        }
        ("/model", _) => {
            return Submission::Invalid(
                "A model ID must be one line without control characters.".into(),
            );
        }
        ("/effort", "") => Command::ShowEffort,
        ("/effort", "default") => Command::SetEffort(None),
        ("/effort", level) if crate::cli::parse_reasoning_effort(level).is_ok() => {
            Command::SetEffort(Some(level.to_owned()))
        }
        ("/effort", level) => {
            return Submission::Invalid(format!(
                "Unknown effort \"{level}\". Choose {}.",
                EFFORT_CHOICES.join(", ")
            ));
        }
        _ => {
            return Submission::Invalid(format!(
                "Unknown command {name}. Type /help to see commands."
            ));
        }
    };
    Submission::Command(command)
}

/// Commands that match what is typed so far, or `None` when the input is not
/// a command. After the name is complete, only that command matches.
fn command_matches(input: &str) -> Option<Vec<&'static CommandSpec>> {
    if !input.starts_with('/') || input.starts_with("//") {
        return None;
    }
    let matches = match input.split_once(char::is_whitespace) {
        Some((name, _)) => COMMANDS.iter().filter(|c| c.name == name).collect(),
        None => COMMANDS
            .iter()
            .filter(|c| c.name.starts_with(input))
            .collect(),
    };
    Some(matches)
}

/// Tab completion: the unique command matching a partly typed name.
fn complete(input: &str) -> Option<String> {
    if input.contains(char::is_whitespace) {
        return None;
    }
    match command_matches(input)?.as_slice() {
        [only] if only.argument.is_some() => Some(format!("{} ", only.name)),
        [only] => Some(only.name.to_owned()),
        _ => None,
    }
}

fn footer(input: &str) -> Styled {
    let text = match command_matches(input).as_deref() {
        None => "Enter send | Alt-Enter newline | PgUp/PgDn scroll | Ctrl-C cancel | Esc quit | /help commands".to_owned(),
        Some([]) => "No matching command. Type /help to see commands.".to_owned(),
        Some([only]) => format!("{} · {} · Tab completes", only.usage(), only.summary),
        Some(several) => format!(
            "{} · Tab completes a unique match",
            several
                .iter()
                .map(|c| c.name)
                .collect::<Vec<_>>()
                .join("  ")
        ),
    };
    Styled::default().with(Role::Muted, &text)
}

fn help_text() -> String {
    let width = COMMANDS.iter().map(|c| c.usage().len()).max().unwrap_or(0);
    let mut text = String::from("Commands:\n");
    for command in &COMMANDS {
        text.push_str(&format!(
            "  {:width$}  {}\n",
            command.usage(),
            command.summary
        ));
    }
    text.push_str("Start a message with // to send it with one leading /.\n");
    text
}

fn effort_label(settings: &Preferences) -> &str {
    settings
        .reasoning_effort
        .as_deref()
        .unwrap_or("model default")
}

/// Session-only settings and conversation state. Commands change these;
/// nothing here is written to the configuration file.
struct Session {
    root: PathBuf,
    settings: Preferences,
    /// None until the first message, and again after `/clear` or a model or
    /// effort change, so the next message starts a new conversation.
    state: Option<PathBuf>,
    model_listing: Option<JoinHandle<Result<Vec<String>, String>>>,
}

impl Session {
    /// The worker binds a conversation to its model and effort, so a change
    /// starts a new conversation. Old session files stay on disk with traces.
    fn change_settings(&mut self, change: impl FnOnce(&mut Preferences)) -> String {
        change(&mut self.settings);
        let mut note = String::new();
        if self.state.take().is_some() {
            note.push_str(" Your next message starts a new conversation, because a conversation keeps the model and effort it began with.");
        }
        note.push_str(" Use microgentic setup to make it the default.");
        note
    }
}

fn run_command(view: &mut View, session: &mut Session, command: Command) {
    let output = match command {
        Command::Help => help_text(),
        Command::Clear => {
            session.state = None;
            view.history.reset();
            view.history
                .push(Role::System, "\nStarted a new conversation.\n");
            return;
        }
        Command::ShowModel => {
            let provider = session.settings.provider;
            let mut text = format!(
                "Model: {} on {}. Switch for this session with /model <id>.\n",
                session.settings.model,
                provider.as_str()
            );
            if session.model_listing.is_none() {
                session.model_listing = Some(tokio::spawn(available_models(provider)));
                text.push_str(&format!(
                    "Asking {} for available models.\n",
                    provider.as_str()
                ));
            }
            text
        }
        Command::SetModel(model) if model == session.settings.model => {
            format!("Already using {model}.\n")
        }
        Command::SetModel(model) => {
            let note = session.change_settings(|settings| settings.model = model.clone());
            format!("Model set to {model} for this session.{note}\n")
        }
        Command::ShowEffort => format!(
            "Effort: {}. Choose with /effort <level>: {}. default uses the model's own setting.\n",
            effort_label(&session.settings),
            EFFORT_CHOICES.join(", ")
        ),
        Command::SetEffort(effort) if effort == session.settings.reasoning_effort => {
            format!("Effort is already {}.\n", effort_label(&session.settings))
        }
        Command::SetEffort(effort) => {
            let note = session.change_settings(|settings| settings.reasoning_effort = effort);
            format!(
                "Effort set to {} for this session.{note}\n",
                effort_label(&session.settings)
            )
        }
    };
    view.history.push(Role::System, &output);
}

/// Show the provider's model list once its request settles.
async fn finish_model_listing(view: &mut View, session: &mut Session) {
    let Some(listing) = session
        .model_listing
        .take_if(|listing| listing.is_finished())
    else {
        return;
    };
    let provider = session.settings.provider.as_str();
    match listing.await.map_err(|error| error.to_string()).flatten() {
        Ok(models) if models.is_empty() => view
            .history
            .push(Role::System, &format!("\n{provider} reported no models.\n")),
        Ok(models) => {
            let mut text = format!("\nAvailable models from {provider}:\n");
            for model in models {
                let current = if model == session.settings.model {
                    " (current)"
                } else {
                    ""
                };
                text.push_str(&format!("  {model}{current}\n"));
            }
            view.history.push(Role::System, &text);
        }
        Err(error) => view.history.push(
            Role::Error,
            &format!("\nCould not list models from {provider}: {error}\n"),
        ),
    }
}

/// Ask the provider for its models, bounded so a stalled server cannot
/// leave the request open.
async fn available_models(provider: Provider) -> Result<Vec<String>, String> {
    use mojentic::llm::LlmGateway;
    use mojentic::llm::gateways::{OllamaGateway, OmlxGateway};
    let request = async {
        match provider {
            Provider::Omlx => OmlxGateway::new().get_available_models().await,
            Provider::Ollama => OllamaGateway::new().get_available_models().await,
        }
    };
    match tokio::time::timeout(MODEL_LIST_TIMEOUT, request).await {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(_) => Err(format!(
            "no answer within {} seconds",
            MODEL_LIST_TIMEOUT.as_secs()
        )),
    }
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

    fn command(input: &str) -> Command {
        match parse_submission(input) {
            Submission::Command(command) => command,
            other => panic!("{input:?} gave {other:?}"),
        }
    }

    fn invalid(input: &str) -> String {
        match parse_submission(input) {
            Submission::Invalid(message) => message,
            other => panic!("{input:?} gave {other:?}"),
        }
    }

    #[test]
    fn commands_parse_with_and_without_arguments() {
        assert_eq!(command("/help"), Command::Help);
        assert_eq!(command("/clear"), Command::Clear);
        assert_eq!(command("/model"), Command::ShowModel);
        assert_eq!(
            command("/model qwen3:8b"),
            Command::SetModel("qwen3:8b".into())
        );
        assert_eq!(command("/effort"), Command::ShowEffort);
        assert_eq!(
            command("/effort high"),
            Command::SetEffort(Some("high".into()))
        );
        assert_eq!(command("/effort default"), Command::SetEffort(None));
    }

    #[test]
    fn command_whitespace_around_names_and_arguments_is_ignored() {
        assert_eq!(command("/help  "), Command::Help);
        assert_eq!(
            command("/model   my-model  "),
            Command::SetModel("my-model".into())
        );
        assert_eq!(
            command("/effort\tlow"),
            Command::SetEffort(Some("low".into()))
        );
        assert_eq!(command("/model\n"), Command::ShowModel);
    }

    #[test]
    fn every_listed_effort_is_accepted_and_others_are_rejected() {
        for level in EFFORT_CHOICES {
            let expected = (level != "default").then(|| level.to_owned());
            assert_eq!(
                command(&format!("/effort {level}")),
                Command::SetEffort(expected)
            );
        }
        for bad in ["extreme", "HIGH", "low medium", "none"] {
            let message = invalid(&format!("/effort {bad}"));
            assert!(message.contains(bad), "{message}");
            assert!(message.contains("default, disabled, enabled, low, medium, high, xhigh"));
        }
    }

    #[test]
    fn model_ids_follow_the_configuration_rule() {
        assert!(invalid("/model first\nsecond").contains("control characters"));
        assert_eq!(
            command("/model name with spaces"),
            Command::SetModel("name with spaces".into())
        );
    }

    #[test]
    fn unknown_commands_and_stray_arguments_are_rejected_locally() {
        assert_eq!(
            invalid("/nope"),
            "Unknown command /nope. Type /help to see commands."
        );
        assert_eq!(
            invalid("/"),
            "Unknown command /. Type /help to see commands."
        );
        assert_eq!(
            invalid("/Help"),
            "Unknown command /Help. Type /help to see commands."
        );
        assert_eq!(invalid("/clear now"), "/clear does not take an argument.");
        assert_eq!(invalid("/help me"), "/help does not take an argument.");
    }

    #[test]
    fn messages_stay_literal_and_double_slash_escapes_one_slash() {
        assert_eq!(
            parse_submission("Hello /help"),
            Submission::Message("Hello /help".into())
        );
        assert_eq!(
            parse_submission(" /help"),
            Submission::Message(" /help".into())
        );
        assert_eq!(
            parse_submission("//etc/hosts is a path"),
            Submission::Message("/etc/hosts is a path".into())
        );
        assert_eq!(parse_submission("//"), Submission::Message("/".into()));
    }

    #[test]
    fn hints_narrow_as_the_name_is_typed() {
        let names = |input: &str| {
            command_matches(input).map(|found| found.iter().map(|c| c.name).collect::<Vec<_>>())
        };
        assert_eq!(names("hello"), None);
        assert_eq!(names("//x"), None);
        assert_eq!(names("/").unwrap().len(), COMMANDS.len());
        assert_eq!(names("/m").unwrap(), ["/model"]);
        assert_eq!(names("/model other"), Some(vec!["/model"]));
        assert_eq!(names("/mo x"), Some(vec![]));
        assert_eq!(names("/z"), Some(vec![]));
        let footer_text = |input: &str| rows_text(&wrap(&footer(input), 200)).remove(0);
        assert!(footer_text("").starts_with("Enter send"));
        assert!(footer_text("/e").starts_with("/effort [level] · Show or set"));
        assert!(footer_text("/").contains("/model  /effort  /clear  /help"));
        assert!(footer_text("/z").starts_with("No matching command"));
    }

    #[test]
    fn tab_completes_only_a_unique_match() {
        assert_eq!(complete("/m").as_deref(), Some("/model "));
        assert_eq!(complete("/cl").as_deref(), Some("/clear"));
        assert_eq!(complete("/"), None);
        assert_eq!(complete("/model x"), None);
        assert_eq!(complete("hello"), None);
    }

    #[test]
    fn help_lists_every_command_and_the_escape() {
        let help = help_text();
        for command in &COMMANDS {
            assert!(help.contains(&command.usage()) && help.contains(command.summary));
        }
        assert!(help.contains("//"));
    }

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
    fn changing_model_or_effort_starts_a_new_conversation_without_saving() {
        let mut view = view();
        let mut session = session();
        run_command(&mut view, &mut session, Command::SetModel("second".into()));
        assert_eq!(session.settings.model, "second");
        assert!(session.state.is_none());
        assert!(history_text(&view).contains("next message starts a new conversation"));
        assert!(history_text(&view).contains("microgentic setup"));
        assert_eq!(view.history.text.runs.last().unwrap().0, Role::System);

        session.state = Some(PathBuf::from("/state/session-2/conversation.json"));
        run_command(&mut view, &mut session, Command::SetModel("second".into()));
        assert!(
            session.state.is_some(),
            "an unchanged model kept the conversation"
        );
        assert!(history_text(&view).ends_with("Already using second.\n"));

        run_command(&mut view, &mut session, Command::SetEffort(None));
        assert_eq!(session.settings.reasoning_effort, None);
        assert!(session.state.is_none());
        assert!(history_text(&view).contains("Effort set to model default for this session."));
    }

    #[test]
    fn clear_drops_the_conversation_and_restores_the_welcome() {
        let mut view = view();
        view.history.push(Role::Plain, "\nYou: earlier\n");
        let mut session = session();
        run_command(&mut view, &mut session, Command::Clear);
        assert!(session.state.is_none());
        assert_eq!(
            history_text(&view),
            format!("{WELCOME}\nStarted a new conversation.\n")
        );
        assert_eq!(session.settings.model, "first", "clearing changed settings");
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
    fn long_headers_lose_the_start_of_the_path_first() {
        let header = Styled::default()
            .with(Role::Muted, "/a/long/project | omlx | ")
            .with(Role::Value, "m1");
        let fitted = fit_from_end(&header, 16);
        assert_eq!(rows_text(&wrap(&fitted, 16)), ["…ect | omlx | m1"]);
        assert_eq!(fitted.runs.last(), Some(&(Role::Value, "m1".to_owned())));
        assert_eq!(fit_from_end(&header, 80), header);
        // A wide character that straddles the cut is dropped whole.
        let wide = Styled::default().with(Role::Muted, "界界ab");
        assert_eq!(rows_text(&wrap(&fit_from_end(&wide, 4), 4)), ["…ab"]);
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
