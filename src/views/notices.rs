mod handler;
mod notice;
mod view;
mod widgets;

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
    search_hint: String,
    cancel_hint: String,
    filter: String,
    mark_count: usize,
    search_query: Option<String>,
    tasks: HashSet<Task>,
    /// Cached at render time so the mouse handler can map y-position -> action
    notices: Vec<Notice>,
}

impl NoticesView {
    pub fn new() -> Self {
        let keybindings = &Config::global().keybindings;
        let hint = format!(
            "(Press {} to clear)",
            keybindings.hint_for(&[Action::ResetView])
        );
        let search_hint = format!(
            "(Press {} to cancel)",
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
            search_hint,
            cancel_hint,
            filter: String::new(),
            mark_count: 0,
            search_query: None,
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
            self.search_query.as_ref().map(|_| Notice::SearchLoading),
            self.search_query
                .as_ref()
                .map(|q| Notice::Search(q.clone())),
            marked,
            clipboard,
            (!self.filter.is_empty()).then_some(Notice::Filter(self.filter.clone())),
        ]
        .into_iter()
        .flatten()
        .collect()
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
