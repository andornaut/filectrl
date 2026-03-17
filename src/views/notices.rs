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
        let mut notices = Vec::new();
        if !self.tasks.is_empty() {
            notices.push(Notice::Progress);
        }
        if let Some(cmd) = &state.clipboard_command {
            notices.push(Notice::Clipboard(cmd.clone()));
        }
        if !state.filter.is_empty() {
            notices.push(Notice::Filter(state.filter.clone()));
        }
        notices
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
