use super::Command;

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
