//! Session-only settings and the commands that change them.
use super::View;
use super::commands::{Command, EFFORT_CHOICES, help_text};
use super::picker::{Panel, Picker, Purpose};
use super::status_bar::{GitStatus, git_status};
use super::styled::Role;
use crate::agent::Provider;
use crate::guidance::{Discovery, Scope, Status};
use crate::preferences::Preferences;
use std::path::PathBuf;
use std::time::Duration;
use tokio::task::JoinHandle;

const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(5);

fn effort_label(settings: &Preferences) -> &str {
    settings
        .reasoning_effort
        .as_deref()
        .unwrap_or("model default")
}

/// Session-only settings and conversation state. Commands change these;
/// nothing here is written to the configuration file.
pub(super) struct Session {
    pub(super) root: PathBuf,
    pub(super) settings: Preferences,
    /// None until the first message, and again after `/clear` or a model or
    /// effort change, so the next message starts a new conversation.
    pub(super) state: Option<PathBuf>,
    pub(super) model_listing: Option<JoinHandle<Result<Vec<String>, String>>>,
    /// The project's git state for the status bar, `None` outside a repository.
    pub(super) git: Option<GitStatus>,
    git_refresh: Option<JoinHandle<Option<GitStatus>>>,
    /// The configuration file the session was opened with; the user-level
    /// guidance file sits beside it.
    pub(super) config_path: Option<PathBuf>,
    /// The project guidance the next message will carry, refreshed like git.
    pub(super) guidance: Discovery,
    guidance_refresh: Option<JoinHandle<Discovery>>,
}

impl Session {
    pub(super) fn new(root: PathBuf, settings: Preferences) -> Self {
        Self {
            root,
            settings,
            state: None,
            model_listing: None,
            git: None,
            git_refresh: None,
            config_path: None,
            guidance: Discovery::default(),
            guidance_refresh: None,
        }
    }

    /// What discovery needs: the project, the user-level file beside the
    /// config, and a budget for the current model.
    fn guidance_inputs(&self) -> (PathBuf, Option<PathBuf>, usize) {
        let user_file = self
            .config_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(|directory| directory.join("AGENTS.md"));
        let budget = crate::guidance::budget_for(
            crate::cli::model_context_window_tokens(&self.settings.model),
            self.settings
                .guidance_fraction
                .unwrap_or(crate::guidance::DEFAULT_WINDOW_FRACTION),
        );
        (self.root.clone(), user_file, budget)
    }

    /// Discover project guidance off the render loop, as the worker will.
    pub(super) fn refresh_guidance(&mut self) {
        if self.guidance_refresh.is_none() {
            let (root, user_file, budget) = self.guidance_inputs();
            self.guidance_refresh = Some(tokio::task::spawn_blocking(move || {
                discover_guidance(&root, user_file.as_deref(), budget)
            }));
        }
    }

    pub(super) async fn finish_guidance_refresh(&mut self) {
        if let Some(refresh) = self
            .guidance_refresh
            .take_if(|refresh| refresh.is_finished())
            && let Ok(discovery) = refresh.await
        {
            self.guidance = discovery;
        }
    }

    /// Guidance files in context, for the status bar.
    pub(super) fn guidance_count(&self) -> usize {
        self.guidance
            .files
            .iter()
            .filter(|file| matches!(file.status, Status::Included | Status::Truncated { .. }))
            .count()
    }

    /// Ask git for the project state without blocking the view. Called at
    /// start, after each exchange, and after `/clear`; never on a timer.
    pub(super) fn refresh_git(&mut self) {
        if self.git_refresh.is_none() {
            let root = self.root.clone();
            self.git_refresh = Some(tokio::spawn(async move { git_status(&root).await }));
        }
    }

