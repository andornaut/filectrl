use ratatui::style::{Color, Modifier};

use super::theme::FileType;

pub(super) fn apply_ls_colors(theme: &mut FileType) {
    let ls_colors = match std::env::var("LS_COLORS") {
        Ok(value) => value,
        Err(_) => return,
    };

    for entry in ls_colors.split(':') {
        let parts: Vec<&str> = entry.split('=').collect();
        if parts.len() != 2 {
            continue;
        }

        let (key, value) = (parts[0], parts[1]);
        let (fg, bg, attrs) = parse(value);
        if fg == Color::Reset && bg == Color::Reset && attrs == Modifier::empty() {
            continue;
        }

        match key {
            "bd" => theme.set_block_device(fg, bg, attrs),
            "ca" => {} // capabilities not supported
            "cd" => theme.set_character_device(fg, bg, attrs),
            "di" => theme.set_directory(fg, bg, attrs),
            "do" => theme.set_door(fg, bg, attrs),
            "ex" => theme.set_executable(fg, bg, attrs),
            "fi" => theme.set_regular_file(fg, bg, attrs),
            "ln" => theme.set_symlink(fg, bg, attrs),
            "mi" => theme.set_missing(fg, bg, attrs),
            "no" => theme.set_normal_file(fg, bg, attrs),
            "or" => theme.set_symlink_broken(fg, bg, attrs),
            "ow" => theme.set_directory_other_writable(fg, bg, attrs),
            "pi" => theme.set_pipe(fg, bg, attrs),
            "sg" => theme.set_setgid(fg, bg, attrs),
            "so" => theme.set_socket(fg, bg, attrs),
            "st" => theme.set_directory_sticky(fg, bg, attrs),
            "tw" => theme.set_directory_sticky_other_writable(fg, bg, attrs),
            "su" => theme.set_setuid(fg, bg, attrs),

            // File patterns (both extensions and names)
            key if key.starts_with('*') => {
                theme.add_pattern_style(key, fg, bg, attrs);
            }
            _ => continue,
        }
    }
}

fn parse(line: &str) -> (Color, Color, Modifier) {
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut attrs = Modifier::empty();

    let codes: Vec<&str> = line.split(';').collect();
    let mut i = 0;

    while i < codes.len() {
        match codes[i] {
            // Text attributes
            "00" | "0" => attrs = Modifier::empty(), // Reset/Normal
            "01" | "1" => attrs |= Modifier::BOLD,   // Bold
            "02" | "2" => attrs |= Modifier::DIM,    // Dim
            "03" | "3" => attrs |= Modifier::ITALIC, // Italic
            "04" | "4" => attrs |= Modifier::UNDERLINED, // Underline
            "05" | "5" => attrs |= Modifier::SLOW_BLINK, // Blink
            "07" | "7" => attrs |= Modifier::REVERSED, // Reverse
            "08" | "8" => {}                         // Hidden - not supported

            // Foreground colors (30-37, 90-97)
            "30" => fg = Color::Black,
            "31" => fg = Color::Red,
            "32" => fg = Color::Green,
            "33" => fg = Color::Yellow,
            "34" => fg = Color::Blue,
            "35" => fg = Color::Magenta,
            "36" => fg = Color::Cyan,
            "37" => fg = Color::White,
            "90" => fg = Color::DarkGray,
            "91" => fg = Color::LightRed,
            "92" => fg = Color::LightGreen,
            "93" => fg = Color::LightYellow,
            "94" => fg = Color::LightBlue,
            "95" => fg = Color::LightMagenta,
            "96" => fg = Color::LightCyan,
            "97" => fg = Color::Gray,

            // Background colors (40-47, 100-107)
            "40" => bg = Color::Black,
            "41" => bg = Color::Red,
            "42" => bg = Color::Green,
            "43" => bg = Color::Yellow,
            "44" => bg = Color::Blue,
            "45" => bg = Color::Magenta,
            "46" => bg = Color::Cyan,
            "47" => bg = Color::White,
            "100" => bg = Color::DarkGray,
            "101" => bg = Color::LightRed,
            "102" => bg = Color::LightGreen,
            "103" => bg = Color::LightYellow,
            "104" => bg = Color::LightBlue,
            "105" => bg = Color::LightMagenta,
            "106" => bg = Color::LightCyan,
            "107" => bg = Color::Gray,

            // Extended color codes
            "38" => {
                if let Some((color, skip)) = parse_extended_color(&codes, i) {
                    fg = color;
                    i += skip;
                }
            }
            "48" => {
                if let Some((color, skip)) = parse_extended_color(&codes, i) {
                    bg = color;
                    i += skip;
                }
            }

            _ => {}
        }

        i += 1; // Move to next code
    }

    (fg, bg, attrs)
}

fn parse_extended_color(codes: &[&str], i: usize) -> Option<(Color, usize)> {
    const MODE_256: &str = "5";
    const MODE_RGB: &str = "2";
    const VALUES_256: usize = 2; // Mode discriminator + color index
    const VALUES_RGB: usize = 4; // Mode discriminator + R + G + B values

    if i + VALUES_256 < codes.len() && codes[i + 1] == MODE_256 {
        // Check for 256 color mode (format: 38;5;N)
        if let Ok(n) = codes[i + 2].parse::<u8>() {
            return Some((Color::Indexed(n), VALUES_256));
        }
    } else if i + VALUES_RGB < codes.len() && codes[i + 1] == MODE_RGB {
        // Check for RGB color mode (format: 38;2;R;G;B)
        if let (Ok(r), Ok(g), Ok(b)) = (
            codes[i + 2].parse::<u8>(),
            codes[i + 3].parse::<u8>(),
            codes[i + 4].parse::<u8>(),
        ) {
            return Some((Color::Rgb(r, g, b), VALUES_RGB));
        }
    }
    None
}
