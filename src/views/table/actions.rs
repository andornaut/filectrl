use super::TableView;
use crate::{
    command::{Command, PromptAction, result::CommandResult},
    file_system::path_info::PathInfo,
};

impl TableView {
    pub(super) fn delete(&mut self) -> CommandResult {
        let paths = if self.has_marks() {
            self.marked_paths()
        } else {
            match self.selected_path() {
                Some(path) => vec![path.clone()],
                None => return CommandResult::Handled,
            }
        };
        let count = paths.len();
        self.pending_delete = paths;
        Command::OpenPrompt(PromptAction::Delete(count)).into()
    }

    pub(super) fn navigate_to_home_directory(&mut self) -> CommandResult {
        match directories::BaseDirs::new() {
            Some(base_dirs) => match PathInfo::try_from(base_dirs.home_dir()) {
                Ok(path) => Command::Open(path).into(),
                Err(_) => Command::AlertError("Could not access home directory".into()).into(),
            },
            None => Command::AlertError("Could not determine home directory".into()).into(),
        }
    }

    pub(super) fn open_filter_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptAction::Filter(self.content.filter().to_string())).into()
    }

    pub(super) fn open_rename_prompt(&self) -> CommandResult {
        match self.selected_path() {
            None => Command::AlertWarn("No file selected".into()).into(),
            Some(path) => {
                let basename = path.basename.clone();
                Command::OpenPrompt(PromptAction::Rename(path.clone(), basename)).into()
            }
        }
    }

    pub(super) fn open_selected(&mut self) -> CommandResult {
        match self.selected_path() {
            Some(path) => Command::Open(path.clone()).into(),
            None => CommandResult::Handled,
        }
    }
}
