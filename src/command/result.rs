use super::Command;
use anyhow::Error;

#[derive(Clone, Debug)]
pub enum CommandResult {
    Handled(Option<Command>),
    NotHandled,
}

impl CommandResult {
    pub fn none() -> Self {
        Self::Handled(None)
    }

    pub fn some(command: Command) -> Self {
        Self::Handled(Some(command))
    }
}

impl From<Command> for CommandResult {
    fn from(value: Command) -> Self {
        Self::some(value)
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
            Ok(()) => CommandResult::none(),
        }
    }
}
