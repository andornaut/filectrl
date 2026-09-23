mod handler;
mod view;

use ratatui::{layout::Rect, widgets::ScrollbarState};

#[derive(Default)]
pub struct ScrollbarView {
    area: Rect,
    /// The rows between the end arrows, which is what a position maps onto.
    /// The whole `area` when no ends are drawn.
    track: Rect,
    is_dragging: bool,
    state: ScrollbarState,
}
