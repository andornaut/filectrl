use crate::{command::mode::InputMode, file_system::path_info::PathInfo};
use super::clipboard::{Clipboard, ClipboardEntry};

#[derive(Default)]
pub struct AppState {
    pub clipboard_entry: Option<ClipboardEntry>,
    pub filter: String,
    pub is_help_visible: bool,
    pub mode: InputMode,
    pub selected: Option<PathInfo>,
}


impl AppState {
    pub(crate) fn new(clipboard: &Clipboard) -> Self {
        // Read any pre-existing clipboard entry so the notice shows on startup
        let clipboard_entry = clipboard.get_clipboard_entry();
        Self {
            clipboard_entry,
            ..Self::default()
        }
    }
}
