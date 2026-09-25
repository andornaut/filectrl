use std::collections::VecDeque;

use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::{Constraint, Rect},
    style::Style,
    text::{Line, Text},
    widgets::{Paragraph, Widget},
};

use super::{View, as_dimension, bordered, contains};
use crate::{
    app::config::keybindings::{Action, KeyBindings},
    app::config::{Config, theme::Theme},
    command::{Command, handler::CommandHandler, result::CommandResult},
    views::unicode::split_with_ellipsis,
};

const MAX_NUMBER_ALERTS: usize = 5;
/// Longest alert kept, in characters. A message can quote text from outside
/// (a clipboard entry, a path), and every frame wraps each alert again, so an
/// unbounded one would slow every redraw until the alerts are cleared.
const MAX_ALERT_CHARS: usize = 1000;
/// Most wrapped lines one alert is drawn on, so a few long messages cannot
/// push the table down to its minimum. The full text is in the log.
const MAX_ALERT_LINES: usize = 3;
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

/// One alert, with the sequence number it was raised under
/// (`AlertsView::mark`).
type Alert = (AlertKind, String, u64);

pub(super) struct AlertsView {
    alerts: VecDeque<Alert>,
    area: Rect,
    hint: String,
    next_seq: u64,
}

impl AlertsView {
    pub fn new(keybindings: &KeyBindings) -> Self {
        let hint = format!(
            "(Press {} to clear)",
            keybindings.hint_for(&[Action::ClearAlerts])
        );
        Self {
            alerts: VecDeque::new(),
            area: Rect::default(),
            hint,
            next_seq: 0,
        }
    }
}

impl AlertsView {
    fn add_alert(&mut self, kind: AlertKind, mut message: String) -> CommandResult {
        if let Some((end, _)) = message.char_indices().nth(MAX_ALERT_CHARS) {
            message.truncate(end);
            message.push('…');
        }
        match kind {
            AlertKind::Info => log::info!("{message}"),
            AlertKind::Warn => log::warn!("{message}"),
            AlertKind::Error => log::error!("{message}"),
        }
        if self.alerts.len() == MAX_NUMBER_ALERTS {
            self.alerts.pop_back();
        }
        self.alerts.push_front((kind, message, self.next_seq));
        self.next_seq += 1;
        CommandResult::Handled
    }

    /// Where the next alert will be numbered from: every alert raised before
    /// this call is below it.
    pub(super) fn mark(&self) -> u64 {
        self.next_seq
    }

    /// Removes the info and warning alerts raised before `mark`, which a
    /// claimed key has now followed: they have been on screen, and the user
    /// has moved on. Errors stay until cleared.
    pub(super) fn expire_before(&mut self, mark: u64) {
        self.alerts
            .retain(|(kind, _, seq)| *kind == AlertKind::Error || *seq >= mark);
    }

    fn clear_alerts(&mut self) -> CommandResult {
        self.alerts.clear();
        CommandResult::Handled
    }

    fn has_border(area: Rect) -> bool {
        area.height >= MIN_HEIGHT_BORDERED
    }

    fn height(&self, area: Rect) -> u16 {
        if !self.should_show(area) {
            return 0;
        }
        let border_size = if Self::has_border(area) { 2 } else { 0 };
        let inner_width = area.width.saturating_sub(border_size);
        let items = self.alerts(inner_width);
        as_dimension(items.len()).saturating_add(border_size)
    }

    fn alerts(&self, inner_width: u16) -> Vec<(AlertKind, Line<'_>)> {
        // The rendered prefix (" • " or "   ") occupies 3 columns.
        let width_without_prefix = inner_width.saturating_sub(3);

        self.alerts
            .iter()
            .flat_map(|(kind, message, _)| {
                let mut lines = split_with_ellipsis(message, width_without_prefix as usize);
                if lines.len() > MAX_ALERT_LINES {
                    lines.truncate(MAX_ALERT_LINES);
                    if let Some(last) = lines.last_mut() {
                        last.pop();
                        last.push('…');
                    }
                }
                lines.into_iter().enumerate().map(|(i, line)| {
                    let prefix = if i == 0 { " •" } else { "  " };
                    (kind.clone(), Line::from(format!("{prefix} {line}")))
                })
            })
            .collect()
    }

    fn should_show(&self, area: Rect) -> bool {
        !self.alerts.is_empty() && area.height >= MIN_HEIGHT_BORDERLESS
    }
}

impl CommandHandler for AlertsView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::AlertInfo(message) => self.add_alert(AlertKind::Info, message.clone()),
            Command::AlertWarn(message) => self.add_alert(AlertKind::Warn, message.clone()),
            Command::AlertError(message) => self.add_alert(AlertKind::Error, message.clone()),
            Command::ResetView => self.clear_alerts(),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        match Config::global().keybindings.normal_action(code, modifiers) {
            Some(Action::ClearAlerts) => self.clear_alerts(),
            _ => CommandResult::NotHandled,
        }
    }
    fn handle_mouse(&mut self, event: MouseEvent) -> CommandResult {
        if let MouseEventKind::Down(MouseButton::Left) = event.kind {
            return self.clear_alerts();
        }
        CommandResult::Handled
    }

    fn should_handle_mouse(&self, event: MouseEvent) -> bool {
        contains(self.area, event)
    }
}

