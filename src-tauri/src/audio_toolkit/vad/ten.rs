use anyhow::Result;
use ndarray::{Array2, Array3};
use ort::inputs;
use ort::value::TensorRef;
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::path::Path;
use std::sync::Arc;

use super::{VadFrame, VoiceActivityDetector};
use crate::audio_toolkit::constants;
use crate::audio_toolkit::ort_session::{
    build_session, InitializedSession, OrtProvider, SessionConfig, DEFAULT_VAD_PROVIDER,
};

/// TEN-VAD (Agora, TEN framework) behind the same trait as [`super::SileroVad`].
///
/// The shipped ONNX graph is a feature-in recurrent classifier, not an
/// audio-in model:
///
/// ```text
/// input  [0] input_1  f32 [-1,3,41]   3-frame context of 40 log-mel bands + 1 pitch
/// input  [1] input_2  f32 [-1,64]  |
/// input  [2] input_3  f32 [-1,64]  | recurrent state, fed from output_2/3/6/7
/// input  [3] input_6  f32 [-1,64]  |
/// input  [4] input_7  f32 [-1,64]  |
/// output [0] output_1 f32 [-1,-1,1] speech probability
/// output [1] output_2 f32 [1,64]   |
/// output [2] output_3 f32 [1,64]   | next recurrent state
/// output [3] output_6 f32 [1,64]   |
/// output [4] output_7 f32 [1,64]   |
/// ```
///
/// Turning audio into those 41 features is the whole front end below, ported
/// from the reference C implementation (TEN-framework/ten-vad `src/`): a
/// pre-emphasised 768-sample Hann STFT zero-padded to 1024, 40 triangular mel
/// bands normalised by the shipped per-band mean and standard deviation, plus
/// one pitch estimate from an LPCNet-derived tracker running on a 4x-decimated
/// LPC residual.
///
/// TEN-VAD analyses 256-sample (16 ms) hops while Sona's trait delivers
/// 480-sample (30 ms) frames. Frames are queued and drained hop by hop exactly
/// as the reference does for a mismatched external hop size, and a frame's
/// verdict is the strongest hop probability inside it, so `SmoothedVad`,
/// `VadConfig` and the recorder need no changes.
pub struct TenVad {
    session: InitializedSession,
    threshold: f32,
    quiet_speech: bool,
    frame_samples: usize,
    front_end: FrontEnd,
    /// `input_1`: the 3-frame, 41-dimensional feature context.
    context: Array3<f32>,
    /// `input_2/3/6/7`: recurrent state carried between hops.
    hidden: [Array2<f32>; HIDDEN_INPUTS],
}

const TEN_FRAME_MS: u32 = 30;
const TEN_FRAME_SAMPLES: u32 = constants::WHISPER_SAMPLE_RATE * TEN_FRAME_MS / 1000;

/// TEN-VAD analysis hop: 256 samples, 16 ms at 16 kHz.
const HOP: usize = 256;
/// Hann analysis window, zero-padded into [`FFT_SIZE`].
const WINDOW: usize = 768;
const FFT_SIZE: usize = 1024;
const BINS: usize = FFT_SIZE / 2 + 1;
const MEL_BANDS: usize = 40;
/// 40 mel bands plus one pitch value.
const FEATURES: usize = MEL_BANDS + 1;
const CONTEXT: usize = 3;
const HIDDEN: usize = 64;
const HIDDEN_INPUTS: usize = 4;
const PRE_EMPHASIS: f32 = 0.97;
const LOG_EPS: f32 = 1e-20;
const MEL_TOP_HZ: f32 = 8000.0;
const MEL_SCALE: f32 = 2595.0;
const MEL_BREAK_HZ: f32 = 700.0;

/// Graph input order, asserted when the session opens so the positional
/// binding below cannot silently drift if the model file is swapped.
const INPUT_NAMES: [&str; 1 + HIDDEN_INPUTS] =
    ["input_1", "input_2", "input_3", "input_6", "input_7"];

/// Per-band feature means, from the reference `AUP_AED_FEATURE_MEANS`.
const FEATURE_MEANS: [f32; FEATURES] = [
    -8.198_236_5,
    -6.265_716_6,
    -5.483_818_5,
    -4.758_691_3,
    -4.417_089,
    -4.142_892_8,
    -3.912_850_4,
    -3.845_928,
    -3.657_090_4,
    -3.723_418_7,
    -3.876_134_2,
    -3.843_890_9,
    -3.690_405_1,
    -3.756_065_8,
    -3.698_696_1,
    -3.650_463_1,
    -3.700_468_8,
    -3.567_321_3,
    -3.498_900_2,
    -3.477_807,
    -3.458_816,
    -3.444_923_9,
    -3.401_328_6,
    -3.306_261_3,
    -3.278_556_8,
    -3.233_250_9,
    -3.198_616,
    -3.204_526_4,
    -3.208_798_6,
    -3.257_838,
    -3.381_376_7,
    -3.534_021_4,
    -3.640_868,
    -3.726_858_9,
    -3.773_731,
    -3.804_667_2,
    -3.832_901,
    -3.871_120_5,
    -3.990_593,
    -4.480_289_5,
    92.356_903,
];

/// Per-band feature standard deviations, from `AUP_AED_FEATURE_STDS`.
const FEATURE_STDS: [f32; FEATURES] = [
    5.166_063_8,
    4.977_209_6,
    4.698_895_9,
    4.630_621_4,
    4.634_347_9,
    4.641_156_2,
    4.640_676_5,
    4.666_367,
    4.650_534_6,
    4.640_020_8,
    4.637_400_2,
    4.620_099,
    4.596_316_3,
    4.562_655,
    4.554_360_4,
    4.566_910_7,
    4.562_490,
    4.562_412_7,
    4.585_299_5,
    4.600_179_7,
    4.592_845_9,
    4.585_922_7,
    4.583_496_6,
    4.626_092_9,
    4.626_957_9,
    4.626_289_4,
    4.637_005_8,
    4.683_015_8,
    4.726_813_8,
    4.734_289_6,
    4.753_227_2,
    4.849_722_9,
    4.869_434_8,
    4.884_482_9,
    4.921_327,
    4.959_212_3,
    4.996_619_2,
    5.044_823_6,
    5.072_217,
    5.096_439_4,
    115.213_692,
];

