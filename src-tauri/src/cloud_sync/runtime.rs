use std::{
    collections::HashMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Listener};
use uuid::Uuid;
use zeroize::Zeroize;

use crate::{
    meeting::{
        cloud_bundle::CloudMeetingBundleV1,
        session::{ImportRecordingRequest, MeetingSessionManager, RecordingOrigin},
        store::{
            CloudCapabilitiesCache, CloudConflict, CloudHead, CloudOutboxChunk, CloudOutboxInput,
            CloudOutboxKind, CloudOutboxRecord, CloudOutboxState, CloudOutboxUpdate,
            CloudShareContentKind, CloudShareInput, CloudShareRecord, CloudShareState,
            CloudShareUpdate, MeetingStore, StoreError,
        },
        types::{
            MeetingCommandError, MeetingConsentProvenance, MeetingListFilter,
            MeetingNavigationDestination, MeetingPhase, MeetingReviewSnapshot, MeetingSessionId,
            MANUAL_DEFAULT_TITLE,
        },
    },
    portable,
    secrets::{CloudSyncKeys, SecretManager},
    settings::{self, CloudSyncSettings, CLOUD_SYNC_CONSENT_VERSION},
};

use super::{
    client::{
        BootstrapDeviceRequest, CloudCapabilities, CloudClient, CloudClientError, CloudCredentials,
        CloudErrorCode, IdempotencyKey, ObjectManifestResponse, ObjectRevisionEnvelope,
        ObjectUploadPlan, PairDeviceRequest, ShareUploadPlan, TombstoneReason, TombstoneRequest,
        UploadChunkPlan,
    },
    crypto::{
        base64_url_decode, base64_url_encode, decode_recovery_code, ed25519_public_key,
        encode_recovery_code, open_object_revision_payload, open_pairing_envelope,
        open_share_payload, seal_object_revision_payload, seal_pairing_envelope,
        seal_share_payload, sha256_base64url, sha256_digest, sign_canonical_bootstrap,
        sign_canonical_pair_approval, sign_canonical_pair_candidate, sign_canonical_tombstone,
        sign_canonical_upload_envelope, verify_ed25519, CanonicalBootstrapInput,
        CanonicalPairApprovalInput, CanonicalPairCandidateInput, CanonicalTombstoneInput,
        CanonicalUploadEnvelopeInput, ObjectContentKind, ObjectRevisionCryptoContext,
        PairingEnvelopeSealInput, SharePayloadContext, SharePayloadDomain, StreamingSha256,
        UploadChunk, UploadKind,
    },
    share_file::{parse_worker_share_transport, read_share_file, write_share_file},
    types::{
        CloudBrowserShareCreateRequest, CloudBrowserShareResult, CloudConflictChoice,
        CloudConflictResolveRequest, CloudMeetingStatus, CloudObjectState,
        CloudPairingAcceptRequest, CloudPairingApproveRequest, CloudPairingOffer,
        CloudPairingOfferRequest, CloudShareCreateRequest, CloudShareImportRequest,
        CloudShareImportResult, CloudShareResult, CloudShareRevokeRequest,
        CloudSyncBootstrapRequest, CloudSyncBootstrapResult, CloudSyncChangedEvent,
        CloudSyncChangedPayload, CloudSyncErrorKind, CloudSyncOverview, CloudSyncRecoveryRequest,
        BROWSER_SHARE_TRUST_DISCLOSURE, CLOUD_SYNC_EVENT_SCHEMA_VERSION,
    },
};

const PROTOCOL_AUDIENCE: &str = "sona-companion";
const PROTOCOL_VERSION: u32 = 1;
const CRYPTO_VERSION: u32 = 1;
const OBJECT_SOURCE_FORMAT: &str = "sona-meeting-bundle-json-v1";
const CAPABILITY_SHARE_KIND: &str = "meeting_bundle";
const BROWSER_SHARE_KIND: &str = "markdown";
const BROWSER_SOURCE_FORMAT: &str = "markdown-utf8";
const OBJECT_MANIFEST_FILE: &str = "manifest.bin";
const MAX_ENCRYPTED_CHUNK_BYTES: usize = 4 * 1024 * 1024;
const MAX_PLAINTEXT_CHUNK_BYTES: usize = MAX_ENCRYPTED_CHUNK_BYTES - 28;
const MAX_BUNDLE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SHARE_PLAINTEXT_BYTES: usize = 256 * 1024 * 1024;
const CAPABILITIES_CACHE_MS: i64 = 5 * 60 * 1000;
const BACKGROUND_SCAN_INTERVAL: Duration = Duration::from_secs(5);
const MAX_RETRY_DELAY_MS: i64 = 5 * 60 * 1000;
const MAX_SHARE_EXPIRY_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// The manifest kind and `source_format` a paired phone or watch writes. Both
/// are the device's, verbatim: `source_format` is bound into the HKDF info and
/// the AES-GCM AAD of every payload in the object, so a single character apart
/// from the writer's value authenticates nothing.
const DEVICE_RECORDING_KIND: &str = "device_recording";
const DEVICE_RECORDING_SOURCE_FORMAT: &str = "sona-device-recording-v1";
const DEVICE_RECORDING_FORMAT_VERSION: u32 = 1;
/// The one audio shape a device uploads: 16 kHz mono signed 16-bit PCM, which
/// is the format the importer resamples every source to anyway.
const DEVICE_RECORDING_CODEC: &str = "pcm_s16le";
const DEVICE_RECORDING_SAMPLE_RATE_HZ: u32 = 16_000;
const DEVICE_RECORDING_CHANNELS: u16 = 1;
const DEVICE_RECORDING_BITS_PER_SAMPLE: u16 = 16;
/// One frame of that stream: one channel, two bytes.
const DEVICE_RECORDING_FRAME_BYTES: u64 = 2;
/// Twelve hours of it. This is `MAX_IMPORT_RECORDING_SAMPLES` in
/// `meeting::session` counted in bytes rather than samples: past it the
/// importer stops decoding, so a longer object would stage over a gigabyte of
/// audio only to be refused one layer down.
const MAX_DEVICE_RECORDING_AUDIO_BYTES: u64 = 16_000 * 60 * 60 * 12 * DEVICE_RECORDING_FRAME_BYTES;
/// A RIFF/WAVE header for one PCM stream: 12-byte container, 24-byte `fmt `
/// chunk, 8-byte `data` header.
const WAVE_HEADER_BYTES: usize = 44;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackgroundScanWake {
    Configured,
    Interval,
}

#[derive(Default)]
struct BackgroundScanState {
    configured: bool,
    scan_immediately: bool,
}

#[derive(Default)]
struct BackgroundScanGate {
    state: Mutex<BackgroundScanState>,
    changed: Condvar,
}

impl BackgroundScanGate {
    fn set_configured(&self, configured: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.configured == configured {
            return;
        }
        state.configured = configured;
        state.scan_immediately = configured;
        self.changed.notify_all();
    }

    fn wake(&self) {
        drop(
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        self.changed.notify_all();
    }

    fn wait(&self, stopped: &AtomicBool, interval: Duration) -> Option<BackgroundScanWake> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if stopped.load(Ordering::Acquire) {
                return None;
            }
            if !state.configured {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                continue;
            }
            if state.scan_immediately {
                state.scan_immediately = false;
                return Some(BackgroundScanWake::Configured);
            }
            let (next_state, timeout) = self
                .changed
                .wait_timeout(state, interval)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next_state;
            if timeout.timed_out() && state.configured {
                return Some(BackgroundScanWake::Interval);
            }
        }
    }
}

/// Whether stored settings ask this device to sync: setup finished, the current
/// disclosure accepted, not paused, an endpoint that parses, and an install
/// with credential storage. The background scan gates on this and so does
/// every error the UI shows, so a sync that is deliberately off cannot report
/// a failure it is not trying to reach.
fn sync_requested(settings: &CloudSyncSettings, portable_mode: bool) -> bool {
    settings.enabled
        && settings.has_current_consent()
        && !settings.paused
        && settings.endpoint().is_ok_and(|endpoint| endpoint.is_some())
        && !portable_mode
}

/// What the runtime knows about itself when a caller asks for the status error,
/// as opposed to what settings say. Named fields: these used to be three
/// positional `bool`s, and transposing two of them compiled while silently
/// moving which suppression was under test.
#[derive(Clone, Copy)]
struct SyncGate {
    portable: bool,
    stopped: bool,
    capturing: bool,
}

/// The one owner of the suppression rule. An operational error is worth showing
/// only while sync is running, so a shut-down runtime and a live capture hide it
/// on top of the settings the background scan already gates on.
fn active_status_error(
    settings: &CloudSyncSettings,
    gate: SyncGate,
    error: Option<CloudSyncErrorKind>,
) -> Option<CloudSyncErrorKind> {
    if gate.stopped || gate.capturing || !sync_requested(settings, gate.portable) {
        return None;
    }
    error
}

pub(crate) struct CloudSyncRuntime {
    app: AppHandle,
    meetings: Arc<MeetingSessionManager>,
    secrets: Arc<SecretManager>,
    stopped: AtomicBool,
    started: AtomicBool,
    client: Mutex<Option<EndpointClient>>,
    operational_error: Mutex<Option<CloudSyncErrorKind>>,
    background_scan: BackgroundScanGate,
}

struct EndpointClient {
    endpoint: String,
    client: CloudClient,
}

struct CloudAccess {
    store: Arc<MeetingStore>,
    state: crate::meeting::store::CloudState,
    client: CloudClient,
    keys: CloudSyncKeys,
}

struct StagedChunk {
    index: u32,
    size: u64,
    sha256: String,
    bytes: Vec<u8>,
}

struct StagedPayload {
    manifest: Vec<u8>,
    manifest_sha256: String,
    chunks: Vec<StagedChunk>,
    total_bytes: u64,
}

struct ConflictCacheInput<'a> {
    object_id: &'a str,
    revision_id: &'a str,
    sequence: u64,
    source_session_id: Option<MeetingSessionId>,
    source_revision: Option<u64>,
    bundle: &'a CloudMeetingBundleV1,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObjectPayloadManifest {
    version: u32,
    kind: String,
    source_format: String,
    chunk_count: u32,
    plaintext_bytes: u64,
    plaintext_sha256: String,
}

/// The plaintext manifest of a `device_recording` object, as
/// `mobile/Shared/DeviceRecordingObject.swift` writes it. The device emits
/// sorted keys; JSON is unordered, so this reader does not care either way.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceRecordingManifest {
    format_version: u32,
    kind: String,
    device_id: String,
    recorded_at_utc_ms: i64,
    /// The device's own measurement. The importer measures the meeting's
    /// duration off the decoded audio, so this is not read here — but the
    /// reader is strict about unknown fields, so it has to be named.
    #[allow(dead_code)]
    duration_ms: i64,
    title: String,
    audio: DeviceRecordingAudio,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceRecordingAudio {
    codec: String,
    sample_rate_hz: u32,
    channels: u16,
    byte_length: u64,
    sha256: String,
}

/// What a revision says it is. The manifest is the only place an object
/// declares its kind, and `source_format` — which decides the manifest's own
/// key — is not on the wire, so the reader tries the formats it installs.
enum RemoteManifest {
    Meeting(ObjectPayloadManifest),
    DeviceRecording(DeviceRecordingManifest),
}

/// What a revision turned out to be once its manifest was opened.
enum RemoteObject {
    /// A meeting bundle, from this vault's desktops. Boxed: the bundle is the
    /// whole meeting, and the other arms are a file handle or nothing.
    Meeting(Box<CloudMeetingBundleV1>),
    /// A paired device's recording, decrypted to a file this process owns.
    DeviceRecording(DeviceRecordingImport),
    /// A device recording this Mac wrote itself. The audio never left, so the
    /// revision is acknowledged and nothing is imported.
    OwnDeviceRecording,
}

/// A pulled recording, staged as a WAV file for the importer to decode.
struct DeviceRecordingImport {
    audio: ScratchAudioFile,
    title: String,
    recorded_at_utc_ms: i64,
    device_id: String,
}

impl DeviceRecordingImport {
    /// The import request this recording files as: the device's title when it
    /// gave one, else the placeholder a meeting carries until its notes name
    /// it. The importer's own fallback is the file stem, and the staged file
    /// is named for the object, so left to it an untitled recording would be
    /// filed under an opaque id. The staged file outlives the request: `audio`
    /// is still what removes it once the import returns.
    fn request(&self) -> ImportRecordingRequest {
        let title = self.title.trim();
        ImportRecordingRequest {
            path: self.audio.path().to_owned(),
            title: Some(
                if title.is_empty() {
                    MANUAL_DEFAULT_TITLE
                } else {
                    title
                }
                .to_owned(),
            ),
            recorded_at_utc_ms: Some(self.recorded_at_utc_ms),
            origin: RecordingOrigin::PairedDevice {
                device_id: self.device_id.clone(),
            },
        }
    }
}

/// A staged audio file, removed when this value is dropped.
///
/// The pull path can fail at a dozen points between writing the first chunk
/// and finishing the import, and every one of them has to leave the store's
/// scratch directory as it found it. Ownership is the only version of that
/// promise which cannot be forgotten at a new `?`.
struct ScratchAudioFile(PathBuf);

impl ScratchAudioFile {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchAudioFile {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "Staged cloud recording {} remains: {error}",
                    self.0.display()
                );
            }
        }
    }
}

/// Which object's chunks a staging file is decrypting, and how many there are.
#[derive(Clone, Copy)]
struct DeviceRecordingPayload<'a> {
    vault_root: &'a [u8],
    vault_id: &'a str,
    object_id: &'a str,
    revision_id: &'a str,
    chunk_count: u64,
}

/// The WAV file a pulled recording is decrypted into, one chunk at a time.
///
/// Twelve hours of audio does not belong in memory, so each chunk is opened,
/// digested and written as it arrives. Each chunk's AAD binds it to this vault,
/// object, revision and index, so what lands on disk is the bytes its writer
/// sealed; `finish` is what says the file as a whole is the audio the manifest
/// declared, and until it returns the file belongs to nobody.
struct DeviceRecordingStaging<'a> {
    scratch: ScratchAudioFile,
    file: File,
    digest: StreamingSha256,
    written: u64,
    payload: DeviceRecordingPayload<'a>,
}

impl<'a> DeviceRecordingStaging<'a> {
    fn create(
        path: PathBuf,
        audio_bytes: u32,
        payload: DeviceRecordingPayload<'a>,
    ) -> Result<Self, CloudRuntimeError> {
        // The guard owns the path from here, so every later failure removes it.
        let scratch = ScratchAudioFile(path);
        let mut file = File::create(scratch.path()).map_err(|_| CloudRuntimeError::File)?;
        file.write_all(&wave_header(audio_bytes))
            .map_err(|_| CloudRuntimeError::File)?;
        Ok(Self {
            scratch,
            file,
            digest: StreamingSha256::default(),
            written: 0,
            payload,
        })
    }

    fn write_chunk(&mut self, index: u64, encrypted_chunk: &[u8]) -> Result<(), CloudRuntimeError> {
        let mut audio = open_object_revision_payload(
            self.payload.vault_root,
            &ObjectRevisionCryptoContext {
                vault_id: self.payload.vault_id,
                object_id: self.payload.object_id,
                revision_id: self.payload.revision_id,
                index,
                total: self.payload.chunk_count,
                content_kind: ObjectContentKind::Chunk,
                source_format: DEVICE_RECORDING_SOURCE_FORMAT,
            },
            encrypted_chunk,
        )
        .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        self.digest.update(&audio);
        self.written = self
            .written
            .saturating_add(u64::try_from(audio.len()).unwrap_or(MAX_DEVICE_RECORDING_AUDIO_BYTES));
        let written = self
            .file
            .write_all(&audio)
            .map_err(|_| CloudRuntimeError::File);
        audio.zeroize();
        written
    }

    /// Hand the file over, or refuse audio that is not what was declared. A
    /// refusal drops the guard, so the scratch directory is left as it was.
    fn finish(
        self,
        declared: &DeviceRecordingAudio,
    ) -> Result<ScratchAudioFile, CloudRuntimeError> {
        if self.written != declared.byte_length || self.digest.finish_base64url() != declared.sha256
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        Ok(self.scratch)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityShareManifest {
    version: u32,
    kind: String,
    source_format: String,
    chunk_count: u32,
    plaintext_bytes: u64,
    plaintext_sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserShareManifest {
    version: u32,
    kind: String,
    source_format: String,
    title: String,
    chunk_count: u32,
    plaintext_bytes: u64,
}

#[derive(Serialize)]
struct WorkerShareTransport<'a> {
    format: &'static str,
    version: u32,
    share: WorkerShareTransportMetadata<'a>,
    manifest: String,
    chunks: Vec<WorkerShareTransportChunk>,
}

#[derive(Serialize)]
struct WorkerShareTransportMetadata<'a> {
    share_id: &'a str,
    crypto_version: u32,
    manifest_sha256: &'a str,
    chunk_count: u32,
    total_bytes: u64,
    writer_signature: &'a str,
}

#[derive(Serialize)]
struct WorkerShareTransportChunk {
    index: u32,
    size: u32,
    sha256: String,
}

pub(crate) enum CloudRuntimeError {
    PortableUnavailable,
    SecretUnavailable,
    SetupRequired,
    IntegrityFailure,
    Conflict,
    UnsupportedProtocol,
    Deferred,
    Client(CloudClientError),
    Storage,
    File,
    Randomness,
}

impl CloudRuntimeError {
    pub(crate) fn kind(&self) -> CloudSyncErrorKind {
        match self {
            Self::PortableUnavailable => CloudSyncErrorKind::PortableUnavailable,
            Self::SecretUnavailable => CloudSyncErrorKind::SecretUnavailable,
            Self::SetupRequired => CloudSyncErrorKind::SetupRequired,
            Self::IntegrityFailure | Self::Storage | Self::File => {
                CloudSyncErrorKind::IntegrityFailure
            }
            Self::Conflict => CloudSyncErrorKind::Conflict,
            Self::UnsupportedProtocol => CloudSyncErrorKind::UnsupportedProtocol,
            Self::Deferred | Self::Randomness => CloudSyncErrorKind::Transient,
            Self::Client(error) => match error.api_error().map(|api| api.code) {
                Some(CloudErrorCode::Unauthorized | CloudErrorCode::RevokedDevice) => {
                    CloudSyncErrorKind::AuthRequired
                }
                Some(CloudErrorCode::QuotaExceeded) => CloudSyncErrorKind::Quota,
                Some(
                    CloudErrorCode::IntegrityFailed
                    | CloudErrorCode::ChunkConflict
                    | CloudErrorCode::IdempotencyConflict,
                ) => CloudSyncErrorKind::IntegrityFailure,
                Some(CloudErrorCode::StaleRevision) => CloudSyncErrorKind::Conflict,
                Some(CloudErrorCode::UnsupportedVersion) => CloudSyncErrorKind::UnsupportedProtocol,
                _ => CloudSyncErrorKind::Transient,
            },
        }
    }
}

impl fmt::Display for CloudRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.kind() {
            CloudSyncErrorKind::PortableUnavailable => "cloud sync is unavailable in portable mode",
            CloudSyncErrorKind::SecretUnavailable => "cloud sync keys are unavailable",
            CloudSyncErrorKind::SetupRequired => "cloud sync needs setup",
            CloudSyncErrorKind::AuthRequired => "cloud sync authentication is required",
            CloudSyncErrorKind::Quota => "cloud sync quota is exhausted",
            CloudSyncErrorKind::IntegrityFailure => "cloud sync integrity validation failed",
            CloudSyncErrorKind::Conflict => "cloud sync has a conflict",
            CloudSyncErrorKind::UnsupportedProtocol => "cloud sync protocol is unsupported",
            CloudSyncErrorKind::Transient => "cloud sync is temporarily unavailable",
        };
        formatter.write_str(message)
    }
}

impl fmt::Debug for CloudRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CloudRuntimeError")
            .field(&self.kind())
            .finish()
    }
}

impl std::error::Error for CloudRuntimeError {}

impl CloudSyncRuntime {
    pub(crate) fn new(
        app: AppHandle,
        meetings: Arc<MeetingSessionManager>,
        secrets: Arc<SecretManager>,
    ) -> Self {
        Self {
            app,
            meetings,
            secrets,
            stopped: AtomicBool::new(false),
            started: AtomicBool::new(false),
            client: Mutex::new(None),
            operational_error: Mutex::new(None),
            background_scan: BackgroundScanGate::default(),
        }
    }

