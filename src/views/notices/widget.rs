use std::{collections::HashSet, time::Duration};

use ratatui::buffer::CellWidth;
use ratatui::{
    layout::Alignment,
    style::{Modifier, Style},
    symbols::block,
    text::{Line, Span},
    widgets::{Block, Borders},
};

use crate::{
    app::{
        clipboard::ClipboardEntry,
        config::theme::{Clipboard, Notice as NoticeTheme, Table},
    },
    command::progress::{Progress, Task, TaskKind},
    views::{
        right_hint_fits,
        unicode::{pluralize_items, truncate_left},
    },
};

const COPY_PREFIX: &str = "[Copy] ";
const MARKED_PREFIX: &str = "[Selected] ";
const MOVE_PREFIX: &str = "[Cut] ";
const FILTER_PREFIX: &str = "[Filtered] ";
const SEARCH_PREFIX: &str = "[Searching...] ";
const SEARCH_CANCELLED_PREFIX: &str = "Cancelled: [Searching] ";

// Number of terminal columns per unit of search-loading indicator speed.
// The indicator advances `width / SEARCH_LOADING_SPEED_DIVISOR` cells per
// step, so wider screens sweep faster instead of taking longer to cross.
const SEARCH_LOADING_SPEED_DIVISOR: u16 = 32;

/// How long one step of the search-loading indicator lasts. Its position is
/// derived from elapsed time rather than counted, so this sets how fast the
/// indicator moves and nothing else. How often it is redrawn is a separate
/// question, answered by whatever wakes the event loop.
const SEARCH_LOADING_STEP: Duration = Duration::from_millis(80);

/// Cells the search-loading indicator itself occupies.
const SEARCH_LOADING_BLOCK_WIDTH: u16 = 3;

/// Where the search-loading indicator sits after `elapsed`, or `None` when the
/// terminal is too narrow to hold it.
///
/// Triangle wave: the position bounces 0 → travel → 0, one step at a time, with
/// each step scaled by a width-derived speed so the indicator crosses a wider
/// screen faster rather than taking longer. The modulo is taken at full width,
/// so a search running long enough to exhaust a `u16` of steps keeps moving
/// smoothly instead of jumping.
fn search_loading_position(width: u16, elapsed: Duration) -> Option<u16> {
    if width <= SEARCH_LOADING_BLOCK_WIDTH {
        return None;
    }
    let travel = u64::from(width - SEARCH_LOADING_BLOCK_WIDTH);
    let speed = u64::from((width / SEARCH_LOADING_SPEED_DIVISOR).max(1));
    let cycle = travel * 2;
    // Durations are u128 milliseconds. Saturating keeps an absurd elapsed time
    // moving at the end of the cycle rather than wrapping to the start.
    let step_millis = SEARCH_LOADING_STEP.as_millis().max(1);
    let steps = u64::try_from(elapsed.as_millis() / step_millis).unwrap_or(u64::MAX);
    let position = steps.saturating_mul(speed) % cycle;
    // Out along the first half of the cycle and back along the second, so the
    // block bounces rather than jumping back to the start.
    let offset = if position < travel {
        position
    } else {
        cycle - position
    };
    // `offset <= travel`, which came from a u16 width.
    Some(u16::try_from(offset).unwrap_or(0))
}

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

    let detail = if paths.len() > 1 {
        pluralize_items(paths.len())
    } else {
        let available_width = width.saturating_sub(prefix.cell_width());
        truncate_left(&paths[0].path.to_string_lossy(), available_width as usize)
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
    let progress = tasks
        .iter()
        .fold(Progress::default(), |acc, task| task.combine_progress(&acc));

    let percentage = progress.percentage();
    let percentage_text = format!(" {percentage}%");
    let bar_width = width.saturating_sub(percentage_text.cell_width());
    let progress_width = progress.scaled(bar_width);

    let filled = block::FULL.repeat(progress_width.into());
    let empty = " ".repeat(bar_width.saturating_sub(progress_width).into());
    let progress_bar = format!("{filled}{empty}");

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

/// The detail text after the verb prefix, left-truncated into whatever width the
/// prefix (always shown in full) leaves. Left-truncation keeps the tail of the
/// path visible (`…naut/Downloads/`), which is the part that identifies it. With
/// no room for any detail, only an ellipsis shows (`Copying …`).
fn truncate_detail(prefix: &str, detail: &str, width: u16) -> String {
    let budget = (width as usize).saturating_sub(prefix.cell_width() as usize);
    if budget < MIN_TRUNCATE_WIDTH {
        "…".to_string()
    } else if detail.cell_width() as usize <= budget {
        detail.to_string()
    } else {
        truncate_left(detail, budget)
    }
}

/// The detail string for one in-progress operation, keeping the most useful part
/// visible as the width shrinks: `"<source> to <destination dir>"` normally, or
/// `"to <full destination path>"` once the source basename would be truncated at
/// all, so the file name still shows in full. Then left-truncated to fit (see
/// [`truncate_detail`]).
fn operation_detail(kind: &TaskKind, width: u16) -> String {
    let prefix = kind.prefix();
    let detail = match (kind.source(), kind.source_basename(), kind.destination()) {
        (Some(source), Some(base), Some(destination)) => {
            let dir = kind.target();
            let budget = (width as usize).saturating_sub(prefix.cell_width() as usize);
            let full = format!("{source} to {dir}");
            // The source basename stays intact if the full form fits as-is, or
            // if `<basename> to <dir>` survives a left-truncation (which costs
            // one column for the ellipsis). Otherwise switch to the `to` form.
            if full.cell_width() as usize <= budget
                || format!("{base} to {dir}").cell_width() as usize <= budget.saturating_sub(1)
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

fn search_message_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    prefix: &'a str,
    query: &str,
    hint: &'a str,
) -> Block<'a> {
    let style = theme.search();
    let query = truncate_detail(prefix, query, width);
    let left = Line::from(vec![
        prefix.into(),
        Span::styled(query, style.add_modifier(Modifier::BOLD)),
    ]);
    create_notice_block(left, style, width, hint)
}

pub(super) fn search_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    query: &str,
    cancel_hint: &'a str,
) -> Block<'a> {
    search_message_widget(theme, width, SEARCH_PREFIX, query, cancel_hint)
}

