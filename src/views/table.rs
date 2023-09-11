mod clipboard;
mod command_handler;
mod navigate;
mod sort;
mod style;
mod view;

use self::{
    clipboard::Clipboard,
    navigate::navigate_overflowing,
    sort::{SortColumn, SortDirection},
};
use crate::{
    app::config::Config,
    command::{result::CommandResult, Command, PromptKind},
    file_system::human::HumanPath,
};
use ratatui::{
    layout::Rect,
    prelude::Constraint,
    widgets::{ScrollbarState, TableState},
};
use std::{cmp::min, time::Instant};

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
    double_click_threshold_milliseconds: u16,

    clipboard: Clipboard,
}

const DEFAULT_DOUBLE_CLICK_THRESHOLD_MILLISECONDS: u16 = 300;

impl TableView {
    pub fn new(config: &Config) -> Self {
        let double_click_threshold_milliseconds = config
            .double_click_threshold_milliseconds
            .unwrap_or(DEFAULT_DOUBLE_CLICK_THRESHOLD_MILLISECONDS);
        Self {
            double_click_threshold_milliseconds,
            ..Self::default()
        }
    }

    // Copy / Cut / Paste
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

    // Handle clicks
    fn handle_click_header(&mut self, x: u16) -> CommandResult {
        if let Some(column) = click_column(x, self.name_column_width) {
            self.sort_by(column)
        } else {
            CommandResult::none()
        }
    }

    fn handle_click_table(&mut self, y: u16) -> CommandResult {
        let first_visual_index = self
            .table_visual_rows
            .iter()
            .position(|&value| value == self.table_state.offset())
            .unwrap_or_default();
        let y = first_visual_index + y as usize - 1;

        if y >= self.table_visual_rows.len() {
            // Clicked past the table
            return CommandResult::none();
        }

        let item_index = self.table_visual_rows[y];
        if let Some(previously_selected_index) = self.table_state.selected() {
            if item_index == previously_selected_index {
                // Maybe double-clicked
                if let Some(start) = self.double_click_start {
                    if start.elapsed().as_millis()
                        <= self.double_click_threshold_milliseconds as u128
                    {
                        return self.open_selected();
                    }
                }
            }
        }

        self.table_state.select(Some(item_index));
        self.double_click_start = Some(Instant::now());
        Command::SetSelected(Some(self.selected().unwrap().clone())).into()
    }

    // Navigate
    fn navigate(&mut self, delta: i8) -> CommandResult {
        if self.directory_items_sorted.is_empty() {
            return CommandResult::none();
        }
        let len = self.directory_items_sorted.len();
        // If nothing is selected, then navigate to the first item, i
        let i = self
            .table_state
            .selected()
            .map_or(0, |i| navigate_overflowing(len, i, delta));
        self.select_index(i)
    }

    fn next(&mut self) -> CommandResult {
        self.navigate(1)
    }

    fn previous(&mut self) -> CommandResult {
        self.navigate(-1)
    }

    fn first(&mut self) -> CommandResult {
        self.select_index(0)
    }

    fn last(&mut self) -> CommandResult {
        self.select_index(self.directory_items_sorted.len() - 1)
    }

    fn select_index(&mut self, index: usize) -> CommandResult {
        self.table_state.select(Some(index));
        self.double_click_start = None;
        return Command::SetSelected(Some(self.selected().unwrap().clone())).into();
    }

    fn last_index_on_page(&self, offset: usize) -> usize {
        let window_min = self.position(offset);
        let window_max = self.window_max(window_min);
        let mut item_index = self.table_visual_rows[window_max];

        // If the last item on this screen spans the next screen, then it won't actually be shown on this screen,
        // in which case we should display the previous item.
        if window_max != self.table_visual_rows.len() - 1 // Not applicable if we're already the last item
            && item_index == self.table_visual_rows[window_max + 1]
        {
            item_index -= 1;
        }
        item_index
    }

    fn next_page(&mut self) -> CommandResult {
        // If the last item isn't selected, then select it;
        //   otherwise, advance such that the last item becomes the first item, and select the new last item.
        let selected_index = self.table_state.selected().unwrap_or_default();
        let last_index = self.last_index_on_page(selected_index);
        if selected_index != last_index {
            return self.select_index(last_index);
        }
        if selected_index == self.directory_items_sorted.len() - 1 {
            // NOOP if the last item is already selected
            return CommandResult::none();
        }

        self.table_state.select(Some(selected_index + 1));
        self.next_page()
    }

    fn previous_page(&mut self) -> CommandResult {
        // If the first item isn't selected, then select it;
        //   otherwise, advance such that the first item becomes the last item, and select the new first item.
        let selected_index = self.table_state.selected().unwrap_or_default();
        let first_index = self.table_state.offset();
        if selected_index != first_index {
            return self.select_index(first_index);
        }
        if selected_index == 0 {
            // NOOP if the first item is already selected
            return CommandResult::none();
        }
        if selected_index == 1 {
            return self.select_index(0); // Workaround the +1 below
        }

        let old_window_min = self.position(first_index);
        let old_item_index = self.table_visual_rows[old_window_min];
        let rows_per_screen = self.table_rect.height as usize - 1; // -1 for table header
        let new_visual_index = old_window_min.saturating_sub(rows_per_screen);
        let mut new_index = self.table_visual_rows[new_visual_index] + 1; // `+1` so that the first row becomes the last on the new page
        let new_window_min = self.position(new_index);
        let new_window_max = self.window_max(new_window_min);

        if new_window_max != self.table_visual_rows.len() - 1 // Not applicable if we're already the last item
            && self.table_visual_rows[new_window_max] == old_item_index // new_window_max could be the next (+1 item) overlapping element
            && self.table_visual_rows[new_window_max] == self.table_visual_rows[new_window_max + 1]
        {
            new_index += 1;
        }
        return self.select_index(new_index);
    }

    // Delete / Open
    fn delete(&self) -> CommandResult {
        match self.selected() {
            Some(path) => Command::DeletePath(path.clone()).into(),
            None => CommandResult::none(),
        }
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

    // Select / Unselect
    fn selected(&self) -> Option<&HumanPath> {
        self.table_state
            .selected()
            .map(|i| &self.directory_items_sorted[i])
    }

    fn reset_selection(&mut self) -> CommandResult {
        self.double_click_start = None;

        if self.directory_items_sorted.is_empty() {
            self.table_state.select(None);
            return Command::SetSelected(None).into();
        } else {
            self.table_state.select(Some(0));
            return Command::SetSelected(Some(self.directory_items_sorted[0].clone())).into();
        }
    }

    // Set directory, filter
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

    // Sort
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

        self.reset_selection()
    }

    fn sort_by(&mut self, column: SortColumn) -> CommandResult {
        if self.sort_column == column {
            self.sort_direction.toggle();
        } else {
            self.sort_column = column;
        }
        self.sort()
    }

    fn window_max(&self, window_min: usize) -> usize {
        let rows_per_screen = self.table_rect.height - 1;
        min(
            self.table_visual_rows.len().saturating_sub(1),
            window_min + rows_per_screen as usize - 1,
        )
    }

    fn position(&self, item_index: usize) -> usize {
        self.table_visual_rows
            .iter()
            .position(|&i| i == item_index)
            .unwrap_or_default()
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
