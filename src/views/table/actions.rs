use super::{TableView, style::PathSet};
use crate::{
    command::{Command, PromptAction, result::CommandResult},
    file_system::path_info::{PathInfo, compact},
};

/// The entries a delete prompt is waiting on, with their paths indexed for the
/// render's per-row lookup.
#[derive(Default)]
pub(super) struct PendingDelete {
    paths: Vec<PathInfo>,
    index: PathSet,
}

impl PendingDelete {
    fn set(&mut self, paths: Vec<PathInfo>) {
        self.index = PathSet::new(&paths);
        self.paths = paths;
    }

    pub(super) fn take(&mut self) -> Vec<PathInfo> {
        self.index = PathSet::default();
        std::mem::take(&mut self.paths)
    }

    pub(super) fn clear(&mut self) {
        self.take();
    }

    pub(super) fn contains(&self, item: &PathInfo) -> bool {
        self.index.contains(item)
    }
}

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
        self.pending_delete.set(paths);
        Command::OpenPrompt(PromptAction::Delete(count)).into()
    }

    pub(super) fn navigate_to_home_directory() -> CommandResult {
        match directories::BaseDirs::new() {
            Some(base_dirs) => match PathInfo::try_from(base_dirs.home_dir()) {
                Ok(path) => Command::Open(path).into(),
                Err(error) => Command::AlertError(format!(
                    "Failed to open the home directory {}: {error:#}",
                    base_dirs.home_dir().display()
                ))
                .into(),
            },
            None => Command::AlertError("Cannot determine the home directory".into()).into(),
        }
    }

    pub(super) fn open_goto_prompt(&self) -> CommandResult {
        let directory = self
            .content
            .directory()
            .map(|d| d.path.to_string_lossy().into_owned())
            .unwrap_or_default();
        Command::OpenPrompt(PromptAction::Goto { directory }).into()
    }

    pub(super) fn open_chmod_prompt(&self) -> CommandResult {
        let (paths, initial_mode) = if self.has_marks() {
            (self.marked_paths(), String::new())
        } else {
            match self.selected_path() {
                // A symlink's own mode is always 777 and chmod would apply to
                // its target, so the prompt would offer the wrong mode for the
                // wrong file. A marked symlink is refused when the chmod runs.
                Some(path) if path.is_symlink() => {
                    return Command::AlertWarn(format!(
                        "Cannot chmod {}: it is a symlink",
                        compact(&path.path)
                    ))
                    .into();
                }
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

    pub(super) fn open_create_directory_prompt() -> CommandResult {
        Command::OpenPrompt(PromptAction::CreateDirectory).into()
    }

    pub(super) fn open_filter_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptAction::Filter(self.content.filter().to_string())).into()
    }

    pub(super) fn open_rename_prompt(&self) -> CommandResult {
        match self.selected_path() {
            None => Command::AlertWarn("No file selected".into()).into(),
            Some(path) => {
                let display_name = path.display_name.clone();
                Command::OpenPrompt(PromptAction::Rename {
                    path: path.clone(),
                    name: display_name,
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
                let name = directory.display_name.clone();
                Command::OpenPrompt(PromptAction::AddBookmark {
                    directory: directory.clone(),
                    name,
                })
                .into()
            }
        }
    }

    pub(super) fn get_bookmarks() -> CommandResult {
        Command::GetBookmarks.into()
    }

    pub(super) fn open_search_prompt() -> CommandResult {
        Command::OpenPrompt(PromptAction::Search(String::new())).into()
    }

    pub(super) fn open_selected(&mut self) -> CommandResult {
        match self.selected_path() {
            Some(path) => Command::Open(path.clone()).into(),
            None => CommandResult::Handled,
        }
    }

    /// The picker offers applications for one path, so this deliberately
    /// ignores marks and uses the selection.
    pub(super) fn open_with(&mut self) -> CommandResult {
        match self.selected_path() {
            Some(path) => Command::OpenWithPrompt(path.clone()).into(),
            None => CommandResult::Handled,
        }
    }
}

/// Which entries each action acts on. Every action here reads either the marks
/// or the cursor, and nothing about the call site says which, so the choice is
/// pinned per action rather than left to the reader.
#[cfg(test)]
mod tests {
    use super::super::{display_names as names, marked_table, navigation::Reselect};
    use super::*;

    fn prompt(result: CommandResult) -> PromptAction {
        match Command::try_from(result) {
            Ok(Command::OpenPrompt(action)) => action,
            other => panic!("expected an OpenPrompt, got {other:?}"),
        }
    }

    #[test]
    fn open_with_offers_the_selection_and_ignores_the_marks() {
        let (_dir, mut table) = marked_table();

        // The picker is single-path: it expands `%F`/`%U` as if one entry was
        // given, so acting on the marks would silently drop all but one.
        let result = table.open_with();

        let Ok(Command::OpenWithPrompt(path)) = Command::try_from(result) else {
            panic!("expected an OpenWithPrompt");
        };
        assert_eq!("c", path.display_name);
    }

    #[test]
    fn rename_names_the_selection_even_where_entries_are_marked() {
        let (_dir, table) = marked_table();

        // One new name cannot describe several entries, so rename is the
        // cursor's regardless of what is marked.
        let PromptAction::Rename { path, name } = prompt(table.open_rename_prompt()) else {
            panic!("expected a Rename prompt");
        };
        assert_eq!("c", path.display_name);
        assert_eq!("c", name);
    }

    #[test]
    fn delete_takes_the_marks_when_there_are_any() {
        let (_dir, mut table) = marked_table();

        let action = prompt(table.delete());

        // The count in the prompt and the paths held for the confirmation have
        // to agree, or the message names a number the delete does not act on.
        assert_eq!(PromptAction::Delete(2), action);
        assert_eq!(vec!["a", "b"], names(&table.pending_delete.paths));
    }

    #[test]
    fn delete_falls_back_to_the_cursor_when_nothing_is_marked() {
        let (_dir, mut table) = marked_table();
        table.clear_marks();

        let action = prompt(table.delete());

        assert_eq!(PromptAction::Delete(1), action);
        assert_eq!(vec!["c"], names(&table.pending_delete.paths));
    }

    #[test]
    fn the_rows_shown_as_pending_delete_are_the_ones_held_for_confirmation() {
        let (_dir, mut table) = marked_table();
        let entries = table.content.items_sorted().to_vec();

        table.delete();
        assert!(table.pending_delete.contains(&entries[0]));
        assert!(!table.pending_delete.contains(&entries[2]));

        // Confirming hands the paths off, so no row stays styled for a delete
        // that is no longer pending.
        table.pending_delete.take();
        assert!(!table.pending_delete.contains(&entries[0]));
    }

    #[test]
    fn chmod_prefills_the_mode_of_the_one_entry_it_can_read_it_from() {
        let (dir, mut table) = marked_table();
        std::fs::set_permissions(
            dir.join("c"),
            std::os::unix::fs::PermissionsExt::from_mode(0o640),
        )
        .unwrap();
        table.clear_marks();
        // Re-read, so the listing carries the mode just set.
        table.begin_directory(PathInfo::try_from(dir.path()).unwrap(), Reselect::Top);
        table
            .content
            .append(&[PathInfo::try_from(dir.join("c").as_path()).unwrap()]);
        table.finish_directory();

        let PromptAction::Chmod { paths, mode } = prompt(table.open_chmod_prompt()) else {
            panic!("expected a Chmod prompt");
        };
        assert_eq!(vec!["c"], names(&paths));
        assert_eq!("640", mode);
    }

    #[test]
    fn chmod_refuses_a_symlink_under_the_cursor_before_prompting() {
        let (dir, mut table) = marked_table();
        let link = dir.join("link");
        std::os::unix::fs::symlink(dir.join("c"), &link).unwrap();
        table.clear_marks();
        table.begin_directory(PathInfo::try_from(dir.path()).unwrap(), Reselect::Top);
        table
            .content
            .append(&[PathInfo::try_from(link.as_path()).unwrap()]);
        table.finish_directory();

        match Command::try_from(table.open_chmod_prompt()) {
            Ok(Command::AlertWarn(message)) => {
                assert!(message.starts_with("Cannot chmod"), "{message}");
                assert!(message.ends_with("it is a symlink"), "{message}");
            }
            other => panic!("expected the symlink to be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_bookmark_cannot_be_added_from_the_bookmarks_view() {
        let (dir, mut table) = marked_table();
        table
            .content
            .set_bookmarks(vec![PathInfo::try_from(dir.path()).unwrap()]);

        // The view keeps the directory it was opened from, which is not the
        // listing on screen, so a prompt would offer to bookmark a directory
        // the user is not looking at.
        assert!(matches!(
            Command::try_from(table.open_add_bookmark_prompt()),
            Ok(Command::AlertWarn(_))
        ));
    }

    #[test]
    fn chmod_leaves_the_mode_blank_for_a_marked_set() {
        let (_dir, table) = marked_table();

        // The marked entries need not share a mode, so prefilling either one
        // would offer to apply it to the rest.
        let PromptAction::Chmod { paths, mode } = prompt(table.open_chmod_prompt()) else {
            panic!("expected a Chmod prompt");
        };
        assert_eq!(vec!["a", "b"], names(&paths));
        assert_eq!("", mode);
    }
}
