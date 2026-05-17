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
    command::progress::{Progress, Task, TaskKind},
    views::unicode::{pluralize_items, truncate_left},
};

const COPY_PREFIX: &str = "[Copy] ";
const MARKED_PREFIX: &str = "[Selected] ";
const MOVE_PREFIX: &str = "[Cut] ";
const FILTER_PREFIX: &str = "[Filtered] ";
const SEARCH_PREFIX: &str = "[Searching...] ";

// Number of terminal columns per unit of search-loading indicator speed.
// The indicator advances `width / SEARCH_LOADING_SPEED_DIVISOR` cells per
// tick, so wider screens sweep faster instead of taking longer to cross.
const SEARCH_LOADING_SPEED_DIVISOR: u16 = 32;

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

// truncate_left() panics unless the budget exceeds the ellipsis width (1).
const MIN_TRUNCATE_WIDTH: usize = 2;

/// The detail text shown after the verb prefix, left-truncated to fit the
/// width left over after the (always shown in full) prefix. Left-truncation
/// keeps the tail of the path visible (e.g. `…naut/Downloads/`), which is the
/// most relevant part for the user. When there is no room for any detail
/// (budget below the minimum), only an ellipsis is shown (e.g. `Copying …`).
fn truncate_detail(prefix: &str, detail: &str, width: u16) -> String {
    let budget = (width as usize).saturating_sub(prefix.width());
    if budget < MIN_TRUNCATE_WIDTH {
        "…".to_string()
    } else if detail.width() <= budget {
        detail.to_string()
    } else {
        truncate_left(detail, budget)
    }
}

/// The detail string for a single in-progress operation, chosen to keep the
/// most useful information visible as the width shrinks:
/// - normally `"<source> to <destination dir>"`
/// - if the source basename would be truncated at all, switch to
///   `"to <full destination path including basename>"` so the file name is
///   still shown in full
/// - then left-truncated to fit (see [`truncate_detail`])
fn operation_detail(kind: &TaskKind, width: u16) -> String {
    let prefix = kind.prefix();
    let detail = match (kind.source(), kind.source_basename(), kind.destination()) {
        (Some(source), Some(base), Some(destination)) => {
            let dir = kind.target();
            let budget = (width as usize).saturating_sub(prefix.width());
            let full = format!("{source} to {dir}");
            // The source basename stays intact if the full form fits as-is, or
            // if `<basename> to <dir>` survives a left-truncation (which costs
            // one column for the ellipsis). Otherwise switch to the `to` form.
            if full.width() <= budget
                || format!("{base} to {dir}").width() <= budget.saturating_sub(1)
            {
                full
            } else {
                format!("to {destination}")
            }
        }
        _ => kind.detail(),
    };
    truncate_detail(prefix, &detail, width)
}

pub(super) fn operations_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    tasks: &'a HashSet<Task>,
    cancel_hint: &'a str,
) -> Block<'a> {
    let style = theme.progress();
    let bold = style.add_modifier(Modifier::BOLD);
    let left = if tasks.len() == 1 {
        // Keep the verb prefix in full; left-truncate the rest so the tail of
        // the path (the destination) stays visible as the width shrinks.
        let kind = tasks.iter().next().unwrap().kind();
        let detail = operation_detail(kind, width);
        Line::from(vec![
            Span::styled(kind.prefix(), bold),
            Span::styled(detail, style),
        ])
    } else {
        let message = format!("Multiple ({}) operations in progress", tasks.len());
        Line::from(Span::styled(truncate_left(&message, width as usize), style))
    };
    create_notice_block(left, style, width, cancel_hint)
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

pub(super) fn search_loading_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    search_tick: u16,
) -> Block<'a> {
    let style = theme.search_loading();
    let block_width: u16 = 3;
    if width <= block_width {
        return Block::default().borders(Borders::NONE).style(style);
    }

    // Triangle wave: position bounces 0 → travel → 0.
    // Scale the tick by a width-derived speed so the indicator crosses
    // wider screens faster (more cells per tick) instead of taking longer.
    let travel = width - block_width;
    let speed = (width / SEARCH_LOADING_SPEED_DIVISOR).max(1);
    let cycle = travel * 2;
    let t = (search_tick.wrapping_mul(speed)) % cycle;
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

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::{operation_detail, truncate_detail};
    use crate::command::progress::TaskKind;

    // Left-truncation keeps the tail (destination) visible as the width
    // shrinks, e.g. `…naut/Downloads/` then `…aut/Downloads/`.
    #[test_case(60, "/tmp/a to /home/andornaut/Downloads/"; "unchanged when it fits")]
    #[test_case(40, "…a to /home/andornaut/Downloads/"; "source truncated from the left first")]
    #[test_case(30, "…/andornaut/Downloads/"; "more of the source dropped")]
    #[test_case(24, "…naut/Downloads/"; "destination tail kept at width 24")]
    #[test_case(23, "…aut/Downloads/"; "destination tail kept at width 23")]
    #[test_case(8, "…"; "only an ellipsis when budget below minimum")]
    fn truncate_detail_copy(width: u16, expected: &str) {
        assert_eq!(
            expected,
            truncate_detail("Copying ", "/tmp/a to /home/andornaut/Downloads/", width)
        );
    }

    #[test_case(80, "/home/andornaut/projects/old/cache/data.bin"; "unchanged when it fits")]
    #[test_case(30, "…s/old/cache/data.bin"; "left-truncated to the tail")]
    #[test_case(20, "…e/data.bin"; "left-truncated further")]
    #[test_case(9, "…"; "only an ellipsis when budget below minimum")]
    fn truncate_detail_delete(width: u16, expected: &str) {
        assert_eq!(
            expected,
            truncate_detail(
                "Deleting ",
                "/home/andornaut/projects/old/cache/data.bin",
                width
            )
        );
    }

    fn copy_kind() -> TaskKind {
        TaskKind::Copy {
            source: "/tmp/a/file.txt".into(),
            destination: "/home/andornaut/Downloads/file.txt".into(),
        }
    }

    // As the width shrinks: full source + dest dir, then left-truncated source,
    // then (once the source basename no longer fits) switch to
    // `to <full destination incl. basename>`, then an ellipsis.
    #[test_case(80, "/tmp/a/file.txt to /home/andornaut/Downloads/"; "full when it fits")]
    #[test_case(50, "…/a/file.txt to /home/andornaut/Downloads/"; "source left-truncated, basename intact")]
    #[test_case(47, "…file.txt to /home/andornaut/Downloads/"; "source basename still fully shown")]
    #[test_case(46, "to /home/andornaut/Downloads/file.txt"; "switches to to-form before basename is truncated")]
    #[test_case(40, "…me/andornaut/Downloads/file.txt"; "destination form left-truncated")]
    #[test_case(24, "…nloads/file.txt"; "destination form truncated further, basename kept")]
    #[test_case(8, "…"; "only an ellipsis when budget below minimum")]
    fn operation_detail_copy(width: u16, expected: &str) {
        assert_eq!(expected, operation_detail(&copy_kind(), width));
    }

    #[test_case(80, "/home/andornaut/projects/old/cache/data.bin"; "full when it fits")]
    #[test_case(30, "…s/old/cache/data.bin"; "left-truncated to the tail")]
    #[test_case(9, "…"; "only an ellipsis when budget below minimum")]
    fn operation_detail_delete(width: u16, expected: &str) {
        let kind = TaskKind::Delete {
            path: "/home/andornaut/projects/old/cache/data.bin".into(),
        };
        assert_eq!(expected, operation_detail(&kind, width));
    }
}
