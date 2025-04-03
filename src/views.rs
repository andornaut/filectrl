mod errors;
mod header;
mod help;
mod prompt;
pub mod root;
mod status;
mod table;

use ratatui::{
    backend::Backend,
    layout::{Margin, Rect},
    style::Style,
    widgets::{Block, Borders},
    Frame,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode},
};

const ELLIPSIS: &str = "…";
const NEWLINE_ELLIPSIS: &str = "\n…";

pub(super) trait View<B: Backend>: CommandHandler {
    fn render(&mut self, frame: &mut Frame, rect: Rect, mode: &InputMode, theme: &Theme);
}

pub(super) fn bordered<B: Backend>(
    frame: &mut Frame,
    rect: Rect,
    style: Style,
    title: Option<String>,
) -> Rect {
    let mut block = Block::default().borders(Borders::ALL).border_style(style);
    if let Some(title) = title {
        block = block.title(title);
    }
    frame.render_widget(block, rect);
    rect.inner(Margin {
        horizontal: 1,
        vertical: 1,
    })
}

pub(super) fn split_with_ellipsis(line: &str, width: u16) -> Vec<String> {
    assert!(width > 0);

    let split = split_utf8_with_reservation(&line, width, NEWLINE_ELLIPSIS);
    let mut lines = Vec::new();
    let mut it = split.into_iter().peekable();
    while let Some(part) = it.next() {
        let is_last = it.peek().is_none();
        let part = if is_last { part.clone() } else { part + "…" };
        lines.push(part);
    }
    lines
}

fn split_utf8_with_reservation(line: &str, width: u16, reservation: &str) -> Vec<String> {
    if len_utf8(line) <= width {
        return vec![line.into()];
    }

    let reserved_len = len_utf8(reservation);
    let width = width.saturating_sub(reserved_len);
    if width == 0 {
        return Vec::new();
    }

    split_utf8(line, width.saturating_sub(reserved_len))
}

fn len_utf8(line: &str) -> u16 {
    UnicodeSegmentation::graphemes(line, true).count() as u16
}

fn split_utf8(line: &str, width: u16) -> Vec<String> {
    let mut graphemes = UnicodeSegmentation::graphemes(line, true);
    (0..)
        .map(|_| graphemes.by_ref().take(width as usize).collect::<String>())
        .take_while(|s| !s.is_empty())
        .collect::<Vec<_>>()
}

pub(super) fn truncate_left_utf8_with_ellipsis(line: &str, width: u16) -> String {
    if len_utf8(line) <= width {
        return line.into();
    }

    let reserved_len = len_utf8(ELLIPSIS);
    let width = width.saturating_sub(reserved_len);
    if width == 0 {
        return "".into();
    }

    let mut graphemes = UnicodeSegmentation::graphemes(line, true);
    let mut chars = graphemes
        .by_ref()
        .rev()
        .take(width as usize)
        .collect::<Vec<&str>>();
    chars.reverse();
    format!("{ELLIPSIS}{}", chars.join(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("example".to_string(),  "example", 7; "full width")]
    #[test_case("…ample".to_string(),  "example", 6; "width -1")]
    #[test_case("…e".to_string(),  "example", 2; "2 width")]
    #[test_case("e".to_string(),  "e", 1; "1 width but full")]
    #[test_case("".to_string(),  "example", 1; "1 width")]
    #[test_case("".to_string(),  "example", 0; "0 width")]
    #[test_case("".to_string(),  "", 4; "empty string")]
    fn truncate_left_utf8_with_ellipsis_is_correct(expected: String, line: &str, width: u16) {
        let result = truncate_left_utf8_with_ellipsis(line, width);

        assert_eq!(expected, result);
    }
}
