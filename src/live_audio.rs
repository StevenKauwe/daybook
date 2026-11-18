//! Bounded, process-local PCM. No raw audio is written to SQLite or disk.
use crate::store::{Result, Store};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
pub type Shared = Arc<Mutex<Inbox>>;
#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Accepted,
    Full,
}
#[derive(Default)]
pub struct Inbox {
    chunks: HashMap<String, Vec<u8>>,
    bytes: usize,
}
pub fn valid_path(path: &str) -> bool {
    path.strip_prefix("memory:").is_some_and(|s| {
        !s.is_empty() && s.len() <= 100 && s.bytes().all(|b| b.is_ascii_digit() || b == b'-')
    })
}
impl Inbox {
    pub fn insert(&mut self, path: &str, pcm: Vec<u8>, limit: usize) -> Result<Admission> {
        if !valid_path(path) || pcm.is_empty() || pcm.len() > 32000 || pcm.len() % 4 != 0 {
            return Err("invalid in-memory audio block".into());
        }
        if self.chunks.contains_key(path) {
            return Ok(Admission::Accepted);
        }
        let limit = limit.min(1000);
        if self.chunks.len() >= limit || self.bytes + pcm.len() > limit * 32000 {
            return Ok(Admission::Full);
        }
        self.bytes += pcm.len();
        self.chunks.insert(path.into(), pcm);
        Ok(Admission::Accepted)
    }
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        self.chunks.get(path).cloned().ok_or_else(|| {
            "in-memory audio is unavailable (process restarted or retention expired)".into()
        })
    }
    pub fn remove(&mut self, path: &str) {
        if let Some(p) = self.chunks.remove(path) {
            self.bytes -= p.len();
        }
    }
    pub fn reap(&mut self, s: &Store) -> Result<()> {
        let mut remove = Vec::new();
        for path in self.chunks.keys() {
            let live:i64=s.db.query_row("SELECT count(*) FROM sources WHERE path=?1 AND status IN ('pending','running','streaming')",[path],|r|r.get(0))?;
            if live == 0 {
                remove.push(path.clone());
            }
        }
        for p in remove {
            self.remove(&p);
        }
        Ok(())
    }
    pub fn stats(&self) -> serde_json::Value {
        serde_json::json!({"blocks":self.chunks.len(),"pcm_bytes":self.bytes,"audio_seconds":self.bytes as f64/64000.0})
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn variable_blocks_both_tracks_and_slow_consumer_stay_byte_bounded() {
        let mut q = Inbox::default();
        let mut expected = 0;
        for i in 0..120 {
            let len = if i % 2 == 0 { 32000 } else { 4096 };
            q.insert(&format!("memory:1-{i}"), vec![0; len], 120)
                .unwrap();
            expected += len;
        }
        assert_eq!(q.stats()["pcm_bytes"], expected);
        assert_eq!(q.stats()["blocks"], 120);
        assert_eq!(
            q.insert("memory:1-121", vec![0; 32000], 120).unwrap(),
            Admission::Full
        );
        for i in 0..120 {
            assert!(!q.read(&format!("memory:1-{i}")).unwrap().is_empty());
            q.remove(&format!("memory:1-{i}"));
        }
        assert_eq!(q.stats()["pcm_bytes"], 0);
        assert!(q.read("memory:1-1").is_err());
        assert!(!valid_path("memory:../../secret"));
        assert!(q.insert("memory:1", vec![0; 32004], 120).is_err());
    }
}
