//! `sona work speaker-assign`: say who a meeting's speaker is, the way the
//! transcript's speaker menu does, and list a meeting's speakers by id. The
//! voice itself is never remembered here: that stays a step the user takes
//! in the app.

use super::{conversations_closed, Fence, Tracker, WorkAnswer, WriteArgs, OPERATION_NAMESPACE};
use crate::meeting::people_types::PersonId;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::voice_identity::{
    MeetingSpeakerIdentifyDisposition, MeetingSpeakerIdentifyRequest,
    MeetingSpeakerIdentifyTarget, SameVoiceSpeaker,
};
use crate::meeting::store::MeetingStore;
use crate::meeting::types::{MeetingOperationId, MeetingSessionId, SpeakerId};
use crate::query::external::{
    command_error, kept_local, meeting_refusal, person_id, session_id, store, visible_meeting,
    ExternalError, ExternalErrorCode,
};
use crate::query::{QueryError, QUERY_SCHEMA_VERSION};
use clap::Args;
use serde::Serialize;
use std::fmt::Write as _;
use std::sync::Arc;
use uuid::Uuid;

const UNKNOWN: &str = "Unknown";

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SpeakerAssignArgs {
    /// The meeting's id, from `meetings`.
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The speaker as the transcript names them ("Speaker 2"), or their id from `meeting`.
    #[arg(value_name = "SPEAKER")]
    pub speaker: String,
    /// Who the speaker is: a person's name, another name they go by, or their id.
    #[arg(
        long,
        value_name = "PERSON",
        required_unless_present_any = ["new", "undo"],
        conflicts_with_all = ["new", "undo"]
    )]
    pub person: Option<String>,
    /// Add a person with this name and say the speaker is them.
    #[arg(long, value_name = "NAME", conflicts_with = "undo")]
    pub new: Option<String>,
    /// Take the name off, as Mark as unknown does in the app: the speaker
    /// reads Unknown, and voice samples kept from this speaker are deleted.
    #[arg(long)]
    pub undo: bool,
    /// Name the same voice too in the other meetings Sona heard it in where
    /// nobody named it, as the app's naming sheet offers. Meetings kept on
    /// this Mac are left alone.
    #[arg(long, conflicts_with = "undo")]
    pub same_voice: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerAction {
    Named,
    Corrected,
    MarkedUnknown,
}

impl SpeakerAction {
    const fn text(self, dry_run: bool) -> &'static str {
        match (self, dry_run) {
            (Self::Named, false) => "Named the speaker",
            (Self::Named, true) => "Would name the speaker",
            (Self::Corrected, false) => "Changed who the speaker is",
            (Self::Corrected, true) => "Would change who the speaker is",
            (Self::MarkedUnknown, false) => "Marked the speaker unknown",
            (Self::MarkedUnknown, true) => "Would mark the speaker unknown",
        }
    }
}

/// One speaker of a meeting, by the name the transcript shows now.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkSpeaker {
    pub speaker_id: Uuid,
    pub name: String,
    /// True while nobody has said who this is.
    pub needs_name: bool,
}

/// One answer from `speaker-assign`. The meeting's history files it under the
/// user: the store records every speaker answer as theirs.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalSpeakerChange {
    pub schema_version: u32,
    pub action: SpeakerAction,
    pub dry_run: bool,
    pub operation_id: String,
    /// True when this answered from an earlier run with the same operation
    /// id, or the speaker already reads that way.
    pub replayed: bool,
    pub meeting_id: String,
    pub speaker_id: Uuid,
    /// The speaker's name before the change.
    pub was: String,
    pub name: String,
    /// Absent for unknown, and on a dry run that adds a person.
    pub person_id: Option<String>,
    /// The other meetings the same voice is named in too, with --same-voice.
    pub same_voice_meetings: Vec<String>,
}

/// Who to say the speaker is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SpeakerName {
    Person { id: PersonId, name: String },
    New(String),
    Unknown,
}