impl TenVad {
    /// Open a TEN-VAD session. `threshold` is compared against the model's
    /// probability with the same strict `>` semantics as [`super::SileroVad`].
    pub fn new<P: AsRef<Path>>(model_path: P, threshold: f32) -> Result<Self> {
        Self::new_with_provider(model_path, threshold, DEFAULT_VAD_PROVIDER)
    }

    pub fn new_with_provider<P: AsRef<Path>>(
        model_path: P,
        threshold: f32,
        provider: OrtProvider,
    ) -> Result<Self> {
        if !(0.0..=1.0).contains(&threshold) {
            anyhow::bail!("threshold must be between 0.0 and 1.0");
        }
        let frame_samples = usize::try_from(TEN_FRAME_SAMPLES)
            .map_err(|_| anyhow::anyhow!("TEN-VAD frame length does not fit target usize"))?;

        let session = build_session(
            model_path.as_ref(),
            SessionConfig {
                label: "ten-vad",
                provider,
                intra_threads: Some(1),
                profiling_path: None,
            },
        )?;

        let inputs = session.inputs();
        let outputs = session.outputs();
        if inputs.len() != INPUT_NAMES.len() || outputs.len() != INPUT_NAMES.len() {
            anyhow::bail!(
                "TEN-VAD model must expose {} inputs and outputs, found {} and {}",
                INPUT_NAMES.len(),
                inputs.len(),
                outputs.len()
            );
        }
        for (index, expected) in INPUT_NAMES.iter().enumerate() {
            if inputs[index].name() != *expected {
                anyhow::bail!(
                    "TEN-VAD input {index} must be named {expected}, found {}",
                    inputs[index].name()
                );
            }
        }

        Ok(Self {
            session,
            threshold,
            quiet_speech: false,
            frame_samples,
            front_end: FrontEnd::new(),
            context: Array3::zeros((1, CONTEXT, FEATURES)),
            hidden: std::array::from_fn(|_| Array2::zeros((1, HIDDEN))),
        })
    }

    pub fn provider(&self) -> OrtProvider {
        self.session.provider()
    }

    pub fn session_init_duration(&self) -> std::time::Duration {
        self.session.init_duration()
    }

    /// Run the currently staged feature context through the model, advance the
    /// recurrent state, and return this hop's speech probability.
    fn run_hop(&mut self) -> Result<f32> {
        let Self {
            session,
            context,
            hidden,
            ..
        } = self;

        let mut next = [[0.0f32; HIDDEN]; HIDDEN_INPUTS];
        let probability = {
            let staged = TensorRef::from_array_view(context.view())
                .map_err(|e| anyhow::anyhow!("TEN-VAD feature tensor: {e}"))?;
            let carried: [TensorRef<'_, f32>; HIDDEN_INPUTS] = [
                TensorRef::from_array_view(hidden[0].view())
                    .map_err(|e| anyhow::anyhow!("TEN-VAD hidden tensor: {e}"))?,
                TensorRef::from_array_view(hidden[1].view())
                    .map_err(|e| anyhow::anyhow!("TEN-VAD hidden tensor: {e}"))?,
                TensorRef::from_array_view(hidden[2].view())
                    .map_err(|e| anyhow::anyhow!("TEN-VAD hidden tensor: {e}"))?,
                TensorRef::from_array_view(hidden[3].view())
                    .map_err(|e| anyhow::anyhow!("TEN-VAD hidden tensor: {e}"))?,
            ];
            let [state_2, state_3, state_6, state_7] = carried;
            let produced = session
                .run(inputs![staged, state_2, state_3, state_6, state_7])
                .map_err(|e| anyhow::anyhow!("TEN-VAD inference failed: {e}"))?;

            let probability = produced[0]
                .try_extract_array::<f32>()
                .map_err(|e| anyhow::anyhow!("TEN-VAD probability output: {e}"))?
                .iter()
                .copied()
                .next()
                .ok_or_else(|| anyhow::anyhow!("TEN-VAD produced no probability"))?;
            for (slot, target) in next.iter_mut().enumerate() {
                let values = produced[slot + 1]
                    .try_extract_array::<f32>()
                    .map_err(|e| anyhow::anyhow!("TEN-VAD hidden output: {e}"))?;
                if values.len() != HIDDEN {
                    anyhow::bail!("TEN-VAD hidden output must hold {HIDDEN} values");
                }
                for (cell, value) in target.iter_mut().zip(values.iter()) {
                    *cell = *value;
                }
            }
            probability
        };

        for (state, values) in hidden.iter_mut().zip(next.iter()) {
            if let Some(slice) = state.as_slice_mut() {
                slice.copy_from_slice(values);
            }
        }
        Ok(probability)
    }
}

impl VoiceActivityDetector for TenVad {
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
        if frame.len() != self.frame_samples {
            anyhow::bail!(
                "expected {} samples, got {}",
                self.frame_samples,
                frame.len()
            );
        }

        self.front_end.enqueue(frame);
        // A 480-sample frame always completes at least one 256-sample hop, so
        // every call produces a fresh verdict; the strongest hop decides it.
        let mut probability = 0.0f32;
        while self.front_end.advance() {
            if let Some(staged) = self.context.as_slice_mut() {
                staged.copy_from_slice(&self.front_end.features);
            }
            probability = probability.max(self.run_hop()?);
        }

        if probability > super::quiet_speech::detection_threshold(self.threshold, self.quiet_speech)
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
        self.front_end.reset();
        for state in &mut self.hidden {
            state.fill(0.0);
        }
        self.context.fill(0.0);
    }
}

/// Audio to a 41-dimensional feature context, one 16 ms hop at a time.
struct FrontEnd {
    fft: Arc<dyn Fft<f32>>,
    spectrum: Vec<Complex32>,
    /// `MEL_BANDS` rows of `BINS` triangular weights.
    mel_filters: Vec<f32>,
    window: [f32; WINDOW],
    /// Sliding 768-sample window of pre-emphasised samples.
    analysis: [f32; WINDOW],
    /// Samples not yet consumed by a hop: pre-emphasised for the STFT, raw for
    /// the pitch tracker, which the reference feeds from the un-emphasised FIFO.
    pending_emphasised: Vec<f32>,
    pending_raw: Vec<f32>,
    previous_sample: f32,
    power: [f32; BINS],
    pitch: PitchEstimator,
    /// The staged model input: `CONTEXT` frames of `FEATURES` values, oldest first.
    features: [f32; CONTEXT * FEATURES],
}

