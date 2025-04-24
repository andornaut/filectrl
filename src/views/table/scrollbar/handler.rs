use ratatui::{
    crossterm::event::{MouseButton, MouseEvent, MouseEventKind},
    layout::Position,
};

use super::ScrollbarView;

impl ScrollbarView {
    pub fn is_clicked(&self, x: u16, y: u16) -> bool {
        self.area.contains(Position { x, y })
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

        // Calculate the relative position within the scrollbar
        let scrollbar_height = self.area.height;
        let last_relative_line = scrollbar_height.saturating_sub(1) as f32;
        let relative_y = y.saturating_sub(self.area.y);
        let percentage = (relative_y as f32 / last_relative_line).min(1.0);

        // Convert percentage to line number
        // (total_lines_count - 1 because line indices are 0-based)
        let max_line = total_lines_count.saturating_sub(1);
        let selected_line = (percentage * max_line as f32).round() as usize;

        Some(selected_line)
    }
}
