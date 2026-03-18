use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::{Constraint, Direction, Layout, Position, Rect},
    text::{Line, Span},
    widgets::{Paragraph, ScrollbarState, StatefulWidget, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::{View, bordered};
use crate::{
    app::{config::theme::Theme, state::AppState},
    command::{Command, handler::CommandHandler, mode::InputMode, result::CommandResult},
};

const MIN_HEIGHT: u16 = 4;

const DEFAULT_KEYBOARD_SHORTCUTS: [(&str, &str); 21] = [
    ("Quit: ", "q"),
    ("Navigate: ", "←/h, ↓/j, ↑/k, →/l"),
    ("Go to home dir: ", "~"),
    ("Go to parent dir: ", "←/b/Backspace"),
    ("Open: ", "→/f/l/Enter/Space"),
    ("Open custom: ", "o"),
    ("Select first row: ", "Home/g/^"),
    ("Select last row: ", "End/G/$"),
    ("Jump to middle row: ", "z"),
    ("Page down: ", "Ctrl+f/Ctrl+d/PgDn"),
    ("Page up: ", "Ctrl+b/Ctrl+u/PgUp"),
    ("Delete: ", "Delete"),
    ("Filter: ", "/"),
    ("Clear filter/alerts/clipboard/progress: ", "Esc, a, c, p"),
    ("Refresh: ", "Ctrl+r/F5"),
    ("Rename: ", "r/F2"),
    ("New window: ", "w"),
    ("Open terminal: ", "t"),
    ("Copy/Cut/Paste selected: ", "Ctrl+c, Ctrl+x, Ctrl+v"),
    ("Sort by name, modified, size: ", "n, m, s"),
    ("Toggle help: ", "?"),
];

const PROMPT_KEYBOARD_SHORTCUTS: [(&str, &str); 11] = [
    ("Submit: ", "Enter"),
    ("Cancel: ", "Esc"),
    ("Move cursor: ", "←/→"),
    ("Move cursor by word: ", "Ctrl+←/→"),
    ("Jump to line start/end: ", "Ctrl+a/Ctrl+e, Home/End"),
    ("Select text: ", "Shift+←/→"),
    ("Select to beginning/end of line: ", "Shift+Home/End"),
    ("Select by word: ", "Ctrl+Shift+←/→"),
    ("Select all: ", "Ctrl+Shift+A"),
    ("Copy/Cut/Paste text: ", "Ctrl+c, Ctrl+x, Ctrl+v"),
    ("Delete before/after cursor: ", "Backspace/Delete"),
];

// Labels are all ASCII, so byte length == display width. Using const fn avoids
// recomputing the max on every render.
const fn max_label_width(shortcuts: &[(&str, &str)]) -> usize {
    let mut max = 0;
    let mut i = 0;
    while i < shortcuts.len() {
        let len = shortcuts[i].0.len();
        if len > max {
            max = len;
        }
        i += 1;
    }
    max
}

const DEFAULT_MAX_LABEL_WIDTH: usize = max_label_width(&DEFAULT_KEYBOARD_SHORTCUTS);
const PROMPT_MAX_LABEL_WIDTH: usize = max_label_width(&PROMPT_KEYBOARD_SHORTCUTS);

#[derive(Default)]
pub(super) struct HelpView {
    area: Rect,
    inner_height: u16, // height of the bordered inner area; used to clamp page-scroll
    is_dragging: bool,
    is_visible: bool,
    max_scroll: u16, // cached in render; used by apply_drag
    scroll_offset: u16,
    scrollbar_area: Rect,
    scrollbar_state: ScrollbarState,
}

impl HelpView {
    // Maps a drag y-position to a scroll offset proportional to the scrollbar area.
    fn apply_drag(&mut self, y: u16) {
        let last_relative = self.scrollbar_area.height.saturating_sub(1) as f32;
        if last_relative == 0.0 || self.max_scroll == 0 {
            return;
        }
        let relative_y = y.saturating_sub(self.scrollbar_area.y);
        let percentage = (relative_y as f32 / last_relative).min(1.0);
        self.scroll_offset = (percentage * self.max_scroll as f32).round() as u16;
    }
}

impl CommandHandler for HelpView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::ToggleHelp => {
                self.is_visible = !self.is_visible;
                self.scroll_offset = 0;
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('?'), KeyModifiers::NONE) => Command::ToggleHelp.into(),
            _ if self.is_visible => match (*code, *modifiers) {
                (KeyCode::Down, KeyModifiers::NONE) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                    self.scroll_offset = self.scroll_offset.saturating_add(1).min(self.max_scroll);
                    CommandResult::Handled
                }
                (KeyCode::Up, KeyModifiers::NONE) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                    self.scroll_offset = self.scroll_offset.saturating_sub(1);
                    CommandResult::Handled
                }
                (KeyCode::PageDown, KeyModifiers::NONE)
                | (KeyCode::Char('f'), KeyModifiers::CONTROL)
                | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                    self.scroll_offset = self
                        .scroll_offset
                        .saturating_add(self.inner_height)
                        .min(self.max_scroll);
                    CommandResult::Handled
                }
                (KeyCode::PageUp, KeyModifiers::NONE)
                | (KeyCode::Char('b'), KeyModifiers::CONTROL)
                | (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                    self.scroll_offset = self.scroll_offset.saturating_sub(self.inner_height);
                    CommandResult::Handled
                }
                (KeyCode::Home, KeyModifiers::NONE)
                | (KeyCode::Char('g'), KeyModifiers::NONE)
                | (KeyCode::Char('^'), KeyModifiers::NONE) => {
                    self.scroll_offset = 0;
                    CommandResult::Handled
                }
                (KeyCode::End, KeyModifiers::NONE)
                | (KeyCode::Char('G'), KeyModifiers::SHIFT)
                | (KeyCode::Char('$'), KeyModifiers::NONE) => {
                    self.scroll_offset = u16::MAX; // clamped to max_scroll in render
                    CommandResult::Handled
                }
                _ => CommandResult::NotHandled,
            },
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::ScrollDown => {
                self.scroll_offset = self.scroll_offset.saturating_add(1).min(self.max_scroll);
                CommandResult::Handled
            }
            MouseEventKind::ScrollUp => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                CommandResult::Handled
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.scrollbar_area.contains(Position {
                    x: event.column,
                    y: event.row,
                }) {
                    self.is_dragging = true;
                    self.apply_drag(event.row);
                } else {
                    return Command::ToggleHelp.into();
                }
                CommandResult::Handled
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.is_dragging = false;
                CommandResult::Handled
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.is_dragging {
                    self.apply_drag(event.row);
                }
                CommandResult::Handled
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_handle_mouse(&self, event: &MouseEvent) -> bool {
        // Accept scroll events globally only when visible, so the user doesn't need to
        // position the cursor over the help panel to scroll it, but table scroll is not
        // affected when help is open.
        // Also accept all events while dragging, so Up/Drag are received wherever the
        // cursor travels during a drag.
        (self.is_visible
            && matches!(
                event.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ))
            || self.is_dragging
            || self.area.contains(Position {
                x: event.column,
                y: event.row,
            })
    }
}

