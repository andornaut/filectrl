use ratatui::{
    layout::Alignment,
    style::{Modifier, Style},
    symbols::block,
    text::{Line, Span},
    widgets::{Block, Borders},
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
    theme: &Theme,
    width: u16,
    path: &'a PathInfo,
    operation: &'a ClipboardOperation,
) -> Block<'a> {
    let operation_label = match operation {
        ClipboardOperation::Cut => " Cut ",
        ClipboardOperation::Copy => " Copied ",
    };
    let prefix = operation_label;
    let middle = &path.path;
    let middle_width = middle.width_cjk() as u16;
    let suffix = "(Press c to cancel)";
    let prefix_width = prefix.width_cjk() as u16;
    let suffix_width = suffix.width_cjk() as u16;

    let has_extra_width = width > prefix_width + middle_width + suffix_width;
    let mut available_path_width = width.saturating_sub(prefix_width);
    if has_extra_width {
        available_path_width = available_path_width.saturating_sub(prefix_width)
    };
    let truncated_path = truncate_left_utf8(middle, available_path_width);
    let left_title = Line::from(vec![
        Span::raw(prefix),
        Span::styled(
            truncated_path,
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]);

    let mut block = Block::default()
        .borders(Borders::NONE)
        .title(left_title)
        .style(theme.notice_clipboard());

    if has_extra_width {
        let right_title = Line::from(suffix).alignment(Alignment::Right);
        block = block.title(right_title);
    }
    block
}

pub(super) fn filter_widget<'a>(theme: &Theme, width: u16, filter: &'a str) -> Block<'a> {
    let bold_style = Style::default().add_modifier(Modifier::BOLD);

    let prefix = " Filtered by ";
    let suffix = "(Press \"Esc\" to clear)";
    let filter_width = filter.width_cjk() as u16;
    let prefix_width = prefix.width_cjk() as u16;
    let suffix_width = suffix.width_cjk() as u16;

    let has_extra_width = width > prefix_width + filter_width + suffix_width;
    let left_title = Line::from(vec![prefix.into(), Span::styled(filter, bold_style)]);
    let mut block = Block::default()
        .borders(Borders::NONE)
        .title(left_title)
        .style(theme.notice_filter());

    if has_extra_width {
        let right_title = Line::from(Span::raw(suffix)).alignment(Alignment::Right);
        block = block.title(right_title);
    }
    block
}

pub(super) fn progress_widget<'a>(
    theme: &Theme,
    width: u16,
    tasks: &'a HashSet<Task>,
) -> Block<'a> {
    let mut progress = Progress(0, 0);
    for task in tasks {
        progress = task.combine_progress(&progress);
    }

    let percent = (progress.0 as f64 / progress.1.max(1) as f64 * 100.0).round() as u32;
    let percent_text = format!(" {}%", percent);
    let percent_width = percent_text.width_cjk() as u16;

    let bar_width = width.saturating_sub(percent_width);
    let progress_width = progress.scaled(bar_width);
    let padding_width = bar_width.saturating_sub(progress_width);

    let progress_bar_text = format!(
        "{}{}",
        block::FULL.repeat(progress_width as usize),
        " ".repeat(padding_width as usize)
    );

    let left_title = Line::from(Span::raw(progress_bar_text));
    let right_title = Line::from(Span::raw(percent_text)).alignment(Alignment::Right);

    Block::default()
        .borders(Borders::NONE)
        .title(left_title)
        .title(right_title)
        .style(theme.notice_progress())
}
