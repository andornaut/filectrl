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
    navigation::PendingLoad, row_map::LineItemMap,
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
    /// Index of the topmost rendered item. Owned by the render pass (instead of
    /// ratatui's auto-scroll) so only the visible window's rows are built.
    first_visible_item: usize,

    /// Generation of the directory load currently being streamed in. Batches
    /// stamped with a different generation are stale and ignored.
    load_generation: u64,
    /// Selection state captured at the start of a streamed load, applied once it
    /// completes (see `begin_directory`/`finish_directory`).
    pending_load: PendingLoad,

    columns: Columns,
    double_click: DoubleClick,
    mapper: LineItemMap,
    scrollbar_view: ScrollbarView,
}
