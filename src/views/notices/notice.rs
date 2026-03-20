use std::collections::HashSet;

use ratatui::widgets::Block;

use super::widgets::{clipboard_widget, delete_widget, filter_widget, marked_widget, progress_widget};
use crate::{app::{clipboard::ClipboardEntry, config::theme::Theme}, command::progress::Task};

/// Represents the different types of notices that can be displayed.
/// The order of the enum variants defines the order in which notices are displayed.
#[derive(Debug)]
pub(super) enum Notice {
    Progress,
    PendingDelete(usize),
    Marked(usize),
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
                clipboard_widget(&theme.clipboard, width, clipboard_entry)
            }
            Notice::Filter(filter) => filter_widget(&theme.notice, width, filter),
            Notice::Marked(count) => marked_widget(&theme.table, width, *count),
            Notice::PendingDelete(count) => delete_widget(&theme.clipboard, width, *count),
            Notice::Progress => progress_widget(&theme.notice, width, tasks),
        }
    }
}
