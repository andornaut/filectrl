use super::View;
use crate::{
    command::{handler::CommandHandler, result::CommandResult, Command},
    views::Renderable,
};
use ratatui::{
    backend::Backend,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, List, ListItem},
    Frame,
};

#[derive(Default)]
pub(super) struct ErrorsView {
    errors: Vec<String>,
}

impl ErrorsView {
    pub fn height(&self) -> u16 {
        if !self.should_render() {
            return 0;
        }
        u16::try_from(self.errors.len() + 1).expect("The numbe of errors + 1 fits within u16")
    }

    fn should_render(&self) -> bool {
        self.errors.len() > 0
    }
}

impl<B: Backend> View<B> for ErrorsView {}

impl CommandHandler for ErrorsView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::ClearErrors => {
                self.errors.clear();
                CommandResult::none()
            }
            Command::Error(message) => {
                self.errors.push(message.clone());
                CommandResult::none()
            }
            _ => CommandResult::NotHandled,
        }
    }
}

impl<B: Backend> Renderable<B> for ErrorsView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect) {
        if !self.should_render() {
            return;
        }
        let list = create_list(&self.errors);
        frame.render_widget(list, rect);
    }
}

fn create_list(messages: &[String]) -> List {
    let style = Style::default().fg(Color::Red);
    let items: Vec<ListItem> = messages
        .iter()
        .map(|error| ListItem::new(error.clone()))
        .collect();
    List::new(items)
        .style(style)
        .block(Block::default().title("Errors:"))
}
