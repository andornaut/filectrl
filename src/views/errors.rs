use super::View;
use crate::{
    app::{focus::Focus, style::error_style},
    command::{handler::CommandHandler, result::CommandResult, Command},
};
use ratatui::{
    backend::Backend,
    layout::Rect,
    widgets::{Block, List, ListItem},
    Frame,
};

#[derive(Default)]
pub(super) struct ErrorsView {
    errors: Vec<String>,
}

impl ErrorsView {
    pub(super) fn height(&self) -> u16 {
        if !self.should_render() {
            return 0;
        }
        u16::try_from(self.errors.len() + 1).expect("The number of errors + 1 fits within u16")
    }

    fn add_error(&mut self, message: String) -> CommandResult {
        self.errors.push(message);
        CommandResult::none()
    }

    fn clear_errors(&mut self) -> CommandResult {
        self.errors.clear();
        CommandResult::none()
    }

    fn should_render(&self) -> bool {
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
        if !self.should_render() {
            return;
        }
        let items: Vec<ListItem> = self
            .errors
            .iter()
            .map(|error| ListItem::new(format!("• {error}")))
            .rev()
            .collect();
        let list = List::new(items)
            .style(error_style())
            .block(Block::default().title("Errors:"));
        frame.render_widget(list, rect);
    }
}
