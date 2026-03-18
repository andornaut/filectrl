mod handler;
mod view;

use ratatui::{layout::Rect, widgets::ScrollbarState};

#[derive(Default)]
pub(super) struct ScrollbarView {
    area: Rect,
    is_dragging: bool,
    state: ScrollbarState,
}
