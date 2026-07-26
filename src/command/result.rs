use anyhow::Error;

use super::Command;

#[derive(Clone, Debug, PartialEq)]
pub enum CommandResult {
    Handled,
    HandledWith(Box<Command>),
    HandledWithMany(Vec<Command>),
    NotHandled,
}

impl CommandResult {
    /// Prepends `command` to this result's derived commands, normalizing
    /// through the same length rules as `From<Vec<Command>>`. `Handled` and
    /// `NotHandled` carry no derived commands, so both become
    /// `HandledWith(command)`: composing a command into a result means the
    /// triggering command was handled.
    pub fn prepend(self, command: Command) -> Self {
        let mut commands = vec![command];
        match self {
            Self::HandledWith(existing) => commands.push(*existing),
            Self::HandledWithMany(existing) => commands.extend(existing),
            Self::Handled | Self::NotHandled => {}
        }
        commands.into()
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
    fn prepend_to_handled_yields_handled_with() {
        assert_eq!(
            CommandResult::HandledWith(Box::new(Command::Quit)),
            CommandResult::Handled.prepend(Command::Quit)
        );
    }

    #[test]
    fn prepend_to_not_handled_yields_handled_with() {
        assert_eq!(
            CommandResult::HandledWith(Box::new(Command::Quit)),
            CommandResult::NotHandled.prepend(Command::Quit)
        );
    }

    #[test]
    fn prepend_to_handled_with_yields_handled_with_many() {
        assert_eq!(
            CommandResult::HandledWithMany(vec![Command::Quit, Command::ResetView]),
            CommandResult::from(Command::ResetView).prepend(Command::Quit)
        );
    }

    #[test]
    fn prepend_to_handled_with_many_extends_in_order() {
        let existing =
            CommandResult::HandledWithMany(vec![Command::ResetView, Command::ResetHelpScroll]);
        assert_eq!(
            CommandResult::HandledWithMany(vec![
                Command::Quit,
                Command::ResetView,
                Command::ResetHelpScroll
            ]),
            existing.prepend(Command::Quit)
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