impl FrontEnd {
    fn new() -> Self {
        let mut planner = FftPlanner::new();
        let mut window = [0.0f32; WINDOW];
        for (index, weight) in window.iter_mut().enumerate() {
            let phase = 2.0 * std::f32::consts::PI * index as f32 / WINDOW as f32;
            *weight = 0.5 - 0.5 * phase.cos();
        }
        Self {
            fft: planner.plan_fft_forward(FFT_SIZE),
            spectrum: vec![Complex32::new(0.0, 0.0); FFT_SIZE],
            mel_filters: mel_filters(),
            window,
            analysis: [0.0; WINDOW],
            pending_emphasised: Vec::with_capacity(HOP * 2),
            pending_raw: Vec::with_capacity(HOP * 2),
            previous_sample: 0.0,
            power: [0.0; BINS],
            pitch: PitchEstimator::new(),
            features: [0.0; CONTEXT * FEATURES],
        }
    }

    fn reset(&mut self) {
        self.analysis = [0.0; WINDOW];
        self.pending_emphasised.clear();
        self.pending_raw.clear();
        self.previous_sample = 0.0;
        self.power = [0.0; BINS];
        self.features = [0.0; CONTEXT * FEATURES];
        self.pitch.reset();
    }

    fn enqueue(&mut self, frame: &[f32]) {
        for sample in frame {
            self.pending_emphasised
                .push(sample - PRE_EMPHASIS * self.previous_sample);
            self.previous_sample = *sample;
            self.pending_raw.push(*sample);
        }
    }

    /// Consume one hop if a full one is queued, leaving the model input in
    /// [`Self::features`]. Returns false when the queue is short of a hop.
    fn advance(&mut self) -> bool {
        if self.pending_emphasised.len() < HOP {
            return false;
        }

        self.analysis.copy_within(HOP.., 0);
        self.analysis[WINDOW - HOP..].copy_from_slice(&self.pending_emphasised[..HOP]);
        self.pending_emphasised.drain(..HOP);

        for (index, slot) in self.spectrum.iter_mut().enumerate() {
            let sample = match self.analysis.get(index) {
                Some(value) => value * self.window[index],
                None => 0.0,
            };
            *slot = Complex32::new(sample, 0.0);
        }
        self.fft.process(&mut self.spectrum);
        for (bin, slot) in self.power.iter_mut().enumerate() {
            let value = self.spectrum[bin];
            *slot = value.re * value.re + value.im * value.im;
        }

        let pitch = self.pitch.push_hop(&self.pending_raw[..HOP], &self.power);
        self.pending_raw.drain(..HOP);

        self.features.copy_within(FEATURES.., 0);
        let newest = (CONTEXT - 1) * FEATURES;
        for band in 0..MEL_BANDS {
            let filter = &self.mel_filters[band * BINS..(band + 1) * BINS];
            let mut energy = 0.0f32;
            for (bin, weight) in filter.iter().enumerate() {
                energy += self.power[bin] * weight;
            }
            self.features[newest + band] =
                ((energy + LOG_EPS).ln() - FEATURE_MEANS[band]) / (FEATURE_STDS[band] + LOG_EPS);
        }
        for extra in MEL_BANDS..FEATURES {
            self.features[newest + extra] =
                (pitch - FEATURE_MEANS[extra]) / (FEATURE_STDS[extra] + LOG_EPS);
        }
        true
    }
}

/// Triangular mel filter bank laid out as the reference builds
/// `melFilterBankCoef`: integer bin edges off the 2595*log10 mel scale between
/// 0 Hz and 8 kHz, each band rising to unity at its centre edge and back down.
fn mel_filters() -> Vec<f32> {
    let high_mel = MEL_SCALE * (1.0 + MEL_TOP_HZ / MEL_BREAK_HZ).log10();
    let mut edges = [0usize; MEL_BANDS + 2];
    for (index, edge) in edges.iter_mut().enumerate() {
        let mel = index as f32 * high_mel / (MEL_BANDS as f32 + 1.0);
        let hz = MEL_BREAK_HZ * (10.0f32.powf(mel / MEL_SCALE) - 1.0);
        *edge = ((FFT_SIZE as f32 + 1.0) * hz / constants::WHISPER_SAMPLE_RATE as f32) as usize;
    }

    let mut filters = vec![0.0f32; MEL_BANDS * BINS];
    for band in 0..MEL_BANDS {
        let (low, mid, high) = (edges[band], edges[band + 1], edges[band + 2]);
        for bin in low..mid {
            filters[band * BINS + bin] = (bin - low) as f32 / (mid - low) as f32;
        }
        for bin in mid..high {
            filters[band * BINS + bin] = (high - bin) as f32 / (high - mid) as f32;
        }
    }
    filters
}

// ---------------------------------------------------------------------------
// Pitch estimator
// ---------------------------------------------------------------------------

/// Bands the pitch tracker's LPC envelope is built from.
const PITCH_BANDS: usize = 18;
const LPC_ORDER: usize = 16;
/// The tracker runs at 4 kHz, a plain 4x decimation of the 16 kHz input.
const PITCH_DECIMATION: usize = 4;
const PITCH_FS: f32 = 4000.0;
const MIN_PERIOD: usize = 32 / PITCH_DECIMATION;
const MAX_PERIOD: usize = 256 / PITCH_DECIMATION;
const DIF_PERIOD: usize = MAX_PERIOD - MIN_PERIOD;
/// Correlation is scored over two half-hops per analysis hop.
const CORR_HALF_HOP: usize = HOP / (PITCH_DECIMATION * 2);
/// 40 ms of correlation history: three hops, two sub-frames each.
const PITCH_FRAMES: usize = 3;
const SUBFRAMES: usize = PITCH_FRAMES * 2;
/// The residual is read this far behind the newest sample so it lines up with
/// the spectrum the LPC envelope came from.
const XCORR_TRAINING_OFFSET: usize = 80;
const INPUT_Q: usize = HOP + HOP;
const ALIGN_OFFSET: usize = INPUT_Q - HOP - XCORR_TRAINING_OFFSET;
const EXC_BUF: usize = MAX_PERIOD + HOP / PITCH_DECIMATION + 1;
const VOICED_THRESHOLD: f32 = 0.4;
const PATH_PENALTY: f32 = 0.02;
/// The band table is written against an 80-point FFT and scaled from there.
const BAND_FFT_REFERENCE: f32 = 80.0;
const LOWPASS_SECTIONS: usize = 5;

