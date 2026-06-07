use super::{TableView, columns::SortColumn};
use crate::{command::result::CommandResult, file_system::path_info::PathInfo};

/// What to select after the visible items change.
#[derive(Clone, Copy, Default)]
pub(super) enum Reselect {
    /// The selection does not carry over (navigating to another directory,
    /// filtering, or re-sorting). Restore the selected file if it still
    /// exists, otherwise select the first item.
    #[default]
    Top,
    /// The same directory was reloaded: keep the selected file, or hold the
    /// cursor at the same position if the file was deleted.
    Keep,
}

impl TableView {
    /// Begin a streamed directory load. Captures what to reselect once the load
    /// completes (in `finish_directory`) and clears the visible listing. The
    /// command handlers reset filter/search/bookmarks beforehand; `reselect`
    /// controls only how the selection is restored.
    pub(super) fn begin_directory(&mut self, new_directory: PathInfo, reselect: Reselect) {
        // Capture the pre-load state BEFORE clearing the listing.
        self.loading_reselect = reselect;
        self.loading_prev_directory = self.content.directory().cloned();
        self.loading_prev_selected = self.selected_path().cloned();
        self.loading_prev_selected_index = self.table_state.selected();

        self.clear_marks();
        self.table_state.select(None);
        self.first_visible_item = 0;
        self.content.start_listing(new_directory);
    }

    /// Finish a streamed directory load: sort the accumulated entries once and
    /// restore the selection captured by `begin_directory`.
    pub(super) fn finish_directory(&mut self) -> CommandResult {
        let reselect = self.loading_reselect;
        self.content
            .finalize_listing(self.columns.sort_column(), self.columns.sort_direction());
        // Marks are stored by index, so the new listing invalidates them.
        self.clear_marks();

        // If we navigated to an ancestor directory, select the child we came from.
        if let Some(prev_directory) = self.loading_prev_directory.take()
            && let Some(new_directory) = self.content.directory()
        {
            let prev_path = prev_directory.as_path();
            let new_path = new_directory.as_path();
            if prev_path.starts_with(new_path) && prev_path != new_path {
                let new_components_count = new_path.components().count();
                // .nth() is 0-indexed, so target_child is a child of new_path
                if let Some(target_child) = prev_path.components().nth(new_components_count) {
                    let target_ancestor_path = new_path.join(target_child);
                    if let Some(item) = self.content.find_by_path(&target_ancestor_path) {
                        return self.select(item);
                    }
                }
            }
        }

        // Otherwise restore the previously selected file by inode, or (on a
        // refresh) hold the cursor position if it was deleted.
        if let Some(selected_path) = self.loading_prev_selected.take() {
            if let Some(new_index) = self.content.find_by_inode(&selected_path) {
                return self.select(new_index);
            }
            if let Reselect::Keep = reselect
                && let Some(idx) = self.loading_prev_selected_index.take()
            {
                return self.select(idx.min(self.content.len().saturating_sub(1)));
            }
        }

        // Fallback: select the first item.
        self.select(0)
    }

    pub(super) fn set_filter(&mut self, filter: String) -> CommandResult {
        // Avoid an extra sort/SelectionChanged when there is no filter change
        if self.content.filter().is_empty() && filter.is_empty() {
            return CommandResult::Handled;
        }
        self.content.set_filter(filter);
        self.sort(Reselect::Top)
    }

    pub(super) fn sort(&mut self, reselect: Reselect) -> CommandResult {
        // Marks are stored by index, so any change to the visible items invalidates them.
        self.clear_marks();

        // Store the currently selected item before reordering, then try to
        // restore it afterward by comparing inodes. The file may have moved
        // position (another process modified the directory) or disappeared
        // (navigation, deletion, or filtering it out).
        let selected = self.selected_path().cloned();
        let selected_index = self.table_state.selected();

        self.content
            .sort(self.columns.sort_column(), self.columns.sort_direction());

        if let Some(selected_path) = selected {
            if let Some(new_index) = self.content.find_by_inode(&selected_path) {
                // The selected file still exists after sort/filter
                return self.select(new_index);
            }

            // The selected file is gone. On a refresh (Reselect::Keep) it was
            // likely deleted, so hold the cursor at the same position;
            // otherwise fall through to the top.
            if let Reselect::Keep = reselect
                && let Some(idx) = selected_index
            {
                return self.select(idx.min(self.content.len().saturating_sub(1)));
            }
        }

        // Fallback: Select the first item
        self.select(0)
    }

