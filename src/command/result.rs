use anyhow::Error;

use super::Command;

#[derive(Clone, Debug)]
pub enum CommandResult {
    Handled,
    HandledWith(Command),
    NotHandled,
}

impl From<Command> for CommandResult {
    fn from(value: Command) -> Self {
        Self::HandledWith(value)
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
