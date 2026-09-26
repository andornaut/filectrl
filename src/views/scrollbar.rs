mod handler;
mod view;

use ratatui::{layout::Rect, widgets::ScrollbarState};

#[derive(Default)]
pub struct ScrollbarView {
    area: Rect,
    /// The rows between the end arrows, or the whole `area` when none are drawn.
    track: Rect,
    is_dragging: bool,
    state: ScrollbarState,
}
