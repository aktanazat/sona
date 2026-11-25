use super::{
    acknowledge_remote_revision, adjusted_now_ms, map_store_error,
    memory_index::phone_sync_directory,
    open_object_revision_payload,
    revision_upload::{self, StagedRevision, UploadRefused},
    sha256_base64url, utc_now_ms, verify_writer_signature, write_staged_file, CloudAccess,
    CloudClientError, CloudErrorCode, CloudRuntimeError, CloudSyncRuntime, ObjectContentKind,
    ObjectRevisionCryptoContext, ObjectRevisionEnvelope,
};
use crate::{
    agent_panel::{
        protocol::{SonaAgentChatRoleV1, SonaAgentChatTurnV1},
        PhoneTurnFailure,
    },
    settings,
};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use zeroize::Zeroizing;

pub(super) const ASK_SOURCE_FORMAT: &str = "sona-agent-ask-v1";
const REPLY_SOURCE_FORMAT: &str = "sona-agent-reply-v1";
const STATE_FILE: &str = "agent-asks.json";
const ASK_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;
const TURN_DEADLINE: Duration = Duration::from_secs(240);
const MAX_QUESTION_BYTES: usize = 8 * 1024;
const MAX_CONVERSATION_ID_CHARS: usize = 64;
const MAX_RECENT_TURNS: usize = 8;

/// A question the phone sealed into the vault. Fields are in key order, so
/// it serializes with sorted keys as the phone writes it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentAsk {
    asked_at_utc_ms: i64,
    conversation_id: String,
    device_id: String,
    format_version: u32,
    kind: String,
    question: String,
    recent_turns: Vec<AskTurn>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AskTurn {
    message: String,
    role: SonaAgentChatRoleV1,
}

impl AgentAsk {
    fn validate(
        &self,
        envelope: &ObjectRevisionEnvelope,
        this_device_id: &str,
    ) -> Result<(), CloudRuntimeError> {
        let conversation_chars = self.conversation_id.chars().count();
        if self.format_version != 1
            || self.kind != "agent_ask"
            || self.device_id != envelope.writer_device_id
            || envelope.writer_device_id == this_device_id
            || envelope.chunk_count != 1
            || self.asked_at_utc_ms < 0
            || !(1..=MAX_CONVERSATION_ID_CHARS).contains(&conversation_chars)
            || self.question.len() > MAX_QUESTION_BYTES
            || self.question.trim().is_empty()
            || self.recent_turns.len() > MAX_RECENT_TURNS
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        Ok(())
    }

    fn expired(&self, now_utc_ms: i64) -> bool {
        self.asked_at_utc_ms < now_utc_ms.saturating_sub(ASK_LIFETIME_MS)
    }

    /// Drop the question and the earlier turns, keeping what replay
    /// protection needs. Whether there was text to drop.
    fn forget_text(&mut self) -> bool {
        let had = !self.question.is_empty() || !self.recent_turns.is_empty();
        self.question = String::new();
        self.recent_turns = Vec::new();
        had
    }
}

