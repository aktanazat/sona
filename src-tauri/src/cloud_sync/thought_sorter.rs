//! Board cards for the phone's thoughts, written on this Mac when the person
//! turns that on. A card is what the server sorter writes, in the same
//! format and under the same object id, so whichever of the two commits a
//! thought's card first owns it and the other finds it taken.
//!
//! Only engines on this Mac read a thought: its voice recording goes to the
//! transcription model, its photos to Vision, and the note to the engine the
//! person chose for notes (Apple Intelligence or their own loopback model).

use super::{
    acknowledge_remote_revision, adjusted_now_ms, base64_url_decode, credentials, map_store_error,
    memory_index::phone_sync_directory,
    open_object_revision_payload,
    phone_thought_manifest::{thought_audio_samples, ThoughtManifest},
    phone_thoughts::PhoneThoughtAttachment,
    revision_upload::{self, StagedRevision, UploadRefused},
    sha256_base64url, write_staged_file, CloudAccess, CloudRuntimeError, CloudSyncRuntime,
    ObjectContentKind, ObjectRevisionCryptoContext, CRYPTO_VERSION,
};
use crate::managers::{history::HistoryManager, transcription::TranscriptionManager};
use crate::meeting::processing::{
    evidence_budget, first_json_value, json_response_schema, MeetingTextGenerator,
};
use crate::meeting::screen_text::{self, ScreenTextError};
use crate::modes::{AsrPlan, RunPlan};
use crate::scratchpad::phone_import::{self, PhoneThoughtReference};
use crate::settings;
use schemars::{schema::RootSchema, JsonSchema};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Arc, LazyLock},
};
use tauri::Manager;
use zeroize::Zeroizing;

pub(super) const CARD_SOURCE_FORMAT: &str = "sona-card-v1";
const STATE_FILE: &str = "thought-sorter.json";
const PROMPT_VERSION: u32 = 1;
const OUTPUT_TOKENS: i32 = 300;
const UNSORTED_KEY: &str = "unsorted";
/// Thoughts read per sync pass, so a backlog of voice notes never holds the
/// rest of sync behind the transcription model for long.
const MAX_THOUGHTS_PER_PASS: usize = 4;
/// Failed tries at one thought revision before the Mac leaves it in Unsorted.
const MAX_ATTEMPTS: u32 = 5;
const RETRY_BASE_MS: i64 = 60 * 1000;
const RETRY_CAP_MS: i64 = 6 * 60 * 60 * 1000;
const MAX_TITLE_CHARS: usize = 80;
const MAX_CLUSTER_CHARS: usize = 40;
const MAX_SUMMARY_CHARS: usize = 400;
const MAX_TAGS: usize = 6;
const MAX_TAG_CHARS: usize = 30;

const SYSTEM_PROMPT: &str = "You file one note into a personal brainstorm board. The notes are quick thoughts a person captured by voice or by typing, sometimes with a link or a photo. Reply with one JSON object and nothing else:
{\"title\": string, \"summary\": string, \"tags\": string[], \"cluster\": string}
- title: at most 8 words, in the note's own language, no trailing period.
- summary: one or two plain sentences that keep every concrete detail (names, numbers, decisions). Empty string when the note is already one short sentence.
- tags: up to 5 lowercase single words or hyphenated pairs.
- cluster: the board column this belongs to. Reuse one of the existing clusters when the note fits it; otherwise name a new one in 1 to 3 words, Title Case.";

static RESPONSE_SCHEMA: LazyLock<RootSchema> =
    LazyLock::new(json_response_schema::<Classification>);

/// A board column. Fields are in key order, as the sorter writes them.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct CardCluster {
    pub hue: u16,
    pub key: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct CardSorter {
    pub model: String,
    pub prompt_version: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct CardWriter {
    pub device_id: String,
    pub role: String,
}

/// What a sorter made of one thought: `sona-card-v1`, as the server sorter
/// and the phone read and write it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct CardManifest {
    pub archived: bool,
    pub cluster: CardCluster,
    pub format_version: u32,
    pub kind: String,
    pub pinned: bool,
    pub sorter: Option<CardSorter>,
    pub summary: String,
    pub tags: Vec<String>,
    pub thought_id: String,
    pub thought_revision_id: String,
    pub title: String,
    pub writer: CardWriter,
    pub written_at_utc_ms: i64,
}

impl CardManifest {
    /// A card a model classified, or one a person placed. A sorter's card
    /// with no classification is the Unsorted stand-in for a thought it
    /// could not read, which this Mac may still replace.
    fn settles(&self) -> bool {
        self.sorter.is_some() || self.writer.role == "user"
    }
}

/// The one object id a thought's card has, wherever it is written.
pub(super) fn card_object_id(thought_id: &str) -> String {
    sha256_base64url(format!("{CARD_SOURCE_FORMAT}:{thought_id}").as_bytes())
}

/// Open a manifest as a card. The AEAD binds it to this vault, object,
/// revision and format, which is all a card's reader needs: the writer
/// signature would add only which device wrote it, and nothing here uses
/// that.
pub(super) fn open_card(
    vault_root: &[u8],
    context: &ObjectRevisionCryptoContext<'_>,
    ciphertext: &[u8],
) -> Result<Option<CardManifest>, CloudRuntimeError> {
    let context = ObjectRevisionCryptoContext {
        source_format: CARD_SOURCE_FORMAT,
        ..*context
    };
    let Ok(bytes) = open_object_revision_payload(vault_root, &context, ciphertext) else {
        return Ok(None);
    };
    let card: CardManifest = serde_json::from_slice(&Zeroizing::new(bytes))
        .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    if card.kind != "card" || card.format_version != 1 {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    Ok(Some(card))
}

/// The lowercase ASCII slug a cluster name is grouped by. Letters outside
/// ASCII are dropped rather than folded, so a new name in them can key
/// differently from the server sorter's; a name already on the board is
/// matched by name first and never forks.
fn cluster_key(name: &str) -> String {
    let mut key = String::new();
    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            key.push(character);
        } else if !key.is_empty() && !key.ends_with('-') {
            key.push('-');
        }
    }
    let key = key.trim_end_matches('-');
    if key.is_empty() {
        format!("cluster-{}", &sha256_base64url(name.as_bytes())[..16])
    } else {
        key.to_owned()
    }
}