const BAND_START: [usize; PITCH_BANDS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 34, 40,
];
const BAND_LPC_COMPENSATION: [f32; PITCH_BANDS] = [
    0.8, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.666_667, 0.5, 0.5, 0.5, 0.333_333, 0.25, 0.25, 0.2,
    0.166_667, 0.173_913,
];

/// 4 kHz anti-alias cascade, from `AUP_PE_{B,A,G}_4KHZ`.
const LOWPASS_B: [[f32; 3]; LOWPASS_SECTIONS] = [
    [1.0, 1.198_825, 1.0],
    [1.0, -0.567_461_4, 1.0],
    [1.0, -1.099_061, 1.0],
    [1.0, -1.265_846, 1.0],
    [1.0, -1.318_849, 1.0],
];
const LOWPASS_A: [[f32; 3]; LOWPASS_SECTIONS] = [
    [1.0, -1.445_267, 0.546_397_4],
    [1.0, -1.426_72, 0.682_013_8],
    [1.0, -1.408_255, 0.828_666_4],
    [1.0, -1.400_909, 0.924_032],
    [1.0, -1.408_242, 0.978_977_6],
];
const LOWPASS_G: f32 = 0.269_254_1;

/// The pitch chain's thresholds are absolute, not relative: the `1e-2` floor
/// under the band log, the `1 +` term in the correlation denominator and the
/// `1e-12` clamp are all written against int16-scale samples. The mel path is
/// scale-free (the reference divides its band powers by `32768^2` right back
/// out), so the front end stays in [-1, 1) and only the tracker is fed in
/// int16 units.
const INT16_FULL_SCALE: f32 = 32768.0;
const INT16_POWER_SCALE: f32 = INT16_FULL_SCALE * INT16_FULL_SCALE;

/// LPCNet-derived pitch tracker, ported from the reference `src/pitch_est.cc`.
///
/// Per hop: build an 18-band LPC envelope from the power spectrum, inverse
/// filter the raw signal through it, decimate the residual to 4 kHz, correlate
/// it against itself at every candidate period, then pick the period with a
/// penalised best-path search across the last 40 ms.
struct PitchEstimator {
    /// `PITCH_BANDS` x `PITCH_BANDS` DCT-II basis, shared by the forward and
    /// inverse transform.
    dct: [f32; PITCH_BANDS * PITCH_BANDS],
    /// `cos(2*pi*k/FFT_SIZE)`, indexed by `(bin * lag) % FFT_SIZE`, so the
    /// autocorrelation lags come out of the envelope without an inverse FFT.
    cosine: [f32; FFT_SIZE],
    input_q: [f32; INPUT_Q],
    residual: [f32; HOP],
    excitation: [f32; EXC_BUF],
    excitation_squared: [f32; EXC_BUF],
    lpc: [f32; LPC_ORDER],
    lpc_memory: [f32; LPC_ORDER],
    lpc_filtered: f32,
    lowpass: [[f32; 2]; LOWPASS_SECTIONS],
    /// Circular 40 ms history of normalised correlation, newest at
    /// `correlation_index`.
    correlation: [[f32; MAX_PERIOD + 1]; SUBFRAMES],
    correlation_scratch: [[f32; MAX_PERIOD + 1]; SUBFRAMES],
    correlation_index: usize,
    weight: [f32; SUBFRAMES],
    weight_normalised: [f32; SUBFRAMES],
    path_score: [[f32; MAX_PERIOD]; 2],
    path_back: [[usize; MAX_PERIOD]; SUBFRAMES],
    path_best: f32,
    best_period: usize,
}

impl PitchEstimator {
    fn new() -> Self {
        let mut dct = [0.0f32; PITCH_BANDS * PITCH_BANDS];
        for row in 0..PITCH_BANDS {
            for column in 0..PITCH_BANDS {
                let mut value = ((row as f32 + 0.5) * column as f32 * std::f32::consts::PI
                    / PITCH_BANDS as f32)
                    .cos();
                if column == 0 {
                    value *= 0.5f32.sqrt();
                }
                dct[row * PITCH_BANDS + column] = value;
            }
        }
        let mut cosine = [0.0f32; FFT_SIZE];
        for (index, slot) in cosine.iter_mut().enumerate() {
            *slot = (2.0 * std::f32::consts::PI * index as f32 / FFT_SIZE as f32).cos();
        }
        Self {
            dct,
            cosine,
            input_q: [0.0; INPUT_Q],
            residual: [0.0; HOP],
            excitation: [0.0; EXC_BUF],
            excitation_squared: [0.0; EXC_BUF],
            lpc: [0.0; LPC_ORDER],
            lpc_memory: [0.0; LPC_ORDER],
            lpc_filtered: 0.0,
            lowpass: [[0.0; 2]; LOWPASS_SECTIONS],
            correlation: [[0.0; MAX_PERIOD + 1]; SUBFRAMES],
            correlation_scratch: [[0.0; MAX_PERIOD + 1]; SUBFRAMES],
            correlation_index: 0,
            weight: [0.0; SUBFRAMES],
            weight_normalised: [0.0; SUBFRAMES],
            path_score: [[0.0; MAX_PERIOD]; 2],
            path_back: [[0; MAX_PERIOD]; SUBFRAMES],
            path_best: 0.0,
            best_period: 0,
        }
    }

    fn reset(&mut self) {
        self.input_q = [0.0; INPUT_Q];
        self.residual = [0.0; HOP];
        self.excitation = [0.0; EXC_BUF];
        self.excitation_squared = [0.0; EXC_BUF];
        self.lpc = [0.0; LPC_ORDER];
        self.lpc_memory = [0.0; LPC_ORDER];
        self.lpc_filtered = 0.0;
        self.lowpass = [[0.0; 2]; LOWPASS_SECTIONS];
        self.correlation = [[0.0; MAX_PERIOD + 1]; SUBFRAMES];
        self.correlation_scratch = [[0.0; MAX_PERIOD + 1]; SUBFRAMES];
        self.correlation_index = 0;
        self.weight = [0.0; SUBFRAMES];
        self.weight_normalised = [0.0; SUBFRAMES];
        self.path_score = [[0.0; MAX_PERIOD]; 2];
        self.path_back = [[0; MAX_PERIOD]; SUBFRAMES];
        self.path_best = 0.0;
        self.best_period = 0;
    }

