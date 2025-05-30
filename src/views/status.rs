mod handler;
mod view;
mod widgets;

use ratatui::layout::Rect;

use crate::{command::result::CommandResult, file_system::path_info::PathInfo};

#[derive(Default)]
pub(super) struct StatusView {
    directory: Option<PathInfo>,
    directory_len: usize,
    area: Rect,
    selected: Option<PathInfo>,
}

impl StatusView {
    fn set_directory(&mut self, directory: PathInfo, children: &[PathInfo]) -> CommandResult {
        self.directory = Some(directory);
        self.directory_len = children.len();
        CommandResult::Handled
    }

    fn set_selected(&mut self, selected: Option<PathInfo>) -> CommandResult {
        self.selected = selected;
        CommandResult::Handled
    }
}
