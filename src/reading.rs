use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ReadingStyle {
    pub size: u8,
    pub journal: bool,
    pub night: bool,
}

impl Default for ReadingStyle {
    fn default() -> Self {
        Self {
            size: 23,
            journal: true,
            night: false,
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
        };
        assert_eq!(
            ReadingStyle::saved(&serde_json::to_string(&choice).unwrap()),
            choice
        );
    }
}
