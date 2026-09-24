use super::{TableView, style::PathSet};
use crate::{
    command::{Command, ForegroundProgram, PromptAction, result::CommandResult},
    file_system::{
        home_directory,
        path_info::{PathInfo, compact},
    },
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
        match home_directory() {
            Ok(path) => Command::Open(path).into(),
            Err(error) => error.into(),
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

    pub(super) fn open_create_directory_prompt(&self) -> CommandResult {
        // As for a paste: the directory would be made behind the bookmarks,
        // in a listing that is not on screen.
        if self.content.is_showing_bookmarks() {
            return Command::AlertWarn("Cannot create a directory from the bookmarks view".into())
                .into();
        }
        Command::OpenPrompt(PromptAction::CreateDirectory).into()
    }

    pub(super) fn open_filter_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptAction::Filter(self.content.filter().to_string())).into()
    }

    pub(super) fn open_rename_prompt(&self) -> CommandResult {
        match self.selected_path() {
            None => Command::AlertWarn("No file selected".into()).into(),
            Some(path) => Command::OpenPrompt(PromptAction::Rename {
                path: path.clone(),
                name: editable_name(path),
            })
            .into(),
        }
    }

    pub(super) fn open_add_bookmark_prompt(&self) -> CommandResult {
        if self.content.is_showing_bookmarks() {
            return Command::AlertWarn("Cannot add a bookmark from the bookmarks view".into())
                .into();
        }
        match self.content.directory() {
            None => Command::AlertWarn("No current directory".into()).into(),
            Some(directory) => Command::OpenPrompt(PromptAction::AddBookmark {
                directory: directory.clone(),
                name: editable_name(directory),
            })
            .into(),
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

    /// Opens the cursor's entry in the editor or pager, which show one file, so
    /// this ignores the marks like `open_with`. A directory, or a link to one,
    /// is refused rather than handed to a program that expects a file, and so
    /// is a broken link, whose program would fail with a message the redraw
    /// on return erases.
    pub(super) fn run_in_foreground(&mut self, program: ForegroundProgram) -> CommandResult {
        let Some(path) = self.selected_path() else {
            return CommandResult::Handled;
        };
        let reason = if path.path.is_dir() {
            Some("it is a directory")
        } else if path.path.is_symlink() && !path.path.exists() {
            Some("its target does not exist")
        } else {
            None
        };
        if let Some(reason) = reason {
            let verb = match program {
                ForegroundProgram::Editor => "edit",
                ForegroundProgram::Pager => "page",
            };
            return Command::AlertWarn(format!("Cannot {verb} {}: {reason}", compact(&path.path)))
                .into();
        }
        Command::RunInForeground {
            program,
            path: path.clone(),
        }
        .into()
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

/// The text a prompt offers for editing `path`'s name: the name itself rather
/// than `display_name`, which spells out disguising characters, so submitting
/// the prompt unchanged does not store the escaped form.
#[allow(clippy::disallowed_methods)]
fn editable_name(path: &PathInfo) -> String {
    path.path
        .file_name()
        .map_or(String::new(), |name| name.to_string_lossy().into_owned())
}

/// Which entries each action acts on. Every action here reads either the marks
/// or the cursor, and nothing about the call site says which, so the choice is
/// pinned per action rather than left to the reader.
#[cfg(test)]
mod tests {
    use test_case::test_case;

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

    /// An editor or pager shows one file, so both take the cursor's entry.
    #[test_case(ForegroundProgram::Editor ; "edit")]
    #[test_case(ForegroundProgram::Pager ; "page")]
    fn edit_and_page_take_the_selection_and_ignore_the_marks(program: ForegroundProgram) {
        let (_dir, mut table) = marked_table();

        let result = table.run_in_foreground(program);

        let Ok(Command::RunInForeground { program: run, path }) = Command::try_from(result) else {
            panic!("expected RunInForeground");
        };
        assert_eq!(program, run);
        assert_eq!("c", path.display_name);
    }

    #[test_case(ForegroundProgram::Editor, "sub" => "Cannot edit" ; "edit a directory")]
    #[test_case(ForegroundProgram::Pager, "sub" => "Cannot page" ; "page a directory")]
    #[test_case(ForegroundProgram::Editor, "link" => "Cannot edit" ; "edit a symlink to a directory")]
    #[test_case(ForegroundProgram::Pager, "link" => "Cannot page" ; "page a symlink to a directory")]
    fn edit_and_page_refuse_a_directory(program: ForegroundProgram, entry: &str) -> String {
        use crate::{app::config::Config, test_support::TempDir};

        Config::init_test();
        let dir = TempDir::new("table_page_directory");
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::os::unix::fs::symlink("sub", dir.join("link")).unwrap();
        let mut table = TableView::default();
        table.begin_directory(PathInfo::try_from(dir.path()).unwrap(), Reselect::Top);
        table
            .content
            .append(&[PathInfo::try_from(dir.join(entry).as_path()).unwrap()]);
        table.finish_directory();
        table.select(0);

        match Command::try_from(table.run_in_foreground(program)) {
            Ok(Command::AlertWarn(message)) => {
                assert!(message.ends_with(": it is a directory"), "{message}");
                message.split(' ').take(2).collect::<Vec<_>>().join(" ")
            }
            other => panic!("expected a warning, got {other:?}"),
        }
    }

    #[test_case(ForegroundProgram::Editor ; "edit")]
    #[test_case(ForegroundProgram::Pager ; "page")]
    fn edit_and_page_refuse_a_broken_symlink(program: ForegroundProgram) {
        use crate::{app::config::Config, test_support::TempDir};

        Config::init_test();
        let dir = TempDir::new("table_page_broken_link");
        std::os::unix::fs::symlink("missing", dir.join("broken")).unwrap();
        let mut table = TableView::default();
        table.begin_directory(PathInfo::try_from(dir.path()).unwrap(), Reselect::Top);
        table
            .content
            .append(&[PathInfo::try_from(dir.join("broken").as_path()).unwrap()]);
        table.finish_directory();
        table.select(0);

        match Command::try_from(table.run_in_foreground(program)) {
            Ok(Command::AlertWarn(message)) => {
                assert!(message.starts_with("Cannot "), "{message}");
                assert!(
                    message.ends_with(": its target does not exist"),
                    "{message}"
                );
            }
            other => panic!("expected a warning, got {other:?}"),
        }
    }

    /// An empty listing has no cursor, so nothing can be marked, and delete
    /// and copy find no entry to act on.
    #[test]
    fn an_empty_listing_offers_nothing_to_mark_delete_or_copy() {
        use crate::{app::config::Config, test_support::TempDir};

        Config::init_test();
        let dir = TempDir::new("table_empty_listing");
        let mut table = TableView::default();
        table.begin_directory(PathInfo::try_from(dir.path()).unwrap(), Reselect::Top);
        table.finish_directory();
        table.select_first();

        table.toggle_mark();
        assert_eq!(0, table.marks.len());
        table.enter_range_mode();
        table.select_last();
        assert_eq!(0, table.marks.len());
        assert!(!table.marks.in_range_mode());

        assert!(matches!(table.delete(), CommandResult::Handled));
        assert!(table.pending_delete.paths.is_empty());
        assert!(matches!(
            Command::try_from(table.copy_to_clipboard()),
            Ok(Command::AlertWarn(_))
        ));
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

    /// The table shows the escaped form; the prompt must hold the real name.
    #[test]
    fn rename_starts_from_the_name_rather_than_its_escaped_form() {
        use crate::{app::config::Config, test_support::TempDir};

        Config::init_test();
        let dir = TempDir::new("table_rename_disguised");
        let path = dir.join("a\u{202e}b");
        std::fs::write(&path, b"x").unwrap();
        let mut table = TableView::default();
        table.begin_directory(PathInfo::try_from(dir.path()).unwrap(), Reselect::Top);
        table
            .content
            .append(&[PathInfo::try_from(path.as_path()).unwrap()]);
        table.finish_directory();
        table.select(0);

        let PromptAction::Rename { path, name } = prompt(table.open_rename_prompt()) else {
            panic!("expected a Rename prompt");
        };
        assert_eq!("a\\u{202e}b", path.display_name);
        assert_eq!("a\u{202e}b", name);
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

    /// Each names the directory behind the bookmarks, which is not the
    /// listing on screen. Outside the view both go ahead.
    #[test_case(TableView::open_add_bookmark_prompt, "Cannot add a bookmark from the bookmarks view" ; "add a bookmark")]
    #[test_case(TableView::paste_from_clipboard, "Cannot paste into the bookmarks view" ; "paste")]
    #[test_case(TableView::open_create_directory_prompt, "Cannot create a directory from the bookmarks view" ; "create a directory")]
    fn an_action_on_the_hidden_directory_is_refused_in_the_bookmarks_view(
        act: fn(&TableView) -> CommandResult,
        warning: &str,
    ) {
        let (dir, mut table) = marked_table();
        assert!(
            !matches!(Command::try_from(act(&table)), Ok(Command::AlertWarn(_))),
            "allowed outside the bookmarks view"
        );
        table
            .content
            .set_bookmarks(vec![PathInfo::try_from(dir.path()).unwrap()]);

        assert_eq!(
            Ok(Command::AlertWarn(warning.to_string())),
            Command::try_from(act(&table)).map_err(|_| ())
        );
    }

    /// As for rename: the bookmark is named by what the prompt holds, so it
    /// must start from the directory's real name, not the escaped one shown.
    #[test]
    fn add_bookmark_starts_from_the_name_rather_than_its_escaped_form() {
        let (dir, mut table) = marked_table();
        let path = dir.join("a\tb");
        std::fs::create_dir(&path).unwrap();
        table.begin_directory(PathInfo::try_from(path.as_path()).unwrap(), Reselect::Top);
        table.finish_directory();

        let PromptAction::AddBookmark { directory, name } =
            prompt(table.open_add_bookmark_prompt())
        else {
            panic!("expected an AddBookmark prompt");
        };
        assert_eq!("a\\tb", directory.display_name);
        assert_eq!("a\tb", name);
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
