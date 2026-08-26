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

    pub fn handle_mouse(&mut self, event: MouseEvent, max_position: usize) -> Option<usize> {
        let x = event.column;
        let y = event.row;

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if self.is_clicked(x, y) => {
                self.is_dragging = true;
                return self.handle_drag(y, max_position);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.is_dragging = false;
            }
            MouseEventKind::Drag(MouseButton::Left) if self.is_dragging => {
                return self.handle_drag(y, max_position);
            }
            _ => {}
        }
        None
    }

    fn handle_drag(&self, y: u16, max_position: usize) -> Option<usize> {
        if max_position == 0 {
            return None;
        }

        let last_relative = self.area.height.saturating_sub(1);
        if last_relative == 0 {
            return None;
        }
        // Clamped before scaling, so a drag past the end lands on the last
        // position rather than beyond it.
        let relative_y = y.saturating_sub(self.area.y).min(last_relative);
        // Integer arithmetic rather than a float ratio: the numerator is at
        // most `last_relative * max_position`, and adding half the denominator
        // before dividing rounds to nearest as the float version did.
        let denominator = u64::from(last_relative);
        let numerator = u64::from(relative_y) * u64::try_from(max_position).unwrap_or(u64::MAX)
            + denominator / 2;
        Some(usize::try_from(numerator / denominator).unwrap_or(max_position))
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use test_case::test_case;

    use super::ScrollbarView;

    fn scrollbar_at(y: u16, height: u16) -> ScrollbarView {
        ScrollbarView {
            area: Rect {
                x: 0,
                y,
                width: 1,
                height,
            },
            ..Default::default()
        }
    }

    #[test]
    fn max_position_zero_returns_none() {
        let s = scrollbar_at(0, 5);
        assert_eq!(None, s.handle_drag(0, 0));
    }

    // height=10 over a max position of 99, so a row maps to 99/9 of the range.
    #[test_case(0, Some(0)     ; "the top row selects the first position")]
    #[test_case(9, Some(99)    ; "the bottom row selects the last position")]
    // relative=5, percentage=5/9 = 0.556, position = round(0.556 * 99)
    #[test_case(5, Some(55)    ; "a middle row selects proportionally")]
    #[test_case(100, Some(99)  ; "a drag past the bottom clamps to the last position")]
    fn a_drag_maps_a_row_to_a_position(y: u16, expected: Option<usize>) {
        let s = scrollbar_at(0, 10);
        assert_eq!(expected, s.handle_drag(y, 99));
    }

    #[test]
    fn drag_with_y_offset_adjusts_relative_position() {
        // scrollbar starts at y=5; drag at y=5 → relative=0 → first position
        let s = scrollbar_at(5, 10);
        assert_eq!(Some(0), s.handle_drag(5, 99));
        // drag at y=14 → relative=9 → last position
        assert_eq!(Some(99), s.handle_drag(14, 99));
    }
}
