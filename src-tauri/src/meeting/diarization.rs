use crate::meeting::types::{DiarizationStatus, SpeakerAssignmentKind};
use hf_hub::api::tokio::ApiBuilder;
use hf_hub::{Repo, RepoType};
use ndarray::Array3;
use ort::inputs;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::TensorRef;
use rustfft::num_complex::Complex32;
use rustfft::num_traits::ToPrimitive;
use rustfft::{Fft, FftPlanner};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::LazyLock;
use std::sync::{Arc, Mutex};
use std::thread;
use transcribe_cpp::{
    Diarize, Model as SortformerModel, RunExtension, RunOptions, Session as SortformerRunSession,
    SortformerPreset, SortformerStreamOptions, SpeakerSegment,
};

pub(crate) const SAMPLE_RATE_HZ: usize = 16_000;
const FRAME_SAMPLES: usize = 400;
const FRAME_HOP_SAMPLES: usize = 160;
const FFT_SIZE: usize = 512;
const MEL_BINS: usize = 80;
pub(crate) const SPEAKER_EMBEDDING_DIMENSIONS: usize = 256;
const MIN_WINDOW_SAMPLES: usize = SAMPLE_RATE_HZ;
const SPEAKER_MATCH_THRESHOLD: f32 = 0.72;
const OVERLAP_MARGIN: f32 = 0.04;
const PROFILE_MATCH_THRESHOLD: f32 = 0.80;
const PROFILE_MATCH_MARGIN: f32 = 0.08;
const WESPEAKER_FEATURE_PIPELINE_REVISION: &str =
    "kaldi-fbank-80-pcm16-hamming-400-hop-160-fft-512-per-bin-cmn-v2";
const WESPEAKER_NORMALIZATION: &str = "l2-v1";
const NORMALIZED_EMBEDDING_NORM_SQUARED_TOLERANCE: f32 = 1e-3;

/// Nanoseconds per second, for offset/sample conversions.
pub(crate) const NS_PER_SECOND: u64 = 1_000_000_000;

/// Nanoseconds per sample at [`SAMPLE_RATE_HZ`]. One constant serves both
/// conversions below, which are exact because 16 kHz divides a second evenly —
/// and neither of them has to multiply, so neither can overflow.
const NS_PER_SAMPLE: u64 = NS_PER_SECOND / 16_000;

/// Sortformer scores a whole track in one pass, so the track's 16 kHz mono f32
/// audio is resident while that pass runs: 64 KB per second, ~230 MB per hour.
/// Past this ceiling the run would cost more memory than a desktop app should
/// take for one post-processing step, so the WeSpeaker fallback — which streams
/// window by window in constant memory — takes the track instead.
const SORTFORMER_MAX_PRIME_SAMPLES: usize = 2 * 3_600 * SAMPLE_RATE_HZ;

/// A window is called [`SpeakerAssignmentKind::Overlap`] when two speakers are
/// audible *at the same time* for at least this share of it. Sequential turns
/// inside one window are not overlap: they are two speakers taking turns, and
/// the longer-active one owns the window.
const SORTFORMER_OVERLAP_SHARE: f64 = 0.20;

/// Frame-level activity below this probability is not treated as speech.
/// Matches the runtime's own segment threshold; segments arrive pre-thresholded,
/// so this only rejects rows the model itself marked unavailable (`p` NaN).
const SORTFORMER_MIN_ACTIVITY: f32 = 0.0;

#[derive(Clone, Debug, Deserialize)]
pub struct DiarizationModelManifest {
    pub id: String,
    pub revision: String,
    pub filename: String,
    pub local_filename: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub license: String,
    pub sample_rate_hz: u32,
    /// WeSpeaker only: mel bins its ONNX graph expects.
    #[serde(default)]
    pub feature_bins: usize,
    /// WeSpeaker only: embedding width its ONNX graph returns.
    #[serde(default)]
    pub embedding_dimensions: usize,
    /// Sortformer only: speakers the checkpoint can separate.
    #[serde(default)]
    pub max_speakers: u32,
}

/// The primary engine: NVIDIA Streaming Sortformer, end-to-end diarization with
/// published DER (14.73% on AMI IHM test at this quant, measured by the GGUF
/// publisher against the NeMo reference).
static SORTFORMER_MANIFEST: LazyLock<DiarizationModelManifest> = LazyLock::new(|| {
    // PANIC: the bundled, compile-time JSON asset is part of the application binary.
    serde_json::from_str(include_str!(
        "../../resources/models/meeting-diarization-sortformer.json"
    ))
    .expect("meeting sortformer manifest is valid")
});

/// The structural fallback: WeSpeaker embeddings plus online cosine clustering.
/// Kept for tracks Sortformer cannot take (weights absent, or audio past
/// [`SORTFORMER_MAX_PRIME_SAMPLES`]), never preferred over Sortformer.
static WESPEAKER_MANIFEST: LazyLock<DiarizationModelManifest> = LazyLock::new(|| {
    // PANIC: the bundled, compile-time JSON asset is part of the application binary.
    serde_json::from_str(include_str!(
        "../../resources/models/meeting-diarization.json"
    ))
    .expect("meeting diarization manifest is valid")
});

/// The model Sona diarizes with when nothing is missing. Callers that record
/// "which model diarizes here" (session snapshots, store defaults) want this;
/// a generation record carries the engine that actually ran, via
/// [`PreparedDiarizationModel`].
pub fn model_manifest() -> &'static DiarizationModelManifest {
    &SORTFORMER_MANIFEST
}

pub fn wespeaker_manifest() -> &'static DiarizationModelManifest {
    &WESPEAKER_MANIFEST
}

/// Every field that must agree before two WeSpeaker vectors can be compared.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SpeakerEmbeddingModelKey {
    pub model_id: &'static str,
    pub model_revision: &'static str,
    pub model_sha256: &'static str,
    pub embedding_dimensions: usize,
    pub sample_rate_hz: u32,
    pub feature_bins: usize,
    pub feature_pipeline_revision: &'static str,
    pub normalization: &'static str,
}