/// FNV-1a over the key, folded onto the colour wheel, as every board does.
fn cluster_hue(key: &str) -> u16 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in key.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    u16::try_from(hash % 360).unwrap_or(0)
}

fn resolve_cluster(name: &str, clusters: &[CardCluster]) -> CardCluster {
    let name = name.trim();
    let key = cluster_key(name);
    clusters
        .iter()
        .find(|cluster| cluster.name.eq_ignore_ascii_case(name) || cluster.key == key)
        .cloned()
        .unwrap_or_else(|| CardCluster {
            hue: cluster_hue(&key),
            key,
            name: name.to_owned(),
        })
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Classification {
    title: String,
    summary: String,
    tags: Vec<String>,
    cluster: String,
}

/// Why a thought got no card this time.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Unfiled {
    /// Nothing in it reads as words: no text, a recording with no speech in
    /// it or in a format this Mac cannot decode, photos with nothing Vision
    /// knows. Not tried again for this revision.
    NothingToRead,
    /// An engine failed or answered nothing usable; tried again later, up to
    /// [`MAX_ATTEMPTS`] times.
    Retry,
}

/// Whether an engine on this Mac failed for now; a picture or a recording
/// with nothing in it is `Ok` and empty.
#[derive(Debug)]
pub(super) struct SenseFailed;

/// What Vision found in one photo.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct PhotoReading {
    /// The words written in it.
    pub text: String,
    /// What it shows, as Vision's scene labels.
    pub labels: Vec<String>,
}

/// The engines on this Mac that turn a thought's attachments into words.
pub(super) trait ThoughtSenses: Send + Sync {
    /// Words spoken in 16 kHz mono audio.
    fn transcribe(&self, samples: &[f32]) -> Result<String, SenseFailed>;
    fn read_image(&self, image: &[u8]) -> Result<PhotoReading, SenseFailed>;
}

/// One thought and its attachments, as they were imported here.
pub(super) struct ThoughtContent {
    pub thought_id: String,
    pub revision_id: String,
    pub manifest: ThoughtManifest,
    pub audio: Option<Vec<u8>>,
    pub images: Vec<Vec<u8>>,
}

/// What this Mac heard and saw in a textless thought's attachments. Kept
/// per revision, so a retry never transcribes or reads the photos again.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Reading {
    spoken: String,
    photos: Vec<PhotoReading>,
}

impl Reading {
    fn is_empty(&self) -> bool {
        self.spoken.is_empty()
            && self
                .photos
                .iter()
                .all(|photo| photo.text.is_empty() && photo.labels.is_empty())
    }

    /// The words for the model, each marked with where it came from.
    fn labelled(&self) -> String {
        let mut parts = Vec::new();
        if !self.spoken.is_empty() {
            parts.push(format!("Said in the recording: {}", self.spoken));
        }
        for photo in &self.photos {
            match (photo.text.is_empty(), photo.labels.is_empty()) {
                (true, true) => {}
                (true, false) => parts.push(format!(
                    "In the photo: a picture of {}",
                    photo.labels.join(", ")
                )),
                (false, true) => parts.push(format!("In the photo: {}", photo.text)),
                (false, false) => parts.push(format!(
                    "In the photo: {} (a picture of {})",
                    photo.text,
                    photo.labels.join(", ")
                )),
            }
        }
        parts.join("\n")
    }

