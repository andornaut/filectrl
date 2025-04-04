use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use std::path::MAIN_SEPARATOR;

use super::View;
use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command},
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct HeaderView {
    breadcrumbs: Vec<String>,
    rect: Rect,
    positions: Vec<Vec<Position>>,
}

impl HeaderView {
    pub(super) fn height(&self, width: u16, theme: &Theme) -> u16 {
        // TODO cache `spans()` result for use in render()
        let active_style = theme.header_active();
        let inactive_style = theme.header();
        let (container, _) = spans(&self.breadcrumbs, width, active_style, inactive_style);
        container.len() as u16
    }

    fn set_directory(&mut self, directory: PathInfo) -> CommandResult {
        self.breadcrumbs = directory.breadcrumbs();
        CommandResult::none()
    }

    fn to_path(&self, end_index: usize) -> Option<PathInfo> {
        if let Some(components) = self.breadcrumbs.get(0..=end_index) {
            let path = if components.len() == 1 {
                // Clicked on the root element, which is empty string
                MAIN_SEPARATOR.to_string()
            } else {
                components.join(&MAIN_SEPARATOR.to_string())
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
                let x = event.column.saturating_sub(self.rect.x);
                let y = event.row.saturating_sub(self.rect.y);
                let row = &self.positions[y as usize];
                let clicked_index = row.iter().find_map(|p| {
                    if p.intersects(x) {
                        Some(p.index())
                    } else {
                        None
                    }
                });
                if let Some(path) = clicked_index.map(|i| self.to_path(i)).flatten() {
                    Command::Open(path).into()
                } else {
                    CommandResult::none()
                }
            }
            _ => CommandResult::none(),
        }
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.rect.intersects(Rect::new(x, y, 1, 1))
    }
}

impl View for HeaderView {
    fn render(&mut self, buf: &mut Buffer, rect: Rect, _: &InputMode, theme: &Theme) {
        self.rect = rect;

        let active_style = theme.header_active();
        let inactive_style = theme.header();
        let (mut container, mut positions) = spans(
            &self.breadcrumbs,
            self.rect.width,
            active_style,
            inactive_style,
        );

        // Prioritize displaying the deepest directories
        let at = positions.len() - self.rect.height as usize;
        let container = container.split_off(at);
        self.positions = positions.split_off(at);

        let text: Vec<_> = container
            .into_iter()
            .map(|spans| Line::from(spans))
            .collect();

        let widget = Paragraph::new(text).style(theme.header());
        widget.render(self.rect, buf);
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
    let mut it = breadcrumbs.into_iter().enumerate().peekable();

    let mut positions: Vec<Vec<Position>> = vec![Vec::new()];

    while let Some((i, name)) = it.next() {
        let is_last = it.peek().is_none();
        let style = if is_last {
            active_style
        } else {
            inactive_style
        };

        let name = format!("{}{MAIN_SEPARATOR}", name);
        let name_len = name.width_cjk() as u16;
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
