use super::{bordered, View};
use crate::{
    app::{focus::Focus, style::error_style},
    command::{handler::CommandHandler, result::CommandResult, Command},
};
use ratatui::{
    backend::Backend,
    layout::Rect,
    widgets::{List, ListItem},
    Frame,
};

#[derive(Default)]
pub(super) struct ErrorsView {
    errors: Vec<String>,
}

impl ErrorsView {
    pub(super) fn height(&self) -> u16 {
        if !self.should_show() {
            return 0;
        }
        self.errors.len() as u16 + 2 // +2 for borders
    }

    fn add_error(&mut self, message: String) -> CommandResult {
        self.errors.push(message);
        CommandResult::none()
    }

    fn clear_errors(&mut self) -> CommandResult {
        self.errors.clear();
        CommandResult::none()
    }

    fn should_show(&self) -> bool {
        !self.errors.is_empty()
    }
}

impl CommandHandler for ErrorsView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::AddError(message) => self.add_error(message.clone()),
            Command::ClearErrors => self.clear_errors(),
            _ => CommandResult::NotHandled,
        }
    }
}

impl<B: Backend> View<B> for ErrorsView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &Focus) {
        if !self.should_show() {
            return;
        }
        let rect = bordered(frame, rect, Some("Errors".into()), Some(error_style()));
        let items: Vec<ListItem> = self
            .errors
            .iter()
            .map(|error| ListItem::new(format!(" • {error}")))
            .rev() // Newest error messages near the top
            .collect();
        let list = List::new(items).style(error_style());
        frame.render_widget(list, rect);
    }
}
