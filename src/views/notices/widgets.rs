use std::collections::HashSet;

use ratatui::{
    layout::Alignment,
    style::{Modifier, Style},
    symbols::block,
    text::{Line, Span},
    widgets::{Block, Borders},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::config::theme::Theme,
    clipboard::ClipboardCommand,
    command::task::{Progress, Task},
    utf8::truncate_left_utf8,
};

const MOVE_PREFIX: &str = " Cut ";
const COPY_PREFIX: &str = " Copied ";
const CLIPBOARD_SUFFIX: &str = "(Press c to cancel)";
const FILTER_PREFIX: &str = " Filtered by ";
const FILTER_SUFFIX: &str = "(Press \"Esc\" to clear)";

pub(super) fn clipboard_widget<'a>(
    theme: &Theme,
    width: u16,
    clipboard_command: &'a ClipboardCommand,
) -> Block<'a> {
    let (prefix, path) = match clipboard_command {
        ClipboardCommand::Move(path) => (MOVE_PREFIX, path),
        ClipboardCommand::Copy(path) => (COPY_PREFIX, path),
    };

    let available_width = width.saturating_sub(prefix.width() as u16);
    let truncated_path = truncate_left_utf8(&path.path, available_width);

    let left = Line::from(vec![
        Span::raw(prefix),
        Span::styled(
            truncated_path,
            theme.notice_clipboard().add_modifier(Modifier::BOLD),
        ),
    ]);

    let right = if width > (prefix.width() + path.path.width() + CLIPBOARD_SUFFIX.width()) as u16 {
        Some(Line::from(CLIPBOARD_SUFFIX))
    } else {
        None
    };

    create_notice_block(left, right, theme.notice_clipboard())
}

pub(super) fn filter_widget<'a>(theme: &Theme, width: u16, filter: &'a str) -> Block<'a> {
    let left = Line::from(vec![
        FILTER_PREFIX.into(),
        Span::styled(filter, theme.notice_filter().add_modifier(Modifier::BOLD)),
    ]);

    let right = if width > (FILTER_PREFIX.width() + filter.width() + FILTER_SUFFIX.width()) as u16 {
        Some(Line::from(FILTER_SUFFIX))
    } else {
        None
    };

    create_notice_block(left, right, theme.notice_filter())
}

pub(super) fn progress_widget<'a>(
    theme: &Theme,
    width: u16,
    tasks: &'a HashSet<Task>,
) -> Block<'a> {
    let progress = tasks
        .iter()
        .fold(Progress(0, 0), |acc, task| task.combine_progress(&acc));
    let percent = (progress.0 as f64 / progress.1.max(1) as f64 * 100.0).round() as u32;
    let percent_text = format!(" {}%", percent);
    let bar_width = width.saturating_sub(percent_text.width() as u16);
    let progress_width = progress.scaled(bar_width);
    let progress_bar = format!(
        "{}{}",
        block::FULL.repeat(progress_width.into()),
        " ".repeat((bar_width - progress_width).into())
    );

    let left = Line::from(progress_bar);
    let right = Some(Line::from(percent_text));

    create_notice_block(left, right, theme.notice_progress())
}
fn create_notice_block<'a>(left: Line<'a>, right: Option<Line<'a>>, style: Style) -> Block<'a> {
    let mut block = Block::default()
        .borders(Borders::NONE)
        .title(left)
        .style(style);

    if let Some(right) = right {
        block = block.title(right.alignment(Alignment::Right));
    }
    block
}
