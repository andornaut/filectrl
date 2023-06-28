use super::Component;
use crate::{
    app::command::{Command, CommandHandler},
    file_system::Path,
    view::Renderable,
};
use ratatui::{backend::Backend, layout::Rect, widgets::Block, Frame};
use std::env;

pub struct Header {
    directory: Path,
}

impl Header {
    pub fn new() -> Self {
        // TODO Ideally, this wouldn't touch `env` or know about `PathBuf`
        let directory = env::current_dir().unwrap();
        let directory = Path::try_from(directory).unwrap();
        Self { directory }
    }
}
impl<B: Backend> Component<B> for Header {}

impl CommandHandler for Header {
    fn handle_command(&mut self, command: &Command) -> Option<Command> {
        if let Command::UpdateCurrentDir(directory, _) = command {
            self.directory = directory.clone();
        }
        None
    }
}

impl<B: Backend> Renderable<B> for Header {
    fn render(&self, frame: &mut Frame<B>, rect: Rect) {
        let block = Block::default().title(self.directory.path.clone());
        frame.render_widget(block, rect);
    }
}
