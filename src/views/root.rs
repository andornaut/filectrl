use super::{
    errors::ErrorsView, header::HeaderView, help::HelpView, prompt::PromptView, status::StatusView,
    table::TableView, View,
};
use crate::{
    app::theme::Theme,
    command::{handler::CommandHandler, result::CommandResult, Command, Focus},
};
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

    fn handle_command(&mut self, command: &Command) -> CommandResult {
        // Consolidate all Focus changes here, so that other CommandHandler's
        // don't have to choose between returning a Focus or other derived
        // command.
        match command {
            Command::ClosePrompt => Command::SetFocus(Focus::Table).into(),
            Command::OpenPrompt(_) => Command::SetFocus(Focus::Prompt).into(),
            Command::RenamePath(_, _) => Command::SetFocus(Focus::Table).into(),
            Command::SetFilter(_) => Command::SetFocus(Focus::Table).into(),
            _ => CommandResult::NotHandled,
        }
    }
}

impl<B: Backend> View<B> for RootView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, focus: &Focus, theme: &Theme) {
        let constraints = vec![
            Constraint::Length(self.errors.height()),
            Constraint::Length(self.help.height()),
            Constraint::Length(self.header.height(rect)),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(self.prompt.height(focus)),
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
            .split(rect)
            .into_iter()
            .zip(handlers.into_iter())
            .for_each(|(chunk, handler)| handler.render(frame, *chunk, focus, theme));
    }
}