pub(crate) fn wespeaker_embedding_model_key() -> SpeakerEmbeddingModelKey {
    let manifest = wespeaker_manifest();
    SpeakerEmbeddingModelKey {
        model_id: manifest.id.as_str(),
        model_revision: manifest.revision.as_str(),
        model_sha256: manifest.sha256.as_str(),
        embedding_dimensions: manifest.embedding_dimensions,
        sample_rate_hz: manifest.sample_rate_hz,
        feature_bins: manifest.feature_bins,
        feature_pipeline_revision: WESPEAKER_FEATURE_PIPELINE_REVISION,
        normalization: WESPEAKER_NORMALIZATION,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiarizationModelAvailability {
    Ready,
    Unavailable,
    Downloading,
    Failed,
}

impl DiarizationModelAvailability {
    pub const fn status(self) -> DiarizationStatus {
        match self {
            Self::Ready => DiarizationStatus::NotRequested,
            Self::Unavailable => DiarizationStatus::ModelUnavailable,
            Self::Downloading => DiarizationStatus::Downloading,
            Self::Failed => DiarizationStatus::Failed,
        }
    }
}

#[derive(Clone)]
pub struct MeetingDiarizer {
    state: Arc<Mutex<DownloadState>>,
    allow_download: bool,
    #[cfg(test)]
    wespeaker_ensure_requests: Arc<AtomicUsize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DownloadState {
    Idle,
    Downloading,
    Failed,
}

impl Default for MeetingDiarizer {
    fn default() -> Self {
        Self::new()
    }
}

impl MeetingDiarizer {
    pub fn new() -> Self {
        Self::with_download(true)
    }

    fn with_download(allow_download: bool) -> Self {
        Self {
            state: Arc::new(Mutex::new(DownloadState::Idle)),
            allow_download,
            #[cfg(test)]
            wespeaker_ensure_requests: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[cfg(test)]
    pub fn without_download() -> Self {
        Self::with_download(false)
    }

    pub fn availability(&self, model_directory: &Path) -> DiarizationModelAvailability {
        if verified_asset_path(model_directory, model_manifest()).is_some()
            || verified_asset_path(model_directory, wespeaker_manifest()).is_some()
        {
            return DiarizationModelAvailability::Ready;
        }
        match *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            DownloadState::Idle => DiarizationModelAvailability::Unavailable,
            DownloadState::Downloading => DiarizationModelAvailability::Downloading,
            DownloadState::Failed => DiarizationModelAvailability::Failed,
        }
    }

    /// Resolve the engine for this track. Sortformer wins whenever its weights
    /// are on disk. A WeSpeaker install with no Sortformer weights yet still
    /// diarizes — with the old engine, named in the log — while the primary
    /// downloads in the background. With neither asset present the caller keeps
    /// the explicit unavailable state: no substitute engine, no remote path.
    pub fn prepare(
        &self,
        model_directory: &Path,
    ) -> Result<PreparedDiarizationModel, DiarizationError> {
        if let Some(path) = verified_asset_path(model_directory, model_manifest()) {
            return Ok(PreparedDiarizationModel {
                path,
                engine: DiarizationEngineKind::Sortformer,
            });
        }
        let fallback = verified_asset_path(model_directory, wespeaker_manifest());
        self.start_download(model_directory);
        match fallback {
            Some(path) => Ok(PreparedDiarizationModel {
                path,
                engine: DiarizationEngineKind::WeSpeaker,
            }),
            None => Err(DiarizationError::ModelUnavailable),
        }
    }

    /// Ensure the separately pinned WeSpeaker asset for an explicit enrollment.
    /// Ordinary diarization calls prepare and never reach this method.
    pub(crate) async fn ensure_wespeaker(
        &self,
        model_directory: &Path,
    ) -> Result<PreparedDiarizationModel, DiarizationError> {
        #[cfg(test)]
        self.wespeaker_ensure_requests
            .fetch_add(1, Ordering::Relaxed);

        if let Some(prepared) = self.wespeaker_fallback(model_directory) {
            return Ok(prepared);
        }
        if !self.allow_download {
            return Err(DiarizationError::ModelUnavailable);
        }
        download_asset(model_directory, wespeaker_manifest()).await?;
        self.wespeaker_fallback(model_directory)
            .ok_or(DiarizationError::ModelInvalid)
    }

    /// Start at most one background download of the primary asset. Already
    /// downloading, or downloads disabled: does nothing.
    fn start_download(&self, model_directory: &Path) {
        if !self.allow_download {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *state != DownloadState::Idle && *state != DownloadState::Failed {
            return;
        }
        *state = DownloadState::Downloading;
        let state = Arc::clone(&self.state);
        let model_directory = model_directory.to_path_buf();
        thread::spawn(move || {
            let result =
                tauri::async_runtime::block_on(download_asset(&model_directory, model_manifest()));
            let mut state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *state = if result.is_ok() {
                DownloadState::Idle
            } else {
                DownloadState::Failed
            };
        });
    }

    /// Open the prepared engine. The returned session reports which engine it
    /// is, so the caller can log it and record it on the generation.
    pub fn open(
        &self,
        prepared: &PreparedDiarizationModel,
    ) -> Result<MeetingDiarizationSession, DiarizationError> {
        match prepared.engine {
            DiarizationEngineKind::Sortformer => SortformerDiarizationSession::open(&prepared.path)
                .map(MeetingDiarizationSession::Sortformer),
            DiarizationEngineKind::WeSpeaker => OnnxDiarizationSession::open(&prepared.path)
                .map(MeetingDiarizationSession::WeSpeaker),
        }
    }

    /// The WeSpeaker asset, when it is on disk, for a track Sortformer refused.
    pub fn wespeaker_fallback(&self, model_directory: &Path) -> Option<PreparedDiarizationModel> {
        verified_asset_path(model_directory, wespeaker_manifest()).map(|path| {
            PreparedDiarizationModel {
                path,
                engine: DiarizationEngineKind::WeSpeaker,
            }
        })
    }
}

/// Which diarizer backs a session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiarizationEngineKind {
    Sortformer,
    WeSpeaker,
}

impl DiarizationEngineKind {
    /// The manifest describing this engine's asset.
    pub fn manifest(self) -> &'static DiarizationModelManifest {
        match self {
            Self::Sortformer => model_manifest(),
            Self::WeSpeaker => wespeaker_manifest(),
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Sortformer => "sortformer",
            Self::WeSpeaker => "wespeaker",
        }
    }
}

/// A verified on-disk asset plus the engine that reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedDiarizationModel {
    pub path: PathBuf,
    pub engine: DiarizationEngineKind,
}

impl PreparedDiarizationModel {
    pub fn model_id(&self) -> &'static str {
        &self.engine.manifest().id
    }

    pub fn model_revision(&self) -> &'static str {
        &self.engine.manifest().revision
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiarizationError {
    ModelUnavailable,
    ModelInvalid,
    InferenceFailed,
    InvalidAudio,
    DownloadFailed,
    /// The track is longer than the one-pass engine will hold in memory.
    AudioTooLong,
}

impl fmt::Display for DiarizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::ModelUnavailable => "diarization model is unavailable",
            Self::ModelInvalid => "diarization model failed verification",
            Self::InferenceFailed => "diarization inference failed",
            Self::InvalidAudio => "diarization received invalid audio",
            Self::DownloadFailed => "diarization model download failed",
            Self::AudioTooLong => "diarization audio exceeds the one-pass ceiling",
        };
        formatter.write_str(text)
    }
}

impl std::error::Error for DiarizationError {}

async fn download_asset(
    model_directory: &Path,
    manifest: &'static DiarizationModelManifest,
) -> Result<(), DiarizationError> {
    let api = ApiBuilder::from_env()
        .build()
        .map_err(|_| DiarizationError::DownloadFailed)?;
    let repository = api.repo(Repo::with_revision(
        manifest.id.clone(),
        RepoType::Model,
        manifest.revision.clone(),
    ));
    let cached = repository
        .get(&manifest.filename)
        .await
        .map_err(|_| DiarizationError::DownloadFailed)?;
    install_verified_asset(&cached, model_directory, manifest)
}

/// Distinguishes two installs of one asset inside a process; the process id
/// distinguishes processes.
static INSTALL_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

fn install_verified_asset(
    cached: &Path,
    model_directory: &Path,
    manifest: &DiarizationModelManifest,
) -> Result<(), DiarizationError> {
    fs::create_dir_all(model_directory).map_err(|_| DiarizationError::DownloadFailed)?;
    let target = model_directory.join(&manifest.local_filename);
    // The staging name is unique per attempt. A fixed one lets two concurrent
    // installs of the same asset unlink and recreate each other's partial
    // file, after which each verifies bytes it did not write and both report a
    // download that succeeded as an invalid model.
    let temporary = model_directory.join(format!(
        "{}.{}.{}.partial",
        manifest.local_filename,
        std::process::id(),
        INSTALL_ATTEMPTS.fetch_add(1, Ordering::Relaxed)
    ));
    let staged = stage_verified_asset(cached, &temporary, manifest);
    if staged.is_err() {
        let _ = fs::remove_file(&temporary);
        return staged;
    }
    fs::rename(&temporary, &target).map_err(|_| DiarizationError::DownloadFailed)?;
    sync_parent(&target).map_err(|_| DiarizationError::DownloadFailed)
}

/// Copies the cached download to `temporary` and verifies it there. The caller
/// owns the staging path, so a failure here has one cleanup site.
fn stage_verified_asset(
    cached: &Path,
    temporary: &Path,
    manifest: &DiarizationModelManifest,
) -> Result<(), DiarizationError> {
    let mut source = File::open(cached).map_err(|_| DiarizationError::DownloadFailed)?;
    let mut destination = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(temporary)
        .map_err(|_| DiarizationError::DownloadFailed)?;
    io::copy(&mut source, &mut destination).map_err(|_| DiarizationError::DownloadFailed)?;
    destination
        .flush()
        .and_then(|()| destination.sync_all())
        .map_err(|_| DiarizationError::DownloadFailed)?;
    drop(destination);

    if !asset_is_verified(temporary, manifest) {
        return Err(DiarizationError::ModelInvalid);
    }
    Ok(())
}

