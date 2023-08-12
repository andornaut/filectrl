use super::Command;
use anyhow::anyhow;
use anyhow::Result;

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

    pub fn try_into_command(&self) -> Result<Command> {
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

impl From<Command> for CommandResult {
    fn from(value: Command) -> Self {
        Self::some(value)
    }
}
