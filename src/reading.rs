use serde::{Deserialize, Serialize};

pub fn entry_label(kind: &str, source: &str) -> &'static str {
    match (kind, source) {
        ("note", "clipboard") => "Clipboard note",
        ("note", _) => "Your note",
        ("screen", "clipboard") => "Copied image",
        ("screen", "screenshot") => "Saved screenshot",
        ("screen", "passive_screen") => "Passive screen text",
        ("screen", "vision") => "Screen description",
        ("screen", _) => "On screen",
        ("transcript_suppressed", _) => "Suppressed audio",
        ("preview_abandoned", _) => "Unfinished passage",
        ("preview", "me") => "You · transcribing",
        ("preview", _) => "System audio · transcribing",
        ("transcript", "me") => "You said",
        ("transcript", _) => "System audio",
        _ => "Activity",
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ReadingStyle {
    pub size: u8,
    pub journal: bool,
    pub night: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub passive_screen: bool,
}

impl Default for ReadingStyle {
    fn default() -> Self {
        Self {
            size: 23,
            journal: true,
            night: false,
            passive_screen: false,
        }
    }
}

impl ReadingStyle {
    pub fn validate(self) -> Result<Self, &'static str> {
        if (16..=33).contains(&self.size) {
            Ok(self)
        } else {
            Err("Reading size must be 16–33 points")
        }
    }
    pub fn saved(value: &str) -> Self {
        serde_json::from_str::<Self>(value)
            .ok()
            .and_then(|s| s.validate().ok())
            .unwrap_or_default()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_saved_styles_fall_back_and_valid_choices_round_trip() {
        assert_eq!(ReadingStyle::saved(""), ReadingStyle::default());
        assert_eq!(
            ReadingStyle::saved(r#"{"size":250}"#),
            ReadingStyle::default()
        );
        let choice = ReadingStyle {
            size: 27,
            journal: false,
            night: true,
            passive_screen: true,
        };
        assert_eq!(
            ReadingStyle::saved(&serde_json::to_string(&choice).unwrap()),
            choice
        );
    }
}
