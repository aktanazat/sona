use anyhow::Result;
use std::path::Path;

use vad_rs::Vad;

use super::{VadFrame, VoiceActivityDetector};
use crate::audio_toolkit::constants;

const SILERO_FRAME_MS: u32 = 30;
const SILERO_FRAME_SAMPLES: u32 = constants::WHISPER_SAMPLE_RATE * SILERO_FRAME_MS / 1000;

pub struct SileroVad {
    engine: Vad,
    threshold: f32,
    quiet_speech: bool,
    frame_samples: usize,
}

impl SileroVad {
    pub fn new<P: AsRef<Path>>(model_path: P, threshold: f32) -> Result<Self> {
        if !(0.0..=1.0).contains(&threshold) {
            anyhow::bail!("threshold must be between 0.0 and 1.0");
        }

        let sample_rate = usize::try_from(constants::WHISPER_SAMPLE_RATE)
            .map_err(|_| anyhow::anyhow!("Whisper sample rate does not fit target usize"))?;
        let frame_samples = usize::try_from(SILERO_FRAME_SAMPLES)
            .map_err(|_| anyhow::anyhow!("Silero frame length does not fit target usize"))?;

        Ok(Self {
            engine: Vad::new(&model_path, sample_rate)
                .map_err(|e| anyhow::anyhow!("Failed to create VAD: {e}"))?,
            threshold,
            quiet_speech: false,
            frame_samples,
        })
    }
}

impl VoiceActivityDetector for SileroVad {
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
        if frame.len() != self.frame_samples {
            anyhow::bail!(
                "expected {} samples, got {}",
                self.frame_samples,
                frame.len()
            );
        }

        let result = self
            .engine
            .compute(frame)
            .map_err(|e| anyhow::anyhow!("Silero VAD error: {e}"))?;

        if result.prob > super::quiet_speech::detection_threshold(self.threshold, self.quiet_speech)
        {
            Ok(VadFrame::Speech(frame))
        } else {
            Ok(VadFrame::Noise)
        }
    }

    fn set_quiet_speech(&mut self, enabled: bool) {
        self.quiet_speech = enabled;
    }

    fn reset(&mut self) {
        // Clear the Silero LSTM hidden/cell state so a new session doesn't
        // inherit recurrent context from the previous recording.
        self.engine.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::SILERO_FRAME_SAMPLES;

    #[test]
    fn silero_frame_length_is_a_30_millisecond_whisper_window() {
        assert_eq!(SILERO_FRAME_SAMPLES, 480);
    }
}
