//! Per-track Speechworks Silero VAD. No audio/network/device APIs here.
use crate::store::Result;
use speechworks_vad::{SampleRate, SileroVad};

pub struct Detector {
    vad: SileroVad,
    frame: [f32; 512],
    filled: usize,
    quiet: usize,
    playback_windows: u8,
}
pub struct Activity {
    pub speech: bool,
    pub playback: bool,
    pub probability: f32,
    pub quiet_samples: usize,
}
impl Detector {
    pub fn new() -> Result<Self> {
        Ok(Self {
            vad: SileroVad::new(SampleRate::Hz16000)?,
            frame: [0.0; 512],
            filled: 0,
            quiet: 0,
            playback_windows: 0,
        })
    }
    pub fn process(
        &mut self,
        pcm: &[f32],
        continuing: bool,
        system_audio: bool,
    ) -> Result<Activity> {
        let mut probability = 0.0f32;
        let mut speech = false;
        let mut playback = false;
        for &sample in pcm {
            self.frame[self.filled] = sample;
            self.filled += 1;
            if self.filled == 512 {
                let p = self.vad.process(&self.frame)?;
                probability = probability.max(p);
                // Digital silence cannot be speech, even if recurrent VAD state has a tail.
                let signal = self.frame.iter().any(|v| v.abs() > 0.000001);
                let voiced = signal && p >= if continuing || speech { 0.1 } else { 0.3 };
                speech |= voiced;
                // A digital playback track can contain intelligible speech under music or
                // effects that Silero rejects. Admit 64 ms above -60 dBFS excluding DC;
                // a single click or a constant offset cannot keep the recognizer running.
                let audible = system_audio && {
                    let mean = self.frame.iter().sum::<f32>() / 512.0;
                    self.frame.iter().map(|x| (x - mean).powi(2)).sum::<f32>() >= 512.0e-6
                };
                self.playback_windows = if audible {
                    self.playback_windows.saturating_add(1)
                } else {
                    0
                };
                let sustained = self.playback_windows >= 2;
                playback |= sustained;
                self.quiet = if voiced || sustained {
                    0
                } else {
                    self.quiet.saturating_add(512).min(16000 * 60)
                };
                self.filled = 0;
            }
        }
        Ok(Activity {
            speech,
            playback,
            probability,
            quiet_samples: self.quiet,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_vad_rejects_silence_with_variable_input_and_bounded_quiet_state() {
        let mut detector = Detector::new().unwrap();
        for n in [137, 8000, 1, 319, 8000, 16000, 8000] {
            let result = detector.process(&vec![0.0; n], true, true).unwrap();
            assert!(!result.speech);
            assert!(!result.playback);
            assert!(result.probability < 0.1);
            assert!(detector.filled < 512);
        }
        assert!(detector.quiet >= 12000);
    }
    #[test]
    fn quiet_digital_playback_crosses_packet_boundaries_and_the_microphone_stays_speech_gated() {
        let wave: Vec<_> = (0..2048).map(|n| (n as f32 * 0.17).sin() * 0.003).collect();
        for system in [true, false] {
            let mut detector = Detector::new().unwrap();
            let mut admitted = false;
            for chunk in wave.chunks(137) {
                admitted |= detector.process(chunk, false, system).unwrap().playback;
            }
            assert_eq!(admitted, system);
            let quiet = detector.process(&[0.0; 12800], true, system).unwrap();
            assert!(!quiet.playback && quiet.quiet_samples >= 12000);
        }
        let mut detector = Detector::new().unwrap();
        let mut click = vec![0.2; 2048];
        click[100] = 0.9;
        assert!(!detector.process(&click, false, true).unwrap().playback);
    }
}
