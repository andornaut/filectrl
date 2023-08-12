use ratatui::style::{Modifier, Style};

use super::color::{
    beige, black, blue, brown, cyan, dark_blue, dark_brown, dark_cyan, dark_grey, gold,
    light_beige, light_blue, light_brown, light_grey, light_orange, light_purple, pink, red,
};

pub fn error_style() -> Style {
    Style::default().bg(dark_brown()).fg(red())
}

pub fn header_active_style() -> Style {
    Style::default().bg(light_beige()).fg(black())
}

pub fn header_inactive_style() -> Style {
    Style::default().bg(light_brown()).fg(light_beige())
}

pub fn header_style() -> Style {
    Style::default().bg(dark_brown()).fg(light_beige())
}

pub fn help_style() -> Style {
    Style::default().bg(dark_brown()).fg(light_beige())
}

pub fn prompt_input_style() -> Style {
    Style::default().bg(dark_brown()).fg(light_beige())
}

pub fn prompt_label_style() -> Style {
    Style::default().bg(beige()).fg(black())
}

pub fn status_filter_mode_style() -> Style {
    Style::default().bg(cyan()).fg(black())
}

pub fn status_normal_mode_style() -> Style {
    Style::default().bg(cyan()).fg(black())
}

pub fn status_directory_label_style() -> Style {
    Style::default().bg(dark_cyan()).fg(light_beige())
}

pub fn status_directory_style() -> Style {
    Style::default()
}

pub fn status_selected_label_style() -> Style {
    Style::default().bg(dark_cyan()).fg(light_beige())
}

pub fn status_selected_style() -> Style {
    Style::default()
}

pub fn table_header_active_style() -> Style {
    Style::default()
        .add_modifier(Modifier::BOLD)
        .bg(beige())
        .fg(black())
}

pub fn table_header_style() -> Style {
    Style::default().bg(light_brown()).fg(black())
}

pub fn table_selected_style() -> Style {
    Style::default().bg(light_beige()).fg(black())
}
