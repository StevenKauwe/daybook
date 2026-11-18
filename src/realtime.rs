//! Ephemeral meter state. No SQLite writes, audio storage, or growing event queue.
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
pub type Shared = Arc<Mutex<Meters>>;
#[derive(Default)]
pub struct Meters {
    epoch: u64,
    active: bool,
    starting: bool,
    stopping: bool,
    armed: bool,
    levels: [Option<Value>; 2],
    health: Value,
}
impl Meters {
    pub fn update(&mut self, event: &Value) {
        match event["type"].as_str() {
            Some("state") => {
                let epoch = event["epoch"].as_u64().unwrap_or(0);
                self.active = event["active"] == true;
                self.starting = event["starting"] == true;
                self.stopping = event["stopping"] == true;
                self.armed = event["armed"] == true;
                if epoch != self.epoch || (!self.active && !self.starting) {
                    self.levels = [None, None];
                }
                self.epoch = epoch;
            }
            Some("meters") if self.active || self.starting => {
                for (i, track) in ["me", "remote"].iter().enumerate() {
                    let r = &event[*track];
                    let (Some(rms), Some(peak), Some(end), Some(rate), Some(frames)) = (
                        r["rms_dbfs"].as_f64(),
                        r["peak_dbfs"].as_f64(),
                        r["end_at"].as_f64(),
                        r["sample_rate"].as_u64(),
                        r["frames"].as_u64(),
                    ) else {
                        continue;
                    };
                    if r["epoch"].as_u64() != Some(self.epoch)
                        || !(-120.0..=0.0).contains(&rms)
                        || !(rms..=0.0).contains(&peak)
                        || !end.is_finite()
                        || !(8000..=192000).contains(&rate)
                        || !(1..=32768).contains(&frames)
                    {
                        continue;
                    }
                    if self.levels[i]
                        .as_ref()
                        .and_then(|p| p["end_at"].as_f64())
                        .is_some_and(|old| old >= end)
                    {
                        continue;
                    }
                    // Whitelist scalar fields, so even a malformed event cannot grow this cache.
                    let method = if r["method"] == "microphone_io" {
                        "microphone_io"
                    } else {
                        "audio_consumer"
                    };
                    self.levels[i] = Some(
                        json!({"rms_dbfs":rms,"peak_dbfs":peak,"end_at":end,"frames":frames,"sample_rate":rate,"window_ms":frames as f64/rate as f64*1000.0,"method":method}),
                    );
                }
            }
            Some("meter_health") => {
                self.health = json!({"microphone_io_active":event["microphone_io_active"]==true,"callbacks":event["callbacks"].as_u64(),"callback_frames":event["callback_frames"].as_u64(),"sample_rate":event["sample_rate"].as_u64(),"skipped_meter_callbacks":event["skipped_meter_callbacks"].as_u64()});
            }
            Some("bridge_exit") => {
                *self = Self::default();
            }
            _ => {}
        }
    }
    pub fn snapshot(&self) -> Value {
        json!({"active":self.active.to_string(),"armed":self.armed.to_string(),"capture_starting":self.starting.to_string(),"capture_stopping":self.stopping.to_string(),"epoch":self.epoch,"health":{"input_levels":{"me":self.levels[0],"remote":self.levels[1]},"meter_input":self.health},"publish_interval_ms":16})
    }
    pub fn synthetic_state(&mut self, active: bool) {
        self.update(&json!({"type":"state","epoch":self.epoch+1,"active":active,"armed":active}));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_short_hardware_windows_and_rejects_unbounded_or_empty_ones() {
        let mut m = Meters::default();
        m.synthetic_state(true);
        for (i, frames) in [64, 128, 256, 512].iter().enumerate() {
            m.update(&json!({"type":"meters","me":{"epoch":1,"rms_dbfs":-20,"peak_dbfs":-10,"end_at":i,"frames":frames,"sample_rate":48000,"method":"microphone_io"}}));
            let s = m.snapshot();
            assert_eq!(s["health"]["input_levels"]["me"]["frames"], *frames);
            assert_eq!(s["health"]["input_levels"]["me"]["method"], "microphone_io");
        }
        for frames in [0, 32769] {
            m.update(&json!({"type":"meters","me":{"epoch":1,"rms_dbfs":-20,"peak_dbfs":-10,"end_at":10,"frames":frames,"sample_rate":48000}}));
        }
        assert_eq!(m.snapshot()["health"]["input_levels"]["me"]["frames"], 512);
    }
    #[test]
    fn coalesces_and_rejects_old_sessions_invalid_levels_and_late_stop_events() {
        let mut m = Meters::default();
        m.synthetic_state(true);
        for i in 0..10000 {
            m.update(&json!({"type":"meters","me":{"epoch":1,"rms_dbfs":-20,"peak_dbfs":-10,"end_at":i,"frames":320,"sample_rate":16000}}));
        }
        assert_eq!(
            m.snapshot()["health"]["input_levels"]["me"]["end_at"],
            9999.0
        );
        assert!(m.snapshot().to_string().len() < 700);
        for (epoch, rms) in [(0, -20), (1, 1), (2, -20)] {
            m.update(&json!({"type":"meters","me":{"epoch":epoch,"rms_dbfs":rms,"peak_dbfs":0,"end_at":10001,"frames":320,"sample_rate":16000}}));
        }
        assert_eq!(
            m.snapshot()["health"]["input_levels"]["me"]["end_at"],
            9999.0
        );
        m.synthetic_state(false);
        m.update(&json!({"type":"meters","me":{"epoch":1,"rms_dbfs":-20,"peak_dbfs":0,"end_at":10002,"frames":320,"sample_rate":16000}}));
        assert!(m.snapshot()["health"]["input_levels"]["me"].is_null());
        m.synthetic_state(true);
        assert!(m.snapshot()["health"]["input_levels"]["me"].is_null());
    }
}