impl View for HelpView {
    fn constraint(&self, _: Rect, state: &AppState) -> Constraint {
        if state.is_help_visible {
            Constraint::Min(MIN_HEIGHT)
        } else {
            Constraint::Length(0)
        }
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, state: &AppState, theme: &Theme) {
        self.area = area;
        if !state.is_help_visible || area.height < MIN_HEIGHT {
            return;
        }

        let style = theme.help.base();
        let (title_left, keyboard_shortcuts, max_label_width) = match state.mode {
            InputMode::Normal => (
                "Help",
                &DEFAULT_KEYBOARD_SHORTCUTS[..],
                DEFAULT_MAX_LABEL_WIDTH,
            ),
            InputMode::Prompt => (
                "Help (Prompt)",
                &PROMPT_KEYBOARD_SHORTCUTS[..],
                PROMPT_MAX_LABEL_WIDTH,
            ),
        };
        let bordered_area = bordered(
            area,
            frame.buffer_mut(),
            style,
            title_left,
            "(Press \"?\" to close)",
        );

        let content_height = keyboard_shortcuts.len() as u16 + 1; // +1 for header
        self.inner_height = bordered_area.height;
        self.max_scroll = content_height.saturating_sub(self.inner_height);
        let scroll = self.scroll_offset.min(self.max_scroll);

        let header_style = theme.help.header();
        let label_style = theme.help.label();
        let shortcut_style = theme.help.shortcuts();
        let header_padding = " ".repeat(max_label_width.saturating_sub("Actions".width()));
        let header = Line::from(vec![
            Span::styled("Actions", header_style),
            Span::raw(header_padding),
            Span::styled("Shortcuts", header_style),
        ]);
        let mut lines: Vec<Line> = vec![header];
        lines.extend(keyboard_shortcuts.iter().map(|&(label, key)| {
            let padding = " ".repeat(max_label_width - label.width());
            Line::from(vec![
                Span::styled(label, label_style),
                Span::raw(padding),
                Span::styled(key, shortcut_style),
            ])
        }));

        if self.max_scroll > 0 {
            let [content_area, scrollbar_area] = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .areas(bordered_area);
            self.scrollbar_area = scrollbar_area;

            Paragraph::new(lines)
                .style(style)
                .scroll((scroll, 0))
                .render(content_area, frame.buffer_mut());

            // content_length = max_scroll + 1 (number of scroll positions, not total rows).
            // viewport_content_length = inner_height.
            // This gives thumb size = inner_height / content_height fraction of the track.
            self.scrollbar_state = ScrollbarState::default()
                .content_length(self.max_scroll as usize + 1)
                .viewport_content_length(self.inner_height as usize)
                .position(scroll as usize);
            StatefulWidget::render(
                super::scrollbar_widget(&theme.scrollbar),
                scrollbar_area,
                frame.buffer_mut(),
                &mut self.scrollbar_state,
            );
        } else {
            self.scrollbar_area = Rect::default();
            Paragraph::new(lines)
                .style(style)
                .render(bordered_area, frame.buffer_mut());
        }
    }
}