impl SpeakerName {
    fn name(&self) -> &str {
        match self {
            Self::Person { name, .. } => name,
            Self::New(name) => name,
            Self::Unknown => UNKNOWN,
        }
    }

    fn key(&self) -> String {
        match self {
            Self::Person { id, .. } => id.uuid().to_string(),
            Self::New(name) => format!("new:{name}"),
            Self::Unknown => "unknown".to_owned(),
        }
    }

    fn target(&self) -> Option<MeetingSpeakerIdentifyTarget> {
        match self {
            Self::Person { id, .. } => Some(MeetingSpeakerIdentifyTarget::Existing(*id)),
            Self::New(name) => Some(MeetingSpeakerIdentifyTarget::Create {
                display_name: name.clone(),
            }),
            Self::Unknown => None,
        }
    }
}

/// The identify request the change makes, and the answer it gives before
/// anything is written.
pub(super) struct SpeakerPlan {
    request: MeetingSpeakerIdentifyRequest,
    change: ExternalSpeakerChange,
}

/// A meeting's speakers, by name, each with its id and whether it still waits
/// for a name. Assumes the caller already refused a meeting kept on this Mac.
pub(super) async fn listed(
    meetings: &Arc<MeetingSessionManager>,
    store: &MeetingStore,
    session: MeetingSessionId,
) -> Result<Vec<ExternalWorkSpeaker>, ExternalError> {
    let waiting = meetings
        .unresolved_active_voice_speaker_ids(session)
        .await
        .map_err(command_error(session.uuid()))?;
    let mut speakers: Vec<ExternalWorkSpeaker> = store
        .speaker_display_names(session)
        .map_err(QueryError::from)?
        .into_iter()
        .map(|(id, name)| ExternalWorkSpeaker {
            speaker_id: id.uuid(),
            name,
            needs_name: waiting.contains(&id),
        })
        .collect();
    speakers.sort_by(|a, b| a.name.cmp(&b.name).then(a.speaker_id.cmp(&b.speaker_id)));
    Ok(speakers)
}

pub(super) fn pick_speaker<'a>(
    speakers: &'a [ExternalWorkSpeaker],
    value: &str,
    meeting: &str,
) -> Result<&'a ExternalWorkSpeaker, ExternalError> {
    let value = value.trim();
    let by_id = Uuid::parse_str(value).ok();
    let found: Vec<&ExternalWorkSpeaker> = speakers
        .iter()
        .filter(|speaker| match &by_id {
            Some(id) => speaker.speaker_id == *id,
            None => speaker.name.trim().eq_ignore_ascii_case(value),
        })
        .collect();
    let list = format!("List them with: sona work meeting {meeting}");
    match found.as_slice() {
        [one] => Ok(one),
        [] => Err(ExternalError::new(
            ExternalErrorCode::NotFound,
            format!("No speaker {value:?} in this meeting."),
        )
        .hint(&list)),
        _ => Err(ExternalError::invalid(format!(
            "{} speakers are called {value:?}; name one by its id.",
            found.len()
        ))
        .hint(&list)),
    }
}

