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
    phone_thought_manifest::ThoughtManifest,
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
use crate::scratchpad::phone_import;
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
    /// An engine failed or was busy; tried again later.
    Retry,
}

/// Whether an engine on this Mac failed for now; a picture or a recording
/// with nothing in it is `Ok` with empty text.
#[derive(Debug)]
pub(super) struct SenseFailed;

/// The engines on this Mac that turn a thought's attachments into words.
pub(super) trait ThoughtSenses: Send + Sync {
    /// Words spoken in 16 kHz mono audio.
    fn transcribe(&self, samples: &[f32]) -> Result<String, SenseFailed>;
    /// Words in a picture, and what it shows.
    fn read_image(&self, image: &[u8]) -> Result<String, SenseFailed>;
}

/// One thought and its attachments, as they were imported here.
pub(super) struct ThoughtContent {
    pub thought_id: String,
    pub revision_id: String,
    pub manifest: ThoughtManifest,
    pub audio: Option<Vec<u8>>,
    pub images: Vec<Vec<u8>>,
}

/// 16 kHz mono samples of a thought's recording, or `None` for a format this
/// Mac does not decode.
pub(crate) fn thought_audio_samples(
    codec: &str,
    sample_rate_hz: u32,
    channels: u16,
    bytes: &[u8],
) -> Option<Vec<f32>> {
    if codec != "pcm_s16le" || sample_rate_hz != 16_000 || channels != 1 {
        return None;
    }
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    Some(
        pairs
            .iter()
            .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32_768.0)
            .collect(),
    )
}

/// The words a thought carries: its own text, or else what this Mac heard
/// in its recording and saw in its photos. The flag says whether the words
/// came from the attachments.
fn note_text(
    content: &ThoughtContent,
    senses: &dyn ThoughtSenses,
) -> Result<(String, bool), Unfiled> {
    let typed = content.manifest.text.trim();
    if !typed.is_empty() {
        return Ok((typed.to_owned(), false));
    }
    let mut parts = Vec::new();
    if let (Some(audio), Some(bytes)) = (&content.manifest.audio, &content.audio) {
        if let Some(samples) =
            thought_audio_samples(&audio.codec, audio.sample_rate_hz, audio.channels, bytes)
        {
            let heard = senses.transcribe(&samples).map_err(|_| Unfiled::Retry)?;
            if !heard.trim().is_empty() {
                parts.push(format!("Said in the recording: {}", heard.trim()));
            }
        }
    }
    for image in &content.images {
        let seen = senses.read_image(image).map_err(|_| Unfiled::Retry)?;
        if !seen.trim().is_empty() {
            parts.push(format!("In the photo: {}", seen.trim()));
        }
    }
    if parts.is_empty() {
        return Err(Unfiled::NothingToRead);
    }
    Ok((parts.join("\n"), true))
}

