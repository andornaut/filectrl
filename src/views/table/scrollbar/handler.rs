use ratatui::{
    crossterm::event::{MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
};

use super::ScrollbarView;

impl ScrollbarView {
    pub fn is_clicked(&self, x: u16, y: u16) -> bool {
        self.area.intersects(Rect::new(x, y, 1, 1))
    }

    pub fn is_dragging(&self) -> bool {
        self.is_dragging
    }

    pub fn handle_mouse(
        &mut self,
        event: &MouseEvent,
        total_lines_count: usize,
        visible_lines_count: usize,
    ) -> Option<usize> {
        let x = event.column;
        let y = event.row;

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.is_clicked(x, y) {
                    self.is_dragging = true;
                    return self.handle_drag(y, total_lines_count, visible_lines_count);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.is_dragging = false;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.is_dragging {
                    return self.handle_drag(y, total_lines_count, visible_lines_count);
                }
            }
            _ => {}
        }
        None
    }

    fn handle_drag(
        &self,
        y: u16,
        total_lines_count: usize,
        visible_lines_count: usize,
    ) -> Option<usize> {
        if total_lines_count <= visible_lines_count {
            return None;
        }

        let scrollbar_height = self.area.height;
        let last_relative_line = scrollbar_height.saturating_sub(1) as f32;
        let relative_y = y.saturating_sub(self.area.y);
        let percentage = (relative_y as f32 / last_relative_line).min(1.0);

        // selected_line is 0-based, but total_lines_count is 1-based, so we need to subtract 1
        let selected_line =
            (percentage * total_lines_count.saturating_sub(1) as f32).round() as usize;
        Some(selected_line)
    }
}
