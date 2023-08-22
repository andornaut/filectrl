use super::{
    errors::ErrorsView, header::HeaderView, help::HelpView, prompt::PromptView, status::StatusView,
    table::TableView, View,
};
use crate::{
    app::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult},
};
use crossterm::event::MouseEvent;
use ratatui::{
    backend::Backend,
    layout::{Constraint, Direction, Layout, Rect},
    Frame,
};

#[derive(Default)]
pub struct RootView {
    errors: ErrorsView,
    header: HeaderView,
    help: HelpView,
    prompt: PromptView,
    last_rendered_rect: Rect,
    status: StatusView,
    table: TableView,
}

impl CommandHandler for RootView {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        let errors: &mut dyn CommandHandler = &mut self.errors;
        let header: &mut dyn CommandHandler = &mut self.header;
        let help: &mut dyn CommandHandler = &mut self.help;
        let prompt: &mut dyn CommandHandler = &mut self.prompt;
        let status: &mut dyn CommandHandler = &mut self.status;
        let table: &mut dyn CommandHandler = &mut self.table;
        vec![errors, header, help, prompt, status, table]
    }

    fn handle_mouse(&mut self, _mouse: &MouseEvent) -> CommandResult {
        // TODO
        CommandResult::none()
    }

    fn should_receive_mouse(&self, _column: u16, _row: u16) -> bool {
        // TODO
        true
    }
}

impl<B: Backend> View<B> for RootView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, mode: &InputMode, theme: &Theme) {
        self.last_rendered_rect = rect;

        let constraints = vec![
            Constraint::Length(self.errors.height()),
            Constraint::Length(self.help.height()),
            Constraint::Length(self.header.height(self.last_rendered_rect)),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(self.prompt.height(mode)),
        ];
        let handlers: Vec<&mut dyn View<_>> = vec![
            &mut self.errors,
            &mut self.help,
            &mut self.header,
            &mut self.table,
            &mut self.status,
            &mut self.prompt,
        ];
        Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(self.last_rendered_rect)
            .into_iter()
            .zip(handlers.into_iter())
            .for_each(|(chunk, handler)| handler.render(frame, *chunk, mode, theme));
    }
}
