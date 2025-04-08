use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    style::Style,
    text::{Line, Text},
    widgets::{Paragraph, Widget},
};
use std::collections::VecDeque;

use super::{bordered, View};
use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command},
    utf8::split_with_ellipsis,
};

const MAX_NUMBER_ALERTS: usize = 5;

#[derive(Clone, Debug, Eq, PartialEq)]
enum AlertKind {
    Info,
    Warn,
    Error,
}

#[derive(Default)]
pub(super) struct AlertsView {
    alerts: VecDeque<(AlertKind, String)>,
    area: Rect,
}

impl AlertsView {
    pub(super) fn height(&self, width: u16) -> u16 {
        if self.should_show() {
            // TODO cache `self.list_items()` result for use in render()
            let width = width.saturating_sub(2); // -2 for horizontal borders
            let items = self.list_items(width);
            items.len() as u16 + 2 // +2 for vertical borders
        } else {
            0
        }
    }

    fn add_alert(&mut self, kind: AlertKind, message: String) -> CommandResult {
        if self.alerts.len() == MAX_NUMBER_ALERTS {
            self.alerts.pop_front();
        }
        self.alerts.push_back((kind, message));
        CommandResult::none()
    }

    fn clear_alerts(&mut self) -> CommandResult {
        self.alerts.clear();
        CommandResult::none()
    }

    fn list_items(&self, width: u16) -> Vec<(Line<'_>, AlertKind)> {
        self.alerts
            .iter()
            .rev() // Newest alert messages near the top
            .flat_map(|(kind, message)| {
                split_with_ellipsis(message, width.saturating_sub(2))
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let prefix = if i == 0 { "•" } else { " " };
                        (Line::from(format!("{prefix} {line}")), kind.clone())
                    })
            })
            .collect()
    }

    fn should_show(&self) -> bool {
        !self.alerts.is_empty()
    }

    fn get_style(&self, kind: &AlertKind, theme: &Theme) -> Style {
        match kind {
            AlertKind::Info => theme.alert_info(),
            AlertKind::Warn => theme.alert_warning(),
            AlertKind::Error => theme.alert_error(),
        }
    }
}

impl CommandHandler for AlertsView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::AlertInfo(message) => self.add_alert(AlertKind::Info, message.clone()),
            Command::AlertWarn(message) => self.add_alert(AlertKind::Warn, message.clone()),
            Command::AlertError(message) => self.add_alert(AlertKind::Error, message.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('a'), KeyModifiers::NONE) => self.clear_alerts(),
            (_, _) => CommandResult::NotHandled,
        }
    }
    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // `self.should_receive_mouse()` guards this method to ensure that the click intersects with this view.
                self.clear_alerts();
                CommandResult::none()
            }
            _ => CommandResult::none(),
        }
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.area.intersects(Rect::new(x, y, 1, 1))
    }
}

impl View for AlertsView {
    fn constraint(&self, area: Rect, _: &InputMode) -> Constraint {
        Constraint::Length(self.height(area.width))
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, _: &InputMode, theme: &Theme) {
        self.area = area;
        if !self.should_show() {
            return;
        }

        let bordered_area = bordered(buf, area, theme.alert(), Some("Alerts".into()));
        let items = self.list_items(bordered_area.width);
        let mut text: Text<'_> = Text::default();

        for (line, kind) in items {
            let style = self.get_style(&kind, theme);
            text.lines.push(Line::from(line.spans).style(style));
        }

        let widget = Paragraph::new(text);
        widget.render(bordered_area, buf);
    }
}