fn verified_asset_path(
    model_directory: &Path,
    manifest: &DiarizationModelManifest,
) -> Option<PathBuf> {
    let path = model_directory.join(&manifest.local_filename);
    asset_is_verified(&path, manifest).then_some(path)
}

fn asset_is_verified(path: &Path, manifest: &DiarizationModelManifest) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if metadata.len() != manifest.size_bytes {
        return false;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = match file.read(&mut buffer) {
            Ok(count) => count,
            Err(_) => return false,
        };
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    format!("{:x}", digest.finalize()) == manifest.sha256
}

fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("diarization model has no parent"))?;
    File::open(parent)?.sync_all()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiarizedWindow {
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
    pub cluster: Option<u32>,
    pub assignment: SpeakerAssignmentKind,
}

#[derive(Debug)]
pub(crate) struct DiarizedWindowResult {
    pub(crate) window: DiarizedWindow,
    /// The fallback's one inference result, retained only for an unambiguous
    /// system-speaker window.
    pub(crate) embedding: Option<SpeakerEmbedding>,
}

/// A verified, L2-normalized WeSpeaker vector with the pinned 256-float width.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpeakerEmbedding {
    values: [f32; SPEAKER_EMBEDDING_DIMENSIONS],
}

impl SpeakerEmbedding {
    /// Reconstruct an embedding from durable bytes only when its shape and
    /// normalization still meet the matching invariant.
    pub(crate) fn from_normalized_slice(values: &[f32]) -> Option<Self> {
        let values: [f32; SPEAKER_EMBEDDING_DIMENSIONS] = values.try_into().ok()?;
        Self::from_normalized_array(values)
    }

    pub(crate) fn as_slice(&self) -> &[f32] {
        &self.values
    }

    fn from_normalized_array(values: [f32; SPEAKER_EMBEDDING_DIMENSIONS]) -> Option<Self> {
        is_normalized(&values).then_some(Self { values })
    }
}

fn is_normalized(values: &[f32]) -> bool {
    let norm_squared = values.iter().map(|value| value * value).sum::<f32>();
    norm_squared.is_finite()
        && (norm_squared - 1.0).abs() <= NORMALIZED_EMBEDDING_NORM_SQUARED_TOLERANCE
}

/// The reusable local WeSpeaker ONNX path. It has no clustering policy: callers
/// receive one normalized vector for each inference.
pub(crate) struct SpeakerEmbeddingSession {
    session: Session,
    fft: Arc<dyn Fft<f32>>,
    mel_filters: Vec<Vec<(usize, f32)>>,
}

impl SpeakerEmbeddingSession {
    pub(crate) fn open(path: &Path) -> Result<Self, DiarizationError> {
        let manifest = wespeaker_manifest();
        if !asset_is_verified(path, manifest)
            || manifest.sample_rate_hz != 16_000
            || manifest.feature_bins != MEL_BINS
            || manifest.embedding_dimensions != SPEAKER_EMBEDDING_DIMENSIONS
        {
            return Err(DiarizationError::ModelInvalid);
        }
        let session = Session::builder()
            .map_err(|_| DiarizationError::InferenceFailed)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|_| DiarizationError::InferenceFailed)?
            .commit_from_file(path)
            .map_err(|_| DiarizationError::InferenceFailed)?;
        let mut planner = FftPlanner::new();
        Ok(Self {
            session,
            fft: planner.plan_fft_forward(FFT_SIZE),
            mel_filters: mel_filters()?,
        })
    }

    pub(crate) fn embed(&mut self, samples: &[f32]) -> Result<SpeakerEmbedding, DiarizationError> {
        let features = log_mel_features(samples, self.fft.as_ref(), &self.mel_filters)?;
        let frame_count = features.len() / MEL_BINS;
        let feature_array = Array3::from_shape_vec((1, frame_count, MEL_BINS), features)
            .map_err(|_| DiarizationError::InferenceFailed)?
            .into_dyn();
        let tensor = TensorRef::from_array_view(feature_array.view())
            .map_err(|_| DiarizationError::InferenceFailed)?;
        let outputs = self
            .session
            .run(inputs!["input_features" => tensor])
            .map_err(|_| DiarizationError::InferenceFailed)?;
        let embedding = outputs[0]
            .try_extract_array::<f32>()
            .map_err(|_| DiarizationError::InferenceFailed)?;
        if embedding.len() != SPEAKER_EMBEDDING_DIMENSIONS {
            return Err(DiarizationError::InferenceFailed);
        }
        let mut values = [0.0_f32; SPEAKER_EMBEDDING_DIMENSIONS];
        for (destination, source) in values.iter_mut().zip(embedding.iter()) {
            *destination = *source;
        }
        normalize(&mut values).ok_or(DiarizationError::InferenceFailed)?;
        SpeakerEmbedding::from_normalized_array(values).ok_or(DiarizationError::InferenceFailed)
    }
}

pub struct OnnxDiarizationSession {
    embedder: SpeakerEmbeddingSession,
    clusters: Vec<SpeakerCluster>,
}

struct SpeakerCluster {
    centroid: SpeakerEmbedding,
    count: u32,
}

impl OnnxDiarizationSession {
    fn open(path: &Path) -> Result<Self, DiarizationError> {
        Ok(Self {
            embedder: SpeakerEmbeddingSession::open(path)?,
            clusters: Vec::new(),
        })
    }

    fn diarize_window(
        &mut self,
        samples: &[f32],
        start_offset_ns: u64,
        end_offset_ns: u64,
    ) -> Result<DiarizedWindowResult, DiarizationError> {
        if samples.len() < MIN_WINDOW_SAMPLES || start_offset_ns >= end_offset_ns {
            return Err(DiarizationError::InvalidAudio);
        }
        let (embedder, clusters) = (&mut self.embedder, &mut self.clusters);
        diarize_fallback_window(
            clusters,
            || embedder.embed(samples),
            start_offset_ns,
            end_offset_ns,
        )
    }
}

fn diarize_fallback_window(
    clusters: &mut Vec<SpeakerCluster>,
    embed: impl FnOnce() -> Result<SpeakerEmbedding, DiarizationError>,
    start_offset_ns: u64,
    end_offset_ns: u64,
) -> Result<DiarizedWindowResult, DiarizationError> {
    let embedding = embed()?;
    let mut best = None;
    let mut runner_up = None;
    for (index, cluster) in clusters.iter().enumerate() {
        retain_top_two(
            &mut best,
            &mut runner_up,
            (index, cosine_similarity(&embedding, &cluster.centroid)),
        );
    }

    if let (Some((_, best_score)), Some((_, runner_up_score))) = (best, runner_up) {
        if best_score >= SPEAKER_MATCH_THRESHOLD
            && runner_up_score >= SPEAKER_MATCH_THRESHOLD
            && best_score - runner_up_score <= OVERLAP_MARGIN
        {
            return Ok(DiarizedWindowResult {
                window: DiarizedWindow {
                    start_offset_ns,
                    end_offset_ns,
                    cluster: None,
                    assignment: SpeakerAssignmentKind::Overlap,
                },
                embedding: None,
            });
        }
    }

    let cluster_index = best
        .filter(|(_, score)| *score >= SPEAKER_MATCH_THRESHOLD)
        .map(|(index, _)| index)
        .unwrap_or_else(|| {
            clusters.push(SpeakerCluster {
                centroid: embedding.clone(),
                count: 0,
            });
            clusters.len() - 1
        });
    let cluster = clusters
        .get_mut(cluster_index)
        .ok_or(DiarizationError::InferenceFailed)?;
    update_centroid(cluster, &embedding)?;
    Ok(DiarizedWindowResult {
        window: DiarizedWindow {
            start_offset_ns,
            end_offset_ns,
            cluster: Some(
                u32::try_from(cluster_index).map_err(|_| DiarizationError::InferenceFailed)?,
            ),
            assignment: SpeakerAssignmentKind::SystemSpeaker,
        },
        embedding: Some(embedding),
    })
}

