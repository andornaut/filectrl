mod columns;
mod double_click;
mod handler;
mod line_item_map;
mod pager;
mod render;
mod scrollbar;
mod style;
mod widgets;

use ratatui::{crossterm::event::MouseEvent, layout::Rect, widgets::TableState};

use self::{
    columns::{Columns, SortColumn, SortDirection},
    double_click::DoubleClick,
    line_item_map::LineItemMap,
    scrollbar::ScrollbarView,
};
use crate::{
    app::config::Config,
    clipboard::Clipboard,
    command::{result::CommandResult, Command, PromptKind},
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct TableView {
    directory: Option<PathInfo>,
    directory_items: Vec<PathInfo>,
    directory_items_sorted: Vec<PathInfo>,
    filter: String,

    table_area: Rect,
    table_state: TableState,

    clipboard: Clipboard,
    columns: Columns,
    double_click: DoubleClick,
    mapper: LineItemMap,
    scrollbar_view: ScrollbarView,
}

impl TableView {
    pub fn new(config: &Config) -> Self {
        Self {
            double_click: DoubleClick::new(config),
            ..Self::default()
        }
    }

    // Copy / Cut / Paste
    fn cancel_clipboard(&mut self) -> CommandResult {
        self.clipboard.clear();
        CommandResult::none()
    }

    fn copy(&mut self) -> CommandResult {
        if let Some(path) = self.selected_path() {
            let path = path.clone();
            self.clipboard.copy_file(path.path.as_str());
            return Command::CopiedToClipboard(path).into();
        }
        CommandResult::none()
    }

    fn cut(&mut self) -> CommandResult {
        if let Some(path) = self.selected_path() {
            let path = path.clone();
            self.clipboard.cut_file(path.path.as_str());
            return Command::CutToClipboard(path).into();
        }
        CommandResult::none()
    }

    fn paste(&mut self) -> CommandResult {
        let destination = self.directory.as_ref().expect("Directory not set");
        match self.clipboard.try_to_command(destination.clone()) {
            Ok(command) => {
                self.clipboard.clear();
                command.into()
            }
            Err(_) => CommandResult::none(),
        }
    }

    // Handle events
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
        if clicked_line >= self.mapper.total_lines_count() {
            // Clicked past the table
            return CommandResult::none();
        }

        let clicked_item = self.mapper.item(clicked_line);
        let clicked_path = &self.directory_items_sorted[clicked_item];
        if self.double_click.click_and_is_double_click(clicked_path) {
            return self.open_selected();
        }

        self.table_state.select(Some(clicked_item));
        Command::SetSelected(Some(self.selected_path().unwrap().clone())).into()
    }

    fn handle_scroll(&mut self, event: &MouseEvent) -> CommandResult {
        if let Some(new_selected_item) = self.scrollbar_view.handle_mouse(
            event,
            self.mapper.total_lines_count(),
            self.mapper.visible_lines_count(),
        ) {
            return self.select(self.mapper.item(new_selected_item));
        }
        CommandResult::none()
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
        Command::SetSelected(Some(self.selected_path().unwrap().clone())).into()
    }

    fn previous(&mut self) -> CommandResult {
        self.table_state.scroll_up_by(1);
        Command::SetSelected(Some(self.selected_path().unwrap().clone())).into()
    }

    fn first(&mut self) -> CommandResult {
        self.select(0)
    }

    fn last(&mut self) -> CommandResult {
        self.select(self.directory_items_sorted.len().saturating_sub(1))
    }

    fn next_page(&mut self) -> CommandResult {
        let selected_item = self.table_state.selected().unwrap_or_default();
        let items_count = self.directory_items_sorted.len();

        if let Some(new_selected_item) = pager::next_page(&self.mapper, selected_item, items_count)
        {
            return self.select(new_selected_item);
        }
        CommandResult::none()
    }

    fn previous_page(&mut self) -> CommandResult {
        let selected_item = self.table_state.selected().unwrap_or_default();
        let offset = self.table_state.offset();

        if let Some(new_selected_item) = pager::previous_page(&self.mapper, selected_item, offset) {
            return self.select(new_selected_item);
        }
        CommandResult::none()
    }

    fn delete(&self) -> CommandResult {
        match self.selected_path() {
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
        match self.selected_path() {
            Some(path) => Command::Open(path.clone()).into(),
            None => CommandResult::none(),
        }
    }

    fn open_selected_in_custom_program(&mut self) -> CommandResult {
        match self.selected_path() {
            Some(path) => Command::OpenCustom(path.clone()).into(),
            None => CommandResult::none(),
        }
    }

    fn select(&mut self, item: usize) -> CommandResult {
        self.table_state.select(Some(item));
        Command::SetSelected(Some(self.selected_path().unwrap().clone())).into()
    }

    fn selected_path(&self) -> Option<&PathInfo> {
        self.table_state
            .selected()
            .map(|i| &self.directory_items_sorted[i])
    }

    fn reset_selection(&mut self) -> CommandResult {
        let selected = if self.directory_items_sorted.is_empty() {
            self.table_state.select(None);
            None
        } else {
            self.table_state.select(Some(0));
            Some(self.directory_items_sorted[0].clone())
        };
        Command::SetSelected(selected).into()
    }

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
        let selected_path = self.selected_path().cloned();
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
}