impl View for AlertsView {
    fn constraint(&self, area: Rect) -> Constraint {
        Constraint::Length(self.height(area))
    }

    fn render(&mut self, theme: &Theme, area: Rect, frame: &mut Frame<'_>) {
        self.area = area;
        if !self.should_show(area) {
            return;
        }

        let style = theme.alert.base();
        let inner_area = if Self::has_border(area) {
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
    use test_case::test_case;

    use super::*;

    fn view() -> AlertsView {
        Config::init_test();
        AlertsView::new(&Config::global().keybindings)
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

    /// A long alert is cut to a few lines, so five of them cannot squeeze the
    /// table down to its minimum.
    #[test]
    fn a_long_alert_is_drawn_on_at_most_three_lines() {
        let mut v = view();
        v.add_alert(AlertKind::Error, "x".repeat(500));

        let lines = v.alerts(40);

        assert_eq!(MAX_ALERT_LINES, lines.len());
        assert!(lines[2].1.to_string().ends_with('…'), "{:?}", lines[2].1);
        assert_eq!(
            as_dimension(MAX_ALERT_LINES) + 2,
            v.height(Rect::new(0, 0, 42, 24))
        );
    }

    #[test]
    fn a_short_alert_keeps_its_lines() {
        let mut v = view();
        v.add_alert(AlertKind::Info, "short".into());

        assert_eq!(1, v.alerts(40).len());
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

    #[test_case(MAX_ALERT_CHARS => MAX_ALERT_CHARS ; "at the limit is kept whole")]
    #[test_case(MAX_ALERT_CHARS + 1 => MAX_ALERT_CHARS + 1 ; "past the limit is cut and ends in an ellipsis")]
    fn a_long_alert_is_truncated(length: usize) -> usize {
        let mut v = view();
        v.add_alert(AlertKind::Warn, "é".repeat(length));
        let kept = &v.alerts.front().unwrap().1;
        assert_eq!(length > MAX_ALERT_CHARS, kept.ends_with('…'));
        kept.chars().count()
    }

    /// One short alert, whose single line the border wraps when there is room
    /// for it.
    #[test_case(10 => 3 ; "a tall area adds the border")]
    #[test_case(3 => 3 ; "the border needs three rows")]
    #[test_case(2 => 1 ; "a shorter area drops the border")]
    #[test_case(0 => 0 ; "no room hides the alerts")]
    fn height_fits_the_alerts_to_the_area(area_height: u16) -> u16 {
        let mut v = view();
        v.add_alert(AlertKind::Info, "boom".into());
        v.height(Rect::new(0, 0, 40, area_height))
    }

    #[test]
    fn a_wrapped_alert_bullets_only_its_first_line() {
        let mut v = view();
        v.add_alert(AlertKind::Info, "abcdefghij".into());

        // Nine columns leave six beside the three-column prefix.
        let lines: Vec<String> = v
            .alerts(9)
            .into_iter()
            .map(|(_, line)| line.to_string())
            .collect();

        assert_eq!(vec![" • abcde…", "   fghij"], lines);
    }

    #[test]
    fn a_click_clears_the_alerts() {
        let mut v = view();
        v.add_alert(AlertKind::Error, "boom".into());

        v.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });

        assert!(v.alerts.is_empty());
    }

    /// A claimed key clears what was on screen before it, but not what the
    /// key itself raised, and never an error.
    #[test]
    fn a_key_expires_the_info_and_warnings_raised_before_it() {
        let mut v = view();
        v.add_alert(AlertKind::Info, "earlier info".into());
        v.add_alert(AlertKind::Warn, "earlier warning".into());
        v.add_alert(AlertKind::Error, "earlier error".into());
        let mark = v.mark();
        v.add_alert(AlertKind::Info, "raised by the key".into());

        v.expire_before(mark);

        let left: Vec<&str> = v.alerts.iter().map(|(_, m, _)| m.as_str()).collect();
        assert_eq!(vec!["raised by the key", "earlier error"], left);
    }

    #[test]
    fn a_reset_clears_every_alert_errors_included() {
        let mut v = view();
        v.add_alert(AlertKind::Info, "info".into());
        v.add_alert(AlertKind::Error, "boom".into());

        v.handle_command(&Command::ResetView);

        assert!(v.alerts.is_empty());
    }

    #[test]
    fn clear_alerts_empties_the_queue() {
        let mut v = view();
        v.add_alert(AlertKind::Error, "boom".into());
        v.clear_alerts();
        assert!(v.alerts.is_empty());
    }
}
