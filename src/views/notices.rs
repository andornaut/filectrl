mod handler;
mod render;
mod widgets;

use log::debug;
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
        debug!("Updating task: {:?}", task);
        debug!("Current tasks: {:?}", self.tasks);
        // If the task is not new and not in our set, it means we previously cleared it.
        // In this case, we should ignore the update to prevent resurrecting cleared tasks.
        if !task.is_new() && !self.tasks.contains(&task) {
            debug!("Ignoring update: {:?}", task);
            return CommandResult::none();
        }

        if task.is_done() {
            debug!("Removing done task: {:?}", task);
            self.tasks.remove(&task);
        } else {
            debug!("Adding/updating task: {:?}", task);
            self.tasks.replace(task);
        }
        debug!("Tasks after update: {:?}", self.tasks);
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