fn retain_top_two(
    best: &mut Option<(usize, f32)>,
    runner_up: &mut Option<(usize, f32)>,
    candidate: (usize, f32),
) {
    if best
        .map(|(_, score)| candidate.1.total_cmp(&score).is_gt())
        .unwrap_or(true)
    {
        *runner_up = *best;
        *best = Some(candidate);
        return;
    }
    if runner_up
        .map(|(_, score)| candidate.1.total_cmp(&score).is_gt())
        .unwrap_or(true)
    {
        *runner_up = Some(candidate);
    }
}

/// One speaker's continuous activity on the track timeline, in nanoseconds
/// from the track origin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SpeakerTurn {
    start_ns: u64,
    end_ns: u64,
    speaker: u32,
}

/// An exact singleton portion of the primed Sortformer timeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DiarizationSpeakerSpan {
    pub(crate) cluster: u32,
    pub(crate) start_offset_ns: u64,
    pub(crate) end_offset_ns: u64,
}

/// NVIDIA Streaming Sortformer, end to end: a FastConformer encoder and an
/// 18-layer Transformer head emit per-frame activity for up to four speakers,
/// ordered by arrival, resolved by the model's own Arrival-Order Speaker Cache.
///
/// Sortformer is exposed by transcribe-cpp in the *run* slot only — there is no
/// incremental stream variant — and arrival-order ids are only consistent
/// inside a single run. So the track is scored in one pass ([`prime`]), and
/// [`diarize_window`] then reads windows off the resulting timeline. Scoring
/// per window instead would restart the speaker cache each time and renumber
/// every speaker, which is exactly the cross-window identity the caller needs.
pub struct SortformerDiarizationSession {
    session: SortformerRunSession,
    buffer: PrimingBuffer,
    timeline: Vec<SpeakerTurn>,
    exclusive_spans: Vec<DiarizationSpeakerSpan>,
    primed: bool,
}

/// Track audio staged for a one-pass diarizer, gap-filled so a sample index
/// maps linearly to a track offset. Separate from the session so the offset
/// arithmetic is exercised without loading a 139 MB checkpoint.
#[derive(Default)]
struct PrimingBuffer {
    pcm: Vec<f32>,
    /// Track offset of `pcm[0]`.
    base_offset_ns: Option<u64>,
    /// Track offset just past the last sample staged.
    filled_to_ns: u64,
}

impl PrimingBuffer {
    /// Stage a run of audio at its true offset. A gap since the previous run is
    /// zero-filled, so the model sees the track's real silences and returns
    /// offsets needing no correction; audio already covered is skipped.
    fn push(&mut self, samples: &[f32], start_offset_ns: u64) -> Result<(), DiarizationError> {
        let base = *self.base_offset_ns.get_or_insert(start_offset_ns);
        if self.pcm.is_empty() {
            self.filled_to_ns = base;
        }
        let skip_samples = ns_to_samples(self.filled_to_ns.saturating_sub(start_offset_ns));
        if skip_samples >= samples.len() {
            return Ok(());
        }
        let gap_samples = ns_to_samples(start_offset_ns.saturating_sub(self.filled_to_ns));
        let appended = samples.len() - skip_samples;
        if self.pcm.len() + gap_samples + appended > SORTFORMER_MAX_PRIME_SAMPLES {
            return Err(DiarizationError::AudioTooLong);
        }
        self.pcm.resize(self.pcm.len() + gap_samples, 0.0);
        self.pcm.extend_from_slice(&samples[skip_samples..]);
        self.filled_to_ns = base.saturating_add(samples_to_ns(self.pcm.len()));
        Ok(())
    }

    fn base_offset_ns(&self) -> u64 {
        self.base_offset_ns.unwrap_or(0)
    }

    /// Hand over the staged audio, releasing it from the buffer.
    fn take(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.pcm)
    }
}

impl SortformerDiarizationSession {
    fn open(path: &Path) -> Result<Self, DiarizationError> {
        let manifest = model_manifest();
        if !asset_is_verified(path, manifest) || manifest.sample_rate_hz != 16_000 {
            return Err(DiarizationError::ModelInvalid);
        }
        let model = SortformerModel::load(path).map_err(|_| DiarizationError::ModelInvalid)?;
        let session = model
            .session()
            .map_err(|_| DiarizationError::InferenceFailed)?;
        Ok(Self {
            session,
            buffer: PrimingBuffer::default(),
            timeline: Vec::new(),
            exclusive_spans: Vec::new(),
            primed: false,
        })
    }

    /// Append a run of track audio at its true offset.
    pub fn push_priming_audio(
        &mut self,
        samples: &[f32],
        start_offset_ns: u64,
    ) -> Result<(), DiarizationError> {
        self.buffer.push(samples, start_offset_ns)
    }

    /// Score the accumulated track in one pass and keep the speaker timeline.
    /// The audio buffer is released here: windows are answered from the
    /// timeline, which is three integers per turn.
    pub fn prime(&mut self) -> Result<(), DiarizationError> {
        let base = self.buffer.base_offset_ns();
        let pcm = self.buffer.take();
        if pcm.len() < MIN_WINDOW_SAMPLES {
            self.primed = true;
            return Ok(());
        }
        let options = RunOptions {
            diarize: Diarize::On,
            // Post-hoc diarization of a finished track: the crate documents
            // very-high-latency as the offline-file operating point, and it is
            // the point the published DER for this checkpoint was measured at.
            family: Some(RunExtension::Sortformer(SortformerStreamOptions {
                preset: Some(SortformerPreset::VeryHighLatency),
            })),
            ..RunOptions::default()
        };
        let transcript = self
            .session
            .run(&pcm, &options)
            .map_err(|_| DiarizationError::InferenceFailed)?;
        self.timeline = timeline_from_segments(&transcript.speaker_segments, base);
        self.exclusive_spans = exclusive_spans_from_timeline(&self.timeline);
        self.primed = true;
        Ok(())
    }

    pub(crate) fn exclusive_spans(&self) -> &[DiarizationSpeakerSpan] {
        &self.exclusive_spans
    }

    /// Resolve one window against the primed timeline. `samples` is unused:
    /// Sortformer already scored this audio in [`prime`], and re-scoring the
    /// window in isolation would renumber its speakers.
    pub fn diarize_window(
        &mut self,
        samples: &[f32],
        start_offset_ns: u64,
        end_offset_ns: u64,
    ) -> Result<DiarizedWindow, DiarizationError> {
        if samples.len() < MIN_WINDOW_SAMPLES || start_offset_ns >= end_offset_ns {
            return Err(DiarizationError::InvalidAudio);
        }
        if !self.primed {
            return Err(DiarizationError::InferenceFailed);
        }
        Ok(resolve_window(
            &self.timeline,
            start_offset_ns,
            end_offset_ns,
        ))
    }
}

/// Reduce the model's speaker rows to a sorted, per-speaker timeline. Rows
/// without usable timing (`t0 == t1 == 0`, or an unattributed speaker) carry no
/// who-spoke-when information and are dropped rather than guessed at.
fn timeline_from_segments(segments: &[SpeakerSegment], base_offset_ns: u64) -> Vec<SpeakerTurn> {
    let mut turns = segments
        .iter()
        .filter(|segment| segment.speaker_id > 0 && segment.t1_ms > segment.t0_ms)
        .filter(|segment| segment.p.is_nan() || segment.p >= SORTFORMER_MIN_ACTIVITY)
        .filter_map(|segment| {
            let start = u64::try_from(segment.t0_ms).ok()?;
            let end = u64::try_from(segment.t1_ms).ok()?;
            let speaker = u32::try_from(segment.speaker_id - 1).ok()?;
            Some(SpeakerTurn {
                start_ns: base_offset_ns.saturating_add(start.saturating_mul(1_000_000)),
                end_ns: base_offset_ns.saturating_add(end.saturating_mul(1_000_000)),
                speaker,
            })
        })
        .collect::<Vec<_>>();
    turns.sort_unstable_by_key(|turn| (turn.start_ns, turn.speaker));
    turns
}

