use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseEvent},
    layout::{Constraint, Position, Rect},
    style::{Modifier, Style},
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

#[derive(Default)]
pub(super) struct HelpView {
    area: Rect,
}

impl CommandHandler for HelpView {
    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('?'), KeyModifiers::NONE) => {
                CommandResult::HandledWith(Box::new(Command::ToggleHelp))
            }
            (_, _) => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, _event: &MouseEvent) -> CommandResult {
        CommandResult::HandledWith(Box::new(Command::ToggleHelp))
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

        let style = theme.help();
        let title_left = match state.mode {
            InputMode::Prompt => "Help (Prompt)",
            _ => "Help",
        };
        let title_right = "(Press \"?\" to close)";
        let title_left_width = title_left.width() as u16;
        let title_right_width = title_right.width() as u16;
        let has_extra_width = area.width > title_left_width + title_right_width + 2; // +2 for the borders

        let title_right = if has_extra_width {
            Some(title_right)
        } else {
            None
        };
        let bordered_area = bordered(
            area,
            frame.buffer_mut(),
            style,
            Some(title_left),
            title_right,
        );
        let keyboard_shortcuts = match state.mode {
            InputMode::Prompt => &PROMPT_KEYBOARD_SHORTCUTS[..],
            _ => &DEFAULT_KEYBOARD_SHORTCUTS[..],
        };

        let label_style = Style::default().add_modifier(Modifier::BOLD);
        let max_label_width = keyboard_shortcuts
            .iter()
            .map(|(label, _)| label.width())
            .max()
            .unwrap_or(0);
        let lines: Vec<Line> = keyboard_shortcuts
            .iter()
            .map(|&(label, key)| {
                let padding = " ".repeat(max_label_width - label.width());
                Line::from(vec![
                    Span::styled(label, label_style),
                    Span::raw(padding),
                    Span::raw(key),
                ])
            })
            .collect();

        Paragraph::new(lines)
            .style(style)
            .render(bordered_area, frame.buffer_mut());
    }
}
