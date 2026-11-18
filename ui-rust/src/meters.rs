//! Small, coalesced local meter feed and time-based display ballistics.
use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
#[derive(Default)]
struct Latest {
    value: Mutex<Option<(Arc<Value>, Instant)>>,
    visible: AtomicBool,
    urgent: AtomicBool,
}
#[derive(Default)]
pub(super) struct Feed {
    latest: Arc<Latest>,
    wake: Option<std::thread::Thread>,
}
impl Feed {
    pub fn start(base: String, ctx: egui::Context) -> Self {
        let latest = Arc::new(Latest::default());
        latest.visible.store(true, Ordering::Relaxed);
        let weak = Arc::downgrade(&latest);
        let worker = std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_millis(600))
                .redirects(0)
                .build();
            while let Some(latest) = weak.upgrade() {
                let began = Instant::now();
                let visible = latest.visible.load(Ordering::Relaxed);
                let mut fast = latest.urgent.swap(false, Ordering::Relaxed);
                if let Ok(value) = call_with(&agent, &base, "/meters", None) {
                    fast |= value["active"] == "true"
                        || value["capture_starting"] == "true"
                        || value["capture_stopping"] == "true";
                    let mut slot = latest.value.lock().unwrap();
                    let changed = slot
                        .as_ref()
                        .is_none_or(|(old, _)| display_changed(old, &value));
                    *slot = Some((Arc::new(value), Instant::now()));
                    drop(slot);
                    if changed && visible {
                        ctx.request_repaint();
                    }
                }
                drop(latest); // Let closing the window terminate the observer.
                std::thread::park_timeout(interval(visible, fast).saturating_sub(began.elapsed()));
            }
        });
        Self {
            latest,
            wake: Some(worker.thread().clone()),
        }
    }
    pub fn snapshot(&self) -> Option<(Arc<Value>, Instant)> {
        self.latest.value.lock().unwrap().clone()
    }
    pub fn visibility(&self, visible: bool) {
        if self.latest.visible.swap(visible, Ordering::Relaxed) != visible && visible {
            self.wake();
        }
    }
    pub fn wake(&self) {
        self.latest.urgent.store(true, Ordering::Relaxed);
        if let Some(thread) = &self.wake {
            thread.unpark();
        }
    }
}
fn display_changed(old: &Value, new: &Value) -> bool {
    if [
        "epoch",
        "active",
        "armed",
        "capture_starting",
        "capture_stopping",
    ]
    .iter()
    .any(|key| old[*key] != new[*key])
    {
        return true;
    }
    ["me", "remote"].iter().any(|track| {
        let before = &old["health"]["input_levels"][*track];
        let after = &new["health"]["input_levels"][*track];
        if before == after {
            return false;
        }
        // Timestamp-only changes at the display floor need no new frame. The feed
        // still refreshes freshness, and any remaining history scrolls itself out.
        let quiet = |v: &Value| v["peak_dbfs"].as_f64().is_some_and(|p| p <= -60.);
        !quiet(before)
            || !quiet(after)
            || (after["end_at"].as_f64().unwrap_or(0.) - before["end_at"].as_f64().unwrap_or(0.))
                > 0.3
    })
}
fn interval(visible: bool, active: bool) -> Duration {
    Duration::from_millis(if !visible {
        1000
    } else if active {
        16
    } else {
        500
    })
}
#[derive(Default)]
pub(super) struct Motion {
    rms: f32,
    peak: f32,
    hold_until: f64,
    last: Option<f64>,
}
impl Motion {
    /// Immediate attack / 55 ms release. Peaks hold 80 ms.
    /// All targets are measured input. No oscillator or synthetic motion.
    pub fn step(&mut self, level: Option<(f32, f32)>, now: f64) -> (f32, f32, bool) {
        let Some((rms_db, peak_db)) = level else {
            *self = Self::default();
            return (0.0, 0.0, false);
        };
        let normalize = |db: f32| ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let target = normalize(rms_db);
        let peak = normalize(peak_db);
        let dt = self.last.map_or(1.0, |last| (now - last).clamp(0.0, 0.1)) as f32;
        self.last = Some(now);
        if target >= self.rms {
            self.rms = target;
        } else {
            self.rms += (target - self.rms) * (1.0 - (-dt / 0.055).exp());
        }
        if (target - self.rms).abs() < 0.001 {
            self.rms = target;
        }
        if peak > self.peak || (peak > 0.0 && (peak - self.peak).abs() < 0.001) {
            self.peak = peak;
            self.hold_until = now + 0.080;
        } else if now >= self.hold_until {
            self.peak = (self.peak - dt * 4.0).max(peak);
        }
        (
            self.rms,
            self.peak,
            (self.rms - target).abs() > 0.001 || (self.peak - peak).abs() > 0.001,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "read-only probe of a running local recorder; set DAYBOOK_PROBE_DATA"]
    fn production_meter_client_probe() {
        let data =
            std::env::var_os("DAYBOOK_PROBE_DATA").expect("explicit data directory required");
        let base =
            std::fs::read_to_string(std::path::Path::new(&data).join("control-url.txt")).unwrap();
        let base = base.trim().to_owned();
        assert!(base.starts_with("http://127.0.0.1:"));
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(600))
            .redirects(0)
            .build();
        for pooled in [true, false] {
            let mut samples = Vec::new();
            for _ in 0..60 {
                let start = Instant::now();
                let v = if pooled {
                    call_with(&agent, &base, "/meters", None)
                } else {
                    call(&base, "/meters", None)
                }
                .unwrap();
                assert!(v["publish_interval_ms"].is_number());
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
                std::thread::sleep(Duration::from_millis(10));
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "PRODUCTION_METER_CLIENT pooled={pooled} p50_ms={:.3} p95_ms={:.3} max_ms={:.3}",
                samples[30], samples[57], samples[59]
            );
        }
        let feed = Feed::start(base, egui::Context::default());
        let mut revisions = [
            std::collections::BTreeSet::new(),
            std::collections::BTreeSet::new(),
        ];
        let mut ages = [Vec::<f64>::new(), Vec::new()];
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(4) {
            if let Some((v, _)) = feed.snapshot() {
                for (i, track) in ["me", "remote"].iter().enumerate() {
                    if let Some(at) = v["health"]["input_levels"][track]["end_at"].as_f64() {
                        revisions[i].insert((at * 1e6) as i64);
                        ages[i].push(
                            chrono::Utc::now().timestamp_micros() as f64 / 1000.0 - at * 1000.0,
                        );
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        for i in 0..2 {
            ages[i].sort_by(f64::total_cmp);
            println!(
                "PRODUCTION_FEED track={i} unique_readings={} p95_age_ms={:?}",
                revisions[i].len(),
                ages[i].get(ages[i].len() * 95 / 100)
            );
        }
    }
    #[test]
    fn drawing_coalesces_silent_ticks_but_wakes_for_signal_state_and_stale_recovery() {
        let mut old = json!({"epoch":1,"active":"true","health":{"input_levels":{"me":{"end_at":100.,"peak_dbfs":-120.,"rms_dbfs":-120.}}}});
        let mut new = old.clone();
        new["health"]["input_levels"]["me"]["end_at"] = json!(100.016);
        assert!(!display_changed(&old, &new));
        new["health"]["input_levels"]["me"]["end_at"] = json!(100.8);
        assert!(display_changed(&old, &new));
        old = new.clone();
        new["health"]["input_levels"]["me"]["peak_dbfs"] = json!(-16.);
        assert!(display_changed(&old, &new));
        old = new.clone();
        new["active"] = json!("false");
        assert!(display_changed(&old, &new));
        old = new.clone();
        new["epoch"] = json!(2);
        assert!(display_changed(&old, &new));
    }
    #[test]
    fn meter_attack_is_fast_release_smooth_and_stop_or_stale_clears_immediately() {
        let mut m = Motion::default();
        m.step(Some((-120.0, -120.0)), 0.0);
        let (r, p, _) = m.step(Some((-12.0, -6.0)), 0.016);
        assert!((r - 0.8).abs() < 0.001);
        assert!((p - 0.9).abs() < 0.001);
        let (next, held, _) = m.step(Some((-120.0, -120.0)), 0.032);
        assert!(next > 0.55 && next < r);
        assert_eq!(held, p);
        for i in 3..200 {
            m.step(Some((-120.0, -120.0)), i as f64 / 60.0);
        }
        assert_eq!(m.step(Some((-120.0, -120.0)), 4.0), (0.0, 0.0, false));
        m.step(Some((-3.0, -1.0)), 5.0);
        assert_eq!(m.step(None, 5.001), (0.0, 0.0, false));
    }
    #[test]
    fn animation_is_time_based_and_minimized_observer_backs_off() {
        let run = |hz: u32| {
            let mut m = Motion::default();
            m.step(Some((-12.0, -6.0)), 0.0);
            for i in 1..=hz {
                m.step(Some((-60.0, -60.0)), i as f64 / hz as f64);
            }
            m.rms
        };
        assert!((run(30) - run(120)).abs() < 0.001);
        assert_eq!(interval(true, true).as_millis(), 16);
        assert_eq!(interval(false, true).as_millis(), 1000);
        assert_eq!(interval(true, false).as_millis(), 500);
    }
}