/// Sweep every start/end edge and retain only intervals with one active
/// arrival-order cluster. Adjacent intervals for that cluster remain one span.
fn exclusive_spans_from_timeline(timeline: &[SpeakerTurn]) -> Vec<DiarizationSpeakerSpan> {
    let mut edges = Vec::with_capacity(timeline.len().saturating_mul(2));
    for turn in timeline.iter().filter(|turn| turn.end_ns > turn.start_ns) {
        edges.push((turn.start_ns, turn.speaker, true));
        edges.push((turn.end_ns, turn.speaker, false));
    }
    edges.sort_unstable_by_key(|edge| edge.0);

    let mut spans = Vec::new();
    let mut active = BTreeMap::<u32, u32>::new();
    let mut previous = None;
    let mut index = 0;
    while index < edges.len() {
        let boundary = edges[index].0;
        if active.len() == 1 {
            if let (Some(start_offset_ns), Some((&cluster, _))) = (previous, active.iter().next()) {
                append_exclusive_span(&mut spans, cluster, start_offset_ns, boundary);
            }
        }

        while index < edges.len() && edges[index].0 == boundary {
            let (_, cluster, is_start) = edges[index];
            if is_start {
                let count = active.entry(cluster).or_insert(0);
                *count = count.saturating_add(1);
            } else {
                let remove = if let Some(count) = active.get_mut(&cluster) {
                    *count = count.saturating_sub(1);
                    *count == 0
                } else {
                    false
                };
                if remove {
                    active.remove(&cluster);
                }
            }
            index += 1;
        }
        previous = Some(boundary);
    }
    spans
}

fn append_exclusive_span(
    spans: &mut Vec<DiarizationSpeakerSpan>,
    cluster: u32,
    start_offset_ns: u64,
    end_offset_ns: u64,
) {
    if start_offset_ns >= end_offset_ns {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.cluster == cluster && last.end_offset_ns == start_offset_ns {
            last.end_offset_ns = end_offset_ns;
            return;
        }
    }
    spans.push(DiarizationSpeakerSpan {
        cluster,
        start_offset_ns,
        end_offset_ns,
    });
}

/// Attribute `[start, end)` from the timeline. Two speakers audible at once for
/// a real share of the window is overlap; two speakers taking turns inside one
/// window is not, and the longer-active one owns it.
fn resolve_window(
    timeline: &[SpeakerTurn],
    start_offset_ns: u64,
    end_offset_ns: u64,
) -> DiarizedWindow {
    let mut active: Vec<(u32, u64)> = Vec::new();
    for turn in timeline {
        if turn.end_ns <= start_offset_ns {
            continue;
        }
        if turn.start_ns >= end_offset_ns {
            break;
        }
        let overlap = turn.end_ns.min(end_offset_ns) - turn.start_ns.max(start_offset_ns);
        match active
            .iter_mut()
            .find(|(speaker, _)| *speaker == turn.speaker)
        {
            Some((_, total)) => *total = total.saturating_add(overlap),
            None => active.push((turn.speaker, overlap)),
        }
    }

    let window_ns = end_offset_ns - start_offset_ns;
    let simultaneous_ns = simultaneous_span_ns(timeline, start_offset_ns, end_offset_ns);
    if window_ns > 0 && (simultaneous_ns as f64) / (window_ns as f64) >= SORTFORMER_OVERLAP_SHARE {
        return DiarizedWindow {
            start_offset_ns,
            end_offset_ns,
            cluster: None,
            assignment: SpeakerAssignmentKind::Overlap,
        };
    }

    match active.iter().max_by_key(|(_, total)| *total) {
        Some((speaker, _)) => DiarizedWindow {
            start_offset_ns,
            end_offset_ns,
            cluster: Some(*speaker),
            assignment: SpeakerAssignmentKind::SystemSpeaker,
        },
        // The VAD heard speech here and the diarizer attributed none of it.
        // Saying "unknown" is the honest answer; inventing the last speaker
        // would put words in someone's mouth.
        None => DiarizedWindow {
            start_offset_ns,
            end_offset_ns,
            cluster: None,
            assignment: SpeakerAssignmentKind::Unknown,
        },
    }
}

/// Total time inside the window where two or more speakers are active at once,
/// counted by sweeping turn boundaries so concurrent runs are measured, not
/// approximated by summing per-speaker durations.
fn simultaneous_span_ns(timeline: &[SpeakerTurn], start_offset_ns: u64, end_offset_ns: u64) -> u64 {
    let mut edges: Vec<u64> = Vec::new();
    for turn in timeline {
        if turn.end_ns <= start_offset_ns || turn.start_ns >= end_offset_ns {
            continue;
        }
        edges.push(turn.start_ns.max(start_offset_ns));
        edges.push(turn.end_ns.min(end_offset_ns));
    }
    edges.sort_unstable();
    edges.dedup();

    let mut total = 0_u64;
    for pair in edges.windows(2) {
        let (slice_start, slice_end) = (pair[0], pair[1]);
        if slice_end <= slice_start {
            continue;
        }
        let midpoint = slice_start + (slice_end - slice_start) / 2;
        let concurrent = timeline
            .iter()
            .filter(|turn| turn.start_ns <= midpoint && midpoint < turn.end_ns)
            .map(|turn| turn.speaker)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        if concurrent > 1 {
            total = total.saturating_add(slice_end - slice_start);
        }
    }
    total
}

fn ns_to_samples(ns: u64) -> usize {
    usize::try_from(ns / NS_PER_SAMPLE).unwrap_or(usize::MAX)
}

fn samples_to_ns(samples: usize) -> u64 {
    u64::try_from(samples)
        .unwrap_or(u64::MAX)
        .saturating_mul(NS_PER_SAMPLE)
}

/// The engine actually backing this generation. Both arms answer the same
/// window question, so everything downstream of [`diarize_window`] is identical
/// whichever one ran.
pub enum MeetingDiarizationSession {
    Sortformer(SortformerDiarizationSession),
    WeSpeaker(OnnxDiarizationSession),
}

impl MeetingDiarizationSession {
    pub const fn engine(&self) -> DiarizationEngineKind {
        match self {
            Self::Sortformer(_) => DiarizationEngineKind::Sortformer,
            Self::WeSpeaker(_) => DiarizationEngineKind::WeSpeaker,
        }
    }

    /// Whether this engine needs the track fed to it before windows resolve.
    pub const fn needs_priming(&self) -> bool {
        matches!(self, Self::Sortformer(_))
    }

    pub fn push_priming_audio(
        &mut self,
        samples: &[f32],
        start_offset_ns: u64,
    ) -> Result<(), DiarizationError> {
        match self {
            Self::Sortformer(session) => session.push_priming_audio(samples, start_offset_ns),
            Self::WeSpeaker(_) => Ok(()),
        }
    }

    pub fn prime(&mut self) -> Result<(), DiarizationError> {
        match self {
            Self::Sortformer(session) => session.prime(),
            Self::WeSpeaker(_) => Ok(()),
        }
    }

    /// Exact singleton activity exists only after a Sortformer prime pass.
    pub(crate) fn exclusive_spans(&self) -> &[DiarizationSpeakerSpan] {
        match self {
            Self::Sortformer(session) => session.exclusive_spans(),
            Self::WeSpeaker(_) => &[],
        }
    }

    pub(crate) fn diarize_window(
        &mut self,
        samples: &[f32],
        start_offset_ns: u64,
        end_offset_ns: u64,
    ) -> Result<DiarizedWindowResult, DiarizationError> {
        match self {
            Self::Sortformer(session) => session
                .diarize_window(samples, start_offset_ns, end_offset_ns)
                .map(|window| DiarizedWindowResult {
                    window,
                    embedding: None,
                }),
            Self::WeSpeaker(session) => {
                session.diarize_window(samples, start_offset_ns, end_offset_ns)
            }
        }
    }
}

fn update_centroid(
    cluster: &mut SpeakerCluster,
    embedding: &SpeakerEmbedding,
) -> Result<(), DiarizationError> {
    let previous = cluster
        .count
        .to_f32()
        .ok_or(DiarizationError::InferenceFailed)?;
    let next = previous + 1.0;
    for (centroid, value) in cluster.centroid.values.iter_mut().zip(embedding.as_slice()) {
        *centroid = (*centroid * previous + *value) / next;
    }
    cluster.count = cluster.count.saturating_add(1);
    normalize(&mut cluster.centroid.values).ok_or(DiarizationError::InferenceFailed)
}