    pub(super) async fn finish_git_refresh(&mut self) {
        if let Some(refresh) = self.git_refresh.take_if(|refresh| refresh.is_finished()) {
            self.git = refresh.await.ok().flatten();
        }
    }

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

pub(super) fn run_command(view: &mut View, session: &mut Session, command: Command) {
    let output = match command {
        Command::Help => help_text(),
        Command::Guidance => {
            // Asked for directly, so read the files now rather than show a
            // result that may be a moment old.
            let (root, user_file, budget) = session.guidance_inputs();
            session.guidance = discover_guidance(&root, user_file.as_deref(), budget);
            guidance_report(&session.guidance, &view.guidance_shown)
        }
        Command::Context => {
            view.context_panel.toggle();
            if view.context_panel.visible {
                "Context panel shown. It lists what the model received in its latest request. Ctrl-O or /context hides it.\n".into()
            } else {
                "Context panel hidden.\n".into()
            }
        }
        Command::Clear => {
            session.state = None;
            view.forget_context();
            view.guidance_shown.clear();
            session.refresh_git();
            session.refresh_guidance();
            view.history.reset();
            view.history.start_block();
            view.history
                .push(Role::System, "Started a new conversation.\n");
            return;
        }
        Command::ShowModel => {
            let provider = session.settings.provider;
            if session.model_listing.is_none() {
                session.model_listing = Some(tokio::spawn(available_models(provider)));
            }
            // The picker opens when the provider answers.
            view.panel = Some(Panel::Loading("Loading models…".into()));
            format!(
                "Model: {} on {}. Switch for this session with /model <id>.\n",
                session.settings.model,
                provider.as_str()
            )
        }
        Command::SetModel(model) if model == session.settings.model => {
            format!("Already using {model}.\n")
        }
        Command::SetModel(model) => {
            let note = session.change_settings(|settings| settings.model = model.clone());
            view.forget_context();
            format!("Model set to {model} for this session.{note}\n")
        }
        Command::ShowEffort => {
            let current = session
                .settings
                .reasoning_effort
                .as_deref()
                .unwrap_or("default");
            view.panel = Picker::new(
                Purpose::Effort,
                "Choose reasoning effort for this session",
                EFFORT_CHOICES.map(String::from).to_vec(),
                current,
            )
            .map(Panel::Choose);
            format!(
                "Effort: {}. Choose with /effort <level>: {}. default uses the model's own setting.\n",
                effort_label(&session.settings),
                EFFORT_CHOICES.join(", ")
            )
        }
        Command::SetEffort(effort) if effort == session.settings.reasoning_effort => {
            format!("Effort is already {}.\n", effort_label(&session.settings))
        }
        Command::SetEffort(effort) => {
            let note = session.change_settings(|settings| settings.reasoning_effort = effort);
            view.forget_context();
            format!(
                "Effort set to {} for this session.{note}\n",
                effort_label(&session.settings)
            )
        }
    };
    view.history.push(Role::System, &output);
}

fn discover_guidance(
    root: &std::path::Path,
    user_file: Option<&std::path::Path>,
    budget: usize,
) -> Discovery {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    crate::guidance::discover(&crate::guidance::Sources {
        project: root,
        home: home.as_deref(),
        user_file,
        budget_tokens: budget,
    })
}

/// Each guidance file with its size and status, most general first.
fn guidance_report(discovery: &Discovery, shown: &std::collections::BTreeSet<String>) -> String {
    if discovery.files.is_empty() && discovery.nested.is_empty() {
        return "No project guidance found. Microgentic reads AGENTS.md (or CLAUDE.md) from this directory up to the repository root, and from beside config.json.\n".into();
    }
    let mut text = String::from("Project guidance, most general first:\n");
    for file in discovery.files.iter().chain(&discovery.nested) {
        let status = match (&file.status, &file.scope) {
            (Status::Included, _) => "included".to_owned(),
            (
                Status::Truncated {
                    shown_chars,
                    total_chars,
                },
                _,
            ) => format!("truncated to {shown_chars} of {total_chars} characters"),
            (Status::Pending, Scope::Nested { directory }) if shown.contains(&file.shown) => {
                format!("nested, shown for {directory}/")
            }
            (Status::Pending, Scope::Nested { directory }) => {
                format!("nested, shown when work reaches {directory}/")
            }
            (Status::Pending, _) => "pending".to_owned(),
            (Status::Excluded { reason }, _) => format!("left out: {reason}"),
        };
        text.push_str(&format!(
            "  {}  {}  {status}\n",
            file.shown,
            size(file.bytes)
        ));
    }
    text
}

fn size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

/// Open the model picker once the provider's list arrives. An empty list or
/// an error is reported in the history instead. Nothing is shown when the
/// user cancelled the loading notice.
pub(super) async fn finish_model_listing(view: &mut View, session: &mut Session) {
    let Some(listing) = session
        .model_listing
        .take_if(|listing| listing.is_finished())
    else {
        return;
    };
    let result = listing.await.map_err(|error| error.to_string()).flatten();
    if !matches!(view.panel, Some(Panel::Loading(_))) {
        return;
    }
    let provider = session.settings.provider.as_str();
    view.panel = None;
    match result {
        Ok(models) => match Picker::new(
            Purpose::Model,
            &format!("Choose a model from {provider} for this session"),
            models,
            &session.settings.model,
        ) {
            Some(picker) => view.panel = Some(Panel::Choose(picker)),
            None => {
                view.history.start_line();
                view.history
                    .push(Role::System, &format!("{provider} reported no models.\n"));
            }
        },
        Err(error) => {
            view.history.start_line();
            view.history.push(
                Role::Error,
                &format!("Could not list models from {provider}: {error}\n"),
            );
        }
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

#[cfg(test)]
mod tests {
    use super::super::history::{History, WELCOME};
    use super::super::{Phase, Status};
    use super::*;

    fn session() -> Session {
        let mut session = Session::new(
            PathBuf::from("/p"),
            Preferences {
                model: "first".into(),
                reasoning_effort: Some("low".into()),
                ..Default::default()
            },
        );
        session.state = Some(PathBuf::from("/state/session-1/conversation.json"));
        session
    }

    fn view() -> View {
        let mut view = View {
            history: History::default(),
            input: String::new(),
            scroll: 3,
            status: Status::new(Phase::Ready, "Ready"),
            panel: None,
            context: None,
            standing: None,
            guidance_shown: Default::default(),
            context_panel: Default::default(),
        };
        view.history.reset();
        view
    }

    fn history_text(view: &View) -> String {
        view.history
            .render(0, 200)
            .runs
            .iter()
            .map(|(_, text)| text.as_str())
            .collect()
    }

    #[test]
    fn the_guidance_report_lists_files_sizes_and_status() {
        use crate::guidance::{GuidanceFile, Status as Found};
        let file = |shown: &str, scope: Scope, status: Found, bytes: u64| GuidanceFile {
            shown: shown.into(),
            scope,
            sha256: String::new(),
            bytes,
            status,
            text: String::new(),
        };
        let discovery = Discovery {
            files: vec![
                file(
                    "~/.config/microgentic/AGENTS.md",
                    Scope::User,
                    Found::Included,
                    300,
                ),
                file(
                    "AGENTS.md",
                    Scope::Directory {
                        levels_up: 0,
                        repository_root: true,
                    },
                    Found::Truncated {
                        shown_chars: 100,
                        total_chars: 400,
                    },
                    4_096,
                ),
            ],
            nested: vec![
                file(
                    "docs/AGENTS.md",
                    Scope::Nested {
                        directory: "docs".into(),
                    },
                    Found::Pending,
                    50,
                ),
                file(
                    "web/AGENTS.md",
                    Scope::Nested {
                        directory: "web".into(),
                    },
                    Found::Pending,
                    60,
                ),
            ],
            budget_tokens: 8_000,
        };
        let shown = std::collections::BTreeSet::from(["web/AGENTS.md".to_owned()]);
        let report = guidance_report(&discovery, &shown);
        assert_eq!(
            report,
            "Project guidance, most general first:\n  ~/.config/microgentic/AGENTS.md  300 B  included\n  AGENTS.md  4.0 KB  truncated to 100 of 400 characters\n  docs/AGENTS.md  50 B  nested, shown when work reaches docs/\n  web/AGENTS.md  60 B  nested, shown for web/\n"
        );
        assert!(
            guidance_report(&Discovery::default(), &shown)
                .starts_with("No project guidance found.")
        );
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
        assert_eq!(
            view.history.render(0, 200).runs.last().unwrap().0,
            Role::System
        );

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

    #[tokio::test]
    async fn clear_drops_the_conversation_and_restores_the_welcome() {
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
}
