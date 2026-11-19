use serde::{Deserialize, Serialize};

/// Identity of one attempted observation. Window identifiers stay in memory.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScreenSample {
    pub session: String,
    pub sequence: u64,
    pub target: String,
    pub at: i64,
}

impl ScreenSample {
    pub fn follows(&self, last: &Self, interval: u64) -> bool {
        self.session == last.session
            && self.target == last.target
            && last.sequence.checked_add(1) == Some(self.sequence)
            && self.at >= last.at
            && self.at.abs_diff(last.at) <= interval + (interval / 10).max(2)
    }
}
