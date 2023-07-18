use super::Component;
use crate::{
    app::{
        command::{Command, CommandHandler, CommandResult},
        focus::Focus,
    },
    file_system::path_display::PathDisplay,
    views::Renderable,
};
use crossterm::event::KeyCode;
use ratatui::{
    backend::Backend,
    layout::Rect,
    widgets::{Block, List, ListItem},
    Frame,
};

#[derive(Default)]
pub struct Header {
    directory: PathDisplay,
    errors: Vec<String>,
}

impl<B: Backend> Component<B> for Header {}

impl CommandHandler for Header {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Key(code, _) => match code {
                KeyCode::Char('q') | KeyCode::Char('Q') => CommandResult::some(Command::Quit),
                KeyCode::Backspace => CommandResult::some(Command::BackDir),
                _ => CommandResult::NotHandled,
            },
            Command::ClearErrors => {
                self.errors.clear();
                CommandResult::none()
            }
            Command::Error(message) => {
                self.errors.push(message.clone());
                CommandResult::none()
            }
            Command::UpdateCurrentDir(directory, _) => {
                self.directory = directory.clone();
                CommandResult::none()
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn is_focussed(&self, focus: &Focus) -> bool {
        *focus == Focus::Header
    }
}

impl<B: Backend> Renderable<B> for Header {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect) {
        let items: Vec<ListItem> = self
            .errors
            .iter()
            .map(|error| ListItem::new(error.clone()))
            .collect();
        if self.errors.is_empty() {
            let path = self.directory.path.clone();
            let block = Block::default().title(path);
            frame.render_widget(block, rect);
        } else {
            let block = List::new(items).block(Block::default().title("Errors:"));
            frame.render_widget(block, rect);
        }
    }
}
