use super::View;
use crate::{
    app::{
        focus::Focus,
        style::{table_header_style_default, table_header_style_sorted, table_selected_style},
    },
    command::{
        handler::CommandHandler,
        result::CommandResult,
        sorting::{SortColumn, SortDirection},
        Command, PromptKind,
    },
    file_system::human::HumanPath,
    views::split_utf8_with_reservation,
};
use crossterm::event::KeyCode;
use ratatui::{
    backend::Backend,
    layout::{Constraint, Rect},
    style::Style,
    widgets::{Cell, Row, Table, TableState},
    Frame,
};

const MODE_LEN: u16 = 10;
const MODIFIED_LEN: u16 = 12;
const SIZE_LEN: u16 = 7;
const SEPARATOR: &str = "\n…";

#[derive(Default)]
pub(super) struct TableView {
    directory_items: Vec<HumanPath>,
    directory: HumanPath,
    directory_items_sorted: Vec<HumanPath>,
    filter: String,
    sort_column: SortColumn,
    sort_direction: SortDirection,
    state: TableState,
}

impl TableView {
    fn delete(&self) -> CommandResult {
        match self.selected() {
            Some(path) => Command::DeletePath(path.clone()).into(),
            None => CommandResult::none(),
        }
    }

    fn header_label(&self, column: &SortColumn) -> String {
        let label = match column {
            SortColumn::Name => "[N]ame",
            SortColumn::Modified => "[M]odified",
            SortColumn::Size => "[S]ize",
        };
        if self.sort_column != *column {
            return label.into();
        }
        match self.sort_direction {
            SortDirection::Ascending => format!("{label}⌃"),
            SortDirection::Descending => format!("{label}⌄"),
        }
    }

    fn header_style(&self, column: &SortColumn) -> Style {
        if self.sort_column == *column {
            table_header_style_sorted()
        } else {
            table_header_style_default()
        }
    }

    fn navigate(&mut self, delta: i8) -> CommandResult {
        if self.directory_items_sorted.is_empty() {
            return CommandResult::none();
        }

        let i = match self.state.selected() {
            Some(i) => navigate(self.directory_items_sorted.len(), i, delta),
            None => 0,
        };
        self.state.select(Some(i));
        Command::SetSelected(Some(self.selected().unwrap().clone())).into()
    }

    fn next(&mut self) -> CommandResult {
        self.navigate(1)
    }

    fn previous(&mut self) -> CommandResult {
        self.navigate(-1)
    }

    fn open(&mut self) -> CommandResult {
        match self.selected() {
            Some(path) => {
                let path = path.clone();
                (if path.is_dir() {
                    Command::ChangeDir(path)
                } else {
                    Command::OpenFile(path)
                })
                .into()
            }
            None => CommandResult::none(),
        }
    }

    fn open_filter_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptKind::Filter).into()
    }

    fn open_rename_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptKind::Rename).into()
    }

    fn selected(&self) -> Option<&HumanPath> {
        match self.state.selected() {
            Some(i) => Some(&self.directory_items_sorted[i]),
            None => None,
        }
    }

    fn set_directory(&mut self, directory: HumanPath, children: Vec<HumanPath>) -> CommandResult {
        self.directory = directory;
        self.directory_items = children;
        self.sort()
    }

    fn set_filter(&mut self, filter: String) -> CommandResult {
        self.filter = filter;
        self.sort()
    }

    fn sort(&mut self) -> CommandResult {
        let mut items = self.directory_items.clone();
        match self.sort_column {
            SortColumn::Name => items.sort(), // Sorts by name by default
            SortColumn::Modified => items.sort_by_cached_key(|path| path.modified),
            SortColumn::Size => items.sort_by_cached_key(|path| path.size),
        };
        if self.sort_direction == SortDirection::Descending {
            items.reverse();
        }

        if !self.filter.is_empty() {
            let filter_lowercase = self.filter.to_ascii_lowercase();
            items = items
                .into_iter()
                .filter(|path| path.name().to_ascii_lowercase().contains(&filter_lowercase))
                .collect();
        }
        self.directory_items_sorted = items;
        self.unselect()
    }

    fn sort_by(&mut self, column: SortColumn) -> CommandResult {
        if self.sort_column == column {
            self.sort_direction.toggle();
        } else {
            self.sort_column = column;
        }
        self.sort()
    }

    fn unselect(&mut self) -> CommandResult {
        self.state.select(None);
        Command::SetSelected(None).into()
    }
}

