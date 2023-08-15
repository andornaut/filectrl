use super::Command;
use anyhow::{anyhow, Error, Result};

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

impl TryInto<Command> for CommandResult {
    type Error = Error;

    fn try_into(self) -> Result<Command, Self::Error> {
        match self {
            Self::Handled(option) => match option {
                Some(command) => Ok(command.clone()),
                None => Err(anyhow!(
                    "Cannot convert to Command, because CommandResult::Handled is None"
                )),
            },
            _ => Err(anyhow!(
                "Cannot convert to Command, because CommandResult is not Handled"
            )),
        }
    }
}
