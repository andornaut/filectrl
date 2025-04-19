use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::{Constraint, Rect},
    style::Style,
    text::{Line, Text},
    widgets::{Paragraph, Widget},
    Frame,
};
use std::collections::VecDeque;
use unicode_width::UnicodeWidthStr;

use super::{bordered, View};
use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command},
    utf8::split_with_ellipsis,
};

const MAX_NUMBER_ALERTS: usize = 5;
const MIN_HEIGHT: u16 = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
enum AlertKind {
    Info,
    Warn,
    Error,
}

impl AlertKind {
    fn to_style(&self, theme: &Theme) -> Style {
        match self {
            AlertKind::Info => theme.alert_info(),
            AlertKind::Warn => theme.alert_warning(),
            AlertKind::Error => theme.alert_error(),
        }
    }
}

#[derive(Default)]
pub(super) struct AlertsView {
    alerts: VecDeque<(AlertKind, String)>,
    area: Rect,
}

impl AlertsView {
    fn add_alert(&mut self, kind: AlertKind, message: String) -> CommandResult {
        if self.alerts.len() == MAX_NUMBER_ALERTS {
            self.alerts.pop_back();
        }
        self.alerts.push_front((kind, message));
        CommandResult::Handled
    }

    fn clear_alerts(&mut self) -> CommandResult {
        self.alerts.clear();
        CommandResult::Handled
    }

    fn height(&self, area: &Rect) -> u16 {
        if !self.should_show(area) {
            return 0;
        }
        // First subtract borders from the outer area
        let inner_width = area.width.saturating_sub(2);
        let items = self.alerts(inner_width);
        items.len() as u16 + 2 // +2 for vertical borders
    }

    fn alerts(&self, width_without_borders: u16) -> Vec<(AlertKind, Line<'_>)> {
        let width_without_prefix = width_without_borders.saturating_sub(2);

        self.alerts
            .iter()
            .flat_map(|(kind, message)| {
                split_with_ellipsis(message, width_without_prefix)
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let prefix = if i == 0 { "•" } else { " " };
                        (kind.clone(), Line::from(format!("{prefix} {line}")))
                    })
            })
            .collect()
    }

    fn should_show(&self, area: &Rect) -> bool {
        !self.alerts.is_empty() && area.height >= MIN_HEIGHT
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
                self.clear_alerts();
                CommandResult::Handled
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.area.intersects(Rect::new(x, y, 1, 1))
    }
}

impl View for AlertsView {
    fn constraint(&self, area: Rect, _: &InputMode) -> Constraint {
        Constraint::Length(self.height(&area))
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, _: &InputMode, theme: &Theme) {
        if !self.should_show(&area) {
            return;
        }
        self.area = area;

        let style = theme.alert();
        let title_left = "Alerts";
        let title_right = "(Press \"a\" to clear)";
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
        let text = Text::from(
            self.alerts(bordered_area.width)
                .into_iter()
                .map(|(kind, line)| line.style(kind.to_style(theme)))
                .collect::<Vec<_>>(),
        );
        let widget = Paragraph::new(text).style(style);
        widget.render(bordered_area, frame.buffer_mut());
    }
}
