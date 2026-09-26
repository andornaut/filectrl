use super::{TableView, style::PathSet};
use crate::{
    command::{Command, ForegroundProgram, PromptAction, result::CommandResult},
    file_system::{
        home_directory,
        path_info::{PathInfo, compact},
    },
};

/// The entries a delete prompt is waiting on.
#[derive(Default)]
pub(super) struct PendingDelete {
    paths: Vec<PathInfo>,
    index: PathSet,
}

impl PendingDelete {
    /// The display name of the one entry pending, or `None` for several.
    pub(super) fn only_name(&self) -> Option<String> {
        match self.paths.as_slice() {
            [path] => Some(path.display_name.clone()),
            _ => None,
        }
    }

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

/// What an operation on the selection acts on.
pub(super) enum Targets<'a> {
    Marked(Vec<PathInfo>),
    Cursor(&'a PathInfo),
}

impl Targets<'_> {
    pub(super) fn into_paths(self) -> Vec<PathInfo> {
        match self {
            Targets::Marked(paths) => paths,
            Targets::Cursor(path) => vec![path.clone()],
        }
    }
}

impl TableView {
    /// The marked entries, else the entry under the cursor. A mark with no
    /// entry under it does not count.
    pub(super) fn targets(&self) -> Option<Targets<'_>> {
        let marked = self.marked_paths();
        if marked.is_empty() {
            self.selected_path().map(Targets::Cursor)
        } else {
            Some(Targets::Marked(marked))
        }
    }

    pub(super) fn delete(&mut self) -> CommandResult {
        let Some(targets) = self.targets() else {
            return CommandResult::Handled;
        };
        let paths = targets.into_paths();
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
        let (paths, initial_mode) = match self.targets() {
            Some(Targets::Marked(marked)) => (marked, String::new()),
            // chmod would apply to the target, not the symlink.
            Some(Targets::Cursor(path)) if path.is_symlink() => {
                return Command::AlertWarn(format!(
                    "Cannot chmod {}: it is a symlink",
                    compact(&path.path)
                ))
                .into();
            }
            Some(Targets::Cursor(path)) => {
                let mode = format!("{:o}", path.mode() & 0o7777);
                (vec![path.clone()], mode)
            }
            None => {
                return Command::AlertWarn("Cannot chmod: nothing is selected".into()).into();
            }
        };
        Command::OpenPrompt(PromptAction::Chmod {
            paths,
            mode: initial_mode,
        })
        .into()
    }

    pub(super) fn open_create_directory_prompt(&self) -> CommandResult {
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
            None => Command::AlertWarn("Cannot rename: nothing is selected".into()).into(),
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
            None => {
                Command::AlertWarn("Cannot add a bookmark: there is no current directory".into())
                    .into()
            }
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

    /// Opens the cursor's entry (never the marks) in the editor or pager.
    /// Directories and broken links are refused.
    pub(super) fn run_in_foreground(&mut self, program: ForegroundProgram) -> CommandResult {
        let Some(path) = self.selected_path() else {
            return CommandResult::Handled;
        };
        let reason = if path.path.is_dir() {
            Some("it is a directory")
        } else if path.is_symlink_broken() {
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

    /// The picker offers applications for one path, so this ignores marks.
    pub(super) fn open_with(&mut self) -> CommandResult {
        match self.selected_path() {
            Some(path) => Command::OpenWithPrompt(path.clone()).into(),
            None => CommandResult::Handled,
        }
    }
}

/// The real name for an editable prompt, not the escaped `display_name`.
#[allow(clippy::disallowed_methods)]
fn editable_name(path: &PathInfo) -> String {
    path.path
        .file_name()
        .map_or(String::new(), |name| name.to_string_lossy().into_owned())
}

/// Which entries each action acts on: the marks or the cursor.
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

    #[test_case("copy" ; "copy")]
    #[test_case("cut" ; "cut")]
    #[test_case("chmod" ; "chmod")]
    #[test_case("rename" ; "rename")]
    fn an_action_with_nothing_selected_says_so(verb: &str) {
        let mut table = TableView::default();

        let result = match verb {
            "copy" => table.copy_to_clipboard(),
            "cut" => table.cut_to_clipboard(),
            "chmod" => table.open_chmod_prompt(),
            "rename" => table.open_rename_prompt(),
            _ => unreachable!(),
        };

        assert_eq!(
            CommandResult::from(Command::AlertWarn(format!(
                "Cannot {verb}: nothing is selected"
            ))),
            result
        );
    }

    #[test]
    fn open_with_offers_the_selection_and_ignores_the_marks() {
        let (_dir, mut table) = marked_table();

        let result = table.open_with();

        let Ok(Command::OpenWithPrompt(path)) = Command::try_from(result) else {
            panic!("expected an OpenWithPrompt");
        };
        assert_eq!("c", path.display_name);
    }

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

        let PromptAction::Rename { path, name } = prompt(table.open_rename_prompt()) else {
            panic!("expected a Rename prompt");
        };
        assert_eq!("c", path.display_name);
        assert_eq!("c", name);
    }

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
    fn a_mark_past_the_end_of_the_listing_is_not_a_selection() {
        let (_dir, mut table) = marked_table();
        table.clear_marks();
        table.marks.insert(99);

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

        let PromptAction::Chmod { paths, mode } = prompt(table.open_chmod_prompt()) else {
            panic!("expected a Chmod prompt");
        };
        assert_eq!(vec!["a", "b"], names(&paths));
        assert_eq!("", mode);
    }
}
