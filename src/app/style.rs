use ratatui::style::{Color, Modifier, Style};

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
    Style::default().bg(Color::LightBlue).fg(Color::Black)
}

pub fn status_directory_style() -> Style {
    Style::default().bg(Color::Magenta)
}

pub fn status_selected_style() -> Style {
    Style::default().bg(Color::LightRed)
}

pub fn table_header_style_default() -> Style {
    Style::default().bg(Color::Blue).fg(Color::Black)
}

pub fn table_header_style_sorted() -> Style {
    Style::default()
        .add_modifier(Modifier::BOLD)
        .bg(Color::Green)
        .fg(Color::Black)
}

pub fn table_selected_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}
