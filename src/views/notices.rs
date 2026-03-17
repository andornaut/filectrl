mod handler;
mod notice;
mod view;
mod widgets;

use std::collections::HashSet;

use notice::Notice;
use ratatui::layout::Rect;

use crate::{
    app::state::AppState,
    command::{result::CommandResult, task::Task},
};

#[derive(Default)]
pub(super) struct NoticesView {
    area: Rect,
    tasks: HashSet<Task>,
    /// Cached at render time so the mouse handler can map y-position → action
    notices: Vec<Notice>,
}

impl NoticesView {
    fn build_notices(&self, state: &AppState) -> Vec<Notice> {
        [
            (!self.tasks.is_empty()).then_some(Notice::Progress),
            state.clipboard_entry.as_ref().map(|e| Notice::Clipboard(e.clone())),
            (!state.filter.is_empty()).then_some(Notice::Filter(state.filter.clone())),
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

        if task.is_done_or_error() {
            self.tasks.remove(&task);
        } else {
            self.tasks.replace(task); // upsert
        }
        CommandResult::Handled
    }
}
