mod actions;
mod clipboard;
mod columns;
mod directory;
mod double_click;
mod handler;
mod marks;
mod mouse;
mod row_map;
mod scroll;
mod scrollbar;
mod selection;
mod style;
mod view;
mod widgets;

use ratatui::{layout::Rect, widgets::TableState};

use self::{
    columns::Columns,
    double_click::DoubleClick,
    marks::Marks,
    row_map::LineItemMap,
    scrollbar::ScrollbarView,
};
use crate::{
    app::config::Config,
    command::{Command, result::CommandResult},
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct TableView {
    directory: Option<PathInfo>,
    directory_items: Vec<PathInfo>,
    directory_items_sorted: Vec<PathInfo>,
    filter: String,
    marks: Marks,

    table_area: Rect,
    table_state: TableState,

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

    // --- Mark methods ---

    fn toggle_mark(&mut self) -> CommandResult {
        if let Some(i) = self.table_state.selected() {
            self.marks.toggle(i);
        }
        Command::SetMarkCount(self.marks.len()).into()
    }

    fn enter_range_mode(&mut self) -> CommandResult {
        if let Some(i) = self.table_state.selected() {
            self.marks.enter_range(i);
        }
        Command::SetMarkCount(self.marks.len()).into()
    }

    fn clear_marks(&mut self) {
        self.marks.clear();
    }

    fn has_marks(&self) -> bool {
        !self.marks.is_empty()
    }

    fn marked_paths(&self) -> Vec<PathInfo> {
        self.marks
            .iter()
            .filter_map(|&i| self.directory_items_sorted.get(i).cloned())
            .collect()
    }

    fn update_range_marks(&mut self) {
        if let Some(cursor) = self.table_state.selected() {
            self.marks.update_range(cursor);
        }
    }
}
