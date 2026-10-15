//! Session-only settings and the commands that change them.
use super::View;
use super::commands::{Command, EFFORT_CHOICES, help_text};
use super::picker::{Panel, Picker, Purpose};
use super::status_bar::{GitStatus, git_status};
use super::styled::Role;
use crate::agent::Provider;
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
        }
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
        Command::Clear => {
            session.state = None;
            view.context = None;
            session.refresh_git();
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
            view.context = None;
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
            view.context = None;
            format!(
                "Effort set to {} for this session.{note}\n",
                effort_label(&session.settings)
            )
        }
    };
    view.history.push(Role::System, &output);
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
