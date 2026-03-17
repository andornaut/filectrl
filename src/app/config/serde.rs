use ratatui::style::{Color, Modifier};
use serde::{de::value::StringDeserializer, Deserialize, Deserializer};

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
    Color::deserialize(StringDeserializer::<D::Error>::new(color_str))
        .map(Some)
}

// Private newtype wrapper around Modifier just for deserialization
struct ModifierWrapper(pub Modifier);

impl<'de> Deserialize<'de> for ModifierWrapper {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let modifiers: Vec<String> = Deserialize::deserialize(deserializer)?;

        // Start with empty modifier and add each specified modifier
        let modifier = modifiers.iter().fold(Modifier::empty(), |acc, m| {
            match m.to_lowercase().as_str() {
                "bold" => acc | Modifier::BOLD,
                "dim" => acc | Modifier::DIM,
                "italic" => acc | Modifier::ITALIC,
                "underlined" => acc | Modifier::UNDERLINED,
                "blink" => acc | Modifier::SLOW_BLINK,
                "rapid_blink" => acc | Modifier::RAPID_BLINK,
                "reversed" => acc | Modifier::REVERSED,
                "crossed_out" => acc | Modifier::CROSSED_OUT,
                _ => acc, // Ignore unknown and unsupported (e.g. "hidden") modifiers
            }
        });

        Ok(ModifierWrapper(modifier))
    }
}

/// Helper function for deserializing Modifier
pub fn deserialize_modifier<'de, D>(deserializer: D) -> Result<Modifier, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(ModifierWrapper::deserialize(deserializer)?.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

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
        assert_eq!(Some(Color::Rgb(0xFF, 0x00, 0x00)), color(r##"color = "#FF0000""##));
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

    #[test]
    fn modifier_names_are_case_insensitive() {
        assert_eq!(Modifier::BOLD, modifier(r#"modifiers = ["BOLD"]"#));
        assert_eq!(Modifier::ITALIC, modifier(r#"modifiers = ["Italic"]"#));
    }

    #[test]
    fn unknown_modifier_is_silently_ignored_and_known_ones_still_apply() {
        // "hidden" is not supported; "bold" should still take effect
        let result = modifier(r#"modifiers = ["bold", "hidden"]"#);
        assert_eq!(Modifier::BOLD, result);
    }
}
