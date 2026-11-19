//! The same time, source and selection rules are used by the recorder and native reader.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MIN_TIME: i64 = -62_135_596_800;
pub const MAX_TIME: i64 = 253_402_300_799;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub from: i64,
    pub until: i64,
}
impl Span {
    pub fn valid(self) -> bool {
        self.from >= MIN_TIME && self.until <= MAX_TIME && self.from < self.until
    }
    pub fn len(self) -> i64 {
        self.until - self.from
    }
    pub fn ending(end: i64, seconds: i64) -> Self {
        let len = seconds.clamp(1, MAX_TIME - MIN_TIME);
        let until = end.clamp(MIN_TIME + len, MAX_TIME);
        Self {
            from: until - len,
            until,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lane {
    Mic,
    System,
    Passive,
    Saves,
}
impl Lane {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mic => "Microphone",
            Self::System => "System audio",
            Self::Passive => "Passive screen text",
            Self::Saves => "Screenshots & clipboard",
        }
    }
    pub fn of(kind: &str, source: &str) -> Option<Self> {
        match (kind, source) {
            ("transcript" | "preview", "me") => Some(Self::Mic),
            ("transcript" | "preview", "remote") => Some(Self::System),
            ("screen", "screen" | "passive_screen" | "vision") => Some(Self::Passive),
            ("screen", "screenshot" | "clipboard") | ("note", "clipboard" | "me") => {
                Some(Self::Saves)
            }
            _ => None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Passage {
    pub id: i64,
    pub at: i64,
    pub end: i64,
    pub kind: String,
    pub source: String,
    pub text: String,
    pub image_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_image: Option<i64>,
    pub truncated: bool,
}
impl Passage {
    pub fn lane(&self) -> Lane {
        Lane::of(&self.kind, &self.source).expect("query filters timeline sources")
    }
    pub fn label(&self) -> &'static str {
        match (self.kind.as_str(), self.source.as_str()) {
            ("screen", "screenshot") => "Saved screenshot",
            ("screen", "clipboard") => "Pasted image",
            ("note", "clipboard") => "Pasted text",
            ("note", "me") => "Note",
            _ => self.lane().label(),
        }
    }
    pub fn body(&self) -> &str {
        if self.text.trim().is_empty() {
            self.label()
        } else {
            &self.text
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cluster {
    pub lane: Lane,
    pub range: Span,
    pub count: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub excerpt: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Window {
    pub range: Span,
    pub extent: Span,
    pub overview: Span,
    pub bands: Vec<Cluster>,
    pub passages: Vec<Passage>,
    pub clusters: Vec<Cluster>,
    pub quiet_at: Option<i64>,
    /// A bounded neighborhood for immediate redraws while a more precise window is loading.
    pub nearby: Vec<Passage>,
    pub nearby_grouped: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    Full,
    Skip,
    Chars([usize; 2]),
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Picks {
    pub lanes: BTreeSet<Lane>,
    pub entries: BTreeMap<i64, Pick>,
}
impl Picks {
    pub fn get(&self, p: &Passage) -> Option<Pick> {
        match self.entries.get(&p.id) {
            Some(Pick::Skip) => None,
            Some(v) => Some(v.clone()),
            None => self.lanes.contains(&p.lane()).then_some(Pick::Full),
        }
    }
}