    /// The words themselves, as the person's note keeps them: what was said
    /// and what the photos read, with no labels.
    fn plain(&self) -> String {
        std::iter::once(self.spoken.as_str())
            .chain(self.photos.iter().map(|photo| photo.text.as_str()))
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// Transcribe a textless thought's recording and read its photos.
fn read_attachments(
    content: &ThoughtContent,
    senses: &dyn ThoughtSenses,
) -> Result<Reading, Unfiled> {
    let mut reading = Reading::default();
    if let (Some(audio), Some(bytes)) = (&content.manifest.audio, &content.audio) {
        if let Some(samples) =
            thought_audio_samples(&audio.codec, audio.sample_rate_hz, audio.channels, bytes)
        {
            reading.spoken = senses
                .transcribe(&samples)
                .map_err(|_| Unfiled::Retry)?
                .trim()
                .to_owned();
        }
    }
    for image in &content.images {
        let photo = senses.read_image(image).map_err(|_| Unfiled::Retry)?;
        reading.photos.push(PhotoReading {
            text: photo.text.trim().to_owned(),
            labels: photo.labels,
        });
    }
    if reading.is_empty() {
        return Err(Unfiled::NothingToRead);
    }
    Ok(reading)
}

/// The imported note's body before and after its attachments were read:
/// what was said and the words the photos show, without labels, and the
/// link line kept. `None` when the photos showed things but no words.
fn note_body(manifest: &ThoughtManifest, reading: &Reading) -> Option<(String, String)> {
    let plain = reading.plain();
    if plain.is_empty() {
        return None;
    }
    let link = manifest.link.as_ref().map(|link| link.url.as_str());
    let body = |text: &str| {
        phone_import::capture_body(text, link, manifest.audio.is_some(), manifest.images.len())
    };
    Some((body(&manifest.text), body(&plain)))
}

/// The request for one note. A note longer than the local model's budget is
/// cut to fit: a model that sees the start of a long note still files it.
fn evidence(
    manifest: &ThoughtManifest,
    text: &str,
    from_attachments: bool,
    clusters: &[CardCluster],
    budget: usize,
) -> String {
    let names = clusters
        .iter()
        .map(|cluster| cluster.name.as_str())
        .collect::<Vec<_>>();
    let mut lines = vec![
        format!(
            "Existing clusters: {}",
            if names.is_empty() {
                "(none yet)".to_owned()
            } else {
                names.join(", ")
            }
        ),
        format!("Captured by: {}", manifest.origin.as_str()),
    ];
    if let Some(link) = &manifest.link {
        lines.push(format!("Link: {}", link.url));
    }
    if !manifest.images.is_empty() {
        lines.push(format!("Attached images: {}", manifest.images.len()));
    }
    if from_attachments {
        lines.push(
            "The note had no typed text; the words below were read from its attachments."
                .to_owned(),
        );
    }
    lines.push(String::new());
    lines.push("Note:".to_owned());
    let mut evidence = lines.join("\n");
    evidence.push('\n');
    let mut end = budget.saturating_sub(evidence.len()).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    evidence.push_str(&text[..end]);
    evidence
}

fn clipped(text: &str, max_chars: usize) -> String {
    text.trim()
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn classify(
    generator: &dyn MeetingTextGenerator,
    evidence: &str,
) -> Result<Classification, Unfiled> {
    let answer = generator
        .generate_json(SYSTEM_PROMPT, evidence, OUTPUT_TOKENS, || &RESPONSE_SCHEMA)
        .map_err(|error| {
            log::info!("A phone thought was not classified: {error:?}");
            Unfiled::Retry
        })?;
    let trimmed = answer.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|body| body.strip_suffix("```"))
        .unwrap_or(trimmed)
        .trim();
    let reply: Classification = first_json_value(json).map_err(|()| Unfiled::Retry)?;
    let title = clipped(&reply.title, MAX_TITLE_CHARS);
    let cluster = clipped(&reply.cluster, MAX_CLUSTER_CHARS);
    if title.is_empty() || cluster.is_empty() || cluster_key(&cluster) == UNSORTED_KEY {
        return Err(Unfiled::Retry);
    }
    Ok(Classification {
        title,
        summary: clipped(&reply.summary, MAX_SUMMARY_CHARS),
        tags: reply
            .tags
            .iter()
            .map(|tag| clipped(tag, MAX_TAG_CHARS))
            .filter(|tag| !tag.is_empty())
            .take(MAX_TAGS)
            .collect(),
        cluster,
    })
}

/// The card for one thought, classified only by the local notes engine:
/// from its typed text, or else from what this Mac read in its attachments.
pub(super) fn card_for(
    content: &ThoughtContent,
    reading: Option<&Reading>,
    clusters: &[CardCluster],
    generator: &dyn MeetingTextGenerator,
    device_id: &str,
    now_utc_ms: i64,
) -> Result<CardManifest, Unfiled> {
    let typed = content.manifest.text.trim();
    let (text, from_attachments) = match reading {
        Some(reading) if typed.is_empty() => (reading.labelled(), true),
        _ if typed.is_empty() => return Err(Unfiled::NothingToRead),
        _ => (typed.to_owned(), false),
    };
    let budget = evidence_budget(generator, SYSTEM_PROMPT, OUTPUT_TOKENS);
    let classification = classify(
        generator,
        &evidence(&content.manifest, &text, from_attachments, clusters, budget),
    )?;
    Ok(CardManifest {
        archived: false,
        cluster: resolve_cluster(&classification.cluster, clusters),
        format_version: 1,
        kind: "card".to_owned(),
        pinned: false,
        sorter: Some(CardSorter {
            model: generator.model_id().to_owned(),
            prompt_version: PROMPT_VERSION,
        }),
        summary: classification.summary,
        tags: classification.tags,
        thought_id: content.thought_id.clone(),
        thought_revision_id: content.revision_id.clone(),
        title: classification.title,
        writer: CardWriter {
            device_id: device_id.to_owned(),
            role: "sorter".to_owned(),
        },
        written_at_utc_ms: now_utc_ms,
    })
}

/// What this Mac remembers of one card object in the vault.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct KnownCard {
    thought_id: String,
    cluster: CardCluster,
    settles: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct Attempt {
    count: u32,
    next_at_utc_ms: i64,
}

#[derive(Clone, Deserialize, Serialize)]
struct PendingCard {
    thought_id: String,
    cluster: CardCluster,
    revision: StagedRevision,
}

#[derive(Default, Deserialize, Serialize)]
struct SorterState {
    vault_id: String,
    device_id: String,
    /// Every card head in the vault has been read once. Cards that arrived
    /// before this Mac knew the format were acknowledged unread.
    cards_backfilled: bool,
    /// Card object id to what it says.
    cards: BTreeMap<String, KnownCard>,
    /// Thought revision to its failed tries so far.
    attempts: BTreeMap<String, Attempt>,
    /// Thought revision to what this Mac read in its attachments, until the
    /// revision is filed or given up.
    readings: BTreeMap<String, Reading>,
    /// Thought revisions this Mac stopped trying: nothing in them to read,
    /// or [`MAX_ATTEMPTS`] failed tries. They stay in Unsorted.
    stopped: BTreeSet<String>,
    pending: Option<PendingCard>,
}

/// What one pass reads: the thoughts due, and the board's clusters.
struct PassPlan<'a> {
    due: Vec<&'a PhoneThoughtReference>,
    clusters: Vec<CardCluster>,
}

impl SorterState {
    /// The thoughts due this pass, oldest first, at most
    /// [`MAX_THOUGHTS_PER_PASS`]. The clusters, which ask the store whether
    /// each card is still in the vault, are read only when one is due.
    fn plan<'a>(
        &self,
        references: &'a [PhoneThoughtReference],
        now_utc_ms: i64,
        live: impl Fn(&str) -> bool,
    ) -> PassPlan<'a> {
        let filed: BTreeSet<&str> = self
            .cards
            .values()
            .filter(|card| card.settles)
            .map(|card| card.thought_id.as_str())
            .collect();
        let due: Vec<_> = references
            .iter()
            .filter(|reference| {
                !filed.contains(reference.object_id.as_str())
                    && !self.stopped.contains(&reference.revision_id)
                    && self
                        .attempts
                        .get(&reference.revision_id)
                        .is_none_or(|attempt| now_utc_ms >= attempt.next_at_utc_ms)
            })
            .take(MAX_THOUGHTS_PER_PASS)
            .collect();
        let clusters = if due.is_empty() {
            Vec::new()
        } else {
            self.clusters(live)
        };
        PassPlan { due, clusters }
    }

    fn clusters(&self, live: impl Fn(&str) -> bool) -> Vec<CardCluster> {
        let mut by_key = BTreeMap::new();
        for (object_id, card) in &self.cards {
            if card.cluster.key != UNSORTED_KEY && live(object_id) {
                by_key
                    .entry(card.cluster.key.clone())
                    .or_insert_with(|| card.cluster.clone());
            }
        }
        by_key.into_values().collect()
    }

    /// One failed try at a thought revision. After [`MAX_ATTEMPTS`] the Mac
    /// stops trying it.
    fn failed(&mut self, revision_id: &str, now_utc_ms: i64) {
        let count = self
            .attempts
            .get(revision_id)
            .map_or(0, |attempt| attempt.count)
            .saturating_add(1);
        if count >= MAX_ATTEMPTS {
            self.stop(revision_id);
        } else {
            self.attempts.insert(
                revision_id.to_owned(),
                Attempt {
                    count,
                    next_at_utc_ms: now_utc_ms.saturating_add(retry_delay(count)),
                },
            );
        }
    }

    fn stop(&mut self, revision_id: &str) {
        self.forget(revision_id);
        self.stopped.insert(revision_id.to_owned());
    }

    fn forget(&mut self, revision_id: &str) {
        self.attempts.remove(revision_id);
        self.readings.remove(revision_id);
    }
}

