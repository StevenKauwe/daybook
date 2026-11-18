//! A bounded cushion for scheduling/IPC stalls, measured in audio time and bytes.
//! Callback admission never waits for the consumer. Overload marks a discontinuity
//! and counts loss; it does not turn off recording or grow memory indefinitely.
use crate::audio::{MAX_FRAMES, Packet};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, SyncSender, TrySendError},
};

pub const PACKETS: usize = 2048;
pub const PCM_BYTES: u64 = 8 * 1024 * 1024;
pub const TRACK_NANOS: u64 = 5_000_000_000;

#[derive(Default)]
struct Loss {
    frames: AtomicU64,
    nanos: AtomicU64,
    packets: AtomicU64,
    last_end: AtomicU64,
}
#[derive(Default)]
struct Budget {
    packets: AtomicU64,
    bytes: AtomicU64,
    frames: AtomicU64,
    nanos: [AtomicU64; 2],
    loss: [Loss; 2],
}
// A reservation stays charged until processing completes, including a blocked
// output write. Dropping a failed send/receiver also releases every reservation.
struct Permit {
    budget: Arc<Budget>,
    track: usize,
    frames: u64,
    bytes: u64,
    nanos: u64,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.budget.packets.fetch_sub(1, Ordering::Relaxed);
        self.budget.bytes.fetch_sub(self.bytes, Ordering::Relaxed);
        self.budget.frames.fetch_sub(self.frames, Ordering::Relaxed);
        self.budget.nanos[self.track].fetch_sub(self.nanos, Ordering::Relaxed);
    }
}
pub struct Queued {
    pub packet: Packet,
    pub continuity: u64,
    _permit: Permit,
}
impl std::ops::Deref for Queued {
    type Target = Packet;
    fn deref(&self) -> &Packet {
        &self.packet
    }
}
pub enum Message {
    Samples(Queued),
    Flush(u64),
}
pub type Receiver = mpsc::Receiver<Message>;
#[derive(Clone)]
pub struct Sender {
    tx: SyncSender<Message>,
    budget: Arc<Budget>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Accepted,
    Overloaded,
}
fn reserve(counter: &AtomicU64, amount: u64, limit: u64) -> bool {
    counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            old.checked_add(amount).filter(|&n| n <= limit)
        })
        .is_ok()
}
pub fn bounded_queue() -> (Sender, Receiver) {
    // One extra slot keeps a flush command outside the PCM packet quota.
    let (tx, rx) = mpsc::sync_channel(PACKETS + 1);
    (
        Sender {
            tx,
            budget: Arc::default(),
        },
        rx,
    )
}
impl Sender {
    pub fn admit(&self, p: Packet) -> Result<Admission, String> {
        let frames = p.pcm.len() as u64;
        if frames == 0
            || frames > MAX_FRAMES as u64
            || !(8000..=192000).contains(&p.rate)
            || !p.at.is_finite()
        {
            return Err("Audio callback exceeds frame/rate admission bound".into());
        }
        let track = usize::from(!p.me);
        let nanos = (frames * 1_000_000_000).div_ceil(p.rate as u64);
        let bytes = p.pcm.capacity() as u64 * 4;
        let b = &self.budget;
        if !reserve(&b.nanos[track], nanos, TRACK_NANOS) {
            return Ok(self.lost(&p, nanos));
        }
        if !reserve(&b.bytes, bytes, PCM_BYTES) {
            b.nanos[track].fetch_sub(nanos, Ordering::Relaxed);
            return Ok(self.lost(&p, nanos));
        }
        if !reserve(&b.packets, 1, PACKETS as u64) {
            b.nanos[track].fetch_sub(nanos, Ordering::Relaxed);
            b.bytes.fetch_sub(bytes, Ordering::Relaxed);
            return Ok(self.lost(&p, nanos));
        }
        b.frames.fetch_add(frames, Ordering::Relaxed);
        let queued = Queued {
            continuity: b.loss[track].packets.load(Ordering::Relaxed),
            packet: p,
            _permit: Permit {
                budget: b.clone(),
                track,
                frames,
                bytes,
                nanos,
            },
        };
        match self.tx.try_send(Message::Samples(queued)) {
            Ok(()) => Ok(Admission::Accepted),
            Err(TrySendError::Full(Message::Samples(p))) => Ok(self.lost(&p, nanos)),
            Err(TrySendError::Disconnected(_)) => Err("Audio consumer disconnected".into()),
            Err(_) => unreachable!(),
        }
    }
    fn lost(&self, p: &Packet, nanos: u64) -> Admission {
        let loss = &self.budget.loss[usize::from(!p.me)];
        loss.frames.fetch_add(p.pcm.len() as u64, Ordering::Relaxed);
        loss.nanos.fetch_add(nanos, Ordering::Relaxed);
        loss.last_end.store(
            (p.at + p.pcm.len() as f64 / p.rate as f64).to_bits(),
            Ordering::Relaxed,
        );
        loss.packets.fetch_add(1, Ordering::Relaxed);
        Admission::Overloaded
    }
    pub fn flush(&self, epoch: u64) -> Result<(), String> {
        self.tx
            .send(Message::Flush(epoch))
            .map_err(|_| "Audio consumer disconnected".into())
    }
    pub fn metrics(&self) -> Value {
        let b = &self.budget;
        let losses: Vec<_> = b.loss.iter().enumerate().map(|(i, l)| {
            json!({"track":if i==0 {"me"} else {"remote"},
                "frames":l.frames.load(Ordering::Relaxed),
                "audio_seconds":l.nanos.load(Ordering::Relaxed) as f64/1e9,
                "packets":l.packets.load(Ordering::Relaxed),
                "last_source_end":if l.packets.load(Ordering::Relaxed)>0 {Some(f64::from_bits(l.last_end.load(Ordering::Relaxed)))} else {None}})
        }).collect();
        let nanos: Vec<_> = b.nanos.iter().map(|n| n.load(Ordering::Relaxed)).collect();
        json!({"queue_packets":b.packets.load(Ordering::Relaxed),"queue_capacity_packets":PACKETS,
            "queue_frames":b.frames.load(Ordering::Relaxed),"queue_pcm_bytes":b.bytes.load(Ordering::Relaxed),
            "queue_capacity_pcm_bytes":PCM_BYTES,"queue_track_capacity_seconds":TRACK_NANOS as f64/1e9,
            "queue_audio_seconds":nanos.iter().sum::<u64>() as f64/1e9,
            "queue_track_seconds":{"me":nanos[0] as f64/1e9,"remote":nanos[1] as f64/1e9},
            "known_rejected_frames":losses.iter().map(|l|l["frames"].as_u64().unwrap()).sum::<u64>(),
            "losses":losses,"scope":"Concurrent admitted PCM snapshot, including in-flight consumer; excludes resampler/session state; OS/device losses unavailable"})
    }
}

