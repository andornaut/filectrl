use super::Component;
use crate::{
    app::command::{Command, CommandHandler},
    file_system::Path,
    view::Renderable,
};
use ratatui::{
    backend::Backend,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem},
    Frame,
};
use std::env;

pub struct Content {
    directory: Path,
    children: Vec<Path>,
}

impl Content {
    pub fn new() -> Self {
        // TODO Ideally, this wouldn't touch `env` or know about `PathBuf`
        let directory = env::current_dir().unwrap();
        let directory = Path::try_from(directory).unwrap();
        Self {
            directory,
            children: vec![],
        }
    }
}

impl CommandHandler for Content {
    fn handle_command(&mut self, command: &Command) -> Option<Command> {
        if let Command::UpdateCurrentDir(directory, children) = command {
            self.directory = directory.clone();
            self.children = children.clone();
        }
        None
    }
}

impl<B: Backend> Component<B> for Content {}

impl<B: Backend> Renderable<B> for Content {
    fn render(&self, frame: &mut Frame<B>, rect: Rect) {
        //sort(&mut items);
        let items: Vec<_> = self
            .children
            .iter()
            .map(|item| ListItem::new(item.basename.as_str()))
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title("Directory list")
                    .borders(Borders::ALL),
            )
            .style(Style::default().fg(Color::Cyan))
            .highlight_style(Style::default().add_modifier(Modifier::ITALIC))
            .highlight_symbol(">>");

        frame.render_widget(list, rect);
    }
}
