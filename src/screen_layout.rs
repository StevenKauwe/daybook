//! Text and normalized Vision rectangles retained for one screen arrangement.
use serde::{Deserialize, Serialize};

pub type Bounds = [f32; 4];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub pixels: Option<[u32; 2]>,
    #[serde(default)]
    pub ocr_failed: bool,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub text: String,
    pub bounds: Option<Bounds>,
    pub words: Vec<(usize, Bounds)>,
}

impl Layout {
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