    pub(super) fn sort_by(&mut self, column: SortColumn) -> CommandResult {
        self.columns.sort_by(column);
        self.sort(Reselect::Top)
    }

    pub(super) fn toggle_show_hidden(&mut self) -> CommandResult {
        self.content.toggle_show_hidden();
        self.sort(Reselect::Top)
    }
}

/// Synchronous convenience for tests: runs the streamed begin/append/finish
/// cycle in one call, mirroring how a directory loads at runtime.
#[cfg(test)]
impl TableView {
    fn set_directory(
        &mut self,
        directory: PathInfo,
        children: Vec<PathInfo>,
        reselect: Reselect,
    ) -> CommandResult {
        self.begin_directory(directory, reselect);
        self.content.append_listing(&children);
        self.finish_directory()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Reselect, SortColumn, TableView};
    use crate::{app::config::Config, file_system::path_info::PathInfo};

    fn ensure_config_initialized() {
        let config = Config::load(None, vec![]).unwrap();
        Config::init(config);
    }

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir =
                std::env::temp_dir().join(format!("filectrl_nav_{}_{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn file(&self, name: &str, size: usize) -> PathInfo {
            let path = self.dir.join(name);
            std::fs::write(&path, vec![b'x'; size]).unwrap();
            PathInfo::try_from(&path).unwrap()
        }

        fn directory(&self) -> PathInfo {
            PathInfo::try_from(&self.dir).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn selected_basename(table: &TableView) -> Option<String> {
        table.selected_path().map(|p| p.display_name.clone())
    }

    #[test]
    fn set_directory_top_selects_the_first_item() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let mut table = TableView::default();
        let children = vec![fx.file("b", 1), fx.file("a", 1), fx.file("c", 1)];
        table.set_directory(fx.directory(), children, Reselect::Top);

        assert_eq!(table.table_state.selected(), Some(0));
        assert_eq!(selected_basename(&table).as_deref(), Some("a"));
    }

    #[test]
    fn sort_keeps_the_selected_file_when_it_moves_position() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let mut table = TableView::default();
        // Name-ascending order: a, b, c
        let children = vec![fx.file("a", 3), fx.file("b", 1), fx.file("c", 2)];
        table.set_directory(fx.directory(), children, Reselect::Top);

        // Select "b" (index 1 by name).
        table.select(1);
        assert_eq!(selected_basename(&table).as_deref(), Some("b"));

        // Re-sort by size (b=1, c=2, a=3): "b" moves to index 0 but stays selected.
        table.sort_by(SortColumn::Size);
        assert_eq!(table.table_state.selected(), Some(0));
        assert_eq!(selected_basename(&table).as_deref(), Some("b"));
    }

    #[test]
    fn reselect_keep_holds_the_cursor_position_when_the_selected_file_is_deleted() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(
            fx.directory(),
            vec![fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)],
            Reselect::Top,
        );
        table.select(1); // "b"

        // Same directory reloaded with "b" removed; cursor holds at index 1.
        table.set_directory(
            fx.directory(),
            vec![fx.file("a", 1), fx.file("c", 1)],
            Reselect::Keep,
        );
        assert_eq!(table.table_state.selected(), Some(1));
        assert_eq!(selected_basename(&table).as_deref(), Some("c"));
    }

    #[test]
    fn reselect_top_falls_back_to_first_when_the_selected_file_is_gone() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(
            fx.directory(),
            vec![fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)],
            Reselect::Top,
        );
        table.select(2); // "c"

        table.set_directory(
            fx.directory(),
            vec![fx.file("a", 1), fx.file("b", 1)],
            Reselect::Top,
        );
        assert_eq!(table.table_state.selected(), Some(0));
        assert_eq!(selected_basename(&table).as_deref(), Some("a"));
    }
}
