use ratatui::style::{Color, Modifier, Style};

use super::color::{
    black, blue, brown, cyan, dark_blue, dark_brown, dark_green, dark_grey, light_blue,
    light_brown, light_grey,
};

pub fn error_style() -> Style {
    Style::default().bg(Color::DarkGray).fg(Color::Red)
}

pub fn prompt_input_style() -> Style {
    Style::default().bg(Color::Blue).fg(Color::Yellow)
}

pub fn prompt_label_style() -> Style {
    Style::default().bg(Color::LightYellow).fg(Color::Black)
}

pub fn status_filter_mode_style() -> Style {
    Style::default().bg(Color::LightCyan).fg(Color::Black)
}

pub fn status_normal_mode_style() -> Style {
    Style::default().bg(cyan()).fg(Color::Black)
}

pub fn status_directory_style() -> Style {
    Style::default().fg(dark_blue())
}

pub fn status_selected_style() -> Style {
    Style::default().fg(dark_green())
}

pub fn table_header_style_default() -> Style {
    Style::default().bg(light_brown()).fg(black())
}

pub fn table_header_style_sorted() -> Style {
    Style::default()
        .add_modifier(Modifier::BOLD)
        .bg(light_grey())
        .fg(black())
}

pub fn table_selected_style() -> Style {
    Style::default().bg(light_grey()).fg(black())
}
