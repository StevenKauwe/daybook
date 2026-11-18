//! Bounded PCM copy/downmix and the pinned Speechworks Rust windowed-sinc resampler.
#![allow(unsafe_op_in_unsafe_fn)]
use crate::ffi::*;
use base64::Engine;
use serde_json::json;
use speechworks::audio::Resampler;
use std::{
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static SERIAL: AtomicU64 = AtomicU64::new(0);
pub const MAX_FRAMES: usize = 32768;
pub struct Packet {
    pub epoch: u64,
    pub me: bool,
    pub rate: usize,
    pub pcm: Vec<f32>,
    pub at: f64,
}
// Measured off the realtime callback, on each <= 0.5 s mono block.
// -120 dBFS is the display floor, not a claim of audible sound or speech.
fn levels(pcm: &[f32]) -> serde_json::Value {
    let sum = pcm.iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
    let peak = pcm.iter().fold(0.0f64, |p, x| p.max(x.abs() as f64));
    let db = |x: f64| (20.0 * x.max(1e-6).log10()).clamp(-120.0, 0.0);
    json!({"rms_dbfs":db((sum / pcm.len().max(1) as f64).sqrt()),"peak_dbfs":db(peak)})
}
pub unsafe fn decode(list: *const AudioList, d: &Asbd, frames: usize) -> Result<Vec<f32>, String> {
    if list.is_null()
        || frames == 0
        || frames > MAX_FRAMES
        || d.channels == 0
        || d.channels > 32
        || !(8000.0..=192000.0).contains(&d.rate)
        || d.format != u32::from_be_bytes(*b"lpcm")
    {
        return Err("Unsupported or oversized PCM format".into());
    }
    let channels = d.channels as usize;
    let count = (*list).count as usize;
    if count == 0 || count > 32 {
        return Err("Invalid audio buffer count".into());
    }
    let buffers = std::slice::from_raw_parts(
        std::ptr::addr_of!((*list).buffers).cast::<AudioBuffer>(),
        count,
    );
    let width = match d.bits {
        16 => 2,
        32 => 4,
        64 => 8,
        _ => return Err("Unsupported PCM sample width".into()),
    };
    let float = d.flags & 1 != 0;
    let big = d.flags & 2 != 0;
    if !float && d.flags & 4 == 0 {
        return Err("Unsigned PCM is unsupported".into());
    }
    if float && width == 2 {
        return Err("Unsupported float width".into());
    }
    if buffers.iter().map(|b| b.channels as usize).sum::<usize>() != channels {
        return Err("Audio channel layout mismatch".into());
    }
    let mut mono = vec![0.0; frames];
    for b in buffers {
        let stride = b.channels as usize * width;
        if b.data.is_null() || (b.bytes as usize) < frames * stride {
            return Err("Truncated PCM buffer".into());
        }
        let bytes = std::slice::from_raw_parts(b.data.cast::<u8>(), b.bytes as usize);
        for (i, out) in mono.iter_mut().enumerate() {
            for c in 0..b.channels as usize {
                let p = &bytes[i * stride + c * width..][..width];
                let value = match (width, float, big) {
                    (4, true, false) => f32::from_le_bytes(p.try_into().unwrap()),
                    (4, true, true) => f32::from_be_bytes(p.try_into().unwrap()),
                    (8, true, false) => f64::from_le_bytes(p.try_into().unwrap()) as f32,
                    (8, true, true) => f64::from_be_bytes(p.try_into().unwrap()) as f32,
                    (2, false, false) => i16::from_le_bytes(p.try_into().unwrap()) as f32 / 32768.0,
                    (2, false, true) => i16::from_be_bytes(p.try_into().unwrap()) as f32 / 32768.0,
                    (4, false, false) => {
                        i32::from_le_bytes(p.try_into().unwrap()) as f32 / 2147483648.0
                    }
                    (4, false, true) => {
                        i32::from_be_bytes(p.try_into().unwrap()) as f32 / 2147483648.0
                    }
                    _ => return Err("Unsupported integer PCM width".into()),
                };
                if !value.is_finite() {
                    return Err("Non-finite audio sample".into());
                }
                *out += value / channels as f32;
            }
        }
    }
    for x in &mut mono {
        *x = x.clamp(-1.0, 1.0)
    }
    Ok(mono)
}
pub struct Track {
    root: PathBuf,
    name: &'static str,
    samples: Vec<f32>,
    resampler: Option<Resampler>,
    rate: usize,
    began: f64,
    epoch: u64,
    disk: bool,
    meter: crate::meters::Window,
    continuity: u64,
}
impl Track {
    pub fn new(root: PathBuf, name: &'static str) -> Self {
        Self {
            root,
            name,
            samples: Vec::with_capacity(8000),
            resampler: None,
            rate: 0,
            began: 0.0,
            epoch: 0,
            disk: false,
            meter: Default::default(),
            continuity: 0,
        }
    }
    pub fn push_queued(&mut self, p: &crate::capture_queue::Queued) -> Result<(), String> {
        if self.continuity != p.continuity {
            // Do not stretch timestamps or join words across lost PCM. The backend
            // already finalizes its per-track recognizer when a session ID changes.
            self.finish()?;
            self.meter = Default::default();
            self.continuity = p.continuity;
        }
        self.push(p)
    }
    fn session(&self) -> String {
        format!("{}-{}-{}", std::process::id(), self.epoch, self.continuity)
    }
    pub fn push(&mut self, p: &Packet) -> Result<(), String> {
        if !p.me || !crate::mic_meter::ACTIVE.load(Ordering::Acquire) {
            if let Some(reading) = self.meter.push(p) {
                crate::meters::publish(p.me, reading);
            }
        }
        if self.epoch != p.epoch {
            self.finish()?;
            self.epoch = p.epoch;
        }
        if self.rate != p.rate {
            self.finish()?;
            self.resampler = Some(Resampler::new(p.rate).map_err(str::to_owned)?);
            self.rate = p.rate;
        }
        if self.samples.is_empty() {
            self.began = p.at
        }
        let output = self
            .resampler
            .as_mut()
            .unwrap()
            .push(&p.pcm)
            .map_err(str::to_owned)?;
        self.append(&output)
    }
    fn append(&mut self, pcm: &[f32]) -> Result<(), String> {
        for &x in pcm {
            self.samples.push(x);
            if self.samples.len() == 8000 {
                self.flush()?
            }
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Result<(), String> {
        if let Some(mut r) = self.resampler.take() {
            self.append(&r.finish())?;
        }
        self.rate = 0;
        self.flush()
    }
    fn flush(&mut self) -> Result<(), String> {
        if self.samples.is_empty() {
            return Ok(());
        }
        let end = self.began + self.samples.len() as f64 / 16000.0;
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let level = levels(&self.samples);
        if !self.disk && !crate::PERSIST_AUDIO.load(Ordering::Acquire) {
            let bytes: Vec<u8> = self.samples.iter().flat_map(|x| x.to_le_bytes()).collect();
            crate::emit(
                json!({"type":"audio","track":self.name,"at":self.began,"end":end,"path":format!("memory:{}-{}-{serial}",std::process::id(),self.epoch),"session":self.session(),"frames":self.samples.len(),"sample_rate":16000,"timing_estimated":false,"level":level,"pcm_base64":base64::engine::general_purpose::STANDARD.encode(bytes)}),
            );
            self.samples.clear();
            self.began = end;
            return Ok(());
        }
        let path = format!(
            "audio/{}-{}-{}-{serial}.f32",
            self.name,
            std::process::id(),
            (crate::stamp() * 1e6) as u64
        );
        let target = self.root.join(&path);
        let tmp = target.with_extension("tmp");
        let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(self.samples.len() * 4);
        for x in &self.samples {
            bytes.extend(x.to_le_bytes())
        }
        file.write_all(&bytes)
            .and_then(|_| file.sync_data())
            .map_err(|e| e.to_string())?;
        let metadata = json!({"type":"audio","track":self.name,"at":self.began,"end":end,"path":path,"session":self.session(),"frames":self.samples.len(),"sample_rate":16000,"timing_estimated":false,"level":level});
        let sidecar = target.with_extension("json");
        let meta_tmp = target.with_extension("meta.tmp");
        let mut meta_file = std::fs::File::create(&meta_tmp).map_err(|e| e.to_string())?;
        meta_file
            .write_all(metadata.to_string().as_bytes())
            .and_then(|_| meta_file.sync_data())
            .map_err(|e| e.to_string())?;
        std::fs::rename(meta_tmp, sidecar).map_err(|e| e.to_string())?;
        std::fs::rename(tmp, target).map_err(|e| e.to_string())?;
        crate::emit(metadata);
        self.samples.clear();
        self.began = end;
        Ok(())
    }
}
pub fn self_test(root: PathBuf) -> Result<(), String> {
    std::fs::create_dir_all(root.join("audio")).map_err(|e| e.to_string())?;
    let mut a = Track::new(root.clone(), "me");
    let mut b = Track::new(root, "remote");
    a.disk = true;
    b.disk = true;
    let pcm: Vec<f32> = (0..48000)
        .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 48000.0).sin() * 0.1)
        .collect();
    for chunk in pcm.chunks(137) {
        let p = Packet {
            epoch: 1,
            me: true,
            rate: 48000,
            pcm: chunk.to_vec(),
            at: crate::stamp() - 1.0,
        };
        a.push(&p)?;
        b.push(&p)?
    }
    a.finish()?;
    b.finish()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn meters_measure_silence_peak_and_rms_for_variable_blocks() {
        assert_eq!(levels(&[0.0; 8000])["rms_dbfs"], -120.0);
        assert_eq!(levels(&[0.0; 13])["peak_dbfs"], -120.0);
        for n in [137, 1024, 8000] {
            let rms = levels(&vec![0.1; n])["rms_dbfs"].as_f64().unwrap();
            assert!((rms + 20.0).abs() < 0.001);
        }
        let sine: Vec<f32> = (0..8000)
            .map(|i| (i as f32 * std::f32::consts::TAU / 16.0).sin() * 0.5)
            .collect();
        let measured = levels(&sine);
        assert!((measured["rms_dbfs"].as_f64().unwrap() + 9.0309).abs() < 0.001);
        assert!((measured["peak_dbfs"].as_f64().unwrap() + 6.0206).abs() < 0.001);
    }
    #[test]
    fn memory_capture_does_not_create_audio_or_metadata_files() {
        let root =
            std::env::temp_dir().join(format!("daybook-native-memory-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("audio")).unwrap();
        let mut track = Track::new(root.clone(), "me");
        for i in 0..100 {
            track
                .push(&Packet {
                    epoch: 1,
                    me: true,
                    rate: 16000,
                    pcm: vec![0.01; 1024],
                    at: 1000.0 + i as f64 * 1024.0 / 16000.0,
                })
                .unwrap();
        }
        track.finish().unwrap();
        assert_eq!(std::fs::read_dir(root.join("audio")).unwrap().count(), 0);
    }
    #[test]
    fn stereo_pcm_downmix() {
        let mut data = [32767i16, -32767, 16384, 16384];
        let list = AudioList {
            count: 1,
            buffers: [AudioBuffer {
                channels: 2,
                bytes: 8,
                data: data.as_mut_ptr().cast(),
            }],
        };
        let d = Asbd {
            rate: 48000.0,
            format: u32::from_be_bytes(*b"lpcm"),
            flags: 4,
            channels: 2,
            bits: 16,
            ..Default::default()
        };
        let out = unsafe { decode(&list, &d, 2) }.unwrap();
        assert!(out[0].abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
        assert!(unsafe { decode(&list, &d, 3) }.is_err());
    }
    #[test]
    fn rejects_oversized_and_nonfinite() {
        let mut data = [f32::NAN];
        let list = AudioList {
            count: 1,
            buffers: [AudioBuffer {
                channels: 1,
                bytes: 4,
                data: data.as_mut_ptr().cast(),
            }],
        };
        let d = Asbd {
            rate: 16000.0,
            format: u32::from_be_bytes(*b"lpcm"),
            flags: 1,
            channels: 1,
            bits: 32,
            ..Default::default()
        };
        assert!(unsafe { decode(&list, &d, 1) }.is_err());
        assert!(unsafe { decode(&list, &d, MAX_FRAMES + 1) }.is_err());
    }
    #[test]
    fn sinc_preserves_chunk_boundaries() {
        let data: Vec<f32> = (0..48000).map(|i| (i as f32 * 0.01).sin()).collect();
        let mut a = Resampler::new(48000).unwrap();
        let mut b = Resampler::new(48000).unwrap();
        let mut one = a.push(&data).unwrap();
        one.extend(a.finish());
        let mut many = Vec::new();
        for x in data.chunks(137) {
            many.extend(b.push(x).unwrap())
        }
        many.extend(b.finish());
        assert_eq!(one, many);
        assert_eq!(one.len(), 16000);
    }
}

pub fn verify_bounds() -> serde_json::Value {
    use crate::capture_queue::{Admission, Message, bounded_queue};
    use std::time::{Duration, Instant};
    let (tx, rx) = bounded_queue();
    let consumer = std::thread::spawn(move || {
        // Model a brief scheduler/IPC stall, followed by a slower consumer.
        std::thread::sleep(Duration::from_millis(350));
        let mut frames = [0usize; 2];
        let mut packets = 0;
        while let Ok(Message::Samples(p)) = rx.recv() {
            frames[usize::from(!p.me)] += p.pcm.len();
            packets += 1;
            std::thread::sleep(Duration::from_micros(500));
        }
        (frames, packets)
    });
    let started = Instant::now();
    let mut frames = [0usize; 2];
    let mut count = [0usize; 2];
    let rates = [44100usize, 16000];
    let mut peak_bytes = 0;
    let mut peak_packets = 0;
    while frames[0] < rates[0] * 2 || frames[1] < rates[1] * 2 {
        for track in 0..2 {
            let rate = rates[track];
            let n = (rate / 100 * (count[track] % 4 + 1)).min(rate * 2 - frames[track]);
            if n > 0 && started.elapsed().as_secs_f64() >= (frames[track] + n) as f64 / rate as f64
            {
                assert_eq!(
                    tx.admit(Packet {
                        epoch: 1,
                        me: track == 0,
                        rate,
                        pcm: vec![0.0; n],
                        at: 100.0 + frames[track] as f64 / rate as f64
                    })
                    .unwrap(),
                    Admission::Accepted
                );
                frames[track] += n;
                count[track] += 1;
                let m = tx.metrics();
                peak_bytes = peak_bytes.max(m["queue_pcm_bytes"].as_u64().unwrap());
                peak_packets = peak_packets.max(m["queue_packets"].as_u64().unwrap());
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    tx.flush(1).unwrap();
    let (received, packets) = consumer.join().unwrap();
    assert_eq!(received, frames);
    assert_eq!(frames, [88200, 32000]);
    assert_eq!(tx.metrics()["known_rejected_frames"], 0);
    assert_eq!(tx.metrics()["queue_pcm_bytes"], 0);
    assert!(
        peak_packets > 8,
        "regression must exceed the old eight-packet cap"
    );
    json!({"type":"test","result":"PASS two real-time synthetic streams survive a 350 ms consumer stall without loss",
        "frames_me":received[0],"frames_remote":received[1],"audio_seconds_per_track":2,
        "elapsed_ms":started.elapsed().as_millis(),"packets":packets,"peak_pcm_bytes":peak_bytes,
        "peak_packets":peak_packets,"rejected_frames":0,"bounds":tx.metrics()})
}
#[cfg(test)]
mod queue_tests {
    #[test]
    fn recovery_splits_sources_without_filling_or_hiding_the_missing_interval() {
        use super::*;
        use crate::capture_queue::{Admission, Message, bounded_queue};
        let root = std::env::temp_dir().join(format!("daybook-gap-source-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("audio")).unwrap();
        let (tx, rx) = bounded_queue();
        let packet = |at| Packet {
            epoch: 1,
            me: true,
            rate: 16000,
            pcm: vec![0.01; 8000],
            at,
        };
        for i in 0..10 {
            assert_eq!(
                tx.admit(packet(100.0 + i as f64 / 2.0)).unwrap(),
                Admission::Accepted
            );
        }
        assert_eq!(tx.admit(packet(105.0)).unwrap(), Admission::Overloaded);
        let mut track = Track::new(root.clone(), "me");
        track.disk = true;
        for _ in 0..10 {
            let Message::Samples(p) = rx.recv().unwrap() else {
                panic!()
            };
            track.push_queued(&p).unwrap();
        }
        assert_eq!(tx.admit(packet(105.5)).unwrap(), Admission::Accepted);
        let Message::Samples(p) = rx.recv().unwrap() else {
            panic!()
        };
        track.push_queued(&p).unwrap();
        drop(p);
        track.finish().unwrap();
        let mut sources: Vec<serde_json::Value> = std::fs::read_dir(root.join("audio"))
            .unwrap()
            .flatten()
            .filter(|p| p.path().extension().is_some_and(|e| e == "json"))
            .map(|p| serde_json::from_slice(&std::fs::read(p.path()).unwrap()).unwrap())
            .collect();
        sources.sort_by(|a, b| {
            a["at"]
                .as_f64()
                .unwrap()
                .total_cmp(&b["at"].as_f64().unwrap())
        });
        assert_eq!(sources.len(), 11);
        assert_eq!(sources[9]["end"], 105.0);
        assert_eq!(sources[10]["at"], 105.5);
        assert_eq!(sources[10]["end"], 106.0);
        assert_ne!(sources[9]["session"], sources[10]["session"]);
        assert_eq!(
            sources
                .iter()
                .map(|v| v["frames"].as_u64().unwrap())
                .sum::<u64>(),
            88000
        );
        assert_eq!(tx.metrics()["queue_pcm_bytes"], 0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn variable_callbacks_both_streams_slow_consumer() {
        super::verify_bounds();
    }
}
