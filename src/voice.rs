//! Per-track Speechworks Silero VAD. No audio/network/device APIs here.
use crate::store::Result;
use speechworks_vad::{SampleRate, SileroVad};

pub struct Detector {
    vad: SileroVad,
    frame: [f32; 512],
    filled: usize,
    quiet: usize,
}
pub struct Activity {
    pub speech: bool,
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
        })
    }
    pub fn process(&mut self, pcm: &[f32], continuing: bool) -> Result<Activity> {
        let mut probability = 0.0f32;
        let mut speech = false;
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
                self.quiet = if voiced {
                    0
                } else {
                    self.quiet.saturating_add(512).min(16000 * 60)
                };
                self.filled = 0;
            }
        }
        Ok(Activity {
            speech,
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
            let result = detector.process(&vec![0.0; n], true).unwrap();
            assert!(!result.speech);
            assert!(result.probability < 0.1);
            assert!(detector.filled < 512);
        }
        assert!(detector.quiet >= 12000);
    }
}
