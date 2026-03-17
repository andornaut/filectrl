use crate::command::mode::InputMode;
use super::clipboard::{Clipboard, ClipboardEntry};

pub struct AppState {
    pub clipboard_entry: Option<ClipboardEntry>,
    pub filter: String,
    pub is_help_visible: bool,
    pub mode: InputMode,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            clipboard_entry: None,
            filter: String::new(),
            is_help_visible: false,
            mode: InputMode::default(),
        }
    }
}

impl AppState {
    pub fn new() -> Self {
        // Read any pre-existing clipboard entry so the notice shows on startup
        let clipboard_entry = Clipboard::default().get_clipboard_entry();
        Self {
            clipboard_entry,
            ..Self::default()
        }
    }
}
