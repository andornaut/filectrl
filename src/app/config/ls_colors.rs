use ratatui::style::{Color, Modifier};

pub(super) fn parse(line: &str) -> (Option<Color>, Option<Color>, Modifier) {
    let mut fg: Option<Color> = None;
    let mut bg: Option<Color> = None;
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
            "05" | "5" => attrs |= Modifier::SLOW_BLINK,  // Blink
            "06" | "6" => attrs |= Modifier::RAPID_BLINK, // Rapid blink
            "07" | "7" => attrs |= Modifier::REVERSED,    // Reverse
            "08" | "8" => {}                               // Hidden - not supported
            "09" | "9" => attrs |= Modifier::CROSSED_OUT, // Crossed out / strikethrough

            // Foreground colors (30-37, 90-97)
            "30" => fg = Some(Color::Black),
            "31" => fg = Some(Color::Red),
            "32" => fg = Some(Color::Green),
            "33" => fg = Some(Color::Yellow),
            "34" => fg = Some(Color::Blue),
            "35" => fg = Some(Color::Magenta),
            "36" => fg = Some(Color::Cyan),
            "37" => fg = Some(Color::White),
            "90" => fg = Some(Color::DarkGray),
            "91" => fg = Some(Color::LightRed),
            "92" => fg = Some(Color::LightGreen),
            "93" => fg = Some(Color::LightYellow),
            "94" => fg = Some(Color::LightBlue),
            "95" => fg = Some(Color::LightMagenta),
            "96" => fg = Some(Color::LightCyan),
            "97" => fg = Some(Color::Gray),

            // Background colors (40-47, 100-107)
            "40" => bg = Some(Color::Black),
            "41" => bg = Some(Color::Red),
            "42" => bg = Some(Color::Green),
            "43" => bg = Some(Color::Yellow),
            "44" => bg = Some(Color::Blue),
            "45" => bg = Some(Color::Magenta),
            "46" => bg = Some(Color::Cyan),
            "47" => bg = Some(Color::White),
            "100" => bg = Some(Color::DarkGray),
            "101" => bg = Some(Color::LightRed),
            "102" => bg = Some(Color::LightGreen),
            "103" => bg = Some(Color::LightYellow),
            "104" => bg = Some(Color::LightBlue),
            "105" => bg = Some(Color::LightMagenta),
            "106" => bg = Some(Color::LightCyan),
            "107" => bg = Some(Color::Gray),

            // Extended color codes
            "38" => {
                if let Some((color, skip)) = parse_extended_color(&codes, i) {
                    fg = Some(color);
                    i += skip;
                } else if i + 1 < codes.len() {
                    // Consume the mode byte ("5" or "2") so the outer loop's
                    // i += 1 doesn't land on it and misread it as a modifier.
                    i += 1;
                }
            }
            "48" => {
                if let Some((color, skip)) = parse_extended_color(&codes, i) {
                    bg = Some(color);
                    i += skip;
                } else if i + 1 < codes.len() {
                    i += 1;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_produces_no_style() {
        let (fg, bg, attrs) = parse("");
        assert!(fg.is_none());
        assert!(bg.is_none());
        assert!(attrs.is_empty());
    }

    #[test]
    fn unknown_code_is_silently_ignored() {
        let (fg, bg, attrs) = parse("99");
        assert!(fg.is_none());
        assert!(bg.is_none());
        assert!(attrs.is_empty());
    }

    #[test]
    fn standard_foreground_color() {
        let (fg, bg, attrs) = parse("31");
        assert_eq!(Some(Color::Red), fg);
        assert!(bg.is_none());
        assert!(attrs.is_empty());
    }

    #[test]
    fn standard_background_color() {
        let (fg, bg, attrs) = parse("41");
        assert!(fg.is_none());
        assert_eq!(Some(Color::Red), bg);
        assert!(attrs.is_empty());
    }

    #[test]
    fn bright_foreground_color() {
        let (fg, _, _) = parse("91");
        assert_eq!(Some(Color::LightRed), fg);
    }

    #[test]
    fn bold_modifier() {
        let (_, _, attrs) = parse("01");
        assert_eq!(Modifier::BOLD, attrs);
    }

    #[test]
    fn rapid_blink_modifier() {
        let (_, _, attrs) = parse("06");
        assert_eq!(Modifier::RAPID_BLINK, attrs);
    }

    #[test]
    fn crossed_out_modifier() {
        let (_, _, attrs) = parse("09");
        assert_eq!(Modifier::CROSSED_OUT, attrs);
    }

    #[test]
    fn combined_fg_and_modifier() {
        let (fg, _, attrs) = parse("01;32");
        assert_eq!(Some(Color::Green), fg);
        assert_eq!(Modifier::BOLD, attrs);
    }

    #[test]
    fn reset_code_clears_modifiers_set_before_it() {
        // Bold is set, then reset — the final result should have no modifiers
        let (_, _, attrs) = parse("01;00");
        assert!(attrs.is_empty());
    }

    #[test]
    fn fg_then_bg_are_both_captured() {
        let (fg, bg, _) = parse("32;42");
        assert_eq!(Some(Color::Green), fg);
        assert_eq!(Some(Color::Green), bg);
    }

    #[test]
    fn extended_256_foreground() {
        let (fg, _, _) = parse("38;5;200");
        assert_eq!(Some(Color::Indexed(200)), fg);
    }

    #[test]
    fn extended_256_background() {
        let (_, bg, _) = parse("48;5;100");
        assert_eq!(Some(Color::Indexed(100)), bg);
    }

    #[test]
    fn extended_rgb_foreground() {
        let (fg, _, _) = parse("38;2;255;128;0");
        assert_eq!(Some(Color::Rgb(255, 128, 0)), fg);
    }

    #[test]
    fn extended_rgb_background() {
        let (_, bg, _) = parse("48;2;0;64;128");
        assert_eq!(Some(Color::Rgb(0, 64, 128)), bg);
    }

    #[test]
    fn truncated_256_sequence_produces_no_style() {
        // "38;5" is missing the color index — should produce nothing
        let (fg, bg, attrs) = parse("38;5");
        assert!(fg.is_none());
        assert!(bg.is_none());
        assert!(attrs.is_empty());
    }

    #[test]
    fn truncated_rgb_sequence_produces_no_style() {
        // "38;2;255" is missing G and B — should produce nothing
        let (fg, bg, attrs) = parse("38;2;255");
        assert!(fg.is_none());
        assert!(bg.is_none());
        assert!(attrs.is_empty());
    }

    #[test]
    fn extended_color_followed_by_modifier_both_apply() {
        // 38;5;200 (indexed fg) then 01 (bold) — the i += skip must not consume the bold code
        let (fg, _, attrs) = parse("38;5;200;01");
        assert_eq!(Some(Color::Indexed(200)), fg);
        assert_eq!(Modifier::BOLD, attrs);
    }
}
