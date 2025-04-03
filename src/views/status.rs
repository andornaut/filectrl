mod handler;
mod render;
mod widgets;

use ratatui::layout::Rect;
use std::collections::HashSet;

use crate::{
    command::{result::CommandResult, task::Task},
    file_system::path_info::PathInfo,
};

#[derive(Default)]
enum Clipboard {
    Copy(PathInfo),
    Cut(PathInfo),
    #[default]
    None,
}

impl Clipboard {
    fn is_some(&self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Default)]
pub(super) struct StatusView {
    clipboard: Clipboard,
    directory: PathInfo,
    directory_len: usize,
    filter: String,
    rect: Rect,
    selected: Option<PathInfo>,
    tasks: HashSet<Task>,
}

impl StatusView {
    fn set_clipboard_copy(&mut self, path: PathInfo) -> CommandResult {
        self.clipboard = Clipboard::Copy(path);
        self.clear_progress_if_done();
        CommandResult::none()
    }

    fn set_clipboard_cut(&mut self, path: PathInfo) -> CommandResult {
        self.clipboard = Clipboard::Cut(path);
        self.clear_progress_if_done();
        CommandResult::none()
    }

    fn set_directory(&mut self, directory: PathInfo, children: &Vec<PathInfo>) -> CommandResult {
        self.clipboard = Clipboard::None;
        self.directory = directory;
        self.directory_len = children.len();
        self.clear_progress_if_done();
        CommandResult::none()
    }

    fn set_filter(&mut self, filter: String) -> CommandResult {
        self.clipboard = Clipboard::None;
        self.filter = filter;
        self.clear_progress_if_done();
        CommandResult::none()
    }

    fn set_selected(&mut self, selected: Option<PathInfo>) -> CommandResult {
        self.clipboard = Clipboard::None;
        self.selected = selected;
        self.clear_progress_if_done();
        CommandResult::none()
    }

    fn clear_progress(&mut self) -> CommandResult {
        self.tasks.clear();
        CommandResult::none()
    }

    fn clear_progress_if_done(&mut self) {
        if self.tasks.iter().all(|task| task.is_done()) {
            self.clear_progress();
        }
    }

    fn update_tasks(&mut self, task: Task) -> CommandResult {
        // If we're starting a new task (eg. from a clipboard paste), then we should reset the progress bar
        if task.is_new() {
            self.clear_progress_if_done();
        }

        self.tasks.replace(task);
        CommandResult::none()
    }
}
