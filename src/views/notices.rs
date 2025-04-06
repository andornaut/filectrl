mod handler;
mod notice_type;
mod render;
mod widgets;

use ratatui::layout::Rect;
use std::collections::HashSet;

use crate::{
    command::{result::CommandResult, task::Task},
    file_system::path_info::PathInfo,
};

pub(super) use notice_type::NoticeType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClipboardOperation {
    Cut,
    Copy,
}

#[derive(Default)]
pub(super) struct NoticesView {
    pub(super) area: Rect,
    pub(super) clipboard: Option<(ClipboardOperation, PathInfo)>,
    pub(super) filter: String,
    pub(super) tasks: HashSet<Task>,
}

impl NoticesView {
    pub(super) fn active_notices(&self) -> impl Iterator<Item = NoticeType<'_>> {
        let mut notices = Vec::new();

        if !self.tasks.is_empty() {
            notices.push(NoticeType::Progress);
        }

        if let Some((operation, path)) = &self.clipboard {
            notices.push(NoticeType::Clipboard((operation, path)));
        }

        if !self.filter.is_empty() {
            notices.push(NoticeType::Filter(&self.filter));
        }

        notices.into_iter()
    }
    pub(super) fn clear_clipboard(&mut self) -> CommandResult {
        self.clipboard = None;
        CommandResult::none()
    }

    pub(super) fn clear_progress(&mut self) -> CommandResult {
        self.tasks.clear();
        CommandResult::none()
    }

    pub(super) fn clear_filter(&mut self) -> CommandResult {
        self.filter.clear();
        CommandResult::none()
    }

    pub(super) fn set_clipboard(
        &mut self,
        path: PathInfo,
        operation: ClipboardOperation,
    ) -> CommandResult {
        self.clipboard = Some((operation, path));
        CommandResult::none()
    }

    pub(super) fn set_filter(&mut self, filter: String) -> CommandResult {
        self.filter = filter;
        CommandResult::none()
    }

    pub(super) fn update_tasks(&mut self, task: Task) -> CommandResult {
        // If the task is not new and not in our set, it means we previously cleared it.
        // In this case, we should ignore the update to prevent resurrecting cleared tasks.
        if !task.is_new() && !self.tasks.contains(&task) {
            return CommandResult::none();
        }

        if task.is_done() {
            self.tasks.remove(&task);
        } else {
            self.tasks.replace(task);
        }
        CommandResult::none()
    }

    pub(super) fn height(&self) -> u16 {
        self.active_notices().count() as u16
    }
}
