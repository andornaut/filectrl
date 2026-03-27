use anyhow::Error;

use super::Command;

#[derive(Clone, Debug, PartialEq)]
pub enum CommandResult {
    Handled,
    HandledWith(Box<Command>),
    NotHandled,
}

impl From<Command> for CommandResult {
    fn from(value: Command) -> Self {
        Self::HandledWith(Box::new(value))
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
}