/// The Mac's answer object, one per ask. Fields are in key order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AgentReply {
    answer: Option<String>,
    ask_object_id: String,
    device_id: String,
    failure: Option<ReplyFailure>,
    format_version: u32,
    kind: String,
    state: ReplyState,
    updated_at_utc_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReplyState {
    Working,
    Answered,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReplyFailure {
    AgentOff,
    Unpaired,
    Unreachable,
    TimedOut,
    Failed,
}

impl AgentReply {
    fn new(
        ask_object_id: &str,
        device_id: &str,
        outcome: Option<Result<String, PhoneTurnFailure>>,
        updated_at_utc_ms: i64,
    ) -> Self {
        let (state, answer, failure) = match outcome {
            None => (ReplyState::Working, None, None),
            Some(Ok(answer)) => (ReplyState::Answered, Some(answer), None),
            Some(Err(failure)) => (
                ReplyState::Failed,
                None,
                Some(match failure {
                    PhoneTurnFailure::AgentOff => ReplyFailure::AgentOff,
                    PhoneTurnFailure::Unpaired => ReplyFailure::Unpaired,
                    PhoneTurnFailure::Unreachable => ReplyFailure::Unreachable,
                    PhoneTurnFailure::TimedOut => ReplyFailure::TimedOut,
                    PhoneTurnFailure::Failed => ReplyFailure::Failed,
                }),
            ),
        };
        Self {
            answer,
            ask_object_id: ask_object_id.to_owned(),
            device_id: device_id.to_owned(),
            failure,
            format_version: 1,
            kind: "agent_reply".to_owned(),
            state,
            updated_at_utc_ms,
        }
    }
}

fn reply_object_id(ask_object_id: &str) -> String {
    sha256_base64url(format!("{REPLY_SOURCE_FORMAT}:{ask_object_id}").as_bytes())
}

/// Every ask this Mac took, in the order it pulled them. An ask is resumed
/// from its phase after a restart; a `Done` entry keeps only its id and time
/// until the ask is a day old, so a replayed revision is never answered twice.
#[derive(Default, Serialize, Deserialize)]
struct AskState {
    vault_id: String,
    device_id: String,
    asks: Vec<RememberedAsk>,
}

#[derive(Serialize, Deserialize)]
struct RememberedAsk {
    object_id: String,
    ask: AgentAsk,
    progress: Progress,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum Progress {
    Remembered,
    PublishingWorking {
        revision: StagedRevision,
    },
    Working,
    PublishingFinal {
        reply: AgentReply,
        revision: StagedRevision,
    },
    Done,
}

#[derive(Debug, Eq, PartialEq)]
enum Step {
    Expire,
    PublishWorking,
    Upload,
    RunTurn,
}

impl AskState {
    fn milestones(&self) -> Vec<(String, u8)> {
        self.asks
            .iter()
            .map(|entry| (entry.object_id.clone(), entry.progress.milestone()))
            .collect()
    }

    /// The next step of every open ask, in pull order. Each ask moves on its
    /// own, so one whose upload keeps failing never holds up the others; past
    /// its lifetime every unfinished ask expires, a pending answer included.
    fn open_steps(&self, now_utc_ms: i64) -> Vec<(String, Step)> {
        self.asks
            .iter()
            .filter_map(|entry| {
                let step = match &entry.progress {
                    Progress::Done => return None,
                    _ if entry.ask.expired(now_utc_ms) => Step::Expire,
                    Progress::Remembered => Step::PublishWorking,
                    Progress::PublishingWorking { .. } | Progress::PublishingFinal { .. } => {
                        Step::Upload
                    }
                    Progress::Working => Step::RunTurn,
                };
                Some((entry.object_id.clone(), step))
            })
            .collect()
    }

    fn entry(&mut self, object_id: &str) -> Option<&mut RememberedAsk> {
        self.asks
            .iter_mut()
            .find(|entry| entry.object_id == object_id)
    }

    fn remember(&mut self, object_id: &str, ask: AgentAsk) -> bool {
        if self.asks.iter().any(|entry| entry.object_id == object_id) {
            return false;
        }
        self.asks.push(RememberedAsk {
            object_id: object_id.to_owned(),
            ask,
            progress: Progress::Remembered,
        });
        true
    }

    fn forget_unstarted(&mut self, object_id: &str) -> bool {
        let before = self.asks.len();
        self.asks.retain(|entry| {
            entry.object_id != object_id || !matches!(entry.progress, Progress::Remembered)
        });
        self.asks.len() != before
    }

    /// Drop a finished ask's text at once and its entry once it is a day old.
    /// Whether anything changed.
    fn tidy(&mut self, now_utc_ms: i64) -> bool {
        let before = self.asks.len();
        self.asks.retain(|entry| {
            !(matches!(entry.progress, Progress::Done) && entry.ask.expired(now_utc_ms))
        });
        let mut changed = self.asks.len() != before;
        for entry in &mut self.asks {
            if matches!(entry.progress, Progress::Done) {
                changed |= entry.ask.forget_text();
            }
        }
        changed
    }
}

/// The asks for this vault and device, tidied, and whether that changed the
/// file. Asks kept for another vault or device are dropped.
fn load(
    directory: &Path,
    vault_id: &str,
    device_id: &str,
    now_utc_ms: i64,
) -> Result<(AskState, bool), CloudRuntimeError> {
    let (mut state, mut changed) = match read_state(directory)? {
        Some(state) if state.vault_id == vault_id && state.device_id == device_id => (state, false),
        stored => (
            AskState {
                vault_id: vault_id.to_owned(),
                device_id: device_id.to_owned(),
                asks: Vec::new(),
            },
            stored.is_some(),
        ),
    };
    changed |= state.tidy(now_utc_ms);
    Ok((state, changed))
}

fn read_state(directory: &Path) -> Result<Option<AskState>, CloudRuntimeError> {
    match fs::read(directory.join(STATE_FILE)) {
        Ok(bytes) => serde_json::from_slice::<AskState>(&Zeroizing::new(bytes))
            .map(Some)
            .map_err(|_| CloudRuntimeError::IntegrityFailure),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(CloudRuntimeError::File),
    }
}

/// Tidy the file for whichever vault wrote it and say whether any ask still
/// needs work. Read before the keychain is, so a Mac with nothing to answer
/// costs one small file read.
fn tidy_on_disk(directory: &Path, now_utc_ms: i64) -> Result<bool, CloudRuntimeError> {
    let Some(mut state) = read_state(directory)? else {
        return Ok(false);
    };
    if state.tidy(now_utc_ms) {
        persist(directory, &state)?;
    }
    Ok(state
        .asks
        .iter()
        .any(|entry| !matches!(entry.progress, Progress::Done)))
}

/// Write the asks, or delete the file once none are left.
fn persist(directory: &Path, state: &AskState) -> Result<(), CloudRuntimeError> {
    if state.asks.is_empty() {
        return remove_state(directory);
    }
    fs::create_dir_all(directory).map_err(|_| CloudRuntimeError::File)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
        .map_err(|_| CloudRuntimeError::File)?;
    write_staged_file(
        directory,
        STATE_FILE,
        &Zeroizing::new(serde_json::to_vec(state).map_err(|_| CloudRuntimeError::File)?),
    )
}

fn remove_state(directory: &Path) -> Result<(), CloudRuntimeError> {
    match fs::remove_file(directory.join(STATE_FILE)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(CloudRuntimeError::File),
    }
}

/// Serializes the sync loop's writes (remember, forget) with the worker's,
/// and keeps one worker running.
#[derive(Default)]
pub(super) struct AskQueue {
    gate: Mutex<()>,
    running: AtomicBool,
}

struct Running<'a>(&'a AtomicBool);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(super) fn open_ask(
    vault_root: &[u8],
    context: &ObjectRevisionCryptoContext<'_>,
    ciphertext: &[u8],
) -> Result<Option<AgentAsk>, CloudRuntimeError> {
    let context = ObjectRevisionCryptoContext {
        source_format: ASK_SOURCE_FORMAT,
        ..*context
    };
    let Ok(bytes) = open_object_revision_payload(vault_root, &context, ciphertext) else {
        return Ok(None);
    };
    serde_json::from_slice(&Zeroizing::new(bytes))
        .map(Some)
        .map_err(|_| CloudRuntimeError::IntegrityFailure)
}

/// Whether a revision is an answer some Mac wrote. Nothing here reads one.
pub(super) fn is_reply(
    vault_root: &[u8],
    context: &ObjectRevisionCryptoContext<'_>,
    ciphertext: &[u8],
) -> bool {
    let context = ObjectRevisionCryptoContext {
        source_format: REPLY_SOURCE_FORMAT,
        ..*context
    };
    open_object_revision_payload(vault_root, &context, ciphertext).is_ok()
}

impl CloudSyncRuntime {
    fn edit_asks(
        &self,
        access: &CloudAccess,
        edit: impl FnOnce(&mut AskState) -> Result<bool, CloudRuntimeError>,
    ) -> Result<(), CloudRuntimeError> {
        let directory = phone_sync_directory(&self.app).map_err(|_| CloudRuntimeError::File)?;
        let _guard = self
            .agent_asks
            .gate
            .lock()
            .map_err(|_| CloudRuntimeError::Storage)?;
        let now = adjusted_now_ms(access.state.clock_offset_ms);
        let (mut state, tidied) = load(
            &directory,
            &access.state.vault_id,
            &access.state.device_id,
            now,
        )?;
        if edit(&mut state)? | tidied {
            state.tidy(now);
            persist(&directory, &state)?;
        }
        Ok(())
    }

    pub(super) async fn download_agent_ask(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        envelope: &ObjectRevisionEnvelope,
        ask: AgentAsk,
    ) -> Result<AgentAsk, CloudRuntimeError> {
        ask.validate(envelope, &access.state.device_id)?;
        let signing_key = self.writer_signing_key(access, envelope).await?;
        let descriptors = self
            .download_chunks(access, object_id, revision_id, envelope, |index, bytes| {
                let plaintext = open_object_revision_payload(
                    &*access.keys.vault_root,
                    &ObjectRevisionCryptoContext {
                        vault_id: &access.state.vault_id,
                        object_id,
                        revision_id,
                        index,
                        total: u64::from(envelope.chunk_count),
                        content_kind: ObjectContentKind::Chunk,
                        source_format: ASK_SOURCE_FORMAT,
                    },
                    &bytes,
                )
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                if plaintext.is_empty() {
                    Ok(())
                } else {
                    Err(CloudRuntimeError::IntegrityFailure)
                }
            })
            .await?;
        verify_writer_signature(
            &access.state.vault_id,
            object_id,
            revision_id,
            envelope,
            &descriptors,
            &signing_key,
        )?;
        Ok(ask)
    }

    /// Keep an ask for the worker and mark its revision seen. The relay is not
    /// touched here: the sync loop never waits on a turn.
    pub(super) fn remember_agent_ask(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        sequence: u64,
        ask: AgentAsk,
    ) -> Result<(), CloudRuntimeError> {
        if !ask.expired(adjusted_now_ms(access.state.clock_offset_ms)) {
            self.edit_asks(access, |state| Ok(state.remember(object_id, ask)))?;
        }
        acknowledge_remote_revision(&access.store, object_id, revision_id, sequence)
    }

    pub(super) fn forget_unstarted_agent_ask(
        &self,
        access: &CloudAccess,
        object_id: &str,
    ) -> Result<(), CloudRuntimeError> {
        self.edit_asks(access, |state| Ok(state.forget_unstarted(object_id)))
    }

    /// Start answering remembered asks unless that is already happening.
    pub(super) fn answer_agent_asks(self: &Arc<Self>) {
        if self.agent_asks.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let runtime = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let _running = Running(&runtime.agent_asks.running);
            if let Err(error) = runtime.answer_remembered_asks().await {
                log::warn!("A question from the phone is not answered yet: {error:?}");
            }
        });
    }

    async fn answer_remembered_asks(&self) -> Result<(), CloudRuntimeError> {
        let directory = phone_sync_directory(&self.app).map_err(|_| CloudRuntimeError::File)?;
        let open = {
            let _guard = self
                .agent_asks
                .gate
                .lock()
                .map_err(|_| CloudRuntimeError::Storage)?;
            if !settings::get_settings(&self.app).cloud_sync.enabled {
                return remove_state(&directory);
            }
            tidy_on_disk(&directory, utc_now_ms())?
        };
        if !open {
            return Ok(());
        }
        loop {
            let Some(access) = self.active_access().await? else {
                return Ok(());
            };
            if !self.request_permitted(&access.state) {
                return Ok(());
            }
            let now = adjusted_now_ms(access.state.clock_offset_ms);
            let mut steps = Vec::new();
            let mut before = Vec::new();
            self.edit_asks(&access, |state| {
                steps = state.open_steps(now);
                before = state.milestones();
                Ok(false)
            })?;
            for (object_id, step) in steps {
                if let Err(error) = self.take_step(&access, &object_id, step).await {
                    log::warn!("A question from the phone is not answered yet: {error:?}");
                }
            }
            let mut after = Vec::new();
            self.edit_asks(&access, |state| {
                after = state.milestones();
                Ok(false)
            })?;
            if !advanced(&before, &after) {
                return Ok(());
            }
        }
    }

    async fn take_step(
        &self,
        access: &CloudAccess,
        object_id: &str,
        step: Step,
    ) -> Result<(), CloudRuntimeError> {
        match step {
            Step::Expire => self.edit_asks(access, |state| {
                if let Some(entry) = state.entry(object_id) {
                    entry.progress = Progress::Done;
                }
                Ok(true)
            }),
            Step::PublishWorking => {
                let reply = AgentReply::new(
                    object_id,
                    &access.state.device_id,
                    None,
                    adjusted_now_ms(access.state.clock_offset_ms),
                );
                let mut revision = stage_reply(access, &reply)?;
                // The claim is a create: the service commits only the first
                // Mac's, even when this Mac already pulled another's claim.
                revision.base_revision_id = None;
                self.edit_asks(access, |state| {
                    let Some(entry) = state.entry(object_id) else {
                        return Ok(false);
                    };
                    if !matches!(entry.progress, Progress::Remembered) {
                        return Ok(false);
                    }
                    entry.progress = Progress::PublishingWorking { revision };
                    Ok(true)
                })?;
                self.upload_reply(access, object_id).await
            }
            Step::Upload => self.upload_reply(access, object_id).await,
            Step::RunTurn => {
                let mut ask = None;
                self.edit_asks(access, |state| {
                    ask = state.entry(object_id).map(|entry| entry.ask.clone());
                    Ok(false)
                })?;
                let Some(ask) = ask else { return Ok(()) };
                let recent_turns = ask
                    .recent_turns
                    .into_iter()
                    .map(|turn| SonaAgentChatTurnV1 {
                        role: turn.role,
                        message: turn.message,
                        outcome: None,
                    })
                    .collect();
                let outcome = crate::agent_panel::run_phone_turn(
                    &self.app,
                    &ask.question,
                    recent_turns,
                    TURN_DEADLINE,
                )
                .await;
                let reply = AgentReply::new(
                    object_id,
                    &access.state.device_id,
                    Some(outcome),
                    adjusted_now_ms(access.state.clock_offset_ms),
                );
                let revision = stage_reply(access, &reply)?;
                self.edit_asks(access, |state| {
                    let Some(entry) = state.entry(object_id) else {
                        return Ok(false);
                    };
                    if !matches!(entry.progress, Progress::Working) {
                        return Ok(false);
                    }
                    entry.progress = Progress::PublishingFinal { reply, revision };
                    Ok(true)
                })
            }
        }
    }

    /// Send the staged revision byte for byte, then move the ask on. A base the
    /// service refuses is staged again from the current head; a final answer
    /// whose reply object the phone already deleted is dropped.
    async fn upload_reply(
        &self,
        access: &CloudAccess,
        object_id: &str,
    ) -> Result<(), CloudRuntimeError> {
        let mut revision = None;
        self.edit_asks(access, |state| {
            revision = match state.entry(object_id).map(|entry| &entry.progress) {
                Some(
                    Progress::PublishingWorking { revision }
                    | Progress::PublishingFinal { revision, .. },
                ) => Some(revision.clone()),
                _ => None,
            };
            Ok(false)
        })?;
        let Some(mut revision) = revision else {
            return Ok(());
        };
        let uploaded =
            revision_upload::upload(self, access, REPLY_SOURCE_FORMAT, &mut revision).await;
        let (outcome, refused) = match uploaded {
            Ok(()) => (Uploaded::Committed, None),
            Err(UploadRefused::Retry(error)) => return Err(error),
            Err(UploadRefused::Restage(error)) if is_stale(&error) => {
                (Uploaded::Stale, Some(error))
            }
            Err(UploadRefused::Restage(error)) => (Uploaded::Restage, Some(error)),
        };
        let reply_deleted = access
            .store
            .cloud_head(&reply_object_id(object_id))
            .map_err(map_store_error)?
            .is_some_and(|head| head.tombstone);
        let mut done = false;
        self.edit_asks(access, |state| {
            let Some(entry) = state.entry(object_id) else {
                return Ok(false);
            };
            let progress = std::mem::replace(&mut entry.progress, Progress::Done);
            entry.progress = progress
                .after_upload(outcome, reply_deleted, |reply| stage_reply(access, reply))?;
            done = matches!(entry.progress, Progress::Done);
            Ok(true)
        })?;
        refused.filter(|_| !done).map_or(Ok(()), Err)
    }
}

