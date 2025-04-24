mod handler;
mod view;
mod widget;

use ratatui::{layout::Rect, widgets::ScrollbarState};

#[derive(Default)]
pub struct ScrollbarView {
    area: Rect,
    is_dragging: bool,
    state: ScrollbarState,
}
