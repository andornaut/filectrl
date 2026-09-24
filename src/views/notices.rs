mod handler;
mod notice;
mod view;
mod widget;

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use notice::Notice;
use ratatui::layout::Rect;

use crate::{
    app::clipboard::ClipboardEntry,
    app::config::keybindings::{Action, KeyBindings},
    command::{progress::Task, result::CommandResult},
};

pub(super) struct NoticesView {
    area: Rect,
    /// When the current search began, which is what the loading indicator's
    /// position is derived from. `None` whenever no search is loading.
    search_started_at: Option<Instant>,
    clipboard_entry: Option<ClipboardEntry>,
    hide_marked: bool,
    hint: String,
    cancel_hint: String,
    filter: String,
    mark_count: usize,
    /// Whether the marks are a range being extended, which the marked notice
    /// names so that range mode is visible.
    range: bool,
    search_query: Option<String>,
    search_cancelled: bool,
    /// Generation of the current search (from `SearchStarted`), used to
    /// ignore `ExitedSearch` from superseded searches.
    search_generation: u64,
    tasks: HashSet<Task>,
    /// Cached notice list, rebuilt by the command handler whenever
    /// notice-relevant state changes. Both `constraint` and `render` read this
    /// instead of rebuilding per frame, and the mouse handler uses it to map a
    /// y-position back to the clicked notice.
    notices: Vec<Notice>,
}

impl NoticesView {
    pub fn new(keybindings: &KeyBindings) -> Self {
        let hint = format!(
            "(Press {} to clear)",
            keybindings.hint_for(&[Action::ResetView])
        );
        let cancel_hint = format!(
            "(Press {} to cancel)",
            keybindings.hint_for(&[Action::CancelTask])
        );
        Self {
            area: Rect::default(),
            search_started_at: None,
            clipboard_entry: None,
            hide_marked: false,
            hint,
            cancel_hint,
            filter: String::new(),
            mark_count: 0,
            range: false,
            search_query: None,
            search_cancelled: false,
            search_generation: 0,
            tasks: HashSet::new(),
            notices: Vec::new(),
        }
    }
}

