use ratatui::style::{Color, Modifier, Style};

pub fn error_style() -> Style {
    Style::default().bg(Color::DarkGray).fg(Color::Red)
}

pub fn header_style_default() -> Style {
    Style::default().bg(Color::Blue).fg(Color::Black)
}

pub fn header_style_sorted() -> Style {
    Style::default()
        .add_modifier(Modifier::BOLD)
        .bg(Color::Green)
        .fg(Color::Black)
}

pub fn prompt_input_style() -> Style {
    Style::default().fg(Color::Yellow)
}

pub fn selected_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

pub fn prompt_label_style() -> Style {
    Style::default().bg(Color::LightYellow).fg(Color::Black)
}
