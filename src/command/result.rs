use anyhow::Error;

use super::Command;

/// The outcome of a `CommandHandler` method. Build the derived-command variants
/// through `From`, which normalizes by length so equality stays canonical.
#[derive(Clone, Debug, PartialEq)]
pub enum CommandResult {
    Handled,
    HandledWith(Box<Command>),
    HandledWithMany(Vec<Command>),
    NotHandled,
}

impl CommandResult {
    /// The derived commands, dropping the handled/not-handled distinction.
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
    fn a_context_chain_is_flattened_onto_the_one_line_an_alert_has() {
        use anyhow::Context;

        let error = Err::<(), _>(anyhow!("permission denied"))
            .context("Failed to copy /a/b")
            .unwrap_err();

        assert_eq!(
            CommandResult::HandledWith(Box::new(Command::AlertError(
                "Failed to copy /a/b: permission denied".to_string()
            ))),
            error.into()
        );
    }

    #[test]
    fn an_err_result_becomes_an_error_alert() {
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
}
