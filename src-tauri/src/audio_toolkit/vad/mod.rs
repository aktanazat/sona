use anyhow::Result;
use std::path::Path;

pub const VAD_PREFILL_FRAMES: usize = 15;
pub const VAD_OFFLINE_HANGOVER_FRAMES: usize = 15;
pub const VAD_STREAMING_HANGOVER_FRAMES: usize = 55;
pub const VAD_ONSET_FRAMES: usize = 2;

pub enum VadFrame<'a> {
    /// Speech – may aggregate several frames (prefill + current + hangover)
    Speech(&'a [f32]),
    /// Non-speech (silence, noise). Down-stream code can ignore it.
    Noise,
}

impl<'a> VadFrame<'a> {
    #[inline]
    pub fn is_speech(&self) -> bool {
        matches!(self, VadFrame::Speech(_))
    }
}

pub trait VoiceActivityDetector: Send + Sync {
    /// Primary streaming API: feed one 30-ms frame, get keep/drop decision.
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>>;

    fn is_voice(&mut self, frame: &[f32]) -> Result<bool> {
        Ok(self.push_frame(frame)?.is_speech())
    }

    /// Set the post-speech hangover tail (in 30 ms frames) applied to
    /// subsequent frames. Detectors without a smoothing tail can ignore this.
    fn set_hangover_frames(&mut self, _frames: usize) {}

    /// Relax this engine's own threshold for soft dictation. Meeting capture
    /// does not pass through the dictation detector.
    fn set_quiet_speech(&mut self, _enabled: bool) {}

    fn reset(&mut self) {}
}

pub(crate) mod quiet_speech;
mod silero;
mod smoothed;
mod ten;

pub use silero::SileroVad;
pub use smoothed::SmoothedVad;
pub use ten::TenVad;

/// Open the preferred detector for a 16 kHz capture: TEN-VAD when its weights
/// are present, Silero when they are not.
///
/// The two engines sit at different points on their own precision-recall
/// curves, so each carries its own threshold and neither may be opened with the
/// other's. Absent weights is a real failure mode rather than a hypothetical
/// one — a resource can go missing from a packaged build, a portable install or
/// a dev tree — and it degrades to the engine Sona shipped before, loudly.
///
/// Both outcomes are logged, including the one that worked. Which detector
/// listened is the first thing anyone diagnosing a missed or phantom utterance
/// needs, and a silent success would leave a machine that quietly fell back
/// indistinguishable from one that did not.
pub fn open_detector(
    ten_model: &Path,
    ten_threshold: f32,
    silero_model: &Path,
    silero_threshold: f32,
) -> Result<Box<dyn VoiceActivityDetector>> {
    if ten_model.exists() {
        match TenVad::new(ten_model, ten_threshold) {
            Ok(detector) => {
                log::info!("Voice activity detector: TEN-VAD at {ten_threshold}");
                return Ok(Box::new(detector));
            }
            Err(error) => log::warn!(
                "TEN-VAD at {} failed to open ({error}); falling back to Silero at {silero_threshold}",
                ten_model.display()
            ),
        }
    } else {
        log::warn!(
            "TEN-VAD weights are missing from {}; falling back to Silero at {silero_threshold}",
            ten_model.display()
        );
    }
    let detector = SileroVad::new(silero_model, silero_threshold)?;
    log::info!("Voice activity detector: Silero at {silero_threshold}");
    Ok(Box::new(detector))
}
