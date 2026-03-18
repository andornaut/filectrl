use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseEvent},
    layout::{Constraint, Position, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use super::{bordered, View};
use crate::{
    app::{config::theme::Theme, state::AppState},
    command::{Command, handler::CommandHandler, mode::InputMode, result::CommandResult},
};

const MIN_HEIGHT: u16 = 4;

const DEFAULT_KEYBOARD_SHORTCUTS: [(&str, &str); 19] = [
    ("Quit: ", "q"),
    ("Navigate: ", "←/h, ↓/j, ↑/k, →/l"),
    ("Go to home dir: ", "~"),
    ("Go to parent dir: ", "←/b/Backspace"),
    ("Open: ", "→/f/l/Enter/Space"),
    ("Select first row: ", "Home/g/^"),
    ("Select last row: ", "End/G/$"),
    ("Select middle of visible rows: ", "z"),
    ("Page down: ", "Ctrl+f/Ctrl+d/PgDn"),
    ("Page up: ", "Ctrl+b/Ctrl+u/PgUp"),
    ("Delete: ", "Delete"),
    ("Filter: ", "/"),
    ("Refresh: ", "Ctrl+r/F5"),
    ("Rename: ", "r/F2"),
    ("New window: ", "w"),
    ("Open terminal: ", "t"),
    ("Clear alerts, clipboard, progress: ", "a, c, p"),
    ("Copy/Cut/Paste selected: ", "Ctrl+c, Ctrl+x, Ctrl+v"),
    ("Sort by name, modified, size: ", "n, m, s"),
];

const PROMPT_KEYBOARD_SHORTCUTS: [(&str, &str); 11] = [
    ("Submit: ", "Enter"),
    ("Cancel: ", "Esc"),
    ("Move cursor: ", "←/→"),
    ("Move cursor by word: ", "Ctrl+←/→"),
    ("Move cursor to beginning/end of line: ", "Home/End"),
    ("Select text: ", "Shift+←/→"),
    ("Select to beginning/end of line: ", "Shift+Home/End"),
    ("Select by word: ", "Ctrl+Shift+←/→"),
    ("Select all: ", "Ctrl+a"),
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
}

impl CommandHandler for HelpView {
    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('?'), KeyModifiers::NONE) => Command::ToggleHelp.into(),
            (_, _) => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, _event: &MouseEvent) -> CommandResult {
        Command::ToggleHelp.into()
    }

    fn should_handle_mouse(&self, event: &MouseEvent) -> bool {
        self.area.contains(Position { x: event.column, y: event.row })
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
            InputMode::Normal => ("Help", &DEFAULT_KEYBOARD_SHORTCUTS[..], DEFAULT_MAX_LABEL_WIDTH),
            InputMode::Prompt => ("Help (Prompt)", &PROMPT_KEYBOARD_SHORTCUTS[..], PROMPT_MAX_LABEL_WIDTH),
        };
        let bordered_area = bordered(
            area,
            frame.buffer_mut(),
            style,
            title_left,
            "(Press \"?\" to close)",
        );

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

        Paragraph::new(lines)
            .style(style)
            .render(bordered_area, frame.buffer_mut());
    }
}
