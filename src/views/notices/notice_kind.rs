use std::collections::HashSet;

use ratatui::widgets::Block;

use super::widgets::{clipboard_widget, filter_widget, progress_widget};
use crate::{app::config::theme::Theme, clipboard::ClipboardCommand, command::task::Task};

/// Represents the different types of notices that can be displayed.
/// The order of the enum variants defines the order in which notices are displayed.
#[derive(Debug)]
pub(super) enum NoticeKind<'a> {
    Progress,
    Clipboard(&'a ClipboardCommand),
    Filter(&'a str),
}

impl<'a> NoticeKind<'a> {
    pub(super) fn create_widget<'b>(
        &'b self,
        theme: &'b Theme,
        width: u16,
        tasks: &'b HashSet<Task>,
    ) -> Block<'b> {
        match self {
            NoticeKind::Clipboard(clipboard_command) => {
                clipboard_widget(theme, width, clipboard_command)
            }
            NoticeKind::Filter(filter) => filter_widget(theme, width, filter),
            NoticeKind::Progress => progress_widget(theme, width, tasks),
        }
    }
}
