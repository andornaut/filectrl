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

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::ScrollbarView;

    fn scrollbar_at(y: u16, height: u16) -> ScrollbarView {
        ScrollbarView {
            area: Rect { x: 0, y, width: 1, height },
            ..Default::default()
        }
    }

    #[test]
    fn content_fits_in_viewport_returns_none() {
        let s = scrollbar_at(0, 5);
        assert_eq!(None, s.handle_drag(0, 5, 5));
        assert_eq!(None, s.handle_drag(0, 3, 5));
    }

    #[test]
    fn drag_at_top_selects_first_line() {
        let s = scrollbar_at(0, 10);
        assert_eq!(Some(0), s.handle_drag(0, 100, 10));
    }

    #[test]
    fn drag_at_bottom_selects_last_line() {
        let s = scrollbar_at(0, 10);
        assert_eq!(Some(99), s.handle_drag(9, 100, 10));
    }

    #[test]
    fn drag_at_middle_selects_proportional_line() {
        // height=10, y=5 → relative=5, percentage=5/9 ≈ 0.556, line = round(0.556 * 99) = 55
        let s = scrollbar_at(0, 10);
        assert_eq!(Some(55), s.handle_drag(5, 100, 10));
    }

    #[test]
    fn drag_with_scrollbar_y_offset_adjusts_relative_position() {
        // scrollbar starts at y=5; drag at y=5 → relative=0 → first line
        let s = scrollbar_at(5, 10);
        assert_eq!(Some(0), s.handle_drag(5, 100, 10));
        // drag at y=14 → relative=9 → last line
        assert_eq!(Some(99), s.handle_drag(14, 100, 10));
    }

    #[test]
    fn drag_beyond_scrollbar_bottom_clamps_to_last_line() {
        // scrollbar height=10, starts at y=0; drag at y=100 → relative clamped to 1.0 → last line
        let s = scrollbar_at(0, 10);
        assert_eq!(Some(99), s.handle_drag(100, 100, 10));
    }
}
