use ratatui::style::{Color, Modifier, Style};

use super::color::{
    beige, black, blue, brown, cyan, dark_blue, dark_brown, dark_cyan, dark_green, dark_grey,
    light_beige, light_blue, light_brown, light_grey, light_orange, light_purple, pink, red,
};

pub fn error_style() -> Style {
    Style::default().bg(Color::DarkGray).fg(red())
}

pub fn header_component_active_style() -> Style {
    Style::default().bg(light_orange()).fg(black())
}

pub fn header_component_style() -> Style {
    Style::default().bg(light_purple()).fg(black())
}

pub fn prompt_input_style() -> Style {
    Style::default().bg(brown()).fg(light_beige())
}

pub fn prompt_label_style() -> Style {
    Style::default().bg(beige()).fg(black())
}

pub fn status_filter_mode_style() -> Style {
    Style::default().bg(Color::LightCyan).fg(Color::Black)
}

pub fn status_normal_mode_style() -> Style {
    Style::default().bg(cyan()).fg(Color::Black)
}

pub fn status_directory_label_style() -> Style {
    Style::default().bg(dark_cyan()).fg(light_beige())
}

pub fn status_selected_label_style() -> Style {
    Style::default().bg(dark_cyan()).fg(light_beige())
}

pub fn status_directory_style() -> Style {
    Style::default().fg(black())
}

pub fn status_selected_style() -> Style {
    Style::default().fg(black())
}

pub fn table_header_style() -> Style {
    Style::default().bg(light_brown()).fg(black())
}

pub fn table_header_active_style() -> Style {
    Style::default()
        .add_modifier(Modifier::BOLD)
        .bg(beige())
        .fg(black())
}

pub fn table_selected_style() -> Style {
    Style::default().bg(light_beige()).fg(black())
}
