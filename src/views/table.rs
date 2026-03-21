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
use crate::{app::clipboard::ClipboardEntry, file_system::path_info::PathInfo};

#[derive(Default)]
pub(super) struct TableView {
    clipboard_entry: Option<ClipboardEntry>,
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