    /// Estimate this hop's pitch in Hz; 0.0 when the hop reads as unvoiced.
    fn push_hop(&mut self, raw: &[f32], power: &[f32; BINS]) -> f32 {
        self.build_lpc(power);
        self.inverse_filter(raw);
        self.decimate_into_excitation();
        self.correlate();
        self.search_best_path()
    }

    /// Turn the power spectrum into a 16th-order LPC envelope, by way of an
    /// 18-band log-energy cepstrum.
    fn build_lpc(&mut self, power: &[f32; BINS]) {
        let mut bands = [0.0f32; PITCH_BANDS];
        band_energy(power, &mut bands);

        let mut log_bands = [0.0f32; PITCH_BANDS];
        let mut log_max = -2.0f32;
        let mut follow = -2.0f32;
        for (band, slot) in log_bands.iter_mut().enumerate() {
            let mut value = (1e-2 + bands[band]).log10();
            value = (log_max - 8.0).max((follow - 2.5).max(value));
            log_max = log_max.max(value);
            follow = (follow - 2.5).max(value);
            *slot = value;
        }

        let ratio = (2.0 / PITCH_BANDS as f32).sqrt();
        let mut cepstrum = [0.0f32; PITCH_BANDS];
        for (out, slot) in cepstrum.iter_mut().enumerate() {
            let mut sum = 0.0f32;
            for (inp, value) in log_bands.iter().enumerate() {
                sum += value * self.dct[inp * PITCH_BANDS + out];
            }
            *slot = sum * ratio;
        }

        let mut envelope = [0.0f32; PITCH_BANDS];
        for (out, slot) in envelope.iter_mut().enumerate() {
            let mut sum = 0.0f32;
            for (inp, value) in cepstrum.iter().enumerate() {
                sum += value * self.dct[out * PITCH_BANDS + inp];
            }
            *slot = 10.0f32.powf(sum * ratio) * BAND_LPC_COMPENSATION[out];
        }

        // Spread the envelope back onto FFT bins, drop Nyquist, then read the
        // first LPC_ORDER + 1 autocorrelation lags straight off it.
        let mut spectrum = [0.0f32; BINS];
        interpolate_bands(&envelope, &mut spectrum);
        spectrum[BINS - 1] = 0.0;

        let mut autocorrelation = [0.0f32; LPC_ORDER + 1];
        for (lag, slot) in autocorrelation.iter_mut().enumerate() {
            let mut sum = spectrum[0];
            for (bin, value) in spectrum[1..BINS - 1].iter().enumerate() {
                sum += 2.0 * value * self.cosine[((bin + 1) * lag) % FFT_SIZE];
            }
            *slot = 0.5 * sum;
        }

        // -40 dB noise floor, then lag windowing, both from the reference.
        let noise_floor = (WINDOW / 12) as f32 / 38.0;
        autocorrelation[0] += autocorrelation[0] * 1e-4 + noise_floor;
        for lag in 1..=LPC_ORDER {
            autocorrelation[lag] *= 1.0 - 6e-5 * (lag * lag) as f32;
        }
        levinson_durbin(&autocorrelation, &mut self.lpc);
    }

    /// Run the raw hop through the LPC inverse filter and its 0.7 comb tail.
    fn inverse_filter(&mut self, raw: &[f32]) {
        self.input_q.copy_within(HOP.., 0);
        for (slot, sample) in self.input_q[INPUT_Q - HOP..].iter_mut().zip(raw) {
            *slot = sample * INT16_FULL_SCALE;
        }

        for index in 0..HOP {
            let sample = self.input_q[ALIGN_OFFSET + index];
            let mut sum = sample;
            for (tap, coefficient) in self.lpc.iter().enumerate() {
                sum += coefficient * self.lpc_memory[tap];
            }
            self.lpc_memory.copy_within(..LPC_ORDER - 1, 1);
            self.lpc_memory[0] = sample;
            self.residual[index] = sum + 0.7 * self.lpc_filtered;
            self.lpc_filtered = sum;
        }
    }

    /// Low-pass the whole residual, then keep every fourth filtered sample and
    /// append the result to the correlation buffer. Every sample runs through
    /// the cascade because the filter is stateful; only the kept ones are read.
    fn decimate_into_excitation(&mut self) {
        let kept = HOP / PITCH_DECIMATION;
        self.excitation.copy_within(kept.., 0);
        for index in 0..HOP {
            let filtered = self.lowpass_sample(self.residual[index]);
            if index % PITCH_DECIMATION == 0 {
                self.excitation[EXC_BUF - kept + index / PITCH_DECIMATION] = filtered;
            }
        }
        for (index, slot) in self.excitation_squared.iter_mut().enumerate() {
            let value = self.excitation[index];
            *slot = value * value;
        }
    }

    /// One sample through the five-section anti-alias cascade.
    #[inline]
    fn lowpass_sample(&mut self, input: f32) -> f32 {
        let mut sample = input;
        for (section, state) in self.lowpass.iter_mut().enumerate() {
            let inner =
                sample - LOWPASS_A[section][1] * state[0] - LOWPASS_A[section][2] * state[1];
            sample = LOWPASS_G
                * (LOWPASS_B[section][0] * inner
                    + LOWPASS_B[section][1] * state[0]
                    + LOWPASS_B[section][2] * state[1]);
            *state = [inner, state[0]];
        }
        sample
    }