/// Reads what the change needs, refusing a meeting kept on this Mac as
/// absent, and plans it. Writes nothing.
pub(super) async fn plan(
    meetings: &Arc<MeetingSessionManager>,
    meeting: &str,
    speaker: &str,
    name: SpeakerName,
    write: &WriteArgs,
    dry_run: bool,
    now_ms: i64,
) -> Result<SpeakerPlan, ExternalError> {
    let session = session_id(meeting)?;
    let store = store(meetings).await?;
    visible_meeting(&store, session)?;
    let revision = store
        .session_snapshot(session)
        .map_err(|error| meeting_refusal(session, error))?
        .revision;
    let speakers = listed(meetings, &store, session).await?;
    let found = pick_speaker(&speakers, speaker, meeting)?;
    let speaker_id = SpeakerId::from_uuid(found.speaker_id);
    let people_revision = store.people_list().map_err(QueryError::from)?.revision;
    let operation_id = write.operation_id.unwrap_or_else(|| {
        let mut key = format!(
            "speaker-assign\0{}\0{}\0{}\0{revision}",
            session.uuid(),
            found.speaker_id,
            name.key()
        );
        if let Some(agent) = &write.agent {
            key.push('\0');
            key.push_str(agent);
        }
        Uuid::new_v5(&OPERATION_NAMESPACE, key.as_bytes())
    });
    let (action, disposition) = match name.target() {
        None => (
            SpeakerAction::MarkedUnknown,
            MeetingSpeakerIdentifyDisposition::MarkUnknown,
        ),
        Some(target) if found.needs_name => (
            SpeakerAction::Named,
            MeetingSpeakerIdentifyDisposition::Label {
                target,
                same_voice: Vec::new(),
            },
        ),
        Some(target) => (
            SpeakerAction::Corrected,
            MeetingSpeakerIdentifyDisposition::CorrectTo { target },
        ),
    };
    let already = !found.needs_name && found.name.trim().eq_ignore_ascii_case(name.name());
    Ok(SpeakerPlan {
        request: MeetingSpeakerIdentifyRequest {
            operation_id: MeetingOperationId::from_uuid(operation_id),
            requested_at_utc_ms: now_ms,
            session_id: session,
            expected_meeting_revision: revision,
            expected_people_revision: people_revision,
            speaker_id,
            disposition,
        },
        change: ExternalSpeakerChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run,
            operation_id: operation_id.to_string(),
            replayed: already,
            meeting_id: session.uuid().to_string(),
            speaker_id: found.speaker_id,
            was: found.name.clone(),
            name: name.name().to_owned(),
            person_id: match &name {
                SpeakerName::Person { id, .. } => Some(id.uuid().to_string()),
                SpeakerName::New(_) | SpeakerName::Unknown => None,
            },
            same_voice_meetings: Vec::new(),
        },
    })
}

impl SpeakerPlan {
    /// Whether the change has nothing to write: a dry run, or a speaker that
    /// already reads that way.
    pub(super) const fn settled(&self) -> bool {
        self.change.dry_run || self.change.replayed
    }

    pub(super) fn answer(self) -> ExternalSpeakerChange {
        self.change
    }

    /// Names the same voice in the other meetings it was heard in where
    /// nobody named it either, leaving out meetings kept on this Mac.
    pub(super) async fn with_same_voice(
        mut self,
        meetings: &Arc<MeetingSessionManager>,
    ) -> Result<Self, ExternalError> {
        let session = self.request.session_id;
        let MeetingSpeakerIdentifyDisposition::Label { same_voice, .. } =
            &mut self.request.disposition
        else {
            return Err(ExternalError::invalid(
                "--same-voice names a speaker who still needs a name. This one already has one.",
            ));
        };
        let kept = kept_local(&*store(meetings).await?)?;
        let others: Vec<SameVoiceSpeaker> = meetings
            .same_voice_meetings(session)
            .await
            .map_err(command_error(session.uuid()))?
            .into_iter()
            .find(|group| group.speaker_id == self.request.speaker_id)
            .map(|group| group.meetings)
            .unwrap_or_default()
            .into_iter()
            .filter(|meeting| !kept.hides(meeting.session_id))
            .map(|meeting| SameVoiceSpeaker {
                session_id: meeting.session_id,
                speaker_id: meeting.speaker_id,
                expected_speaker_revision: meeting.speaker_revision,
            })
            .collect();
        self.change.same_voice_meetings = others
            .iter()
            .map(|other| other.session_id.uuid().to_string())
            .collect();
        *same_voice = others;
        Ok(self)
    }

    /// Makes the change through the same identify step the app's speaker
    /// menu takes. An operation id that already ran answers from that run.
    pub(super) async fn apply(
        self,
        meetings: &Arc<MeetingSessionManager>,
    ) -> Result<ExternalSpeakerChange, ExternalError> {
        let Self {
            request,
            mut change,
        } = self;
        let expected = request.expected_meeting_revision;
        let session = request.session_id;
        let result = meetings
            .identify_voice_speaker(request)
            .await
            .map_err(command_error(session.uuid()))?;
        change.replayed = result.receipt.expected_revision != expected;
        change.person_id = result
            .resolved_person_id
            .map(|id| id.uuid().to_string());
        Ok(change)
    }
}

