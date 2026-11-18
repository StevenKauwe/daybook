//! Fast signal measurements, independent of the durable 500 ms audio blocks.
//! Runs on the audio consumer, never in the macOS realtime callbacks.
use serde::Serialize;
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Serialize)]
pub struct Reading {
    pub epoch: u64,
    pub rms_dbfs: f64,
    pub peak_dbfs: f64,
    pub end_at: f64,
    pub frames: usize,
    pub sample_rate: usize,
    pub method: &'static str,
}
#[derive(Default)]
pub struct Window {
    rate: usize,
    epoch: u64,
    frames: usize,
    sum: f64,
    peak: f64,
}
fn db(value: f64) -> f64 {
    (20.0 * value.max(1e-6).log10()).clamp(-120.0, 0.0)
}
impl Window {
    pub fn push(&mut self, p: &crate::audio::Packet) -> Option<Reading> {
        if self.rate != p.rate || self.epoch != p.epoch {
            *self = Self {
                rate: p.rate,
                epoch: p.epoch,
                ..Self::default()
            };
        }
        let width = (p.rate / 50).max(1); // 20 ms, irrespective of callback block size.
        let mut latest: Option<Reading> = None;
        for (i, &sample) in p.pcm.iter().enumerate() {
            self.sum += (sample as f64).powi(2);
            self.peak = self.peak.max(sample.abs() as f64);
            self.frames += 1;
            if self.frames == width {
                let peak = db(self.peak).max(latest.map_or(-120.0, |r| r.peak_dbfs));
                latest = Some(Reading {
                    epoch: p.epoch,
                    rms_dbfs: db((self.sum / width as f64).sqrt()),
                    peak_dbfs: peak,
                    end_at: p.at + (i + 1) as f64 / p.rate as f64,
                    frames: width,
                    sample_rate: p.rate,
                    method: "audio_consumer",
                });
                self.frames = 0;
                self.sum = 0.0;
                self.peak = 0.0;
            }
        }
        latest
    }
}
#[derive(Default)]
struct Slots([Option<Reading>; 2]);
impl Slots {
    fn put(&mut self, track: usize, mut reading: Reading) {
        if let Some(old) = self.0[track].filter(|old| old.epoch == reading.epoch) {
            reading.peak_dbfs = reading.peak_dbfs.max(old.peak_dbfs);
        }
        self.0[track] = Some(reading);
    }
    fn take(&mut self) -> [Option<Reading>; 2] {
        std::mem::take(&mut self.0)
    }
}
struct Bus {
    slots: Mutex<Slots>,
    wake: mpsc::SyncSender<()>,
}
static BUS: OnceLock<Arc<Bus>> = OnceLock::new();
pub fn publish_realtime(reading: Reading) -> bool {
    if let Some(bus) = BUS.get() {
        if let Ok(mut slots) = bus.slots.try_lock() {
            slots.put(0, reading);
            return true;
        }
    }
    false
}
pub fn wake() {
    if let Some(bus) = BUS.get() {
        let _ = bus.wake.try_send(());
    }
}
pub fn publish(me: bool, reading: Reading) {
    if let Some(bus) = BUS.get() {
        bus.slots
            .lock()
            .unwrap()
            .put(if me { 0 } else { 1 }, reading);
        let _ = bus.wake.try_send(());
    }
}
pub fn start() -> Result<(), String> {
    let (tx, rx) = mpsc::sync_channel(1);
    let bus = Arc::new(Bus {
        slots: Mutex::new(Slots::default()),
        wake: tx,
    });
    BUS.set(bus.clone())
        .map_err(|_| "meter publisher already initialized")?;
    std::thread::Builder::new()
        .name("daybook-meters".into())
        .spawn(move || {
            let interval = Duration::from_millis(16);
            let mut sent = Instant::now() - interval;
            loop {
                if crate::EPOCH.load(std::sync::atomic::Ordering::Acquire) == 0 {
                    if rx.recv().is_err() {
                        return;
                    }
                }
                std::thread::sleep(interval.saturating_sub(sent.elapsed()));
                // At most two readings and one wake token, even with a slow pipe consumer.
                while rx.try_recv().is_ok() {}
                let [me, remote] = bus.slots.lock().unwrap().take();
                if me.is_some() || remote.is_some() {
                    crate::emit(serde_json::json!({"type":"meters","me":me,"remote":remote}));
                }
                sent = Instant::now();
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}
/// Generated pulses on one track and exact silence on the other. No capture APIs.
pub fn self_test(seconds: u64) -> Result<(), String> {
    start()?;
    crate::EPOCH.store(1, std::sync::atomic::Ordering::Release);
    crate::mic_meter::ACTIVE.store(true, std::sync::atomic::Ordering::Release);
    wake();
    let mut me = crate::audio::Track::new(Default::default(), "me");
    let mut remote = crate::audio::Track::new(Default::default(), "remote");
    let began = Instant::now();
    for i in 0..seconds * 100 {
        std::thread::sleep(Duration::from_millis((i + 1) * 10).saturating_sub(began.elapsed()));
        let at = crate::stamp() - 0.010;
        for track in [true, false] {
            let amplitude = if track && i % 50 < 25 { 0.1 } else { 0.0 };
            let p = crate::audio::Packet {
                epoch: 1,
                me: track,
                rate: 16000,
                pcm: (0..160)
                    .map(|j| (j as f32 * std::f32::consts::TAU / 16.0).sin() * amplitude)
                    .collect(),
                at,
            };
            if track {
                let list = crate::ffi::AudioList {
                    count: 1,
                    buffers: [crate::ffi::AudioBuffer {
                        channels: 1,
                        bytes: (p.pcm.len() * 4) as u32,
                        data: p.pcm.as_ptr().cast_mut().cast(),
                    }],
                };
                let format = crate::ffi::Asbd {
                    rate: 16000.0,
                    format: u32::from_be_bytes(*b"lpcm"),
                    flags: 1,
                    channels: 1,
                    bits: 32,
                    ..Default::default()
                };
                let reading = unsafe {
                    crate::mic_meter::observe(&list, &format, p.pcm.len(), 1, at + 0.010)
                }
                .ok_or("synthetic microphone meter failed")?;
                if !publish_realtime(reading) {
                    crate::mic_meter::SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                me.push(&p)?;
            } else {
                remote.push(&p)?;
            }
        }
    }
    me.finish()?;
    remote.finish()?;
    std::thread::sleep(Duration::from_millis(40));
    crate::EPOCH.store(0, std::sync::atomic::Ordering::Release);
    crate::mic_meter::ACTIVE.store(false, std::sync::atomic::Ordering::Release);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn variable_callbacks_measure_twenty_ms_without_waiting_for_audio_flush() {
        for rate in [8000, 16000, 44100, 48000, 192000] {
            let mut w = Window::default();
            let mut offset = 0;
            for n in [13, 137, 1024, 7, 32768] {
                let reading = w.push(&crate::audio::Packet {
                    epoch: 1,
                    me: true,
                    rate,
                    pcm: vec![0.1; n],
                    at: 100.0 + offset as f64 / rate as f64,
                });
                offset += n;
                if let Some(r) = reading {
                    assert_eq!(r.frames, rate / 50);
                    assert!((r.rms_dbfs + 20.0).abs() < 0.001);
                    assert!(
                        (r.end_at
                            - (100.0 + (offset / (rate / 50) * (rate / 50)) as f64 / rate as f64))
                            .abs()
                            < 1e-8
                    );
                }
                assert!(w.frames < rate / 50);
            }
            let silent = w
                .push(&crate::audio::Packet {
                    epoch: 2,
                    me: true,
                    rate,
                    pcm: vec![0.0; rate / 50],
                    at: 200.0,
                })
                .unwrap();
            assert_eq!(silent.rms_dbfs, -120.0); // New sessions never inherit old signal.
        }
    }
    #[test]
    fn slow_observer_keeps_two_latest_values_and_transient_peaks() {
        let mut slots = Slots::default();
        for i in 0..10000 {
            for track in 0..2 {
                slots.put(
                    track,
                    Reading {
                        epoch: 1,
                        rms_dbfs: -30.0,
                        peak_dbfs: if i == 0 { -3.0 } else { -24.0 },
                        end_at: i as f64,
                        frames: 320,
                        sample_rate: 16000,
                        method: "audio_consumer",
                    },
                );
            }
        }
        for r in slots.take() {
            let r = r.unwrap();
            assert_eq!(r.end_at, 9999.0);
            assert_eq!(r.peak_dbfs, -3.0);
        }
        assert!(slots.take().iter().all(Option::is_none));
    }
}
