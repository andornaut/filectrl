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
    app::{
        clipboard::ClipboardEntry,
        config::theme::{Clipboard, Notice as NoticeTheme, Table},
    },
    command::progress::{Progress, Task},
    views::unicode::{pluralize_items, truncate_left},
};

const COPY_PREFIX: &str = "[Copy] ";
const MARKED_PREFIX: &str = "[Selected] ";
const MOVE_PREFIX: &str = "[Cut] ";
const FILTER_PREFIX: &str = "[Filtered] ";
const SEARCH_PREFIX: &str = "[Searching...] ";

pub(super) fn clipboard_widget<'a>(
    theme: &Clipboard,
    width: u16,
    clipboard_entry: &'a ClipboardEntry,
    hint: &'a str,
) -> Block<'a> {
    let paths = clipboard_entry.paths();
    let prefix = match clipboard_entry {
        ClipboardEntry::Move(_) => MOVE_PREFIX,
        ClipboardEntry::Copy(_) => COPY_PREFIX,
    };

    let style = match clipboard_entry {
        ClipboardEntry::Copy(_) => theme.copy(),
        ClipboardEntry::Move(_) => theme.cut(),
    };

    let (detail, _) = if paths.len() > 1 {
        let text = pluralize_items(paths.len());
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

    create_notice_block(left, style, width, hint)
}

pub(super) fn marked_widget<'a>(
    theme: &Table,
    width: u16,
    count: usize,
    hint: &'a str,
) -> Block<'a> {
    let style = theme.marked();
    let left = Line::from(vec![
        Span::styled(MARKED_PREFIX, style.add_modifier(Modifier::BOLD)),
        Span::styled(pluralize_items(count), style),
    ]);
    create_notice_block(left, style, width, hint)
}

pub(super) fn filter_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    filter: &'a str,
    hint: &'a str,
) -> Block<'a> {
    let left = Line::from(vec![
        FILTER_PREFIX.into(),
        Span::styled(filter, theme.filter().add_modifier(Modifier::BOLD)),
    ]);
    create_notice_block(left, theme.filter(), width, hint)
}

pub(super) fn progress_widget<'a>(
    theme: &NoticeTheme,
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
    let right = Line::from(percentage_text).alignment(Alignment::Right);

    Block::default()
        .borders(Borders::NONE)
        .title(left)
        .title(right)
        .style(theme.progress())
}

pub(super) fn search_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    query: &'a str,
    hint: &'a str,
) -> Block<'a> {
    let style = theme.search();
    let left = Line::from(vec![
        SEARCH_PREFIX.into(),
        Span::styled(query, style.add_modifier(Modifier::BOLD)),
    ]);
    create_notice_block(left, style, width, hint)
}

pub(super) fn search_indicator_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    search_tick: u16,
) -> Block<'a> {
    let style = theme.search_loading();
    let block_width: u16 = 3;
    if width <= block_width {
        return Block::default().borders(Borders::NONE).style(style);
    }

    // Triangle wave: position bounces 0 → travel → 0
    let travel = width - block_width;
    let cycle = travel * 2;
    let t = search_tick % cycle;
    let pos = if t < travel { t } else { cycle - t };

    let before = " ".repeat(pos as usize);
    let indicator = block::FULL.repeat(block_width as usize);
    let after = " ".repeat(width.saturating_sub(pos + block_width) as usize);

    let left = Line::from(format!("{before}{indicator}{after}"));

    Block::default()
        .borders(Borders::NONE)
        .title(left)
        .style(style)
}

fn create_notice_block<'a>(left: Line<'a>, style: Style, width: u16, hint: &'a str) -> Block<'a> {
    let left_width = left.width();
    let block = Block::default()
        .borders(Borders::NONE)
        .title(left)
        .style(style);

    if width as usize > left_width + hint.width() {
        let right = Line::from(hint).alignment(Alignment::Right);
        block.title(right)
    } else {
        block
    }
}