impl Tracker<'_> {
    pub(super) async fn speaker_assign(
        &self,
        args: &SpeakerAssignArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let name = match (&args.person, &args.new) {
            (Some(person), _) => {
                let board = self.board().await?;
                let found = board.person(person)?;
                SpeakerName::Person {
                    id: person_id(&found.id)?,
                    name: found.name.clone(),
                }
            }
            (None, Some(new)) => {
                if !Fence::read(self.meetings).await?.is_open() {
                    return Err(conversations_closed());
                }
                SpeakerName::New(new.trim().to_owned())
            }
            (None, None) => {
                if !Fence::read(self.meetings).await?.is_open() {
                    return Err(conversations_closed());
                }
                SpeakerName::Unknown
            }
        };
        let mut plan = plan(
            self.meetings,
            &args.meeting,
            &args.speaker,
            name,
            &args.write,
            args.dry_run,
            super::now_ms(),
        )
        .await?;
        if args.same_voice {
            plan = plan.with_same_voice(self.meetings).await?;
        }
        if plan.settled() {
            return Ok(WorkAnswer::Speaker(Box::new(plan.answer())));
        }
        self.applies("naming a meeting's speaker")?;
        Ok(WorkAnswer::Speaker(Box::new(plan.apply(self.meetings).await?)))
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    if let WorkAnswer::Speaker(change) = answer {
        let _ = writeln!(
            text,
            "{}{}",
            change.action.text(change.dry_run),
            if change.replayed && !change.dry_run {
                " (already so)"
            } else {
                ""
            }
        );
        let _ = writeln!(text, "  {} -> {}", change.was, change.name);
        let _ = writeln!(text, "  speaker {}", change.speaker_id);
        if !change.same_voice_meetings.is_empty() {
            let _ = writeln!(
                text,
                "  same voice named in: {}",
                change.same_voice_meetings.join(", ")
            );
        }
    }
}