fn cosine_similarity(left: &SpeakerEmbedding, right: &SpeakerEmbedding) -> f32 {
    left.as_slice()
        .iter()
        .zip(right.as_slice())
        .map(|(a, b)| a * b)
        .sum()
}

/// Return the id of the unique profile above the fixed raw-cosine threshold
/// and runner-up margin.
///
/// Model compatibility belongs to the caller, which selects the stored profiles
/// that were computed by the running model and hands over an id for each. That
/// leaves one owner for eligibility instead of a filter here that a caller has
/// already applied, and the id comes back rather than a position, so nothing
/// has to keep a parallel vector aligned.
pub(crate) fn match_speaker_profile<'a, Id>(
    candidate: &SpeakerEmbedding,
    profiles: impl IntoIterator<Item = (Id, &'a SpeakerEmbedding)>,
) -> Option<Id> {
    let mut best: Option<(Id, f32)> = None;
    let mut runner_up_score = f32::NEG_INFINITY;
    for (id, profile) in profiles {
        let score = cosine_similarity(candidate, profile);
        match &best {
            Some((_, best_score)) if score <= *best_score => {
                runner_up_score = runner_up_score.max(score);
            }
            Some((_, best_score)) => {
                runner_up_score = runner_up_score.max(*best_score);
                best = Some((id, score));
            }
            None => best = Some((id, score)),
        }
    }

    let (id, best_score) = best?;
    if best_score < PROFILE_MATCH_THRESHOLD {
        return None;
    }
    if best_score - runner_up_score < PROFILE_MATCH_MARGIN {
        return None;
    }
    Some(id)
}

fn normalize(values: &mut [f32]) -> Option<()> {
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    (norm.is_finite() && norm > f32::EPSILON).then(|| {
        for value in values {
            *value /= norm;
        }
    })
}

// The pinned model's preprocessor uses Kaldi fbank followed by a temporal mean
// for each mel band, not one scalar mean over the entire feature matrix.
// https://github.com/wenet-e2e/wespeaker/blob/45941e7cba2c3ea99e232d02bedf617fc71b0dad/wespeaker/bin/infer_onnx.py
fn log_mel_features(
    samples: &[f32],
    fft: &dyn Fft<f32>,
    filters: &[Vec<(usize, f32)>],
) -> Result<Vec<f32>, DiarizationError> {
    if samples.len() < FRAME_SAMPLES {
        return Err(DiarizationError::InvalidAudio);
    }
    let frame_count = 1 + (samples.len() - FRAME_SAMPLES) / FRAME_HOP_SAMPLES;
    let frame_samples = FRAME_SAMPLES
        .to_f32()
        .ok_or(DiarizationError::InferenceFailed)?;
    let mut features = Vec::with_capacity(frame_count * MEL_BINS);
    let window = hamming_window()?;
    let mut spectrum = [Complex32::default(); FFT_SIZE];
    let mut fft_scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
    let mut power = [0.0_f32; FFT_SIZE / 2];
    for frame_index in 0..frame_count {
        spectrum.fill(Complex32::default());
        let start = frame_index * FRAME_HOP_SAMPLES;
        let frame = &samples[start..start + FRAME_SAMPLES];
        let mean = frame.iter().sum::<f32>() / frame_samples;
        // Kaldi removes each frame's DC offset, preemphasizes with 0.97 and
        // replicates its first sample at the left edge before windowing.
        // The model author scales normalized audio back to PCM16 magnitude.
        let mut previous = (frame[0] - mean) * 32_768.0;
        for ((sample, windowed), coefficient) in
            frame.iter().zip(spectrum.iter_mut()).zip(window.iter())
        {
            let current = (*sample - mean) * 32_768.0;
            *windowed = Complex32::new((current - 0.97 * previous) * coefficient, 0.0);
            previous = current;
        }
        fft.process_with_scratch(&mut spectrum, &mut fft_scratch);
        for (energy, value) in power.iter_mut().zip(spectrum.iter()) {
            *energy = value.norm_sqr();
        }
        for filter in filters {
            let energy = filter
                .iter()
                .map(|(bin, weight)| power[*bin] * *weight)
                .sum::<f32>();
            features.push(energy.max(f32::EPSILON).ln());
        }
    }
    let mut means = [0.0_f32; MEL_BINS];
    for frame in features.chunks_exact(MEL_BINS) {
        for (mean, value) in means.iter_mut().zip(frame) {
            *mean += *value;
        }
    }
    let frame_count = frame_count
        .to_f32()
        .ok_or(DiarizationError::InferenceFailed)?;
    for mean in &mut means {
        *mean /= frame_count;
    }
    for frame in features.chunks_exact_mut(MEL_BINS) {
        for (value, mean) in frame.iter_mut().zip(means.iter()) {
            *value -= mean;
        }
    }
    Ok(features)
}

fn hamming_window() -> Result<[f32; FRAME_SAMPLES], DiarizationError> {
    let denominator = (FRAME_SAMPLES - 1)
        .to_f32()
        .ok_or(DiarizationError::InferenceFailed)?;
    let mut window = [0.0_f32; FRAME_SAMPLES];
    for (index, coefficient) in window.iter_mut().enumerate() {
        let index = index.to_f32().ok_or(DiarizationError::InferenceFailed)?;
        *coefficient = 0.54 - 0.46 * (2.0 * std::f32::consts::PI * index / denominator).cos();
    }
    Ok(window)
}

fn mel_filters() -> Result<Vec<Vec<(usize, f32)>>, DiarizationError> {
    let lower_mel = hz_to_mel(20.0);
    let upper_mel = hz_to_mel(8_000.0);
    let denominator = (MEL_BINS + 1)
        .to_f32()
        .ok_or(DiarizationError::InferenceFailed)?;
    let mel_step = (upper_mel - lower_mel) / denominator;
    let bin_width = SAMPLE_RATE_HZ
        .to_f32()
        .ok_or(DiarizationError::InferenceFailed)?
        / FFT_SIZE.to_f32().ok_or(DiarizationError::InferenceFailed)?;
    let mut bin_mels = [0.0_f32; FFT_SIZE / 2];
    for (index, mel) in bin_mels.iter_mut().enumerate() {
        let index = index.to_f32().ok_or(DiarizationError::InferenceFailed)?;
        *mel = hz_to_mel(index * bin_width);
    }
    (0..MEL_BINS)
        .map(|index| {
            let index = index.to_f32().ok_or(DiarizationError::InferenceFailed)?;
            let lower = lower_mel + index * mel_step;
            let center = lower_mel + (index + 1.0) * mel_step;
            let upper = lower_mel + (index + 2.0) * mel_step;
            // Kaldi evaluates the triangles in mel space at each actual FFT
            // frequency; flooring the triangle edges to integer bins changes
            // the filters, especially the narrow low-frequency bands.
            let filter = bin_mels
                .iter()
                .enumerate()
                .filter_map(|(bin, mel)| {
                    let weight =
                        ((mel - lower) / (center - lower)).min((upper - mel) / (upper - center));
                    (weight > 0.0).then_some((bin, weight))
                })
                .collect();
            Ok(filter)
        })
        .collect()
}

