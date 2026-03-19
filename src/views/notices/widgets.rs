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
    app::{clipboard::ClipboardEntry, config::theme::Theme},
    command::progress::{Progress, Task},
    views::unicode::truncate_left,
};

const COPY_PREFIX: &str = "[Copy] ";
const MARKED_PREFIX: &str = "[Marked] ";
const MOVE_PREFIX: &str = "[Cut] ";
const ESC_SUFFIX: &str = "(Press \"Esc\" to clear)";
const FILTER_PREFIX: &str = " Filtered by ";

pub(super) fn clipboard_widget<'a>(
    theme: &Theme,
    width: u16,
    clipboard_entry: &'a ClipboardEntry,
) -> Block<'a> {
    let paths = clipboard_entry.paths();
    let prefix = match clipboard_entry {
        ClipboardEntry::Move(_) => MOVE_PREFIX,
        ClipboardEntry::Copy(_) => COPY_PREFIX,
    };

    let style = match clipboard_entry {
        ClipboardEntry::Copy(_) => theme.clipboard.copy(),
        ClipboardEntry::Move(_) => theme.clipboard.cut(),
    };

    let (detail, detail_width) = if paths.len() > 1 {
        let text = format!("{} items", paths.len());
        let w = text.width();
        (text, w)
    } else {
        let available_width = width.saturating_sub(prefix.width() as u16);
        let truncated = truncate_left(&paths[0].path, available_width as usize);
        let w = truncated.width();
        (truncated, w)
    };

    let left = Line::from(vec![
        Span::styled(prefix, style.add_modifier(Modifier::BOLD)),
        Span::styled(detail, style),
    ]);

    let right = if width > (prefix.width() + detail_width + ESC_SUFFIX.width()) as u16 {
        Some(Line::from(ESC_SUFFIX))
    } else {
        None
    };

    create_notice_block(left, right, style)
}

pub(super) fn marked_widget(theme: &Theme, count: usize) -> Block<'_> {
    let style = theme.table.marked();
    let left = Line::from(vec![
        Span::styled(MARKED_PREFIX, style.add_modifier(Modifier::BOLD)),
        Span::styled(format!("{} items", count), style),
    ]);
    create_notice_block(left, None, style)
}

pub(super) fn filter_widget<'a>(theme: &Theme, width: u16, filter: &'a str) -> Block<'a> {
    let left = Line::from(vec![
        FILTER_PREFIX.into(),
        Span::styled(filter, theme.notice.filter().add_modifier(Modifier::BOLD)),
    ]);

    let right = if width > (FILTER_PREFIX.width() + filter.width() + ESC_SUFFIX.width()) as u16 {
        Some(Line::from(ESC_SUFFIX))
    } else {
        None
    };

    create_notice_block(left, right, theme.notice.filter())
}

pub(super) fn progress_widget<'a>(
    theme: &Theme,
    width: u16,
    tasks: &'a HashSet<Task>,
) -> Block<'a> {
    // Combine the progress from all the tasks
    let progress = tasks
        .iter()
        .fold(Progress::default(), |acc, task| task.combine_progress(&acc));

    let percentage = progress.percentage();
    let percentage_text = format!(" {}%", percentage);
    let bar_width = width.saturating_sub(percentage_text.width() as u16);
    let progress_width = progress.scaled(bar_width);

    let filled = block::FULL.repeat(progress_width.into());
    let empty = " ".repeat(bar_width.saturating_sub(progress_width).into());
    let progress_bar = format!("{}{}", filled, empty);

    let left = Line::from(progress_bar);
    let right = Some(Line::from(percentage_text));

    create_notice_block(left, right, theme.notice.progress())
}

fn create_notice_block<'a>(left: Line<'a>, right: Option<Line<'a>>, style: Style) -> Block<'a> {
    let block = Block::default()
        .borders(Borders::NONE)
        .title(left)
        .style(style);

    match right {
        Some(right_text) => block.title(right_text.alignment(Alignment::Right)),
        None => block,
    }
}
