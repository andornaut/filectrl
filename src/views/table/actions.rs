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

    pub(super) fn open_goto_prompt(&self) -> CommandResult {
        let directory = self
            .content
            .directory()
            .map(|d| d.path.clone())
            .unwrap_or_default();
        Command::OpenPrompt(PromptAction::Goto { directory }).into()
    }

    pub(super) fn open_chmod_prompt(&self) -> CommandResult {
        let (paths, initial_mode) = if self.has_marks() {
            (self.marked_paths(), String::new())
        } else {
            match self.selected_path() {
                Some(path) => {
                    let mode = format!("{:o}", path.mode() & 0o7777);
                    (vec![path.clone()], mode)
                }
                None => return Command::AlertWarn("No file(s) selected".into()).into(),
            }
        };
        Command::OpenPrompt(PromptAction::Chmod {
            paths,
            mode: initial_mode,
        })
        .into()
    }

    pub(super) fn open_create_directory_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptAction::CreateDirectory).into()
    }

    pub(super) fn open_filter_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptAction::Filter(self.content.filter().to_string())).into()
    }

    pub(super) fn open_rename_prompt(&self) -> CommandResult {
        match self.selected_path() {
            None => Command::AlertWarn("No file selected".into()).into(),
            Some(path) => {
                let basename = path.basename.clone();
                Command::OpenPrompt(PromptAction::Rename {
                    path: path.clone(),
                    name: basename,
                })
                .into()
            }
        }
    }

    pub(super) fn open_add_bookmark_prompt(&self) -> CommandResult {
        if self.content.is_showing_bookmarks() {
            return Command::AlertWarn("Cannot add a bookmark from the bookmarks view".into())
                .into();
        }
        match self.content.directory() {
            None => Command::AlertWarn("No current directory".into()).into(),
            Some(directory) => {
                let name = directory.basename.clone();
                Command::OpenPrompt(PromptAction::AddBookmark {
                    directory: directory.clone(),
                    name,
                })
                .into()
            }
        }
    }

    pub(super) fn show_bookmarks(&self) -> CommandResult {
        Command::GetBookmarks.into()
    }

    pub(super) fn open_search_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptAction::Search(String::new())).into()
    }

    pub(super) fn open_selected(&mut self) -> CommandResult {
        match self.selected_path() {
            Some(path) => Command::Open(path.clone()).into(),
            None => CommandResult::Handled,
        }
    }
}
