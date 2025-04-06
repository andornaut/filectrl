use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::collections::HashSet;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::config::theme::Theme,
    command::task::{Progress, Task},
    file_system::path_info::PathInfo,
    utf8::truncate_left_utf8,
};

use super::ClipboardOperation;

pub(super) fn clipboard_widget<'a>(
    path: &'a PathInfo,
    operation: &'a ClipboardOperation,
    width: u16,
    theme: &Theme,
) -> Option<Paragraph<'a>> {
    let label = match operation {
        ClipboardOperation::Cut => "Cut",
        ClipboardOperation::Copy => "Copied",
    };
    let width = width.saturating_sub(label.width_cjk() as u16 + 4); // 2 for spaces + 2 for quotation marks
    let path = truncate_left_utf8(&path.path, width);
    let spans = vec![
        Span::raw(format!(" {label} \"")),
        Span::styled(path, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("\""),
    ];

    Some(Paragraph::new(Line::from(spans)).style(theme.notice_clipboard()))
}

pub(super) fn filter_widget<'a>(filter: &'a str, theme: &Theme) -> Option<Paragraph<'a>> {
    let bold_style = Style::default().add_modifier(Modifier::BOLD);
    let spans = vec![
        Span::raw(" Filtered by \""),
        Span::styled(filter, bold_style),
        Span::raw("\". Press "),
        Span::styled("Esc", bold_style),
        Span::raw(" to exit filtered mode."),
    ];
    Some(Paragraph::new(Line::from(spans)).style(theme.notice_filter()))
}

pub(super) fn progress_widget<'a>(
    tasks: &'a HashSet<Task>,
    theme: &Theme,
    width: u16,
) -> Option<Paragraph<'a>> {
    let mut progress = Progress(0, 0);
    let mut has_error = false;
    for task in tasks {
        progress = task.combine_progress(&progress);
        if task.is_error() {
            has_error = true;
        }
    }
    let current = progress.scaled(width);
    let text = "█".repeat(current as usize);
    let style = if has_error {
        theme.notice_progress_error()
    } else if progress.is_done() {
        theme.notice_progress_done()
    } else {
        theme.notice_progress()
    };
    Some(Paragraph::new(text).style(style))
}
