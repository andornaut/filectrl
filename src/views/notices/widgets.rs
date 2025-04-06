use ratatui::{
    style::{Modifier, Style},
    symbols::block,
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
) -> Paragraph<'a> {
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

    Paragraph::new(Line::from(spans)).style(theme.notice_clipboard())
}

pub(super) fn filter_widget<'a>(filter: &'a str, theme: &Theme) -> Paragraph<'a> {
    let bold_style = Style::default().add_modifier(Modifier::BOLD);
    let spans = vec![
        Span::raw(" Filtered by \""),
        Span::styled(filter, bold_style),
        Span::raw("\". Press "),
        Span::styled("Esc", bold_style),
        Span::raw(" to exit filtered mode."),
    ];
    Paragraph::new(Line::from(spans)).style(theme.notice_filter())
}

pub(super) fn progress_widget<'a>(
    tasks: &'a HashSet<Task>,
    theme: &Theme,
    width: u16,
) -> Paragraph<'a> {
    let mut progress = Progress(0, 0);
    for task in tasks {
        progress = task.combine_progress(&progress);
    }

    let percent = (progress.0 as f64 / progress.1.max(1) as f64 * 100.0).round() as u32;
    let percent_text = format!("{}%", percent);
    let percent_width = percent_text.width_cjk() as u16;

    let bar_width = width.saturating_sub(percent_width);
    let progress_width = progress.scaled(bar_width);
    let padding_width = bar_width - progress_width;

    let progress_text = block::FULL.repeat(progress_width as usize);
    let padding_text = " ".repeat(padding_width as usize);
    let text = format!("{}{}{}", progress_text, padding_text, percent_text);

    Paragraph::new(text).style(theme.notice_progress())
}
