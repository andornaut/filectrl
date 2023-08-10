use super::{bordered, errors::ErrorsView, table::TableView, View};
use crate::{
    app::focus::Focus,
    command::{handler::CommandHandler, result::CommandResult, Command},
    file_system::human::HumanPath,
    views::prompt::PromptView,
};
use crossterm::event::KeyCode;
use ratatui::{
    backend::Backend,
    layout::{Constraint, Rect},
    prelude::{Direction, Layout},
    Frame,
};

#[derive(Default)]
pub(super) struct ContentView {
    errors: ErrorsView,
    table: TableView,
}

impl ContentView {}

impl CommandHandler for ContentView {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        let errors: &mut dyn CommandHandler = &mut self.errors;
        let table: &mut dyn CommandHandler = &mut self.table;
        vec![errors, table]
    }

    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Key(code, _) => match code {
                _ => CommandResult::NotHandled,
            },
            _ => CommandResult::NotHandled,
        }
    }

    fn is_focussed(&self, focus: &Focus) -> bool {
        *focus == Focus::Content
    }
}

impl<B: Backend> View<B> for ContentView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, focus: &Focus) {
        let rect = bordered(frame, rect, None);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(self.errors.height()), Constraint::Min(0)].as_ref());
        let split = &layout.split(rect);
        let mut chunks = split.into_iter();
        self.errors.render(frame, *chunks.next().unwrap(), focus);
        self.table.render(frame, *chunks.next().unwrap(), focus);
    }
}
