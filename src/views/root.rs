use super::{
    errors::ErrorsView, header::HeaderView, help::HelpView, prompt::PromptView, status::StatusView,
    table::TableView, View,
};
use crate::{
    app::focus::Focus,
    command::{handler::CommandHandler, result::CommandResult, Command},
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
    status: StatusView,
    table: TableView,
    prompt: PromptView,
    show_help: bool,
}

impl RootView {
    fn toggle_show_help(&mut self) -> CommandResult {
        self.show_help = !self.show_help;
        CommandResult::none()
    }
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
        match command {
            Command::ToggleHelp => self.toggle_show_help(),
            _ => CommandResult::NotHandled,
        }
    }
}

impl<B: Backend> View<B> for RootView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, focus: &Focus) {
        let mut constraints = vec![
            Constraint::Length(self.errors.height()),
            Constraint::Length(self.header.height(rect)),
            Constraint::Min(5),
            Constraint::Length(1),
        ];
        let mut handlers: Vec<&mut dyn View<_>> = vec![
            &mut self.errors,
            &mut self.header,
            &mut self.table,
            &mut self.status,
        ];

        if self.show_help {
            constraints.insert(1, Constraint::Length(4));
            handlers.insert(1, &mut self.help);
        }
        if focus.is_prompt() {
            constraints.push(Constraint::Length(1));
            handlers.push(&mut self.prompt)
        }

        Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(rect)
            .into_iter()
            .zip(handlers.into_iter())
            .for_each(|(chunk, handler)| handler.render(frame, *chunk, focus));
    }
}
