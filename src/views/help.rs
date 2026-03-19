use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::{Constraint, Direction, Layout, Position, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::{ScrollbarView, View, bordered};
use crate::{
    app::{AppState, config::theme::Theme},
    command::{Command, handler::CommandHandler, result::CommandResult},
};

const MIN_HEIGHT: u16 = 5;

const NORMAL_MODE_SHORTCUTS: [(&str, &str); 24] = [
    ("Quit: ", "q"),
    ("Go to parent dir: ", "←/h/b/Backspace"),
    ("Open: ", "→/l/f/Enter/Space"),
    ("Open custom: ", "o"),
    ("Open new window: ", "w"),
    ("Open terminal: ", "t"),
    ("Go to home dir: ", "~"),
    ("Select next/previous row: ", "↓/j, ↑/k"),
    ("Select first/last row: ", "Home/g/^, End/G/$"),
    ("Jump to middle row: ", "z"),
    ("Page down/up: ", "Ctrl+f/d/PgDn, Ctrl+b/u/PgUp"),
    ("Mark/unmark item: ", "v"),
    ("Range mark: ", "V"),
    ("Copy: ", "Ctrl+c"),
    ("Cut: ", "Ctrl+x"),
    ("Paste: ", "Ctrl+v"),
    ("Delete: ", "Delete"),
    ("Rename: ", "r/F2"),
    ("Filter: ", "/"),
    ("Sort by name/modified/size: ", "n, m, s"),
    ("Refresh: ", "Ctrl+r/F5"),
    ("Clear clipboard/filter/marks: ", "Esc"),
    ("Clear alerts/progress: ", "a, p"),
    ("Toggle help: ", "?"),
];

const PROMPT_MODE_SHORTCUTS: [(&str, &str); 12] = [
    ("Submit: ", "Enter"),
    ("Cancel: ", "Esc"),
    ("Reset to initial value: ", "Ctrl+z"),
    ("Move cursor: ", "←/→"),
    ("Move cursor by word: ", "Ctrl+←/→"),
    ("Jump to line start/end: ", "Ctrl+a/Home, Ctrl+e/End"),
    ("Select text: ", "Shift+←/→"),
    ("Select to line start/end: ", "Shift+Home, Shift+End"),
    ("Select by word: ", "Ctrl+Shift+←/→"),
    ("Select all: ", "Ctrl+Shift+A"),
    ("Copy/Cut/Paste text: ", "Ctrl+c/x/v"),
    ("Delete before/after cursor: ", "Backspace/Delete"),
];

use std::sync::LazyLock;

// Uses UnicodeWidthStr::width() for correct display width with non-ASCII labels
static MAX_LABEL_WIDTH: LazyLock<usize> = LazyLock::new(|| {
    NORMAL_MODE_SHORTCUTS
        .iter()
        .chain(PROMPT_MODE_SHORTCUTS.iter())
        .map(|(label, _)| label.width())
        .max()
        .unwrap_or(0)
});

#[derive(Default)]
pub(super) struct HelpView {
    area: Rect,
    inner_height: u16,
    max_scroll: u16,
    scroll_offset: u16,
    scrollbar_view: ScrollbarView,
}

impl HelpView {
    fn reset_scroll(&mut self) {
        self.scroll_offset = 0;
    }

    fn scroll_down(&mut self, lines: u16) {
        self.scroll_offset = self
            .scroll_offset
            .saturating_add(lines)
            .min(self.max_scroll);
    }

    fn scroll_up(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }
}

impl CommandHandler for HelpView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::ResetHelpScroll => {
                self.reset_scroll();
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Down, KeyModifiers::NONE) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                self.scroll_down(1);
                CommandResult::Handled
            }
            (KeyCode::Up, KeyModifiers::NONE) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                self.scroll_up(1);
                CommandResult::Handled
            }
            (KeyCode::PageDown, KeyModifiers::NONE)
            | (KeyCode::Char('f'), KeyModifiers::CONTROL)
            | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                self.scroll_down(self.inner_height);
                CommandResult::Handled
            }
            (KeyCode::PageUp, KeyModifiers::NONE)
            | (KeyCode::Char('b'), KeyModifiers::CONTROL)
            | (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.scroll_up(self.inner_height);
                CommandResult::Handled
            }
            (KeyCode::Home, KeyModifiers::NONE)
            | (KeyCode::Char('g'), KeyModifiers::NONE)
            | (KeyCode::Char('^'), KeyModifiers::NONE) => {
                self.reset_scroll();
                CommandResult::Handled
            }
            (KeyCode::End, KeyModifiers::NONE)
            | (KeyCode::Char('G'), KeyModifiers::SHIFT)
            | (KeyCode::Char('$'), KeyModifiers::NONE) => {
                self.scroll_offset = self.max_scroll;
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::ScrollDown => {
                self.scroll_down(1);
                CommandResult::Handled
            }
            MouseEventKind::ScrollUp => {
                self.scroll_up(1);
                CommandResult::Handled
            }
            MouseEventKind::Down(MouseButton::Left)
            | MouseEventKind::Up(MouseButton::Left)
            | MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(pos) = self
                    .scrollbar_view
                    .handle_mouse(event, self.max_scroll as usize)
                {
                    self.scroll_offset = pos as u16;
                }
                CommandResult::Handled
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_handle_mouse(&self, event: &MouseEvent) -> bool {
        matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) || self.scrollbar_view.is_dragging()
            || self.area.contains(Position {
                x: event.column,
                y: event.row,
            })
    }
}

