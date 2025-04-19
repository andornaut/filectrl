use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use super::{bordered, View};
use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult},
};

const MIN_HEIGHT: u16 = 2;

const DEFAULT_KEYBOARD_SHORTCUTS: [(&str, &str); 7] = [
    ("Left/Down/Up/Right: ", "h/j/k/l"),
    ("Open: ", "f"),
    ("Navigate back: ", "b"),
    ("Refresh: ", "CTRL+r"),
    ("Rename: ", "r"),
    ("Delete: ", "Delete"),
    ("Quit: ", "q"),
];
const PROMPT_KEYBOARD_SHORTCUTS: [(&str, &str); 2] = [("Submit: ", "Enter"), ("Cancel: ", "Esc")];

#[derive(Default)]
pub(super) struct HelpView {
    area: Rect,
    is_visible: bool,
}

impl HelpView {
    fn height(&self) -> u16 {
        if self.is_visible {
            4 // 2 lines of text + 2 borders
        } else {
            0
        }
    }

    fn toggle_visibility(&mut self) -> CommandResult {
        self.is_visible = !self.is_visible;
        CommandResult::Handled
    }
}

impl CommandHandler for HelpView {
    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('?'), KeyModifiers::NONE) => self.toggle_visibility(),
            (_, _) => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.is_visible = false;
                CommandResult::Handled
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.is_visible && self.area.intersects(Rect::new(x, y, 1, 1))
    }
}

impl View for HelpView {
    fn constraint(&self, _: Rect, _: &InputMode) -> Constraint {
        Constraint::Length(self.height())
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, mode: &InputMode, theme: &Theme) {
        if !self.is_visible || area.height < MIN_HEIGHT {
            return;
        }
        self.area = area;

        let style = theme.help();
        let title_left = "Help";
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
        let keyboard_shortcuts = match *mode {
            InputMode::Prompt => &PROMPT_KEYBOARD_SHORTCUTS[..],
            _ => &DEFAULT_KEYBOARD_SHORTCUTS[..],
        };

        let key_style = Style::default().add_modifier(Modifier::BOLD);
        let spans: Vec<Span> = keyboard_shortcuts
            .iter()
            .enumerate()
            .flat_map(|(index, &(description, key))| {
                let mut spans = Vec::with_capacity(3);
                if index > 0 {
                    spans.push(" ".into());
                }
                spans.push(description.into());
                spans.push(Span::styled(key, key_style));
                spans
            })
            .collect();

        let widget = Paragraph::new(Line::from(spans))
            .style(style)
            .wrap(Wrap { trim: true });
        widget.render(bordered_area, frame.buffer_mut());
    }
}
