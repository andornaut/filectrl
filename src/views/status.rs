mod handler;
mod view;
mod widget;

use super::ListingCount;
use crate::{
    app::config::keybindings::{Action, KeyBindings},
    command::result::CommandResult,
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct StatusView {
    directory: Option<PathInfo>,
    directory_len: usize,
    /// Generation of the directory load the count follows.
    load_generation: u64,
    selected: Option<PathInfo>,
    listing_count: ListingCount,
    /// Entries counted for a reload, applied when it completes. `None` during navigation, which
    /// counts into `directory_len`.
    staged_len: Option<usize>,
    /// Right-aligned help-key hint; empty when the key is unbound.
    help_hint: String,
}

impl StatusView {
    pub(super) fn new(keybindings: &KeyBindings) -> Self {
        Self {
            help_hint: help_hint(keybindings),
            ..Self::default()
        }
    }

    fn begin_directory(&mut self, directory: PathInfo, generation: u64) -> CommandResult {
        self.directory = Some(directory);
        self.directory_len = 0;
        self.load_generation = generation;
        self.staged_len = None;
        CommandResult::Handled
    }

    /// Begins a reload, keeping the current count because the table keeps its listing on screen.
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

    /// Applies a reload's count as the table swaps its entries in.
    fn finish_listing(&mut self, generation: u64) -> CommandResult {
        if generation == self.load_generation
            && let Some(staged_len) = self.staged_len.take()
        {
            self.directory_len = staged_len;
        }
        CommandResult::Handled
    }

    pub(super) fn set_listing_count(&mut self, listing_count: ListingCount) {
        self.listing_count = listing_count;
    }

    /// The `# Items` count as `(total, shown)`; the table counts search results and bookmarks.
    fn item_count(&self) -> (usize, Option<usize>) {
        match self.listing_count {
            ListingCount::Directory { shown } => (self.directory_len, Some(shown)),
            ListingCount::Results { shown, total } => (total, Some(shown)),
            ListingCount::Bookmarks { shown } => (shown, None),
        }
    }

    fn set_selected(&mut self, selected: Option<PathInfo>) -> CommandResult {
        self.selected = selected;
        CommandResult::Handled
    }
}

fn help_hint(keybindings: &KeyBindings) -> String {
    keybindings
        .keys_for(Action::ToggleHelp)
        .first()
        .map_or_else(String::new, |key| format!(" {key} Help "))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::command::{Command, handler::CommandHandler};

    #[test]
    fn the_help_hint_names_the_help_key() {
        crate::app::config::Config::init_test();

        assert_eq!(
            " ? Help ",
            help_hint(&crate::app::config::Config::global().keybindings)
        );
    }

    fn path(name: &str) -> PathInfo {
        let mut info = PathInfo::try_from(Path::new(".")).unwrap();
        info.display_name = name.to_string();
        info
    }

    #[test]
    fn the_item_count_follows_what_the_table_lists() {
        let mut view = StatusView::default();
        navigated(&mut view, 1);
        batch(&mut view, 5, 1);

        view.set_listing_count(ListingCount::Directory { shown: 2 });
        assert_eq!((5, Some(2)), view.item_count());
        view.set_listing_count(ListingCount::Results {
            shown: 3,
            total: 42,
        });
        assert_eq!((42, Some(3)), view.item_count());
        view.set_listing_count(ListingCount::Bookmarks { shown: 3 });
        assert_eq!((3, None), view.item_count());
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

        // A streamed listing arrives in batches, so the count is summed.
        assert_eq!(5, view.directory_len);
    }

    #[test]
    fn batches_from_a_superseded_load_are_not_counted() {
        let mut view = StatusView::default();
        navigated(&mut view, 2);

        // A batch in flight from before navigating away belongs to the previous listing.
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

        assert_eq!(0, view.directory_len);
    }

    #[test]
    fn a_refresh_recounts_rather_than_adding_to_the_previous_total() {
        let mut view = StatusView::default();
        navigated(&mut view, 1);
        batch(&mut view, 4, 1);

        // The watcher re-reads the same directory, so its batches repeat counted entries.
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

        // The listing stays on screen during a reload, so the count must not drop to zero.
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

        // Completion of the load this reload replaced.
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

        // Search results stream under their own generation, which keeps them out of the count.
        batch(&mut view, 100, 9);

        assert_eq!(4, view.directory_len);
    }

    #[test]
    fn the_bookmarks_listing_does_not_change_the_directory_summary() {
        let mut view = StatusView::default();
        navigated(&mut view, 1);
        batch(&mut view, 4, 1);

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
            range: false,
        });
        assert_eq!(
            Some("chosen".to_string()),
            view.selected.as_ref().map(|info| info.display_name.clone())
        );

        view.handle_command(&Command::SelectionChanged {
            selected: None,
            mark_count: 0,
            range: false,
        });
        assert!(view.selected.is_none());
    }
}
