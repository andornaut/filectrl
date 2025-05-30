use std::path::MAIN_SEPARATOR;

use ratatui::{
    crossterm::event::{MouseButton, MouseEvent, MouseEventKind},
    layout::{Constraint, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use super::View;
use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command},
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct HeaderView {
    breadcrumbs: Vec<String>,
    area: Rect,
    positions: Vec<Vec<Position>>,
}

impl HeaderView {
    fn height(&self, width: u16) -> u16 {
        // Calculate height based on content length and width, without theme styling
        let (container, _) = spans(&self.breadcrumbs, width, Style::default(), Style::default());
        container.len() as u16
    }

    fn set_directory(&mut self, directory: PathInfo) -> CommandResult {
        self.breadcrumbs = directory.breadcrumbs();
        CommandResult::Handled
    }

    fn to_path(&self, end_index: usize) -> Option<PathInfo> {
        if let Some(components) = self.breadcrumbs.get(0..=end_index) {
            let path = if components.len() == 1 {
                // Clicked on the root element, which is empty string
                MAIN_SEPARATOR.to_string()
            } else {
                components.join(std::path::MAIN_SEPARATOR_STR)
            };
            PathInfo::try_from(path).ok()
        } else {
            None
        }
    }
}

impl CommandHandler for HeaderView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::SetDirectory(directory, _) => self.set_directory(directory.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let x = event.column.saturating_sub(self.area.x);
                let y = event.row.saturating_sub(self.area.y);
                let row = &self.positions[y as usize];
                let clicked_index = row.iter().find_map(|p| {
                    if p.intersects(x) {
                        Some(p.index())
                    } else {
                        None
                    }
                });
                if let Some(path) = clicked_index.and_then(|i| self.to_path(i)) {
                    Command::Open(path).into()
                } else {
                    CommandResult::Handled
                }
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.area.contains(ratatui::layout::Position { x, y })
    }
}

impl View for HeaderView {
    fn constraint(&self, area: Rect, _: &InputMode) -> Constraint {
        Constraint::Length(self.height(area.width))
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, _: &InputMode, theme: &Theme) {
        self.area = area;

        let active_style = theme.header_active();
        let inactive_style = theme.header();
        let (mut container, mut positions) = spans(
            &self.breadcrumbs,
            self.area.width,
            active_style,
            inactive_style,
        );

        // Prioritize displaying the deepest directories
        let at = positions.len() - self.area.height as usize;
        let container = container.split_off(at);
        self.positions = positions.split_off(at);

        let text: Vec<_> = container.into_iter().map(Line::from).collect();

        let widget = Paragraph::new(text).style(theme.header());
        widget.render(self.area, frame.buffer_mut());
    }
}

#[derive(Debug)]
struct Position(u16, usize); // x_end_position, breadcrumbs_index

impl Position {
    pub(super) fn intersects(&self, x: u16) -> bool {
        x <= self.0
    }

    pub(super) fn index(&self) -> usize {
        self.1
    }
}

fn spans<'a>(
    breadcrumbs: &[String],
    width: u16,
    active_style: Style,
    inactive_style: Style,
) -> (Vec<Vec<Span<'a>>>, Vec<Vec<Position>>) {
    let mut container = vec![Vec::new()];
    let mut row_len = 0;
    let mut it = breadcrumbs.iter().enumerate().peekable();

    let mut positions: Vec<Vec<Position>> = vec![Vec::new()];

    while let Some((i, name)) = it.next() {
        let is_last = it.peek().is_none();
        let style = if is_last {
            active_style
        } else {
            inactive_style
        };

        let name = format!("{}{MAIN_SEPARATOR}", name);
        let name_len = name.width() as u16;
        row_len += name_len;
        if row_len > width {
            // Move to the next row
            container.push(Vec::new());
            positions.push(Vec::new());
            row_len = name_len;
        }

        let container_row = &mut container.last_mut().unwrap();
        container_row.push(Span::styled(name, style));

        let positions_row = &mut positions.last_mut().unwrap();
        positions_row.push(Position(row_len - 1, i));
    }
    (container, positions)
}
