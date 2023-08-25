mod navigate;
mod render;
mod sort;
mod style;

use self::{
    navigate::navigate,
    render::{header, row, scrollbar},
    sort::{SortColumn, SortDirection},
};
use super::View;
use crate::{
    app::theme::Theme,
    command::{
        handler::CommandHandler, mode::InputMode, result::CommandResult, Command, PromptKind,
    },
    file_system::human::HumanPath,
};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    backend::Backend,
    layout::Rect,
    prelude::{Constraint, Direction, Layout},
    style::Stylize,
    symbols::scrollbar::VERTICAL,
    widgets::{Block, Scrollbar, ScrollbarOrientation, ScrollbarState, Table, TableState},
    Frame,
};

const NAME_MIN_LEN: u16 = 39;
const MODE_LEN: u16 = 10;
const MODIFIED_LEN: u16 = 12;
const SIZE_LEN: u16 = 7;

#[derive(Default)]
pub(super) struct TableView {
    directory_items: Vec<HumanPath>,
    directory: HumanPath,
    directory_items_sorted: Vec<HumanPath>,
    filter: String,
    name_column_width: u16,
    sort_column: SortColumn,
    sort_direction: SortDirection,

    scrollbar_rect: Rect,
    scrollbar_state: ScrollbarState,

    table_visual_rows: Vec<usize>,
    table_rect: Rect,
    table_state: TableState,
}

impl TableView {
    fn delete(&self) -> CommandResult {
        match self.selected() {
            Some(path) => Command::DeletePath(path.clone()).into(),
            None => CommandResult::none(),
        }
    }

    fn handle_click_header(&mut self, x: u16) -> CommandResult {
        if let Some(column) = click_column(x, self.name_column_width) {
            self.sort_by(column)
        } else {
            CommandResult::none()
        }
    }

    fn handle_click_table(&mut self, y: u16) -> CommandResult {
        let y = y + self.table_state.offset() as u16 - 1; // 1 for header. Cannot overflow because header clicks are handled separately.
        if y >= self.table_visual_rows.len() as u16 {
            // Clicked past the table
            return CommandResult::none();
        }

        let i = self.table_visual_rows[y as usize];
        eprintln!("Table.handle_click_table() y:{y} i:{i}");
        self.table_state.select(Some(i));
        Command::SetSelected(Some(self.selected().unwrap().clone())).into()
    }

    fn navigate(&mut self, delta: i8) -> CommandResult {
        if self.directory_items_sorted.is_empty() {
            return CommandResult::none();
        }
        let len = self.directory_items_sorted.len();
        let i = self
            .table_state
            .selected()
            .map_or(0, |i| navigate(len, i, delta));
        self.table_state.select(Some(i));
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
            Some(path) => Command::Open(path.clone()).into(),
            None => CommandResult::none(),
        }
    }

    fn selected(&self) -> Option<&HumanPath> {
        self.table_state
            .selected()
            .map(|i| &self.directory_items_sorted[i])
    }

    fn set_directory(&mut self, directory: HumanPath, children: Vec<HumanPath>) -> CommandResult {
        self.directory = directory;
        self.directory_items = children;
        self.sort()
    }

    fn set_filter(&mut self, filter: String) -> CommandResult {
        // Avoid performing an extra SetFilter(None)
        // set_directory() -> sort() -> SetFilter(None) -> set_filter() -> sort() -> SetFilter(None)
        if self.filter.is_empty() && filter.is_empty() {
            return CommandResult::none();
        }
        self.filter = filter;
        self.sort()
    }

    fn sort(&mut self) -> CommandResult {
        let mut items = self.directory_items.clone();
        match self.sort_column {
            SortColumn::Name => items.sort_by_cached_key(|path| path.name_comparator()),
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
        self.table_state.select(None);
        Command::SetSelected(None).into()
    }
}

impl CommandHandler for TableView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::SetDirectory(directory, children) => {
                self.set_directory(directory.clone(), children.clone())
            }
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.set_filter("".into()).into()
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
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let x = event.column.saturating_sub(self.table_rect.x);
                let y = event.row.saturating_sub(self.table_rect.y);
                if y == 0 {
                    self.handle_click_header(x)
                } else {
                    self.handle_click_table(y)
                }
            }
            MouseEventKind::ScrollUp => self.previous(),
            MouseEventKind::ScrollDown => self.next(),
            _ => CommandResult::none(),
        }
    }

    fn should_receive_mouse(&self, column: u16, row: u16) -> bool {
        let point = Rect::new(column, row, 1, 1);
        self.table_rect.intersects(point)
    }
}

impl<B: Backend> View<B> for TableView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &InputMode, theme: &Theme) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(1)].as_ref())
            .split(rect);
        self.table_rect = chunks[0];
        self.scrollbar_rect = chunks[1];

        // Extend the table header above the scrollbar as a 1x1 block
        let block = Block::default().style(theme.table_header());
        frame.render_widget(
            block,
            Rect {
                height: 1,
                ..self.scrollbar_rect
            },
        );

        // Make room for the above
        self.scrollbar_rect.y += 1;
        self.scrollbar_rect.height -= 1;

        let (column_constraints, name_column_width) = column_constraints(self.table_rect.width);
        self.name_column_width = name_column_width;

        self.table_visual_rows.clear();
        let rows = self
            .directory_items_sorted
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let (row, height) = row(item, name_column_width, theme);
                for _ in 0..height {
                    self.table_visual_rows.push(i)
                }
                row
            });

        let header = header(theme, &self.sort_column, &self.sort_direction);
        let table = Table::new(rows)
            .header(header)
            .highlight_style(theme.table_selected())
            .widths(&column_constraints);
        frame.render_stateful_widget(table, self.table_rect, &mut self.table_state);

        let content_length = self.table_visual_rows.len() as u16;
        if content_length > self.scrollbar_rect.height {
            self.scrollbar_state = self.scrollbar_state.content_length(content_length);
            self.scrollbar_state = self
                .scrollbar_state
                .position(self.table_state.selected().unwrap_or_default() as u16);
            frame.render_stateful_widget(
                scrollbar(theme),
                self.scrollbar_rect,
                &mut self.scrollbar_state,
            );
        }
    }
}

fn click_column(x: u16, name_column_width: u16) -> Option<SortColumn> {
    if x <= name_column_width {
        Some(SortColumn::Name)
    } else if x <= name_column_width + MODIFIED_LEN {
        Some(SortColumn::Modified)
    } else if x <= name_column_width + MODIFIED_LEN + SIZE_LEN {
        Some(SortColumn::Size)
    } else {
        None
    }
}

fn column_constraints(width: u16) -> (Vec<Constraint>, u16) {
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
