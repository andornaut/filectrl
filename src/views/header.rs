use super::{len_utf8, View};
use crate::{
    app::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command},
    file_system::human::HumanPath,
};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    backend::Backend,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::path::MAIN_SEPARATOR;

#[derive(Default)]
pub(super) struct HeaderView {
    breadcrumbs: Vec<String>,
    rect: Rect,
    positions: Vec<Vec<Position>>,
}

impl HeaderView {
    pub(super) fn height(&self, parent_rect: Rect) -> u16 {
        // If the `rect.width` hasn't changed, then use the cached height to avoid some work.
        let width = parent_rect.width as u16;
        let style = Style::default();
        let (container, _) = spans(&self.breadcrumbs, width, style, style);
        container.len() as u16
    }

    fn set_directory(&mut self, directory: HumanPath) -> CommandResult {
        self.breadcrumbs = directory.breadcrumbs();
        CommandResult::none()
    }

    fn to_path(&self, end_index: usize) -> Option<HumanPath> {
        if let Some(components) = self.breadcrumbs.get(0..=end_index) {
            let path = if components.len() == 1 {
                // Clicked on the root element, which is empty string
                MAIN_SEPARATOR.to_string()
            } else {
                components.join(&MAIN_SEPARATOR.to_string())
            };
            HumanPath::try_from(path).ok()
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
    fn should_receive_mouse(&self, column: u16, row: u16) -> bool {
        let point = Rect::new(column, row, 1, 1);
        self.rect.intersects(point)
    }
}

impl<B: Backend> View<B> for HeaderView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &InputMode, theme: &Theme) {
        self.rect = rect;

        let active_style = theme.header_active();
        let inactive_style = theme.header();
        let (container, positions) = spans(
            &self.breadcrumbs,
            self.rect.width,
            active_style,
            inactive_style,
        );
        self.positions = positions;
        let text: Vec<_> = container
            .into_iter()
            .map(|spans| Line::from(spans))
            .collect();

        let paragraph = Paragraph::new(text).style(theme.header());
        frame.render_widget(paragraph, self.rect);
    }
}

#[derive(Debug)]
struct Position(u16, usize); // end x, index

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
        let name = format!("{}{MAIN_SEPARATOR}", name);
        let is_last = it.peek().is_none();
        let style = if is_last {
            active_style
        } else {
            inactive_style
        };

        let name_len = len_utf8(&name);
        row_len += name_len;
        if row_len > width {
            // Move to the next row
            container.push(Vec::new());
            row_len = name_len;
            positions.push(Vec::new());
        }

        let container_row = &mut container.last_mut().unwrap();
        container_row.push(Span::styled(name, style));

        let positions_row = &mut positions.last_mut().unwrap();
        positions_row.push(Position(row_len - 1, i));
    }
    (container, positions)
}