fn evidence(
    manifest: &ThoughtManifest,
    text: &str,
    from_attachments: bool,
    clusters: &[CardCluster],
    budget: usize,
) -> Result<String, Unfiled> {
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
    evidence.push_str(text);
    if evidence.len() > budget {
        return Err(Unfiled::Retry);
    }
    Ok(evidence)
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

/// The card for one thought, read and classified only by engines on this
/// Mac.
pub(super) fn card_for(
    content: &ThoughtContent,
    clusters: &[CardCluster],
    senses: &dyn ThoughtSenses,
    generator: &dyn MeetingTextGenerator,
    device_id: &str,
    now_utc_ms: i64,
) -> Result<(CardManifest, Option<String>), Unfiled> {
    let (text, from_attachments) = note_text(content, senses)?;
    let budget = evidence_budget(generator, SYSTEM_PROMPT, OUTPUT_TOKENS);
    let classification = classify(
        generator,
        &evidence(&content.manifest, &text, from_attachments, clusters, budget)?,
    )?;
    let card = CardManifest {
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
    };
    Ok((card, from_attachments.then_some(text)))
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
    attempts: BTreeMap<String, Attempt>,
    /// Thought revisions with nothing in them to read.
    unreadable: BTreeSet<String>,
    pending: Option<PendingCard>,
}

impl SorterState {
    /// Whether a live card already files this thought. `live` says whether a
    /// card object is still in the vault.
    fn files(&self, thought_id: &str, live: impl Fn(&str) -> bool) -> bool {
        self.cards.iter().any(|(object_id, card)| {
            card.thought_id == thought_id && card.settles && live(object_id)
        })
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

    fn read_image(&self, image: &[u8]) -> Result<String, SenseFailed> {
        let text = match screen_text::read(image) {
            Ok(read) => read.text,
            Err(ScreenTextError::Unreadable) => return Ok(String::new()),
            Err(ScreenTextError::Failed) => return Err(SenseFailed),
        };
        let labels = screen_text::labels(image).map_err(|_| SenseFailed)?;
        Ok(match (text.trim().is_empty(), labels.is_empty()) {
            (true, true) => String::new(),
            (true, false) => format!("a picture of {}", labels.join(", ")),
            (false, true) => text,
            (false, false) => format!("{text}\n(a picture of {})", labels.join(", ")),
        })
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
        let mut read = 0;
        for reference in references {
            if read == MAX_THOUGHTS_PER_PASS {
                break;
            }
            let now = adjusted_now_ms(access.state.clock_offset_ms);
            let mut due = false;
            let mut clusters = Vec::new();
            self.edit_sorter(access, |state| {
                let live = |object_id: &str| self.card_is_live(access, object_id);
                due = !state.files(&reference.object_id, live)
                    && !state.unreadable.contains(&reference.revision_id)
                    && state
                        .attempts
                        .get(&reference.object_id)
                        .is_none_or(|attempt| now >= attempt.next_at_utc_ms);
                clusters = state.clusters(live);
                Ok(false)
            })?;
            if !due {
                continue;
            }
            read += 1;
            let content = self
                .thought_content(&reference.object_id, &reference.revision_id)
                .await?;
            let Some(content) = content else { continue };
            let expected_body = phone_import::capture_body(
                &content.manifest.text,
                content.manifest.link.as_ref().map(|link| link.url.as_str()),
                content.manifest.audio.is_some(),
                content.manifest.images.len(),
            );
            let generator = Arc::clone(&generator);
            let senses = Arc::clone(&senses);
            let device_id = access.state.device_id.clone();
            let filed = tauri::async_runtime::spawn_blocking(move || {
                card_for(
                    &content,
                    &clusters,
                    senses.as_ref(),
                    generator.as_ref(),
                    &device_id,
                    now,
                )
            })
            .await
            .map_err(|_| CloudRuntimeError::Storage)?;
            match filed {
                Ok((card, derived_text)) => {
                    if let Some(text) = derived_text {
                        phone_import::enrich(
                            &self.app,
                            &history,
                            &reference,
                            &expected_body,
                            &text,
                            now,
                        )
                        .map_err(|_| CloudRuntimeError::Storage)?;
                    }
                    let plaintext = Zeroizing::new(
                        serde_json::to_vec(&card)
                            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
                    );
                    let revision = revision_upload::stage(
                        access,
                        CARD_SOURCE_FORMAT,
                        card_object_id(&reference.object_id),
                        &plaintext,
                        &[&[]],
                    )?;
                    self.edit_sorter(access, |state| {
                        state.attempts.remove(&reference.object_id);
                        state.pending = Some(PendingCard {
                            thought_id: reference.object_id.clone(),
                            cluster: card.cluster.clone(),
                            revision,
                        });
                        Ok(true)
                    })?;
                    self.upload_pending_card(access).await?;
                }
                Err(Unfiled::NothingToRead) => self.edit_sorter(access, |state| {
                    state.attempts.remove(&reference.object_id);
                    state.unreadable.insert(reference.revision_id.clone());
                    Ok(true)
                })?,
                Err(Unfiled::Retry) => self.edit_sorter(access, |state| {
                    let count = state
                        .attempts
                        .get(&reference.object_id)
                        .map_or(0, |attempt| attempt.count)
                        .saturating_add(1);
                    state.attempts.insert(
                        reference.object_id.clone(),
                        Attempt {
                            count,
                            next_at_utc_ms: now.saturating_add(retry_delay(count)),
                        },
                    );
                    Ok(true)
                })?,
            }
        }
        Ok(())
    }

    /// A thought and its attachments, as imported here; `None` when its
    /// source is gone or moved on since it was listed.
    async fn thought_content(
        &self,
        object_id: &str,
        revision_id: &str,
    ) -> Result<Option<ThoughtContent>, CloudRuntimeError> {
        let Some(source) = self.phone_thought_source(object_id, revision_id).await? else {
            return Ok(None);
        };
        let manifest = source.manifest;
        let typed = !manifest.text.trim().is_empty();
        let audio = if typed || manifest.audio.is_none() {
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
        if !typed {
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
        ThoughtAudio, ThoughtImage, ThoughtOrigin,
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
            assert_eq!(samples, &[0.0, 0.5, -0.5]);
            Ok("Grow tomatoes this spring".to_owned())
        }
        fn read_image(&self, image: &[u8]) -> Result<String, SenseFailed> {
            assert_eq!(image, b"fixture-photo");
            Ok("Garden plans: tomatoes this spring".to_owned())
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
        voice.manifest.audio = Some(ThoughtAudio {
            codec: "pcm_s16le".to_owned(),
            sample_rate_hz: 16_000,
            channels: 1,
            byte_length: 6,
            sha256: String::new(),
            duration_ms: 1,
            chunk_start: 0,
            chunk_count: 1,
        });
        voice.audio = Some(vec![0, 0, 0, 64, 0, 192]);
        let model = LocalModel::default();
        let (card, text) = card_for(&voice, &[], &Senses, &model, "mac", 123).unwrap();
        assert!(text.unwrap().contains("Grow tomatoes"));
        assert_eq!(card.cluster.key, "garden");
        assert_eq!(card.thought_id, voice.thought_id);
        assert!(model.evidence.lock().unwrap()[0].contains("Grow tomatoes this spring"));
    }

    #[test]
    fn a_textless_photo_thought_is_read_and_filed_in_a_cluster() {
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
        let model = LocalModel::default();
        let (card, text) = card_for(&photo, &[], &Senses, &model, "mac", 123).unwrap();
        assert!(text.unwrap().contains("Garden plans"));
        assert_eq!(card.cluster.key, "garden");
        assert!(model.evidence.lock().unwrap()[0].contains("Garden plans: tomatoes this spring"));
    }

    #[test]
    fn a_remote_filing_is_not_replaced_but_an_unsorted_placeholder_can_be() {
        let mut state = SorterState::default();
        state.cards.insert(
            "legacy-random-object".to_owned(),
            KnownCard {
                thought_id: "thought_01".to_owned(),
                cluster: resolve_cluster("Garden", &[]),
                settles: true,
            },
        );
        assert!(state.files("thought_01", |_| true));
        state.cards.get_mut("legacy-random-object").unwrap().settles = false;
        assert!(!state.files("thought_01", |_| true));
        state.cards.get_mut("legacy-random-object").unwrap().settles = true;
        assert!(!state.files("thought_01", |_| false));
    }

    #[test]
    fn private_thought_content_uses_the_local_notes_engine_never_the_cloud_slot() {
        let processing = MeetingProcessingService::new(None);
        let local = Arc::new(LocalModel::default());
        processing.set_text_generators(local.clone(), Arc::new(CloudModel));
        let generator = processing.local_text_generator().unwrap();
        let mut private = content();
        private.manifest.text = "Personal garden plans kept on this Mac".to_owned();
        let (card, text) =
            card_for(&private, &[], &Senses, generator.as_ref(), "mac", 123).unwrap();
        assert!(text.is_none());
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
    fn a_note_too_long_for_the_local_model_is_not_silently_cut() {
        let thought = content();
        assert_eq!(
            evidence(&thought.manifest, "one two three", false, &[], 2),
            Err(Unfiled::Retry)
        );
    }

    #[test]
    fn an_unknown_audio_format_is_left_unread_not_sent_elsewhere() {
        assert!(thought_audio_samples("unknown", 16_000, 1, &[0, 0]).is_none());
    }
}