/// How an upload of a staged reply ended.
#[derive(Clone, Copy)]
enum Uploaded {
    Committed,
    /// The object's head is not the staged base: for a claim, another Mac
    /// committed its claim first.
    Stale,
    /// The service dropped the upload; stage it again.
    Restage,
}

fn is_stale(error: &CloudRuntimeError) -> bool {
    matches!(
        error,
        CloudRuntimeError::Client(CloudClientError::Api(error))
            if matches!(error.code, CloudErrorCode::StaleRevision)
    )
}

/// Whether any ask got further. Staging or restaging a revision is not a step
/// forward: only a committed claim, a finished turn, a committed answer or an end.
fn advanced(before: &[(String, u8)], after: &[(String, u8)]) -> bool {
    after.iter().any(|(object_id, milestone)| {
        before
            .iter()
            .find(|(id, _)| id == object_id)
            .is_some_and(|(_, earlier)| milestone > earlier)
    })
}

impl Progress {
    fn milestone(&self) -> u8 {
        match self {
            Progress::Remembered | Progress::PublishingWorking { .. } => 0,
            Progress::Working => 1,
            Progress::PublishingFinal { .. } => 2,
            Progress::Done => 3,
        }
    }

    /// Where an ask goes once its reply upload ended. A final answer whose
    /// reply object the phone already deleted is dropped; any other refused
    /// final answer is staged again from the current head.
    fn after_upload(
        self,
        outcome: Uploaded,
        reply_deleted: bool,
        restage: impl FnOnce(&AgentReply) -> Result<StagedRevision, CloudRuntimeError>,
    ) -> Result<Self, CloudRuntimeError> {
        Ok(match (self, outcome) {
            (Progress::PublishingWorking { .. }, Uploaded::Committed) => Progress::Working,
            (Progress::PublishingWorking { .. }, Uploaded::Stale) => Progress::Done,
            (Progress::PublishingWorking { .. }, Uploaded::Restage) => Progress::Remembered,
            (Progress::PublishingFinal { .. }, Uploaded::Committed) => Progress::Done,
            (Progress::PublishingFinal { .. }, _) if reply_deleted => Progress::Done,
            (Progress::PublishingFinal { reply, .. }, _) => {
                let revision = restage(&reply)?;
                Progress::PublishingFinal { reply, revision }
            }
            (progress, _) => progress,
        })
    }
}

