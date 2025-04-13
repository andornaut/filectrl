// use log::debug;
use ratatui::crossterm::event::{
        KeyCode,
        KeyEvent,
        KeyModifiers,
        MouseEvent,
        // MouseButton, MouseEventKind,
    };
// use tui_textarea::Input;

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
            KeyCode::Enter => self.submit(),
            KeyCode::Esc => Command::ClosePrompt.into(),
            _ => {
                self.input.input(KeyEvent::new(*code, *modifiers));
                CommandResult::none()
            }
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        if self.input.input(*event) {
            CommandResult::none()
        } else {
            CommandResult::NotHandled
        }
    }

    fn should_receive_key(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Prompt)
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        x >= self.input_widget_area.x
            && x < self.input_widget_area.x + self.input_widget_area.width
            && y >= self.input_widget_area.y
            && y < self.input_widget_area.y + self.input_widget_area.height
    }
}
