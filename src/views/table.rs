mod clipboard;
mod double_click;
mod handler;
mod line_to_item;
mod sort;
mod style;
mod view;
mod widgets;

use self::{
    clipboard::Clipboard,
    double_click::DoubleClick,
    sort::{SortColumn, SortDirection},
};
use crate::{
    app::config::Config,
    command::{result::CommandResult, Command, PromptKind},
    file_system::human::HumanPath,
};
use line_to_item::LineToItemMapper;
use log::debug;
use ratatui::{
    layout::Rect,
    prelude::Constraint,
    widgets::{ScrollbarState, TableState},
};
use std::cmp::min;

const NAME_MIN_LEN: u16 = 39;
const MODE_LEN: u16 = 10;
const MODIFIED_LEN: u16 = 12;
const SIZE_LEN: u16 = 7;

#[derive(Default)]
pub(super) struct TableView {
    directory: HumanPath,
    directory_items: Vec<HumanPath>,
    directory_items_sorted: Vec<HumanPath>,
    filter: String,
    mapper: LineToItemMapper,
    name_column_width: u16,
    sort_column: SortColumn,
    sort_direction: SortDirection,

    scrollbar_rect: Rect,
    scrollbar_state: ScrollbarState,

    table_rect: Rect,
    table_state: TableState,

    clipboard: Clipboard,
    double_click: DoubleClick,
}

impl TableView {
    pub fn new(config: &Config) -> Self {
        Self {
            double_click: DoubleClick::new(config.double_click_threshold_milliseconds),
            ..Self::default()
        }
    }

    // Copy / Cut / Paste
    fn copy(&mut self) -> CommandResult {
        if let Some(path) = self.selected() {
            let path = path.clone();
            self.clipboard.copy(path.path.as_str());
            return Command::ClipboardCopy(path).into();
        }
        CommandResult::none()
    }

    fn cut(&mut self) -> CommandResult {
        if let Some(path) = self.selected() {
            let path = path.clone();
            self.clipboard.cut(path.path.as_str());
            return Command::ClipboardCut(path).into();
        }
        CommandResult::none()
    }

    fn paste(&mut self) -> CommandResult {
        match self.clipboard.maybe_command(self.directory.clone()) {
            Some(command) => command.into(),
            None => CommandResult::none(),
        }
    }

    fn click_header(&mut self, x: u16) -> CommandResult {
        if let Some(column) = click_column(x, self.name_column_width) {
            self.sort_by(column)
        } else {
            CommandResult::none()
        }
    }

    fn click_table(&mut self, y: u16) -> CommandResult {
        let y = y as usize - 1; // -1 for the header
        let clicked_line = self.mapper.first_visible_line() + y;
        if clicked_line >= self.mapper.total_number_of_lines() {
            // Clicked past the table
            return CommandResult::none();
        }

        let clicked_item = self.mapper[clicked_line];
        let clicked_path = &self.directory_items_sorted[clicked_item];
        if self
            .double_click
            .click_and_check_for_double_click(clicked_path)
        {
            return self.open_selected();
        }

        self.table_state.select(Some(clicked_item));
        Command::SetSelected(Some(self.selected().unwrap().clone())).into()
    }

    fn next(&mut self) -> CommandResult {
        //self.navigate(1)
        let delta = 1;
        if let Some(selected) = self.table_state.selected() {
            if selected + delta >= self.directory_items_sorted.len() {
                return CommandResult::none();
            }
        }
        self.table_state.scroll_down_by(delta as u16);
        return Command::SetSelected(Some(self.selected().unwrap().clone())).into();
    }

    fn previous(&mut self) -> CommandResult {
        self.table_state.scroll_up_by(1);
        return Command::SetSelected(Some(self.selected().unwrap().clone())).into();
    }

    fn first(&mut self) -> CommandResult {
        self.select(0)
    }

    fn last(&mut self) -> CommandResult {
        self.select(self.directory_items_sorted.len() - 1)
    }

    fn select(&mut self, item: usize) -> CommandResult {
        self.table_state.select(Some(item));
        return Command::SetSelected(Some(self.selected().unwrap().clone())).into();
    }

    fn next_page(&mut self) -> CommandResult {
        let selected_item = self.table_state.selected().unwrap_or_default();
        let old_last_item = self.mapper.last_visible_item();
        if selected_item != old_last_item {
            return self.select(old_last_item);
        }
        if selected_item == self.directory_items_sorted.len() - 1 {
            // NOOP if the last item is already selected
            return CommandResult::none();
        }

        // At this point, the selected_item is the last visible item
        let old_last_line = self.mapper.get_line(selected_item);
        let visible_lines = self.table_rect.height as usize - 1; // -1 for table header
        let last_line = min(
            old_last_line + visible_lines,
            self.mapper.total_number_of_lines() - 1,
        );
        let mut last_item = self.mapper[last_line];

        let first_line = self.mapper.get_line(last_item);
        let first_item = self.mapper[first_line];
        if old_last_item < first_item {
            // if old_last_item is offscreen, then shift over so that it is visible
            last_item -= 1;
        }
        self.select(last_item)
    }

    fn previous_page(&mut self) -> CommandResult {
        let selected_item = self.table_state.selected().unwrap_or_default();
        let old_first_item = self.table_state.offset();
        if selected_item != old_first_item {
            return self.select(old_first_item);
        }
        if selected_item == 0 {
            // NOOP if the first item is already selected
            return CommandResult::none();
        }

        // At this point, the selected_item is the first visible item
        let old_first_line = self.mapper.get_line(old_first_item);
        let visible_lines = self.table_rect.height as usize - 1; // -1 for table header
        let first_line = old_first_line.saturating_sub(visible_lines);
        let mut first_item = self.mapper[first_line];

        let last_line = self.mapper.last_visible_line(first_line);
        let last_item = self.mapper[last_line];
        if old_first_item > last_item {
            // If old_first_item is offscreen, then shift over so that it is visible
            first_item += 1;
        }
        self.select(first_item)
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
