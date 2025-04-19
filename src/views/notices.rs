mod handler;
mod notice_kind;
mod view;
mod widgets;

use std::collections::HashSet;

use notice_kind::NoticeKind;
use ratatui::layout::Rect;

use crate::{
    clipboard::ClipboardCommand,
    command::{result::CommandResult, task::Task},
};

#[derive(Default)]
pub(super) struct NoticesView {
    area: Rect,
    clipboard_command: Option<ClipboardCommand>,
    filter: String,
    tasks: HashSet<Task>,
}

impl NoticesView {
    fn active_notices(&self) -> impl Iterator<Item = NoticeKind<'_>> {
        let mut notices = Vec::new();

        if !self.tasks.is_empty() {
            notices.push(NoticeKind::Progress);
        }

        if let Some(command) = &self.clipboard_command {
            notices.push(NoticeKind::Clipboard(command));
        }

        if !self.filter.is_empty() {
            notices.push(NoticeKind::Filter(&self.filter));
        }

        notices.into_iter()
    }

    fn clear_clipboard(&mut self) -> CommandResult {
        self.clipboard_command = None;
        CommandResult::Handled
    }

    fn clear_progress(&mut self) -> CommandResult {
        self.tasks.clear();
        CommandResult::Handled
    }

    fn height(&self) -> u16 {
        self.active_notices().count() as u16
    }

    fn set_filter(&mut self, filter: String) -> CommandResult {
        self.filter = filter;
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
