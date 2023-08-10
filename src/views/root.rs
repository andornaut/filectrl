use super::{
    content::ContentView, header::HeaderView, help::HelpView, prompt::PromptView,
    status::StatusView, View,
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
    content: ContentView,
    header: HeaderView,
    help: HelpView,
    status: StatusView,
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
        let content: &mut dyn CommandHandler = &mut self.content;
        let header: &mut dyn CommandHandler = &mut self.header;
        let help: &mut dyn CommandHandler = &mut self.help;
        let prompt: &mut dyn CommandHandler = &mut self.prompt;
        let status: &mut dyn CommandHandler = &mut self.status;
        vec![header, help, content, status, prompt]
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
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
        ];
        if self.show_help {
            constraints.insert(0, Constraint::Length(4));
        }

        if show_prompt(focus) {
            constraints.push(Constraint::Length(1));
        }

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints);
        let split = layout.split(rect);
        let mut chunks = split.into_iter();

        if self.show_help {
            self.help.render(frame, *chunks.next().unwrap(), focus);
        }
        self.header.render(frame, *chunks.next().unwrap(), focus);
        self.content.render(frame, *chunks.next().unwrap(), focus);
        self.status.render(frame, *chunks.next().unwrap(), focus);
        if show_prompt(focus) {
            self.prompt.render(frame, *chunks.next().unwrap(), focus);
        }
    }
}

fn show_prompt(focus: &Focus) -> bool {
    matches!(focus, Focus::Prompt)
}