    pub(crate) fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        self.cloud_settings_changed(&settings::get_settings(&self.app).cloud_sync);

        let event_runtime = Arc::clone(self);
        self.app.listen("meeting:session-changed", move |event| {
            let Ok(payload) =
                serde_json::from_str::<crate::meeting::types::MeetingEventPayload>(event.payload())
            else {
                return;
            };
            let Some(session_id) = payload.session_id else {
                return;
            };
            let runtime = Arc::clone(&event_runtime);
            tauri::async_runtime::spawn(async move {
                let _ = runtime.queue_current_session(session_id).await;
            });
        });

        let runtime = Arc::clone(self);
        let _ = thread::Builder::new()
            .name("sona-cloud-sync".to_owned())
            .spawn(move || {
                while let Some(wake) = runtime
                    .background_scan
                    .wait(&runtime.stopped, BACKGROUND_SCAN_INTERVAL)
                {
                    let started = std::time::Instant::now();
                    if wake == BackgroundScanWake::Configured {
                        if let Ok(store) =
                            tauri::async_runtime::block_on(runtime.meetings.cloud_store())
                        {
                            let _ = store.recover_claimed_cloud_outbox(utc_now_ms());
                        }
                    }
                    let _ = tauri::async_runtime::block_on(runtime.sync_once());
                    log::debug!(
                        "Cloud sync background scan finished in {:?}",
                        started.elapsed()
                    );
                }
            })
            .map_err(|error| log::warn!("Cloud sync loop is unavailable: {error}"));
    }

    pub(crate) fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        self.background_scan.wake();
    }

    pub(crate) fn cloud_settings_changed(&self, settings: &CloudSyncSettings) {
        let configured = sync_requested(settings, portable::is_portable());
        self.background_scan.set_configured(configured);
        if !configured {
            self.set_operational_error(None);
        }
    }

    pub(crate) fn status_error_for(
        &self,
        settings: &CloudSyncSettings,
        portable_mode: bool,
    ) -> Option<CloudSyncErrorKind> {
        let error = *self
            .operational_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active_status_error(
            settings,
            SyncGate {
                portable: portable_mode,
                stopped: self.stopped.load(Ordering::Acquire),
                capturing: self.meetings.is_capture_active(),
            },
            error,
        )
    }

    fn set_operational_error(&self, error: Option<CloudSyncErrorKind>) {
        let changed = {
            let mut current = self
                .operational_error
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *current == error {
                false
            } else {
                *current = error;
                true
            }
        };
        if changed {
            self.emit_changed(None, None);
        }
    }

    pub(crate) async fn overview(&self) -> Result<CloudSyncOverview, CloudRuntimeError> {
        let settings = settings::get_settings(&self.app).cloud_sync;
        let portable_mode = portable::is_portable();
        let Some(store) = self.meetings.cloud_store().await.ok() else {
            // Closed, not corrupt: startup recovery has not finished, or the
            // mount is still waiting behind a system prompt.
            self.set_operational_error(Some(CloudSyncErrorKind::SetupRequired));
            return Ok(CloudSyncOverview {
                enabled: settings.enabled && settings.has_current_consent(),
                portable_mode,
                paused: settings.paused,
                queued_objects: 0,
                pending_deletions: 0,
                terminal_error: self.status_error_for(&settings, portable_mode),
            });
        };
        let counts = store.cloud_status_counts().map_err(map_store_error)?;
        let terminal_error = store
            .cloud_latest_terminal_error()
            .map_err(map_store_error)?
            .as_deref()
            .and_then(terminal_error_kind);
        let state_paused = store
            .cloud_state()
            .map_err(map_store_error)?
            .is_some_and(|state| state.paused);
        Ok(CloudSyncOverview {
            enabled: settings.enabled && settings.has_current_consent(),
            portable_mode,
            paused: settings.paused || state_paused,
            queued_objects: counts.queued_outbox.saturating_add(counts.claimed_outbox),
            pending_deletions: counts.pending_tombstones,
            terminal_error: self
                .status_error_for(&settings, portable_mode)
                .or(terminal_error),
        })
    }

    pub(crate) async fn meeting_status(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<CloudMeetingStatus, CloudRuntimeError> {
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        self.meeting_status_from_store(&store, session_id)
    }

    pub(crate) async fn meeting_status_list(
        &self,
    ) -> Result<Vec<CloudMeetingStatus>, CloudRuntimeError> {
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        let page = self
            .meetings
            .list(None, 100, MeetingListFilter::default())
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        page.entries
            .into_iter()
            .map(|entry| self.meeting_status_from_store(&store, entry.session_id))
            .collect()
    }

    pub(crate) async fn bootstrap(
        &self,
        request: CloudSyncBootstrapRequest,
    ) -> Result<CloudSyncBootstrapResult, CloudRuntimeError> {
        self.reject_portable()?;
        let endpoint = canonical_endpoint(&request.endpoint)?;
        if request.bootstrap_secret.trim().is_empty() {
            return Err(CloudRuntimeError::SetupRequired);
        }
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        let keys = self
            .secrets
            .cloud_sync_keys()
            .await
            .map_err(|_| CloudRuntimeError::SecretUnavailable)?;
        let existing = store.cloud_state().map_err(map_store_error)?;
        let (vault_id, device_id) = match existing {
            Some(state) if state.endpoint == endpoint && state.paused => {
                (state.vault_id, state.device_id)
            }
            Some(_) => return Err(CloudRuntimeError::Conflict),
            None => (random_opaque_id()?, random_opaque_id()?),
        };
        let signing_public_key = ed25519_public_key(&*keys.signing_seed)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let pairing_public_key = super::crypto::x25519_public_key(&*keys.pairing_secret)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let bootstrap_input = CanonicalBootstrapInput {
            audience: PROTOCOL_AUDIENCE,
            vault_id: &vault_id,
            device_id: &device_id,
            signing_public_key: &signing_public_key,
            pairing_public_key: &pairing_public_key,
        };
        let bootstrap_signature = sign_canonical_bootstrap(&bootstrap_input, &*keys.signing_seed)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let mut state = crate::meeting::store::CloudState {
            vault_id: vault_id.clone(),
            device_id: device_id.clone(),
            endpoint: endpoint.clone(),
            cursor: None,
            snapshot_high_water: None,
            clock_offset_ms: 0,
            paused: true,
        };
        store.upsert_cloud_state(&state).map_err(map_store_error)?;
        let client = self.client_for(&endpoint)?;
        let idempotency = idempotency_key(&["bootstrap", &vault_id, &device_id])?;
        let response = client
            .bootstrap_device(
                request.bootstrap_secret.trim(),
                &idempotency,
                &BootstrapDeviceRequest {
                    version: PROTOCOL_VERSION,
                    vault_id: vault_id.clone(),
                    device_id: device_id.clone(),
                    signing_public_key: base64_url_encode(&signing_public_key),
                    pairing_public_key: base64_url_encode(&pairing_public_key),
                    self_signature: base64_url_encode(&bootstrap_signature),
                },
            )
            .await
            .map_err(CloudRuntimeError::Client);
        self.persist_clock(&store, &client);
        let response = response?;
        if response.vault_id != vault_id
            || response.device_id != device_id
            || response.status != "active"
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        validate_capabilities(&response.capabilities)?;
        self.persist_capabilities(&store, &endpoint, &response.capabilities)?;
        state.clock_offset_ms = client.clock_offset_ms();
        state.paused = false;
        store.upsert_cloud_state(&state).map_err(map_store_error)?;
        settings::update_settings(&self.app, |settings| {
            settings.cloud_sync.enabled = true;
            settings.cloud_sync.paused = false;
            settings.cloud_sync.consent_version = Some(CLOUD_SYNC_CONSENT_VERSION);
            settings.cloud_sync.endpoint = Some(endpoint);
        })?;
        let recovery_code = encode_recovery_code(&vault_id, &*keys.vault_root)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let overview = self.overview().await?;
        self.emit_changed(None, None);
        Ok(CloudSyncBootstrapResult {
            overview,
            recovery_code,
        })
    }

    pub(crate) async fn recover(
        &self,
        request: CloudSyncRecoveryRequest,
    ) -> Result<CloudSyncOverview, CloudRuntimeError> {
        self.reject_portable()?;
        let endpoint = canonical_endpoint(&request.endpoint)?;
        let recovery = decode_recovery_code(request.recovery_code.trim())
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        self.secrets
            .replace_cloud_vault_root(recovery.vault_root)
            .await
            .map_err(|_| CloudRuntimeError::SecretUnavailable)?;
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        let existing = store.cloud_state().map_err(map_store_error)?;
        let state = match existing {
            Some(mut state) if state.vault_id == recovery.vault_id => {
                state.endpoint = endpoint;
                state
            }
            Some(_) | None => crate::meeting::store::CloudState {
                vault_id: recovery.vault_id,
                device_id: random_opaque_id()?,
                endpoint,
                cursor: None,
                snapshot_high_water: None,
                clock_offset_ms: 0,
                paused: true,
            },
        };
        store.upsert_cloud_state(&state).map_err(map_store_error)?;
        self.emit_changed(None, None);
        self.overview().await
    }

    pub(crate) async fn pairing_offer(
        &self,
        request: CloudPairingOfferRequest,
    ) -> Result<CloudPairingOffer, CloudRuntimeError> {
        self.reject_portable()?;
        let endpoint = canonical_endpoint(&request.endpoint)?;
        let keys = self
            .secrets
            .cloud_sync_keys()
            .await
            .map_err(|_| CloudRuntimeError::SecretUnavailable)?;
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        let existing = store.cloud_state().map_err(map_store_error)?;
        let device_id = match existing {
            Some(state)
                if state.vault_id == request.vault_id
                    && state.endpoint == endpoint
                    && state.paused =>
            {
                state.device_id
            }
            Some(_) => return Err(CloudRuntimeError::Conflict),
            None => random_opaque_id()?,
        };
        let signing_public_key = ed25519_public_key(&*keys.signing_seed)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let pairing_public_key = super::crypto::x25519_public_key(&*keys.pairing_secret)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let pairing_nonce = random_array::<16>()?;
        let expires_at_utc_ms = adjusted_now_ms(0)
            .checked_add(15 * 60 * 1000)
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        let expires_at =
            u64::try_from(expires_at_utc_ms).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let candidate_input = CanonicalPairCandidateInput {
            audience: PROTOCOL_AUDIENCE,
            vault_id: &request.vault_id,
            candidate_device_id: &device_id,
            candidate_signing_public_key: &signing_public_key,
            candidate_pairing_public_key: &pairing_public_key,
            pairing_nonce: &pairing_nonce,
            expires_at,
        };
        let candidate_proof = sign_canonical_pair_candidate(&candidate_input, &*keys.signing_seed)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let candidate_record = super::crypto::canonical_pair_candidate_bytes(&candidate_input)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let fingerprint = candidate_fingerprint(&candidate_record)?;
        let state = crate::meeting::store::CloudState {
            vault_id: request.vault_id.clone(),
            device_id: device_id.clone(),
            endpoint,
            cursor: None,
            snapshot_high_water: None,
            clock_offset_ms: 0,
            paused: true,
        };
        store.upsert_cloud_state(&state).map_err(map_store_error)?;
        Ok(CloudPairingOffer {
            protocol_version: PROTOCOL_VERSION,
            vault_id: request.vault_id,
            device_id,
            signing_public_key: base64_url_encode(&signing_public_key),
            pairing_public_key: base64_url_encode(&pairing_public_key),
            candidate_proof: base64_url_encode(&candidate_proof),
            pairing_nonce: base64_url_encode(&pairing_nonce),
            expires_at_utc_ms,
            fingerprint,
        })
    }

    pub(crate) async fn pairing_approve(
        &self,
        request: CloudPairingApproveRequest,
    ) -> Result<CloudSyncOverview, CloudRuntimeError> {
        let access = self.configured_access().await?;
        let offer = request.offer;
        if offer.protocol_version != PROTOCOL_VERSION || offer.vault_id != access.state.vault_id {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let now = adjusted_now_ms(access.state.clock_offset_ms);
        let max_expiry = now
            .checked_add(15 * 60 * 1000)
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        if offer.expires_at_utc_ms <= now || offer.expires_at_utc_ms > max_expiry {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let candidate = verified_candidate(&offer)?;
        let (mut ephemeral_secret_key, envelope_nonce) =
            pairing_envelope_material(&access.keys.pairing_secret, &candidate.record);
        let envelope = seal_pairing_envelope(&PairingEnvelopeSealInput {
            recipient_public_key: &candidate.pairing_public_key,
            ephemeral_secret_key: &ephemeral_secret_key,
            nonce: &envelope_nonce,
            vault_root: &*access.keys.vault_root,
        });
        ephemeral_secret_key.zeroize();
        let envelope = envelope.map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let approval_input = CanonicalPairApprovalInput {
            vault_id: &access.state.vault_id,
            candidate_record: &candidate.record,
            candidate_proof: &candidate.proof,
            envelope: &envelope,
        };
        let approval_signature =
            sign_canonical_pair_approval(&approval_input, &*access.keys.signing_seed)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        self.require_request_permission(&access.state).await?;
        let credentials = credentials(&access)?;
        let key = idempotency_key(&[
            "pair",
            &access.state.vault_id,
            &offer.device_id,
            &offer.pairing_nonce,
        ])?;
        let result = access
            .client
            .pair_device(
                &credentials,
                &key,
                &PairDeviceRequest {
                    version: PROTOCOL_VERSION,
                    candidate_device_id: offer.device_id.clone(),
                    candidate_signing_public_key: offer.signing_public_key,
                    candidate_pairing_public_key: offer.pairing_public_key,
                    candidate_proof: offer.candidate_proof,
                    pairing_nonce: offer.pairing_nonce,
                    expires_at: u64::try_from(offer.expires_at_utc_ms)
                        .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
                    envelope: base64_url_encode(&envelope),
                    approval_signature: base64_url_encode(&approval_signature),
                },
            )
            .await
            .map_err(CloudRuntimeError::Client);
        self.persist_clock(&access.store, &access.client);
        let response = result?;
        if response.device_id != offer.device_id || response.status != "active" {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        self.emit_changed(None, None);
        self.overview().await
    }

    pub(crate) async fn pairing_accept(
        &self,
        request: CloudPairingAcceptRequest,
    ) -> Result<CloudSyncOverview, CloudRuntimeError> {
        self.reject_portable()?;
        let endpoint = canonical_endpoint(&request.endpoint)?;
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        let state = store
            .cloud_state()
            .map_err(map_store_error)?
            .ok_or(CloudRuntimeError::SetupRequired)?;
        let offer = request.offer;
        if !state.paused
            || state.endpoint != endpoint
            || state.vault_id != offer.vault_id
            || state.device_id != offer.device_id
            || offer.protocol_version != PROTOCOL_VERSION
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let keys = self
            .secrets
            .cloud_sync_keys()
            .await
            .map_err(|_| CloudRuntimeError::SecretUnavailable)?;
        let expected_signing = ed25519_public_key(&*keys.signing_seed)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let expected_pairing = super::crypto::x25519_public_key(&*keys.pairing_secret)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        if base64_url_encode(&expected_signing) != offer.signing_public_key
            || base64_url_encode(&expected_pairing) != offer.pairing_public_key
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let client = self.client_for(&endpoint)?;
        client.set_clock_offset_ms(state.clock_offset_ms);
        let credentials =
            CloudCredentials::new(&state.vault_id, &state.device_id, &keys.signing_seed)
                .map_err(CloudRuntimeError::Client)?;
        let response = client
            .self_device(&credentials)
            .await
            .map_err(CloudRuntimeError::Client);
        self.persist_clock(&store, &client);
        let response = response?;
        if response.device_id != state.device_id
            || response.status != "active"
            || response.protocol_version != Some(PROTOCOL_VERSION)
            || response.signing_public_key != offer.signing_public_key
            || response.pairing_public_key != offer.pairing_public_key
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let envelope = response
            .envelope
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        let envelope =
            base64_url_decode(&envelope).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let vault_root = open_pairing_envelope(&*keys.pairing_secret, &envelope)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        self.secrets
            .replace_cloud_vault_root(vault_root)
            .await
            .map_err(|_| CloudRuntimeError::SecretUnavailable)?;
        let mut active_state = state;
        active_state.paused = false;
        active_state.clock_offset_ms = client.clock_offset_ms();
        store
            .upsert_cloud_state(&active_state)
            .map_err(map_store_error)?;
        settings::update_settings(&self.app, |settings| {
            settings.cloud_sync.enabled = true;
            settings.cloud_sync.paused = false;
            settings.cloud_sync.consent_version = Some(CLOUD_SYNC_CONSENT_VERSION);
            settings.cloud_sync.endpoint = Some(endpoint);
        })?;
        self.emit_changed(None, None);
        self.overview().await
    }

    pub(crate) async fn pause(&self) -> Result<CloudSyncOverview, CloudRuntimeError> {
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        store.set_cloud_paused(true).map_err(map_store_error)?;
        settings::update_settings(&self.app, |settings| settings.cloud_sync.paused = true)?;
        self.emit_changed(None, None);
        self.overview().await
    }

    pub(crate) async fn resume(&self) -> Result<CloudSyncOverview, CloudRuntimeError> {
        self.reject_portable()?;
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        if store.cloud_state().map_err(map_store_error)?.is_none() {
            return Err(CloudRuntimeError::SetupRequired);
        }
        store.set_cloud_paused(false).map_err(map_store_error)?;
        settings::update_settings(&self.app, |settings| settings.cloud_sync.paused = false)?;
        self.emit_changed(None, None);
        self.overview().await
    }

    pub(crate) async fn retry(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<CloudMeetingStatus, CloudRuntimeError> {
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        let outboxes = store
            .cloud_outboxes_for_session(session_id)
            .map_err(map_store_error)?;
        let now = utc_now_ms();
        let mut retried = false;
        for outbox in outboxes
            .iter()
            .filter(|outbox| outbox.state == CloudOutboxState::Terminal)
        {
            store
                .retry_terminal_cloud_outbox(&outbox.outbox_id, now)
                .map_err(map_store_error)?;
            retried = true;
        }
        if !retried && outboxes.is_empty() {
            return Err(CloudRuntimeError::SetupRequired);
        }
        let status = self.meeting_status_from_store(&store, session_id)?;
        self.emit_changed(Some(session_id), Some(status.state));
        Ok(status)
    }

    pub(crate) async fn conflict_resolve(
        &self,
        request: CloudConflictResolveRequest,
    ) -> Result<CloudMeetingStatus, CloudRuntimeError> {
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        let head = store
            .cloud_head_for_session(request.session_id)
            .map_err(map_store_error)?
            .ok_or(CloudRuntimeError::Conflict)?;
        match request.choice {
            CloudConflictChoice::KeepLocal => {
                store
                    .resolve_cloud_conflict_keep_local(
                        &head.object_id,
                        stable_idempotency_value(&["keep-local", &head.object_id]),
                        utc_now_ms(),
                    )
                    .map_err(map_store_error)?;
            }
            CloudConflictChoice::UseRemote => {
                let path = store
                    .cloud_conflict_bundle_path(&head.object_id)
                    .map_err(map_store_error)?;
                let bytes = fs::read(path).map_err(|_| CloudRuntimeError::File)?;
                let bundle = CloudMeetingBundleV1::from_json_bytes(&bytes)
                    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                let snapshot = self
                    .meetings
                    .import_cloud_bundle(bundle)
                    .await
                    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                store
                    .resolve_cloud_conflict_use_remote(&head.object_id, snapshot.session_id)
                    .map_err(map_store_error)?;
            }
        }
        let status = self.meeting_status_from_store(&store, request.session_id)?;
        self.emit_changed(Some(request.session_id), Some(status.state));
        Ok(status)
    }

    pub(crate) async fn share_create(
        &self,
        request: CloudShareCreateRequest,
    ) -> Result<CloudShareResult, CloudRuntimeError> {
        self.reject_portable()?;
        let store = self.queueable_store().await?;
        let destination = PathBuf::from(&request.destination_path);
        if !destination
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("sona"))
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let share = self
            .create_share_intent(
                &store,
                request.session_id,
                request.expires_at_utc_ms,
                CloudShareContentKind::CapabilityBundle,
            )
            .await?;
        let (root, payload) = self.stage_share(&store, &share.outbox, &share.record)?;
        let writer_signature =
            self.share_writer_signature(&share.access, &share.record, &payload)?;
        let transport = worker_share_transport(&share.record, &payload, &writer_signature)?;
        let validated = parse_worker_share_transport(transport)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        write_share_file(
            &destination,
            &root,
            &validated.header,
            &validated.ciphertext_frames,
        )
        .map_err(|_| CloudRuntimeError::File)?;
        self.emit_changed(Some(request.session_id), Some(CloudObjectState::Queued));
        Ok(CloudShareResult {
            share_id: share.record.share_id,
            expires_at_utc_ms: share.record.expires_at_utc_ms,
            file_path: request.destination_path,
        })
    }

    pub(crate) async fn browser_share_create(
        &self,
        request: CloudBrowserShareCreateRequest,
    ) -> Result<CloudBrowserShareResult, CloudRuntimeError> {
        self.reject_portable()?;
        let store = self.queueable_store().await?;
        let share = self
            .create_share_intent(
                &store,
                request.session_id,
                request.expires_at_utc_ms,
                CloudShareContentKind::BrowserMarkdown,
            )
            .await?;
        let (root, _) = self.stage_share(&store, &share.outbox, &share.record)?;
        let endpoint = share.access.state.endpoint.trim_end_matches('/');
        let share_url = format!(
            "{endpoint}/s/{}#{}",
            share.record.share_id,
            base64_url_encode(&root)
        );
        self.emit_changed(Some(request.session_id), Some(CloudObjectState::Queued));
        Ok(CloudBrowserShareResult {
            share_id: share.record.share_id,
            expires_at_utc_ms: share.record.expires_at_utc_ms,
            share_url,
            trust_disclosure: BROWSER_SHARE_TRUST_DISCLOSURE.to_owned(),
        })
    }

    pub(crate) async fn share_revoke(
        &self,
        request: CloudShareRevokeRequest,
    ) -> Result<CloudSyncOverview, CloudRuntimeError> {
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        let current = store
            .cloud_share(&request.share_id)
            .map_err(map_store_error)?
            .ok_or(CloudRuntimeError::SetupRequired)?;
        if current.state == CloudShareState::Revoked {
            return self.overview().await;
        }
        if let Some(outbox_id) = &current.outbox_id {
            let _ = store.cancel_cloud_outbox(outbox_id);
        }
        let revoked_at_utc_ms = utc_now_ms();
        let revoked = store
            .revoke_cloud_share(&request.share_id, revoked_at_utc_ms)
            .map_err(map_store_error)?;
        let outbox = store
            .enqueue_cloud_outbox(CloudOutboxInput {
                kind: CloudOutboxKind::Share,
                object_id: revoked.share_id.clone(),
                source_session_id: None,
                source_revision: None,
                base_remote_revision_id: None,
                share_content_kind: Some(revoked.content_kind),
                remote_revision_id: None,
                idempotency_key: stable_idempotency_value(&[
                    "share-revoke",
                    &revoked.share_id,
                    &revoked_at_utc_ms.to_string(),
                ]),
                next_attempt_utc_ms: revoked_at_utc_ms,
            })
            .map_err(map_store_error)?;
        store
            .update_cloud_share(
                &revoked.share_id,
                CloudShareUpdate {
                    expires_at_utc_ms: revoked.expires_at_utc_ms,
                    state: CloudShareState::Revoked,
                    outbox_id: Some(outbox.outbox_id),
                    revoked_at_utc_ms: revoked.revoked_at_utc_ms,
                },
            )
            .map_err(map_store_error)?;
        self.emit_changed(
            revoked.source_session_id,
            Some(CloudObjectState::PendingDeletion),
        );
        self.overview().await
    }

    pub(crate) async fn share_import(
        &self,
        request: CloudShareImportRequest,
    ) -> Result<CloudShareImportResult, CloudRuntimeError> {
        let path = Path::new(&request.path);
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("sona"))
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let file = read_share_file(path).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let bundle = decrypt_capability_bundle(&file)?;
        let snapshot = self
            .meetings
            .import_cloud_bundle(bundle)
            .await
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        crate::show_meeting_destination(
            &self.app,
            MeetingNavigationDestination::Session,
            Some(&snapshot),
        );
        self.emit_changed(Some(snapshot.session_id), Some(CloudObjectState::Committed));
        Ok(CloudShareImportResult {
            session_id: snapshot.session_id,
        })
    }

    pub(crate) async fn opened_share_file(&self, path: PathBuf) -> Result<(), CloudRuntimeError> {
        self.share_import(CloudShareImportRequest {
            path: path.to_string_lossy().into_owned(),
        })
        .await
        .map(|_| ())
    }

    async fn sync_once(&self) -> Result<(), CloudRuntimeError> {
        let Some(access) = self.active_access().await? else {
            return Ok(());
        };
        if !self.ensure_capabilities(&access).await? {
            return Ok(());
        }
        self.drain_outbox(&access).await?;
        if self.request_permitted(&access.state).await {
            self.pull_changes(&access).await?;
        }
        Ok(())
    }

    async fn active_access(&self) -> Result<Option<CloudAccess>, CloudRuntimeError> {
        if self.stopped.load(Ordering::Acquire) || portable::is_portable() {
            return Ok(None);
        }
        let cloud_settings = settings::get_settings(&self.app).cloud_sync;
        if self.meetings.is_capture_active() {
            self.set_operational_error(None);
            return Ok(None);
        }
        if !cloud_settings.enabled || !cloud_settings.has_current_consent() || cloud_settings.paused
        {
            return Ok(None);
        }
        let endpoint = match cloud_settings.endpoint() {
            Ok(Some(endpoint)) => endpoint,
            Ok(None) | Err(_) => return Ok(None),
        };
        let store = match self.meetings.cloud_store().await {
            Ok(store) => store,
            // Not mounted yet, which is setup rather than a failed check.
            Err(_) => {
                self.set_operational_error(Some(CloudSyncErrorKind::SetupRequired));
                return Ok(None);
            }
        };
        let state = match store.cloud_state() {
            Ok(Some(state)) if state.paused => {
                self.set_operational_error(None);
                return Ok(None);
            }
            Ok(Some(state)) if state.endpoint == endpoint => state,
            Ok(_) => {
                self.set_operational_error(Some(CloudSyncErrorKind::SetupRequired));
                return Ok(None);
            }
            Err(_) => {
                self.set_operational_error(Some(CloudSyncErrorKind::IntegrityFailure));
                return Ok(None);
            }
        };
        let keys = match self.secrets.cloud_sync_keys().await {
            Ok(keys) => keys,
            Err(_) => {
                self.set_operational_error(Some(CloudSyncErrorKind::SecretUnavailable));
                return Ok(None);
            }
        };
        let client = self.client_for(&endpoint)?;
        client.set_clock_offset_ms(state.clock_offset_ms);
        self.set_operational_error(None);
        Ok(Some(CloudAccess {
            store,
            state,
            client,
            keys,
        }))
    }

    async fn configured_access(&self) -> Result<CloudAccess, CloudRuntimeError> {
        self.reject_portable()?;
        self.active_access()
            .await?
            .ok_or(CloudRuntimeError::SetupRequired)
    }

    async fn queueable_store(&self) -> Result<Arc<MeetingStore>, CloudRuntimeError> {
        self.reject_portable()?;
        let cloud_settings = settings::get_settings(&self.app).cloud_sync;
        if !cloud_settings.enabled || !cloud_settings.has_current_consent() || cloud_settings.paused
        {
            return Err(CloudRuntimeError::SetupRequired);
        }
        self.meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)
    }

    fn reject_portable(&self) -> Result<(), CloudRuntimeError> {
        if portable::is_portable() {
            return Err(CloudRuntimeError::PortableUnavailable);
        }
        Ok(())
    }

    fn client_for(&self, endpoint: &str) -> Result<CloudClient, CloudRuntimeError> {
        let mut slot = self
            .client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = slot
            .as_ref()
            .filter(|existing| existing.endpoint == endpoint)
        {
            return Ok(existing.client.clone());
        }
        let client = CloudClient::new(endpoint).map_err(CloudRuntimeError::Client)?;
        *slot = Some(EndpointClient {
            endpoint: endpoint.to_owned(),
            client: client.clone(),
        });
        Ok(client)
    }

    async fn request_permitted(&self, state: &crate::meeting::store::CloudState) -> bool {
        if self.stopped.load(Ordering::Acquire)
            || portable::is_portable()
            || self.meetings.is_capture_active()
            || state.paused
        {
            return false;
        }
        let cloud_settings = settings::get_settings(&self.app).cloud_sync;
        if !cloud_settings.enabled || !cloud_settings.has_current_consent() || cloud_settings.paused
        {
            return false;
        }
        let Ok(Some(endpoint)) = cloud_settings.endpoint() else {
            return false;
        };
        if endpoint != state.endpoint {
            return false;
        }
        self.secrets.cloud_sync_keys().await.is_ok()
    }

    async fn require_request_permission(
        &self,
        state: &crate::meeting::store::CloudState,
    ) -> Result<(), CloudRuntimeError> {
        if self.request_permitted(state).await {
            Ok(())
        } else if portable::is_portable() {
            Err(CloudRuntimeError::PortableUnavailable)
        } else {
            Err(CloudRuntimeError::Deferred)
        }
    }

    fn persist_clock(&self, store: &MeetingStore, client: &CloudClient) {
        if let Some(observation) = client.latest_server_date() {
            let _ = store.set_cloud_clock_offset(observation.clock_offset_ms);
        }
    }

    fn persist_capabilities(
        &self,
        store: &MeetingStore,
        endpoint: &str,
        capabilities: &CloudCapabilities,
    ) -> Result<(), CloudRuntimeError> {
        let capabilities_json =
            serde_json::to_string(capabilities).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        store
            .upsert_cloud_capabilities(&CloudCapabilitiesCache {
                endpoint: endpoint.to_owned(),
                capabilities_json,
                fetched_at_utc_ms: utc_now_ms(),
            })
            .map_err(map_store_error)
    }

    async fn ensure_capabilities(&self, access: &CloudAccess) -> Result<bool, CloudRuntimeError> {
        if let Some(cache) = access
            .store
            .cloud_capabilities(&access.state.endpoint)
            .map_err(map_store_error)?
            .filter(|cache| {
                utc_now_ms().saturating_sub(cache.fetched_at_utc_ms) <= CAPABILITIES_CACHE_MS
            })
        {
            let capabilities: CloudCapabilities = serde_json::from_str(&cache.capabilities_json)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
            validate_capabilities(&capabilities)?;
            return Ok(true);
        }
        if !self.request_permitted(&access.state).await {
            return Ok(false);
        }
        let credentials = credentials(access)?;
        let result = access
            .client
            .capabilities(&credentials)
            .await
            .map_err(CloudRuntimeError::Client);
        self.persist_clock(&access.store, &access.client);
        let capabilities = result?;
        validate_capabilities(&capabilities)?;
        self.persist_capabilities(&access.store, &access.state.endpoint, &capabilities)?;
        Ok(true)
    }

    async fn queue_current_session(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<(), CloudRuntimeError> {
        let Some(access) = self.active_access().await? else {
            return Ok(());
        };
        if queue_session_upload(&access.store, session_id)? {
            self.emit_changed(Some(session_id), Some(CloudObjectState::Queued));
        }
        Ok(())
    }

    async fn drain_outbox(&self, access: &CloudAccess) -> Result<(), CloudRuntimeError> {
        let due = access
            .store
            .due_cloud_outbox(utc_now_ms(), 16)
            .map_err(map_store_error)?;
        for due_record in due {
            if !self.request_permitted(&access.state).await {
                return Ok(());
            }
            let claim_token = random_opaque_id()?;
            let Some(record) = access
                .store
                .claim_cloud_outbox(&due_record.outbox_id, &claim_token, utc_now_ms())
                .map_err(map_store_error)?
            else {
                continue;
            };
            if !self.request_permitted(&access.state).await {
                let _ = access.store.release_cloud_outbox_claim(
                    &record.outbox_id,
                    &claim_token,
                    utc_now_ms(),
                );
                return Ok(());
            }
            let result = match record.kind {
                CloudOutboxKind::Object => self.process_object(access, &record, &claim_token).await,
                CloudOutboxKind::Tombstone => {
                    self.process_tombstone(access, &record, &claim_token).await
                }
                CloudOutboxKind::Share => self.process_share(access, &record, &claim_token).await,
            };
            if let Err(error) = result {
                self.handle_outbox_failure(access, &record, &claim_token, error)
                    .await?;
            }
        }
        Ok(())
    }

    async fn process_object(
        &self,
        access: &CloudAccess,
        original: &CloudOutboxRecord,
        claim_token: &str,
    ) -> Result<(), CloudRuntimeError> {
        let mut record = original.clone();
        let revision_id = record
            .remote_revision_id
            .clone()
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        self.stage_object(&access.store, &access.state, &access.keys, &record)?;
        if !self.request_permitted(&access.state).await {
            return Err(CloudRuntimeError::Deferred);
        }
        if record.upload_id.is_none() {
            let upload_id = random_opaque_id()?;
            record = access
                .store
                .set_cloud_outbox_upload_id(&record.outbox_id, claim_token, upload_id)
                .map_err(map_store_error)?;
        }
        let payload = load_staged_payload(&access.store, &record)?;
        let upload_id = record
            .upload_id
            .clone()
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        let plan = self.object_upload_plan(access, &record, &payload, &upload_id)?;
        let credentials = credentials(access)?;
        let accepted;
        if original.upload_id.is_some() {
            self.require_request_permission(&access.state).await?;
            let status = access.client.upload_status(&credentials, &upload_id).await;
            self.persist_clock(&access.store, &access.client);
            match status {
                Ok(status) if status.upload_id == upload_id && status.state == "committed" => {
                    self.complete_object(
                        access,
                        &record,
                        claim_token,
                        status.committed_sequence.unwrap_or(0),
                    )?;
                    return Ok(());
                }
                Ok(status) if status.upload_id == upload_id && status.state == "active" => {
                    accepted = status.accepted_indexes;
                }
                Ok(_) => return Err(CloudRuntimeError::IntegrityFailure),
                Err(CloudClientError::Api(error)) if error.code == CloudErrorCode::NotFound => {
                    self.require_request_permission(&access.state).await?;
                    let created = access
                        .client
                        .create_object_upload(
                            &credentials,
                            &operation_key(&record, "create")?,
                            &plan,
                        )
                        .await
                        .map_err(CloudRuntimeError::Client);
                    self.persist_clock(&access.store, &access.client);
                    accepted = created?.accepted_indexes;
                }
                Err(error) => return Err(CloudRuntimeError::Client(error)),
            }
        } else {
            self.require_request_permission(&access.state).await?;
            let created = access
                .client
                .create_object_upload(&credentials, &operation_key(&record, "create")?, &plan)
                .await
                .map_err(CloudRuntimeError::Client);
            self.persist_clock(&access.store, &access.client);
            accepted = created?.accepted_indexes;
        }
        self.record_accepted_indexes(&access.store, &record, claim_token, &payload, &accepted)?;
        for chunk in access
            .store
            .missing_cloud_outbox_chunks(&record.outbox_id)
            .map_err(map_store_error)?
        {
            self.require_request_permission(&access.state).await?;
            let staged = payload
                .chunks
                .iter()
                .find(|staged| staged.index == chunk.chunk_index)
                .ok_or(CloudRuntimeError::IntegrityFailure)?;
            let response = access
                .client
                .upload_chunk(
                    &credentials,
                    &operation_key(&record, &format!("chunk-{}", chunk.chunk_index))?,
                    &upload_id,
                    chunk.chunk_index,
                    staged.bytes.clone(),
                )
                .await
                .map_err(CloudRuntimeError::Client);
            self.persist_clock(&access.store, &access.client);
            let response = response?;
            if response.upload_id != upload_id
                || response.index != chunk.chunk_index
                || !response.accepted
            {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
            access
                .store
                .mark_cloud_outbox_chunk_accepted(
                    &record.outbox_id,
                    chunk.chunk_index,
                    claim_token,
                    utc_now_ms(),
                )
                .map_err(map_store_error)?;
        }
        self.require_request_permission(&access.state).await?;
        let committed = access
            .client
            .commit_upload(&credentials, &operation_key(&record, "commit")?, &upload_id)
            .await
            .map_err(CloudRuntimeError::Client);
        self.persist_clock(&access.store, &access.client);
        let committed = committed?;
        if committed.upload_id != upload_id
            || committed.state != "committed"
            || committed.revision_id.as_deref() != Some(revision_id.as_str())
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        self.complete_object(
            access,
            &record,
            claim_token,
            committed
                .change_sequence
                .ok_or(CloudRuntimeError::IntegrityFailure)?,
        )
    }

    fn complete_object(
        &self,
        access: &CloudAccess,
        record: &CloudOutboxRecord,
        claim_token: &str,
        sequence: u64,
    ) -> Result<(), CloudRuntimeError> {
        let revision_id = record
            .remote_revision_id
            .clone()
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        access
            .store
            .upsert_cloud_head(&CloudHead {
                object_id: record.object_id.clone(),
                source_session_id: record.source_session_id,
                remote_revision_id: Some(revision_id.clone()),
                tombstone: false,
                acknowledged_revision_id: Some(revision_id.clone()),
                change_sequence: sequence,
            })
            .map_err(map_store_error)?;
        access
            .store
            .update_cloud_outbox(
                &record.outbox_id,
                claim_token,
                CloudOutboxUpdate {
                    state: CloudOutboxState::Completed,
                    remote_revision_id: Some(revision_id),
                    upload_id: record.upload_id.clone(),
                    terminal_error: None,
                },
            )
            .map_err(map_store_error)?;
        self.emit_changed(record.source_session_id, Some(CloudObjectState::Committed));
        Ok(())
    }

    async fn process_tombstone(
        &self,
        access: &CloudAccess,
        record: &CloudOutboxRecord,
        claim_token: &str,
    ) -> Result<(), CloudRuntimeError> {
        let Some(base_revision_id) = record.base_remote_revision_id.as_deref() else {
            access
                .store
                .update_cloud_outbox(
                    &record.outbox_id,
                    claim_token,
                    CloudOutboxUpdate {
                        state: CloudOutboxState::Completed,
                        remote_revision_id: record.remote_revision_id.clone(),
                        upload_id: None,
                        terminal_error: None,
                    },
                )
                .map_err(map_store_error)?;
            return Ok(());
        };
        let tombstone_revision_id = record
            .remote_revision_id
            .as_deref()
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        let tombstone_input = CanonicalTombstoneInput {
            vault_id: &access.state.vault_id,
            object_id: &record.object_id,
            tombstone_revision_id,
            base_revision_id,
            reason: "user_request",
            format_version: u64::from(PROTOCOL_VERSION),
        };
        let signature = sign_canonical_tombstone(&tombstone_input, &*access.keys.signing_seed)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        self.require_request_permission(&access.state).await?;
        let credentials = credentials(access)?;
        let response = access
            .client
            .tombstone_object(
                &credentials,
                &operation_key(record, "tombstone")?,
                &record.object_id,
                &TombstoneRequest {
                    tombstone_revision_id: tombstone_revision_id.to_owned(),
                    base_revision_id: base_revision_id.to_owned(),
                    format_version: PROTOCOL_VERSION,
                    reason: TombstoneReason::UserRequest,
                    writer_signature: base64_url_encode(&signature),
                },
            )
            .await
            .map_err(CloudRuntimeError::Client);
        self.persist_clock(&access.store, &access.client);
        let response = response?;
        if response.object_id != record.object_id
            || response.revision_id != tombstone_revision_id
            || !response.tombstone
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        access
            .store
            .upsert_cloud_head(&CloudHead {
                object_id: record.object_id.clone(),
                source_session_id: record.source_session_id,
                remote_revision_id: Some(response.revision_id.clone()),
                tombstone: true,
                acknowledged_revision_id: Some(response.revision_id.clone()),
                change_sequence: response.change_sequence,
            })
            .map_err(map_store_error)?;
        access
            .store
            .update_cloud_outbox(
                &record.outbox_id,
                claim_token,
                CloudOutboxUpdate {
                    state: CloudOutboxState::Completed,
                    remote_revision_id: Some(response.revision_id),
                    upload_id: None,
                    terminal_error: None,
                },
            )
            .map_err(map_store_error)?;
        self.emit_changed(record.source_session_id, Some(CloudObjectState::Deleted));
        Ok(())
    }

    async fn process_share(
        &self,
        access: &CloudAccess,
        original: &CloudOutboxRecord,
        claim_token: &str,
    ) -> Result<(), CloudRuntimeError> {
        let share = access
            .store
            .cloud_share_for_outbox(&original.outbox_id)
            .map_err(map_store_error)?
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        if share.state == CloudShareState::Revoked {
            self.require_request_permission(&access.state).await?;
            let credentials = credentials(access)?;
            let response = access
                .client
                .revoke_share(
                    &credentials,
                    &operation_key(original, "revoke")?,
                    &share.share_id,
                )
                .await
                .map_err(CloudRuntimeError::Client);
            self.persist_clock(&access.store, &access.client);
            let response = response?;
            if response.share_id != share.share_id || response.state != "revoked" {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
            access
                .store
                .update_cloud_outbox(
                    &original.outbox_id,
                    claim_token,
                    CloudOutboxUpdate {
                        state: CloudOutboxState::Completed,
                        remote_revision_id: None,
                        upload_id: original.upload_id.clone(),
                        terminal_error: None,
                    },
                )
                .map_err(map_store_error)?;
            self.emit_changed(share.source_session_id, Some(CloudObjectState::Deleted));
            return Ok(());
        }
        if share.state != CloudShareState::Pending || original.object_id != share.share_id {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let mut record = original.clone();
        self.stage_share(&access.store, &record, &share)?;
        if !self.request_permitted(&access.state).await {
            return Err(CloudRuntimeError::Deferred);
        }
        if record.upload_id.is_none() {
            record = access
                .store
                .set_cloud_outbox_upload_id(&record.outbox_id, claim_token, random_opaque_id()?)
                .map_err(map_store_error)?;
        }
        let payload = load_staged_payload(&access.store, &record)?;
        let upload_id = record
            .upload_id
            .clone()
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        let plan = self.share_upload_plan(access, &share, &payload, &upload_id)?;
        let credentials = credentials(access)?;
        let accepted;
        if original.upload_id.is_some() {
            self.require_request_permission(&access.state).await?;
            let status = access.client.upload_status(&credentials, &upload_id).await;
            self.persist_clock(&access.store, &access.client);
            match status {
                Ok(status) if status.upload_id == upload_id && status.state == "committed" => {
                    self.complete_share(access, &record, &share, claim_token)?;
                    return Ok(());
                }
                Ok(status) if status.upload_id == upload_id && status.state == "active" => {
                    accepted = status.accepted_indexes;
                }
                Ok(_) => return Err(CloudRuntimeError::IntegrityFailure),
                Err(CloudClientError::Api(error)) if error.code == CloudErrorCode::NotFound => {
                    self.require_request_permission(&access.state).await?;
                    let created = access
                        .client
                        .create_share_upload(
                            &credentials,
                            &operation_key(&record, "create")?,
                            &plan,
                        )
                        .await
                        .map_err(CloudRuntimeError::Client);
                    self.persist_clock(&access.store, &access.client);
                    accepted = created?.accepted_indexes;
                }
                Err(error) => return Err(CloudRuntimeError::Client(error)),
            }
        } else {
            self.require_request_permission(&access.state).await?;
            let created = access
                .client
                .create_share_upload(&credentials, &operation_key(&record, "create")?, &plan)
                .await
                .map_err(CloudRuntimeError::Client);
            self.persist_clock(&access.store, &access.client);
            let created = created?;
            if created.share_id.as_deref() != Some(share.share_id.as_str()) {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
            accepted = created.accepted_indexes;
        }
        self.record_accepted_indexes(&access.store, &record, claim_token, &payload, &accepted)?;
        for chunk in access
            .store
            .missing_cloud_outbox_chunks(&record.outbox_id)
            .map_err(map_store_error)?
        {
            self.require_request_permission(&access.state).await?;
            let staged = payload
                .chunks
                .iter()
                .find(|staged| staged.index == chunk.chunk_index)
                .ok_or(CloudRuntimeError::IntegrityFailure)?;
            let response = access
                .client
                .upload_chunk(
                    &credentials,
                    &operation_key(&record, &format!("chunk-{}", chunk.chunk_index))?,
                    &upload_id,
                    chunk.chunk_index,
                    staged.bytes.clone(),
                )
                .await
                .map_err(CloudRuntimeError::Client);
            self.persist_clock(&access.store, &access.client);
            let response = response?;
            if response.upload_id != upload_id
                || response.index != chunk.chunk_index
                || !response.accepted
            {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
            access
                .store
                .mark_cloud_outbox_chunk_accepted(
                    &record.outbox_id,
                    chunk.chunk_index,
                    claim_token,
                    utc_now_ms(),
                )
                .map_err(map_store_error)?;
        }
        self.require_request_permission(&access.state).await?;
        let committed = access
            .client
            .commit_upload(&credentials, &operation_key(&record, "commit")?, &upload_id)
            .await
            .map_err(CloudRuntimeError::Client);
        self.persist_clock(&access.store, &access.client);
        let committed = committed?;
        if committed.upload_id != upload_id
            || committed.state != "active"
            || committed.share_id.as_deref() != Some(share.share_id.as_str())
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        self.complete_share(access, &record, &share, claim_token)
    }

    fn complete_share(
        &self,
        access: &CloudAccess,
        record: &CloudOutboxRecord,
        share: &CloudShareRecord,
        claim_token: &str,
    ) -> Result<(), CloudRuntimeError> {
        access
            .store
            .update_cloud_share(
                &share.share_id,
                CloudShareUpdate {
                    expires_at_utc_ms: share.expires_at_utc_ms,
                    state: CloudShareState::Active,
                    outbox_id: Some(record.outbox_id.clone()),
                    revoked_at_utc_ms: None,
                },
            )
            .map_err(map_store_error)?;
        access
            .store
            .update_cloud_outbox(
                &record.outbox_id,
                claim_token,
                CloudOutboxUpdate {
                    state: CloudOutboxState::Completed,
                    remote_revision_id: None,
                    upload_id: record.upload_id.clone(),
                    terminal_error: None,
                },
            )
            .map_err(map_store_error)?;
        self.emit_changed(share.source_session_id, Some(CloudObjectState::Committed));
        Ok(())
    }

    fn record_accepted_indexes(
        &self,
        store: &MeetingStore,
        record: &CloudOutboxRecord,
        claim_token: &str,
        payload: &StagedPayload,
        accepted: &[u32],
    ) -> Result<(), CloudRuntimeError> {
        for index in accepted {
            if !payload.chunks.iter().any(|chunk| chunk.index == *index) {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
            store
                .mark_cloud_outbox_chunk_accepted(
                    &record.outbox_id,
                    *index,
                    claim_token,
                    utc_now_ms(),
                )
                .map_err(map_store_error)?;
        }
        Ok(())
    }

    async fn handle_outbox_failure(
        &self,
        access: &CloudAccess,
        record: &CloudOutboxRecord,
        claim_token: &str,
        error: CloudRuntimeError,
    ) -> Result<(), CloudRuntimeError> {
        if matches!(error, CloudRuntimeError::Deferred) {
            let _ = access.store.release_cloud_outbox_claim(
                &record.outbox_id,
                claim_token,
                utc_now_ms(),
            );
            return Ok(());
        }
        if let Some((retry_after, terminal)) = retry_directive(&error, record.attempt_count) {
            if let Some(next_attempt_utc_ms) = retry_after {
                access
                    .store
                    .retry_cloud_outbox(
                        &record.outbox_id,
                        claim_token,
                        "retry",
                        next_attempt_utc_ms,
                    )
                    .map_err(map_store_error)?;
                self.emit_changed(record.source_session_id, Some(CloudObjectState::Queued));
                return Ok(());
            }
            let terminal_error = terminal.unwrap_or("integrity_failure").to_owned();
            access
                .store
                .update_cloud_outbox(
                    &record.outbox_id,
                    claim_token,
                    CloudOutboxUpdate {
                        state: CloudOutboxState::Terminal,
                        remote_revision_id: record.remote_revision_id.clone(),
                        upload_id: record.upload_id.clone(),
                        terminal_error: Some(terminal_error.clone()),
                    },
                )
                .map_err(map_store_error)?;
            let state = terminal_object_state(&terminal_error);
            self.emit_changed(record.source_session_id, Some(state));
            return Ok(());
        }
        Err(CloudRuntimeError::IntegrityFailure)
    }

    fn stage_object(
        &self,
        store: &MeetingStore,
        state: &crate::meeting::store::CloudState,
        keys: &CloudSyncKeys,
        record: &CloudOutboxRecord,
    ) -> Result<(), CloudRuntimeError> {
        if !store
            .cloud_outbox_chunks(&record.outbox_id)
            .map_err(map_store_error)?
            .is_empty()
        {
            load_staged_payload(store, record)?;
            return Ok(());
        }
        let session_id = record
            .source_session_id
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        let revision_id = record
            .remote_revision_id
            .as_deref()
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        let bundle =
            CloudMeetingBundleV1::export_from_store(store, session_id).map_err(map_store_error)?;
        let mut plaintext = bundle.to_json_bytes().map_err(map_store_error)?;
        let result = (|| {
            if plaintext.is_empty() || plaintext.len() > MAX_BUNDLE_BYTES {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
            let chunk_count = u32::try_from(plaintext.chunks(MAX_PLAINTEXT_CHUNK_BYTES).len())
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
            let manifest_plaintext = serde_json::to_vec(&ObjectPayloadManifest {
                version: PROTOCOL_VERSION,
                kind: CAPABILITY_SHARE_KIND.to_owned(),
                source_format: OBJECT_SOURCE_FORMAT.to_owned(),
                chunk_count,
                plaintext_bytes: u64::try_from(plaintext.len())
                    .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
                plaintext_sha256: sha256_base64url(&plaintext),
            })
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
            let manifest_context = ObjectRevisionCryptoContext {
                vault_id: &state.vault_id,
                object_id: &record.object_id,
                revision_id,
                index: 0,
                total: u64::from(chunk_count),
                content_kind: ObjectContentKind::Manifest,
                source_format: OBJECT_SOURCE_FORMAT,
            };
            let manifest = seal_object_revision_payload(
                &*keys.vault_root,
                &manifest_context,
                &random_array::<12>()?,
                &manifest_plaintext,
            )
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
            let directory = store
                .cloud_outbox_payload_directory(&record.outbox_id)
                .map_err(map_store_error)?;
            write_staged_file(&directory, OBJECT_MANIFEST_FILE, &manifest)?;
            let mut chunks = Vec::with_capacity(
                usize::try_from(chunk_count).map_err(|_| CloudRuntimeError::IntegrityFailure)?,
            );
            for (index, plaintext_chunk) in plaintext.chunks(MAX_PLAINTEXT_CHUNK_BYTES).enumerate()
            {
                let index =
                    u32::try_from(index).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                let context = ObjectRevisionCryptoContext {
                    vault_id: &state.vault_id,
                    object_id: &record.object_id,
                    revision_id,
                    index: u64::from(index),
                    total: u64::from(chunk_count),
                    content_kind: ObjectContentKind::Chunk,
                    source_format: OBJECT_SOURCE_FORMAT,
                };
                let encrypted = seal_object_revision_payload(
                    &*keys.vault_root,
                    &context,
                    &random_array::<12>()?,
                    plaintext_chunk,
                )
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                write_staged_file(&directory, &chunk_file_name(index), &encrypted)?;
                chunks.push(CloudOutboxChunk {
                    chunk_index: index,
                    size_bytes: u64::try_from(encrypted.len())
                        .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
                    sha256: sha256_base64url(&encrypted),
                    accepted: false,
                });
            }
            store
                .stage_cloud_outbox_chunks(&record.outbox_id, &chunks)
                .map_err(map_store_error)?;
            Ok(())
        })();
        plaintext.zeroize();
        result
    }

    fn stage_share(
        &self,
        store: &MeetingStore,
        record: &CloudOutboxRecord,
        share: &CloudShareRecord,
    ) -> Result<([u8; 32], StagedPayload), CloudRuntimeError> {
        let root = fixed_array_32(
            base64_url_decode(&share.encrypted_link_material)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        )?;
        if share.outbox_id.as_deref() != Some(record.outbox_id.as_str())
            || record.share_content_kind != Some(share.content_kind)
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        if !store
            .cloud_outbox_chunks(&record.outbox_id)
            .map_err(map_store_error)?
            .is_empty()
        {
            return Ok((root, load_staged_payload(store, record)?));
        }
        let session_id = share
            .source_session_id
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        let (mut plaintext, manifest_plaintext) = match share.content_kind {
            CloudShareContentKind::CapabilityBundle => {
                let bundle = CloudMeetingBundleV1::export_from_store(store, session_id)
                    .map_err(map_store_error)?;
                let plaintext = bundle.to_json_bytes().map_err(map_store_error)?;
                let chunk_count = u32::try_from(plaintext.chunks(MAX_PLAINTEXT_CHUNK_BYTES).len())
                    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                let manifest = serde_json::to_vec(&CapabilityShareManifest {
                    version: PROTOCOL_VERSION,
                    kind: CAPABILITY_SHARE_KIND.to_owned(),
                    source_format: OBJECT_SOURCE_FORMAT.to_owned(),
                    chunk_count,
                    plaintext_bytes: u64::try_from(plaintext.len())
                        .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
                    plaintext_sha256: sha256_base64url(&plaintext),
                })
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                (plaintext, manifest)
            }
            CloudShareContentKind::BrowserMarkdown => {
                let review = store.review_snapshot(session_id).map_err(map_store_error)?;
                let (title, markdown) = strict_browser_markdown(&review)?;
                let chunk_count = u32::try_from(markdown.chunks(MAX_PLAINTEXT_CHUNK_BYTES).len())
                    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                let manifest = serde_json::to_vec(&BrowserShareManifest {
                    version: PROTOCOL_VERSION,
                    kind: BROWSER_SHARE_KIND.to_owned(),
                    source_format: BROWSER_SOURCE_FORMAT.to_owned(),
                    title,
                    chunk_count,
                    plaintext_bytes: u64::try_from(markdown.len())
                        .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
                })
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                (markdown, manifest)
            }
        };
        let result = (|| {
            if plaintext.is_empty() || plaintext.len() > MAX_SHARE_PLAINTEXT_BYTES {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
            let chunk_count = u32::try_from(plaintext.chunks(MAX_PLAINTEXT_CHUNK_BYTES).len())
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
            let manifest = seal_share_payload(
                &root,
                &SharePayloadContext {
                    share_id: &share.share_id,
                    index: 0,
                    total: u64::from(chunk_count),
                    domain: SharePayloadDomain::Manifest,
                },
                &random_array::<12>()?,
                &manifest_plaintext,
            )
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
            let directory = store
                .cloud_outbox_payload_directory(&record.outbox_id)
                .map_err(map_store_error)?;
            write_staged_file(&directory, OBJECT_MANIFEST_FILE, &manifest)?;
            let mut chunks = Vec::with_capacity(
                usize::try_from(chunk_count).map_err(|_| CloudRuntimeError::IntegrityFailure)?,
            );
            for (index, plaintext_chunk) in plaintext.chunks(MAX_PLAINTEXT_CHUNK_BYTES).enumerate()
            {
                let index =
                    u32::try_from(index).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                let encrypted = seal_share_payload(
                    &root,
                    &SharePayloadContext {
                        share_id: &share.share_id,
                        index: u64::from(index),
                        total: u64::from(chunk_count),
                        domain: SharePayloadDomain::Chunk,
                    },
                    &random_array::<12>()?,
                    plaintext_chunk,
                )
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                write_staged_file(&directory, &chunk_file_name(index), &encrypted)?;
                chunks.push(CloudOutboxChunk {
                    chunk_index: index,
                    size_bytes: u64::try_from(encrypted.len())
                        .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
                    sha256: sha256_base64url(&encrypted),
                    accepted: false,
                });
            }
            store
                .stage_cloud_outbox_chunks(&record.outbox_id, &chunks)
                .map_err(map_store_error)?;
            load_staged_payload(store, record)
        })();
        plaintext.zeroize();
        result.map(|payload| (root, payload))
    }

    fn object_upload_plan(
        &self,
        access: &CloudAccess,
        record: &CloudOutboxRecord,
        payload: &StagedPayload,
        upload_id: &str,
    ) -> Result<ObjectUploadPlan, CloudRuntimeError> {
        let revision_id = record
            .remote_revision_id
            .as_deref()
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        let chunks = upload_chunks(&payload.chunks)?;
        let signature_input = CanonicalUploadEnvelopeInput {
            vault_id: &access.state.vault_id,
            kind: UploadKind::Object,
            object_id: Some(&record.object_id),
            revision_id: Some(revision_id),
            base_revision_id: record.base_remote_revision_id.as_deref(),
            share_id: None,
            manifest_digest: &payload.manifest_sha256,
            crypto_version: u64::from(CRYPTO_VERSION),
            total_bytes: payload.total_bytes,
            chunks: &chunks,
        };
        let signature =
            sign_canonical_upload_envelope(&signature_input, &*access.keys.signing_seed)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        Ok(ObjectUploadPlan {
            version: PROTOCOL_VERSION,
            crypto_version: CRYPTO_VERSION,
            upload_id: upload_id.to_owned(),
            object_id: record.object_id.clone(),
            revision_id: revision_id.to_owned(),
            base_revision_id: record.base_remote_revision_id.clone(),
            manifest: base64_url_encode(&payload.manifest),
            manifest_sha256: payload.manifest_sha256.clone(),
            chunks: upload_chunk_plans(&payload.chunks)?,
            chunk_count: u32::try_from(payload.chunks.len())
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
            total_bytes: payload.total_bytes,
            writer_signature: base64_url_encode(&signature),
        })
    }

    fn share_writer_signature(
        &self,
        access: &CloudAccess,
        share: &CloudShareRecord,
        payload: &StagedPayload,
    ) -> Result<String, CloudRuntimeError> {
        let chunks = upload_chunks(&payload.chunks)?;
        let signature_input = CanonicalUploadEnvelopeInput {
            vault_id: &access.state.vault_id,
            kind: UploadKind::Share,
            object_id: None,
            revision_id: None,
            base_revision_id: None,
            share_id: Some(&share.share_id),
            manifest_digest: &payload.manifest_sha256,
            crypto_version: u64::from(CRYPTO_VERSION),
            total_bytes: payload.total_bytes,
            chunks: &chunks,
        };
        let signature =
            sign_canonical_upload_envelope(&signature_input, &*access.keys.signing_seed)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        Ok(base64_url_encode(&signature))
    }

    fn share_upload_plan(
        &self,
        access: &CloudAccess,
        share: &CloudShareRecord,
        payload: &StagedPayload,
        upload_id: &str,
    ) -> Result<ShareUploadPlan, CloudRuntimeError> {
        let writer_signature = self.share_writer_signature(access, share, payload)?;
        Ok(ShareUploadPlan {
            version: PROTOCOL_VERSION,
            crypto_version: CRYPTO_VERSION,
            upload_id: upload_id.to_owned(),
            share_id: share.share_id.clone(),
            expires_at: u64::try_from(share.expires_at_utc_ms)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
            manifest: base64_url_encode(&payload.manifest),
            manifest_sha256: payload.manifest_sha256.clone(),
            chunks: upload_chunk_plans(&payload.chunks)?,
            chunk_count: u32::try_from(payload.chunks.len())
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
            total_bytes: payload.total_bytes,
            writer_signature,
        })
    }

    async fn create_share_intent(
        &self,
        store: &MeetingStore,
        session_id: MeetingSessionId,
        expires_at_utc_ms: i64,
        content_kind: CloudShareContentKind,
    ) -> Result<NewShareIntent, CloudRuntimeError> {
        let access = self.configured_access().await?;
        if self.meetings.is_capture_active() {
            return Err(CloudRuntimeError::Deferred);
        }
        let now = adjusted_now_ms(access.state.clock_offset_ms);
        if expires_at_utc_ms <= now || expires_at_utc_ms > now.saturating_add(MAX_SHARE_EXPIRY_MS) {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let snapshot = store
            .session_snapshot(session_id)
            .map_err(map_store_error)?;
        if !matches!(
            snapshot.phase,
            MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired
        ) {
            return Err(CloudRuntimeError::Conflict);
        }
        let object_id = match store
            .cloud_head_for_session(session_id)
            .map_err(map_store_error)?
        {
            Some(head) => head.object_id,
            None => {
                let object_id = random_opaque_id()?;
                store
                    .upsert_cloud_head(&CloudHead {
                        object_id: object_id.clone(),
                        source_session_id: Some(session_id),
                        remote_revision_id: None,
                        tombstone: false,
                        acknowledged_revision_id: None,
                        change_sequence: 0,
                    })
                    .map_err(map_store_error)?;
                object_id
            }
        };
        let share_id = random_opaque_id()?;
        let root = random_array::<32>()?;
        let outbox = store
            .enqueue_cloud_outbox(CloudOutboxInput {
                kind: CloudOutboxKind::Share,
                object_id: share_id.clone(),
                source_session_id: Some(session_id),
                source_revision: Some(snapshot.revision),
                base_remote_revision_id: None,
                share_content_kind: Some(content_kind),
                remote_revision_id: None,
                idempotency_key: stable_idempotency_value(&["share", &share_id]),
                next_attempt_utc_ms: utc_now_ms(),
            })
            .map_err(map_store_error)?;
        let record = store
            .create_cloud_share(CloudShareInput {
                share_id,
                object_id,
                source_session_id: Some(session_id),
                expires_at_utc_ms,
                content_kind,
                encrypted_link_material: base64_url_encode(&root),
                outbox_id: Some(outbox.outbox_id.clone()),
            })
            .map_err(map_store_error)?;
        Ok(NewShareIntent {
            access,
            outbox,
            record,
        })
    }

    async fn pull_changes(&self, access: &CloudAccess) -> Result<(), CloudRuntimeError> {
        let mut cursor = access.state.cursor.clone();
        loop {
            self.require_request_permission(&access.state).await?;
            let credentials = credentials(access)?;
            let page = access
                .client
                .changes(&credentials, cursor.as_deref(), Some(100))
                .await;
            self.persist_clock(&access.store, &access.client);
            let page = match page {
                Ok(page) => page,
                Err(CloudClientError::Api(error))
                    if error.code == CloudErrorCode::CursorExpired =>
                {
                    self.pull_snapshot(access).await?;
                    return Ok(());
                }
                Err(error) => return Err(CloudRuntimeError::Client(error)),
            };
            for change in &page.changes {
                self.apply_remote_change(
                    access,
                    change.sequence,
                    &change.object_id,
                    &change.revision_id,
                    change.tombstone,
                )
                .await?;
            }
            let next_cursor = page.next_cursor;
            access
                .store
                .update_cloud_cursor(Some(next_cursor.clone()), None)
                .map_err(map_store_error)?;
            if !page.has_more {
                return Ok(());
            }
            cursor = Some(next_cursor);
        }
    }

    async fn pull_snapshot(&self, access: &CloudAccess) -> Result<(), CloudRuntimeError> {
        let mut high_water = access.state.snapshot_high_water.clone();
        let mut after = None;
        loop {
            self.require_request_permission(&access.state).await?;
            let credentials = credentials(access)?;
            let page = access
                .client
                .snapshot(
                    &credentials,
                    high_water.as_deref(),
                    after.as_deref(),
                    Some(100),
                )
                .await
                .map_err(CloudRuntimeError::Client);
            self.persist_clock(&access.store, &access.client);
            let page = page?;
            for head in &page.heads {
                self.apply_remote_change(
                    access,
                    head.sequence,
                    &head.object_id,
                    &head.revision_id,
                    head.tombstone,
                )
                .await?;
            }
            high_water = Some(page.high_water);
            after = page.after;
            access
                .store
                .update_cloud_cursor(None, high_water.clone())
                .map_err(map_store_error)?;
            if !page.has_more {
                access
                    .store
                    .update_cloud_cursor(None, None)
                    .map_err(map_store_error)?;
                return Ok(());
            }
        }
    }

    async fn apply_remote_change(
        &self,
        access: &CloudAccess,
        sequence: u64,
        object_id: &str,
        revision_id: &str,
        tombstone: bool,
    ) -> Result<(), CloudRuntimeError> {
        let current = access
            .store
            .cloud_head(object_id)
            .map_err(map_store_error)?;
        if current
            .as_ref()
            .is_some_and(|head| head.remote_revision_id.as_deref() == Some(revision_id))
        {
            let mut head = current.ok_or(CloudRuntimeError::IntegrityFailure)?;
            head.change_sequence = sequence;
            head.tombstone = tombstone;
            head.acknowledged_revision_id = Some(revision_id.to_owned());
            access
                .store
                .upsert_cloud_head(&head)
                .map_err(map_store_error)?;
            return Ok(());
        }
        if tombstone {
            if current.is_none() {
                access
                    .store
                    .upsert_cloud_head(&CloudHead {
                        object_id: object_id.to_owned(),
                        source_session_id: None,
                        remote_revision_id: Some(revision_id.to_owned()),
                        tombstone: true,
                        acknowledged_revision_id: Some(revision_id.to_owned()),
                        change_sequence: sequence,
                    })
                    .map_err(map_store_error)?;
            }
            return Ok(());
        }
        let installed = self
            .install_remote_revision(access, object_id, revision_id, sequence)
            .await;
        acknowledge_if_refused(&access.store, object_id, revision_id, sequence, installed)
    }

    /// Fetch a revision and install whatever it turns out to be.
    async fn install_remote_revision(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        sequence: u64,
    ) -> Result<(), CloudRuntimeError> {
        let object = self
            .fetch_remote_object(access, object_id, revision_id)
            .await?;
        match object {
            RemoteObject::Meeting(bundle) => {
                self.install_or_conflict(access, object_id, revision_id, sequence, *bundle)
                    .await
            }
            RemoteObject::DeviceRecording(recording) => {
                self.install_device_recording(access, object_id, revision_id, sequence, recording)
                    .await
            }
            RemoteObject::OwnDeviceRecording => {
                acknowledge_remote_revision(&access.store, object_id, revision_id, sequence)
            }
        }
    }

    async fn fetch_remote_object(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
    ) -> Result<RemoteObject, CloudRuntimeError> {
        self.require_request_permission(&access.state).await?;
        let credentials = credentials(access)?;
        let response = access
            .client
            .object_manifest(&credentials, object_id, revision_id)
            .await
            .map_err(CloudRuntimeError::Client);
        self.persist_clock(&access.store, &access.client);
        let response = response?;
        self.verify_and_decrypt_remote_object(access, object_id, revision_id, response)
            .await
    }

    /// Authenticate a revision and open it into whatever it turned out to be.
    ///
    /// The manifest is opened before any chunk is fetched, because what the
    /// object is decides how many bytes it may cost: a meeting bundle is a few
    /// megabytes of JSON, a paired device's recording is up to twelve hours of
    /// audio. Its AES-GCM AAD already binds the vault, the object, the revision
    /// and the payload domain, so opening it first reads exactly the bytes its
    /// writer sealed — and the writer's signature over the whole envelope is
    /// still verified below, before anything is installed.
    async fn verify_and_decrypt_remote_object(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        response: ObjectManifestResponse,
    ) -> Result<RemoteObject, CloudRuntimeError> {
        let envelope = response.envelope;
        if envelope.object_id != object_id
            || envelope.revision_id != revision_id
            || envelope.crypto_version != CRYPTO_VERSION
            || envelope.chunk_count == 0
            || envelope.chunk_count > 4096
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let encrypted_manifest = base64_url_decode(&response.manifest)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        if sha256_base64url(&encrypted_manifest) != envelope.manifest_sha256 {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        match open_remote_manifest(
            &*access.keys.vault_root,
            &access.state.vault_id,
            object_id,
            revision_id,
            envelope.chunk_count,
            &encrypted_manifest,
        )? {
            RemoteManifest::Meeting(manifest) => self
                .download_meeting_bundle(access, object_id, revision_id, &envelope, &manifest)
                .await
                .map(|bundle| RemoteObject::Meeting(Box::new(bundle))),
            RemoteManifest::DeviceRecording(manifest) => {
                // This Mac does not write device recordings, and importing one
                // it did write would file a second meeting over audio it
                // already has.
                if envelope.writer_device_id == access.state.device_id {
                    return Ok(RemoteObject::OwnDeviceRecording);
                }
                self.download_device_recording(access, object_id, revision_id, &envelope, manifest)
                    .await
                    .map(RemoteObject::DeviceRecording)
            }
        }
    }

    /// Fetch a meeting bundle's chunks, verify the writer signed the envelope
    /// they describe, then decrypt them into the one JSON document its manifest
    /// declared.
    async fn download_meeting_bundle(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        envelope: &ObjectRevisionEnvelope,
        manifest: &ObjectPayloadManifest,
    ) -> Result<CloudMeetingBundleV1, CloudRuntimeError> {
        if manifest.version != PROTOCOL_VERSION
            || manifest.chunk_count != envelope.chunk_count
            || envelope.total_bytes
                > u64::try_from(MAX_BUNDLE_BYTES + 1024)
                    .map_err(|_| CloudRuntimeError::IntegrityFailure)?
            || usize::try_from(manifest.plaintext_bytes)
                .ok()
                .filter(|length| *length <= MAX_BUNDLE_BYTES)
                .is_none()
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let signing_public_key = self.writer_signing_key(access, envelope).await?;
        let mut chunks = Vec::with_capacity(
            usize::try_from(envelope.chunk_count)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        );
        let descriptors = self
            .download_chunks(access, object_id, revision_id, envelope, |_, bytes| {
                chunks.push(bytes);
                Ok(())
            })
            .await?;
        verify_writer_signature(
            &access.state.vault_id,
            object_id,
            revision_id,
            envelope,
            &descriptors,
            &signing_public_key,
        )?;
        let mut plaintext = Vec::with_capacity(
            usize::try_from(manifest.plaintext_bytes)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        );
        for (index, chunk) in chunks.iter_mut().enumerate() {
            let index = u64::try_from(index).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
            let decoded = open_object_revision_payload(
                &*access.keys.vault_root,
                &ObjectRevisionCryptoContext {
                    vault_id: &access.state.vault_id,
                    object_id,
                    revision_id,
                    index,
                    total: u64::from(envelope.chunk_count),
                    content_kind: ObjectContentKind::Chunk,
                    source_format: OBJECT_SOURCE_FORMAT,
                },
                chunk,
            )
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
            plaintext.extend_from_slice(&decoded);
            chunk.zeroize();
        }
        if plaintext.len()
            != usize::try_from(manifest.plaintext_bytes)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?
            || sha256_base64url(&plaintext) != manifest.plaintext_sha256
        {
            plaintext.zeroize();
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let bundle = CloudMeetingBundleV1::from_json_bytes(&plaintext)
            .map_err(|_| CloudRuntimeError::IntegrityFailure);
        plaintext.zeroize();
        bundle
    }

    /// Decrypt a paired device's recording into a WAV file this process owns.
    ///
    /// The chunks are decrypted as they arrive, because twelve hours of audio
    /// does not belong in memory. Each one's AAD binds it to this vault, object,
    /// revision and index, so what lands on disk is the bytes its writer
    /// sealed; the signature over the envelope is checked before the file is
    /// handed back, and the caller is the only thing that can turn it into a
    /// meeting.
    ///
    /// A refusal anywhere in here is logged: a recording that never arrives is
    /// otherwise invisible, since the panel's error line reports the upload
    /// side only.
    async fn download_device_recording(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        envelope: &ObjectRevisionEnvelope,
        manifest: DeviceRecordingManifest,
    ) -> Result<DeviceRecordingImport, CloudRuntimeError> {
        let audio_bytes = declared_device_audio_bytes(&manifest, envelope).inspect_err(|_| {
            log::warn!("Cloud recording {object_id} declares audio this Mac will not install");
        })?;
        let signing_public_key = self.writer_signing_key(access, envelope).await?;
        // ponytail: a network, disk or deferral error at chunk n drops the
        // staging file and the next scan restarts from chunk 0. Resume by
        // keeping the staged file and its verified chunk count across scans,
        // keyed by object and revision id, when long recordings on flaky links
        // fail to complete.
        let mut staging = DeviceRecordingStaging::create(
            access
                .store
                .cloud_recording_staging_path(object_id)
                .map_err(map_store_error)?,
            audio_bytes,
            DeviceRecordingPayload {
                vault_root: &*access.keys.vault_root,
                vault_id: &access.state.vault_id,
                object_id,
                revision_id,
                chunk_count: u64::from(envelope.chunk_count),
            },
        )?;
        let descriptors = self
            .download_chunks(access, object_id, revision_id, envelope, |index, chunk| {
                staging.write_chunk(index, &chunk)
            })
            .await?;
        verify_writer_signature(
            &access.state.vault_id,
            object_id,
            revision_id,
            envelope,
            &descriptors,
            &signing_public_key,
        )?;
        let audio = staging.finish(&manifest.audio).inspect_err(|_| {
            log::warn!("Cloud recording {object_id} did not match its manifest");
        })?;
        Ok(DeviceRecordingImport {
            audio,
            title: manifest.title,
            recorded_at_utc_ms: manifest.recorded_at_utc_ms,
            device_id: manifest.device_id,
        })
    }

    /// Fetch a revision's chunks in order, check each against the digest the
    /// service returns as its ETag, and hand it to `receive`. Returns the
    /// `(index, size, digest)` descriptors the writer's signature covers.
    ///
    /// Nothing here decides what a chunk means: a meeting bundle keeps the
    /// ciphertext until its signature checks out, and a recording writes
    /// through to disk as it arrives.
    async fn download_chunks(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        envelope: &ObjectRevisionEnvelope,
        mut receive: impl FnMut(u64, Vec<u8>) -> Result<(), CloudRuntimeError>,
    ) -> Result<Vec<(u64, u64, String)>, CloudRuntimeError> {
        let credentials = credentials(access)?;
        let mut descriptors = Vec::with_capacity(
            usize::try_from(envelope.chunk_count)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        );
        let mut total_bytes = 0_u64;
        for index in 0..envelope.chunk_count {
            self.require_request_permission(&access.state).await?;
            let chunk = access
                .client
                .object_chunk(&credentials, object_id, revision_id, index)
                .await
                .map_err(CloudRuntimeError::Client);
            self.persist_clock(&access.store, &access.client);
            let chunk = chunk?;
            let digest = sha256_base64url(&chunk.bytes);
            let expected_etag = format!("\"{digest}\"");
            if chunk.etag.as_deref() != Some(expected_etag.as_str()) || chunk.bytes.len() < 28 {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
            let size = u64::try_from(chunk.bytes.len())
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
            total_bytes = total_bytes
                .checked_add(size)
                .ok_or(CloudRuntimeError::IntegrityFailure)?;
            // What the envelope declared is also the ceiling on what this loop
            // may read, so a revision that keeps handing out bytes stops here
            // rather than after it has spent all of them.
            if total_bytes > envelope.total_bytes {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
            descriptors.push((u64::from(index), size, digest));
            receive(u64::from(index), chunk.bytes)?;
        }
        if total_bytes != envelope.total_bytes {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        Ok(descriptors)
    }

    /// The signing key of the device that wrote this revision, refused unless
    /// the service still lists that device as an active member of the vault.
    async fn writer_signing_key(
        &self,
        access: &CloudAccess,
        envelope: &ObjectRevisionEnvelope,
    ) -> Result<[u8; 32], CloudRuntimeError> {
        self.require_request_permission(&access.state).await?;
        let credentials = credentials(access)?;
        let devices = access
            .client
            .devices(&credentials)
            .await
            .map_err(CloudRuntimeError::Client);
        self.persist_clock(&access.store, &access.client);
        let devices = devices?;
        let device = devices
            .devices
            .iter()
            .find(|device| {
                device.device_id == envelope.writer_device_id
                    && device.status == "active"
                    && device.revoked_at.is_none()
            })
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        fixed_array_32(
            base64_url_decode(&device.signing_public_key)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        )
    }

    async fn install_or_conflict(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        sequence: u64,
        bundle: CloudMeetingBundleV1,
    ) -> Result<(), CloudRuntimeError> {
        let existing_head = access
            .store
            .cloud_head(object_id)
            .map_err(map_store_error)?;
        let existing_session = access
            .store
            .session_snapshot(bundle.session.session_id)
            .ok();
        if existing_head.is_some() || existing_session.is_some() {
            let source_session_id = existing_head
                .as_ref()
                .and_then(|head| head.source_session_id)
                .or_else(|| existing_session.as_ref().map(|session| session.session_id));
            let source_revision = existing_session.as_ref().map(|session| session.revision);
            self.cache_conflict(
                &access.store,
                ConflictCacheInput {
                    object_id,
                    revision_id,
                    sequence,
                    source_session_id,
                    source_revision,
                    bundle: &bundle,
                },
            )?;
            self.emit_changed(source_session_id, Some(CloudObjectState::Conflict));
            return Ok(());
        }
        let snapshot = self
            .meetings
            .import_cloud_bundle(bundle)
            .await
            .map_err(map_import_error)?;
        access
            .store
            .upsert_cloud_head(&CloudHead {
                object_id: object_id.to_owned(),
                source_session_id: Some(snapshot.session_id),
                remote_revision_id: Some(revision_id.to_owned()),
                tombstone: false,
                acknowledged_revision_id: Some(revision_id.to_owned()),
                change_sequence: sequence,
            })
            .map_err(map_store_error)?;
        self.emit_changed(Some(snapshot.session_id), Some(CloudObjectState::Committed));
        Ok(())
    }

    /// File a paired device's recording as a meeting, through the same import
    /// a recording the operator picks off disk takes.
    ///
    /// The head is recorded the way a meeting object's install records it, with
    /// the new session as its source: that is what stops the next scan from
    /// importing the object again, and it is also what makes deleting the
    /// meeting later enqueue the tombstone that removes the object from the
    /// vault. The device keeps no second copy and never writes a second
    /// revision, so this meeting is the recording's only reader. It is not a
    /// writer either: `queue_session_upload` reads the recording's provenance
    /// off the consent row and never uploads this meeting onto the device's
    /// object.
    async fn install_device_recording(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        sequence: u64,
        recording: DeviceRecordingImport,
    ) -> Result<(), CloudRuntimeError> {
        if access
            .store
            .cloud_head(object_id)
            .map_err(map_store_error)?
            .is_some()
        {
            // A device that revised a recording it already delivered would
            // otherwise arrive as a second meeting over the same audio.
            log::warn!("Cloud recording {object_id} was revised after it was imported");
            return acknowledge_remote_revision(&access.store, object_id, revision_id, sequence);
        }
        let snapshot = self
            .meetings
            .import_recording(recording.request())
            .await
            .map_err(|error| {
                log::warn!("Cloud recording {object_id} would not import: {error:?}");
                map_import_error(error)
            })?;
        access
            .store
            .upsert_cloud_head(&CloudHead {
                object_id: object_id.to_owned(),
                source_session_id: Some(snapshot.session_id),
                remote_revision_id: Some(revision_id.to_owned()),
                tombstone: false,
                acknowledged_revision_id: Some(revision_id.to_owned()),
                change_sequence: sequence,
            })
            .map_err(map_store_error)?;
        self.emit_changed(Some(snapshot.session_id), Some(CloudObjectState::Committed));
        Ok(())
    }

    fn cache_conflict(
        &self,
        store: &MeetingStore,
        input: ConflictCacheInput<'_>,
    ) -> Result<(), CloudRuntimeError> {
        let path = store
            .cloud_conflict_staging_path(input.object_id)
            .map_err(map_store_error)?;
        let bytes = input.bundle.to_json_bytes().map_err(map_store_error)?;
        write_path_atomically(&path, &bytes)?;
        store
            .cache_cloud_conflict(&CloudConflict {
                object_id: input.object_id.to_owned(),
                source_session_id: input.source_session_id,
                source_revision: input.source_revision,
                remote_revision_id: input.revision_id.to_owned(),
                remote_sequence: input.sequence,
                remote_bundle_relative_path: format!(".cloud-conflicts/{}.bundle", input.object_id),
            })
            .map_err(map_store_error)
    }

    fn meeting_status_from_store(
        &self,
        store: &MeetingStore,
        session_id: MeetingSessionId,
    ) -> Result<CloudMeetingStatus, CloudRuntimeError> {
        let cloud_settings = settings::get_settings(&self.app).cloud_sync;
        let head = store
            .cloud_head_for_session(session_id)
            .map_err(map_store_error)?;
        let share_count = store
            .cloud_share_count_for_session(session_id)
            .map_err(map_store_error)?;
        let outboxes = store
            .cloud_outboxes_for_session(session_id)
            .map_err(map_store_error)?;
        let conflict = head
            .as_ref()
            .map(|head| store.cloud_conflict(&head.object_id))
            .transpose()
            .map_err(map_store_error)?
            .flatten();
        let state = if conflict.is_some() {
            CloudObjectState::Conflict
        } else if let Some(outbox) = outboxes
            .iter()
            .rev()
            .find(|outbox| outbox.state == CloudOutboxState::Terminal)
        {
            terminal_object_state(
                outbox
                    .terminal_error
                    .as_deref()
                    .unwrap_or("integrity_failure"),
            )
        } else if outboxes
            .iter()
            .any(|outbox| outbox.state == CloudOutboxState::Claimed)
        {
            CloudObjectState::Uploading
        } else if outboxes.iter().any(|outbox| {
            outbox.state == CloudOutboxState::Pending && outbox.kind == CloudOutboxKind::Tombstone
        }) {
            CloudObjectState::PendingDeletion
        } else if outboxes
            .iter()
            .any(|outbox| outbox.state == CloudOutboxState::Pending)
        {
            CloudObjectState::Queued
        } else if head.as_ref().is_some_and(|head| head.tombstone) {
            CloudObjectState::Deleted
        } else if head
            .as_ref()
            .and_then(|head| head.remote_revision_id.as_ref())
            .is_some()
        {
            CloudObjectState::Committed
        } else if cloud_settings.paused {
            CloudObjectState::Paused
        } else {
            CloudObjectState::Local
        };
        let retry_at_utc_ms = outboxes
            .iter()
            .filter(|outbox| outbox.state == CloudOutboxState::Pending)
            .map(|outbox| outbox.next_attempt_utc_ms)
            .min();
        Ok(CloudMeetingStatus {
            session_id,
            state,
            remote_revision_id: head.and_then(|head| head.remote_revision_id),
            retry_at_utc_ms,
            share_count,
        })
    }

    fn emit_changed(&self, session_id: Option<MeetingSessionId>, state: Option<CloudObjectState>) {
        let _ = self.app.emit(
            <CloudSyncChangedEvent as tauri_specta::Event>::NAME,
            CloudSyncChangedPayload {
                event_schema_version: CLOUD_SYNC_EVENT_SCHEMA_VERSION,
                session_id,
                state,
            },
        );
    }
}

struct NewShareIntent {
    access: CloudAccess,
    outbox: CloudOutboxRecord,
    record: CloudShareRecord,
}

fn credentials(access: &CloudAccess) -> Result<CloudCredentials<'_>, CloudRuntimeError> {
    CloudCredentials::new(
        &access.state.vault_id,
        &access.state.device_id,
        &access.keys.signing_seed,
    )
    .map_err(CloudRuntimeError::Client)
}

fn validate_capabilities(capabilities: &CloudCapabilities) -> Result<(), CloudRuntimeError> {
    if capabilities.protocol_version != PROTOCOL_VERSION
        || capabilities.crypto_version != CRYPTO_VERSION
        || capabilities.request_auth.algorithm != "Ed25519"
        || capabilities.request_auth.clock_skew_seconds != 300
        || capabilities.request_auth.nonce_retention_seconds != 600
        || capabilities.limits.chunk_bytes
            != u64::try_from(MAX_ENCRYPTED_CHUNK_BYTES)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?
        || capabilities.limits.chunks_per_upload != 4096
        || capabilities.limits.change_page > 100
        || capabilities.limits.snapshot_page > 100
    {
        return Err(CloudRuntimeError::UnsupportedProtocol);
    }
    Ok(())
}

fn canonical_endpoint(raw: &str) -> Result<String, CloudRuntimeError> {
    CloudSyncSettings {
        endpoint: Some(raw.to_owned()),
        ..CloudSyncSettings::default()
    }
    .endpoint()
    .map_err(|_| CloudRuntimeError::SetupRequired)?
    .ok_or(CloudRuntimeError::SetupRequired)
}

/// A settings write that never reached the disk is the same failure as a
/// meetings-store write that never did - see [`map_store_error`] - so cloud
/// sync reports it the same way instead of claiming the pairing landed.
impl From<crate::settings::SettingsPersistError> for CloudRuntimeError {
    fn from(_: crate::settings::SettingsPersistError) -> Self {
        Self::Storage
    }
}

fn map_store_error(_error: StoreError) -> CloudRuntimeError {
    CloudRuntimeError::Storage
}

fn random_opaque_id() -> Result<String, CloudRuntimeError> {
    Ok(base64_url_encode(&random_array::<24>()?))
}

fn random_array<const N: usize>() -> Result<[u8; N], CloudRuntimeError> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).map_err(|_| CloudRuntimeError::Randomness)?;
    Ok(bytes)
}

fn stable_idempotency_value(parts: &[&str]) -> String {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(part.as_bytes());
        bytes.push(0);
    }
    base64_url_encode(&sha256_digest(&bytes))
}

fn idempotency_key(parts: &[&str]) -> Result<IdempotencyKey, CloudRuntimeError> {
    IdempotencyKey::new(stable_idempotency_value(parts)).map_err(CloudRuntimeError::Client)
}

fn operation_key(
    record: &CloudOutboxRecord,
    operation: &str,
) -> Result<IdempotencyKey, CloudRuntimeError> {
    idempotency_key(&["outbox", &record.idempotency_key, operation])
}

/// A pasted offer decoded into the record its candidate signed, with the proof
/// checked over it.
struct VerifiedCandidate {
    record: Vec<u8>,
    pairing_public_key: [u8; 32],
    proof: [u8; 64],
}

/// Rebuild and check the record a candidate device signed.
///
/// Approval and the fingerprint the operator compares both read the record
/// from here, so the string on screen can only ever describe the offer approval
/// would act on.
fn verified_candidate(offer: &CloudPairingOffer) -> Result<VerifiedCandidate, CloudRuntimeError> {
    let signing_public_key = fixed_array_32(
        base64_url_decode(&offer.signing_public_key)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
    )?;
    let pairing_public_key = fixed_array_32(
        base64_url_decode(&offer.pairing_public_key)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
    )?;
    let pairing_nonce = fixed_array_16(
        base64_url_decode(&offer.pairing_nonce).map_err(|_| CloudRuntimeError::IntegrityFailure)?,
    )?;
    let proof = fixed_array_64(
        base64_url_decode(&offer.candidate_proof)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
    )?;
    let record = super::crypto::canonical_pair_candidate_bytes(&CanonicalPairCandidateInput {
        audience: PROTOCOL_AUDIENCE,
        vault_id: &offer.vault_id,
        candidate_device_id: &offer.device_id,
        candidate_signing_public_key: &signing_public_key,
        candidate_pairing_public_key: &pairing_public_key,
        pairing_nonce: &pairing_nonce,
        expires_at: u64::try_from(offer.expires_at_utc_ms)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
    })
    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    if !verify_ed25519(&signing_public_key, &proof, &record) {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    Ok(VerifiedCandidate {
        record,
        pairing_public_key,
        proof,
    })
}

/// The twelve characters a candidate device prints beside its offer.
fn candidate_fingerprint(candidate_record: &[u8]) -> Result<String, CloudRuntimeError> {
    let digest = base64_url_encode(&sha256_digest(candidate_record));
    Ok(digest
        .get(..12)
        .ok_or(CloudRuntimeError::IntegrityFailure)?
        .to_owned())
}

/// The fingerprint of an offer that arrived from somewhere else, derived from
/// the pasted record rather than read out of the paste: the operator is
/// comparing this against the device's own screen, and a value the paste
/// supplied would agree with a doctored paste.
pub(crate) fn pairing_offer_fingerprint(
    offer: &CloudPairingOffer,
) -> Result<String, CloudRuntimeError> {
    if offer.protocol_version != PROTOCOL_VERSION {
        return Err(CloudRuntimeError::UnsupportedProtocol);
    }
    candidate_fingerprint(&verified_candidate(offer)?.record)
}

/// Open a revision's manifest, the only place an object says what it is.
///
/// `source_format` is bound into the manifest's own key and AAD but never
/// travels on the wire, so the reader tries the two formats it installs and
/// lets AES-GCM answer: a payload opens under exactly the format its writer
/// sealed it with. The `kind` inside then has to agree with that format, so
/// neither half names the object on its own.
fn open_remote_manifest(
    vault_root: &[u8],
    vault_id: &str,
    object_id: &str,
    revision_id: &str,
    chunk_count: u32,
    encrypted_manifest: &[u8],
) -> Result<RemoteManifest, CloudRuntimeError> {
    let mut context = ObjectRevisionCryptoContext {
        vault_id,
        object_id,
        revision_id,
        index: 0,
        total: u64::from(chunk_count),
        content_kind: ObjectContentKind::Manifest,
        source_format: OBJECT_SOURCE_FORMAT,
    };
    if let Ok(mut plaintext) =
        open_object_revision_payload(vault_root, &context, encrypted_manifest)
    {
        let manifest = serde_json::from_slice::<ObjectPayloadManifest>(&plaintext)
            .map_err(|_| CloudRuntimeError::IntegrityFailure);
        plaintext.zeroize();
        let manifest = manifest?;
        if manifest.kind != CAPABILITY_SHARE_KIND || manifest.source_format != OBJECT_SOURCE_FORMAT
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        return Ok(RemoteManifest::Meeting(manifest));
    }
    context.source_format = DEVICE_RECORDING_SOURCE_FORMAT;
    let mut plaintext = open_object_revision_payload(vault_root, &context, encrypted_manifest)
        .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    let manifest = serde_json::from_slice::<DeviceRecordingManifest>(&plaintext)
        .map_err(|_| CloudRuntimeError::IntegrityFailure);
    plaintext.zeroize();
    let manifest = manifest?;
    if manifest.kind != DEVICE_RECORDING_KIND
        || manifest.format_version != DEVICE_RECORDING_FORMAT_VERSION
    {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    Ok(RemoteManifest::DeviceRecording(manifest))
}

/// The plaintext audio length a device recording declares, refused unless the
/// declaration is one this Mac can install.
///
/// The device slices its audio at `MAX_PLAINTEXT_CHUNK_BYTES` and every payload
/// costs a 12-byte nonce and a 16-byte tag, so the envelope's chunk count and
/// total are a function of that length. Checking them here refuses a truncated
/// or padded revision before a byte of it is fetched, and the ceiling keeps a
/// recording no importer would decode from being staged first.
///
/// The manifest is sealed under the vault root every member holds, so the
/// device it names is only a claim; the writer the service names is the one
/// the signature binds. The two have to agree, so that the origin the meeting
/// records is the device that wrote the bytes.
fn declared_device_audio_bytes(
    manifest: &DeviceRecordingManifest,
    envelope: &ObjectRevisionEnvelope,
) -> Result<u32, CloudRuntimeError> {
    let audio_bytes = manifest.audio.byte_length;
    let chunk_bytes = u64::try_from(MAX_PLAINTEXT_CHUNK_BYTES)
        .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    let chunk_count = audio_bytes.div_ceil(chunk_bytes).max(1);
    let expected_total = audio_bytes
        .checked_add(28 * chunk_count)
        .ok_or(CloudRuntimeError::IntegrityFailure)?;
    if manifest.audio.codec != DEVICE_RECORDING_CODEC
        || manifest.audio.sample_rate_hz != DEVICE_RECORDING_SAMPLE_RATE_HZ
        || manifest.audio.channels != DEVICE_RECORDING_CHANNELS
        || manifest.device_id != envelope.writer_device_id
        || audio_bytes == 0
        || !audio_bytes.is_multiple_of(DEVICE_RECORDING_FRAME_BYTES)
        || audio_bytes > MAX_DEVICE_RECORDING_AUDIO_BYTES
        || chunk_count != u64::from(envelope.chunk_count)
        || expected_total != envelope.total_bytes
    {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    u32::try_from(audio_bytes).map_err(|_| CloudRuntimeError::IntegrityFailure)
}

/// The RIFF/WAVE header for `audio_bytes` of the device capture format.
///
/// A device uploads bare samples and the importer decodes containers, so the
/// staged file is the samples behind this header. The two sizes are the bytes
/// after the first eight (36 plus the audio) and the `fmt ` chunk's own 16.
fn wave_header(audio_bytes: u32) -> [u8; WAVE_HEADER_BYTES] {
    let block_align = DEVICE_RECORDING_CHANNELS * (DEVICE_RECORDING_BITS_PER_SAMPLE / 8);
    let byte_rate = DEVICE_RECORDING_SAMPLE_RATE_HZ * u32::from(block_align);
    let mut header = [0_u8; WAVE_HEADER_BYTES];
    header[..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&audio_bytes.saturating_add(36).to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"fmt ");
    header[16..20].copy_from_slice(&16_u32.to_le_bytes());
    // Format tag 1 is uncompressed PCM.
    header[20..22].copy_from_slice(&1_u16.to_le_bytes());
    header[22..24].copy_from_slice(&DEVICE_RECORDING_CHANNELS.to_le_bytes());
    header[24..28].copy_from_slice(&DEVICE_RECORDING_SAMPLE_RATE_HZ.to_le_bytes());
    header[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    header[32..34].copy_from_slice(&block_align.to_le_bytes());
    header[34..36].copy_from_slice(&DEVICE_RECORDING_BITS_PER_SAMPLE.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..].copy_from_slice(&audio_bytes.to_le_bytes());
    header
}

/// Refuse a revision the writing device did not sign.
///
/// The payload keys come from the vault root, which every paired device holds,
/// so this signature is what says which of them wrote these bytes — and the
/// device list this key came from is what says that device is still in the
/// vault.
fn verify_writer_signature(
    vault_id: &str,
    object_id: &str,
    revision_id: &str,
    envelope: &ObjectRevisionEnvelope,
    descriptors: &[(u64, u64, String)],
    signing_public_key: &[u8; 32],
) -> Result<(), CloudRuntimeError> {
    let writer_signature = fixed_array_64(
        base64_url_decode(&envelope.writer_signature)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
    )?;
    let chunks = descriptors
        .iter()
        .map(|(index, size, digest)| UploadChunk {
            index: *index,
            size: *size,
            sha256: digest,
        })
        .collect::<Vec<_>>();
    let signed = CanonicalUploadEnvelopeInput {
        vault_id,
        kind: UploadKind::Object,
        object_id: Some(object_id),
        revision_id: Some(revision_id),
        base_revision_id: envelope.parent_revision_id.as_deref(),
        share_id: None,
        manifest_digest: &envelope.manifest_sha256,
        crypto_version: u64::from(envelope.crypto_version),
        total_bytes: envelope.total_bytes,
        chunks: &chunks,
    };
    if !verify_ed25519(
        signing_public_key,
        &writer_signature,
        &super::crypto::canonical_upload_envelope_bytes(&signed)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
    ) {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    Ok(())
}

fn pairing_envelope_material(
    pairing_secret: &[u8; 32],
    candidate_record: &[u8],
) -> ([u8; 32], [u8; 12]) {
    let mut material = Vec::with_capacity(pairing_secret.len() + candidate_record.len() + 32);
    material.extend_from_slice(b"sona-pairing-ephemeral-v1");
    material.extend_from_slice(pairing_secret);
    material.extend_from_slice(candidate_record);
    let ephemeral_secret_key = sha256_digest(&material);
    material.clear();
    material.extend_from_slice(b"sona-pairing-nonce-v1");
    material.extend_from_slice(pairing_secret);
    material.extend_from_slice(candidate_record);
    let nonce_digest = sha256_digest(&material);
    material.zeroize();
    let mut nonce = [0_u8; 12];
    nonce.copy_from_slice(&nonce_digest[..12]);
    (ephemeral_secret_key, nonce)
}

fn adjusted_now_ms(clock_offset_ms: i64) -> i64 {
    utc_now_ms().saturating_add(clock_offset_ms)
}

fn utc_now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Enqueue a reviewed meeting's current revision as an object upload. Returns
/// whether anything was queued.
///
/// A meeting over a paired device's recording never queues. Its head points
/// at the device's object, so this would upload the Mac's bundle as a second
/// revision of an object the device wrote in another format, and a second
/// Mac importing the same recording would then find its own head and file a
/// conflict on every phone recording. The consent row is where an import
/// says where its recording came from.
fn queue_session_upload(
    store: &MeetingStore,
    session_id: MeetingSessionId,
) -> Result<bool, CloudRuntimeError> {
    let snapshot = match store.session_snapshot(session_id) {
        Ok(snapshot) => snapshot,
        Err(StoreError::NotFound) => return Ok(false),
        Err(error) => return Err(map_store_error(error)),
    };
    if !matches!(
        snapshot.phase,
        MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired
    ) {
        return Ok(false);
    }
    if store
        .latest_consent_for_session(session_id)
        .map_err(map_store_error)?
        .is_some_and(|consent| {
            matches!(
                consent.provenance,
                MeetingConsentProvenance::PairedDevice { .. }
            )
        })
    {
        return Ok(false);
    }
    let existing_head = store
        .cloud_head_for_session(session_id)
        .map_err(map_store_error)?;
    let object_id = match existing_head.as_ref() {
        Some(head) if head.tombstone => return Ok(false),
        Some(head) => head.object_id.clone(),
        None => {
            let object_id = random_opaque_id()?;
            store
                .upsert_cloud_head(&CloudHead {
                    object_id: object_id.clone(),
                    source_session_id: Some(session_id),
                    remote_revision_id: None,
                    tombstone: false,
                    acknowledged_revision_id: None,
                    change_sequence: 0,
                })
                .map_err(map_store_error)?;
            object_id
        }
    };
    let mut base_remote_revision_id = existing_head
        .as_ref()
        .and_then(|head| head.remote_revision_id.clone());
    let outboxes = store
        .cloud_outboxes_for_session(session_id)
        .map_err(map_store_error)?;
    if let Some(previous) = outboxes.iter().rev().find(|outbox| {
        outbox.kind == CloudOutboxKind::Object
            && outbox.object_id == object_id
            && matches!(
                outbox.state,
                CloudOutboxState::Pending | CloudOutboxState::Claimed
            )
            && outbox.remote_revision_id.is_some()
    }) {
        base_remote_revision_id = previous.remote_revision_id.clone();
    }
    let revision = random_opaque_id()?;
    let idempotency_key =
        stable_idempotency_value(&["object", &object_id, &snapshot.revision.to_string()]);
    store
        .enqueue_cloud_outbox(CloudOutboxInput {
            kind: CloudOutboxKind::Object,
            object_id,
            source_session_id: Some(session_id),
            source_revision: Some(snapshot.revision),
            base_remote_revision_id,
            share_content_kind: None,
            remote_revision_id: Some(revision),
            idempotency_key,
            next_attempt_utc_ms: utc_now_ms(),
        })
        .map_err(map_store_error)?;
    Ok(true)
}

/// Record a revision as seen without installing it, keeping whatever session
/// the object already points at.
///
/// This is what a skipped object costs: one head write, so the next scan reads
/// the revision as already applied instead of fetching it again.
fn acknowledge_remote_revision(
    store: &MeetingStore,
    object_id: &str,
    revision_id: &str,
    sequence: u64,
) -> Result<(), CloudRuntimeError> {
    let source_session_id = store
        .cloud_head(object_id)
        .map_err(map_store_error)?
        .and_then(|head| head.source_session_id);
    store
        .upsert_cloud_head(&CloudHead {
            object_id: object_id.to_owned(),
            source_session_id,
            remote_revision_id: Some(revision_id.to_owned()),
            tombstone: false,
            acknowledged_revision_id: Some(revision_id.to_owned()),
            change_sequence: sequence,
        })
        .map_err(map_store_error)
}

/// Record a revision that was refused for what it is, and hand back a failure
/// that was this Mac's own.
///
/// A revision's bytes are immutable: the service serves each chunk under the
/// digest it was uploaded with. So a refusal of those bytes (a digest, length,
/// signature, writer, format or importer refusal, all `IntegrityFailure`) would
/// come out the same on every later fetch, and left unacknowledged it would
/// download the object again on every scan, up to twelve hours of audio a
/// time, and hold every later change in the vault behind it. A network, disk
/// or store failure, or a deferral, says nothing about the bytes and is
/// retried.
fn acknowledge_if_refused(
    store: &MeetingStore,
    object_id: &str,
    revision_id: &str,
    sequence: u64,
    installed: Result<(), CloudRuntimeError>,
) -> Result<(), CloudRuntimeError> {
    match installed {
        Err(CloudRuntimeError::IntegrityFailure) => {
            log::warn!(
                "Cloud object {object_id} revision {revision_id} was refused and will not be fetched again"
            );
            acknowledge_remote_revision(store, object_id, revision_id, sequence)
        }
        other => other,
    }
}

/// What an importer's refusal says about the revision that caused it: storage
/// trouble is this Mac's and is retried; anything else is about the bytes.
fn map_import_error(error: MeetingCommandError) -> CloudRuntimeError {
    match error {
        MeetingCommandError::StorageUnavailable => CloudRuntimeError::Storage,
        _ => CloudRuntimeError::IntegrityFailure,
    }
}

fn retry_directive(
    error: &CloudRuntimeError,
    attempt_count: u32,
) -> Option<(Option<i64>, Option<&'static str>)> {
    let now = utc_now_ms();
    match error {
        CloudRuntimeError::Client(CloudClientError::Api(api)) => match api.code {
            CloudErrorCode::ClockSkew => Some((Some(now), None)),
            CloudErrorCode::RateLimited | CloudErrorCode::DependencyUnavailable
                if api.retryable =>
            {
                let retry_after_ms = api
                    .retry_after
                    .and_then(|delay| i64::try_from(delay.as_millis()).ok());
                Some((
                    Some(now.saturating_add(backoff_ms(attempt_count, retry_after_ms))),
                    None,
                ))
            }
            CloudErrorCode::Unauthorized | CloudErrorCode::RevokedDevice => {
                Some((None, Some("auth_required")))
            }
            CloudErrorCode::QuotaExceeded => Some((None, Some("quota"))),
            CloudErrorCode::StaleRevision => Some((None, Some("conflict"))),
            CloudErrorCode::UnsupportedVersion => Some((None, Some("unsupported_protocol"))),
            CloudErrorCode::IntegrityFailed
            | CloudErrorCode::ChunkConflict
            | CloudErrorCode::IdempotencyConflict
            | CloudErrorCode::Replay => Some((None, Some("integrity_failure"))),
            _ => Some((None, Some("integrity_failure"))),
        },
        CloudRuntimeError::Client(_) | CloudRuntimeError::Deferred => Some((
            Some(now.saturating_add(backoff_ms(attempt_count, None))),
            None,
        )),
        CloudRuntimeError::Conflict => Some((None, Some("conflict"))),
        CloudRuntimeError::UnsupportedProtocol => Some((None, Some("unsupported_protocol"))),
        _ => Some((None, Some("integrity_failure"))),
    }
}

fn backoff_ms(attempt_count: u32, retry_after_ms: Option<i64>) -> i64 {
    let exponent = attempt_count.min(8);
    let seconds = 1_i64.checked_shl(exponent).unwrap_or(256);
    let bounded = seconds.saturating_mul(1000).min(MAX_RETRY_DELAY_MS);
    retry_after_ms
        .unwrap_or(0)
        .clamp(0, MAX_RETRY_DELAY_MS)
        .max(bounded)
}

fn terminal_error_kind(value: &str) -> Option<CloudSyncErrorKind> {
    match value {
        "auth_required" => Some(CloudSyncErrorKind::AuthRequired),
        "quota" => Some(CloudSyncErrorKind::Quota),
        "conflict" => Some(CloudSyncErrorKind::Conflict),
        "unsupported_protocol" => Some(CloudSyncErrorKind::UnsupportedProtocol),
        "integrity_failure" => Some(CloudSyncErrorKind::IntegrityFailure),
        _ => None,
    }
}

fn terminal_object_state(value: &str) -> CloudObjectState {
    match value {
        "auth_required" => CloudObjectState::AuthRequired,
        "quota" => CloudObjectState::Quota,
        "conflict" => CloudObjectState::Conflict,
        _ => CloudObjectState::IntegrityFailure,
    }
}

fn fixed_array_16(mut bytes: Vec<u8>) -> Result<[u8; 16], CloudRuntimeError> {
    let result = bytes
        .as_slice()
        .try_into()
        .map_err(|_| CloudRuntimeError::IntegrityFailure);
    bytes.zeroize();
    result
}

fn fixed_array_32(mut bytes: Vec<u8>) -> Result<[u8; 32], CloudRuntimeError> {
    let result = bytes
        .as_slice()
        .try_into()
        .map_err(|_| CloudRuntimeError::IntegrityFailure);
    bytes.zeroize();
    result
}

fn fixed_array_64(mut bytes: Vec<u8>) -> Result<[u8; 64], CloudRuntimeError> {
    let result = bytes
        .as_slice()
        .try_into()
        .map_err(|_| CloudRuntimeError::IntegrityFailure);
    bytes.zeroize();
    result
}

fn chunk_file_name(index: u32) -> String {
    format!("chunk-{index}.bin")
}

fn write_staged_file(
    directory: &Path,
    file_name: &str,
    bytes: &[u8],
) -> Result<(), CloudRuntimeError> {
    let destination = directory.join(file_name);
    let temporary = directory.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|_| CloudRuntimeError::File)?;
        file.write_all(bytes).map_err(|_| CloudRuntimeError::File)?;
        file.sync_all().map_err(|_| CloudRuntimeError::File)?;
        fs::rename(&temporary, &destination).map_err(|_| CloudRuntimeError::File)?;
        let _ = File::open(directory).and_then(|directory| directory.sync_all());
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_path_atomically(path: &Path, bytes: &[u8]) -> Result<(), CloudRuntimeError> {
    let parent = path.parent().ok_or(CloudRuntimeError::File)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or(CloudRuntimeError::File)?;
    write_staged_file(parent, file_name, bytes)
}

fn load_staged_payload(
    store: &MeetingStore,
    record: &CloudOutboxRecord,
) -> Result<StagedPayload, CloudRuntimeError> {
    let directory = store
        .cloud_outbox_payload_directory(&record.outbox_id)
        .map_err(map_store_error)?;
    let manifest =
        fs::read(directory.join(OBJECT_MANIFEST_FILE)).map_err(|_| CloudRuntimeError::File)?;
    if manifest.len() < 28 {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    let chunks = store
        .cloud_outbox_chunks(&record.outbox_id)
        .map_err(map_store_error)?;
    if chunks.is_empty() {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    let mut staged = Vec::with_capacity(chunks.len());
    let mut total_bytes = 0_u64;
    for metadata in chunks {
        let bytes = fs::read(directory.join(chunk_file_name(metadata.chunk_index)))
            .map_err(|_| CloudRuntimeError::File)?;
        let size = u64::try_from(bytes.len()).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        if size != metadata.size_bytes
            || sha256_base64url(&bytes) != metadata.sha256
            || bytes.len() < 28
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        total_bytes = total_bytes
            .checked_add(size)
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        staged.push(StagedChunk {
            index: metadata.chunk_index,
            size,
            sha256: metadata.sha256,
            bytes,
        });
    }
    Ok(StagedPayload {
        manifest_sha256: sha256_base64url(&manifest),
        manifest,
        chunks: staged,
        total_bytes,
    })
}

fn upload_chunks(chunks: &[StagedChunk]) -> Result<Vec<UploadChunk<'_>>, CloudRuntimeError> {
    let mut expected = 0_u32;
    let mut values = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if chunk.index != expected || chunk.size < 28 {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        expected = expected
            .checked_add(1)
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        values.push(UploadChunk {
            index: u64::from(chunk.index),
            size: chunk.size,
            sha256: &chunk.sha256,
        });
    }
    Ok(values)
}

fn upload_chunk_plans(chunks: &[StagedChunk]) -> Result<Vec<UploadChunkPlan>, CloudRuntimeError> {
    upload_chunks(chunks)?;
    chunks
        .iter()
        .map(|chunk| {
            Ok(UploadChunkPlan {
                index: chunk.index,
                size: chunk.size,
                sha256: chunk.sha256.clone(),
            })
        })
        .collect()
}

fn worker_share_transport(
    share: &CloudShareRecord,
    payload: &StagedPayload,
    writer_signature: &str,
) -> Result<Vec<u8>, CloudRuntimeError> {
    let chunks = payload
        .chunks
        .iter()
        .map(|chunk| {
            Ok(WorkerShareTransportChunk {
                index: chunk.index,
                size: u32::try_from(chunk.size).map_err(|_| CloudRuntimeError::IntegrityFailure)?,
                sha256: chunk.sha256.clone(),
            })
        })
        .collect::<Result<Vec<_>, CloudRuntimeError>>()?;
    let header = serde_json::to_vec(&WorkerShareTransport {
        format: "sona-encrypted-share-v1",
        version: PROTOCOL_VERSION,
        share: WorkerShareTransportMetadata {
            share_id: &share.share_id,
            crypto_version: CRYPTO_VERSION,
            manifest_sha256: &payload.manifest_sha256,
            chunk_count: u32::try_from(payload.chunks.len())
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
            total_bytes: payload.total_bytes,
            writer_signature,
        },
        manifest: base64_url_encode(&payload.manifest),
        chunks,
    })
    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    let mut transport = header;
    transport.push(b'\n');
    for chunk in &payload.chunks {
        let size =
            u32::try_from(chunk.bytes.len()).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        transport.extend_from_slice(&size.to_be_bytes());
        transport.extend_from_slice(&chunk.bytes);
    }
    Ok(transport)
}

fn decrypt_capability_bundle(
    file: &super::share_file::ValidatedShareFile,
) -> Result<CloudMeetingBundleV1, CloudRuntimeError> {
    let header = &file.header;
    if header.crypto_version != CRYPTO_VERSION
        || header.chunk_count == 0
        || usize::try_from(header.chunk_count).ok() != Some(file.ciphertext_frames.len())
    {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    let mut manifest_plaintext = open_share_payload(
        file.share_root(),
        &SharePayloadContext {
            share_id: &header.share_id,
            index: 0,
            total: u64::from(header.chunk_count),
            domain: SharePayloadDomain::Manifest,
        },
        &header.manifest_ciphertext,
    )
    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    let manifest = serde_json::from_slice::<CapabilityShareManifest>(&manifest_plaintext)
        .map_err(|_| CloudRuntimeError::IntegrityFailure);
    manifest_plaintext.zeroize();
    let manifest = manifest?;
    if manifest.version != PROTOCOL_VERSION
        || manifest.kind != CAPABILITY_SHARE_KIND
        || manifest.source_format != OBJECT_SOURCE_FORMAT
        || manifest.chunk_count != header.chunk_count
        || usize::try_from(manifest.plaintext_bytes)
            .ok()
            .filter(|length| *length <= MAX_BUNDLE_BYTES)
            .is_none()
    {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    let mut plaintext = Vec::with_capacity(
        usize::try_from(manifest.plaintext_bytes)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
    );
    for (index, ciphertext) in file.ciphertext_frames.iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let decoded = open_share_payload(
            file.share_root(),
            &SharePayloadContext {
                share_id: &header.share_id,
                index: u64::from(index),
                total: u64::from(header.chunk_count),
                domain: SharePayloadDomain::Chunk,
            },
            ciphertext,
        )
        .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        plaintext.extend_from_slice(&decoded);
    }
    if plaintext.len()
        != usize::try_from(manifest.plaintext_bytes)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?
        || sha256_base64url(&plaintext) != manifest.plaintext_sha256
    {
        plaintext.zeroize();
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    let bundle = CloudMeetingBundleV1::from_json_bytes(&plaintext)
        .map_err(|_| CloudRuntimeError::IntegrityFailure);
    plaintext.zeroize();
    bundle
}

fn strict_browser_markdown(
    review: &MeetingReviewSnapshot,
) -> Result<(String, Vec<u8>), CloudRuntimeError> {
    let title = strict_text(&review.session.title);
    if title.is_empty() || title.len() > 240 {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    let speakers = review
        .speakers
        .iter()
        .map(|speaker| (speaker.speaker_id, strict_text(&speaker.display_name)))
        .collect::<HashMap<_, _>>();
    let mut markdown = String::new();
    markdown.push_str("# ");
    markdown.push_str(&title);
    markdown.push_str("\n\n## Transcript\n");
    let mut has_transcript = false;
    for segment in review.transcript.iter().filter(|segment| !segment.removed) {
        has_transcript = true;
        let text = segment
            .replacement_text
            .as_deref()
            .unwrap_or(segment.base.text.as_str());
        let speaker = speakers
            .get(&segment.assigned_speaker_id)
            .map(String::as_str)
            .unwrap_or("Unknown speaker");
        markdown.push_str("- ");
        markdown.push_str(speaker);
        markdown.push_str(": ");
        markdown.push_str(&strict_text(text));
        markdown.push('\n');
    }
    if !has_transcript {
        markdown.push_str("No transcript is available.\n");
    }
    markdown.push_str("\n## Notes\n");
    if review.notes.is_empty() {
        markdown.push_str("No manual notes.\n");
    } else {
        for note in &review.notes {
            markdown.push_str("- ");
            markdown.push_str(&strict_text(&note.body));
            markdown.push('\n');
        }
    }
    if markdown.len() > MAX_BUNDLE_BYTES {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    Ok((title, markdown.into_bytes()))
}

fn strict_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.replace(['\r', '\n'], " ").chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '!' | '|' => {
                escaped.push('\\');
                escaped.push(character);
            }
            _ => escaped.push(character),
        }
    }
    escaped.trim().to_owned()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{MemorySecretBackend, SecretErrorKind};

    #[test]
    fn unconfigured_background_sync_has_no_scan_deadline() {
        let gate = Arc::new(BackgroundScanGate::default());
        let stopped = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker_gate = Arc::clone(&gate);
        let worker_stopped = Arc::clone(&stopped);
        let worker = thread::spawn(move || {
            while let Some(wake) = worker_gate.wait(&worker_stopped, Duration::from_secs(1)) {
                sender.send(wake).expect("scan receiver remains");
            }
        });

        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        gate.set_configured(true);
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("configuration wakes the scanner"),
            BackgroundScanWake::Configured
        );
        gate.set_configured(false);
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));

        stopped.store(true, Ordering::Release);
        gate.wake();
        worker.join().expect("scan gate worker");
    }

    #[test]
    fn unavailable_keychain_is_visible_only_while_sync_can_run() {
        let backend = Arc::new(MemorySecretBackend::new());
        let secrets = SecretManager::with_backend(backend.clone());
        backend.fail_next(SecretErrorKind::Unavailable);

        let observed_error = tauri::async_runtime::block_on(secrets.cloud_sync_keys())
            .err()
            .map(|_| CloudSyncErrorKind::SecretUnavailable);
        assert_eq!(observed_error, Some(CloudSyncErrorKind::SecretUnavailable));
        assert!(!backend.has("cloud_sync/vault-root-v1"));
        assert!(!backend.has("cloud_sync/signing-seed-v1"));
        assert!(!backend.has("cloud_sync/pairing-secret-v1"));

        let mut settings = CloudSyncSettings {
            enabled: true,
            paused: false,
            consent_version: Some(CLOUD_SYNC_CONSENT_VERSION),
            endpoint: Some("https://sync.example.test/v1".to_owned()),
        };
        let running = SyncGate {
            portable: false,
            stopped: false,
            capturing: false,
        };
        let status_error = |settings: &CloudSyncSettings, gate: SyncGate| {
            active_status_error(settings, gate, observed_error)
        };
        assert_eq!(status_error(&settings, running), observed_error);

        settings.paused = true;
        assert_eq!(status_error(&settings, running), None);
        settings.paused = false;
        settings.enabled = false;
        assert_eq!(status_error(&settings, running), None);
        settings.enabled = true;
        settings.consent_version = None;
        assert_eq!(status_error(&settings, running), None);
        settings.consent_version = Some(CLOUD_SYNC_CONSENT_VERSION);

        let mut gate = running;
        gate.portable = true;
        assert_eq!(status_error(&settings, gate), None);
        gate = running;
        gate.stopped = true;
        assert_eq!(status_error(&settings, gate), None);
        gate = running;
        gate.capturing = true;
        assert_eq!(status_error(&settings, gate), None);
    }

    fn browser_share_payload(root: &[u8; 32]) -> (CloudShareRecord, StagedPayload) {
        let share = CloudShareRecord {
            share_id: "shareid123456789".to_owned(),
            object_id: "objectid12345678".to_owned(),
            source_session_id: None,
            expires_at_utc_ms: 1,
            state: CloudShareState::Active,
            content_kind: CloudShareContentKind::BrowserMarkdown,
            encrypted_link_material: base64_url_encode(root),
            outbox_id: None,
            revoked_at_utc_ms: None,
        };
        let manifest = serde_json::to_vec(&BrowserShareManifest {
            version: PROTOCOL_VERSION,
            kind: BROWSER_SHARE_KIND.to_owned(),
            source_format: BROWSER_SOURCE_FORMAT.to_owned(),
            title: "Safe title".to_owned(),
            chunk_count: 1,
            plaintext_bytes: 8,
        })
        .expect("browser manifest");
        let manifest = seal_share_payload(
            root,
            &SharePayloadContext {
                share_id: &share.share_id,
                index: 0,
                total: 1,
                domain: SharePayloadDomain::Manifest,
            },
            &[1; 12],
            &manifest,
        )
        .expect("seal browser manifest");
        let bytes = seal_share_payload(
            root,
            &SharePayloadContext {
                share_id: &share.share_id,
                index: 0,
                total: 1,
                domain: SharePayloadDomain::Chunk,
            },
            &[2; 12],
            b"# safe\n",
        )
        .expect("seal browser chunk");
        let size = u64::try_from(bytes.len()).expect("chunk size");
        (
            share,
            StagedPayload {
                manifest_sha256: sha256_base64url(&manifest),
                manifest,
                chunks: vec![StagedChunk {
                    index: 0,
                    size,
                    sha256: sha256_base64url(&bytes),
                    bytes,
                }],
                total_bytes: size,
            },
        )
    }

    #[test]
    fn browser_payload_cannot_cross_into_capability_import() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = [7; 32];
        let (share, payload) = browser_share_payload(&root);
        let transport = worker_share_transport(&share, &payload, &base64_url_encode(&[3; 64]))
            .expect("worker transport");
        let worker = parse_worker_share_transport(transport).expect("validated worker transport");
        let path = directory.path().join("browser.sona");
        write_share_file(&path, &root, &worker.header, &worker.ciphertext_frames)
            .expect("capability write");
        let file = read_share_file(&path).expect("capability read");
        assert!(matches!(
            decrypt_capability_bundle(&file),
            Err(CloudRuntimeError::IntegrityFailure)
        ));
    }

    #[test]
    fn retries_are_bounded_and_honor_retry_after() {
        assert_eq!(backoff_ms(0, None), 1_000);
        assert_eq!(backoff_ms(20, Some(600_000)), MAX_RETRY_DELAY_MS);
        assert_eq!(backoff_ms(2, Some(5_000)), 5_000);
    }

    #[test]
    fn operation_idempotency_is_stable_per_mutation_target() {
        let record = CloudOutboxRecord {
            outbox_id: Uuid::new_v4().to_string(),
            kind: CloudOutboxKind::Object,
            object_id: "objectid12345678".to_owned(),
            source_session_id: None,
            source_revision: None,
            base_remote_revision_id: None,
            share_content_kind: None,
            remote_revision_id: Some("revision123456789".to_owned()),
            upload_id: None,
            idempotency_key: "idempotencykey123".to_owned(),
            state: CloudOutboxState::Pending,
            attempt_count: 0,
            next_attempt_utc_ms: 0,
            terminal_error: None,
            payload_relative_dir: ".cloud-outbox/test".to_owned(),
            claim_token: None,
        };
        let first = operation_key(&record, "commit").expect("first key");
        let second = operation_key(&record, "commit").expect("second key");
        let chunk = operation_key(&record, "chunk-0").expect("chunk key");
        assert_eq!(first.as_str(), second.as_str());
        assert_ne!(first.as_str(), chunk.as_str());
    }

    #[test]
    fn strict_markdown_escapes_markup_and_newlines() {
        let text = strict_text("<script>alert(1)</script>\n# heading");
        assert!(!text.contains('<'));
        assert!(!text.contains('>'));
        assert!(!text.contains('\n'));
        assert!(text.contains("&lt;script&gt;"));
    }

    /* A phone's object, built the way `mobile/Shared/DeviceRecordingObject.swift`
     * and `mobile/Shared/SonaCrypto.swift` build one: the manifest JSON with
     * sorted keys, sealed under the device source format, and the PCM sliced at
     * the plaintext chunk ceiling. Nothing here goes through the HTTP client,
     * which is what the pull path adds around this and is unchanged. */

    const TEST_VAULT_ROOT: [u8; 32] = [7; 32];
    const TEST_VAULT_ID: &str = "vaultid123456789";
    const TEST_OBJECT_ID: &str = "objectid12345678";
    const TEST_REVISION_ID: &str = "revisionid123456";
    const TEST_DEVICE_ID: &str = "phonedeviceid123";

    struct PhoneObject {
        sealed_manifest: Vec<u8>,
        sealed_chunks: Vec<Vec<u8>>,
        envelope: ObjectRevisionEnvelope,
    }

    /// Audio long enough to cross the chunk ceiling, so the object has the two
    /// chunks a real recording has.
    fn phone_audio() -> Vec<u8> {
        (0..MAX_PLAINTEXT_CHUNK_BYTES + 3_200)
            .map(|index| u8::try_from(index % 251).expect("byte"))
            .collect()
    }

    fn seal(index: u64, total: u64, kind: ObjectContentKind, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce = [0_u8; 12];
        nonce[0] = u8::try_from(index % 251).expect("byte");
        nonce[1] = u8::from(kind == ObjectContentKind::Manifest);
        seal_object_revision_payload(
            &TEST_VAULT_ROOT,
            &ObjectRevisionCryptoContext {
                vault_id: TEST_VAULT_ID,
                object_id: TEST_OBJECT_ID,
                revision_id: TEST_REVISION_ID,
                index,
                total,
                content_kind: kind,
                source_format: DEVICE_RECORDING_SOURCE_FORMAT,
            },
            &nonce,
            plaintext,
        )
        .expect("the device seals its payloads")
    }

    fn phone_object(audio: Vec<u8>, declared_sha256: &str, writer_device_id: &str) -> PhoneObject {
        let chunks: Vec<Vec<u8>> = audio
            .chunks(MAX_PLAINTEXT_CHUNK_BYTES)
            .map(Vec::from)
            .collect();
        let total = u64::try_from(chunks.len()).expect("chunk count");
        // Sorted keys, exactly as `DeviceRecordingObject.encodeManifest` emits.
        // A `\` at a line end eats the newline and the indent after it, so this
        // literal is one line of JSON with no whitespace in it.
        let manifest = format!(
            "{{\"audio\":{{\"byte_length\":{byte_length},\"channels\":1,\
             \"codec\":\"pcm_s16le\",\"sample_rate_hz\":16000,\
             \"sha256\":\"{declared_sha256}\"}},\
             \"device_id\":\"{writer_device_id}\",\"duration_ms\":{duration_ms},\
             \"format_version\":1,\"kind\":\"device_recording\",\
             \"recorded_at_utc_ms\":1788305031276,\"title\":\"Phone recording\"}}",
            byte_length = audio.len(),
            // 16 kHz mono s16le is 32 bytes a millisecond.
            duration_ms = audio.len() / 32,
        );
        let sealed_manifest = seal(0, total, ObjectContentKind::Manifest, manifest.as_bytes());
        let sealed_chunks: Vec<Vec<u8>> = chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| {
                seal(
                    u64::try_from(index).expect("index"),
                    total,
                    ObjectContentKind::Chunk,
                    chunk,
                )
            })
            .collect();
        let envelope = ObjectRevisionEnvelope {
            object_id: TEST_OBJECT_ID.to_owned(),
            revision_id: TEST_REVISION_ID.to_owned(),
            parent_revision_id: None,
            manifest_sha256: sha256_base64url(&sealed_manifest),
            chunk_count: u32::try_from(sealed_chunks.len()).expect("chunk count"),
            total_bytes: sealed_chunks
                .iter()
                .map(|chunk| u64::try_from(chunk.len()).expect("chunk size"))
                .sum(),
            crypto_version: CRYPTO_VERSION,
            writer_device_id: writer_device_id.to_owned(),
            writer_signature: base64_url_encode(&[0; 64]),
        };
        PhoneObject {
            sealed_manifest,
            sealed_chunks,
            envelope,
        }
    }

    /// Everything the pull path does between the last byte off the wire and the
    /// importer: open the manifest, check it against the envelope, decrypt the
    /// chunks into the staged WAV, and refuse audio that is not what was
    /// declared.
    fn stage(object: &PhoneObject, path: PathBuf) -> Result<ScratchAudioFile, CloudRuntimeError> {
        let manifest = match open_remote_manifest(
            &TEST_VAULT_ROOT,
            TEST_VAULT_ID,
            TEST_OBJECT_ID,
            TEST_REVISION_ID,
            object.envelope.chunk_count,
            &object.sealed_manifest,
        )? {
            RemoteManifest::DeviceRecording(manifest) => manifest,
            RemoteManifest::Meeting(_) => panic!("a device recording read as a meeting bundle"),
        };
        let audio_bytes = declared_device_audio_bytes(&manifest, &object.envelope)?;
        let mut staging = DeviceRecordingStaging::create(
            path,
            audio_bytes,
            DeviceRecordingPayload {
                vault_root: &TEST_VAULT_ROOT,
                vault_id: TEST_VAULT_ID,
                object_id: TEST_OBJECT_ID,
                revision_id: TEST_REVISION_ID,
                chunk_count: u64::from(object.envelope.chunk_count),
            },
        )?;
        for (index, chunk) in object.sealed_chunks.iter().enumerate() {
            staging.write_chunk(u64::try_from(index).expect("index"), chunk)?;
        }
        staging.finish(&manifest.audio)
    }

    /// The whole point of the glue: what the phone uploaded arrives as an
    /// ordinary meeting, through the same import a file the operator picks
    /// takes, with the device recorded as the track's origin.
    #[test]
    fn a_phone_recording_becomes_a_meeting() {
        let (_files, manager) = crate::meeting::session::tests::importing_manager();
        let store = tauri::async_runtime::block_on(manager.store()).expect("the store mounts");
        let audio = phone_audio();
        let object = phone_object(audio.clone(), &sha256_base64url(&audio), TEST_DEVICE_ID);
        // The scratch path the pull path stages into: inside the store's own
        // private root, not a shared temporary directory.
        let staged = stage(
            &object,
            store
                .cloud_recording_staging_path(TEST_OBJECT_ID)
                .expect("the store owns a staging path"),
        )
        .expect("the object stages");
        assert!(staged.path().starts_with(store.root()));

        // The staged file is the phone's samples behind a header the decoder
        // reads, byte for byte.
        let bytes = fs::read(staged.path()).expect("the staged file is readable");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(bytes.len(), WAVE_HEADER_BYTES + audio.len());
        assert_eq!(&bytes[WAVE_HEADER_BYTES..], audio.as_slice());

        let snapshot =
            tauri::async_runtime::block_on(manager.import_recording(ImportRecordingRequest {
                path: staged.path().to_owned(),
                title: Some("Phone recording".to_owned()),
                recorded_at_utc_ms: Some(1_788_305_031_276),
                origin: RecordingOrigin::PairedDevice {
                    device_id: TEST_DEVICE_ID.to_owned(),
                },
            }))
            .expect("the pulled recording imports");
        let review = tauri::async_runtime::block_on(manager.get(snapshot.session_id))
            .expect("the imported meeting is readable");

        assert_eq!(review.session.phase, MeetingPhase::ReviewReady);
        assert_eq!(review.session.title, "Phone recording");
        assert_eq!(review.session.started_at_utc_ms, Some(1_788_305_031_276));
        assert!(!review.transcript.is_empty());
        assert!(review.tracks[0].durable_record_count > 0);
    }

    /// A phone may upload a recording with no title. The staged file is named
    /// for the object, so the importer's own fallback to the file stem would
    /// title the meeting with an opaque id; the request asks instead for the
    /// placeholder a meeting carries until its notes name it.
    #[test]
    fn an_untitled_phone_recording_is_not_titled_with_its_object_id() {
        let (_files, manager) = crate::meeting::session::tests::importing_manager();
        let store = tauri::async_runtime::block_on(manager.store()).expect("the store mounts");
        let audio = phone_audio();
        let object = phone_object(audio.clone(), &sha256_base64url(&audio), TEST_DEVICE_ID);
        let recording = DeviceRecordingImport {
            audio: stage(
                &object,
                store
                    .cloud_recording_staging_path(TEST_OBJECT_ID)
                    .expect("the store owns a staging path"),
            )
            .expect("the object stages"),
            title: " \n".to_owned(),
            recorded_at_utc_ms: 1_788_305_031_276,
            device_id: TEST_DEVICE_ID.to_owned(),
        };

        let snapshot =
            tauri::async_runtime::block_on(manager.import_recording(recording.request()))
                .expect("the untitled recording imports");

        assert_eq!(
            snapshot.title,
            crate::meeting::types::MANUAL_DEFAULT_TITLE,
            "an untitled recording was filed under its object id"
        );
    }

    /// The head an install writes points the meeting at the phone's object so
    /// the next scan does not import it again and a delete tombstones it. It
    /// must not also make the review transition upload this Mac's bundle as a
    /// second revision of that object: the phone wrote it in another format,
    /// and a second Mac importing the same recording would then file a
    /// conflict on every phone recording. A meeting over a file the operator
    /// picked still queues, as its own object.
    #[test]
    fn a_meeting_over_a_phone_recording_is_not_uploaded_onto_the_phones_object() {
        let (_files, manager) = crate::meeting::session::tests::importing_manager();
        let store = tauri::async_runtime::block_on(manager.store()).expect("the store mounts");
        let audio = phone_audio();
        let object = phone_object(audio.clone(), &sha256_base64url(&audio), TEST_DEVICE_ID);
        let staged = stage(
            &object,
            store
                .cloud_recording_staging_path(TEST_OBJECT_ID)
                .expect("the store owns a staging path"),
        )
        .expect("the object stages");
        let import = |origin: RecordingOrigin| {
            let snapshot =
                tauri::async_runtime::block_on(manager.import_recording(ImportRecordingRequest {
                    path: staged.path().to_owned(),
                    title: None,
                    recorded_at_utc_ms: Some(1_788_305_031_276),
                    origin,
                }))
                .expect("the recording imports");
            let review = tauri::async_runtime::block_on(manager.get(snapshot.session_id))
                .expect("the imported meeting is readable");
            assert_eq!(review.session.phase, MeetingPhase::ReviewReady);
            snapshot.session_id
        };

        let pulled = import(RecordingOrigin::PairedDevice {
            device_id: TEST_DEVICE_ID.to_owned(),
        });
        // The head `install_device_recording` writes once the import returns.
        store
            .upsert_cloud_head(&CloudHead {
                object_id: TEST_OBJECT_ID.to_owned(),
                source_session_id: Some(pulled),
                remote_revision_id: Some(TEST_REVISION_ID.to_owned()),
                tombstone: false,
                acknowledged_revision_id: Some(TEST_REVISION_ID.to_owned()),
                change_sequence: 12,
            })
            .expect("the install records its head");

        assert!(
            !queue_session_upload(&store, pulled).expect("the review transition is handled"),
            "the review transition queued an upload for a phone recording"
        );
        assert!(
            store
                .cloud_outboxes_for_session(pulled)
                .expect("outboxes are readable")
                .is_empty(),
            "this Mac's bundle was queued as a revision of the phone's object"
        );

        let picked = import(RecordingOrigin::LocalFile);
        assert!(queue_session_upload(&store, picked).expect("the review transition is handled"));
        let outboxes = store
            .cloud_outboxes_for_session(picked)
            .expect("outboxes are readable");
        assert_eq!(outboxes.len(), 1);
        assert_eq!(outboxes[0].kind, CloudOutboxKind::Object);
        assert_ne!(outboxes[0].object_id, TEST_OBJECT_ID);
        assert!(outboxes[0].base_remote_revision_id.is_none());
    }

    /// A digest that does not describe the audio is the one thing that cannot be
    /// let through: it is what a truncated or swapped upload looks like. The
    /// staged file goes with the refusal, and no session was ever opened.
    #[test]
    fn a_recording_whose_digest_is_wrong_is_refused() {
        let (_files, manager) = crate::meeting::session::tests::importing_manager();
        let store = tauri::async_runtime::block_on(manager.store()).expect("the store mounts");
        let audio = phone_audio();
        let object = phone_object(audio, &sha256_base64url(b"other audio"), TEST_DEVICE_ID);
        let path = store
            .cloud_recording_staging_path(TEST_OBJECT_ID)
            .expect("the store owns a staging path");

        assert!(matches!(
            stage(&object, path.clone()),
            Err(CloudRuntimeError::IntegrityFailure)
        ));
        assert!(!path.exists(), "the staged audio outlived its refusal");
        assert!(
            store
                .list_sessions(None, 10, &MeetingListFilter::default())
                .expect("sessions are listable")
                .entries
                .is_empty(),
            "a refused recording left a meeting behind"
        );
    }

    /// A recording this Mac wrote is already here, so the pull path skips it
    /// and acknowledges the revision instead. That head write is what stops the
    /// next scan from fetching the object again — `apply_remote_change` reads it
    /// and returns before any download.
    #[test]
    fn an_acknowledged_revision_is_not_fetched_again() {
        let (_files, manager) = crate::meeting::session::tests::importing_manager();
        let store = tauri::async_runtime::block_on(manager.store()).expect("the store mounts");

        acknowledge_remote_revision(&store, TEST_OBJECT_ID, TEST_REVISION_ID, 12)
            .expect("a skipped revision is recorded");

        let head = store
            .cloud_head(TEST_OBJECT_ID)
            .expect("the head is readable")
            .expect("the head was written");
        assert_eq!(head.remote_revision_id.as_deref(), Some(TEST_REVISION_ID));
        assert_eq!(
            head.acknowledged_revision_id.as_deref(),
            Some(TEST_REVISION_ID),
            "an unacknowledged head would be fetched again on the next scan"
        );
        assert_eq!(head.change_sequence, 12);
        assert!(!head.tombstone);
        assert!(
            head.source_session_id.is_none(),
            "nothing was imported, so the object points at no meeting"
        );
    }

    /// A revision refused for what its bytes are is recorded as seen. The
    /// bytes cannot change, so the next scan would refuse it again after
    /// downloading it again, and every change behind it in the feed would wait
    /// on that for ever. A failure this Mac had is not recorded: the next scan
    /// tries the revision again.
    #[test]
    fn a_refused_recording_is_not_fetched_again() {
        let (_files, manager) = crate::meeting::session::tests::importing_manager();
        let store = tauri::async_runtime::block_on(manager.store()).expect("the store mounts");
        let audio = phone_audio();
        let object = phone_object(audio, &sha256_base64url(b"other audio"), TEST_DEVICE_ID);
        let path = store
            .cloud_recording_staging_path(TEST_OBJECT_ID)
            .expect("the store owns a staging path");
        let refused = stage(&object, path).map(drop);
        assert!(matches!(refused, Err(CloudRuntimeError::IntegrityFailure)));

        acknowledge_if_refused(&store, TEST_OBJECT_ID, TEST_REVISION_ID, 12, refused)
            .expect("a refusal is recorded, not returned");
        let head = store
            .cloud_head(TEST_OBJECT_ID)
            .expect("the head is readable")
            .expect("the refusal wrote a head");
        assert_eq!(
            head.remote_revision_id.as_deref(),
            Some(TEST_REVISION_ID),
            "an unacknowledged head would be fetched again on the next scan"
        );
        assert_eq!(head.change_sequence, 12);
        assert!(head.source_session_id.is_none(), "nothing was installed");

        let disk_failed = acknowledge_if_refused(
            &store,
            "otherobject12345",
            TEST_REVISION_ID,
            13,
            Err(CloudRuntimeError::File),
        );
        assert!(matches!(disk_failed, Err(CloudRuntimeError::File)));
        assert!(
            store
                .cloud_head("otherobject12345")
                .expect("the head is readable")
                .is_none(),
            "a failure of this Mac's own must be retried, not recorded"
        );
    }

    /// The signature the pull path checks after the chunks are on disk. The
    /// vault root every paired device holds is what decrypts them; this is what
    /// says which device wrote them.
    #[test]
    fn a_revision_signed_by_another_key_is_refused() {
        let audio = phone_audio();
        let object = phone_object(audio.clone(), &sha256_base64url(&audio), TEST_DEVICE_ID);
        let descriptors: Vec<(u64, u64, String)> = object
            .sealed_chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| {
                (
                    u64::try_from(index).expect("index"),
                    u64::try_from(chunk.len()).expect("size"),
                    sha256_base64url(chunk),
                )
            })
            .collect();
        let chunks: Vec<UploadChunk<'_>> = descriptors
            .iter()
            .map(|(index, size, digest)| UploadChunk {
                index: *index,
                size: *size,
                sha256: digest,
            })
            .collect();
        let signing_seed = [3_u8; 32];
        let signature = sign_canonical_upload_envelope(
            &CanonicalUploadEnvelopeInput {
                vault_id: TEST_VAULT_ID,
                kind: UploadKind::Object,
                object_id: Some(TEST_OBJECT_ID),
                revision_id: Some(TEST_REVISION_ID),
                base_revision_id: None,
                share_id: None,
                manifest_digest: &object.envelope.manifest_sha256,
                crypto_version: u64::from(CRYPTO_VERSION),
                total_bytes: object.envelope.total_bytes,
                chunks: &chunks,
            },
            &signing_seed,
        )
        .expect("the writer signs its envelope");
        let signed = ObjectRevisionEnvelope {
            writer_signature: base64_url_encode(&signature),
            ..object.envelope.clone()
        };
        let writer_key = ed25519_public_key(&signing_seed).expect("the writer's public key");

        assert!(verify_writer_signature(
            TEST_VAULT_ID,
            TEST_OBJECT_ID,
            TEST_REVISION_ID,
            &signed,
            &descriptors,
            &writer_key,
        )
        .is_ok());
        assert!(matches!(
            verify_writer_signature(
                TEST_VAULT_ID,
                TEST_OBJECT_ID,
                TEST_REVISION_ID,
                &signed,
                &descriptors,
                &ed25519_public_key(&[4_u8; 32]).expect("another device's public key"),
            ),
            Err(CloudRuntimeError::IntegrityFailure)
        ));
    }

    /// The manifest is sealed under the vault root every member holds, so any
    /// paired device can seal one naming another. The writer signature binds
    /// `envelope.writer_device_id`, which the service derives from the
    /// uploader's credentials, so the manifest's claim has to match it before
    /// the recording is staged or a chunk of it fetched. Only then is the
    /// origin the meeting records a proven fact.
    #[test]
    fn a_manifest_naming_another_device_is_refused_before_any_chunk() {
        let audio = phone_audio();
        let mut object = phone_object(audio.clone(), &sha256_base64url(&audio), "otherdevice12345");
        object.envelope.writer_device_id = TEST_DEVICE_ID.to_owned();
        let manifest = match open_remote_manifest(
            &TEST_VAULT_ROOT,
            TEST_VAULT_ID,
            TEST_OBJECT_ID,
            TEST_REVISION_ID,
            object.envelope.chunk_count,
            &object.sealed_manifest,
        )
        .expect("the manifest opens")
        {
            RemoteManifest::DeviceRecording(manifest) => manifest,
            RemoteManifest::Meeting(_) => panic!("a device recording read as a meeting bundle"),
        };

        assert!(matches!(
            declared_device_audio_bytes(&manifest, &object.envelope),
            Err(CloudRuntimeError::IntegrityFailure)
        ));

        // The same declaration from the device it names is accepted.
        object.envelope.writer_device_id = "otherdevice12345".to_owned();
        assert!(declared_device_audio_bytes(&manifest, &object.envelope).is_ok());
    }

    /// A meeting bundle and a recording are told apart by the format their
    /// manifest was sealed under, so neither can be read as the other.
    #[test]
    fn a_recording_manifest_does_not_read_as_a_meeting_bundle() {
        let manifest = serde_json::to_vec(&ObjectPayloadManifest {
            version: PROTOCOL_VERSION,
            kind: CAPABILITY_SHARE_KIND.to_owned(),
            source_format: OBJECT_SOURCE_FORMAT.to_owned(),
            chunk_count: 1,
            plaintext_bytes: 4,
            plaintext_sha256: sha256_base64url(b"json"),
        })
        .expect("a meeting manifest");
        let sealed_as_recording = seal(0, 1, ObjectContentKind::Manifest, &manifest);

        assert!(matches!(
            open_remote_manifest(
                &TEST_VAULT_ROOT,
                TEST_VAULT_ID,
                TEST_OBJECT_ID,
                TEST_REVISION_ID,
                1,
                &sealed_as_recording,
            ),
            Err(CloudRuntimeError::IntegrityFailure)
        ));
    }
}