pub(super) fn search_cancelled_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    query: &str,
    hint: &'a str,
) -> Block<'a> {
    search_message_widget(theme, width, SEARCH_CANCELLED_PREFIX, query, hint)
}

pub(super) fn search_loading_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    elapsed: Duration,
) -> Block<'a> {
    let style = theme.search_loading();
    let Some(pos) = search_loading_position(width, elapsed) else {
        return Block::default().borders(Borders::NONE).style(style);
    };

    let before = " ".repeat(pos as usize);
    let indicator = block::FULL.repeat(SEARCH_LOADING_BLOCK_WIDTH as usize);
    let after = " ".repeat(width.saturating_sub(pos + SEARCH_LOADING_BLOCK_WIDTH) as usize);

    let left = Line::from(format!("{before}{indicator}{after}"));

    Block::default()
        .borders(Borders::NONE)
        .title(left)
        .style(style)
}

/// Renders the message as the left title and, via [`right_hint_fits`], the
/// hint as the right title only when it fits alongside the full message.
fn create_notice_block<'a>(left: Line<'a>, style: Style, width: u16, hint: &'a str) -> Block<'a> {
    let left_width = left.width();
    let block = Block::default()
        .borders(Borders::NONE)
        .title(left)
        .style(style);

    if right_hint_fits(width as usize, left_width, hint.cell_width() as usize, 0) {
        let right = Line::from(hint).alignment(Alignment::Right);
        block.title(right)
    } else {
        block
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use test_case::test_case;

    use super::{operation_detail, search_loading_position, truncate_detail};
    use crate::command::progress::{TaskKind, Transfer};

    // Width 80 gives a travel of 77 cells at 2 cells per 80 ms step, so the
    // indicator turns around 39 steps (3120 ms) in and completes a cycle after
    // 77 steps (6160 ms).
    #[test_case(0, Some(0); "starts at the left edge")]
    #[test_case(80, Some(2); "advances one step")]
    #[test_case(120, Some(2); "holds position between steps")]
    #[test_case(3_120, Some(76); "turns around at the far edge")]
    #[test_case(3_200, Some(74); "comes back")]
    #[test_case(6_160, Some(0); "returns to the left edge")]
    // An hour of steps overflows a u16 many times over; the position is still
    // just the phase, with no jump where the count would have wrapped.
    #[test_case(3_600_000, Some(64); "stays continuous after a long search")]
    fn search_loading_position_bounces(elapsed_ms: u64, expected: Option<u16>) {
        assert_eq!(
            expected,
            search_loading_position(80, Duration::from_millis(elapsed_ms))
        );
    }

    #[test]
    fn search_loading_position_is_none_when_too_narrow() {
        assert_eq!(None, search_loading_position(3, Duration::ZERO));
        assert_eq!(Some(0), search_loading_position(4, Duration::ZERO));
    }

    // Left-truncation keeps the tail (destination) visible as the width
    // shrinks, e.g. `…oper/Downloads/` then `…per/Downloads/`.
    #[test_case(60, "/tmp/a to /home/developer/Downloads/"; "unchanged when it fits")]
    #[test_case(40, "…a to /home/developer/Downloads/"; "source truncated from the left first")]
    #[test_case(30, "…/developer/Downloads/"; "more of the source dropped")]
    #[test_case(24, "…oper/Downloads/"; "destination tail kept at width 24")]
    #[test_case(23, "…per/Downloads/"; "destination tail kept at width 23")]
    #[test_case(8, "…"; "only an ellipsis when budget below minimum")]
    fn truncate_detail_copy(width: u16, expected: &str) {
        assert_eq!(
            expected,
            truncate_detail("Copying ", "/tmp/a to /home/developer/Downloads/", width)
        );
    }

    #[test_case(80, "/home/developer/projects/old/cache/data.bin"; "unchanged when it fits")]
    #[test_case(30, "…s/old/cache/data.bin"; "left-truncated to the tail")]
    #[test_case(20, "…e/data.bin"; "left-truncated further")]
    #[test_case(9, "…"; "only an ellipsis when budget below minimum")]
    fn truncate_detail_delete(width: u16, expected: &str) {
        assert_eq!(
            expected,
            truncate_detail(
                "Deleting ",
                "/home/developer/projects/old/cache/data.bin",
                width
            )
        );
    }

    fn copy_kind() -> TaskKind {
        TaskKind::Copy(Transfer {
            source: "/tmp/a/file.txt".into(),
            destination: "/home/developer/Downloads/file.txt".into(),
        })
    }

    // As the width shrinks: full source + dest dir, then left-truncated source,
    // then (once the source basename no longer fits) switch to
    // `to <full destination incl. basename>`, then an ellipsis.
    #[test_case(80, "/tmp/a/file.txt to /home/developer/Downloads/"; "full when it fits")]
    #[test_case(50, "…/a/file.txt to /home/developer/Downloads/"; "source left-truncated, basename intact")]
    #[test_case(47, "…file.txt to /home/developer/Downloads/"; "source basename still fully shown")]
    #[test_case(46, "to /home/developer/Downloads/file.txt"; "switches to to-form before basename is truncated")]
    #[test_case(40, "…me/developer/Downloads/file.txt"; "destination form left-truncated")]
    #[test_case(24, "…nloads/file.txt"; "destination form truncated further, basename kept")]
    #[test_case(8, "…"; "only an ellipsis when budget below minimum")]
    fn operation_detail_copy(width: u16, expected: &str) {
        assert_eq!(expected, operation_detail(&copy_kind(), width));
    }

    #[test_case(80, "/home/developer/projects/old/cache/data.bin"; "full when it fits")]
    #[test_case(30, "…s/old/cache/data.bin"; "left-truncated to the tail")]
    #[test_case(9, "…"; "only an ellipsis when budget below minimum")]
    fn operation_detail_delete(width: u16, expected: &str) {
        let kind = TaskKind::Delete {
            path: "/home/developer/projects/old/cache/data.bin".into(),
        };
        assert_eq!(expected, operation_detail(&kind, width));
    }
}