impl CommandHandler for TableView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Key(code, _) => match code {
                KeyCode::Delete => self.delete(),
                KeyCode::Esc => Command::SetFilter("".into()).into(),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('f') | KeyCode::Char('l') => {
                    self.open()
                }
                KeyCode::Down | KeyCode::Char('j') => self.next(),
                KeyCode::Up | KeyCode::Char('k') => self.previous(),
                KeyCode::Char('r') | KeyCode::F(2) => self.open_rename_prompt(),
                KeyCode::Char('/') => self.open_filter_prompt(),
                KeyCode::Char('n') | KeyCode::Char('N') => self.sort_by(SortColumn::Name),
                KeyCode::Char('m') | KeyCode::Char('M') => self.sort_by(SortColumn::Modified),
                KeyCode::Char('s') | KeyCode::Char('S') => self.sort_by(SortColumn::Size),
                KeyCode::Char(' ') => self.unselect(),

                _ => CommandResult::NotHandled,
            },
            Command::SetDirectory(directory, children) => {
                self.set_directory(directory.clone(), children.clone())
            }
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn is_focussed(&self, focus: &Focus) -> bool {
        *focus == Focus::Content
    }
}

impl<B: Backend> View<B> for TableView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &Focus) {
        let mut header_cells: Vec<_> = [SortColumn::Name, SortColumn::Modified, SortColumn::Size]
            .into_iter()
            .map(|header| Cell::from(self.header_label(&header)).style(self.header_style(&header)))
            .collect();
        header_cells.push(Cell::from("Mode").style(table_header_style_default()));
        let header = Row::new(header_cells).style(table_header_style_default());
        let (constraints, name_width) = constraints(rect.width);
        let rows = self.directory_items_sorted.iter().map(|item| {
            let name_lines =
                split_utf8_with_reservation(&item.name(), name_width as usize, SEPARATOR);

            Row::new(vec![
                Cell::from(name_lines.join(SEPARATOR)),
                Cell::from(item.modified()),
                Cell::from(format!("{: >7}", item.size())), // 7 must match SIZE_LEN
                Cell::from(item.mode()),
            ])
            .height(name_lines.len() as u16)
        });
        let table = Table::new(rows)
            .header(header)
            .highlight_style(table_selected_style())
            .widths(&constraints);
        frame.render_stateful_widget(table, rect, &mut self.state);
    }
}

fn constraints(width: u16) -> (Vec<Constraint>, u16) {
    let mut constraints = Vec::new();
    let mut name_width = width;
    if width > 39 {
        name_width = width - MODIFIED_LEN - 1; // 1 for the cell padding
        constraints.push(Constraint::Length(MODIFIED_LEN));
    }
    if width > 39 + MODIFIED_LEN + 1 + SIZE_LEN + 1 {
        name_width -= SIZE_LEN + 1;
        constraints.push(Constraint::Length(SIZE_LEN));
    }
    if width > 39 + MODIFIED_LEN + 1 + SIZE_LEN + 1 + MODE_LEN + 1 {
        name_width -= MODE_LEN + 1;
        constraints.push(Constraint::Length(MODE_LEN));
    }
    constraints.insert(0, Constraint::Length(name_width));
    (constraints, name_width)
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
    fn navigate_is_correct(expected: usize, len: usize, index: usize, delta: i8) {
        let result = navigate(len, index, delta);

        assert_eq!(expected, result);
    }
}
