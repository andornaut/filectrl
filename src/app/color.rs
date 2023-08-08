use ratatui::style::{Color, Style};

#[derive(Copy, Clone)]
pub enum ColorTheme {
    Bg,
    BorderFg,
    Fg,
    ErrorFg,
}

const BLACK: Color = Color::Rgb(21, 21, 21);
const _GREEN: Color = Color::Rgb(102, 175, 51);
const _GREEN_LIGHT: Color = Color::Rgb(153, 255, 153);
const GREY: Color = Color::Rgb(104, 104, 104);
const _GREY_DARK: Color = Color::Rgb(51, 51, 51);
const _GREY_LIGHT: Color = Color::Rgb(204, 204, 204);
const _GREY_MEDIUM_LIGHT: Color = Color::Rgb(153, 153, 153);
const _PINK: Color = Color::Rgb(175, 51, 175);
const _PINK_LIGHT: Color = Color::Rgb(255, 153, 255);
const RED: Color = Color::Rgb(204, 25, 25);

impl From<ColorTheme> for Color {
    fn from(color_theme: ColorTheme) -> Self {
        match color_theme {
            ColorTheme::Bg => BLACK,
            ColorTheme::BorderFg => GREY,
            ColorTheme::Fg => GREY,
            ColorTheme::ErrorFg => RED,
        }
    }
}

pub fn error_style() -> Style {
    Style::default().bg(Color::DarkGray).fg(Color::Red)
}
