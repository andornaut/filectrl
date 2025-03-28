use ratatui::style::{Color, Modifier};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Custom deserializer for Option<Color> that deserializes empty strings as None
pub fn deserialize_optional_color<'de, D>(deserializer: D) -> Result<Option<Color>, D::Error>
where
    D: Deserializer<'de>,
{
    let color_str: String = Deserialize::deserialize(deserializer)?;
    if color_str.is_empty() {
        return Ok(None);
    }

    // For non-empty strings, use the built-in Color deserialization
    match Color::deserialize(serde::de::value::StringDeserializer::<D::Error>::new(
        color_str,
    )) {
        Ok(color) => Ok(Some(color)),
        Err(e) => Err(e),
    }
}

/// Custom serializer for Option<Color> that serializes None as empty string
pub fn serialize_optional_color<S>(color: &Option<Color>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match color {
        Some(color) => color.serialize(serializer),
        None => "".serialize(serializer),
    }
}

// Private newtype wrapper around Modifier just for de/serialization
struct ModifierWrapper(pub Modifier);

impl Serialize for ModifierWrapper {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Convert Modifier to a string array of enabled modifiers
        let mut modifiers = Vec::new();

        // We ignore Modifier::HIDDEN, because we don't want to hide any text
        if self.0.contains(Modifier::BOLD) {
            modifiers.push("bold");
        }
        if self.0.contains(Modifier::DIM) {
            modifiers.push("dim");
        }
        if self.0.contains(Modifier::ITALIC) {
            modifiers.push("italic");
        }
        if self.0.contains(Modifier::UNDERLINED) {
            modifiers.push("underlined");
        }
        if self.0.contains(Modifier::SLOW_BLINK) {
            modifiers.push("blink");
        }
        if self.0.contains(Modifier::RAPID_BLINK) {
            modifiers.push("rapid_blink");
        }
        if self.0.contains(Modifier::REVERSED) {
            modifiers.push("reversed");
        }
        if self.0.contains(Modifier::CROSSED_OUT) {
            modifiers.push("crossed_out");
        }

        // Serialize as array of strings
        modifiers.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ModifierWrapper {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let modifiers: Vec<String> = Deserialize::deserialize(deserializer)?;

        let mut modifier = Modifier::empty();

        for m in modifiers {
            match m.to_lowercase().as_str() {
                "bold" => modifier |= Modifier::BOLD,
                "dim" => modifier |= Modifier::DIM,
                "italic" => modifier |= Modifier::ITALIC,
                "underlined" => modifier |= Modifier::UNDERLINED,
                "blink" => modifier |= Modifier::SLOW_BLINK,
                "rapid_blink" => modifier |= Modifier::RAPID_BLINK,
                "reversed" => modifier |= Modifier::REVERSED,
                "crossed_out" => modifier |= Modifier::CROSSED_OUT,
                _ => {} // Ignore unknown and unsupported (e.g. "hidden") modifiers
            }
        }

        Ok(ModifierWrapper(modifier))
    }
}

/// Helper function for serializing Modifier
pub fn serialize_modifier<S>(modifier: &Modifier, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    ModifierWrapper(*modifier).serialize(serializer)
}

/// Helper function for deserializing Modifier
pub fn deserialize_modifier<'de, D>(deserializer: D) -> Result<Modifier, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(ModifierWrapper::deserialize(deserializer)?.0)
}
