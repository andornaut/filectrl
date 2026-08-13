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
            "05" | "5" => attrs |= Modifier::SLOW_BLINK, // Blink
            "06" | "6" => attrs |= Modifier::RAPID_BLINK, // Rapid blink
            "07" | "7" => attrs |= Modifier::REVERSED, // Reverse
            "08" | "8" => {}                         // Hidden - not supported
            "09" | "9" => attrs |= Modifier::CROSSED_OUT, // Crossed out / strikethrough

            // Foreground colors (30-37, 90-97)
            "30" => fg = Some(Color::Black),
            "31" => fg = Some(Color::Red),
            "32" => fg = Some(Color::Green),
            "33" => fg = Some(Color::Yellow),
            "34" => fg = Some(Color::Blue),
            "35" => fg = Some(Color::Magenta),
            "36" => fg = Some(Color::Cyan),
            "37" => fg = Some(Color::Gray),
            "90" => fg = Some(Color::DarkGray),
            "91" => fg = Some(Color::LightRed),
            "92" => fg = Some(Color::LightGreen),
            "93" => fg = Some(Color::LightYellow),
            "94" => fg = Some(Color::LightBlue),
            "95" => fg = Some(Color::LightMagenta),
            "96" => fg = Some(Color::LightCyan),
            "97" => fg = Some(Color::White),

            // Background colors (40-47, 100-107)
            "40" => bg = Some(Color::Black),
            "41" => bg = Some(Color::Red),
            "42" => bg = Some(Color::Green),
            "43" => bg = Some(Color::Yellow),
            "44" => bg = Some(Color::Blue),
            "45" => bg = Some(Color::Magenta),
            "46" => bg = Some(Color::Cyan),
            "47" => bg = Some(Color::Gray),
            "100" => bg = Some(Color::DarkGray),
            "101" => bg = Some(Color::LightRed),
            "102" => bg = Some(Color::LightGreen),
            "103" => bg = Some(Color::LightYellow),
            "104" => bg = Some(Color::LightBlue),
            "105" => bg = Some(Color::LightMagenta),
            "106" => bg = Some(Color::LightCyan),
            "107" => bg = Some(Color::White),

            // Extended color codes
            "38" => {
                let (color, skip) = parse_extended_color(&codes, i);
                if let Some(color) = color {
                    fg = Some(color);
                }
                i += skip;
            }
            "48" => {
                let (color, skip) = parse_extended_color(&codes, i);
                if let Some(color) = color {
                    bg = Some(color);
                }
                i += skip;
            }

            _ => {}
        }

        i += 1; // Move to next code
    }

    (fg, bg, attrs)
}

/// Parses an extended color sequence starting at `codes[i]` ("38"/"48").
/// Returns the color (when the sequence is complete and valid) and the number
/// of extra codes consumed. The whole parameter group is consumed (clamped to
/// the slice end) even when its values are invalid, so they are not
/// reinterpreted as standalone SGR codes by the caller.
fn parse_extended_color(codes: &[&str], i: usize) -> (Option<Color>, usize) {
    const MODE_256: &str = "5";
    const MODE_RGB: &str = "2";
    const VALUES_256: usize = 2; // Mode discriminator + color index
    const VALUES_RGB: usize = 4; // Mode discriminator + R + G + B values

    let Some(&mode) = codes.get(i + 1) else {
        return (None, 0);
    };
    let remaining = codes.len() - i - 1;
    let parse_u8 = |offset: usize| codes.get(i + offset)?.parse::<u8>().ok();

    match mode {
        // 256 color mode (format: 38;5;N)
        MODE_256 => (parse_u8(2).map(Color::Indexed), VALUES_256.min(remaining)),
        // RGB color mode (format: 38;2;R;G;B)
        MODE_RGB => {
            let color = (|| Some(Color::Rgb(parse_u8(2)?, parse_u8(3)?, parse_u8(4)?)))();
            (color, VALUES_RGB.min(remaining))
        }
        // Unrecognized mode byte: consume it so it is not misread as a code
        _ => (None, 1),
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    type Style = (Option<Color>, Option<Color>, Modifier);

    const NONE: Style = (None, None, Modifier::empty());

    fn style(fg: Option<Color>, bg: Option<Color>, attrs: Modifier) -> Style {
        (fg, bg, attrs)
    }

    // ratatui's `Gray` is ANSI 7 (normal white) and `White` is ANSI 15 (bright
    // white), so the normal codes (37/47) must map to the dimmer one.
    #[test_case("31" => style(Some(Color::Red), None, Modifier::empty()) ; "standard foreground")]
    #[test_case("41" => style(None, Some(Color::Red), Modifier::empty()) ; "standard background")]
    #[test_case("91" => style(Some(Color::LightRed), None, Modifier::empty()) ; "bright foreground")]
    #[test_case("37" => style(Some(Color::Gray), None, Modifier::empty()) ; "normal white foreground is Gray")]
    #[test_case("97" => style(Some(Color::White), None, Modifier::empty()) ; "bright white foreground is White")]
    #[test_case("47" => style(None, Some(Color::Gray), Modifier::empty()) ; "normal white background is Gray")]
    #[test_case("107" => style(None, Some(Color::White), Modifier::empty()) ; "bright white background is White")]
    #[test_case("32;42" => style(Some(Color::Green), Some(Color::Green), Modifier::empty()) ; "foreground then background")]
    #[test_case("01" => style(None, None, Modifier::BOLD) ; "bold")]
    #[test_case("06" => style(None, None, Modifier::RAPID_BLINK) ; "rapid blink")]
    #[test_case("09" => style(None, None, Modifier::CROSSED_OUT) ; "crossed out")]
    #[test_case("01;32" => style(Some(Color::Green), None, Modifier::BOLD) ; "modifier then foreground")]
    #[test_case("01;00" => NONE ; "reset clears the modifiers set before it")]
    #[test_case("38;5;200" => style(Some(Color::Indexed(200)), None, Modifier::empty()) ; "extended 256 foreground")]
    #[test_case("48;5;100" => style(None, Some(Color::Indexed(100)), Modifier::empty()) ; "extended 256 background")]
    #[test_case("38;2;255;128;0" => style(Some(Color::Rgb(255, 128, 0)), None, Modifier::empty()) ; "extended rgb foreground")]
    #[test_case("48;2;0;64;128" => style(None, Some(Color::Rgb(0, 64, 128)), Modifier::empty()) ; "extended rgb background")]
    // The index of an extended color must not be consumed as a code of its own.
    #[test_case("38;5;200;01" => style(Some(Color::Indexed(200)), None, Modifier::BOLD) ; "extended color then modifier")]
    // The trailing "0" belongs to the malformed group, so it is not the reset code.
    #[test_case("01;38;2;255;bad;0" => style(None, None, Modifier::BOLD) ; "a malformed group does not reset the modifiers")]
    fn parse_produces(line: &str) -> Style {
        parse(line)
    }

    // A malformed extended-color group is skipped whole, so the codes inside it
    // are never read as standalone colors or modifiers.
    #[test_case("" ; "empty input")]
    #[test_case("99" ; "unknown code")]
    #[test_case("38;5" ; "256 sequence missing its index")]
    #[test_case("38;5;300" ; "256 index out of range")]
    #[test_case("38;2;255" ; "rgb sequence missing green and blue")]
    #[test_case("38;2;300;31;40" ; "rgb component out of range")]
    fn parse_produces_no_style(line: &str) {
        assert_eq!(NONE, parse(line));
    }
}
