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

    pub fn handle_mouse(&mut self, event: &MouseEvent, max_position: usize) -> Option<usize> {
        let x = event.column;
        let y = event.row;

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.is_clicked(x, y) {
                    self.is_dragging = true;
                    return self.handle_drag(y, max_position);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.is_dragging = false;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.is_dragging {
                    return self.handle_drag(y, max_position);
                }
            }
            _ => {}
        }
        None
    }

    fn handle_drag(&self, y: u16, max_position: usize) -> Option<usize> {
        if max_position == 0 {
            return None;
        }

        let last_relative = self.area.height.saturating_sub(1) as f32;
        if last_relative == 0.0 {
            return None;
        }
        let relative_y = y.saturating_sub(self.area.y);
        let percentage = (relative_y as f32 / last_relative).min(1.0);
        Some((percentage * max_position as f32).round() as usize)
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
    fn max_position_zero_returns_none() {
        let s = scrollbar_at(0, 5);
        assert_eq!(None, s.handle_drag(0, 0));
    }

    #[test]
    fn drag_at_top_selects_first_position() {
        let s = scrollbar_at(0, 10);
        assert_eq!(Some(0), s.handle_drag(0, 99));
    }

    #[test]
    fn drag_at_bottom_selects_last_position() {
        let s = scrollbar_at(0, 10);
        assert_eq!(Some(99), s.handle_drag(9, 99));
    }

    #[test]
    fn drag_at_middle_selects_proportional_position() {
        // height=10, y=5 → relative=5, percentage=5/9 ≈ 0.556, position = round(0.556 * 99) = 55
        let s = scrollbar_at(0, 10);
        assert_eq!(Some(55), s.handle_drag(5, 99));
    }

    #[test]
    fn drag_with_y_offset_adjusts_relative_position() {
        // scrollbar starts at y=5; drag at y=5 → relative=0 → first position
        let s = scrollbar_at(5, 10);
        assert_eq!(Some(0), s.handle_drag(5, 99));
        // drag at y=14 → relative=9 → last position
        assert_eq!(Some(99), s.handle_drag(14, 99));
    }

    #[test]
    fn drag_beyond_bottom_clamps_to_last_position() {
        let s = scrollbar_at(0, 10);
        assert_eq!(Some(99), s.handle_drag(100, 99));
    }
}
