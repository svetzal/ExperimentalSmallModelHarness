//! Session-only settings and the commands that change them.
use super::View;
use super::commands::{Command, EFFORT_CHOICES, help_text};
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

pub(super) fn run_command(view: &mut View, session: &mut Session, command: Command) {
    let output = match command {
        Command::Help => help_text(),
        Command::Clear => {
            session.state = None;
            view.history.reset();
            view.history.start_block();
            view.history
                .push(Role::System, "Started a new conversation.\n");
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
pub(super) async fn finish_model_listing(view: &mut View, session: &mut Session) {
    let Some(listing) = session
        .model_listing
        .take_if(|listing| listing.is_finished())
    else {
        return;
    };
    let provider = session.settings.provider.as_str();
    view.history.start_line();
    match listing.await.map_err(|error| error.to_string()).flatten() {
        Ok(models) if models.is_empty() => view
            .history
            .push(Role::System, &format!("{provider} reported no models.\n")),
        Ok(models) => {
            let mut text = format!("Available models from {provider}:\n");
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
            &format!("Could not list models from {provider}: {error}\n"),
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

#[cfg(test)]
mod tests {
    use super::super::history::{History, WELCOME};
    use super::super::{Phase, Status};
    use super::*;

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
            .render(0)
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
        assert_eq!(view.history.render(0).runs.last().unwrap().0, Role::System);

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
}
