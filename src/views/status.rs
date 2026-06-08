mod handler;
mod view;
mod widget;

use crate::{command::result::CommandResult, file_system::path_info::PathInfo};

#[derive(Default)]
pub(super) struct StatusView {
    directory: Option<PathInfo>,
    directory_len: usize,
    /// Generation of the directory load whose entries `directory_len` counts.
    load_generation: u64,
    selected: Option<PathInfo>,
}

impl StatusView {
    fn begin_directory(&mut self, directory: PathInfo, generation: u64) -> CommandResult {
        self.directory = Some(directory);
        self.directory_len = 0;
        self.load_generation = generation;
        CommandResult::Handled
    }

    fn count_listing(&mut self, items: &[PathInfo], generation: u64) -> CommandResult {
        if generation == self.load_generation {
            self.directory_len += items.len();
        }
        CommandResult::Handled
    }

    fn set_selected(&mut self, selected: Option<PathInfo>) -> CommandResult {
        self.selected = selected;
        CommandResult::Handled
    }
}
