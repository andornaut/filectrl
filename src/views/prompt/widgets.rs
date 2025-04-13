// use log::debug;
use ratatui::{
    // style::{Color, Modifier, Style},
    // text::{Line, Span},
    widgets::Paragraph,
    // widgets::{Paragraph, Wrap},
};

use crate::app::config::theme::Theme;

pub(super) fn prompt_widget<'a>(theme: &'a Theme, label: String) -> Paragraph<'a> {
    Paragraph::new(label).style(theme.prompt_label())
}
