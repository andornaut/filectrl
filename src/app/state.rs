use crate::{
    clipboard::{Clipboard, ClipboardCommand},
    command::mode::InputMode,
};

pub struct AppState {
    pub clipboard_command: Option<ClipboardCommand>,
    pub filter: String,
    pub mode: InputMode,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            clipboard_command: None,
            filter: String::new(),
            mode: InputMode::default(),
        }
    }
}

impl AppState {
    pub fn new() -> Self {
        // Read any pre-existing clipboard entry so the notice shows on startup
        let clipboard_command = Clipboard::default().get_clipboard_command();
        Self {
            clipboard_command,
            ..Self::default()
        }
    }
}
