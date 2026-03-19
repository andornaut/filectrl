mod handler;
mod view;

use ratatui::{layout::Rect, widgets::ScrollbarState};

#[derive(Default)]
pub(crate) struct ScrollbarView {
    area: Rect,
    is_dragging: bool,
    state: ScrollbarState,
}
