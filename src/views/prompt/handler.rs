use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::PromptView;
use crate::command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command};

impl CommandHandler for PromptView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::SetDirectory(directory, _) => {
                if let Some(previous_directory) = &self.directory {
                    if previous_directory.path != directory.path {
                        self.directory = Some(directory.clone());
                        return Command::SetFilter("".into()).into();
                    }
                }
                CommandResult::none()
            }
            Command::OpenPrompt(kind) => self.open(kind),
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            Command::SetSelected(selected) => self.set_selected(selected.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match *code {
            KeyCode::Esc => Command::ClosePrompt.into(),
            KeyCode::Enter => self.submit(),
            _ => self.handle_key(*code, *modifiers),
        }
    }

    fn should_receive_key(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Prompt)
    }
}
