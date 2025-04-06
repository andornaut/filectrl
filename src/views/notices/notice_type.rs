use std::collections::HashSet;

use ratatui::widgets::Paragraph;

use super::{
    widgets::{clipboard_widget, filter_widget, progress_widget},
    ClipboardOperation,
};
use crate::{app::config::theme::Theme, command::task::Task, file_system::path_info::PathInfo};

/// Represents the different types of notices that can be displayed.
/// The order of the enum variants defines the order in which notices are displayed.
#[derive(Debug, Clone)]
pub enum NoticeType<'a> {
    Progress,
    Clipboard((&'a ClipboardOperation, &'a PathInfo)),
    Filter(&'a str),
}

impl<'a> NoticeType<'a> {
    pub(super) fn create_widget<'b>(
        &'b self,
        theme: &'b Theme,
        width: u16,
        tasks: &'b HashSet<Task>,
    ) -> Paragraph<'b> {
        match self {
            NoticeType::Progress => progress_widget(theme, width, tasks),
            NoticeType::Clipboard((operation, path)) => {
                clipboard_widget(theme, width, path, operation)
            }
            NoticeType::Filter(filter) => filter_widget(theme, width, filter),
        }
    }
}