fn retry_delay(count: u32) -> i64 {
    RETRY_BASE_MS
        .saturating_mul(1_i64 << count.min(20))
        .min(RETRY_CAP_MS)
}

fn read_state(directory: &Path) -> Result<Option<SorterState>, CloudRuntimeError> {
    match fs::read(directory.join(STATE_FILE)) {
        Ok(bytes) => serde_json::from_slice(&Zeroizing::new(bytes))
            .map(Some)
            .map_err(|_| CloudRuntimeError::IntegrityFailure),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(CloudRuntimeError::File),
    }
}

fn persist(directory: &Path, state: &SorterState) -> Result<(), CloudRuntimeError> {
    fs::create_dir_all(directory).map_err(|_| CloudRuntimeError::File)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
        .map_err(|_| CloudRuntimeError::File)?;
    write_staged_file(
        directory,
        STATE_FILE,
        &Zeroizing::new(serde_json::to_vec(state).map_err(|_| CloudRuntimeError::File)?),
    )
}

/// The Mac's own engines, as they are set up now.
struct MacSenses {
    transcription: Option<(Arc<TranscriptionManager>, AsrPlan)>,
}

impl ThoughtSenses for MacSenses {
    fn transcribe(&self, samples: &[f32]) -> Result<String, SenseFailed> {
        let (manager, plan) = self.transcription.as_ref().ok_or(SenseFailed)?;
        manager
            .transcribe_shared(plan, samples)
            .map(|decode| {
                if decode.model_produced_text {
                    decode.text
                } else {
                    String::new()
                }
            })
            .map_err(|_| SenseFailed)
    }

    fn read_image(&self, image: &[u8]) -> Result<PhotoReading, SenseFailed> {
        let text = match screen_text::read(image) {
            Ok(read) => read.text,
            Err(ScreenTextError::Unreadable) => return Ok(PhotoReading::default()),
            Err(ScreenTextError::Failed) => return Err(SenseFailed),
        };
        let labels = screen_text::labels(image).map_err(|_| SenseFailed)?;
        Ok(PhotoReading { text, labels })
    }
}

impl CloudSyncRuntime {
    fn edit_sorter(
        &self,
        access: &CloudAccess,
        edit: impl FnOnce(&mut SorterState) -> Result<bool, CloudRuntimeError>,
    ) -> Result<(), CloudRuntimeError> {
        let directory = phone_sync_directory(&self.app).map_err(|_| CloudRuntimeError::File)?;
        let _guard = self
            .phone_thought_gate
            .lock()
            .map_err(|_| CloudRuntimeError::Storage)?;
        let mut state = match read_state(&directory)? {
            Some(state)
                if state.vault_id == access.state.vault_id
                    && state.device_id == access.state.device_id =>
            {
                state
            }
            _ => SorterState {
                vault_id: access.state.vault_id.clone(),
                device_id: access.state.device_id.clone(),
                ..SorterState::default()
            },
        };
        if edit(&mut state)? {
            persist(&directory, &state)?;
        }
        Ok(())
    }

    /// Note a card some sorter wrote, so this Mac never files its thought
    /// again.
    pub(super) fn remember_card(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        sequence: u64,
        card: CardManifest,
    ) -> Result<(), CloudRuntimeError> {
        self.edit_sorter(access, |state| {
            state.cards.insert(
                object_id.to_owned(),
                KnownCard {
                    settles: card.settles(),
                    thought_id: card.thought_id,
                    cluster: card.cluster,
                },
            );
            Ok(true)
        })?;
        acknowledge_remote_revision(&access.store, object_id, revision_id, sequence)
    }

    fn card_is_live(&self, access: &CloudAccess, object_id: &str) -> bool {
        !matches!(
            access.store.cloud_head(object_id),
            Ok(Some(head)) if head.tombstone
        )
    }