pub(super) fn listed_text(speakers: &[ExternalWorkSpeaker], text: &mut String) {
    if speakers.is_empty() {
        return;
    }
    text.push_str("Speakers:\n");
    for speaker in speakers {
        let _ = writeln!(
            text,
            "  {}{}  {}",
            speaker.name,
            if speaker.needs_name {
                " (needs a name)"
            } else {
                ""
            },
            speaker.speaker_id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{listed, plan, SpeakerAction, SpeakerName};
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::people_types::PersonId;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::session::{MeetingSessionManager, NoCaptureSources};
    use crate::meeting::store::workflow_core_tests::{person, reviewable_meeting};
    use crate::meeting::types::{MeetingOperationId, MeetingSessionId, SourceKind};
    use crate::query::external::tracker::WriteArgs;
    use crate::query::external::ExternalErrorCode;
    use crate::secrets::{MemorySecretBackend, SecretManager};
    use std::sync::Arc;
    use tempfile::TempDir;

    const NOW: i64 = 1_767_000_000_000;

    struct Corpus {
        _directory: TempDir,
        meetings: Arc<MeetingSessionManager>,
        session: MeetingSessionId,
        dana: PersonId,
    }

    async fn corpus() -> Corpus {
        let directory = TempDir::new().unwrap();
        let secrets = Arc::new(SecretManager::with_backend(Arc::new(
            MemorySecretBackend::new(),
        )));
        let meetings = Arc::new(
            MeetingSessionManager::with_parts(
                None,
                Some(directory.path().join("meetings")),
                secrets,
                Arc::new(NoCaptureSources),
            )
            .reading_only(),
        );
        let store = meetings.store().await.unwrap();
        let session = reviewable_meeting(&store, "Pricing sync", NOW);
        store
            .diarization_speaker(session, SourceKind::SystemAudio, 1)
            .unwrap();
        let dana = person(&store, "Dana Reyes", &[], &[]);
        Corpus {
            _directory: directory,
            meetings,
            session,
            dana,
        }
    }

    fn organizer() -> WriteArgs {
        WriteArgs {
            operation_id: None,
            agent: Some("Organizer".to_owned()),
        }
    }

    fn dana(corpus: &Corpus) -> SpeakerName {
        SpeakerName::Person {
            id: corpus.dana,
            name: "Dana Reyes".to_owned(),
        }
    }

    #[test]
    fn speaker_assign_names_the_speaker_as_the_person() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let meeting = corpus.session.uuid().to_string();
            let planned = plan(
                &corpus.meetings,
                &meeting,
                "speaker 2",
                dana(&corpus),
                &organizer(),
                false,
                NOW,
            )
            .await
            .unwrap();
            assert!(!planned.settled());
            let change = planned.apply(&corpus.meetings).await.unwrap();
            assert_eq!(change.action, SpeakerAction::Named);
            assert!(!change.replayed);
            assert_eq!(change.was, "Speaker 2");
            assert_eq!(change.person_id, Some(corpus.dana.uuid().to_string()));

            let store = corpus.meetings.store().await.unwrap();
            let speakers = listed(&corpus.meetings, &store, corpus.session)
                .await
                .unwrap();
            assert_eq!(speakers.len(), 1);
            assert_eq!(speakers[0].speaker_id, change.speaker_id);
            assert_eq!(speakers[0].name, "Dana Reyes");
            assert!(!speakers[0].needs_name);

            let again = plan(
                &corpus.meetings,
                &meeting,
                &change.speaker_id.to_string(),
                dana(&corpus),
                &organizer(),
                false,
                NOW,
            )
            .await
            .unwrap();
            assert!(again.settled());
            assert!(again.answer().replayed);
        });
    }

    #[test]
    fn speaker_assign_refuses_a_meeting_kept_on_this_mac() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let store = corpus.meetings.store().await.unwrap();
            store
                .remember_calendar_facts(
                    corpus.session,
                    &CalendarEventSummary {
                        event_key: format!("weekly-pricing#{NOW}"),
                        series_key: "weekly-pricing".to_owned(),
                        title: "Pricing sync".to_owned(),
                        attendee_count: 2,
                        start_utc_ms: NOW,
                        end_utc_ms: NOW + 30 * 60 * 1_000,
                        attendees: Vec::new(),
                        notes: None,
                        calendar_name: None,
                        location: None,
                        url: None,
                        place: None,
                    },
                )
                .unwrap();
            store
                .set_series_remote_opt_out(
                    &MeetingSeriesRemoteOptOutSetRequest {
                        operation_id: MeetingOperationId::new(),
                        series_key: "weekly-pricing".to_owned(),
                        remote_intelligence_opt_out: true,
                        expected_revision: 0,
                    },
                    NOW,
                )
                .unwrap();

            let refused = plan(
                &corpus.meetings,
                &corpus.session.uuid().to_string(),
                "Speaker 2",
                dana(&corpus),
                &organizer(),
                false,
                NOW,
            )
            .await
            .err()
            .unwrap();
            assert_eq!(refused.error, ExternalErrorCode::NotFound);
        });
    }

    #[test]
    fn a_dry_run_speaker_assign_has_nothing_to_write() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let planned = plan(
                &corpus.meetings,
                &corpus.session.uuid().to_string(),
                "Speaker 2",
                dana(&corpus),
                &organizer(),
                true,
                NOW,
            )
            .await
            .unwrap();
            assert!(planned.settled());
            let change = planned.answer();
            assert!(change.dry_run);
            assert_eq!(change.action, SpeakerAction::Named);

            let store = corpus.meetings.store().await.unwrap();
            let speakers = listed(&corpus.meetings, &store, corpus.session)
                .await
                .unwrap();
            assert_eq!(speakers[0].name, "Speaker 2");
            assert!(speakers[0].needs_name);
        });
    }
}