fn hz_to_mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_model_stays_explicitly_unavailable_without_download(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let diarizer = MeetingDiarizer::without_download();
        assert_eq!(
            diarizer.availability(directory.path()),
            DiarizationModelAvailability::Unavailable
        );
        assert_eq!(
            diarizer.prepare(directory.path()),
            Err(DiarizationError::ModelUnavailable)
        );
        Ok(())
    }

    #[test]
    fn mel_features_are_bounded_by_window_size() -> Result<(), DiarizationError> {
        let mut planner = FftPlanner::new();
        let filters = mel_filters()?;
        let audio = vec![0.1_f32; SAMPLE_RATE_HZ * 2];
        let features = log_mel_features(
            &audio,
            planner.plan_fft_forward(FFT_SIZE).as_ref(),
            &filters,
        )?;
        assert_eq!(features.len() % MEL_BINS, 0);
        assert!(features.len() <= (SAMPLE_RATE_HZ * 2 / FRAME_HOP_SAMPLES + 1) * MEL_BINS);
        Ok(())
    }

    #[test]
    fn wespeaker_features_match_the_published_kaldi_contract() -> Result<(), DiarizationError> {
        let audio: Vec<f32> = (0..720)
            .map(|index| {
                let sample = i16::try_from((index * 37 + 19) % 257)
                    .expect("the deterministic sample is in i16 range")
                    - 128;
                f32::from(sample) / 512.0 + if index < 360 { 0.25 } else { -0.125 }
            })
            .collect();
        let mut planner = FftPlanner::new();
        let features = log_mel_features(
            &audio,
            planner.plan_fft_forward(FFT_SIZE).as_ref(),
            &mel_filters()?,
        )?;
        assert_eq!(features.len(), 3 * MEL_BINS);
        // Independent scalar evaluation of the published Kaldi fbank equations
        // and WeSpeaker CMN, using a direct DFT rather than this FFT pipeline.
        // The tolerance covers f32 FFT arithmetic versus the f64 reference.
        let reference = [
            (0, 0, -0.677_605_6_f32),
            (0, 1, 0.112_294_41),
            (0, 7, -1.635_925),
            (0, 20, -0.734_759_5),
            (0, 40, -0.431_189_33),
            (0, 60, 0.047_798_596),
            (0, 79, -0.115_032_75),
            (1, 0, 2.034_684_4),
            (1, 1, 3.604_758_7),
            (1, 7, 1.763_793_1),
            (1, 20, 1.615_619_1),
            (1, 40, 0.896_348_3),
            (1, 60, -0.151_868_21),
            (1, 79, 0.200_659_59),
            (2, 0, -1.357_078_7),
            (2, 1, -3.717_053),
            (2, 7, -0.127_868_18),
            (2, 20, -0.880_859_55),
            (2, 40, -0.465_159),
            (2, 60, 0.104_069_61),
            (2, 79, -0.085_626_85),
        ];
        let max_error = reference
            .iter()
            .map(|(frame, band, expected)| (features[frame * MEL_BINS + band] - expected).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_error < 0.002,
            "Kaldi reference maximum absolute error: {max_error}"
        );
        Ok(())
    }

    #[test]
    fn wespeaker_cmn_centers_each_band_over_time() -> Result<(), DiarizationError> {
        let audio: Vec<f32> = (0..720)
            .map(|index| {
                let sample = i16::try_from((index * 37 + 19) % 257)
                    .expect("the deterministic sample is in i16 range")
                    - 128;
                f32::from(sample) / 512.0
            })
            .collect();
        let mut planner = FftPlanner::new();
        let features = log_mel_features(
            &audio,
            planner.plan_fft_forward(FFT_SIZE).as_ref(),
            &mel_filters()?,
        )?;
        assert_eq!(features.len(), 3 * MEL_BINS);
        let largest_band_mean = (0..MEL_BINS)
            .map(|band| {
                features
                    .chunks_exact(MEL_BINS)
                    .map(|frame| frame[band])
                    .sum::<f32>()
                    .abs()
                    / 3.0
            })
            .fold(0.0_f32, f32::max);
        assert!(
            largest_band_mean < 0.000_01,
            "each mel band must be centered separately: {largest_band_mean}"
        );
        Ok(())
    }

    #[test]
    fn wespeaker_ensure_is_explicit_only() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let diarizer = MeetingDiarizer::without_download();

        assert_eq!(
            diarizer.prepare(directory.path()),
            Err(DiarizationError::ModelUnavailable)
        );
        assert_eq!(
            diarizer.wespeaker_ensure_requests.load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            tauri::async_runtime::block_on(diarizer.ensure_wespeaker(directory.path())),
            Err(DiarizationError::ModelUnavailable)
        );
        assert_eq!(
            diarizer.wespeaker_ensure_requests.load(Ordering::Relaxed),
            1
        );
        Ok(())
    }

    fn turn(speaker: u32, start_ms: u64, end_ms: u64) -> SpeakerTurn {
        SpeakerTurn {
            start_ns: start_ms * 1_000_000,
            end_ns: end_ms * 1_000_000,
            speaker,
        }
    }

    fn span(cluster: u32, start_ms: u64, end_ms: u64) -> DiarizationSpeakerSpan {
        DiarizationSpeakerSpan {
            cluster,
            start_offset_ns: start_ms * 1_000_000,
            end_offset_ns: end_ms * 1_000_000,
        }
    }

    fn embedding_with_cosine(cosine: f32) -> SpeakerEmbedding {
        let mut values = [0.0_f32; SPEAKER_EMBEDDING_DIMENSIONS];
        values[0] = cosine;
        values[1] = (1.0 - cosine * cosine).sqrt();
        normalize(&mut values).expect("test vector has a finite norm");
        SpeakerEmbedding::from_normalized_array(values).expect("test vector is normalized")
    }

    fn segment(speaker_id: i32, t0_ms: i64, t1_ms: i64) -> SpeakerSegment {
        SpeakerSegment {
            t0_ms,
            t1_ms,
            speaker_id,
            p: 0.9,
        }
    }

    #[test]
    fn exclusive_spans_drop_every_overlap_and_coalesce_one_speaker() {
        let spans = exclusive_spans_from_timeline(&[
            turn(0, 0, 300),
            turn(1, 100, 200),
            turn(0, 300, 400),
            turn(2, 450, 500),
        ]);

        assert_eq!(
            spans,
            vec![span(0, 0, 100), span(0, 200, 400), span(2, 450, 500)]
        );
    }

    #[test]
    fn profile_match_requires_the_raw_cosine_threshold() {
        let candidate = embedding_with_cosine(1.0);
        let at_threshold = embedding_with_cosine(0.80);
        let below_threshold = embedding_with_cosine(0.79);

        assert_eq!(
            match_speaker_profile(&candidate, [(0, &at_threshold)]),
            Some(0)
        );
        assert_eq!(
            match_speaker_profile(&candidate, [(0, &below_threshold)]),
            None
        );
    }

    #[test]
    fn profile_match_requires_a_runner_up_margin() {
        let candidate = embedding_with_cosine(1.0);
        let best = embedding_with_cosine(0.85);
        let separated_runner_up = embedding_with_cosine(0.76);
        let close_runner_up = embedding_with_cosine(0.80);
        let tied_runner_up = embedding_with_cosine(0.85);

        assert_eq!(
            match_speaker_profile(&candidate, [(0, &best), (1, &separated_runner_up)],),
            Some(0)
        );
        assert_eq!(
            match_speaker_profile(&candidate, [(0, &best), (1, &close_runner_up)]),
            None
        );
        assert_eq!(
            match_speaker_profile(&candidate, [(0, &best), (1, &tied_runner_up)]),
            None
        );
    }

    #[test]
    fn profile_match_rejects_unnormalized_embeddings() {
        let mut values = [0.0_f32; SPEAKER_EMBEDDING_DIMENSIONS];
        values[0] = 2.0;

        assert!(SpeakerEmbedding::from_normalized_slice(&values).is_none());
    }

    #[test]
    fn fallback_result_carries_its_existing_embedding_once() -> Result<(), DiarizationError> {
        let embedding = embedding_with_cosine(1.0);
        let mut clusters = Vec::new();
        let mut embed_calls = 0;

        let result = diarize_fallback_window(
            &mut clusters,
            || {
                embed_calls += 1;
                Ok(embedding.clone())
            },
            0,
            NS_PER_SECOND,
        )?;

        assert_eq!(embed_calls, 1);
        assert_eq!(result.window.cluster, Some(0));
        assert_eq!(
            result.window.assignment,
            SpeakerAssignmentKind::SystemSpeaker
        );
        assert_eq!(result.embedding.as_ref(), Some(&embedding));
        Ok(())
    }

    #[test]
    fn fallback_overlap_drops_the_transient_embedding() -> Result<(), DiarizationError> {
        let embedding = embedding_with_cosine(1.0);
        let mut clusters = vec![
            SpeakerCluster {
                centroid: embedding_with_cosine(0.90),
                count: 1,
            },
            SpeakerCluster {
                centroid: embedding_with_cosine(0.88),
                count: 1,
            },
        ];

        let result = diarize_fallback_window(&mut clusters, || Ok(embedding), 0, NS_PER_SECOND)?;

        assert_eq!(result.window.assignment, SpeakerAssignmentKind::Overlap);
        assert_eq!(result.embedding, None);
        Ok(())
    }

    #[test]
    fn sortformer_manifest_pins_a_commit_and_a_digest() {
        let manifest = model_manifest();
        assert_eq!(manifest.sha256.len(), 64);
        assert_eq!(manifest.revision.len(), 40);
        assert!(manifest.filename.ends_with(".gguf"));
        assert_eq!(manifest.sample_rate_hz, 16_000);
        assert_eq!(manifest.max_speakers, 4);
        // The fallback stays a distinct, separately pinned asset.
        assert_ne!(manifest.id, wespeaker_manifest().id);
        assert_eq!(wespeaker_manifest().feature_bins, MEL_BINS);
    }

    #[test]
    fn wespeaker_compatibility_key_captures_the_pinned_pipeline() {
        let manifest = wespeaker_manifest();
        let key = wespeaker_embedding_model_key();
        // The revision string is what the store compares before it matches a
        // stored centroid, so it has to name the pipeline this build runs.
        let pipeline = format!(
            "kaldi-fbank-{MEL_BINS}-pcm16-hamming-{FRAME_SAMPLES}-hop-{FRAME_HOP_SAMPLES}\
             -fft-{FFT_SIZE}-per-bin-cmn-v2"
        );
        assert_eq!(key.feature_pipeline_revision, pipeline);
        assert_ne!(
            key.feature_pipeline_revision, "log-mel-80-hamming-400-hop-160-fft-512-mean-center-v1",
            "legacy vectors from the incompatible feature extractor must not match"
        );

        assert_eq!(key.model_id, manifest.id.as_str());
        assert_eq!(key.model_revision, manifest.revision.as_str());
        assert_eq!(key.model_sha256, manifest.sha256.as_str());
        assert_eq!(key.embedding_dimensions, SPEAKER_EMBEDDING_DIMENSIONS);
        assert_eq!(key.sample_rate_hz, 16_000);
        assert_eq!(key.feature_bins, MEL_BINS);
        assert_eq!(
            key.feature_pipeline_revision,
            WESPEAKER_FEATURE_PIPELINE_REVISION
        );
        assert_eq!(key.normalization, WESPEAKER_NORMALIZATION);
    }

    #[test]
    fn sequential_turns_in_one_window_pick_the_longer_speaker() {
        // Two speakers take turns inside one 2 s window and never overlap:
        // that is a turn boundary, not overlap, so the window has an owner.
        let timeline = vec![turn(0, 0, 600), turn(1, 700, 2_000)];
        let resolved = resolve_window(&timeline, 0, 2 * NS_PER_SECOND);
        assert_eq!(resolved.cluster, Some(1));
        assert_eq!(resolved.assignment, SpeakerAssignmentKind::SystemSpeaker);
    }

    #[test]
    fn simultaneous_speakers_mark_the_window_as_overlap() {
        // 800 ms of genuine co-speech inside a 2 s window is 40%, past the share.
        let timeline = vec![turn(0, 0, 1_200), turn(1, 400, 2_000)];
        let resolved = resolve_window(&timeline, 0, 2 * NS_PER_SECOND);
        assert_eq!(resolved.cluster, None);
        assert_eq!(resolved.assignment, SpeakerAssignmentKind::Overlap);
    }

    #[test]
    fn brief_co_speech_still_leaves_the_window_owned() {
        // 100 ms of co-speech in a 2 s window is 5%: a boundary blur, not a
        // two-people-talking window, so the dominant speaker keeps it.
        let timeline = vec![turn(0, 0, 1_100), turn(1, 1_000, 2_000)];
        let resolved = resolve_window(&timeline, 0, 2 * NS_PER_SECOND);
        assert_eq!(resolved.cluster, Some(0));
        assert_eq!(resolved.assignment, SpeakerAssignmentKind::SystemSpeaker);
    }

    #[test]
    fn a_window_the_diarizer_attributed_to_nobody_is_unknown() {
        let timeline = vec![turn(0, 5_000, 6_000)];
        let resolved = resolve_window(&timeline, 0, 2 * NS_PER_SECOND);
        assert_eq!(resolved.cluster, None);
        assert_eq!(resolved.assignment, SpeakerAssignmentKind::Unknown);
    }

    #[test]
    fn speaker_rows_become_zero_based_clusters_at_the_track_offset() {
        let base = 4 * NS_PER_SECOND;
        let timeline = timeline_from_segments(&[segment(1, 0, 500), segment(2, 500, 900)], base);
        assert_eq!(
            timeline,
            vec![
                SpeakerTurn {
                    start_ns: base,
                    end_ns: base + 500_000_000,
                    speaker: 0
                },
                SpeakerTurn {
                    start_ns: base + 500_000_000,
                    end_ns: base + 900_000_000,
                    speaker: 1
                },
            ]
        );
    }

    #[test]
    fn untimed_and_unattributed_speaker_rows_are_dropped() {
        // speaker_id 0 means "no attribution"; t0 == t1 means "no turn timing".
        // Neither carries who-spoke-when, so neither is guessed at.
        let timeline = timeline_from_segments(
            &[
                segment(0, 0, 500),
                segment(1, 700, 700),
                segment(2, 800, 900),
            ],
            0,
        );
        assert_eq!(timeline, vec![turn(1, 800, 900)]);
    }

    #[test]
    fn priming_gaps_are_zero_filled_so_offsets_stay_true() -> Result<(), DiarizationError> {
        let mut buffer = PrimingBuffer::default();
        buffer.push(&[0.5_f32; SAMPLE_RATE_HZ], 0)?;
        // Next record starts a second late: the silence must be materialized so
        // sample index still maps to track offset.
        buffer.push(&[0.25_f32; SAMPLE_RATE_HZ], 2 * NS_PER_SECOND)?;
        let pcm = buffer.take();
        assert_eq!(pcm.len(), 3 * SAMPLE_RATE_HZ);
        assert_eq!(pcm[0], 0.5);
        assert_eq!(pcm[SAMPLE_RATE_HZ + 100], 0.0);
        assert_eq!(pcm[2 * SAMPLE_RATE_HZ + 100], 0.25);
        Ok(())
    }

    #[test]
    fn priming_skips_audio_already_covered() -> Result<(), DiarizationError> {
        let mut buffer = PrimingBuffer::default();
        buffer.push(&[0.5_f32; SAMPLE_RATE_HZ], 0)?;
        // A record that re-sends the second half of what was already staged.
        buffer.push(&[0.25_f32; SAMPLE_RATE_HZ], NS_PER_SECOND / 2)?;
        let pcm = buffer.take();
        assert_eq!(pcm.len(), 3 * SAMPLE_RATE_HZ / 2);
        Ok(())
    }

    #[test]
    fn priming_refuses_audio_past_the_one_pass_ceiling() {
        let mut buffer = PrimingBuffer::default();
        let chunk = vec![0.1_f32; SAMPLE_RATE_HZ];
        let mut offset = 0_u64;
        // Jump straight past the ceiling with one gap rather than staging 2 h.
        assert_eq!(buffer.push(&chunk, offset), Ok(()));
        offset = samples_to_ns(SORTFORMER_MAX_PRIME_SAMPLES) + NS_PER_SECOND;
        assert_eq!(
            buffer.push(&chunk, offset),
            Err(DiarizationError::AudioTooLong)
        );
    }

    #[test]
    fn the_ceiling_is_the_documented_two_hours() {
        assert_eq!(SORTFORMER_MAX_PRIME_SAMPLES, 2 * 3_600 * SAMPLE_RATE_HZ);
        assert_eq!(samples_to_ns(SAMPLE_RATE_HZ), NS_PER_SECOND);
        assert_eq!(ns_to_samples(NS_PER_SECOND), SAMPLE_RATE_HZ);
    }
}