    /// Read every card head once, for the cards that came before this Mac
    /// read the format.
    async fn backfill_cards(&self, access: &CloudAccess) -> Result<(), CloudRuntimeError> {
        let history = self.app.state::<Arc<HistoryManager>>();
        let mut known = BTreeSet::new();
        self.edit_sorter(access, |state| {
            known.extend(state.cards.keys().cloned());
            Ok(false)
        })?;
        let mut high_water = None;
        let mut after = None;
        loop {
            self.require_request_permission(&access.state)?;
            let page = access
                .client
                .snapshot(
                    &credentials(access)?,
                    high_water.as_deref(),
                    after.as_deref(),
                    Some(100),
                )
                .await
                .map_err(CloudRuntimeError::Client)?;
            self.persist_clock(&access.store, &access.client);
            for head in &page.heads {
                if head.tombstone || known.contains(&head.object_id) {
                    continue;
                }
                let current = access
                    .store
                    .cloud_head(&head.object_id)
                    .map_err(map_store_error)?;
                if current.is_some_and(|current| current.source_session_id.is_some())
                    || phone_import::reference(
                        &history,
                        &access.state.vault_id,
                        &head.object_id,
                        None,
                    )
                    .map_err(|_| CloudRuntimeError::Storage)?
                    .is_some()
                {
                    continue;
                }
                if let Some(card) = self
                    .fetch_card(access, &head.object_id, &head.revision_id)
                    .await?
                {
                    self.remember_card(
                        access,
                        &head.object_id,
                        &head.revision_id,
                        head.sequence,
                        card,
                    )?;
                }
            }
            high_water = Some(page.high_water);
            after = page.after;
            if !page.has_more {
                break;
            }
        }
        self.edit_sorter(access, |state| {
            state.cards_backfilled = true;
            Ok(true)
        })
    }

    async fn fetch_card(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
    ) -> Result<Option<CardManifest>, CloudRuntimeError> {
        self.require_request_permission(&access.state)?;
        let response = access
            .client
            .object_manifest(&credentials(access)?, object_id, revision_id)
            .await
            .map_err(CloudRuntimeError::Client)?;
        self.persist_clock(&access.store, &access.client);
        let encrypted = base64_url_decode(&response.manifest)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let envelope = &response.envelope;
        if envelope.object_id != object_id
            || envelope.revision_id != revision_id
            || envelope.crypto_version != CRYPTO_VERSION
            || sha256_base64url(&encrypted) != envelope.manifest_sha256
        {
            return Ok(None);
        }
        open_card(
            &*access.keys.vault_root,
            &ObjectRevisionCryptoContext {
                vault_id: &access.state.vault_id,
                object_id,
                revision_id,
                index: 0,
                total: u64::from(envelope.chunk_count),
                content_kind: ObjectContentKind::Manifest,
                source_format: CARD_SOURCE_FORMAT,
            },
            &encrypted,
        )
        .or(Ok(None))
    }

    /// Send the staged card. A refusal of its base or its create means
    /// another sorter wrote that thought's card first: the change feed
    /// brings it, and the thought is judged again next pass.
    async fn upload_pending_card(&self, access: &CloudAccess) -> Result<(), CloudRuntimeError> {
        let mut pending = None;
        self.edit_sorter(access, |state| {
            pending = state.pending.clone();
            Ok(false)
        })?;
        let Some(mut pending) = pending else {
            return Ok(());
        };
        let uploaded =
            revision_upload::upload(self, access, CARD_SOURCE_FORMAT, &mut pending.revision).await;
        let refused = match uploaded {
            Ok(()) => None,
            Err(UploadRefused::Retry(error)) => return Err(error),
            Err(UploadRefused::Restage(error)) => Some(error),
        };
        self.edit_sorter(access, |state| {
            if refused.is_none() {
                state.cards.insert(
                    pending.revision.object_id.clone(),
                    KnownCard {
                        thought_id: pending.thought_id.clone(),
                        cluster: pending.cluster.clone(),
                        settles: true,
                    },
                );
            }
            state.pending = None;
            Ok(true)
        })?;
        if let Some(error) = refused {
            log::info!("A phone thought's card was written elsewhere first: {error:?}");
        }
        Ok(())
    }

    /// One pass of the Mac's sorter: off unless the person turned it on.
    pub(super) async fn sort_phone_thoughts(
        &self,
        access: &CloudAccess,
    ) -> Result<(), CloudRuntimeError> {
        if !settings::get_settings(&self.app)
            .cloud_sync
            .sort_thoughts_here
        {
            return Ok(());
        }
        let mut backfilled = false;
        self.edit_sorter(access, |state| {
            backfilled = state.cards_backfilled;
            Ok(false)
        })?;
        if !backfilled {
            self.backfill_cards(access).await?;
        }
        self.upload_pending_card(access).await?;
        let Some(generator) = self.meetings.processing().local_text_generator() else {
            return Ok(());
        };
        let history = Arc::clone(self.app.state::<Arc<HistoryManager>>().inner());
        let references = phone_import::current_references(&history, &access.state.vault_id)
            .map_err(|_| CloudRuntimeError::Storage)?;
        let now = adjusted_now_ms(access.state.clock_offset_ms);
        let mut readings = BTreeMap::new();
        let mut plan = PassPlan {
            due: Vec::new(),
            clusters: Vec::new(),
        };
        self.edit_sorter(access, |state| {
            plan = state.plan(&references, now, |object_id| {
                self.card_is_live(access, object_id)
            });
            for reference in &plan.due {
                if let Some(reading) = state.readings.get(&reference.revision_id) {
                    readings.insert(reference.revision_id.clone(), reading.clone());
                }
            }
            Ok(false)
        })?;
        if plan.due.is_empty() {
            return Ok(());
        }
        let senses: Arc<dyn ThoughtSenses> = Arc::new(MacSenses {
            transcription: self
                .app
                .try_state::<Arc<TranscriptionManager>>()
                .and_then(|manager| {
                    let plan = RunPlan::for_media_import(&settings::get_settings(&self.app))
                        .ok()?
                        .asr()
                        .clone();
                    Some((Arc::clone(manager.inner()), plan))
                }),
        });
        for reference in plan.due {
            let sorted = self
                .sort_thought(
                    access,
                    &history,
                    reference,
                    readings.remove(&reference.revision_id),
                    &plan.clusters,
                    &generator,
                    &senses,
                    now,
                )
                .await;
            if let Err(error) = sorted {
                log::warn!("A phone thought was not sorted this time: {error:?}");
                self.edit_sorter(access, |state| {
                    state.failed(&reference.revision_id, now);
                    Ok(true)
                })?;
            }
            self.upload_pending_card(access).await?;
        }
        Ok(())
    }

