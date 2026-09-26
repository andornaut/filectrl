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
        unicode::{fit_left, pluralize_items},
    },
};

const COPY_PREFIX: &str = "[Copy] ";
const MARKED_PREFIX: &str = "[Selected] ";
const RANGE_PREFIX: &str = "[Range] ";
const MOVE_PREFIX: &str = "[Cut] ";
const FILTER_PREFIX: &str = "[Filtered] ";
const SEARCH_PREFIX: &str = "[Searching...] ";
const SEARCH_CANCELLED_PREFIX: &str = "[Search cancelled] ";

// Columns per unit of indicator speed, so wider screens sweep faster.
const SEARCH_LOADING_SPEED_DIVISOR: u16 = 32;

/// Duration of one step of the search-loading indicator (its speed, not its redraw rate).
const SEARCH_LOADING_STEP: Duration = Duration::from_millis(80);

const SEARCH_LOADING_BLOCK_WIDTH: u16 = 3;

/// Where the search-loading indicator sits after `elapsed` (a triangle wave), or `None` when too
/// narrow.
fn search_loading_position(width: u16, elapsed: Duration) -> Option<u16> {
    if width <= SEARCH_LOADING_BLOCK_WIDTH {
        return None;
    }
    let travel = u64::from(width - SEARCH_LOADING_BLOCK_WIDTH);
    let speed = u64::from((width / SEARCH_LOADING_SPEED_DIVISOR).max(1));
    let cycle = travel * 2;
    // Saturating keeps an absurd elapsed time at the end of the cycle rather than wrapping.
    let step_millis = SEARCH_LOADING_STEP.as_millis().max(1);
    let steps = u64::try_from(elapsed.as_millis() / step_millis).unwrap_or(u64::MAX);
    let position = steps.saturating_mul(speed) % cycle;
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

    let detail = match paths {
        [only] => {
            let path = crate::file_system::path_info::visible_path(&only.path);
            detail_after(prefix, &path, width)
        }
        _ => pluralize_items(paths.len()),
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
    range: bool,
    hint: &'a str,
) -> Block<'a> {
    let style = theme.marked();
    let prefix = if range { RANGE_PREFIX } else { MARKED_PREFIX };
    let left = Line::from(vec![
        Span::styled(prefix, style.add_modifier(Modifier::BOLD)),
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
        Span::styled(
            crate::visible(filter),
            theme.filter().add_modifier(Modifier::BOLD),
        ),
    ]);
    create_notice_block(left, theme.filter(), width, hint)
}

pub(super) fn progress_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    tasks: &'a HashSet<Task>,
    finished: usize,
) -> Block<'a> {
    let progress = batch_progress(tasks, finished);

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

const TASK_PARTS: u64 = 1000;

/// Batch progress as the mean of each task's fraction, ended tasks counted complete.
/// A running task stops one part short, so only an ended batch reads 100%.
pub(super) fn batch_progress(tasks: &HashSet<Task>, finished: usize) -> Progress {
    let running: u64 = tasks
        .iter()
        .map(|task| {
            let own = task.progress();
            if own.total == 0 {
                return 0;
            }
            let parts = u128::from(own.completed) * u128::from(TASK_PARTS) / u128::from(own.total);
            u64::try_from(parts)
                .unwrap_or(TASK_PARTS)
                .min(TASK_PARTS - 1)
        })
        .sum();
    let finished = u64::try_from(finished).unwrap_or(u64::MAX / (2 * TASK_PARTS));
    let count = finished + u64::try_from(tasks.len()).unwrap_or(0);
    Progress {
        completed: finished * TASK_PARTS + running,
        total: count * TASK_PARTS,
    }
}

/// The detail text after `prefix`, fitted into the width the prefix leaves, without the prefix.
fn detail_after(prefix: &str, detail: &str, width: u16) -> String {
    fit_left(prefix, detail, "", usize::from(width)).split_off(prefix.len())
}

/// Detail for one operation: `"<source> to <destination dir>"`, or `"to <destination path>"`
/// once the source basename would be truncated; then left-truncated to fit.
fn operation_detail(kind: &TaskKind, width: u16) -> String {
    let prefix = kind.prefix();
    let detail = match (kind.source(), kind.source_basename(), kind.destination()) {
        (Some(source), Some(base), Some(destination)) => {
            let dir = kind.target();
            let budget = (width as usize).saturating_sub(prefix.cell_width() as usize);
            let full = format!("{source} to {dir}");
            // Keep `<basename> to <dir>` if it fits, even left-truncated; otherwise use the `to`
            // form.
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
    detail_after(prefix, &detail, width)
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
        // Keep the verb prefix; left-truncate the rest so the destination stays visible.
        let kind = tasks.iter().next().unwrap().kind();
        let detail = operation_detail(kind, width);
        Line::from(vec![
            Span::styled(kind.prefix(), bold),
            Span::styled(detail, style),
        ])
    } else {
        let message = format!("Multiple ({}) operations in progress", tasks.len());
        Line::from(Span::styled(
            fit_left("", &message, "", usize::from(width)),
            style,
        ))
    };
    create_notice_block(left, style, width, cancel_hint)
}

fn search_message_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    prefix: String,
    query: &str,
    hint: &'a str,
) -> Block<'a> {
    let style = theme.search();
    let query = detail_after(&prefix, &crate::visible(query), width);
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
    search_message_widget(theme, width, SEARCH_PREFIX.to_string(), query, cancel_hint)
}

