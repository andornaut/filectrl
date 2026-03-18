use ratatui::widgets::Scrollbar;

use crate::app::config::theme::ScrollbarConfig;

pub(super) fn scrollbar(theme: &ScrollbarConfig) -> Scrollbar<'_> {
    crate::views::scrollbar_widget(theme)
}
