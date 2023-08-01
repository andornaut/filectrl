use super::{errors::ErrorsView, View};
use crate::{
    app::{
        command::{Command, CommandHandler, CommandResult},
        focus::Focus,
    },
    file_system::path_display::PathDisplay,
    views::Renderable,
};
use crossterm::event::KeyCode;
use ratatui::{
    backend::Backend,
    layout::{Constraint, Rect},
    prelude::{Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

#[derive(Default)]
pub struct Content {
    errors: ErrorsView,
    directory_contents: Vec<PathDisplay>,
    directory: PathDisplay,
    state: TableState,
}

impl Content {
    fn open(&mut self) -> CommandResult {
        if let Some(i) = self.state.selected() {
            let path = &self.directory_contents[i];
            let result = PathDisplay::try_from(path.path.clone());
            return CommandResult::some(match result {
                Err(err) => Command::Error(err.to_string()),
                Ok(path) => {
                    if path.is_dir {
                        Command::ChangeDir(path)
                    } else {
                        // TODO: handle symlinks
                        Command::OpenFile(path)
                    }
                }
            });
        }
        CommandResult::none()
    }

    fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.directory_contents.len() - 1 {
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
                    self.directory_contents.len() - 1
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
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        let errors: &mut dyn CommandHandler = &mut self.errors;
        vec![errors]
    }

    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Key(code, _) => match code {
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open(),
                KeyCode::Up | KeyCode::Char('k') => {
                    self.previous();
                    CommandResult::none()
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.next();
                    CommandResult::none()
                }
                _ => CommandResult::NotHandled,
            },
            Command::UpdateCurrentDir(directory, children) => {
                self.directory = directory.clone();
                self.directory_contents = children.clone();
                CommandResult::none()
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn is_focussed(&self, focus: &Focus) -> bool {
        *focus == Focus::Content
    }
}

impl<B: Backend> View<B> for Content {}

impl<B: Backend> Renderable<B> for Content {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(self.errors.height()), Constraint::Min(0)].as_ref());
        let chunks = layout.split(rect);
        let errors_rect = chunks[0];
        let content_rect = chunks[1];
        self.errors.render(frame, errors_rect);
        frame.render_stateful_widget(
            create_table(&self.directory_contents),
            content_rect,
            &mut self.state,
        );
    }
}

fn create_table(children: &[PathDisplay]) -> Table {
    let selected_style = Style::default().add_modifier(Modifier::REVERSED);
    let normal_style = Style::default().bg(Color::Blue);
    let header_cells = ["Name", "Mode", "Size", "Modified"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Red)));
    let header = Row::new(header_cells)
        .style(normal_style)
        .height(1)
        .bottom_margin(1);
    let rows = children.iter().map(|item| {
        let height = 1;
        let cells = vec![
            Cell::from(item.basename.clone()),
            Cell::from(item.mode.to_string()),
            Cell::from(item.human_size()),
            Cell::from(item.human_modified()),
        ];
        Row::new(cells).height(height as u16).bottom_margin(1)
    });
    Table::new(rows)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Table"))
        .highlight_style(selected_style)
        .widths(&[
            Constraint::Percentage(55),
            Constraint::Length(5),
            Constraint::Length(10),
            Constraint::Min(35),
        ])
}