    /// Read, classify and stage the card for one thought, recording how it
    /// went. An error is a failure of this thought alone, which the caller
    /// counts as one try.
    #[expect(
        clippy::too_many_arguments,
        reason = "one pass's shared inputs, passed once per thought"
    )]
    async fn sort_thought(
        &self,
        access: &CloudAccess,
        history: &HistoryManager,
        reference: &PhoneThoughtReference,
        known: Option<Reading>,
        clusters: &[CardCluster],
        generator: &Arc<dyn MeetingTextGenerator>,
        senses: &Arc<dyn ThoughtSenses>,
        now: i64,
    ) -> Result<(), CloudRuntimeError> {
        let revision_id = reference.revision_id.as_str();
        let Some(content) = self
            .thought_content(&reference.object_id, revision_id, known.is_none())
            .await?
        else {
            return Ok(());
        };
        let typed = !content.manifest.text.trim().is_empty();
        let (content, reading) = match known {
            Some(reading) => (content, Some(reading)),
            None if typed => (content, None),
            None => {
                let senses = Arc::clone(senses);
                let (content, read) = tauri::async_runtime::spawn_blocking(move || {
                    let read = read_attachments(&content, senses.as_ref());
                    (content, read)
                })
                .await
                .map_err(|_| CloudRuntimeError::Storage)?;
                let reading = match read {
                    Ok(reading) => reading,
                    Err(unfiled) => {
                        return self.edit_sorter(access, |state| {
                            match unfiled {
                                Unfiled::NothingToRead => state.stop(revision_id),
                                Unfiled::Retry => state.failed(revision_id, now),
                            }
                            Ok(true)
                        });
                    }
                };
                if let Some((expected, body)) = note_body(&content.manifest, &reading) {
                    phone_import::enrich(&self.app, history, reference, &expected, &body, now)
                        .map_err(|_| CloudRuntimeError::Storage)?;
                }
                self.edit_sorter(access, |state| {
                    state
                        .readings
                        .insert(revision_id.to_owned(), reading.clone());
                    Ok(true)
                })?;
                (content, Some(reading))
            }
        };
        let generator = Arc::clone(generator);
        let clusters = clusters.to_vec();
        let device_id = access.state.device_id.clone();
        let filed = tauri::async_runtime::spawn_blocking(move || {
            card_for(
                &content,
                reading.as_ref(),
                &clusters,
                generator.as_ref(),
                &device_id,
                now,
            )
        })
        .await
        .map_err(|_| CloudRuntimeError::Storage)?;
        let card = match filed {
            Ok(card) => card,
            Err(unfiled) => {
                return self.edit_sorter(access, |state| {
                    match unfiled {
                        Unfiled::NothingToRead => state.stop(revision_id),
                        Unfiled::Retry => state.failed(revision_id, now),
                    }
                    Ok(true)
                });
            }
        };
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&card).map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        );
        let revision = revision_upload::stage(
            access,
            CARD_SOURCE_FORMAT,
            card_object_id(&reference.object_id),
            &plaintext,
            &[&[]],
        )?;
        self.edit_sorter(access, |state| {
            state.forget(revision_id);
            state.pending = Some(PendingCard {
                thought_id: reference.object_id.clone(),
                cluster: card.cluster.clone(),
                revision,
            });
            Ok(true)
        })
    }

    /// A thought, and its attachments when `attachments` asks for them, as
    /// imported here; `None` when its source is gone or moved on since it
    /// was listed.
    async fn thought_content(
        &self,
        object_id: &str,
        revision_id: &str,
        attachments: bool,
    ) -> Result<Option<ThoughtContent>, CloudRuntimeError> {
        let Some(source) = self.phone_thought_source(object_id, revision_id).await? else {
            return Ok(None);
        };
        let manifest = source.manifest;
        let skip = !attachments || !manifest.text.trim().is_empty();
        let audio = if skip || manifest.audio.is_none() {
            None
        } else {
            Some(
                self.phone_thought_attachment(
                    object_id,
                    revision_id,
                    PhoneThoughtAttachment::Audio,
                )
                .await?,
            )
        };
        let mut images = Vec::new();
        if !skip {
            for index in 0..manifest.images.len() {
                let index =
                    u32::try_from(index).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                images.push(
                    self.phone_thought_attachment(
                        object_id,
                        revision_id,
                        PhoneThoughtAttachment::Image { index },
                    )
                    .await?,
                );
            }
        }
        Ok(Some(ThoughtContent {
            thought_id: object_id.to_owned(),
            revision_id: revision_id.to_owned(),
            manifest,
            audio,
            images,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_sync::runtime::phone_thought_manifest::{
        ThoughtAudio, ThoughtImage, ThoughtLink, ThoughtOrigin,
    };
    use crate::meeting::processing::{
        MeetingProcessingService, MeetingTextGenerationError, ReplyShape,
    };
    use std::borrow::Cow;
    use std::sync::Mutex;

    #[derive(Default)]
    struct LocalModel {
        evidence: Mutex<Vec<String>>,
    }

    impl MeetingTextGenerator for LocalModel {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "fixture-local-notes"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            16_384
        }
        fn generate(
            &self,
            _prompt: &str,
            evidence: &str,
            _max_tokens: i32,
            _shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            self.evidence.lock().unwrap().push(evidence.to_owned());
            Ok(r#"{"title":"Garden plans","summary":"Grow tomatoes this spring.","tags":["garden"],"cluster":"Garden"}"#.to_owned())
        }
    }

    struct CloudModel;

    impl MeetingTextGenerator for CloudModel {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "cloud-must-not-be-used"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            usize::MAX
        }
        fn generate(
            &self,
            _prompt: &str,
            _evidence: &str,
            _max_tokens: i32,
            _shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            panic!("a phone thought reached a cloud model")
        }
    }

    struct Senses;

    impl ThoughtSenses for Senses {
        fn transcribe(&self, samples: &[f32]) -> Result<String, SenseFailed> {
            assert!(!samples.is_empty());
            Ok("Grow tomatoes this spring".to_owned())
        }
        fn read_image(&self, image: &[u8]) -> Result<PhotoReading, SenseFailed> {
            assert_eq!(image, b"fixture-photo");
            Ok(PhotoReading {
                text: "Garden plans: tomatoes this spring".to_owned(),
                labels: vec!["outdoor".to_owned(), "sky".to_owned()],
            })
        }
    }

    fn audio(codec: &str, byte_length: u64) -> ThoughtAudio {
        ThoughtAudio {
            codec: codec.to_owned(),
            sample_rate_hz: 16_000,
            channels: 1,
            byte_length,
            sha256: String::new(),
            duration_ms: 1,
            chunk_start: 0,
            chunk_count: 1,
        }
    }

    fn photo() -> ThoughtContent {
        let mut photo = content();
        photo.manifest.origin = ThoughtOrigin::Shared;
        photo.manifest.images.push(ThoughtImage {
            mime: "image/jpeg".to_owned(),
            byte_length: 13,
            sha256: String::new(),
            width: 100,
            height: 100,
            chunk_start: 0,
            chunk_count: 1,
        });
        photo.images.push(b"fixture-photo".to_vec());
        photo
    }

    fn reference(index: usize) -> PhoneThoughtReference {
        PhoneThoughtReference {
            vault_id: "vault".to_owned(),
            object_id: format!("thought_{index}"),
            revision_id: format!("revision_{index}"),
            note_id: format!("note_{index}"),
            captured_at_utc_ms: 1,
            origin: "voice".to_owned(),
            device_id: "phone".to_owned(),
            link: None,
            source_deleted: false,
            change_sequence: 1,
        }
    }

    fn card_on(thought: usize, cluster: &str, settles: bool) -> KnownCard {
        KnownCard {
            thought_id: format!("thought_{thought}"),
            cluster: resolve_cluster(cluster, &[]),
            settles,
        }
    }

    fn content() -> ThoughtContent {
        ThoughtContent {
            thought_id: "thought_01".to_owned(),
            revision_id: "thought_revision_01".to_owned(),
            manifest: ThoughtManifest {
                format_version: 1,
                kind: "thought".to_owned(),
                device_id: "fixture-phone".to_owned(),
                captured_at_utc_ms: 1_700_000_000_000,
                origin: ThoughtOrigin::Voice,
                text: String::new(),
                link: None,
                audio: None,
                images: Vec::new(),
            },
            audio: None,
            images: Vec::new(),
        }
    }

    #[test]
    fn a_textless_voice_thought_is_transcribed_and_filed_in_a_cluster() {
        let mut voice = content();
        voice.manifest.audio = Some(audio("pcm_s16le", 6));
        voice.audio = Some(vec![0, 0, 0, 64, 0, 192]);
        let reading = read_attachments(&voice, &Senses).unwrap();
        let model = LocalModel::default();
        let card = card_for(&voice, Some(&reading), &[], &model, "mac", 123).unwrap();
        assert_eq!(card.cluster.key, "garden");
        assert_eq!(card.thought_id, voice.thought_id);
        assert!(model.evidence.lock().unwrap()[0]
            .contains("Said in the recording: Grow tomatoes this spring"));
    }

    #[test]
    fn a_compact_phones_voice_thought_is_transcribed_and_filed() {
        let bytes = include_bytes!("../../tests/fixtures/phone-audio/jfk.aac");
        let mut voice = content();
        voice.manifest.audio = Some(audio("aac_adts", bytes.len() as u64));
        voice.audio = Some(bytes.to_vec());
        let reading = read_attachments(&voice, &Senses).unwrap();
        let card = card_for(
            &voice,
            Some(&reading),
            &[],
            &LocalModel::default(),
            "mac",
            123,
        )
        .unwrap();
        assert_eq!(card.cluster.key, "garden");
    }

    #[test]
    fn a_textless_photo_thought_is_read_and_filed_in_a_cluster() {
        let photo = photo();
        let reading = read_attachments(&photo, &Senses).unwrap();
        let model = LocalModel::default();
        let card = card_for(&photo, Some(&reading), &[], &model, "mac", 123).unwrap();
        assert_eq!(card.cluster.key, "garden");
        assert!(model.evidence.lock().unwrap()[0].contains(
            "In the photo: Garden plans: tomatoes this spring (a picture of outdoor, sky)"
        ));
    }

    #[test]
    fn the_note_keeps_only_the_heard_words_and_its_link() {
        let mut voice = content();
        voice.manifest.audio = Some(audio("pcm_s16le", 6));
        voice.manifest.link = Some(ThoughtLink {
            url: "https://example.com/seeds".to_owned(),
        });
        voice.audio = Some(vec![0, 0, 0, 64, 0, 192]);
        let reading = read_attachments(&voice, &Senses).unwrap();
        assert_eq!(
            note_body(&voice.manifest, &reading),
            Some((
                "https://example.com/seeds".to_owned(),
                "Grow tomatoes this spring\n\nhttps://example.com/seeds".to_owned()
            ))
        );
    }

    #[test]
    fn the_note_keeps_the_photos_words_without_vision_labels() {
        let photo = photo();
        let reading = read_attachments(&photo, &Senses).unwrap();
        assert_eq!(
            note_body(&photo.manifest, &reading).map(|(_, body)| body),
            Some("Garden plans: tomatoes this spring".to_owned())
        );
        let pictured = Reading {
            spoken: String::new(),
            photos: vec![PhotoReading {
                text: String::new(),
                labels: vec!["sky".to_owned()],
            }],
        };
        assert_eq!(note_body(&photo.manifest, &pictured), None);
    }

    #[test]
    fn a_remote_filing_is_not_replaced_but_an_unsorted_placeholder_can_be() {
        let mut state = SorterState::default();
        state
            .cards
            .insert("remote".to_owned(), card_on(0, "Garden", true));
        state
            .cards
            .insert("placeholder".to_owned(), card_on(1, "Unsorted", false));
        let references = [reference(0), reference(1)];
        let plan = state.plan(&references, 0, |_| true);
        assert_eq!(
            plan.due
                .iter()
                .map(|due| due.object_id.as_str())
                .collect::<Vec<_>>(),
            ["thought_1"]
        );
    }

    /// The pass used to read the state file once and ask the store about
    /// every card once for each thought: 300 thoughts and 300 cards cost
    /// about 90,000 lookups every 5 seconds. Now a pass with nothing due
    /// asks nothing, and one with work asks once per card.
    #[test]
    fn a_pass_asks_the_store_once_per_card_and_only_when_a_thought_is_due() {
        let mut state = SorterState::default();
        let references: Vec<_> = (0..300).map(reference).collect();
        for index in 0..300 {
            state
                .cards
                .insert(format!("card_{index}"), card_on(index, "Garden", true));
        }
        let lookups = std::cell::Cell::new(0);
        let live = |_: &str| {
            lookups.set(lookups.get() + 1);
            true
        };
        assert!(state.plan(&references, 0, live).due.is_empty());
        assert_eq!(lookups.get(), 0);
        state.cards.remove("card_299");
        let plan = state.plan(&references, 0, live);
        assert_eq!(plan.due.len(), 1);
        assert_eq!(plan.clusters.len(), 1);
        assert_eq!(lookups.get(), 299);
    }

    #[test]
    fn a_thought_that_keeps_failing_is_left_in_unsorted_after_the_cap() {
        let mut state = SorterState::default();
        let references = [reference(0)];
        state
            .readings
            .insert("revision_0".to_owned(), Reading::default());
        for _ in 1..MAX_ATTEMPTS {
            state.failed("revision_0", 0);
        }
        assert!(state.readings.contains_key("revision_0"));
        assert_eq!(state.plan(&references, i64::MAX, |_| true).due.len(), 1);
        state.failed("revision_0", 0);
        assert!(state.stopped.contains("revision_0"));
        assert!(!state.readings.contains_key("revision_0"));
        assert!(state.plan(&references, i64::MAX, |_| true).due.is_empty());
    }

    #[test]
    fn private_thought_content_uses_the_local_notes_engine_never_the_cloud_slot() {
        let processing = MeetingProcessingService::new(None);
        let local = Arc::new(LocalModel::default());
        processing.set_text_generators(local.clone(), Arc::new(CloudModel));
        let generator = processing.local_text_generator().unwrap();
        let mut private = content();
        private.manifest.text = "Personal garden plans kept on this Mac".to_owned();
        let card = card_for(&private, None, &[], generator.as_ref(), "mac", 123).unwrap();
        assert_eq!(card.sorter.unwrap().model, "fixture-local-notes");
        assert!(local.evidence.lock().unwrap()[0].contains("Personal garden plans"));
    }

    #[test]
    fn the_card_object_id_matches_the_server_sorters() {
        assert_eq!(
            card_object_id("thought_01"),
            "rr5feVJMsR91jsgmiCzMXec5MRKmVyEs7KmBTQwyx4M"
        );
    }

    #[test]
    fn the_mac_reads_a_card_sealed_by_the_typescript_sorter() {
        #[derive(Deserialize)]
        struct Fixture {
            object_id: String,
            revision_id: String,
            manifest: String,
            card: CardManifest,
        }
        let fixture: Fixture =
            serde_json::from_str(include_str!("../../../sorter/test/fixtures/card-v1.json"))
                .unwrap();
        let encrypted = base64_url_decode(&fixture.manifest).unwrap();
        let context = ObjectRevisionCryptoContext {
            vault_id: "fixture_vault_0001",
            object_id: &fixture.object_id,
            revision_id: &fixture.revision_id,
            index: 0,
            total: 1,
            content_kind: ObjectContentKind::Manifest,
            source_format: CARD_SOURCE_FORMAT,
        };
        assert_eq!(
            open_card(&[7; 32], &context, &encrypted).unwrap(),
            Some(fixture.card)
        );
    }

    #[test]
    fn a_non_ascii_cluster_name_is_not_dropped_into_unsorted() {
        assert_ne!(resolve_cluster("Сад", &[]).key, UNSORTED_KEY);
    }

    #[test]
    fn a_note_too_long_for_the_local_model_is_cut_to_fit() {
        let thought = content();
        let header = evidence(&thought.manifest, "", false, &[], usize::MAX);
        let budget = header.len() + 9;
        let cut = evidence(&thought.manifest, "one two three", false, &[], budget);
        assert_eq!(cut, format!("{header}one two t"));
    }

    #[test]
    fn an_unknown_audio_format_is_left_unread_not_sent_elsewhere() {
        assert!(thought_audio_samples("unknown", 16_000, 1, &[0, 0]).is_none());
    }
}
