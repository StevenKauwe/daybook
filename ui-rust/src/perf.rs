//! Opt-in profiling of readings actually used to draw the native input monitors.
use super::*;

// Tenths of a millisecond, with a final overflow bucket. Memory stays constant.
struct Histogram {
    bins: Box<[u32]>,
    total: u32,
}
impl Default for Histogram {
    fn default() -> Self {
        Self {
            bins: vec![0; 20_001].into(),
            total: 0,
        }
    }
}
impl Histogram {
    fn record(&mut self, ms: f64) {
        let bin = ((ms.max(0.) * 10.).round() as usize).min(self.bins.len() - 1);
        self.bins[bin] += 1;
        self.total += 1;
    }
    fn p95(&self) -> Option<f64> {
        if self.total == 0 {
            return None;
        }
        let target = (u64::from(self.total) * 95).div_ceil(100);
        let mut count = 0u64;
        for (i, n) in self.bins.iter().enumerate() {
            count += u64::from(*n);
            if count >= target {
                return Some(i as f64 / 10.);
            }
        }
        None
    }
}

pub struct Probe {
    path: std::path::PathBuf,
    began: Instant,
    last_frame: Option<(u64, Instant)>,
    frames: u32,
    gaps: Histogram,
    ages: [Histogram; 2],
    last_end: [Option<f64>; 2],
    updates: [u32; 2],
}
impl Probe {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self {
            path,
            began: Instant::now(),
            last_frame: None,
            frames: 0,
            gaps: Default::default(),
            ages: Default::default(),
            last_end: [None; 2],
            updates: [0; 2],
        }
    }
    pub fn drawn(&mut self, frame: u64, reading: &Value) {
        let now = Instant::now();
        if let Some((previous, at)) = self.last_frame {
            if previous == frame {
                return;
            }
            if self.frames > 10 {
                self.gaps.record((now - at).as_secs_f64() * 1000.);
            }
        }
        self.frames += 1;
        self.last_frame = Some((frame, now));
        let unix_ms = chrono::Utc::now().timestamp_micros() as f64 / 1000.;
        for (i, track) in ["me", "remote"].into_iter().enumerate() {
            if let Some(end) = reading["health"]["input_levels"][track]["end_at"].as_f64() {
                if self.last_end[i] != Some(end) {
                    self.last_end[i] = Some(end);
                    self.updates[i] += 1;
                }
                self.ages[i].record(unix_ms - end * 1000.);
            }
        }
    }
    pub fn finish_if_due(&self) -> bool {
        if self.began.elapsed() < Duration::from_secs(5) {
            return false;
        }
        let result = json!({"seconds":self.began.elapsed().as_secs_f64(),"frames":self.frames,"frame_gap_p95_ms":self.gaps.p95(),"meter_age_p95_ms":[self.ages[0].p95(),self.ages[1].p95()],"distinct_readings":self.updates,"scope":"Actual native monitor draw calls with production Controls and localhost feed, bounded timing histograms, 0.1 ms resolution. Excludes display scan-out and physical sound."});
        if let Err(error) = std::fs::write(&self.path, result.to_string()) {
            eprintln!("Writing UI timing results: {error}");
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_probe_observes_late_changes_and_counts_each_draw_once() {
        let mut probe = Probe::new("/unused".into());
        let mut reading = json!({"health":{"input_levels":{"me":{"end_at":12.}}}});
        for frame in 0..1800 {
            probe.drawn(frame, &reading);
            probe.drawn(frame, &reading);
        }
        reading["health"]["input_levels"]["me"]["end_at"] = json!(13.);
        probe.drawn(1800, &reading);
        assert_eq!(probe.frames, 1801);
        assert_eq!(probe.updates, [2, 0]);
        assert_eq!(probe.ages[1].p95(), None);
    }
}
