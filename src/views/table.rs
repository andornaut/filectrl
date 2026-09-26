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
mod widget;

use ratatui::{layout::Rect, widgets::TableState};

use self::{
    actions::PendingDelete, columns::Columns, content::DirectoryContent, double_click::DoubleClick,
    marks::Marks, navigation::PendingLoad, row_map::LineItemMap, style::ClipboardHighlight,
};
use super::ScrollbarView;
use crate::app::config::{UiConfig, keybindings::KeyBindings};
#[cfg(test)]
use crate::{app::config::Config, file_system::path_info::PathInfo};

pub(super) struct TableView {
    clipboard: Option<ClipboardHighlight>,
    content: DirectoryContent,
    marks: Marks,
    pending_delete: PendingDelete,

    table_area: Rect,
    table_state: TableState,
    /// Index of the topmost rendered item, owned by the render pass so only the
    /// visible rows are built.
    first_visible_item: usize,
    /// Line the scrollbar thumb is dragged to while a drag is active.
    drag_line: Option<usize>,
    /// Set when the wheel scrolled the window away from the cursor, so the
    /// render does not bring the cursor into view. Cleared by any handled key.
    wheel_scrolled: bool,

    /// Generation of the stream feeding the listing; batches from another
    /// generation are ignored.
    stream_generation: u64,
    /// Selection state captured at the start of a streamed load, applied once it
    /// completes.
    pending_load: PendingLoad,
    /// Whether input moved the cursor or marked while the current search
    /// streamed in. If not, the finished search puts the cursor on the top row.
    search_cursor_chosen: bool,

    columns: Columns,
    double_click: DoubleClick,
    mapper: LineItemMap,
    /// Per-item row heights, rebuilt with `mapper` only when `height_cache_key`
    /// changes.
    cached_heights: Vec<usize>,
    /// The (name column width, visible line count, content revision) the
    /// cache was built for.
    height_cache_key: Option<(u16, usize, u64)>,
    scrollbar_view: ScrollbarView,
    /// The sortable column headers, which name their sort keys.
    header_labels: [String; 3],
    /// Set while a y/n confirmation prompt is open; mouse input is ignored.
    ignores_mouse: bool,
}

impl TableView {
    pub(super) fn set_ignores_mouse(&mut self, ignores: bool) {
        self.ignores_mouse = ignores;
    }

    /// The display name of the one entry a delete prompt asks about.
    pub(super) fn pending_delete_name(&self) -> Option<String> {
        self.pending_delete.only_name()
    }

    pub(super) fn new(ui: UiConfig, keybindings: &KeyBindings) -> Self {
        Self {
            clipboard: None,
            content: DirectoryContent::new(ui),
            marks: Marks::default(),
            pending_delete: PendingDelete::default(),
            table_area: Rect::default(),
            table_state: TableState::default(),
            first_visible_item: 0,
            drag_line: None,
            wheel_scrolled: false,
            stream_generation: 0,
            pending_load: PendingLoad::default(),
            search_cursor_chosen: false,
            columns: Columns::default(),
            double_click: DoubleClick::new(ui.double_click_interval_milliseconds),
            mapper: LineItemMap::default(),
            cached_heights: Vec::new(),
            height_cache_key: None,
            scrollbar_view: ScrollbarView::default(),
            header_labels: widget::header_labels(keybindings),
            ignores_mouse: false,
        }
    }
}

/// The shipped defaults. Test-only: the app builds its table from its config.
#[cfg(test)]
impl Default for TableView {
    fn default() -> Self {
        Config::init_test();
        Self::new(Config::global().ui, &Config::global().keybindings)
    }
}

/// A listing of `a`, `b` and `c`, with `a` and `b` marked and the cursor on `c`,
/// so actions reading the marks and the cursor give different answers.
#[cfg(test)]
fn marked_table() -> (crate::test_support::TempDir, TableView) {
    use crate::test_support::TempDir;

    Config::init_test();
    let dir = TempDir::new("table_actions");
    let items: Vec<PathInfo> = ["a", "b", "c"]
        .iter()
        .map(|name| {
            let path = dir.join(name);
            std::fs::write(&path, b"x").unwrap();
            PathInfo::try_from(path.as_path()).unwrap()
        })
        .collect();

    let mut table = TableView::default();
    table.begin_directory(
        PathInfo::try_from(dir.path()).unwrap(),
        navigation::Reselect::Top,
    );
    table.content.append(&items);
    table.finish_directory();
    table.select(0);
    table.toggle_mark();
    table.select(1);
    table.toggle_mark();
    table.select(2);
    assert_eq!(2, table.marks.len());
    (dir, table)
}

#[cfg(test)]
fn display_names(paths: &[PathInfo]) -> Vec<String> {
    paths.iter().map(|p| p.display_name.clone()).collect()
}
