mod columns;
mod double_click;
mod handler;
mod line_item_map;
mod render;
mod scrollbar;
mod style;
mod widgets;

use ratatui::{
    layout::Rect,
    widgets::{ScrollbarState, TableState},
};

use self::{
    columns::{Columns, SortColumn, SortDirection},
    double_click::DoubleClick,
    line_item_map::LineItemMap,
    scrollbar::ScrollbarController,
};
use crate::{
    app::config::Config,
    clipboard::{Clipboard, ClipboardPasteContext},
    command::{result::CommandResult, Command, PromptKind},
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct TableView {
    directory: Option<PathInfo>,
    directory_items: Vec<PathInfo>,
    directory_items_sorted: Vec<PathInfo>,
    filter: String,

    scrollbar_area: Rect,
    scrollbar_state: ScrollbarState,
    table_area: Rect,
    table_state: TableState,

    clipboard: Clipboard,
    columns: Columns,
    double_click: DoubleClick,
    mapper: LineItemMap,
    is_scrollbar_dragging: bool,
    scrollbar_controller: ScrollbarController,
}

impl TableView {
    pub fn new(config: &Config) -> Self {
        Self {
            double_click: DoubleClick::new(config.double_click_threshold_milliseconds),
            scrollbar_controller: ScrollbarController::default(),
            ..Self::default()
        }
    }

    // Copy / Cut / Paste
    fn cancel_clipboard(&mut self) -> CommandResult {
        self.clipboard.clear();
        CommandResult::none()
    }

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
        let destination = self.directory.as_ref().expect("Directory not set").clone();
        let context = ClipboardPasteContext::new(&mut self.clipboard, destination);
        match Command::try_from(context) {
            Ok(command) => {
                self.clipboard.clear();
                command.into()
            }
            Err(_) => CommandResult::none(),
        }
    }

    // Handle clicks
    fn click_header(&mut self, x: u16) -> CommandResult {
        if let Some(column) = self.columns.sort_column_for_click(x) {
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

        let clicked_item = self.mapper.item(clicked_line);
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

    // Navigate
    fn next(&mut self) -> CommandResult {
        let delta = 1;
        if let Some(selected) = self.table_state.selected() {
            if selected + delta >= self.directory_items_sorted.len() {
                return CommandResult::none();
            }
        }
        self.table_state.scroll_down_by(delta as u16);
        Command::SetSelected(Some(self.selected().unwrap().clone())).into()
    }

    fn previous(&mut self) -> CommandResult {
        self.table_state.scroll_up_by(1);
        Command::SetSelected(Some(self.selected().unwrap().clone())).into()
    }

    fn first(&mut self) -> CommandResult {
        self.select(0)
    }

    fn last(&mut self) -> CommandResult {
        self.select(self.directory_items_sorted.len().saturating_sub(1))
    }

    fn next_page(&mut self) -> CommandResult {
        let selected_item = self.table_state.selected().unwrap_or_default();

        // If not at the last visible item, then move to it
        let current_last_item = self.mapper.item(self.mapper.last_visible_line());
        if selected_item != current_last_item {
            return self.select(current_last_item);
        }

        // If already at the last item, then no-op
        if selected_item == self.directory_items_sorted.len().saturating_sub(1) {
            return CommandResult::none();
        }

        let new_first_line = self.mapper.first_line(selected_item);
        let new_last_line = self.mapper.last_visible_line_starting_at(new_first_line);
        let mut new_last_item = self.mapper.item(new_last_line);

        // Adjust if necessary to keep the selected item visible
        // If the last item overflows, ratatui will scroll down until it is fully visible,
        // so we need to "scroll up" `new_last_item`, so that the `current_last_item` remains visible.
        let new_last_item_last_line = self.mapper.last_line(new_last_item);
        if new_last_item_last_line > new_last_line {
            new_last_item -= 1;
        }
        self.select(new_last_item)
    }

    fn previous_page(&mut self) -> CommandResult {
        let selected_item = self.table_state.selected().unwrap_or_default();

        // If not at the first visible item, then move to it
        let current_first_item = self.table_state.offset();
        if selected_item != current_first_item {
            return self.select(current_first_item);
        }

        // If already at the first item, then no-op
        if selected_item == 0 {
            return CommandResult::none();
        }

        let new_last_item_first_line = self.mapper.first_line(selected_item);
        let new_first_line = self
            .mapper
            .first_visible_line_ending_at(new_last_item_first_line);
        let mut new_first_item = self.mapper.item(new_first_line);

        // Adjust if necessary to keep the selected item visible
        // If the first item overflows, ratatui will scroll up until it is fully visible,
        // so we need to "scroll down" `new_first_item`, so that the `current_first_item` remains visible.
        let new_first_item_first_line = self.mapper.first_line(new_first_item);
        if new_first_item_first_line < new_first_line {
            new_first_item += 1;
        }
        self.select(new_first_item)
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

    // Select
    fn select(&mut self, item: usize) -> CommandResult {
        self.table_state.select(Some(item));
        Command::SetSelected(Some(self.selected().unwrap().clone())).into()
    }

    fn selected(&self) -> Option<&PathInfo> {
        self.table_state
            .selected()
            .map(|i| &self.directory_items_sorted[i])
    }

    fn reset_selection(&mut self) -> CommandResult {
        if self.directory_items_sorted.is_empty() {
            self.table_state.select(None);
            Command::SetSelected(None).into()
        } else {
            self.table_state.select(Some(0));
            Command::SetSelected(Some(self.directory_items_sorted[0].clone())).into()
        }
    }

    // Set directory, filter
    fn set_directory(&mut self, directory: PathInfo, children: Vec<PathInfo>) -> CommandResult {
        self.directory = Some(directory);
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
        match self.columns.sort_column() {
            SortColumn::Name => items.sort_by_cached_key(|path| path.name_comparator()),
            SortColumn::Modified => items.sort_by_cached_key(|path| path.modified_comparator()),
            SortColumn::Size => items.sort_by_cached_key(|path| path.size),
        };
        if *self.columns.sort_direction() == SortDirection::Descending {
            items.reverse();
        }

        if !self.filter.is_empty() {
            let filter_lowercase = self.filter.to_ascii_lowercase();
            items.retain(|path| path.name().to_ascii_lowercase().contains(&filter_lowercase));
        }

        // TODO: Put this back once paging works!
        // set_directory(), which is called when navigating or refreshing, set_filter(), and sort_by() all
        // call this method. Sometimes, we won't be able to retain the currently selected item, b/c it
        // may no longer be present in the `items`, but other times it is present, though possibly at a
        // different position. We handle these cases by storing the currently selected item before assigning
        // the new items, and then attempting to restore the selection afterward.
        let selected_path = self.selected().cloned();
        self.directory_items_sorted = items;
        if let Some(selected_path) = selected_path {
            if let Some(new_index) = self
                .directory_items_sorted
                .iter()
                .position(|p| p == &selected_path)
            {
                self.table_state.select(Some(new_index));
                return Command::SetSelected(Some(selected_path)).into();
            }
        }
        self.reset_selection()
    }

    fn sort_by(&mut self, column: SortColumn) -> CommandResult {
        self.columns.sort_by(column);
        self.sort()
    }

    fn is_scrollbar_click(&self, x: u16, y: u16) -> bool {
        self.scrollbar_controller.is_clicked(x, y)
    }

    fn update_scrollbar_position(&mut self, y: u16) {
        let total_lines = self.mapper.total_number_of_lines();
        let visible_height = self.table_area.height as usize;
        let current_offset = self.table_state.offset();
        let item_count = self.directory_items_sorted.len();

        // Use the scrollbar controller to compute the new scroll position
        if let Some(result) = self.scrollbar_controller.handle_drag(
            y,
            total_lines,
            visible_height,
            current_offset,
            item_count,
            |line| self.mapper.item(line),
        ) {
            // Update the table state with the result
            *self.table_state.offset_mut() = result.new_offset;
            self.table_state.select(Some(result.selected_item));
            self.scrollbar_state = result.scrollbar_state;
        }
    }
}