    /// Normalised autocorrelation over every candidate period, for both
    /// half-hops, written into the circular 40 ms history.
    fn correlate(&mut self) {
        self.weight.copy_within(2.., 0);

        for sub in 0..2 {
            let slot = 2 * self.correlation_index + sub;
            let offset = sub * CORR_HALF_HOP;

            let mut instant = [0.0f32; MAX_PERIOD];
            for (lag, value) in instant.iter_mut().enumerate() {
                let mut sum = 0.0f32;
                for tap in 0..CORR_HALF_HOP {
                    sum += self.excitation[MAX_PERIOD + offset + tap]
                        * self.excitation[offset + lag + tap];
                }
                *value = sum;
            }

            let reference: f32 = self.excitation_squared
                [MAX_PERIOD + offset..MAX_PERIOD + offset + CORR_HALF_HOP]
                .iter()
                .sum();
            self.weight[SUBFRAMES - 2 + sub] = reference;

            let mut window: f32 = self.excitation_squared[offset..offset + CORR_HALF_HOP]
                .iter()
                .sum();
            let mut denominator = 1e-12f32.max(window + (1.0 + reference));
            self.correlation[slot][0] = 2.0 * instant[0] / denominator;
            for lag in 1..MAX_PERIOD {
                window = 0.0f32.max(window - self.excitation_squared[offset + lag - 1]);
                window += self.excitation_squared[offset + lag + CORR_HALF_HOP - 1];
                denominator = 1e-12f32.max(window + (1.0 + reference));
                self.correlation[slot][lag] = 2.0 * instant[lag] / denominator;
            }

            // Suppress candidates no stronger than their own octave.
            for lag in 0..MAX_PERIOD - 2 * MIN_PERIOD {
                let octave = self.correlation[slot][(MAX_PERIOD + lag) / 2]
                    .max(self.correlation[slot][(MAX_PERIOD + lag + 2) / 2])
                    .max(self.correlation[slot][(MAX_PERIOD + lag - 1) / 2]);
                if self.correlation[slot][lag] < octave * 1.1 {
                    self.correlation[slot][lag] *= 0.8;
                }
            }
        }

        self.correlation_index = (self.correlation_index + 1) % PITCH_FRAMES;
    }

    /// Penalised best-path search over the correlation history, then a weighted
    /// linear fit of the period contour. Returns pitch in Hz, 0.0 if unvoiced.
    fn search_best_path(&mut self) -> f32 {
        let total: f32 = 1e-15 + self.weight.iter().sum::<f32>();
        for (sub, slot) in self.weight_normalised.iter_mut().enumerate() {
            *slot = self.weight[sub] * (SUBFRAMES as f32 / total);
        }
        self.correlation_scratch = self.correlation;
        self.path_back.copy_within(2.., 0);

        for sub in SUBFRAMES - 2..SUBFRAMES {
            let scored = (sub + self.correlation_index * 2) % SUBFRAMES;
            for lag in 0..DIF_PERIOD {
                let mut best = self.path_best - 1e10;
                self.path_back[sub][lag] = self.best_period;
                let mut step = (4 - lag as isize).min(0);
                while step <= 4 {
                    let candidate = lag as isize + step;
                    if candidate >= DIF_PERIOD as isize {
                        break;
                    }
                    let score = self.path_score[0][candidate as usize]
                        - PATH_PENALTY * (step * step) as f32;
                    if score > best {
                        best = score;
                        self.path_back[sub][lag] = candidate as usize;
                    }
                    step += 1;
                }
                self.path_score[1][lag] =
                    best + self.weight_normalised[sub] * self.correlation_scratch[scored][lag];
            }

            let mut best = -1e15f32;
            let mut argmax = 0usize;
            for lag in 0..DIF_PERIOD {
                if self.path_score[1][lag] > best {
                    best = self.path_score[1][lag];
                    argmax = lag;
                }
            }
            self.path_best = best;
            self.best_period = argmax;

            self.path_score[0] = self.path_score[1];
            for lag in 0..DIF_PERIOD {
                self.path_score[0][lag] -= best;
            }
        }

        let mut cursor = self.best_period;
        let mut correlation = 0.0f32;
        let mut periods = [0.0f32; SUBFRAMES];
        for sub in (0..SUBFRAMES).rev() {
            periods[sub] = (MAX_PERIOD - cursor) as f32;
            let scored = (sub + self.correlation_index * 2) % SUBFRAMES;
            correlation += self.weight_normalised[sub] * self.correlation_scratch[scored][cursor];
            cursor = self.path_back[sub][cursor];
        }
        correlation = 0.0f32.max(correlation / SUBFRAMES as f32);
        let voiced = correlation >= VOICED_THRESHOLD;

        let (mut sum_w, mut sum_x, mut sum_xx, mut sum_xy, mut sum_y) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (sub, weight) in self.weight_normalised.iter().enumerate() {
            let position = sub as f32;
            sum_w += weight;
            sum_x += weight * position;
            sum_xx += weight * position * position;
            sum_xy += weight * position * periods[sub];
            sum_y += weight * periods[sub];
        }

        let denominator = sum_w * sum_xx - sum_x * sum_x;
        let mut slope = if denominator == 0.0 {
            (sum_w * sum_xy - sum_x * sum_y) / 1e-15
        } else {
            (sum_w * sum_xy - sum_x * sum_y) / denominator
        };
        if voiced {
            let bound = (sum_y / sum_w) / (4 * SUBFRAMES) as f32;
            slope = bound.min((-bound).max(slope));
        } else {
            slope = 0.0;
        }
        let period = (sum_y - slope * sum_x) / sum_w + 5.5 * slope;

        if voiced {
            PITCH_FS / 1.0f32.max(period)
        } else {
            0.0
        }
    }
}

/// Fold a 513-bin power spectrum into the tracker's 18 overlapping bands.
fn band_energy(power: &[f32; BINS], bands: &mut [f32; PITCH_BANDS]) {
    let rate = FFT_SIZE as f32 / BAND_FFT_REFERENCE;
    bands.fill(0.0);
    for band in 0..PITCH_BANDS - 1 {
        let width = (((BAND_START[band + 1] - BAND_START[band]) as f32) * rate).round() as usize;
        let offset = ((BAND_START[band] as f32) * rate).round() as usize;
        for step in 0..width {
            let fraction = step as f32 / width as f32;
            let bin = (offset + step).min(BINS - 1);
            bands[band] += (1.0 - fraction) * power[bin];
            bands[band + 1] += fraction * power[bin];
        }
    }
    bands[0] *= 2.0;
    bands[PITCH_BANDS - 1] *= 2.0;
    for band in bands.iter_mut() {
        *band *= INT16_POWER_SCALE;
    }
}

