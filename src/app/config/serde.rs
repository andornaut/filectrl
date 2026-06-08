use ratatui::style::{Color, Modifier};
use serde::{Deserialize, Deserializer, de::Error, de::value::StringDeserializer};

/// Custom deserializer for Color that deserializes empty strings as None (inherit from parent)
pub fn deserialize_color<'de, D>(deserializer: D) -> Result<Option<Color>, D::Error>
where
    D: Deserializer<'de>,
{
    let color_str: String = Deserialize::deserialize(deserializer)?;
    if color_str.is_empty() {
        return Ok(None);
    }

    // For non-empty strings, use the built-in Color deserialization
    Color::deserialize(StringDeserializer::<D::Error>::new(color_str)).map(Some)
}

/// Deserializes a list of modifier names (e.g. `["bold", "italic"]`) into a `Modifier`.
/// An unrecognized name is a hard error so that a typo fails the config load
/// rather than being silently dropped.
pub fn deserialize_modifier<'de, D>(deserializer: D) -> Result<Modifier, D::Error>
where
    D: Deserializer<'de>,
{
    let modifiers: Vec<String> = Deserialize::deserialize(deserializer)?;

    let mut result = Modifier::empty();
    for m in &modifiers {
        result |= match m.to_lowercase().as_str() {
            "bold" => Modifier::BOLD,
            "dim" => Modifier::DIM,
            "italic" => Modifier::ITALIC,
            "underlined" => Modifier::UNDERLINED,
            "blink" => Modifier::SLOW_BLINK,
            "rapid_blink" => Modifier::RAPID_BLINK,
            "reversed" => Modifier::REVERSED,
            "crossed_out" => Modifier::CROSSED_OUT,
            other => {
                return Err(D::Error::custom(format!(
                    "Unknown modifier {other:?} (valid values: bold, dim, italic, underlined, blink, rapid_blink, reversed, crossed_out)"
                )));
            }
        };
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use test_case::test_case;

    #[derive(Deserialize)]
    struct ColorHolder {
        #[serde(deserialize_with = "deserialize_color")]
        color: Option<Color>,
    }

    #[derive(Deserialize)]
    struct ModifierHolder {
        #[serde(deserialize_with = "deserialize_modifier")]
        modifiers: Modifier,
    }

    fn color(toml: &str) -> Option<Color> {
        toml::from_str::<ColorHolder>(toml).unwrap().color
    }

    fn modifier(toml: &str) -> Modifier {
        toml::from_str::<ModifierHolder>(toml).unwrap().modifiers
    }

    fn try_modifier(toml: &str) -> Result<Modifier, toml::de::Error> {
        toml::from_str::<ModifierHolder>(toml).map(|h| h.modifiers)
    }

    #[test]
    fn empty_string_is_treated_as_no_color_so_it_inherits_from_parent() {
        assert_eq!(None, color(r#"color = """#));
    }

    #[test]
    fn named_color_deserializes() {
        assert_eq!(Some(Color::Red), color(r#"color = "Red""#));
    }

    #[test]
    fn hex_color_deserializes() {
        assert_eq!(
            Some(Color::Rgb(0xFF, 0x00, 0x00)),
            color(r##"color = "#FF0000""##)
        );
    }

    #[test]
    fn empty_modifier_list_produces_no_modifiers() {
        assert_eq!(Modifier::empty(), modifier("modifiers = []"));
    }

    #[test]
    fn single_modifier_deserializes() {
        assert_eq!(Modifier::BOLD, modifier(r#"modifiers = ["bold"]"#));
    }

    #[test]
    fn multiple_modifiers_are_combined() {
        let result = modifier(r#"modifiers = ["bold", "italic"]"#);
        assert_eq!(Modifier::BOLD | Modifier::ITALIC, result);
    }

    #[test_case("BOLD"   => Modifier::BOLD   ; "all caps")]
    #[test_case("Italic" => Modifier::ITALIC ; "title case")]
    fn modifier_name_is_case_insensitive(name: &str) -> Modifier {
        modifier(&format!(r#"modifiers = ["{name}"]"#))
    }

    #[test]
    fn unknown_modifier_is_an_error() {
        // "hidden" is not supported; the whole config load must fail rather than
        // silently dropping it.
        let result = try_modifier(r#"modifiers = ["bold", "hidden"]"#);
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("hidden"),
            "error should name the bad modifier: {err}"
        );
    }
}