fn stage_reply(
    access: &CloudAccess,
    reply: &AgentReply,
) -> Result<StagedRevision, CloudRuntimeError> {
    let manifest =
        Zeroizing::new(serde_json::to_vec(reply).map_err(|_| CloudRuntimeError::IntegrityFailure)?);
    revision_upload::stage(
        access,
        REPLY_SOURCE_FORMAT,
        reply_object_id(&reply.ask_object_id),
        &manifest,
        &[&[]],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASK_FIXTURE: &str = include_str!("../../../mobile/SonaTests/Fixtures/agent-ask-v1.json");
    const REPLY_FIXTURE: &str =
        include_str!("../../../mobile/SonaTests/Fixtures/agent-reply-v1.json");

    fn fixture_ask() -> AgentAsk {
        serde_json::from_str(ASK_FIXTURE).expect("ask fixture")
    }

    fn envelope(writer_device_id: &str) -> ObjectRevisionEnvelope {
        ObjectRevisionEnvelope {
            object_id: "ask_fixture_object".to_owned(),
            revision_id: "revision".to_owned(),
            parent_revision_id: None,
            manifest_sha256: String::new(),
            chunk_count: 1,
            total_bytes: 28,
            crypto_version: 1,
            writer_device_id: writer_device_id.to_owned(),
            writer_signature: String::new(),
        }
    }

    fn remembered(object_id: &str, asked_at_utc_ms: i64, progress: Progress) -> RememberedAsk {
        RememberedAsk {
            object_id: object_id.to_owned(),
            ask: AgentAsk {
                asked_at_utc_ms,
                ..fixture_ask()
            },
            progress,
        }
    }

    #[test]
    fn fixtures_decode_and_the_reply_encodes_byte_for_byte() {
        let ask = fixture_ask();
        assert_eq!(ask.question, "What did Nolan and I leave open?");
        assert_eq!(ask.recent_turns[1].role, SonaAgentChatRoleV1::Assistant);
        assert_eq!(serde_json::to_string(&ask).expect("ask"), ASK_FIXTURE);

        let reply: AgentReply = serde_json::from_str(REPLY_FIXTURE).expect("reply fixture");
        let built = AgentReply::new(
            "ask_fixture_object",
            "dev_mac_fixture",
            Some(Ok("You owe Nolan the pricing deck by Friday.".to_owned())),
            1_763_600_005_000,
        );
        assert_eq!(built, reply);
        assert_eq!(serde_json::to_string(&built).expect("reply"), REPLY_FIXTURE);
    }

    #[test]
    fn an_ask_is_refused_unless_its_writer_asked_it() {
        let ask = fixture_ask();
        assert!(ask
            .validate(&envelope("dev_phone_fixture"), "dev_mac_fixture")
            .is_ok());
        assert!(matches!(
            ask.validate(&envelope("dev_other_phone"), "dev_mac_fixture"),
            Err(CloudRuntimeError::IntegrityFailure)
        ));
        assert!(matches!(
            ask.validate(&envelope("dev_phone_fixture"), "dev_phone_fixture"),
            Err(CloudRuntimeError::IntegrityFailure)
        ));
    }

    fn first_step(state: &AskState, now: i64) -> Option<(String, Step)> {
        state.open_steps(now).into_iter().next()
    }

    fn staged() -> StagedRevision {
        StagedRevision {
            object_id: reply_object_id("ask"),
            revision_id: "revision".to_owned(),
            upload_id: "upload".to_owned(),
            base_revision_id: None,
            manifest: Vec::new(),
            chunks: Vec::new(),
        }
    }

    fn pending_answer() -> Progress {
        Progress::PublishingFinal {
            reply: AgentReply::new("ask", "mac", Some(Ok("An answer".to_owned())), 1),
            revision: staged(),
        }
    }

    #[test]
    fn a_restart_resumes_an_unanswered_ask_and_never_reruns_an_answered_one() {
        let directory = tempfile::tempdir().expect("directory");
        let now = 1_763_600_100_000;
        let (mut state, _) = load(directory.path(), "vault", "mac", now).expect("empty state");
        state
            .asks
            .push(remembered("answered", now - 1_000, Progress::Done));
        state
            .asks
            .push(remembered("working", now - 500, Progress::Working));
        state
            .asks
            .push(remembered("later", now - 100, Progress::Remembered));
        persist(directory.path(), &state).expect("persist");

        let (mut restarted, _) = load(directory.path(), "vault", "mac", now).expect("reload");
        assert_eq!(
            first_step(&restarted, now),
            Some(("working".to_owned(), Step::RunTurn))
        );
        assert!(!restarted.remember("answered", fixture_ask()));
        restarted.entry("working").expect("working").progress = Progress::Done;
        assert_eq!(
            first_step(&restarted, now),
            Some(("later".to_owned(), Step::PublishWorking))
        );
        restarted.entry("later").expect("later").progress = Progress::Done;
        assert_eq!(first_step(&restarted, now), None);
    }

    #[test]
    fn a_tombstone_drops_only_an_ask_not_yet_started() {
        let now = 1_763_600_100_000;
        let mut state = AskState::default();
        state
            .asks
            .push(remembered("unstarted", now, Progress::Remembered));
        state
            .asks
            .push(remembered("thinking", now, Progress::Working));
        assert!(state.forget_unstarted("unstarted"));
        assert!(!state.forget_unstarted("thinking"));
        assert_eq!(
            first_step(&state, now),
            Some(("thinking".to_owned(), Step::RunTurn))
        );
    }

    #[test]
    fn an_ask_older_than_a_day_is_not_answered() {
        let now = 1_763_600_100_000;
        let mut state = AskState::default();
        state.asks.push(remembered(
            "stale",
            now - ASK_LIFETIME_MS - 1,
            Progress::Remembered,
        ));
        assert_eq!(
            first_step(&state, now),
            Some(("stale".to_owned(), Step::Expire))
        );
    }

    #[test]
    fn an_answer_that_cannot_upload_holds_up_no_other_ask_and_expires() {
        let now = 1_763_600_100_000;
        let mut state = AskState::default();
        state
            .asks
            .push(remembered("stuck", now - 1_000, pending_answer()));
        state
            .asks
            .push(remembered("next", now - 500, Progress::Remembered));
        assert_eq!(
            state.open_steps(now),
            [
                ("stuck".to_owned(), Step::Upload),
                ("next".to_owned(), Step::PublishWorking),
            ]
        );
        assert_eq!(
            first_step(&state, now + ASK_LIFETIME_MS),
            Some(("stuck".to_owned(), Step::Expire))
        );
    }

    #[test]
    fn a_finished_ask_keeps_no_text_and_the_file_goes_with_the_last_ask() {
        let directory = tempfile::tempdir().expect("directory");
        let file = directory.path().join(STATE_FILE);
        let now = 1_763_600_100_000;
        let mut state = AskState {
            vault_id: "vault".to_owned(),
            device_id: "mac".to_owned(),
            asks: vec![remembered("answered", now, Progress::Done)],
        };
        persist(directory.path(), &state).expect("persist");
        assert_eq!(
            fs::metadata(&file).expect("file").permissions().mode() & 0o777,
            0o600
        );

        let (tidied, changed) = load(directory.path(), "vault", "mac", now).expect("load");
        assert!(changed);
        persist(directory.path(), &tidied).expect("persist");
        let question = fixture_ask().question;
        assert!(!String::from_utf8(fs::read(&file).expect("read"))
            .expect("utf8")
            .contains(&question));

        let (gone, changed) =
            load(directory.path(), "vault", "mac", now + ASK_LIFETIME_MS + 1).expect("load");
        assert!(changed && gone.asks.is_empty());
        persist(directory.path(), &gone).expect("persist");
        assert!(!file.exists());

        state.asks = vec![remembered("open", now, Progress::Working)];
        persist(directory.path(), &state).expect("persist");
        let (other, changed) = load(directory.path(), "other", "mac", now).expect("load");
        assert!(changed);
        persist(directory.path(), &other).expect("persist");
        assert!(!file.exists());
    }

    #[test]
    fn only_the_mac_whose_claim_commits_answers() {
        let now = 1_763_600_100_000;
        let claim = || Progress::PublishingWorking { revision: staged() };
        let no_restage = |_: &AgentReply| -> Result<StagedRevision, CloudRuntimeError> {
            panic!("a claim is never restaged as a final answer")
        };
        let mut first = AskState::default();
        let mut second = AskState::default();
        first.asks.push(remembered(
            "ask",
            now,
            claim()
                .after_upload(Uploaded::Committed, false, no_restage)
                .expect("committed"),
        ));
        second.asks.push(remembered(
            "ask",
            now,
            claim()
                .after_upload(Uploaded::Stale, false, no_restage)
                .expect("stale"),
        ));
        assert_eq!(
            first_step(&first, now),
            Some(("ask".to_owned(), Step::RunTurn))
        );
        assert_eq!(first_step(&second, now), None);

        let mut dropped = AskState::default();
        dropped.asks.push(remembered(
            "ask",
            now,
            claim()
                .after_upload(Uploaded::Restage, false, no_restage)
                .expect("restage"),
        ));
        assert_eq!(
            first_step(&dropped, now),
            Some(("ask".to_owned(), Step::PublishWorking))
        );
    }

    #[test]
    fn staging_or_restaging_is_not_progress_but_a_committed_claim_is() {
        let now = 1_763_600_100_000;
        let state = |claim: Progress, answer: Progress| AskState {
            asks: vec![
                remembered("claim", now, claim),
                remembered("answer", now, answer),
            ],
            ..AskState::default()
        };
        let before = state(Progress::Remembered, pending_answer()).milestones();
        let restaged = state(
            Progress::PublishingWorking { revision: staged() },
            pending_answer(),
        )
        .milestones();
        assert!(!advanced(&before, &restaged));
        let claimed = state(Progress::Working, pending_answer()).milestones();
        assert!(advanced(&restaged, &claimed));
        let answered = state(Progress::Working, Progress::Done).milestones();
        assert!(advanced(&claimed, &answered));
    }
}