/// Accelerated 24-hour queue lifecycle, not a 24-hour hardware/inference soak.
pub fn verify_day() -> Value {
    let began = std::time::Instant::now();
    let (tx, rx) = bounded_queue();
    let mut accepted = 0u64;
    let mut lost = 0u64;
    let mut peak_bytes = 0;
    for cycle in 0..17280 {
        let mut packets = 0;
        for me in [true, false] {
            let mut frames = 0;
            for n in [137, 2048, 16384, 32768, 511, 8000].into_iter().cycle() {
                let n = n.min(80000 - frames);
                assert_eq!(
                    tx.admit(Packet {
                        epoch: 1,
                        me,
                        rate: 16000,
                        pcm: vec![0.0; n],
                        at: cycle as f64 * 5.0 + frames as f64 / 16000.0
                    })
                    .unwrap(),
                    Admission::Accepted
                );
                packets += 1;
                frames += n;
                accepted += n as u64;
                if frames == 80000 {
                    break;
                }
            }
            if cycle % 7 == 0 {
                assert_eq!(
                    tx.admit(Packet {
                        epoch: 1,
                        me,
                        rate: 16000,
                        pcm: vec![0.0; 320],
                        at: cycle as f64 * 5.0 + 5.0
                    })
                    .unwrap(),
                    Admission::Overloaded
                );
                lost += 320;
            }
        }
        peak_bytes = peak_bytes.max(tx.metrics()["queue_pcm_bytes"].as_u64().unwrap());
        for _ in 0..packets {
            drop(rx.recv().unwrap());
        }
        let m = tx.metrics();
        assert_eq!(m["queue_pcm_bytes"], 0);
        assert_eq!(m["queue_frames"], 0);
        assert_eq!(m["queue_packets"], 0);
        assert_eq!(m["queue_audio_seconds"], 0.0);
    }
    assert_eq!(accepted, 2 * 86400 * 16000);
    assert_eq!(tx.metrics()["known_rejected_frames"], lost);
    json!({"type":"test","result":"PASS accelerated 24-hour queue lifecycle with repeated overload/recovery",
        "simulated_hours_per_track":24,"cycles":17280,"accepted_frames":accepted,
        "deliberately_rejected_frames":lost,"peak_pcm_bytes":peak_bytes,"final_queue":tx.metrics(),
        "elapsed_seconds":began.elapsed().as_secs_f64(),"scope":"generated PCM admission/processing lifetime; not full-day OS capture or transcription"})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(me: bool, rate: usize, frames: usize) -> Packet {
        Packet {
            epoch: 1,
            me,
            rate,
            pcm: vec![0.0; frames],
            at: 100.0,
        }
    }
    #[test]
    fn five_second_capacity_is_independent_of_callback_sizes_and_tracks() {
        for rate in [8000, 16000, 44100, 48000, 192000] {
            let (tx, rx) = bounded_queue();
            let mut expected = [0; 2];
            for (track, block) in [(0, 512), (1, 3200)] {
                while expected[track] + block <= rate * 4 {
                    assert_eq!(
                        tx.admit(packet(track == 0, rate, block)).unwrap(),
                        Admission::Accepted
                    );
                    expected[track] += block;
                }
            }
            let m = tx.metrics();
            assert_eq!(m["queue_frames"], expected.iter().sum::<usize>());
            assert_eq!(m["queue_pcm_bytes"], expected.iter().sum::<usize>() * 4);
            assert!(m["queue_pcm_bytes"].as_u64().unwrap() <= PCM_BYTES);
            assert_eq!(m["known_rejected_frames"], 0);
            drop(rx);
            assert_eq!(tx.metrics()["queue_pcm_bytes"], 0);
            assert_eq!(tx.metrics()["queue_packets"], 0);
        }
    }
    #[test]
    fn overload_preserves_backlog_and_recovers_with_track_specific_discontinuity() {
        let (tx, rx) = bounded_queue();
        for _ in 0..10 {
            assert_eq!(
                tx.admit(packet(true, 16000, 8000)).unwrap(),
                Admission::Accepted
            );
        }
        assert_eq!(
            tx.admit(packet(true, 16000, 8000)).unwrap(),
            Admission::Overloaded
        );
        // A stalled microphone must not spend the system track's time budget.
        assert_eq!(
            tx.admit(packet(false, 16000, 8000)).unwrap(),
            Admission::Accepted
        );
        let m = tx.metrics();
        assert_eq!(m["losses"][0]["audio_seconds"], 0.5);
        assert_eq!(m["losses"][0]["frames"], 8000);
        assert_eq!(m["losses"][1]["frames"], 0);
        // Dequeue alone does not release memory before processing completes.
        let in_flight = rx.recv().unwrap();
        assert_eq!(
            tx.admit(packet(true, 16000, 8000)).unwrap(),
            Admission::Overloaded
        );
        drop(in_flight);
        assert_eq!(
            tx.admit(packet(true, 16000, 8000)).unwrap(),
            Admission::Accepted
        );
        let mut received = Vec::new();
        while let Ok(Message::Samples(p)) = rx.try_recv() {
            received.push((p.me, p.continuity));
        }
        assert_eq!(received.last(), Some(&(true, 2)));
        assert!(received[..received.len() - 1].iter().all(|(_, n)| *n == 0));
        assert_eq!(tx.metrics()["queue_frames"], 0);
        assert_eq!(tx.metrics()["queue_audio_seconds"], 0.0);
    }
    #[test]
    fn bytes_packets_invalid_and_disconnected_admission_are_bounded() {
        let (tx, rx) = bounded_queue();
        let mut oversized = packet(true, 16000, 1);
        oversized.pcm.reserve(PCM_BYTES as usize / 4 + 1);
        assert_eq!(tx.admit(oversized).unwrap(), Admission::Overloaded);
        for _ in 0..PACKETS {
            assert_eq!(
                tx.admit(packet(true, 192000, 1)).unwrap(),
                Admission::Accepted
            );
        }
        assert_eq!(
            tx.admit(packet(false, 192000, 1)).unwrap(),
            Admission::Overloaded
        );
        tx.flush(1).unwrap(); // reserved control slot, even at packet capacity
        assert!(tx.admit(packet(true, 0, 1)).is_err());
        assert!(tx.admit(packet(true, 16000, 0)).is_err());
        assert!(tx.admit(packet(true, 16000, MAX_FRAMES + 1)).is_err());
        drop(rx);
        assert!(tx.admit(packet(true, 16000, 8000)).is_err());
        assert_eq!(tx.metrics()["queue_frames"], 0);
        assert_eq!(tx.metrics()["queue_pcm_bytes"], 0);
    }
    #[test]
    fn repeated_overload_recovery_does_not_leak_capacity() {
        let (tx, rx) = bounded_queue();
        for _ in 0..1000 {
            for track in [true, false] {
                for _ in 0..10 {
                    assert_eq!(
                        tx.admit(packet(track, 16000, 8000)).unwrap(),
                        Admission::Accepted
                    );
                }
                assert_eq!(
                    tx.admit(packet(track, 16000, 8000)).unwrap(),
                    Admission::Overloaded
                );
            }
            for _ in 0..20 {
                drop(rx.recv().unwrap());
            }
            let m = tx.metrics();
            assert_eq!(m["queue_pcm_bytes"], 0);
            assert_eq!(m["queue_packets"], 0);
            assert_eq!(m["queue_audio_seconds"], 0.0);
        }
        assert_eq!(tx.metrics()["known_rejected_frames"], 16_000_000);
    }
}
