use ratatui::{
    symbols::{block, line},
    widgets::{Scrollbar, ScrollbarOrientation},
};

use crate::app::config::theme::Theme;

pub(super) fn scrollbar(theme: &Theme) -> Scrollbar<'_> {
    let mut scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .thumb_style(theme.table.scrollbar_thumb())
        .thumb_symbol(block::FULL)
        .track_style(theme.table.scrollbar_track())
        .track_symbol(Some(line::VERTICAL));

    if theme.table.scrollbar_show_begin_end_symbols() {
        scrollbar = scrollbar
            .begin_symbol(Some("▲"))
            .begin_style(theme.table.scrollbar_begin())
            .end_symbol(Some("▼"))
            .end_style(theme.table.scrollbar_end());
    } else {
        scrollbar = scrollbar.begin_symbol(None).end_symbol(None);
    }

    scrollbar
}
