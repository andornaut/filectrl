use ratatui::style::{Color, Modifier};
use serde::{de::value::StringDeserializer, Deserialize, Deserializer, Serialize, Serializer};

/// Custom deserializer for Color that deserializes empty strings as the default color (reset)
pub fn deserialize_color<'de, D>(deserializer: D) -> Result<Color, D::Error>
where
    D: Deserializer<'de>,
{
    let color_str: String = Deserialize::deserialize(deserializer)?;
    if color_str.is_empty() {
        return Ok(Color::Reset);
    }

    // For non-empty strings, use the built-in Color deserialization
    Color::deserialize(StringDeserializer::<D::Error>::new(color_str))
}

/// Custom serializer for Color that serializes Reset as empty string
pub fn serialize_color<S>(color: &Color, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match color {
        Color::Reset => "".serialize(serializer),
        _ => color.serialize(serializer),
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
