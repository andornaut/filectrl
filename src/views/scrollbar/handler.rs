use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::ScrollbarView;
use crate::views::contains;

impl ScrollbarView {
    pub fn is_clicked(&self, event: MouseEvent) -> bool {
        contains(self.area, event)
    }

    pub fn is_dragging(&self) -> bool {
        self.is_dragging
    }

    /// Ends a drag whose release never arrived, as when another view took it.
    pub fn end_drag(&mut self) {
        self.is_dragging = false;
    }

    /// A drag begun without the click that starts one, for a test whose view
    /// has no scrollbar drawn to click.
    #[cfg(test)]
    pub fn begin_drag(&mut self) {
        self.is_dragging = true;
    }

    pub fn handle_mouse(&mut self, event: MouseEvent, max_position: usize) -> Option<usize> {
        let y = event.row;

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if self.is_clicked(event) => {
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

        let last_relative = self.track.height.saturating_sub(1);
        if last_relative == 0 {
            return None;
        }
        // Measured over the track, not the end arrows. Clamped before scaling,
        // so a drag past either end, or a click on an arrow, lands on the
        // nearest position rather than beyond it.
        let relative_y = y.saturating_sub(self.track.y).min(last_relative);
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
        let area = Rect {
            x: 0,
            y,
            width: 1,
            height,
        };
        ScrollbarView {
            area,
            track: area,
            ..Default::default()
        }
    }

    #[test]
    fn hiding_ends_a_drag_whose_release_it_may_never_see() {
        let mut s = scrollbar_at(0, 5);
        s.handle_mouse(
            ratatui::crossterm::event::MouseEvent {
                kind: ratatui::crossterm::event::MouseEventKind::Down(
                    ratatui::crossterm::event::MouseButton::Left,
                ),
                column: 0,
                row: 2,
                modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
            },
            10,
        );
        assert!(s.is_dragging());

        s.hide();

        assert!(!s.is_dragging());
    }

    #[test]
    fn a_zero_range_has_no_position_to_drag_to() {
        let s = scrollbar_at(0, 5);
        assert_eq!(None, s.handle_drag(0, 0));
    }

    // height=10 over a max position of 100, so a row maps to 100/9 of the
    // range, which is not a whole number: truncating and rounding differ.
    #[test_case(0, Some(0)     ; "the top row selects the first position")]
    #[test_case(9, Some(100)   ; "the bottom row selects the last position")]
    // relative=5, position = 5 * 100 / 9 = 55.6, rounded to nearest
    #[test_case(5, Some(56)    ; "a middle row selects proportionally")]
    #[test_case(100, Some(100) ; "a drag past the bottom clamps to the last position")]
    fn a_drag_maps_a_row_to_a_position(y: u16, expected: Option<usize>) {
        let s = scrollbar_at(0, 10);
        assert_eq!(expected, s.handle_drag(y, 100));
    }

    #[test]
    fn a_one_row_scrollbar_has_no_range_to_drag_over() {
        let s = scrollbar_at(0, 1);
        assert_eq!(None, s.handle_drag(0, 99));
    }

    #[test]
    fn only_a_press_on_the_scrollbar_starts_a_drag_and_a_release_ends_it() {
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut s = scrollbar_at(0, 10);
        let mut mouse = |kind, column| {
            s.handle_mouse(
                MouseEvent {
                    kind,
                    column,
                    row: 9,
                    modifiers: KeyModifiers::NONE,
                },
                99,
            )
        };
        let press = MouseEventKind::Down(MouseButton::Left);
        let drag = MouseEventKind::Drag(MouseButton::Left);
        let release = MouseEventKind::Up(MouseButton::Left);

        // Column 5 is beside the one-column scrollbar.
        assert_eq!(None, mouse(press, 5));
        assert_eq!(None, mouse(drag, 5));

        assert_eq!(Some(99), mouse(press, 0));
        // A drag keeps scrolling once started, wherever the pointer is.
        assert_eq!(Some(99), mouse(drag, 5));

        assert_eq!(None, mouse(release, 0));
        assert_eq!(None, mouse(drag, 0));
    }

    #[test]
    fn drag_with_y_offset_adjusts_relative_position() {
        // scrollbar starts at y=5; drag at y=5 → relative=0 → first position
        let s = scrollbar_at(5, 10);
        assert_eq!(Some(0), s.handle_drag(5, 99));
        // drag at y=14 → relative=9 → last position
        assert_eq!(Some(99), s.handle_drag(14, 99));
    }

    /// With end arrows drawn, the track is the rows between them, so its first
    /// and last rows are the ends of the range and the arrows clamp to them.
    #[test_case(1, Some(0)   ; "the first track row selects the first position")]
    #[test_case(8, Some(99)  ; "the last track row selects the last position")]
    #[test_case(0, Some(0)   ; "the top arrow clamps to the first position")]
    #[test_case(9, Some(99)  ; "the bottom arrow clamps to the last position")]
    fn a_track_between_end_arrows_maps_its_own_rows(y: u16, expected: Option<usize>) {
        let mut s = scrollbar_at(0, 10);
        s.track = Rect {
            y: 1,
            height: 8,
            ..s.area
        };
        assert_eq!(expected, s.handle_drag(y, 99));
    }

    #[test]
    fn rendering_with_end_arrows_leaves_them_out_of_the_track() {
        use crate::{
            app::config::{Config, RuntimeEnv},
            test_support::TempDir,
        };

        let dir = TempDir::new("scrollbar_ends");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[theme.scrollbar]\nshow_ends = true\n[theme256.scrollbar]\nshow_ends = true\n",
        )
        .unwrap();
        let config = Config::load(RuntimeEnv::default(), Some(path), &[]).unwrap();
        let area = Rect::new(3, 2, 1, 10);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let mut s = ScrollbarView::default();

        s.render(config.theme(), area, &mut buf, 0, 99, 10);

        assert_eq!(Rect::new(3, 3, 1, 8), s.track);
        assert_eq!(area, s.area);
    }
}
