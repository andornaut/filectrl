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

    fn set_clipboard(
        &mut self,
        make_entry: fn(Vec<PathInfo>) -> ClipboardEntry,
    ) -> CommandResult {
        let result = if self.has_marks() {
            Command::SetClipboard(make_entry(self.marked_paths())).into()
        } else {
            match self.selected_path() {
                None => return Command::AlertWarn("No file selected".into()).into(),
                Some(path) => Command::SetClipboard(make_entry(vec![path.clone()])).into(),
            }
        };
        self.clear_marks();
        result
    }

    pub(super) fn paste_from_clipboard(&self) -> CommandResult {
        let destination = self.directory.as_ref().expect("Directory is always set");
        Command::Paste(destination.clone()).into()
    }
}
