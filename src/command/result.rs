use super::Command;

#[derive(Clone, Debug)]
pub enum CommandResult {
    Handled(Option<Command>),
    NotHandled,
}

impl CommandResult {
    pub fn option(optional_command: Option<Command>) -> Self {
        if let Some(derived_command) = optional_command {
            CommandResult::some(derived_command)
        } else {
            CommandResult::none()
        }
    }

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
