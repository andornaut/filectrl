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
    columns::Columns, content::DirectoryContent, double_click::DoubleClick, marks::Marks,
    navigation::PendingLoad, row_map::LineItemMap,
};
use super::ScrollbarView;
#[cfg(test)]
use crate::app::config::Config;
use crate::{
    app::{clipboard::ClipboardEntry, config::UiConfig},
    file_system::path_info::PathInfo,
};

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
    /// Line the scrollbar thumb is dragged to, tracked while a drag is active
    /// so the thumb renders at the cursor even when the window top snaps
    /// across a wrapped row.
    drag_line: Option<usize>,

    /// Generation of the stream (directory load or search) currently feeding
    /// the listing. `ListingBatch`es stamped with a different generation are
    /// stale and ignored.
    stream_generation: u64,
    /// Selection state captured at the start of a streamed load, applied once it
    /// completes (see `begin_directory`/`finish_directory`).
    pending_load: PendingLoad,

    columns: Columns,
    double_click: DoubleClick,
    mapper: LineItemMap,
    /// Per-item row heights, cached across frames. Rebuilt (together with
    /// `mapper`) only when `height_cache_key` changes, so scrolling a large
    /// directory stays O(visible rows) instead of O(items).
    cached_heights: Vec<usize>,
    /// The (name column width, content revision) the cache was built for.
    height_cache_key: Option<(u16, u64)>,
    scrollbar_view: ScrollbarView,
}

impl TableView {
    /// The listing settings and the double-click window come from the config
    /// here, once, rather than from a global reached for during a sort or a
    /// click.
    pub(super) fn new(ui: UiConfig) -> Self {
        Self {
            clipboard_entry: None,
            content: DirectoryContent::new(ui.show_hidden_files, ui.sort_directories_first),
            marks: Marks::default(),
            pending_delete: Vec::new(),
            table_area: Rect::default(),
            table_state: TableState::default(),
            first_visible_item: 0,
            drag_line: None,
            stream_generation: 0,
            pending_load: PendingLoad::default(),
            columns: Columns::default(),
            double_click: DoubleClick::new(ui.double_click_interval_milliseconds),
            mapper: LineItemMap::default(),
            cached_heights: Vec::new(),
            height_cache_key: None,
            scrollbar_view: ScrollbarView::default(),
        }
    }
}

/// The shipped defaults, so a test says only what it is about. Test-only: the
/// app builds its table from the config it loaded, and a `Default` that read a
/// global would put a test's behaviour at the mercy of whether another test
/// had initialized one.
#[cfg(test)]
impl Default for TableView {
    fn default() -> Self {
        Config::init_test();
        Self::new(Config::global().ui)
    }
}

/// A listing of `a`, `b` and `c`, with `a` and `b` marked and the cursor left
/// on `c`, so an action reading the marks and one reading the cursor cannot
/// produce the same answer. Shared by the sibling modules whose tests are about
/// which of the two an action reads.
#[cfg(test)]
fn marked_table() -> (crate::test_support::TempDir, TableView) {
    use crate::{app::config::Config, test_support::TempDir};

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
