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

    fn delete(&self) -> CommandResult {
        match self.selected() {
            Some(path) => Command::DeletePath(path.clone()).into(),
            None => CommandResult::none(),
        }
    }

    fn next(&mut self) -> CommandResult {
        self.navigate(1)
    }

    fn previous(&mut self) -> CommandResult {
        self.navigate(-1)
    }

    fn navigate(&mut self, delta: i8) -> CommandResult {
        let i = match self.state.selected() {
            Some(i) => navigate(self.directory_contents.len() - 1, i, delta),
            None => 0,
        };
        self.state.select(Some(i));
        CommandResult::none()
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

    fn update_current_dir(
        &mut self,
        directory: HumanPath,
        children: Vec<HumanPath>,
    ) -> CommandResult {
        self.directory = directory;
        self.directory_contents = children;
        self.unselect_all();
        CommandResult::none()
    }

    fn unselect_all(&mut self) {
        self.state.select(None);
    }
}

impl CommandHandler for TableView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Key(code, _) => match code {
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open(),
                KeyCode::Up | KeyCode::Char('k') => self.previous(),
                KeyCode::Down | KeyCode::Char('j') => self.next(),
                KeyCode::Char('r') => self.delete(),
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

fn navigate(len: usize, index: usize, delta: i8) -> usize {
    let len = i32::try_from(len).expect("Directory list length fits into an i32");
    let index = i32::try_from(index).unwrap();
    let delta = i32::from(delta);
    let mut result = (index + delta) % len;
    if result < 0 {
        result += len;
    }
    usize::try_from(result).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(1,  4, 0, 1 ; "add 1")]
    #[test_case(2,  4, 0, 2 ; "add 2")]
    #[test_case(0,  4, 3, 1 ; "add 1 overflow")]
    #[test_case(1,  4, 3, 2 ; "add 2 overflow")]
    #[test_case(2,  4, 3, -1 ; "subtract 1")]
    #[test_case(1,  4, 3, -2 ; "subtract 2")]
    #[test_case(3,  4, 0, -1 ; "subtract 1 overflow")]
    #[test_case(2,  4, 0, -2 ; "subtract 2 overflow")]
    #[test_case(0,  4, 2, 10 ; "add 10 overflow")]
    #[test_case(1,  4, 2, 11 ; "add 11 overflow")]
    #[test_case(0,  4, 2, -10 ; "subtract 10 overflow")]
    #[test_case(3,  4, 2, -11 ; "subtract 11 overflow")]
    fn test(expected: usize, len: usize, index: usize, delta: i8) {
        let result = navigate(len, index, delta);

        assert_eq!(expected, result);
    }
}
