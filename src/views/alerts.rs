use std::{collections::VecDeque, rc::Rc};

use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::{Constraint, Position, Rect},
    style::Style,
    text::{Line, Text},
    widgets::{Paragraph, Widget},
    Frame,
};

use super::{bordered, View};
use crate::{
    app::{config::theme::Theme, AppState},
    command::{handler::CommandHandler, result::CommandResult, Command},
    app::config::keybindings::{Action, KeyBindings},
    views::unicode::split_with_ellipsis,
};

const MAX_NUMBER_ALERTS: usize = 5;
const MIN_HEIGHT: u16 = 3; // border(2) + 1 alert line

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
    keybindings: Rc<KeyBindings>,
}

impl AlertsView {
    pub fn new(keybindings: Rc<KeyBindings>) -> Self {
        Self {
            alerts: VecDeque::new(),
            area: Rect::default(),
            keybindings,
        }
    }
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
        match self.keybindings.normal_action(code, modifiers) {
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
        self.area.contains(Position { x: event.column, y: event.row })
    }
}

impl View for AlertsView {
    fn constraint(&self, area: Rect, _: &AppState) -> Constraint {
        Constraint::Length(self.height(&area))
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, _: &AppState, theme: &Theme) {
        self.area = area;
        if !self.should_show(&area) {
            return;
        }

        let style = theme.alert.base();
        let hint = format!(
            "(Press \"{}\" to clear)",
            self.keybindings.display_for(Action::ClearAlerts)
        );
        let bordered_area = bordered(area, frame.buffer_mut(), style, "Alerts", &hint);
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