pub(super) fn search_cancelled_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    query: &str,
    hint: &'a str,
) -> Block<'a> {
    search_message_widget(
        theme,
        width,
        SEARCH_CANCELLED_PREFIX.to_string(),
        query,
        hint,
    )
}

/// A finished search with its result count, which leads so a long query is what gets cut.
pub(super) fn search_finished_widget<'a>(
    theme: &NoticeTheme,
    width: u16,
    query: &str,
    results: usize,
    hint: &'a str,
) -> Block<'a> {
    search_message_widget(theme, width, search_finished_prefix(results), query, hint)
}

fn search_finished_prefix(results: usize) -> String {
    match results {
        1 => "[Search: 1 result] ".to_string(),
        _ => format!("[Search: {results} results] "),
    }
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

/// A block titled with the message, and the hint on the right only when it fits.
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

    use super::{
        clipboard_widget, filter_widget, marked_widget, operation_detail, search_cancelled_widget,
        search_finished_prefix, search_loading_position, search_widget,
    };
    use crate::{
        app::{clipboard::ClipboardEntry, config::Config},
        command::progress::{TaskKind, Transfer},
    };

    fn rendered(block: ratatui::widgets::Block) -> String {
        let area = ratatui::layout::Rect::new(0, 0, 80, 1);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(block, area, &mut buffer);
        (0..80)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>()
            .trim()
            .to_string()
    }

    #[test_case(false => "[Selected] 3 items" ; "marks")]
    #[test_case(true => "[Range] 3 items" ; "a range")]
    fn the_marked_notice_names_range_mode(range: bool) -> String {
        Config::init_test();
        rendered(marked_widget(
            &Config::global().theme.table,
            80,
            3,
            range,
            "",
        ))
    }

    #[test]
    fn typed_text_in_the_notices_is_spelled_out() {
        Config::init_test();
        let theme = &Config::global().theme.notice;
        let typed = "a\u{202e}b";

        for text in [
            rendered(filter_widget(theme, 80, typed, "")),
            rendered(search_widget(theme, 80, typed, "")),
            rendered(search_cancelled_widget(theme, 80, typed, "")),
        ] {
            assert!(text.contains("a\\u{202e}b"), "{text}");
        }
    }

    #[test]
    fn a_cancelled_search_is_labelled_as_one() {
        Config::init_test();
        let theme = &Config::global().theme.notice;

        assert_eq!(
            "[Search cancelled] q",
            rendered(search_cancelled_widget(theme, 80, "q", ""))
        );
    }

    #[test]
    fn an_empty_clipboard_entry_renders_as_a_count() {
        Config::init_test();
        let entry = ClipboardEntry::Copy(Vec::new());

        let text = rendered(clipboard_widget(
            &Config::global().theme.clipboard,
            80,
            &entry,
            "",
        ));

        assert!(text.ends_with("0 items"), "{text}");
    }

    // Width 80: travel 77 cells at 2 cells per 80 ms step; turns at 39 steps, cycles at 77.
    #[test_case(0, Some(0); "starts at the left edge")]
    #[test_case(80, Some(2); "advances one step")]
    #[test_case(120, Some(2); "holds position between steps")]
    #[test_case(3_120, Some(76); "turns around at the far edge")]
    #[test_case(3_200, Some(74); "comes back")]
    #[test_case(6_160, Some(0); "returns to the left edge")]
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

    #[test]
    fn a_terminal_narrower_than_the_speed_divisor_still_moves_the_indicator() {
        assert_eq!(
            Some(1),
            search_loading_position(20, Duration::from_millis(80))
        );
    }

    fn copy_kind() -> TaskKind {
        TaskKind::Copy(Transfer {
            source: "/tmp/a/file.txt".into(),
            destination: "/home/developer/Downloads/file.txt".into(),
        })
    }

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
    #[test_case(20, "…e/data.bin"; "left-truncated further")]
    #[test_case(9, "…"; "only an ellipsis when budget below minimum")]
    fn operation_detail_delete(width: u16, expected: &str) {
        let kind = TaskKind::Delete {
            path: "/home/developer/projects/old/cache/data.bin".into(),
        };
        assert_eq!(expected, operation_detail(&kind, width));
    }

    #[test_case(0 => "[Search: 0 results] " ; "none")]
    #[test_case(1 => "[Search: 1 result] " ; "one")]
    #[test_case(42 => "[Search: 42 results] " ; "several")]
    fn a_finished_search_counts_its_results(results: usize) -> String {
        search_finished_prefix(results)
    }
}
