mod handler;
mod notice;
mod view;
mod widget;

use std::collections::HashSet;

use notice::Notice;
use ratatui::layout::Rect;

use crate::{
    app::config::keybindings::Action,
    app::{clipboard::ClipboardEntry, config::Config},
    command::{progress::Task, result::CommandResult},
};

pub(super) struct NoticesView {
    area: Rect,
    search_tick: u16,
    clipboard_entry: Option<ClipboardEntry>,
    hide_marked: bool,
    hint: String,
    cancel_hint: String,
    filter: String,
    mark_count: usize,
    search_query: Option<String>,
    search_cancelled: bool,
    tasks: HashSet<Task>,
    /// Cached notice list, rebuilt by the command handler whenever
    /// notice-relevant state changes. Both `constraint` and `render` read this
    /// instead of rebuilding per frame, and the mouse handler uses it to map a
    /// y-position back to the clicked notice.
    notices: Vec<Notice>,
}

impl NoticesView {
    pub fn new() -> Self {
        let keybindings = &Config::global().keybindings;
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
            search_tick: 0,
            clipboard_entry: None,
            hide_marked: false,
            hint,
            cancel_hint,
            filter: String::new(),
            mark_count: 0,
            search_query: None,
            search_cancelled: false,
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
            Some(Notice::Marked(self.mark_count))
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
        app::clipboard::ClipboardEntry,
        app::config::RuntimeEnv,
        command::{
            Command,
            progress::{ActiveTask, Task, TaskKind, Transfer},
        },
        file_system::path_info::PathInfo,
    };

    fn view() -> NoticesView {
        let config = Config::load(RuntimeEnv::default(), None, vec![]).unwrap();
        Config::init(config);
        NoticesView::new()
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
                Notice::Marked(_) => "marked",
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
    fn filter_change_resets_the_mark_count() {
        use crate::command::handler::CommandHandler;
        let mut v = view();
        v.mark_count = 2;
        v.handle_command(&Command::FilterChanged("x".into()));
        assert_eq!(v.mark_count, 0);
        assert_eq!(tags(&v.build_notices()), vec!["filter"]);
    }

    #[test]
    fn showing_bookmarks_resets_the_mark_count() {
        use crate::command::handler::CommandHandler;
        let mut v = view();
        v.mark_count = 2;
        v.handle_command(&Command::Bookmarks { bookmarks: vec![] });
        assert_eq!(v.mark_count, 0);
    }

    #[test]
    fn updates_for_cleared_tasks_are_not_resurrected() {
        let mut v = view();
        let (tx, rx) = mpsc::channel();
        let (mut at, initial, _cancel) = ActiveTask::new(tx, copy_kind(), 100);
        v.update_tasks(initial);
        v.clear_progress();
        assert!(v.build_notices().is_empty());

        // A late, non-new (in-progress) update for the cleared task is ignored.
        at.increment(10);
        at.send_progress();
        let update = recv_task(&rx);
        assert!(!update.is_new());
        v.update_tasks(update);
        assert!(v.build_notices().is_empty());
    }
}
