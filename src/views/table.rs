mod clipboard;
mod command_handler;
mod navigate;
mod sort;
mod style;
mod view;

use self::{
    clipboard::Clipboard,
    navigate::navigate,
    sort::{SortColumn, SortDirection},
};
use crate::{
    command::{result::CommandResult, Command, PromptKind},
    file_system::human::HumanPath,
};
use ratatui::{
    layout::Rect,
    prelude::Constraint,
    widgets::{ScrollbarState, TableState},
};
use std::time::Instant;

const DOUBLE_CLICK_MS: u128 = 500;
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

    double_click_start: Option<Instant>,

    clipboard: Clipboard,
}

impl TableView {
    fn copy(&mut self) -> CommandResult {
        if let Some(path) = self.selected() {
            let path = &path.path.clone();
            self.clipboard.copy(&path);
        }
        CommandResult::none()
    }

    fn cut(&mut self) -> CommandResult {
        if let Some(path) = self.selected() {
            let path = &path.path.clone();
            self.clipboard.cut(&path);
        }
        CommandResult::none()
    }

    fn paste(&mut self) -> CommandResult {
        match self.clipboard.maybe_command(self.directory.clone()) {
            Some(command) => command.into(),
            None => CommandResult::none(),
        }
    }

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
        let previously_selected = self.table_state.selected();
        let previous_double_click_start = self.double_click_start;

        self.table_state.select(Some(i));
        self.double_click_start = Some(Instant::now());
        if let Some(selected_index) = previously_selected {
            if i == selected_index {
                // Maybe double-clicked
                if let Some(start) = previous_double_click_start {
                    if start.elapsed().as_millis() < DOUBLE_CLICK_MS {
                        eprintln!("Double clicked on {i}");
                        return self.open_selected();
                    }
                }
            }
        }
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

        self.double_click_start = None;
        Command::SetSelected(Some(self.selected().unwrap().clone())).into()
    }

    fn next(&mut self) -> CommandResult {
        self.navigate(1)
    }

    fn previous(&mut self) -> CommandResult {
        self.navigate(-1)
    }

    fn next_page(&mut self) -> CommandResult {
        todo!("TODO: next_page");
    }

    fn previous_page(&mut self) -> CommandResult {
        todo!("TODO: previous_page");
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

    fn open_selected_in_custom_program(&mut self) -> CommandResult {
        match self.selected() {
            Some(path) => Command::OpenCustom(path.clone()).into(),
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
            SortColumn::Modified => items.sort_by_cached_key(|path| path.modified_comparator()),
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
        self.double_click_start = None;
        Command::SetSelected(None).into()
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
