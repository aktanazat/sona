//! `sona work` verbs for the names voice matching gives a meeting's
//! speakers: what it suggests, taking a suggestion, and undoing a name it
//! gave by itself. No voice is remembered here.

use super::speakers::{listed, pick_speaker};
use super::{Tracker, WorkAnswer, WriteArgs, OPERATION_NAMESPACE};
use crate::meeting::session::{MeetingSessionManager, MeetingSpeakerRenameRequest};
use crate::meeting::store::voice_identity::{
    MeetingSpeakerIdentifyDisposition, MeetingSpeakerIdentifyRequest,
    MeetingSpeakerIdentifyTarget, VoiceNameResolution,
};
use crate::meeting::types::{MeetingOperationId, MeetingSessionId, OperationResult, SpeakerId};
use crate::query::external::{
    command_error, local_time, meeting_refusal, session_id, store, visible_meeting, ExternalError,
    ExternalErrorCode,
};
use crate::query::{QueryError, QUERY_SCHEMA_VERSION};
use clap::Args;
use serde::Serialize;
use std::fmt::Write as _;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SpeakerVoicesArgs {
    /// The meeting's id, from `meetings`.
    #[arg(value_name = "MEETING")]
    pub meeting: String,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SpeakerVoiceArgs {
    /// The meeting's id, from `meetings`.
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The speaker as the transcript names them, or their id from `speaker-voices`.
    #[arg(value_name = "SPEAKER")]
    pub speaker: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalVoiceSuggestion {
    pub speaker_id: Uuid,
    pub speaker: String,
    /// Absent when the voice is the user's own.
    pub person_id: Option<String>,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSameVoiceMeeting {
    pub meeting_id: String,
    pub title: String,
    pub at_ms: i64,
    pub at: String,
    pub speaker_id: Uuid,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSameVoice {
    pub speaker_id: Uuid,
    pub speaker: String,
    pub meetings: Vec<ExternalSameVoiceMeeting>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalVoiceNamed {
    pub speaker_id: Uuid,
    pub name: String,
    /// The name it had before voice matching named it.
    pub was: String,
    pub resolution: VoiceNameResolution,
}

/// What voice matching says about a meeting's speakers. Meetings kept on
/// this Mac are left out of the same-voice lists.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalSpeakerVoices {
    pub schema_version: u32,
    pub meeting_id: String,
    pub suggestions: Vec<ExternalVoiceSuggestion>,
    pub same_voice: Vec<ExternalSameVoice>,
    pub named_by_voice: Vec<ExternalVoiceNamed>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceAction {
    Confirmed,
    Undone,
}

/// One answer from `speaker-confirm` or `speaker-voice-undo`. The meeting's
/// history files it under the user, as it does every speaker answer.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalVoiceChange {
    pub schema_version: u32,
    pub action: VoiceAction,
    pub dry_run: bool,
    pub operation_id: String,
    pub replayed: bool,
    pub meeting_id: String,
    pub speaker_id: Uuid,
    pub was: String,
    pub name: String,
    pub person_id: Option<String>,
}

enum VoiceWrite {
    Identify(MeetingSpeakerIdentifyRequest),
    Rename(MeetingSpeakerRenameRequest),
}

pub(super) struct VoicePlan {
    write: VoiceWrite,
    change: ExternalVoiceChange,
}

pub(super) async fn speaker_voices(
    meetings: &Arc<MeetingSessionManager>,
    meeting: &str,
) -> Result<ExternalSpeakerVoices, ExternalError> {
    let session = session_id(meeting)?;
    let store = store(meetings).await?;
    let kept = visible_meeting(&store, session)?;
    store
        .session_snapshot(session)
        .map_err(|error| meeting_refusal(session, error))?;
    let speakers = listed(meetings, &store, session).await?;
    let name_of = |id: SpeakerId| {
        speakers
            .iter()
            .find(|speaker| speaker.speaker_id == id.uuid())
            .map_or_else(String::new, |speaker| speaker.name.clone())
    };
    let failed = command_error(session.uuid());
    let suggestions = meetings
        .voice_speaker_suggestions(session)
        .await
        .map_err(&failed)?
        .into_iter()
        .map(|suggestion| ExternalVoiceSuggestion {
            speaker_id: suggestion.speaker_id.uuid(),
            speaker: name_of(suggestion.speaker_id),
            person_id: suggestion.person_id.map(|id| id.uuid().to_string()),
            name: suggestion.display_name,
        })
        .collect();
    let same_voice = meetings
        .same_voice_meetings(session)
        .await
        .map_err(&failed)?
        .into_iter()
        .map(|group| ExternalSameVoice {
            speaker_id: group.speaker_id.uuid(),
            speaker: name_of(group.speaker_id),
            meetings: group
                .meetings
                .into_iter()
                .filter(|other| !kept.hides(other.session_id))
                .map(|other| ExternalSameVoiceMeeting {
                    meeting_id: other.session_id.uuid().to_string(),
                    title: other.title,
                    at_ms: other.started_at_utc_ms,
                    at: local_time(other.started_at_utc_ms),
                    speaker_id: other.speaker_id.uuid(),
                })
                .collect::<Vec<_>>(),
        })
        .filter(|group| !group.meetings.is_empty())
        .collect();
    let named_by_voice = meetings
        .voice_named_speakers(session)
        .await
        .map_err(&failed)?
        .into_iter()
        .map(|named| ExternalVoiceNamed {
            speaker_id: named.speaker_id.uuid(),
            name: name_of(named.speaker_id),
            was: named.original_display_name,
            resolution: named.resolution_kind,
        })
        .collect();
    Ok(ExternalSpeakerVoices {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: session.uuid().to_string(),
        suggestions,
        same_voice,
        named_by_voice,
    })
}

struct Speaker {
    session: MeetingSessionId,
    revision: u64,
    people_revision: u64,
    id: SpeakerId,
    name: String,
}

async fn speaker(
    meetings: &Arc<MeetingSessionManager>,
    meeting: &str,
    value: &str,
) -> Result<Speaker, ExternalError> {
    let session = session_id(meeting)?;
    let store = store(meetings).await?;
    visible_meeting(&store, session)?;
    let revision = store
        .session_snapshot(session)
        .map_err(|error| meeting_refusal(session, error))?
        .revision;
    let speakers = listed(meetings, &store, session).await?;
    let found = pick_speaker(&speakers, value, meeting)?;
    Ok(Speaker {
        session,
        revision,
        people_revision: store.people_list().map_err(QueryError::from)?.revision,
        id: SpeakerId::from_uuid(found.speaker_id),
        name: found.name.clone(),
    })
}

fn operation_id(verb: &str, speaker: &Speaker, write: &WriteArgs) -> Uuid {
    write.operation_id.unwrap_or_else(|| {
        let mut key = format!(
            "{verb}\0{}\0{}\0{}",
            speaker.session.uuid(),
            speaker.id.uuid(),
            speaker.revision
        );
        if let Some(agent) = &write.agent {
            key.push('\0');
            key.push_str(agent);
        }
        Uuid::new_v5(&OPERATION_NAMESPACE, key.as_bytes())
    })
}

fn voice_change(
    action: VoiceAction,
    speaker: &Speaker,
    operation_id: Uuid,
    dry_run: bool,
    name: String,
) -> ExternalVoiceChange {
    ExternalVoiceChange {
        schema_version: QUERY_SCHEMA_VERSION,
        action,
        dry_run,
        operation_id: operation_id.to_string(),
        replayed: false,
        meeting_id: speaker.session.uuid().to_string(),
        speaker_id: speaker.id.uuid(),
        was: speaker.name.clone(),
        name,
        person_id: None,
    }
}

/// Takes what voice matching suggests for a waiting speaker, as the app's
/// suggestion button does: the matched person, or the user's own name for
/// the user's voice.
pub(super) async fn plan_confirm(
    meetings: &Arc<MeetingSessionManager>,
    args: &SpeakerVoiceArgs,
    now_ms: i64,
) -> Result<VoicePlan, ExternalError> {
    let speaker = speaker(meetings, &args.meeting, &args.speaker).await?;
    let suggestion = meetings
        .voice_speaker_suggestions(speaker.session)
        .await
        .map_err(command_error(speaker.session.uuid()))?
        .into_iter()
        .find(|suggestion| suggestion.speaker_id == speaker.id)
        .ok_or_else(|| {
            ExternalError::invalid(format!(
                "Sona has no voice suggestion for {}. See them with: sona work speaker-voices {}",
                speaker.name,
                speaker.session.uuid()
            ))
        })?;
    let operation_id = operation_id("speaker-confirm", &speaker, &args.write);
    let mut change = voice_change(
        VoiceAction::Confirmed,
        &speaker,
        operation_id,
        args.dry_run,
        suggestion.display_name.clone(),
    );
    change.person_id = suggestion.person_id.map(|id| id.uuid().to_string());
    let write = match suggestion.person_id {
        Some(person_id) => VoiceWrite::Identify(MeetingSpeakerIdentifyRequest {
            operation_id: MeetingOperationId::from_uuid(operation_id),
            requested_at_utc_ms: now_ms,
            session_id: speaker.session,
            expected_meeting_revision: speaker.revision,
            expected_people_revision: speaker.people_revision,
            speaker_id: speaker.id,
            disposition: MeetingSpeakerIdentifyDisposition::Label {
                target: MeetingSpeakerIdentifyTarget::Existing(person_id),
                same_voice: Vec::new(),
            },
        }),
        None => VoiceWrite::Rename(MeetingSpeakerRenameRequest {
            operation_id: MeetingOperationId::from_uuid(operation_id),
            session_id: speaker.session,
            expected_revision: speaker.revision,
            speaker_id: speaker.id,
            display_name: suggestion.display_name,
        }),
    };
    Ok(VoicePlan { write, change })
}

/// Gives a speaker voice matching named by itself back the name it had.
pub(super) async fn plan_undo(
    meetings: &Arc<MeetingSessionManager>,
    args: &SpeakerVoiceArgs,
) -> Result<VoicePlan, ExternalError> {
    let speaker = speaker(meetings, &args.meeting, &args.speaker).await?;
    let named = meetings
        .voice_named_speakers(speaker.session)
        .await
        .map_err(command_error(speaker.session.uuid()))?
        .into_iter()
        .find(|named| named.speaker_id == speaker.id)
        .ok_or_else(|| {
            ExternalError::invalid(format!(
                "{} was not named by voice matching, or has changed since.",
                speaker.name
            ))
        })?;
    let operation_id = operation_id("speaker-voice-undo", &speaker, &args.write);
    let change = voice_change(
        VoiceAction::Undone,
        &speaker,
        operation_id,
        args.dry_run,
        named.original_display_name.clone(),
    );
    Ok(VoicePlan {
        write: VoiceWrite::Rename(MeetingSpeakerRenameRequest {
            operation_id: MeetingOperationId::from_uuid(operation_id),
            session_id: speaker.session,
            expected_revision: speaker.revision,
            speaker_id: speaker.id,
            display_name: named.original_display_name,
        }),
        change,
    })
}

impl VoicePlan {
    pub(super) fn answer(self) -> ExternalVoiceChange {
        self.change
    }

    pub(super) async fn apply(
        self,
        meetings: &Arc<MeetingSessionManager>,
    ) -> Result<ExternalVoiceChange, ExternalError> {
        let Self { write, mut change } = self;
        let (receipt, expected) = match write {
            VoiceWrite::Identify(request) => {
                let session = request.session_id;
                let expected = request.expected_meeting_revision;
                let result = meetings
                    .identify_voice_speaker(request)
                    .await
                    .map_err(command_error(session.uuid()))?;
                change.person_id = result.resolved_person_id.map(|id| id.uuid().to_string());
                (result.receipt, expected)
            }
            VoiceWrite::Rename(request) => {
                let session = request.session_id;
                let expected = request.expected_revision;
                let receipt = meetings
                    .speaker_rename(request)
                    .await
                    .map_err(command_error(session.uuid()))?
                    .receipt;
                (receipt, expected)
            }
        };
        if receipt.result != OperationResult::Committed {
            return Err(ExternalError::new(
                ExternalErrorCode::Failed,
                "The meeting changed while this ran. Run it again.",
            ));
        }
        change.replayed = receipt.expected_revision != expected;
        Ok(change)
    }
}

impl Tracker<'_> {
    pub(super) async fn speaker_voices(
        &self,
        args: &SpeakerVoicesArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        Ok(WorkAnswer::SpeakerVoices(Box::new(
            speaker_voices(self.meetings, &args.meeting).await?,
        )))
    }

    pub(super) async fn speaker_confirm(
        &self,
        args: &SpeakerVoiceArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let plan = plan_confirm(self.meetings, args, super::now_ms()).await?;
        self.voice_write(plan, args.dry_run, "taking a voice suggestion")
            .await
    }

    pub(super) async fn speaker_voice_undo(
        &self,
        args: &SpeakerVoiceArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let plan = plan_undo(self.meetings, args).await?;
        self.voice_write(plan, args.dry_run, "undoing a name voice matching gave")
            .await
    }

    async fn voice_write(
        &self,
        plan: VoicePlan,
        dry_run: bool,
        what: &str,
    ) -> Result<WorkAnswer, ExternalError> {
        if dry_run {
            return Ok(WorkAnswer::VoiceChange(Box::new(plan.answer())));
        }
        self.applies(what)?;
        Ok(WorkAnswer::VoiceChange(Box::new(
            plan.apply(self.meetings).await?,
        )))
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::SpeakerVoices(voices) => {
            if voices.suggestions.is_empty()
                && voices.same_voice.is_empty()
                && voices.named_by_voice.is_empty()
            {
                text.push_str("Voice matching has nothing to say about this meeting.\n");
            }
            if !voices.suggestions.is_empty() {
                text.push_str("Suggested by voice:\n");
                for suggestion in &voices.suggestions {
                    let _ = writeln!(
                        text,
                        "  {} -> {}{}  {}",
                        suggestion.speaker,
                        suggestion.name,
                        if suggestion.person_id.is_none() { " (you)" } else { "" },
                        suggestion.speaker_id
                    );
                }
            }
            if !voices.same_voice.is_empty() {
                text.push_str("Same voice, also unnamed in:\n");
                for group in &voices.same_voice {
                    let _ = writeln!(text, "  {}  {}", group.speaker, group.speaker_id);
                    for meeting in &group.meetings {
                        let _ = writeln!(
                            text,
                            "    {}  {}  {}",
                            meeting.at, meeting.title, meeting.meeting_id
                        );
                    }
                }
            }
            if !voices.named_by_voice.is_empty() {
                text.push_str("Named by voice matching:\n");
                for named in &voices.named_by_voice {
                    let _ = writeln!(
                        text,
                        "  {} (was {})  {}",
                        named.name, named.was, named.speaker_id
                    );
                }
            }
        }
        WorkAnswer::VoiceChange(change) => {
            let verb = match (change.action, change.dry_run) {
                (VoiceAction::Confirmed, false) => "Took the voice suggestion",
                (VoiceAction::Confirmed, true) => "Would take the voice suggestion",
                (VoiceAction::Undone, false) => "Undid the name voice matching gave",
                (VoiceAction::Undone, true) => "Would undo the name voice matching gave",
            };
            let _ = writeln!(
                text,
                "{verb}{}",
                if change.replayed && !change.dry_run { " (already so)" } else { "" }
            );
            let _ = writeln!(text, "  {} -> {}", change.was, change.name);
            let _ = writeln!(text, "  speaker {}", change.speaker_id);
        }
        _ => {}
    }
}
