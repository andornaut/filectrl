use anyhow::Error;

use super::Command;

/// The outcome of `CommandHandler::handle_command`/`handle_key`/`handle_mouse`.
///
/// Build the derived-command variants through `From` (`command.into()` for one,
/// `commands.into()` for a `Vec`) rather than naming them directly: the `Vec`
/// conversion normalizes by length, and a hand-built `HandledWithMany` holding
/// zero or one command compares unequal to the `Handled`/`HandledWith` it means.
#[derive(Clone, Debug, PartialEq)]
pub enum CommandResult {
    Handled,
    HandledWith(Box<Command>),
    HandledWithMany(Vec<Command>),
    NotHandled,
}

impl CommandResult {
    /// The derived commands, dropping the handled/not-handled distinction.
    /// The lossless way for production code to consume a result: it never
    /// assumes a single derived command.
    pub fn into_commands(self) -> Vec<Command> {
        match self {
            Self::HandledWith(command) => vec![*command],
            Self::HandledWithMany(commands) => commands,
            Self::Handled | Self::NotHandled => Vec::new(),
        }
    }
}

impl From<Command> for CommandResult {
    fn from(value: Command) -> Self {
        Self::HandledWith(Box::new(value))
    }
}

/// Normalizes by length so equality stays canonical: an empty `Vec` is
/// `Handled`, a single command is `HandledWith`, and only two or more become
/// `HandledWithMany`.
impl From<Vec<Command>> for CommandResult {
    fn from(mut value: Vec<Command>) -> Self {
        match value.len() {
            0 => Self::Handled,
            1 => Self::HandledWith(Box::new(value.remove(0))),
            _ => Self::HandledWithMany(value),
        }
    }
}

impl From<Error> for CommandResult {
    fn from(value: Error) -> Self {
        let command: Command = value.into();
        command.into()
    }
}

impl From<Result<(), Error>> for CommandResult {
    fn from(value: Result<(), Error>) -> Self {
        match value {
            Err(error) => error.into(),
            Ok(()) => CommandResult::Handled,
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    #[test]
    fn from_error_is_alert_error() {
        assert_eq!(
            CommandResult::HandledWith(Box::new(Command::AlertError("oops".to_string()))),
            anyhow!("oops").into()
        );
    }

    #[test]
    fn from_err_result_is_alert_error() {
        assert_eq!(
            CommandResult::HandledWith(Box::new(Command::AlertError("oops".to_string()))),
            Err::<(), _>(anyhow!("oops")).into()
        );
    }

    #[test]
    fn into_commands_covers_every_variant() {
        assert!(CommandResult::Handled.into_commands().is_empty());
        assert!(CommandResult::NotHandled.into_commands().is_empty());
        assert_eq!(
            vec![Command::Quit],
            CommandResult::from(Command::Quit).into_commands()
        );
        assert_eq!(
            vec![Command::Quit, Command::ResetView],
            CommandResult::HandledWithMany(vec![Command::Quit, Command::ResetView]).into_commands()
        );
    }

    #[test]
    fn from_vec_normalizes_by_length() {
        assert_eq!(CommandResult::Handled, Vec::<Command>::new().into());
        assert_eq!(
            CommandResult::HandledWith(Box::new(Command::Quit)),
            vec![Command::Quit].into()
        );
        assert_eq!(
            CommandResult::HandledWithMany(vec![Command::Quit, Command::ResetView]),
            vec![Command::Quit, Command::ResetView].into()
        );
    }

    #[test]
    fn try_from_handled_with_extracts_command() {
        let result = CommandResult::HandledWith(Box::new(Command::Quit));
        assert_eq!(Command::Quit, Command::try_from(result).unwrap());
    }

    #[test]
    fn try_from_handled_is_err() {
        assert!(Command::try_from(CommandResult::Handled).is_err());
    }

    #[test]
    fn try_from_not_handled_is_err() {
        assert!(Command::try_from(CommandResult::NotHandled).is_err());
    }

    #[test]
    fn try_from_handled_with_many_is_err() {
        assert!(
            Command::try_from(CommandResult::HandledWithMany(vec![
                Command::Quit,
                Command::ResetView
            ]))
            .is_err()
        );
    }
}
