use super::View;
use crate::{
    app::focus::Focus,
    command::{handler::CommandHandler, result::CommandResult, Command},
    file_system::path::HumanPath,
    views::Renderable,
};
use ratatui::{backend::Backend, layout::Rect, widgets::Block, Frame};

#[derive(Default)]
pub(super) struct HeaderView {
    directory: HumanPath,
}

impl<B: Backend> View<B> for HeaderView {}

impl CommandHandler for HeaderView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
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

impl<B: Backend> Renderable<B> for HeaderView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect) {
        let path = self.directory.path.clone();
        let block = Block::default().title(path);
        frame.render_widget(block, rect);
    }
}
