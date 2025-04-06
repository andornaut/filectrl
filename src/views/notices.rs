mod handler;
mod render;
mod widgets;

use ratatui::layout::Rect;
use std::collections::HashSet;

use crate::{
    command::{result::CommandResult, task::Task},
    file_system::path_info::PathInfo,
};

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
    pub(super) fn clear_clipboard(&mut self) -> CommandResult {
        self.clipboard = None;
        CommandResult::none()
    }

    pub(super) fn clear_progress(&mut self) -> CommandResult {
        self.tasks.clear();
        CommandResult::none()
    }

    pub(super) fn clear_progress_if_done(&mut self) {
        if self.tasks.iter().all(|task| task.is_done()) {
            self.clear_progress();
        }
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
        // If we're starting a new task (eg. from a clipboard paste), then we should reset the progress bar
        if task.is_new() {
            self.clear_progress_if_done();
        }

        self.tasks.replace(task);
        CommandResult::none()
    }

    pub(super) fn height(&self) -> u16 {
        let mut height = 0;
        if self.clipboard.is_some() {
            height += 1;
        }
        if !self.filter.is_empty() {
            height += 1;
        }
        if !self.tasks.is_empty() {
            height += 1;
        }
        height
    }
}
