mod actions;
mod clipboard;
mod columns;
mod content;
mod double_click;
mod handler;
mod marks;
mod mouse;
mod navigation;
mod row_map;
mod scroll;
mod selection;
mod style;
mod view;
mod widgets;

use ratatui::{layout::Rect, widgets::TableState};

use self::{
    columns::Columns, content::DirectoryContent, double_click::DoubleClick, marks::Marks,
    row_map::LineItemMap,
};
use super::ScrollbarView;
use crate::file_system::path_info::PathInfo;

pub(super) struct TableView {
    content: DirectoryContent,
    marks: Marks,
    pending_delete: Vec<PathInfo>,

    table_area: Rect,
    table_state: TableState,

    columns: Columns,
    double_click: DoubleClick,
    mapper: LineItemMap,
    scrollbar_view: ScrollbarView,
}

impl TableView {
    pub fn new(double_click_interval_milliseconds: u16) -> Self {
        Self {
            content: DirectoryContent::default(),
            marks: Marks::default(),
            pending_delete: Vec::new(),
            table_area: Rect::default(),
            table_state: TableState::default(),
            columns: Columns::default(),
            double_click: DoubleClick::new(double_click_interval_milliseconds),
            mapper: LineItemMap::default(),
            scrollbar_view: ScrollbarView::default(),
        }
    }
}
