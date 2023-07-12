use super::Component;
use crate::{
    app::command::{Command, CommandHandler},
    file_system::path_display::PathDisplay,
    views::Renderable,
};
use ratatui::{
    backend::Backend,
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

pub struct Content {
    children: Vec<PathDisplay>,
    directory: PathDisplay,
    state: TableState,
}

impl Content {
    pub fn new() -> Self {
        let directory = PathDisplay::try_from("/").unwrap();
        Self {
            children: vec![],
            directory,
            state: TableState::default(),
        }
    }

    fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.children.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.children.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
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
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect) {
        let selected_style = Style::default().add_modifier(Modifier::REVERSED);
        let normal_style = Style::default().bg(Color::Blue);
        let header_cells = ["Name", "Mode", "Size", "Modified"]
            .iter()
            .map(|h| Cell::from(*h).style(Style::default().fg(Color::Red)));
        let header = Row::new(header_cells)
            .style(normal_style)
            .height(1)
            .bottom_margin(1);
        let rows = self.children.iter().map(|item| {
            let height = 1;
            let cells = vec![
                Cell::from(item.basename.clone()),
                Cell::from(item.mode.to_string()),
                Cell::from(item.human_size()),
                Cell::from(item.human_modified()),
            ];
            Row::new(cells).height(height as u16).bottom_margin(1)
        });
        let t = Table::new(rows)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title("Table"))
            .highlight_style(selected_style)
            .highlight_symbol(">> ")
            .widths(&[
                Constraint::Percentage(55),
                Constraint::Length(5),
                Constraint::Length(10),
                Constraint::Min(35),
            ]);
        frame.render_stateful_widget(t, rect, &mut self.state);
    }
}
