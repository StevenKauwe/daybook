//! Meter-only AVAudioSinkNode, connected to the existing microphone input.
//! The recording tap still owns audio delivery; this callback retains no PCM.
use crate::ffi::*;
use crate::meters::Reading;
use block2::RcBlock;
use objc2::{msg_send, runtime::AnyObject};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub static ACTIVE: AtomicBool = AtomicBool::new(false);
pub static SKIPPED: AtomicU64 = AtomicU64::new(0);
pub static CALLBACKS: AtomicU64 = AtomicU64::new(0);
pub static LAST_FRAMES: AtomicU64 = AtomicU64::new(0);
pub static LAST_RATE: AtomicU64 = AtomicU64::new(0);

/// No allocation, Objective-C calls, waiting, audio copies, or retained input pointers.
/// Buffers are borrowed only for the duration of the engine's I/O callback.
unsafe fn measure(list: *const AudioList, d: &Asbd, frames: usize) -> Option<(f64, f64)> {
    if list.is_null()
        || frames == 0
        || frames > crate::audio::MAX_FRAMES
        || d.channels == 0
        || d.channels > 32
        || !(8000.0..=192000.0).contains(&d.rate)
        || d.format != u32::from_be_bytes(*b"lpcm")
    {
        return None;
    }
    // AVAudioEngine's hardware-rate input scope is float32 PCM. An unexpected format
    // falls back to the ordinary tap meter instead of changing the recording format.
    if d.bits != 32 || d.flags & 1 == 0 || d.flags & 2 != 0 {
        return None;
    }
    let count = (*list).count as usize;
    if count == 0 || count > 32 {
        return None;
    }
    let buffers = std::slice::from_raw_parts(
        std::ptr::addr_of!((*list).buffers).cast::<AudioBuffer>(),
        count,
    );
    if buffers.iter().map(|b| b.channels as usize).sum::<usize>() != d.channels as usize {
        return None;
    }
    if buffers
        .iter()
        .any(|b| b.data.is_null() || (b.bytes as usize) < frames * b.channels as usize * 4)
    {
        return None;
    }
    let mut sum = 0.0;
    let mut peak = 0.0f64;
    for i in 0..frames {
        let mut mono = 0.0f64;
        for b in buffers {
            for c in 0..b.channels as usize {
                let x =
                    std::ptr::read_unaligned(b.data.cast::<f32>().add(i * b.channels as usize + c));
                if !x.is_finite() {
                    return None;
                }
                mono += x as f64 / d.channels as f64;
            }
        }
        mono = mono.clamp(-1.0, 1.0);
        sum += mono * mono;
        peak = peak.max(mono.abs());
    }
    let db = |v: f64| (20.0 * v.max(1e-6).log10()).clamp(-120.0, 0.0);
    Some((db((sum / frames as f64).sqrt()), db(peak)))
}
pub unsafe fn observe(
    list: *const AudioList,
    d: &Asbd,
    frames: usize,
    epoch: u64,
    at: f64,
) -> Option<Reading> {
    let (rms, peak) = measure(list, d, frames)?;
    Some(Reading {
        epoch,
        rms_dbfs: rms,
        peak_dbfs: peak,
        end_at: at,
        frames,
        sample_rate: d.rate as usize,
        method: "microphone_io",
    })
}
pub unsafe fn node(d: Asbd, epoch: u64) -> Obj {
    let block = RcBlock::new(
        move |_timestamp: *const std::ffi::c_void, frames: u32, list: *const AudioList| -> i32 {
            if crate::EPOCH.load(Ordering::Acquire) != epoch {
                return 0;
            }
            CALLBACKS.fetch_add(1, Ordering::Relaxed);
            LAST_FRAMES.store(frames as u64, Ordering::Relaxed);
            LAST_RATE.store(d.rate as u64, Ordering::Relaxed);
            match observe(list, &d, frames as usize, epoch, crate::stamp()) {
                Some(reading) => {
                    ACTIVE.store(true, Ordering::Release);
                    // try_lock only: an occupied telemetry slot never holds up audio I/O.
                    if !crate::meters::publish_realtime(reading) {
                        SKIPPED.fetch_add(1, Ordering::Relaxed);
                    }
                }
                None => {
                    ACTIVE.store(false, Ordering::Release);
                    SKIPPED.fetch_add(1, Ordering::Relaxed);
                }
            }
            0
        },
    );
    let allocated: objc2::rc::Allocated<AnyObject> = msg_send![class(c"AVAudioSinkNode"), alloc];
    msg_send![allocated,initWithReceiverBlock:&*block]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn format(channels: u32) -> Asbd {
        Asbd {
            rate: 48000.0,
            format: u32::from_be_bytes(*b"lpcm"),
            flags: 1,
            bits: 32,
            channels,
            ..Default::default()
        }
    }
    #[test]
    fn hardware_windows_measure_pulses_without_waiting_for_tap_blocks() {
        for n in [64, 128, 256, 512, 1024] {
            let mut pcm = vec![0.1f32; n];
            let list = AudioList {
                count: 1,
                buffers: [AudioBuffer {
                    channels: 1,
                    bytes: (n * 4) as u32,
                    data: pcm.as_mut_ptr().cast(),
                }],
            };
            let r = unsafe { observe(&list, &format(1), n, 1, 100.0) }.unwrap();
            assert_eq!(r.frames, n);
            assert!((r.rms_dbfs + 20.0).abs() < 0.001);
            assert_eq!(r.method, "microphone_io");
            pcm.fill(0.0);
            let r = unsafe { observe(&list, &format(1), n, 1, 100.01) }.unwrap();
            assert_eq!(r.rms_dbfs, -120.0);
        }
    }
    #[test]
    fn stereo_and_invalid_buffers_do_not_invent_signal() {
        let mut pcm = vec![0.1f32; 512];
        for i in (1..512).step_by(2) {
            pcm[i] = -0.1;
        }
        let mut list = AudioList {
            count: 1,
            buffers: [AudioBuffer {
                channels: 2,
                bytes: 2048,
                data: pcm.as_mut_ptr().cast(),
            }],
        };
        assert_eq!(
            unsafe { observe(&list, &format(2), 256, 1, 0.0) }
                .unwrap()
                .peak_dbfs,
            -120.0
        );
        list.buffers[0].bytes = 4;
        assert!(unsafe { observe(&list, &format(2), 256, 1, 0.0) }.is_none());
        list.buffers[0].bytes = 2048;
        pcm[0] = f32::NAN;
        assert!(unsafe { observe(&list, &format(2), 256, 1, 0.0) }.is_none());
    }
    #[test]
    #[ignore = "requires normal macOS framework context; no device or engine is started"]
    fn sink_constructs_without_opening_a_device_or_starting_an_engine() {
        objc2::rc::autoreleasepool(|_| unsafe {
            let sink = node(format(1), u64::MAX);
            let count: usize = msg_send![&*sink, numberOfOutputs];
            assert_eq!(count, 0);
        });
    }
}
