//! Slash commands: parsing, help, footer hints, and completion.
use super::styled::{Role, Styled};

/// A slash command. Commands act on this session and never reach the model.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Command {
    Help,
    ShowModel,
    SetModel(String),
    ShowEffort,
    /// `None` returns to the model's own default.
    SetEffort(Option<String>),
    Clear,
    Guidance,
}

/// What Enter does with the input.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Submission {
    Message(String),
    Command(Command),
    Invalid(String),
}

struct CommandSpec {
    pub(super) name: &'static str,
    pub(super) argument: Option<&'static str>,
    pub(super) summary: &'static str,
}

impl CommandSpec {
    pub(super) fn usage(&self) -> String {
        match self.argument {
            Some(argument) => format!("{} {argument}", self.name),
            None => self.name.to_owned(),
        }
    }
}

/// One table drives help, footer hints, and Tab completion.
const COMMANDS: [CommandSpec; 5] = [
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
        name: "/guidance",
        argument: None,
        summary: "List the project guidance files and their status",
    },
    CommandSpec {
        name: "/help",
        argument: None,
        summary: "List commands",
    },
];

/// Accepted `/effort` values: `default` plus every level the runtime parses.
pub(super) const EFFORT_CHOICES: [&str; 7] = [
    "default", "disabled", "enabled", "low", "medium", "high", "xhigh",
];

/// Decide whether input is a message or a command. A leading `//` sends the
/// message with one leading `/`. Messages are otherwise kept literally.
pub(super) fn parse_submission(input: &str) -> Submission {
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
        ("/guidance", "") => Command::Guidance,
        ("/help" | "/clear" | "/guidance", _) => {
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
pub(super) fn complete(input: &str) -> Option<String> {
    if input.contains(char::is_whitespace) {
        return None;
    }
    match command_matches(input)?.as_slice() {
        [only] if only.argument.is_some() => Some(format!("{} ", only.name)),
        [only] => Some(only.name.to_owned()),
        _ => None,
    }
}

pub(super) fn footer(input: &str) -> Styled {
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

pub(super) fn help_text() -> String {
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

#[cfg(test)]
mod tests {
    use super::super::styled::rows_text;
    use super::super::wrap::wrap;
    use super::*;

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
        assert_eq!(command("/guidance"), Command::Guidance);
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
        assert!(footer_text("/").contains("/model  /effort  /clear  /guidance  /help"));
        assert!(footer_text("/g").starts_with("/guidance · List the project guidance"));
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
}
