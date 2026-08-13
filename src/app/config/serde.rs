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
    use serde::Deserialize;
    use test_case::test_case;

    use super::*;

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

    fn color(value: &str) -> Option<Color> {
        toml::from_str::<ColorHolder>(&format!("color = {value}"))
            .unwrap()
            .color
    }

    fn try_modifier(list: &str) -> Result<Modifier, toml::de::Error> {
        toml::from_str::<ModifierHolder>(&format!("modifiers = {list}")).map(|h| h.modifiers)
    }

    // An empty string means "unset", so the style inherits from its parent
    // rather than resolving to a color of its own.
    #[test_case(r#""""# => None ; "empty string inherits")]
    #[test_case(r#""Red""# => Some(Color::Red) ; "named")]
    #[test_case(r##""#FF0000""## => Some(Color::Rgb(0xFF, 0x00, 0x00)) ; "hex")]
    fn color_deserializes(value: &str) -> Option<Color> {
        color(value)
    }

    #[test_case("[]" => Modifier::empty() ; "empty list")]
    #[test_case(r#"["bold"]"# => Modifier::BOLD ; "one modifier")]
    #[test_case(r#"["bold", "italic"]"# => Modifier::BOLD | Modifier::ITALIC ; "combined")]
    #[test_case(r#"["BOLD"]"# => Modifier::BOLD ; "all caps")]
    #[test_case(r#"["Italic"]"# => Modifier::ITALIC ; "title case")]
    fn modifier_deserializes(list: &str) -> Modifier {
        try_modifier(list).unwrap()
    }

    #[test]
    fn an_unknown_modifier_fails_the_load() {
        // "hidden" is not supported, and the whole config must fail rather than
        // silently dropping it.
        let error = try_modifier(r#"["bold", "hidden"]"#)
            .expect_err("an unknown modifier should be rejected")
            .to_string();
        assert!(
            error.contains("hidden"),
            "error should name the bad modifier: {error}"
        );
    }
}
