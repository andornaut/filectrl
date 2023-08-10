use anyhow::anyhow;
use anyhow::Result;
use ratatui::style::Color;
use regex::Regex;

pub fn black() -> Color {
    hex_to_color("#1d1f21").unwrap()
}
pub fn dark_brown() -> Color {
    hex_to_color("#373424").unwrap()
}
pub fn brown() -> Color {
    hex_to_color("#423f2e").unwrap()
}
pub fn light_brown() -> Color {
    hex_to_color("#777755").unwrap()
}
pub fn dark_grey() -> Color {
    hex_to_color("#555").unwrap()
}
pub fn blue() -> Color {
    hex_to_color("#80a0ff").unwrap()
}

pub fn dark_blue() -> Color {
    hex_to_color("#5c6886").unwrap()
}

pub fn dark_green() -> Color {
    hex_to_color("#5c8668").unwrap()
}

pub fn cyan() -> Color {
    hex_to_color("#70c0b1").unwrap()
}

pub fn light_blue() -> Color {
    hex_to_color("#81a2be").unwrap()
}

pub fn light_orange() -> Color {
    hex_to_color("#f0c674").unwrap()
}

pub fn light_grey() -> Color {
    hex_to_color("#c5c8c6").unwrap()
}

pub fn light_purple() -> Color {
    hex_to_color("#b294bb").unwrap()
}

pub fn light_salmon() -> Color {
    hex_to_color("#cc6666").unwrap()
}

const COLOR_HEX: &str = "^[a-f0-9]{3}([a-f0-9]{3})?$";

fn hex_to_color(value: &str) -> Result<Color> {
    let value = value.trim().trim_start_matches('#').to_lowercase();
    let re = Regex::new(COLOR_HEX).unwrap();
    if !re.is_match(&value) {
        return Err(anyhow!("Must be valid hex color code, such as #aabbcc"));
    }

    // Inspired by https://github.com/uttarayan21/color-to-tui
    let (r, g, b);
    match value.len() {
        3 => {
            r = from_3_chars_to_bytes(&value[0..1]);
            g = from_3_chars_to_bytes(&value[1..2]);
            b = from_3_chars_to_bytes(&value[2..3]);
        }
        6 => {
            r = from_6_chars_to_bytes(&value[0..2]);
            g = from_6_chars_to_bytes(&value[2..4]);
            b = from_6_chars_to_bytes(&value[4..6]);
        }
        _ => unreachable!("The length was previously validated to be 3 or 6"),
    }
    Ok(Color::Rgb(r, g, b))
}

fn from_3_chars_to_bytes(src: &str) -> u8 {
    from_6_chars_to_bytes(src) * 17
}

fn from_6_chars_to_bytes(src: &str) -> u8 {
    u8::from_str_radix(src, 16).expect("src was previously validate to be a valid color hex code")
}