/// Inverse of [`band_energy`]: spread band gains back over FFT bins.
fn interpolate_bands(bands: &[f32; PITCH_BANDS], spectrum: &mut [f32; BINS]) {
    let rate = FFT_SIZE as f32 / BAND_FFT_REFERENCE;
    spectrum.fill(0.0);
    for band in 0..PITCH_BANDS - 1 {
        let width = (((BAND_START[band + 1] - BAND_START[band]) as f32) * rate).round() as usize;
        let offset = ((BAND_START[band] as f32) * rate).round() as usize;
        for step in 0..width {
            let fraction = step as f32 / width as f32;
            let bin = (offset + step).min(BINS - 1);
            spectrum[bin] = (1.0 - fraction) * bands[band] + fraction * bands[band + 1];
        }
    }
}

/// Levinson-Durbin recursion, matching the reference `celt_lpc` including its
/// 30 dB early exit.
fn levinson_durbin(autocorrelation: &[f32; LPC_ORDER + 1], lpc: &mut [f32; LPC_ORDER]) {
    lpc.fill(0.0);
    if autocorrelation[0] == 0.0 {
        return;
    }
    let mut error = autocorrelation[0];
    for order in 0..LPC_ORDER {
        let mut sum = autocorrelation[order + 1];
        for tap in 0..order {
            sum += lpc[tap] * autocorrelation[order - tap];
        }
        let reflection = -sum / error;
        lpc[order] = reflection;
        for tap in 0..(order + 1) >> 1 {
            let low = lpc[tap];
            let high = lpc[order - 1 - tap];
            lpc[tap] = low + reflection * high;
            lpc[order - 1 - tap] = high + reflection * low;
        }
        error -= reflection * reflection * error;
        if error < 0.001 * autocorrelation[0] {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_frame_length_is_a_30_millisecond_whisper_window() {
        assert_eq!(TEN_FRAME_SAMPLES, 480);
    }

    #[test]
    fn analysis_geometry_matches_the_reference_configuration() {
        assert_eq!(HOP, 256);
        assert_eq!(WINDOW, 768);
        assert_eq!(FFT_SIZE, 1024);
        assert_eq!(BINS, 513);
        assert_eq!(FEATURES, 41);
        assert_eq!(MIN_PERIOD, 8);
        assert_eq!(MAX_PERIOD, 64);
        assert_eq!(DIF_PERIOD, 56);
        assert_eq!(CORR_HALF_HOP, 32);
        assert_eq!(SUBFRAMES, 6);
        assert_eq!(INPUT_Q, 512);
        assert_eq!(ALIGN_OFFSET, 176);
        assert_eq!(EXC_BUF, 129);
    }

    #[test]
    fn mel_filters_cover_every_band_with_a_rising_and_falling_edge() {
        let filters = mel_filters();
        assert_eq!(filters.len(), MEL_BANDS * BINS);
        for band in 0..MEL_BANDS {
            let row = &filters[band * BINS..(band + 1) * BINS];
            let peak = row.iter().copied().fold(0.0f32, f32::max);
            assert!(peak > 0.99, "band {band} never reaches unity gain");
            assert!(row.iter().all(|weight| (0.0..=1.0).contains(weight)));
        }
    }

    #[test]
    fn levinson_durbin_recovers_a_first_order_pole() {
        // Autocorrelation of x[n] = 0.5 x[n-1] + e[n] is r[k] = 0.5^k, whose
        // predictor is a[1] = 0.5 with every higher coefficient zero. The
        // reference stores the negated predictor, so lpc[0] is -0.5.
        let mut autocorrelation = [0.0f32; LPC_ORDER + 1];
        for (lag, slot) in autocorrelation.iter_mut().enumerate() {
            *slot = 0.5f32.powi(lag as i32);
        }
        let mut lpc = [0.0f32; LPC_ORDER];
        levinson_durbin(&autocorrelation, &mut lpc);
        assert!(
            (lpc[0] + 0.5).abs() < 1e-5,
            "first coefficient should recover the pole, got {}",
            lpc[0]
        );
        assert!(lpc[1..].iter().all(|value| value.abs() < 1e-5));
    }

    #[test]
    fn band_energy_and_interpolation_round_trip_a_flat_spectrum() {
        let power = [1.0f32; BINS];
        let mut bands = [0.0f32; PITCH_BANDS];
        band_energy(&power, &mut bands);
        assert!(
            bands.iter().all(|value| *value > 0.0),
            "every band must draw energy from a flat spectrum"
        );
        let mut spectrum = [0.0f32; BINS];
        interpolate_bands(&bands, &mut spectrum);
        assert!(spectrum[0] > 0.0);
        assert!(spectrum[BINS - 2] > 0.0);
    }

    /// 1 s of a harmonic complex at `f0`, band-limited below Nyquist and
    /// peak-normalised to 0.2, i.e. a speech-level periodic excitation.
    fn harmonic_complex(f0: f32) -> Vec<f32> {
        let rate = constants::WHISPER_SAMPLE_RATE as f32;
        let mut signal = vec![0.0f32; constants::WHISPER_SAMPLE_RATE as usize];
        for (index, sample) in signal.iter_mut().enumerate() {
            let mut value = 0.0f32;
            for harmonic in 1..=20u32 {
                let frequency = f0 * harmonic as f32;
                if frequency >= rate / 2.0 {
                    break;
                }
                value += (1.0 / harmonic as f32)
                    * (2.0 * std::f32::consts::PI * frequency * index as f32 / rate).sin();
            }
            *sample = value;
        }
        let peak = signal
            .iter()
            .fold(0.0f32, |worst, value| worst.max(value.abs()));
        for sample in signal.iter_mut() {
            *sample = *sample / peak * 0.2;
        }
        signal
    }

    /// Drive the front end frame by frame and read the pitch feature back out
    /// of every hop, de-normalised to Hz.
    fn tracked_pitch(signal: &[f32]) -> Vec<f32> {
        let mut front_end = FrontEnd::new();
        let mut observed = Vec::new();
        let slot = (CONTEXT - 1) * FEATURES + MEL_BANDS;
        for frame in signal.chunks_exact(TEN_FRAME_SAMPLES as usize) {
            front_end.enqueue(frame);
            while front_end.advance() {
                observed.push(
                    front_end.features[slot] * (FEATURE_STDS[MEL_BANDS] + LOG_EPS)
                        + FEATURE_MEANS[MEL_BANDS],
                );
            }
        }
        observed
    }

    #[test]
    fn pitch_tracker_locks_onto_a_periodic_excitation() {
        // Expected values are what the reference C implementation reports for
        // these exact signals. They are not f0 itself: the tracker resolves
        // periods as whole 4 kHz samples, so 120 Hz lands on 4000/33 = 121.21
        // and 200 Hz on 4000/20 = 200.00.
        for (f0, expected) in [(120.0f32, 121.21f32), (200.0, 200.00)] {
            let tracked = tracked_pitch(&harmonic_complex(f0));
            assert!(
                tracked.len() > 40,
                "one second of audio should yield many hops, got {}",
                tracked.len()
            );
            for (hop, value) in tracked.iter().enumerate().skip(20) {
                assert!(
                    (value - expected).abs() < 0.5,
                    "f0 {f0}: hop {hop} tracked {value} Hz, expected {expected} Hz"
                );
            }
        }
    }

    #[test]
    fn pitch_tracker_reports_nothing_for_aperiodic_and_silent_input() {
        let voiced = |signal: &[f32]| {
            tracked_pitch(signal)
                .iter()
                .skip(20)
                .filter(|value| **value > 0.0)
                .count()
        };

        let silence = vec![0.0f32; constants::WHISPER_SAMPLE_RATE as usize];
        assert_eq!(
            voiced(&silence),
            0,
            "digital silence must never read voiced"
        );

        // Deterministic xorshift noise, zero-mean, at the tone's level.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let noise: Vec<f32> = (0..constants::WHISPER_SAMPLE_RATE)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                ((state >> 40) as f32 / 8_388_608.0 - 1.0) * 0.2
            })
            .collect();
        let voiced_hops = voiced(&noise);
        assert!(
            voiced_hops < 8,
            "aperiodic noise should rarely read voiced, got {voiced_hops} hops"
        );
    }

    fn bundled(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources/models")
            .join(name)
    }

    /// 20 s of room-noise-like bed with one quiet utterance buried in the
    /// middle, at the levels measured on the real fixture this stands in for
    /// (bed rms 0.0145, utterance peak 0.029 — the speech is far below the
    /// bed's peak and contributes nothing to it, as in DictationTrust's
    /// `snr-lo`). The bed is low-passed because room noise is low-frequency
    /// dominated; flat white noise at the same level is a harsher signal than
    /// any microphone produces and both detectors read it differently.
    fn long_low_snr_capture(utterance_at: usize, utterance: &[f32]) -> Vec<f32> {
        const BED_RMS: f32 = 0.0145;
        const UTTERANCE_PEAK: f32 = 0.029;

        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut low_passed = 0.0f32;
        let mut capture: Vec<f32> = (0..constants::WHISPER_SAMPLE_RATE as usize * 20)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let white = (state >> 40) as f32 / 8_388_608.0 - 1.0;
                low_passed += 0.08 * (white - low_passed);
                low_passed
            })
            .collect();
        let rms =
            (capture.iter().map(|value| value * value).sum::<f32>() / capture.len() as f32).sqrt();
        for sample in capture.iter_mut() {
            *sample *= BED_RMS / rms;
        }

        let peak = utterance
            .iter()
            .fold(0.0f32, |worst, value| worst.max(value.abs()));
        for (offset, sample) in utterance.iter().enumerate() {
            capture[utterance_at + offset] += sample * (UTTERANCE_PEAK / peak);
        }
        capture
    }

    /// The regression this whole slice exists to prevent.
    ///
    /// A capture longer than `VAD_SILENCE_ARBITRATION_MAX_SAMPLES` (15 s) is the
    /// one regime where the VAD's answer is final: below the cutoff a
    /// VAD-silent capture is still handed to the model, above it the verdict
    /// stands alone and a false negative loses the transcript outright. Silero
    /// at its own operating point forwards nothing here; TEN-VAD recovers the
    /// utterance. Both go through the real `SmoothedVad` with the app's
    /// constants, so this is measured where the app forwards from.
    #[test]
    fn ten_vad_recovers_a_long_quiet_capture_silero_forwards_nothing_from() {
        use super::super::{
            SileroVad, SmoothedVad, VAD_OFFLINE_HANGOVER_FRAMES, VAD_ONSET_FRAMES,
            VAD_PREFILL_FRAMES,
        };

        let frame = TEN_FRAME_SAMPLES as usize;
        let utterance = harmonic_complex(150.0);
        let utterance_at = constants::WHISPER_SAMPLE_RATE as usize * 10;
        let capture = long_low_snr_capture(utterance_at, &utterance);
        assert!(
            capture.len() > constants::WHISPER_SAMPLE_RATE as usize * 15,
            "the fixture must sit above the arbitration cutoff to test this class"
        );
        let utterance_frames =
            utterance_at / frame..(utterance_at + utterance.len()).div_ceil(frame);

        let forwarded = |mut detector: SmoothedVad| {
            capture
                .chunks_exact(frame)
                .enumerate()
                .filter(|(_, chunk)| matches!(detector.push_frame(chunk), Ok(VadFrame::Speech(_))))
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        };

        let silero = forwarded(SmoothedVad::new(
            Box::new(SileroVad::new(bundled("silero_vad_v4.onnx"), 0.3).expect("open Silero")),
            VAD_PREFILL_FRAMES,
            VAD_OFFLINE_HANGOVER_FRAMES,
            VAD_ONSET_FRAMES,
        ));
        assert!(
            silero.is_empty(),
            "Silero at 0.3 is expected to forward nothing on this fixture; it forwarded {} frames \
             starting at {:?}. If this now passes, the fixture no longer probes the class.",
            silero.len(),
            silero.first()
        );

        let ten = forwarded(SmoothedVad::new(
            Box::new(TenVad::new(bundled("ten-vad.onnx"), 0.55).expect("open TEN-VAD")),
            VAD_PREFILL_FRAMES,
            VAD_OFFLINE_HANGOVER_FRAMES,
            VAD_ONSET_FRAMES,
        ));
        assert!(
            ten.iter().any(|index| utterance_frames.contains(index)),
            "TEN-VAD must forward part of the utterance at frames {utterance_frames:?}; \
             it forwarded {ten:?}"
        );
    }
}
