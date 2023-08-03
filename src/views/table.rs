use super::View;
use crate::{
    app::focus::Focus,
    command::{handler::CommandHandler, result::CommandResult, Command},
    file_system::path::HumanPath,
};
use crossterm::event::KeyCode;
use ratatui::{
    backend::Backend,
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

#[derive(Default)]
pub(super) struct TableView {
    directory_contents: Vec<HumanPath>,
    directory: HumanPath,
    state: TableState,
}

impl TableView {
    pub(super) fn selected(&self) -> Option<&HumanPath> {
        match self.state.selected() {
            Some(i) => Some(&self.directory_contents[i]),
            None => None,
        }
    }

    fn open(&mut self) -> CommandResult {
        match self.selected() {
            Some(path) => {
                let path = path.clone();
                // TODO: handle symlinks
                (if path.is_dir {
                    Command::ChangeDir(path)
                } else {
                    Command::OpenFile(path)
                })
                .into()
            }
            None => CommandResult::none(),
        }
    }

    fn next(&mut self) -> CommandResult {
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
        CommandResult::none()
    }

    fn previous(&mut self) -> CommandResult {
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
        CommandResult::none()
    }

    fn update_current_dir(
        &mut self,
        directory: HumanPath,
        children: Vec<HumanPath>,
    ) -> CommandResult {
        self.directory = directory;
        self.directory_contents = children;
        self.state.select(None);
        CommandResult::none()
    }
}

impl CommandHandler for TableView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Key(code, _) => match code {
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open(),
                KeyCode::Up | KeyCode::Char('k') => self.previous(),
                KeyCode::Down | KeyCode::Char('j') => self.next(),
                _ => CommandResult::NotHandled,
            },
            Command::UpdateCurrentDir(directory, children) => {
                self.update_current_dir(directory.clone(), children.clone())
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn is_focussed(&self, focus: &Focus) -> bool {
        *focus == Focus::Content
    }
}

impl<B: Backend> View<B> for TableView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect) {
        let table = create_table(&self.directory_contents);
        frame.render_stateful_widget(table, rect, &mut self.state);
    }
}

fn create_table(children: &[HumanPath]) -> Table {
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
