mod handler;
mod view;
mod widget;

use crate::{command::result::CommandResult, file_system::path_info::PathInfo};

#[derive(Default)]
pub(super) struct StatusView {
    directory: Option<PathInfo>,
    directory_len: usize,
    /// Generation of the directory load whose entries the count follows.
    load_generation: u64,
    selected: Option<PathInfo>,
    /// Entries counted for a reload, applied once it completes. `None` while a
    /// navigation loads, which counts straight into `directory_len` because
    /// the listing it described is gone.
    staged_len: Option<usize>,
}

impl StatusView {
    fn begin_directory(&mut self, directory: PathInfo, generation: u64) -> CommandResult {
        self.directory = Some(directory);
        self.directory_len = 0;
        self.load_generation = generation;
        self.staged_len = None;
        CommandResult::Handled
    }

    /// Begin a reload of the directory already summarized. The table keeps its
    /// listing on screen for the duration, so the count it belongs to is held
    /// too: resetting it here would show `# Items: 0` under a listing that has
    /// not changed, once per watcher refresh.
    fn begin_reload(&mut self, directory: PathInfo, generation: u64) -> CommandResult {
        self.directory = Some(directory);
        self.load_generation = generation;
        self.staged_len = Some(0);
        CommandResult::Handled
    }

    fn count_listing(&mut self, items: &[PathInfo], generation: u64) -> CommandResult {
        if generation == self.load_generation {
            match &mut self.staged_len {
                Some(staged_len) => *staged_len += items.len(),
                None => self.directory_len += items.len(),
            }
        }
        CommandResult::Handled
    }

    /// Apply a reload's count, in step with the table swapping its entries in.
    fn finish_listing(&mut self, generation: u64) -> CommandResult {
        if generation == self.load_generation
            && let Some(staged_len) = self.staged_len.take()
        {
            self.directory_len = staged_len;
        }
        CommandResult::Handled
    }

    fn set_selected(&mut self, selected: Option<PathInfo>) -> CommandResult {
        self.selected = selected;
        CommandResult::Handled
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::command::{Command, handler::CommandHandler};

    fn path(name: &str) -> PathInfo {
        let mut info = PathInfo::try_from(Path::new(".")).unwrap();
        info.display_name = name.to_string();
        info
    }

    fn navigated(view: &mut StatusView, generation: u64) {
        view.handle_command(&Command::NavigatedDirectory {
            directory: path("dir"),
            generation,
        });
    }

    fn refreshed(view: &mut StatusView, generation: u64) {
        view.handle_command(&Command::RefreshedDirectory {
            directory: path("dir"),
            generation,
        });
    }

    fn complete(view: &mut StatusView, generation: u64) {
        view.handle_command(&Command::DirectoryListingComplete { generation });
    }

    fn batch(view: &mut StatusView, count: usize, generation: u64) {
        view.handle_command(&Command::ListingBatch {
            items: (0..count).map(|i| path(&format!("f{i}"))).collect(),
            generation,
        });
    }

    #[test]
    fn batches_of_one_load_accumulate_into_the_item_count() {
        let mut view = StatusView::default();
        navigated(&mut view, 1);

        batch(&mut view, 2, 1);
        batch(&mut view, 3, 1);

        // The count is summed across batches because a streamed listing
        // arrives in pieces and no single command carries the total.
        assert_eq!(5, view.directory_len);
    }

    #[test]
    fn batches_from_a_superseded_load_are_not_counted() {
        let mut view = StatusView::default();
        navigated(&mut view, 2);

        // A batch still in flight when the user navigated away belongs to the
        // previous listing; counting it would inflate the new directory.
        batch(&mut view, 4, 1);
        assert_eq!(0, view.directory_len);

        batch(&mut view, 3, 2);
        assert_eq!(3, view.directory_len);
    }

    #[test]
    fn navigating_resets_the_count_before_the_new_listing_streams_in() {
        let mut view = StatusView::default();
        navigated(&mut view, 1);
        batch(&mut view, 4, 1);

        navigated(&mut view, 2);

        // Leaving the old count in place would show the previous directory's
        // total while the new one is still loading.
        assert_eq!(0, view.directory_len);
    }

    #[test]
    fn a_refresh_recounts_rather_than_adding_to_the_previous_total() {
        let mut view = StatusView::default();
        navigated(&mut view, 1);
        batch(&mut view, 4, 1);

        // The watcher re-reads the same directory, so its batches repeat
        // entries that were already counted.
        refreshed(&mut view, 2);
        batch(&mut view, 6, 2);
        complete(&mut view, 2);

        assert_eq!(6, view.directory_len);
    }

    #[test]
    fn a_refresh_holds_the_previous_count_until_the_reload_completes() {
        let mut view = StatusView::default();
        navigated(&mut view, 1);
        batch(&mut view, 4, 1);

        refreshed(&mut view, 2);
        batch(&mut view, 5, 2);

        // The table keeps its listing on screen for the whole reload, so a
        // count dropping to zero and climbing back would flash a total that
        // contradicts the rows above it.
        assert_eq!(4, view.directory_len);

        complete(&mut view, 2);
        assert_eq!(5, view.directory_len);
    }

    #[test]
    fn a_superseded_completion_does_not_apply_a_reloads_count() {
        let mut view = StatusView::default();
        navigated(&mut view, 1);
        batch(&mut view, 4, 1);
        refreshed(&mut view, 2);
        batch(&mut view, 5, 2);

        // The completion of the load this reload replaced. Applying the
        // pending count here would show it before its own listing arrives.
        complete(&mut view, 1);
        assert_eq!(4, view.directory_len);

        complete(&mut view, 2);
        assert_eq!(5, view.directory_len);
    }

    #[test]
    fn search_results_do_not_change_the_directory_summary() {
        let mut view = StatusView::default();
        navigated(&mut view, 1);
        batch(&mut view, 4, 1);

        // The Directory section describes the directory the user is in, not
        // the listing on screen. Search results stream under their own
        // generation, which is what keeps them out of the count.
        batch(&mut view, 100, 9);

        assert_eq!(4, view.directory_len);
    }

    #[test]
    fn the_bookmarks_listing_does_not_change_the_directory_summary() {
        let mut view = StatusView::default();
        navigated(&mut view, 1);
        batch(&mut view, 4, 1);

        // Bookmarks are an overlay listing, not a change of directory.
        view.handle_command(&Command::Bookmarks {
            bookmarks: vec![path("a"), path("b")],
        });

        assert_eq!(4, view.directory_len);
        assert_eq!(
            Some("dir".to_string()),
            view.directory
                .as_ref()
                .map(|info| info.display_name.clone())
        );
    }

    #[test]
    fn the_selection_snapshot_replaces_the_selected_details() {
        let mut view = StatusView::default();

        view.handle_command(&Command::SelectionChanged {
            selected: Some(path("chosen")),
            mark_count: 0,
        });
        assert_eq!(
            Some("chosen".to_string()),
            view.selected.as_ref().map(|info| info.display_name.clone())
        );

        // An empty listing clears the selection, so the pane must not keep
        // describing a file that is no longer selected.
        view.handle_command(&Command::SelectionChanged {
            selected: None,
            mark_count: 0,
        });
        assert!(view.selected.is_none());
    }
}
