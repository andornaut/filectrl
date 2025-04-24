use ratatui::{
    symbols::{block, line},
    widgets::{Scrollbar, ScrollbarOrientation},
};

use crate::app::config::theme::Theme;

pub fn scrollbar(theme: &Theme) -> Scrollbar<'_> {
    let mut scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .thumb_style(theme.table_scrollbar_thumb())
        .thumb_symbol(block::FULL)
        .track_style(theme.table_scrollbar_track())
        .track_symbol(Some(line::VERTICAL));

    if theme.table_scrollbar_begin_end_enabled() {
        scrollbar = scrollbar
            .begin_symbol(Some("▲"))
            .begin_style(theme.table_scrollbar_begin())
            .end_symbol(Some("▼"))
            .end_style(theme.table_scrollbar_end());
    } else {
        scrollbar = scrollbar.begin_symbol(None).end_symbol(None);
    }

    scrollbar
}
