use std::collections::VecDeque;

use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::{Constraint, Position, Rect},
    style::Style,
    text::{Line, Text},
    widgets::{Paragraph, Widget},
};

use super::{View, bordered};
use crate::{
    app::config::keybindings::Action,
    app::config::{Config, theme::Theme},
    command::{Command, handler::CommandHandler, result::CommandResult},
    views::unicode::split_with_ellipsis,
};

const MAX_NUMBER_ALERTS: usize = 5;
const MIN_HEIGHT_BORDERED: u16 = 3; // border(2) + 1 alert line
const MIN_HEIGHT_BORDERLESS: u16 = MIN_HEIGHT_BORDERED - 2; // 1 alert line

#[derive(Clone, Debug, Eq, PartialEq)]
enum AlertKind {
    Info,
    Warn,
    Error,
}

impl AlertKind {
    fn to_style(&self, theme: &Theme) -> Style {
        match self {
            AlertKind::Info => theme.alert.info(),
            AlertKind::Warn => theme.alert.warn(),
            AlertKind::Error => theme.alert.error(),
        }
    }
}

pub(super) struct AlertsView {
    alerts: VecDeque<(AlertKind, String)>,
    area: Rect,
    hint: String,
}

impl AlertsView {
    pub fn new() -> Self {
        let hint = format!(
            "(Press {} to clear)",
            Config::global()
                .keybindings
                .hint_for(&[Action::ClearAlerts])
        );
        Self {
            alerts: VecDeque::new(),
            area: Rect::default(),
            hint,
        }
    }
}

impl AlertsView {
    fn add_alert(&mut self, kind: AlertKind, message: String) -> CommandResult {
        match kind {
            AlertKind::Info => log::info!("{message}"),
            AlertKind::Warn => log::warn!("{message}"),
            AlertKind::Error => log::error!("{message}"),
        }
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

    fn has_border(&self, area: &Rect) -> bool {
        area.height >= MIN_HEIGHT_BORDERED
    }

    fn height(&self, area: &Rect) -> u16 {
        if !self.should_show(area) {
            return 0;
        }
        let border_size = if self.has_border(area) { 2 } else { 0 };
        let inner_width = area.width.saturating_sub(border_size);
        let items = self.alerts(inner_width);
        items.len() as u16 + border_size
    }

    fn alerts(&self, inner_width: u16) -> Vec<(AlertKind, Line<'_>)> {
        let width_without_prefix = inner_width.saturating_sub(2);

        self.alerts
            .iter()
            .flat_map(|(kind, message)| {
                split_with_ellipsis(message, width_without_prefix as usize)
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let prefix = if i == 0 { " •" } else { "  " };
                        (kind.clone(), Line::from(format!("{prefix} {line}")))
                    })
            })
            .collect()
    }

    fn should_show(&self, area: &Rect) -> bool {
        !self.alerts.is_empty() && area.height >= MIN_HEIGHT_BORDERLESS
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
        match Config::global().keybindings.normal_action(code, modifiers) {
            Some(Action::ClearAlerts) => self.clear_alerts(),
            _ => CommandResult::NotHandled,
        }
    }
    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        if let MouseEventKind::Down(MouseButton::Left) = event.kind {
            return self.clear_alerts();
        }
        CommandResult::Handled
    }

    fn should_handle_mouse(&self, event: &MouseEvent) -> bool {
        self.area.contains(Position {
            x: event.column,
            y: event.row,
        })
    }
}

impl View for AlertsView {
    fn constraint(&self, area: Rect) -> Constraint {
        Constraint::Length(self.height(&area))
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        self.area = area;
        if !self.should_show(&area) {
            return;
        }

        let theme = Config::global().theme();
        let style = theme.alert.base();
        let inner_area = if self.has_border(&area) {
            bordered(area, frame.buffer_mut(), style, "Alerts", &self.hint)
        } else {
            area
        };
        let text = Text::from(
            self.alerts(inner_area.width)
                .into_iter()
                .map(|(kind, line)| line.style(kind.to_style(theme)))
                .collect::<Vec<_>>(),
        );
        let widget = Paragraph::new(text).style(style);
        widget.render(inner_area, frame.buffer_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::config::{Config, RuntimeEnv};

    fn view() -> AlertsView {
        let config = Config::load(RuntimeEnv::default(), None, vec![]).unwrap();
        Config::init(config);
        AlertsView::new()
    }

    #[test]
    fn add_alert_prepends_newest_first() {
        let mut v = view();
        v.add_alert(AlertKind::Info, "first".into());
        v.add_alert(AlertKind::Warn, "second".into());
        assert_eq!(v.alerts.len(), 2);
        assert_eq!(v.alerts.front().unwrap().1, "second");
        assert_eq!(v.alerts.back().unwrap().1, "first");
    }

    #[test]
    fn add_alert_caps_at_max_and_drops_the_oldest() {
        let mut v = view();
        for i in 0..(MAX_NUMBER_ALERTS + 2) {
            v.add_alert(AlertKind::Info, format!("msg{i}"));
        }
        assert_eq!(v.alerts.len(), MAX_NUMBER_ALERTS);
        // Newest stays at the front; the two oldest ("msg0", "msg1") fell off.
        assert_eq!(
            v.alerts.front().unwrap().1,
            format!("msg{}", MAX_NUMBER_ALERTS + 1)
        );
        assert_eq!(v.alerts.back().unwrap().1, "msg2");
    }

    #[test]
    fn clear_alerts_empties_the_queue() {
        let mut v = view();
        v.add_alert(AlertKind::Error, "boom".into());
        v.clear_alerts();
        assert!(v.alerts.is_empty());
    }
}
