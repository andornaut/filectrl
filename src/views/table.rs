mod actions;
mod clipboard;
mod columns;
mod content;
mod navigation;
mod double_click;
mod handler;
mod marks;
mod mouse;
mod row_map;
mod scroll;
mod selection;
mod style;
mod view;
mod widgets;

use std::rc::Rc;

use ratatui::{layout::Rect, widgets::TableState};

use self::{
    columns::Columns,
    content::DirectoryContent,
    double_click::DoubleClick,
    marks::Marks,
    row_map::LineItemMap,
};
use super::ScrollbarView;
use crate::{
    app::config::Config,
    command::{Command, result::CommandResult},
    file_system::path_info::PathInfo,
    app::config::keybindings::KeyBindings,
};

pub(super) struct TableView {
    content: DirectoryContent,
    keybindings: Rc<KeyBindings>,
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
            content: DirectoryContent::default(),
            keybindings: Rc::clone(&config.keybindings),
            marks: Marks::default(),
            table_area: Rect::default(),
            table_state: TableState::default(),
            columns: Columns::default(),
            double_click: DoubleClick::new(config),
            mapper: LineItemMap::default(),
            scrollbar_view: ScrollbarView::default(),
        }
    }

    // --- Mark methods ---

    fn toggle_mark(&mut self) -> CommandResult {
        if let Some(i) = self.table_state.selected() {
            if self.marks.in_range_mode() {
                self.marks.enter_range(i); // toggles range mode off
            } else {
                self.marks.toggle(i);
            }
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
            .filter_map(|&i| self.content.get(i).cloned())
            .collect()
    }

    fn update_range_marks(&mut self) {
        if let Some(cursor) = self.table_state.selected() {
            self.marks.update_range(cursor);
        }
    }
}
