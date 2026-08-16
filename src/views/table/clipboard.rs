use super::TableView;
use crate::{
    app::clipboard::ClipboardEntry,
    command::{Command, result::CommandResult},
    file_system::path_info::PathInfo,
};

impl TableView {
    pub(super) fn copy_to_clipboard(&mut self) -> CommandResult {
        self.set_clipboard(ClipboardEntry::Copy)
    }

    pub(super) fn cut_to_clipboard(&mut self) -> CommandResult {
        self.set_clipboard(ClipboardEntry::Move)
    }

    fn set_clipboard(&mut self, make_entry: fn(Vec<PathInfo>) -> ClipboardEntry) -> CommandResult {
        if self.has_marks() {
            Command::SetClipboardEntry(Some(make_entry(self.marked_paths()))).into()
        } else {
            match self.selected_path() {
                None => Command::AlertWarn("No file selected".into()).into(),
                Some(path) => {
                    Command::SetClipboardEntry(Some(make_entry(vec![path.clone()]))).into()
                }
            }
        }
    }

    pub(super) fn paste_from_clipboard(&self) -> CommandResult {
        let destination = self.content.directory().expect("Directory is always set");
        Command::Paste(destination.clone()).into()
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::super::{TableView, display_names, marked_table};
    use super::*;

    fn entry(result: CommandResult) -> ClipboardEntry {
        match Command::try_from(result) {
            Ok(Command::SetClipboardEntry(Some(entry))) => entry,
            other => panic!("expected SetClipboardEntry(Some(_)), got {other:?}"),
        }
    }

    /// Copy and cut differ only in the variant they build, and the variant is
    /// what the paste reads to decide whether to remove the source.
    #[test_case(TableView::copy_to_clipboard, ClipboardEntry::Copy as fn(_) -> _ ; "copy")]
    #[test_case(TableView::cut_to_clipboard, ClipboardEntry::Move as fn(_) -> _ ; "cut")]
    fn the_clipboard_takes_the_marks_when_there_are_any(
        act: fn(&mut TableView) -> CommandResult,
        expected: fn(Vec<PathInfo>) -> ClipboardEntry,
    ) {
        let (_dir, mut table) = marked_table();

        let entry = entry(act(&mut table));

        assert_eq!(
            std::mem::discriminant(&expected(Vec::new())),
            std::mem::discriminant(&entry)
        );
        assert_eq!(vec!["a", "b"], display_names(entry.paths()));
    }

    #[test]
    fn the_clipboard_falls_back_to_the_cursor_when_nothing_is_marked() {
        let (_dir, mut table) = marked_table();
        table.clear_marks();

        let entry = entry(table.copy_to_clipboard());

        assert_eq!(vec!["c"], display_names(entry.paths()));
    }

    #[test]
    fn an_empty_listing_warns_rather_than_setting_an_empty_clipboard() {
        let (_dir, mut table) = marked_table();
        table.clear_marks();
        table.begin_directory(
            PathInfo::try_from("/tmp").unwrap(),
            super::super::navigation::Reselect::Top,
        );
        table.finish_directory();

        // An entry naming nothing would clear the clipboard on the next paste
        // while reading as a successful copy.
        assert!(matches!(
            Command::try_from(table.copy_to_clipboard()),
            Ok(Command::AlertWarn(_))
        ));
    }
}
