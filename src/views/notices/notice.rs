use std::collections::HashSet;

use ratatui::widgets::Block;

use super::widgets::{clipboard_widget, filter_widget, progress_widget};
use crate::{app::config::theme::Theme, clipboard::ClipboardEntry, command::task::Task};

/// Represents the different types of notices that can be displayed.
/// The order of the enum variants defines the order in which notices are displayed.
#[derive(Debug)]
pub(super) enum Notice {
    Progress,
    Clipboard(ClipboardEntry),
    Filter(String),
}

impl Notice {
    pub(super) fn create_widget<'a>(
        &'a self,
        theme: &'a Theme,
        width: u16,
        tasks: &'a HashSet<Task>,
    ) -> Block<'a> {
        match self {
            Notice::Clipboard(clipboard_entry) => {
                clipboard_widget(theme, width, clipboard_entry)
            }
            Notice::Filter(filter) => filter_widget(theme, width, filter),
            Notice::Progress => progress_widget(theme, width, tasks),
        }
    }
}