use crate::app::config::theme::Help;

fn add_section<'a>(
    lines: &mut Vec<Line<'a>>,
    title: &'a str,
    shortcuts: &[(&'a str, &'a str)],
    help: &Help,
) {
    let header_padding = " ".repeat((*MAX_LABEL_WIDTH).saturating_sub(title.width()));
    lines.push(Line::from(vec![
        Span::styled(title, help.header()),
        Span::raw(header_padding),
        Span::styled("Shortcuts", help.header()),
    ]));
    lines.extend(shortcuts.iter().map(|&(label, key)| {
        let padding = " ".repeat(*MAX_LABEL_WIDTH - label.width());
        Line::from(vec![
            Span::styled(label, help.actions()),
            Span::raw(padding),
            Span::styled(key, help.shortcuts()),
        ])
    }));
}

impl View for HelpView {
    fn constraint(&self, _: Rect, _: &AppState) -> Constraint {
        Constraint::Min(MIN_HEIGHT)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, _state: &AppState, theme: &Theme) {
        self.area = area;
        if area.height < MIN_HEIGHT {
            return;
        }

        let style = theme.help.base();
        let bordered_area = bordered(
            area,
            frame.buffer_mut(),
            style,
            "Help",
            "(Press \"?\" or Esc to close)",
        );

        let mut lines: Vec<Line> = Vec::new();
        add_section(
            &mut lines,
            "Normal Mode",
            &NORMAL_MODE_SHORTCUTS,
            &theme.help,
        );
        lines.push(Line::raw(""));
        add_section(
            &mut lines,
            "Prompt Mode",
            &PROMPT_MODE_SHORTCUTS,
            &theme.help,
        );

        let content_height = lines.len() as u16;
        self.inner_height = bordered_area.height;
        self.max_scroll = content_height.saturating_sub(self.inner_height);
        let scroll = self.scroll_offset.min(self.max_scroll);

        if self.max_scroll > 0 {
            let [content_area, scrollbar_area] = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .areas(bordered_area);

            Paragraph::new(lines)
                .style(style)
                .scroll((scroll, 0))
                .render(content_area, frame.buffer_mut());

            self.scrollbar_view.render(
                scrollbar_area,
                frame.buffer_mut(),
                theme,
                scroll as usize,
                self.max_scroll as usize,
                self.inner_height as usize,
            );
        } else {
            self.scrollbar_view
                .render(Rect::default(), frame.buffer_mut(), theme, 0, 0, 0);
            Paragraph::new(lines)
                .style(style)
                .render(bordered_area, frame.buffer_mut());
        }
    }
}
