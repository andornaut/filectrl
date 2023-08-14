use super::View;
use crate::{
    app::theme::Theme,
    command::{
        handler::CommandHandler,
        result::CommandResult,
        sorting::{SortColumn, SortDirection},
        Command, Focus, PromptKind,
    },
    file_system::human::HumanPath,
    views::split_utf8_with_reservation,
};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    backend::Backend,
    layout::{Constraint, Rect},
    style::Style,
    text::{Line, Span, Text},
    widgets::{Cell, Row, Table, TableState},
    Frame,
};

const NAME_MIN_LEN: u16 = 39;
const MODE_LEN: u16 = 10;
const MODIFIED_LEN: u16 = 12;
const SIZE_LEN: u16 = 7;
const LINE_SEPARATOR: &str = "\n…";

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

    fn open_filter_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptKind::Filter).into()
    }

    fn open_rename_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptKind::Rename).into()
    }

    fn open_selected(&mut self) -> CommandResult {
        match self.selected() {
            Some(path) => {
                let path = path.clone();
                (if path.is_directory() {
                    Command::ChangeDir(path)
                } else {
                    Command::OpenFile(path)
                })
                .into()
            }
            None => CommandResult::none(),
        }
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
            Command::Key(code, modifiers) => match (*code, *modifiers) {
                (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    Command::SetFilter("".into()).into()
                }
                (_, _) => match code {
                    KeyCode::Delete => self.delete(),
                    KeyCode::Enter | KeyCode::Right | KeyCode::Char('f') | KeyCode::Char('l') => {
                        self.open_selected()
                    }
                    KeyCode::Down | KeyCode::Char('j') => self.next(),
                    KeyCode::Up | KeyCode::Char('k') => self.previous(),
                    KeyCode::Char('/') => self.open_filter_prompt(),
                    KeyCode::Char('r') | KeyCode::F(2) => self.open_rename_prompt(),
                    KeyCode::Char('n') | KeyCode::Char('N') => self.sort_by(SortColumn::Name),
                    KeyCode::Char('m') | KeyCode::Char('M') => self.sort_by(SortColumn::Modified),
                    KeyCode::Char('s') | KeyCode::Char('S') => self.sort_by(SortColumn::Size),
                    KeyCode::Char(' ') => self.unselect(),
                    _ => CommandResult::NotHandled,
                },
            },
            Command::SetDirectory(directory, children) => {
                self.set_directory(directory.clone(), children.clone())
            }
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn is_focussed(&self, focus: &Focus) -> bool {
        *focus == Focus::Table
    }
}

impl<B: Backend> View<B> for TableView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &Focus, theme: &Theme) {
        let (constraints, name_width) = constraints(rect.width);
        let header = header(theme, &self.sort_column, &self.sort_direction);
        let rows = self
            .directory_items_sorted
            .iter()
            .map(|item| row(item, name_width, theme));
        let table = Table::new(rows)
            .header(header)
            .highlight_style(theme.table_selected())
            .widths(&constraints);
        frame.render_stateful_widget(table, rect, &mut self.state);
    }
}

fn constraints(width: u16) -> (Vec<Constraint>, u16) {
    let mut constraints = Vec::new();
    let mut name_column_width = width;
    let mut len = NAME_MIN_LEN;
    if width > len {
        name_column_width = width - MODIFIED_LEN - 1; // 1 for the cell padding
        constraints.push(Constraint::Length(MODIFIED_LEN));
    }
    len += MODIFIED_LEN + 1 + SIZE_LEN + 1;
    if width > len {
        name_column_width -= SIZE_LEN + 1;
        constraints.push(Constraint::Length(SIZE_LEN));
    }
    len += MODE_LEN + 1;
    if width > len {
        name_column_width -= MODE_LEN + 1;
        constraints.push(Constraint::Length(MODE_LEN));
    }
    constraints.insert(0, Constraint::Length(name_column_width));
    (constraints, name_column_width)
}

fn header_label(
    sort_column: &SortColumn,
    sort_direction: &SortDirection,
    column: &SortColumn,
) -> String {
    let label = match column {
        SortColumn::Name => "[N]ame",
        SortColumn::Modified => "[M]odified",
        SortColumn::Size => "[S]ize",
    };
    if sort_column != column {
        return label.into();
    }
    match sort_direction {
        SortDirection::Ascending => format!("{label}⌃"),
        SortDirection::Descending => format!("{label}⌄"),
    }
}

fn header_style(theme: &Theme, sort_column: &SortColumn, column: &SortColumn) -> Style {
    if sort_column == column {
        theme.table_header_active()
    } else {
        theme.table_header()
    }
}

fn header<'a>(
    theme: &Theme,
    sort_column: &'a SortColumn,
    sort_direction: &'a SortDirection,
) -> Row<'a> {
    let mut cells: Vec<_> = [SortColumn::Name, SortColumn::Modified, SortColumn::Size]
        .into_iter()
        .map(|header| {
            Cell::from(header_label(sort_column, sort_direction, &header)).style(header_style(
                theme,
                sort_column,
                &header,
            ))
        })
        .collect();
    cells.push(Cell::from("Mode").style(theme.table_header())); // Mode cannot be sorted/active
    Row::new(cells).style(theme.table_header())
}

fn row<'a>(item: &'a HumanPath, name_column_width: u16, theme: &Theme) -> Row<'a> {
    let lines = split_name(&item, name_column_width, theme);
    let len = lines.len();

    // 7 must match SIZE_LEN
    let size = format!("{: >7}", item.size());
    Row::new(vec![
        Cell::from(Text::from(lines)),
        Cell::from(item.modified()),
        Cell::from(size),
        Cell::from(item.mode()),
    ])
    .height(len as u16)
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

fn split_name<'a>(path: &HumanPath, width: u16, theme: &Theme) -> Vec<Line<'a>> {
    let line = path.name();
    let split = split_utf8_with_reservation(&line, width, LINE_SEPARATOR);
    let mut lines = Vec::new();
    let mut it = split.into_iter().peekable();
    while let Some(part) = it.next() {
        let is_last = it.peek().is_none();
        let part = if is_last { part.clone() } else { part + "…" };
        lines.push(Line::from(Span::styled(part, name_style(path, theme))));
    }
    lines
}

fn name_style(path: &HumanPath, theme: &Theme) -> Style {
    if path.is_block_device() {
        return theme.table_name_block_device();
    }
    if path.is_character_device() {
        return theme.table_name_character_device();
    }
    if path.is_directory() {
        return theme.table_name_directory();
    }
    if path.is_fifo() {
        return theme.table_name_fifo();
    }
    if path.is_setgid() {
        return theme.table_name_setgid();
    }
    if path.is_setuid() {
        return theme.table_name_setuid();
    }
    if path.is_socket() {
        return theme.table_name_socket();
    }
    if path.is_sticky() {
        return theme.table_name_sticky();
    }
    if path.is_symlink() {
        return theme.table_name_symlink();
    }
    // catch-all
    return theme.table_name_file();
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