impl NoticesView {
    fn build_notices(&self) -> Vec<Notice> {
        let clipboard = self
            .clipboard_entry
            .as_ref()
            .map(|e| Notice::Clipboard(e.clone()));
        let marked = if !self.hide_marked && clipboard.is_none() && self.mark_count > 0 {
            Some(Notice::Marked {
                count: self.mark_count,
                range: self.range,
            })
        } else {
            None
        };
        [
            (!self.tasks.is_empty()).then_some(Notice::Progress),
            (!self.tasks.is_empty()).then_some(Notice::Operations),
            self.search_query
                .as_ref()
                .filter(|_| !self.search_cancelled)
                .map(|_| Notice::SearchLoading),
            self.search_query.as_ref().map(|q| {
                if self.search_cancelled {
                    Notice::SearchCancelled(q.clone())
                } else {
                    Notice::Search(q.clone())
                }
            }),
            marked,
            clipboard,
            (!self.filter.is_empty()).then_some(Notice::Filter(self.filter.clone())),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Recompute the cached `notices` list. Any path that mutates
    /// notice-relevant state (tasks, clipboard, marks, filter, search) must
    /// call this so `constraint`/`render` read an up-to-date list; that is why
    /// `handle_command`/`handle_key` invoke it after dispatch.
    fn rebuild_notices(&mut self) {
        self.notices = self.build_notices();
    }

    fn clear_progress(&mut self) -> CommandResult {
        self.tasks.clear();
        CommandResult::Handled
    }

    /// Reset the search-notice state in full: query, cancelled flag, and the
    /// loading indicator's start time.
    fn clear_search_notice(&mut self) {
        self.search_query = None;
        self.search_cancelled = false;
        self.search_started_at = None;
    }

    /// How long the current search has been loading, which the indicator's
    /// position is a function of. Zero when none is.
    fn search_elapsed(&self) -> Duration {
        self.search_started_at
            .map_or(Duration::ZERO, |started_at| started_at.elapsed())
    }

    fn update_tasks(&mut self, task: Task) -> CommandResult {
        // If the task is not new and not in our set, it means we previously cleared it.
        // In this case, we should ignore the update to prevent resurrecting cleared tasks.
        if !task.is_new() && !self.tasks.contains(&task) {
            return CommandResult::Handled;
        }

        if task.is_terminal() {
            self.tasks.remove(&task);
        } else {
            self.tasks.replace(task); // upsert
        }
        CommandResult::Handled
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::{
        app::config::Config,
        command::{
            Command,
            handler::CommandHandler,
            progress::{ActiveTask, TaskKind, Transfer},
        },
        file_system::path_info::PathInfo,
    };

    fn view() -> NoticesView {
        Config::init_test();
        NoticesView::new(&Config::global().keybindings)
    }

    fn tags(notices: &[Notice]) -> Vec<&'static str> {
        notices
            .iter()
            .map(|n| match n {
                Notice::Progress => "progress",
                Notice::Operations => "operations",
                Notice::Search(_) => "search",
                Notice::SearchCancelled(_) => "search_cancelled",
                Notice::SearchLoading => "search_loading",
                Notice::Marked { .. } => "marked",
                Notice::Clipboard(_) => "clipboard",
                Notice::Filter(_) => "filter",
            })
            .collect()
    }

    fn clipboard_entry() -> ClipboardEntry {
        ClipboardEntry::Copy(vec![PathInfo::try_from("/tmp").unwrap()])
    }

    // --- build_notices ordering / mutual exclusion ---

    #[test]
    fn clipboard_suppresses_the_marked_notice() {
        let mut v = view();
        v.clipboard_entry = Some(clipboard_entry());
        v.mark_count = 3;
        assert_eq!(tags(&v.build_notices()), vec!["clipboard"]);
    }

    #[test]
    fn hide_marked_suppresses_the_marked_notice() {
        let mut v = view();
        v.mark_count = 2;
        assert_eq!(tags(&v.build_notices()), vec!["marked"]);
        v.hide_marked = true;
        assert!(v.build_notices().is_empty());
    }

    #[test]
    fn cancelled_search_replaces_loading_and_search() {
        let mut v = view();
        v.search_query = Some("foo".into());
        assert_eq!(tags(&v.build_notices()), vec!["search_loading", "search"]);
        v.search_cancelled = true;
        assert_eq!(tags(&v.build_notices()), vec!["search_cancelled"]);
    }

    #[test]
    fn notices_are_emitted_in_a_fixed_priority_order() {
        let mut v = view();
        v.search_query = Some("q".into());
        v.clipboard_entry = Some(clipboard_entry());
        v.filter = "f".into();
        // No tasks; marked is suppressed by the clipboard entry.
        assert_eq!(
            tags(&v.build_notices()),
            vec!["search_loading", "search", "clipboard", "filter"]
        );
    }

    // --- update_tasks ---

    fn copy_kind() -> TaskKind {
        TaskKind::Copy(Transfer {
            source: "/a".into(),
            destination: "/b".into(),
        })
    }

    fn recv_task(rx: &mpsc::Receiver<Command>) -> Task {
        match rx.recv().unwrap() {
            Command::Progress(t) => t,
            _ => panic!("expected Command::Progress"),
        }
    }

    #[test]
    fn new_task_is_added_and_shows_progress_notices() {
        let mut v = view();
        let (tx, _rx) = mpsc::channel();
        let (_at, initial, _cancel) = ActiveTask::new(tx, copy_kind(), 100);
        assert!(initial.is_new());
        v.update_tasks(initial);
        assert_eq!(tags(&v.build_notices()), vec!["progress", "operations"]);
    }

    #[test]
    fn terminal_task_is_removed() {
        let mut v = view();
        let (tx, rx) = mpsc::channel();
        let (at, initial, _cancel) = ActiveTask::new(tx, copy_kind(), 100);
        v.update_tasks(initial);
        at.done(); // sends a terminal snapshot of the same task
        v.update_tasks(recv_task(&rx));
        assert!(v.build_notices().is_empty());
    }

    #[test]
    fn selection_snapshot_updates_the_mark_count() {
        let mut v = view();
        v.handle_command(&Command::SelectionChanged {
            selected: None,
            mark_count: 2,
            range: false,
        });
        assert_eq!(v.mark_count, 2);
        assert_eq!(tags(&v.build_notices()), vec!["marked"]);

        v.handle_command(&Command::SelectionChanged {
            selected: None,
            mark_count: 0,
            range: false,
        });
        assert_eq!(v.mark_count, 0);
        assert!(v.build_notices().is_empty());
    }

    #[test]
    fn entering_and_leaving_range_mode_renames_the_marked_notice() {
        let mut v = view();
        let snapshot = |range| Command::SelectionChanged {
            selected: None,
            mark_count: 1,
            range,
        };
        v.handle_command(&snapshot(false));
        assert!(matches!(
            v.notices.as_slice(),
            [Notice::Marked { range: false, .. }]
        ));

        // The mark count is unchanged, so only the range flag can rebuild it.
        v.handle_command(&snapshot(true));
        assert!(matches!(
            v.notices.as_slice(),
            [Notice::Marked { range: true, .. }]
        ));

        v.handle_command(&snapshot(false));
        assert!(matches!(
            v.notices.as_slice(),
            [Notice::Marked { range: false, .. }]
        ));
    }

    #[test]
    fn a_filter_being_typed_shows_its_notice() {
        let mut v = view();

        v.handle_command(&Command::FilterEdited("ap".into()));

        assert_eq!(tags(&v.notices), vec!["filter"]);
    }

    #[test]
    fn starting_a_search_clears_the_filter_notice() {
        let mut v = view();
        v.filter = "ap".to_string();
        v.rebuild_notices();
        assert_eq!(tags(&v.notices), vec!["filter"]);

        // Search results are unfiltered, so a lingering "Filter: ap" notice
        // would describe a filter that is no longer applied.
        v.handle_command(&Command::StartSearch("q".to_string()));

        assert_eq!(v.filter, "");
        // A just-started search shows its own notices and no filter notice.
        assert_eq!(tags(&v.notices), vec!["search_loading", "search"]);
    }

    #[test]
    fn opening_bookmarks_clears_the_filter_notice() {
        let mut v = view();
        v.filter = "ap".to_string();
        v.rebuild_notices();
        assert_eq!(tags(&v.notices), vec!["filter"]);

        // The bookmarks listing is unfiltered, so a lingering "Filter: ap"
        // notice would describe a filter that is no longer applied.
        v.handle_command(&Command::Bookmarks { bookmarks: vec![] });

        assert_eq!(v.filter, "");
        assert!(v.notices.is_empty());
    }

    #[test]
    fn unchanged_mark_count_keeps_the_clipboard_and_skips_the_rebuild() {
        let mut v = view();
        v.handle_command(&Command::SelectionChanged {
            selected: None,
            mark_count: 2,
            range: false,
        });
        // Copying marked files sets the clipboard while the marks are kept.
        v.clipboard_entry = Some(clipboard_entry());
        let cached = tags(&v.notices);

        // A cursor move re-emits the same mark count.
        let result = v.handle_command(&Command::SelectionChanged {
            selected: None,
            mark_count: 2,
            range: false,
        });

        assert_eq!(result, CommandResult::Handled);
        assert!(v.clipboard_entry.is_some());
        // No rebuild: the cached notice list is untouched.
        assert_eq!(cached, tags(&v.notices));
    }

    #[test]
    fn marking_derives_a_clipboard_clear() {
        let mut v = view();
        v.clipboard_entry = Some(clipboard_entry());

        // Marks and clipboard are mutually exclusive.
        let result = v.handle_command(&Command::SelectionChanged {
            selected: None,
            mark_count: 1,
            range: false,
        });
        assert_eq!(result, Command::SetClipboardEntry(None).into());
    }

    #[test]
    fn showing_bookmarks_clears_the_search_notice() {
        let mut v = view();
        v.handle_command(&Command::StartSearch("q".into()));
        v.handle_command(&Command::SearchStarted { generation: 1 });
        assert_eq!(tags(&v.build_notices()), vec!["search_loading", "search"]);

        // The cancelled walker's ExitedSearch may lag; the notice must clear
        // immediately.
        v.handle_command(&Command::Bookmarks { bookmarks: vec![] });
        assert!(v.build_notices().is_empty());

        // The eventual exit stays a no-op.
        v.handle_command(&Command::ExitedSearch { generation: 1 });
        assert!(v.build_notices().is_empty());
    }

    #[test]
    fn stale_search_exit_does_not_clear_the_current_search() {
        let mut v = view();
        v.handle_command(&Command::StartSearch("q".into()));
        v.handle_command(&Command::SearchStarted { generation: 2 });

        // A superseded search's exit must not clear the current notice.
        v.handle_command(&Command::ExitedSearch { generation: 1 });
        assert_eq!(tags(&v.build_notices()), vec!["search_loading", "search"]);

        v.handle_command(&Command::ExitedSearch { generation: 2 });
        assert!(v.build_notices().is_empty());
    }

    #[test]
    fn a_cancelled_search_keeps_its_notice_through_the_walkers_exit() {
        let mut v = view();
        v.handle_command(&Command::StartSearch("q".into()));
        v.handle_command(&Command::SearchStarted { generation: 1 });

        v.handle_command(&Command::CancelSearch);
        assert_eq!(tags(&v.notices), vec!["search_cancelled"]);

        // The cancelled walker still exits; the relabelled notice stays until
        // the user clears it.
        v.handle_command(&Command::ExitedSearch { generation: 1 });
        assert_eq!(tags(&v.notices), vec!["search_cancelled"]);
    }

    #[test]
    fn clearing_the_marks_keeps_the_clipboard() {
        let mut v = view();
        v.handle_command(&Command::SelectionChanged {
            selected: None,
            mark_count: 2,
            range: false,
        });
        v.clipboard_entry = Some(clipboard_entry());

        // Only marking something displaces the clipboard; unmarking does not.
        let result = v.handle_command(&Command::SelectionChanged {
            selected: None,
            mark_count: 0,
            range: false,
        });

        assert_eq!(CommandResult::Handled, result);
        assert!(v.clipboard_entry.is_some());
    }

    #[test]
    fn the_delete_prompt_hides_the_marked_notice_until_it_is_answered() {
        let mut v = view();
        v.handle_command(&Command::SelectionChanged {
            selected: None,
            mark_count: 2,
            range: false,
        });

        // The prompt states the count itself.
        v.handle_command(&Command::OpenPrompt(crate::command::PromptAction::Delete(
            2,
        )));
        assert!(v.notices.is_empty());

        v.handle_command(&Command::CancelPrompt);
        assert_eq!(tags(&v.notices), vec!["marked"]);
    }

    #[test]
    fn navigating_clears_the_filter_and_the_marks_but_not_the_clipboard() {
        let mut v = view();
        v.handle_command(&Command::FilterChanged("f".into()));
        v.mark_count = 2;
        v.clipboard_entry = Some(clipboard_entry());

        v.handle_command(&Command::NavigatedDirectory {
            directory: PathInfo::try_from("/tmp").unwrap(),
            generation: 1,
        });

        // A clipboard survives navigation: that is how a paste reaches another
        // directory.
        assert_eq!(tags(&v.notices), vec!["clipboard"]);
    }

    #[test]
    fn reset_view_clears_the_clipboard_filter_and_marks() {
        let mut v = view();
        v.handle_command(&Command::FilterChanged("f".into()));
        v.mark_count = 2;
        v.clipboard_entry = Some(clipboard_entry());

        v.handle_command(&Command::ResetView);

        assert!(v.notices.is_empty());
        assert_eq!(0, v.mark_count);
    }

    #[test]
    fn a_click_resets_the_view_only_on_a_dismissable_notice() {
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut v = view();
        let (tx, _rx) = mpsc::channel();
        let (_at, initial, _cancel) = ActiveTask::new(tx, copy_kind(), 100);
        v.handle_command(&Command::Progress(initial));
        v.handle_command(&Command::FilterChanged("f".into()));
        assert_eq!(tags(&v.notices), vec!["progress", "operations", "filter"]);
        // Below the top of the screen, so a row is read relative to the view.
        v.area = Rect::new(0, 5, 40, 3);
        let mut click = |row| {
            v.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };

        // Progress is not something the reset clears.
        assert_eq!(CommandResult::Handled, click(5));
        assert_eq!(CommandResult::from(Command::ResetView), click(7));
    }

    #[test]
    fn updates_for_cleared_tasks_are_not_resurrected() {
        let mut v = view();
        let (tx, rx) = mpsc::channel();
        let (mut at, initial, _cancel) = ActiveTask::new(tx, copy_kind(), 100);
        v.update_tasks(initial);
        v.clear_progress();
        assert!(v.build_notices().is_empty());

        // Every later update is non-new, whether it carries progress or a
        // total the directory size scan only just produced, so none of them
        // may re-add the cleared task.
        at.increment(10);
        at.send_progress();
        at.set_total(500);
        for _ in 0..2 {
            let update = recv_task(&rx);
            assert!(!update.is_new());
            v.update_tasks(update);
            assert!(v.build_notices().is_empty());
        }
    }
}
