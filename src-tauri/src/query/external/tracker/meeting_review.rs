//! What a meeting's page shows beside its notes, read or changed through the
//! functions the page calls: how the conversation went, what came before it
//! with each person, the questions asked of it, the links kept from it, its
//! kind and place, the maybes waiting on it, the commitments nobody tracked,
//! the names a call offered, and a marked copy of a PDF added to it.

use super::library::{
    change, changed, file_made, library, made_before, meeting, operation, receipt_replayed, told,
    Change, ExternalLibraryChange, Gate, MeetingRef,
};
use super::meeting_settings::day;
use super::notes::request_json;
use super::speakers::{listed, pick_speaker};
use super::{refusal, Tracker, WorkAnswer, READ_SCOPES, WRITE_SCOPES};
use crate::managers::history::HistoryManager;
use crate::meeting::context::{ConversationContextUpdate, ConversationKind, ConversationLocation};
use crate::meeting::document_types::{DocumentDifference, DocumentMark, MarkedCopyRequest};
use crate::meeting::evening::{FilingSuggestion, FilingTarget};
use crate::meeting::open_windows::{MeetingOpenWindow, OpenWindowId};
use crate::meeting::people_types::MeetingPersonContextRow;
use crate::meeting::session::{MeetingMutationRequest, MeetingSessionManager};
use crate::meeting::store::MeetingStore;
use crate::meeting::types::{
    MeetingAnswer, MeetingAnswerState, MeetingQuestionId, MeetingQuestionScope, MeetingSessionId,
    MeetingSessionSnapshot, SpeakerId, TranscriptSegmentId,
};
use crate::planning::untracked;
use crate::query::external::{
    command_error, kept_local, local_time, meeting_refusal, session_id, visible_meeting,
    ExternalError, ExternalErrorCode, ExternalScope,
};
use crate::query::{KeptLocal, QueryError, QUERY_SCHEMA_VERSION};
use clap::{ArgGroup, Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;
use uuid::Uuid;

const NS_PER_MS: u64 = 1_000_000;
const LINK_REMOVE: &str = "cli_meeting_link_remove";
const QUESTION_FORGET: &str = "cli_meeting_question_forget";
const CONTEXT: &str = "cli_meeting_context";

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum MeetingReviewVerb {
    /// How a meeting's conversation went: each speaker's share of the
    /// talking, the turns, the longest monologue, and what your keyword
    /// trackers caught.
    MeetingAnalytics(MeetingRef),
    /// The people in a meeting and what came before it with each of them:
    /// the meetings before, the last one, and the top open item.
    MeetingPeople(MeetingRef),
    /// The questions asked of a meeting, with their answers.
    MeetingQuestions(MeetingRef),
    /// Forget a question asked of a meeting, and its answer.
    MeetingQuestionForget(QuestionArgs),
    /// The links kept from the windows in front while a meeting recorded.
    MeetingLinks(MeetingRef),
    /// Remove one of a meeting's links.
    MeetingLinkRemove(LinkArgs),
    /// Say what kind of conversation a meeting was, or where it happened.
    MeetingContext(ContextArgs),
    /// The maybes waiting on one recording: the event, person, project,
    /// kind, place or earlier plan it may be. Decide one with `filing`.
    ContextSuggestions(MeetingRef),
    /// Commitments from finished meetings that no task tracks yet and nobody
    /// set aside. Track one with `action-track`.
    Untracked(UntrackedArgs),
    /// Turn down the name a call offered for one of a meeting's speakers.
    CallNameDismiss(CallNameArgs),
    /// Save a copy of a meeting's PDF with each passage that differs from
    /// what was said marked, and a note beside it.
    DocMarkedCopy(MarkedCopyArgs),
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct QuestionArgs {
    /// The meeting's id, from `meetings`.
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The question's id, from `meeting-questions`.
    #[arg(value_name = "QUESTION_ID")]
    pub question: Uuid,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LinkArgs {
    /// The meeting's id, from `meetings`.
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The link's id, from `meeting-links`.
    #[arg(value_name = "LINK_ID")]
    pub link: Uuid,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum MeetingKind {
    /// No kind chosen: a conversation.
    Unspecified,
    Huddle,
    OnlineCall,
    InPerson,
    NoteSession,
    /// An imported recording.
    Imported,
}

impl From<MeetingKind> for ConversationKind {
    fn from(kind: MeetingKind) -> Self {
        match kind {
            MeetingKind::Unspecified => Self::Unspecified,
            MeetingKind::Huddle => Self::Huddle,
            MeetingKind::OnlineCall => Self::OnlineCall,
            MeetingKind::InPerson => Self::InPerson,
            MeetingKind::NoteSession => Self::NoteSession,
            MeetingKind::Imported => Self::Imported,
        }
    }
}

/// A coordinate the parser has already checked is finite, so it compares
/// like any other value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Degrees(f64);

impl Eq for Degrees {}

fn degrees(value: &str, limit: f64) -> Result<Degrees, String> {
    let parsed: f64 = value
        .trim()
        .parse()
        .map_err(|_| format!("{value:?} is not a number of degrees."))?;
    if parsed.is_finite() && (-limit..=limit).contains(&parsed) {
        Ok(Degrees(parsed))
    } else {
        Err(format!("Degrees here run from -{limit} to {limit}."))
    }
}

fn latitude(value: &str) -> Result<Degrees, String> {
    degrees(value, 90.0)
}

fn longitude(value: &str) -> Result<Degrees, String> {
    degrees(value, 180.0)
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
#[command(group(ArgGroup::new("context").required(true).multiple(true).args(["kind", "place", "no_place"])))]
pub struct ContextArgs {
    /// The meeting's id, from `meetings`.
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// What kind of conversation it was.
    #[arg(long, value_enum, value_name = "KIND")]
    pub kind: Option<MeetingKind>,
    /// Where it happened, by name. Give --lat and --lon with it.
    #[arg(long, value_name = "NAME", requires_all = ["lat", "lon"], conflicts_with = "no_place")]
    pub place: Option<String>,
    /// The place's latitude, from -90 to 90.
    #[arg(long, value_name = "DEGREES", value_parser = latitude, allow_negative_numbers = true, requires = "place")]
    pub lat: Option<Degrees>,
    /// The place's longitude, from -180 to 180.
    #[arg(long, value_name = "DEGREES", value_parser = longitude, allow_negative_numbers = true, requires = "place")]
    pub lon: Option<Degrees>,
    /// Take the place off the meeting.
    #[arg(long)]
    pub no_place: bool,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct UntrackedArgs {
    /// Only this meeting's, by its id. Without it, every finished meeting's.
    #[arg(value_name = "MEETING")]
    pub meeting: Option<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CallNameArgs {
    /// The meeting's id, from `meetings`.
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The speaker as the transcript names them ("Speaker 2"), or their id,
    /// from `meeting MEETING`.
    #[arg(value_name = "SPEAKER")]
    pub speaker: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MarkedCopyArgs {
    /// The PDF's id, from `docs --meeting MEETING`.
    #[arg(value_name = "DOCUMENT_ID")]
    pub document: Uuid,
    /// The meeting the PDF was added to.
    #[arg(long, value_name = "MEETING")]
    pub meeting: String,
    /// Where to save the copy, a .pdf file. A file already there is
    /// replaced; the PDF you added stays as it is.
    #[arg(long, value_name = "FILE")]
    pub out: PathBuf,
    #[command(flatten)]
    pub change: Change,
}

impl MeetingReviewVerb {
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self {
            Self::MeetingAnalytics(_)
            | Self::MeetingPeople(_)
            | Self::MeetingQuestions(_)
            | Self::MeetingLinks(_)
            | Self::ContextSuggestions(_)
            | Self::Untracked(_) => READ_SCOPES,
            Self::MeetingQuestionForget(QuestionArgs { change, .. })
            | Self::MeetingLinkRemove(LinkArgs { change, .. })
            | Self::MeetingContext(ContextArgs { change, .. })
            | Self::CallNameDismiss(CallNameArgs { change, .. })
            | Self::DocMarkedCopy(MarkedCopyArgs { change, .. }) => {
                if change.dry_run {
                    READ_SCOPES
                } else {
                    WRITE_SCOPES
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSpeakerShare {
    pub speaker_id: Uuid,
    pub name: String,
    pub speaking_ms: u64,
    /// Thousandths of all the talking; the shares add up to 1000.
    pub share_permille: u32,
    pub turns: u32,
    pub longest_monologue_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalTrackerHits {
    pub name: String,
    pub hits: u32,
    /// The transcript lines that carry them.
    pub line_ids: Vec<TranscriptSegmentId>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalMeetingAnalytics {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    pub computed_at_ms: i64,
    pub speaking_ms: u64,
    pub turns: u32,
    /// How often the floor changed hands.
    pub interactions: u32,
    pub speakers: Vec<ExternalSpeakerShare>,
    pub longest_monologue_ms: u64,
    pub longest_monologue_speaker: Option<String>,
    /// Where the longest monologue starts, from the start of the recording.
    pub longest_monologue_at_ms: Option<u64>,
    /// The median silence between one speaker finishing and the next
    /// starting; none when the floor never changed hands.
    pub median_switch_gap_ms: Option<u64>,
    pub trackers: Vec<ExternalTrackerHits>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalMeetingPeople {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    /// A meeting kept on this Mac is left out: it is not counted among the
    /// meetings before, and it is no one's last meeting or the source of an
    /// open item.
    pub people: Vec<MeetingPersonContextRow>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalMeetingQuestions {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    pub questions: Vec<MeetingAnswer>,
    /// True when a question that reads a meeting kept on this Mac was left
    /// out.
    pub left_out: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalMeetingLinks {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    pub links: Vec<MeetingOpenWindow>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalContextSuggestions {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    pub suggestions: Vec<FilingSuggestion>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalUntrackedAction {
    pub loop_id: String,
    /// What was promised.
    pub text: String,
    pub owner: Option<String>,
    /// The words it was read from.
    pub quote: Option<String>,
    /// Where it was said, from the start of the recording.
    pub at_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalUntrackedMeeting {
    pub meeting_id: Uuid,
    pub title: String,
    pub actions: Vec<ExternalUntrackedAction>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalUntracked {
    pub schema_version: u32,
    pub meetings: Vec<ExternalUntrackedMeeting>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ExternalMeetingReviewAnswer {
    Analytics(ExternalMeetingAnalytics),
    People(ExternalMeetingPeople),
    Questions(ExternalMeetingQuestions),
    Links(ExternalMeetingLinks),
    Suggestions(ExternalContextSuggestions),
    Untracked(ExternalUntracked),
}

const fn ms(ns: u64) -> u64 {
    ns / NS_PER_MS
}

/// "3:12" or "1:04:07", the clock the meeting page shows.
fn clock(ms: u64) -> String {
    let total = ms / 1000;
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn store_failed(error: crate::meeting::store::StoreError) -> ExternalError {
    QueryError::from(error).into()
}

/// Whether a filing suggestion offers a plan heard in a meeting kept on
/// this Mac.
pub(super) fn plan_kept(
    store: &MeetingStore,
    kept: &KeptLocal,
    target: &FilingTarget,
) -> Result<bool, ExternalError> {
    Ok(match target {
        FilingTarget::Meeting { plan_id, .. } => {
            kept.hides(store.heard_plan(*plan_id).map_err(store_failed)?.session_id)
        }
        _ => false,
    })
}

/// The maybes waiting on a recording as an agent may read them: none offers
/// a plan heard in a meeting kept on this Mac, and none shows a place only
/// such a meeting saved.
pub(super) fn agent_maybes(
    store: &MeetingStore,
    kept: &KeptLocal,
    session: MeetingSessionId,
    waiting: Vec<FilingSuggestion>,
) -> Result<Vec<FilingSuggestion>, ExternalError> {
    let mut shown = Vec::with_capacity(waiting.len());
    for mut suggestion in waiting {
        if !plan_kept(store, kept, &suggestion.target)? {
            without_kept_place(store, kept, session, &mut suggestion.target)?;
            shown.push(suggestion);
        }
    }
    Ok(shown)
}

/// Drops the place a meeting maybe on `session` found unless the
/// recordings an agent may read give that same place.
pub(super) fn without_kept_place(
    store: &MeetingStore,
    kept: &KeptLocal,
    session: MeetingSessionId,
    target: &mut FilingTarget,
) -> Result<(), ExternalError> {
    let FilingTarget::Meeting {
        place, location, ..
    } = target
    else {
        return Ok(());
    };
    if location.is_none() || kept.sessions().is_empty() {
        return Ok(());
    }
    let shown = match place {
        Some(words) => store
            .saved_place_without(session, words, kept.sessions())
            .map_err(store_failed)?,
        None => None,
    };
    if shown != *location {
        *location = None;
    }
    Ok(())
}

async fn analytics(
    meetings: &Arc<MeetingSessionManager>,
    args: &MeetingRef,
) -> Result<ExternalMeetingAnalytics, ExternalError> {
    let session = session_id(&args.meeting)?;
    let store = library(meetings).await?;
    visible_meeting(&store, session)?;
    let snapshot = meetings
        .analytics_get(session)
        .await
        .map_err(command_error(session.uuid()))?;
    let names = store
        .speaker_display_names(session)
        .map_err(|error| meeting_refusal(session, error))?;
    let name = |id: &SpeakerId| {
        names
            .get(id)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_owned())
    };
    let talk = snapshot.analytics.talk;
    Ok(ExternalMeetingAnalytics {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: session.uuid(),
        computed_at_ms: snapshot.computed_at_utc_ms,
        speaking_ms: ms(talk.total_speaking_ns),
        turns: talk.turn_count,
        interactions: talk.interaction_count,
        speakers: talk
            .speakers
            .iter()
            .map(|share| ExternalSpeakerShare {
                speaker_id: share.speaker_id.uuid(),
                name: name(&share.speaker_id),
                speaking_ms: ms(share.speaking_ns),
                share_permille: share.share_permille,
                turns: share.turn_count,
                longest_monologue_ms: ms(share.longest_monologue_ns),
            })
            .collect(),
        longest_monologue_ms: ms(talk.longest_monologue_ns),
        longest_monologue_speaker: talk.longest_monologue_speaker_id.as_ref().map(name),
        longest_monologue_at_ms: talk.longest_monologue_start_ns.map(ms),
        median_switch_gap_ms: talk.median_switch_gap_ms,
        trackers: snapshot
            .analytics
            .trackers
            .into_iter()
            .map(|tracker| ExternalTrackerHits {
                name: tracker.name,
                hits: tracker.hit_count,
                line_ids: tracker.segment_ids,
            })
            .collect(),
    })
}

async fn people(
    meetings: &Arc<MeetingSessionManager>,
    args: &MeetingRef,
) -> Result<ExternalMeetingPeople, ExternalError> {
    let session = session_id(&args.meeting)?;
    let store = library(meetings).await?;
    let kept = visible_meeting(&store, session)?;
    let mut people = store
        .meeting_people_context_without(session, kept.sessions())
        .map_err(|error| meeting_refusal(session, error))?
        .rows;
    for open in people
        .iter_mut()
        .filter_map(|row| row.top_open_loop.as_mut())
    {
        if open
            .carried_into_meeting_id
            .is_some_and(|id| kept.hides(id))
        {
            open.carried_into_meeting_id = None;
        }
    }
    Ok(ExternalMeetingPeople {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: session.uuid(),
        people,
    })
}

/// Whether a question reads a meeting kept on this Mac, through its series
/// or what it cites.
fn reads_kept(answer: &MeetingAnswer, kept: &KeptLocal) -> bool {
    let series = match &answer.scope {
        MeetingQuestionScope::ThisMeeting => false,
        MeetingQuestionScope::ExplicitSeries { session_ids } => {
            session_ids.iter().any(|id| kept.hides(*id))
        }
    };
    series
        || answer
            .citations
            .iter()
            .any(|citation| kept.hides(citation.session_id))
}

async fn questions(
    meetings: &Arc<MeetingSessionManager>,
    args: &MeetingRef,
) -> Result<ExternalMeetingQuestions, ExternalError> {
    let (store, review) = meeting(meetings, &args.meeting).await?;
    let kept = kept_local(&store)?;
    let asked = review.questions.len();
    let questions: Vec<MeetingAnswer> = review
        .questions
        .into_iter()
        .filter(|answer| !reads_kept(answer, &kept))
        .collect();
    Ok(ExternalMeetingQuestions {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: review.session.session_id.uuid(),
        left_out: questions.len() != asked,
        questions,
    })
}

async fn question_forget(
    meetings: &Arc<MeetingSessionManager>,
    history: &HistoryManager,
    args: &QuestionArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let (store, review) = meeting(meetings, &args.meeting).await?;
    let kept = kept_local(&store)?;
    let MeetingSessionSnapshot {
        session_id: session,
        revision,
        ..
    } = review.session;
    let (id, operation_id) = operation(
        &args.change.write,
        "meeting-question-forget",
        &(session.uuid(), args.question, revision),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Forgot the question", "Would forget the question"),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    let request = format!("{}/{}", session.uuid(), args.question);
    if made_before(history, &answer.operation_id, QUESTION_FORGET, &request)? {
        answer.replayed = true;
        return Ok(answer);
    }
    let question = review
        .questions
        .iter()
        .filter(|asked| !reads_kept(asked, &kept))
        .find(|asked| asked.question_id.uuid() == args.question)
        .ok_or_else(|| {
            ExternalError::new(
                ExternalErrorCode::NotFound,
                format!("No question {} in this meeting.", args.question),
            )
            .hint(&format!(
                "List them with: sona work meeting-questions {}",
                args.meeting
            ))
        })?;
    answer.replayed = question.state == MeetingAnswerState::Forgotten;
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("forgetting a question")?;
    let result = meetings
        .question_forget(
            MeetingMutationRequest {
                operation_id,
                session_id: session,
                expected_revision: revision,
            },
            MeetingQuestionId::from_uuid(args.question),
        )
        .await
        .map_err(command_error(session.uuid()))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    file_made(history, &answer.operation_id, QUESTION_FORGET, &request)?;
    Ok(answer)
}

/// The windows the person kept as the meeting's links. A window only
/// offered during the recording is not one of them.
fn attached_links(
    store: &MeetingStore,
    session: crate::meeting::types::MeetingSessionId,
) -> Result<Vec<MeetingOpenWindow>, ExternalError> {
    Ok(store
        .open_windows(session)
        .map_err(|error| meeting_refusal(session, error))?
        .into_iter()
        .filter(|window| window.attached_at_utc_ms.is_some())
        .collect())
}

async fn links(
    meetings: &Arc<MeetingSessionManager>,
    args: &MeetingRef,
) -> Result<ExternalMeetingLinks, ExternalError> {
    let session = session_id(&args.meeting)?;
    let store = library(meetings).await?;
    visible_meeting(&store, session)?;
    Ok(ExternalMeetingLinks {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: session.uuid(),
        links: attached_links(&store, session)?,
    })
}

async fn link_remove(
    meetings: &Arc<MeetingSessionManager>,
    history: &HistoryManager,
    args: &LinkArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let session = session_id(&args.meeting)?;
    let id = args
        .change
        .write
        .operation_id("meeting-link-remove", &(session.uuid(), args.link))?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Removed the link", "Would remove the link"),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    let request = format!("{}/{}", session.uuid(), args.link);
    if made_before(history, &answer.operation_id, LINK_REMOVE, &request)? {
        answer.replayed = true;
        return Ok(answer);
    }
    let store = library(meetings).await?;
    visible_meeting(&store, session)?;
    let link = OpenWindowId::from_uuid(args.link);
    if !attached_links(&store, session)?
        .iter()
        .any(|window| window.item_id == link)
    {
        return Err(ExternalError::new(
            ExternalErrorCode::NotFound,
            format!("No link {} in this meeting.", args.link),
        )
        .hint(&format!(
            "List them with: sona work meeting-links {}",
            args.meeting
        )));
    }
    if dry_run {
        return Ok(answer);
    }
    gate("removing a meeting link")?;
    store
        .remove_open_window(session, link)
        .map_err(|error| meeting_refusal(session, error))?;
    file_made(history, &answer.operation_id, LINK_REMOVE, &request)?;
    Ok(answer)
}

async fn context(
    meetings: &Arc<MeetingSessionManager>,
    history: &HistoryManager,
    args: &ContextArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let session = session_id(&args.meeting)?;
    let store = library(meetings).await?;
    visible_meeting(&store, session)?;
    let current = store
        .conversation_context(session)
        .map_err(|error| meeting_refusal(session, error))?;
    let kind = args.kind.map_or(current.kind, ConversationKind::from);
    let location = match (&args.place, args.lat, args.lon) {
        _ if args.no_place => None,
        (Some(name), Some(Degrees(latitude)), Some(Degrees(longitude))) => {
            let place = ConversationLocation {
                name: name.trim().to_owned(),
                latitude,
                longitude,
            };
            if !place.is_valid() {
                return Err(ExternalError::invalid(
                    "A place needs a name, at most 1,000 bytes long.",
                ));
            }
            Some(place)
        }
        _ => current.location.clone(),
    };
    let id = args.change.write.operation_id(
        "meeting-context",
        &(session.uuid(), kind, &location, current.revision),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(
            dry_run,
            "Changed the meeting's kind and place",
            "Would change the meeting's kind and place",
        ),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    let request = request_json(&(
        session.uuid(),
        args.kind.map(ConversationKind::from),
        &args.place,
        args.lat.map(|Degrees(degrees)| degrees),
        args.lon.map(|Degrees(degrees)| degrees),
        args.no_place,
    ))?;
    if made_before(history, &answer.operation_id, CONTEXT, &request)? {
        answer.replayed = true;
        return Ok(answer);
    }
    answer.replayed = current.kind == kind && current.location == location;
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("changing a meeting's kind and place")?;
    store
        .update_conversation_context(ConversationContextUpdate {
            session_id: session,
            expected_revision: current.revision,
            kind,
            location,
        })
        .map_err(|error| meeting_refusal(session, error))?;
    file_made(history, &answer.operation_id, CONTEXT, &request)?;
    Ok(answer)
}

async fn call_name_dismiss(
    meetings: &Arc<MeetingSessionManager>,
    args: &CallNameArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    let session = session_id(&args.meeting)?;
    let store = library(meetings).await?;
    visible_meeting(&store, session)?;
    let speakers = listed(meetings, &store, session).await?;
    let speaker = pick_speaker(&speakers, &args.speaker, &args.meeting)?;
    let status = store
        .call_name_status(session)
        .map_err(|error| meeting_refusal(session, error))?;
    let offered = status
        .suggestions
        .iter()
        .find(|suggestion| suggestion.speaker_id.uuid() == speaker.speaker_id);
    let id = args
        .change
        .write
        .operation_id("call-name-dismiss", &(session.uuid(), speaker.speaker_id))?;
    let dry_run = args.change.dry_run;
    let Some(offered) = offered else {
        let mut answer = change(
            format!("The call offers no name for {}", speaker.name),
            dry_run,
            id,
        );
        answer.meeting_id = Some(session.uuid());
        answer.replayed = true;
        return Ok(answer);
    };
    let mut answer = change(
        if dry_run {
            format!(
                "Would turn down {} for {}",
                offered.display_name, speaker.name
            )
        } else {
            format!("Turned down {} for {}", offered.display_name, speaker.name)
        },
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    if dry_run {
        return Ok(answer);
    }
    gate("turning down a name the call offered")?;
    meetings
        .call_name_dismiss(session, SpeakerId::from_uuid(speaker.speaker_id))
        .await
        .map_err(command_error(session.uuid()))?;
    Ok(answer)
}

/// The note beside a marked passage, written the way the meeting page
/// writes it, so it reads on its own in the PDF.
fn mark_note(difference: &DocumentDifference, meeting: &MeetingSessionSnapshot) -> String {
    let on = meeting
        .started_at_utc_ms
        .map(|ms| format!(" on {}", day(ms)))
        .unwrap_or_default();
    format!(
        "{} said “{}” at {} in “{}”{on}.",
        difference.speaker.as_deref().unwrap_or("Someone"),
        difference.quote,
        clock(ms(difference.citation.start_offset_ns)),
        meeting.title,
    )
}

fn marked(count: usize, dry_run: bool) -> String {
    let (saved, up_to) = if dry_run {
        ("Would save a copy", "up to ")
    } else {
        ("Saved a copy", "")
    };
    match count {
        0 => format!("{saved}. No passage could be marked"),
        1 => format!("{saved} with {up_to}1 mark"),
        count => format!("{saved} with {up_to}{count} marks"),
    }
}

async fn marked_copy(
    meetings: &Arc<MeetingSessionManager>,
    args: &MarkedCopyArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    if !args
        .out
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
    {
        return Err(ExternalError::invalid(
            "Save the marked copy as a .pdf file.",
        ));
    }
    let (_, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let document = meetings
        .doc_meeting_list(session)
        .await
        .map_err(command_error(session.uuid()))?
        .into_iter()
        .find(|document| document.document.id.uuid() == args.document)
        .ok_or_else(|| {
            ExternalError::new(
                ExternalErrorCode::NotFound,
                format!("No document {} in this meeting.", args.document),
            )
            .hint(&format!(
                "List them with: sona work docs --meeting {}",
                args.meeting
            ))
        })?;
    if document.document.media_type != "application/pdf" {
        return Err(ExternalError::invalid("Only a PDF can be marked."));
    }
    let differences =
        match document.differences {
            None => {
                return Err(ExternalError::invalid(
                    "The meeting's transcript was deleted, so there is nothing to compare.",
                ))
            }
            Some(differences) if differences.is_empty() => return Err(ExternalError::invalid(
                "Nothing said in the meeting differs from this PDF, so there is nothing to mark.",
            )),
            Some(differences) => differences,
        };
    let marks: Vec<DocumentMark> = differences
        .iter()
        .filter_map(|difference| {
            difference.page.map(|page| DocumentMark {
                page,
                passage: difference.passage.clone(),
                note: mark_note(difference, &review.session),
            })
        })
        .collect();
    let path = args.out.display().to_string();
    let id = args.change.write.operation_id(
        "doc-marked-copy",
        &(session.uuid(), args.document, &path, &marks),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(marked(marks.len(), dry_run), dry_run, id);
    answer.meeting_id = Some(session.uuid());
    answer.path = Some(path.clone());
    if dry_run {
        return Ok(answer);
    }
    gate("saving a file")?;
    let made = meetings
        .doc_marked_copy(MarkedCopyRequest {
            session_id: session,
            document_id: document.document.id,
            destination_path: path,
            marks,
        })
        .await
        .map_err(command_error(session.uuid()))?;
    answer.summary = marked(usize::try_from(made).unwrap_or(usize::MAX), false);
    Ok(answer)
}

impl Tracker<'_> {
    async fn context_suggestions(
        &self,
        args: &MeetingRef,
    ) -> Result<ExternalContextSuggestions, ExternalError> {
        let session = session_id(&args.meeting)?;
        let store = library(self.meetings).await?;
        let kept = visible_meeting(&store, session)?;
        let waiting =
            crate::commands::context_suggestions::context_suggestions(self.app.state(), session)
                .await
                .map_err(|message| ExternalError::new(ExternalErrorCode::Unavailable, message))?;
        let suggestions = agent_maybes(&store, &kept, session, waiting)?;
        Ok(ExternalContextSuggestions {
            schema_version: QUERY_SCHEMA_VERSION,
            meeting_id: session.uuid(),
            suggestions,
        })
    }

    async fn untracked(&self, args: &UntrackedArgs) -> Result<ExternalUntracked, ExternalError> {
        let store = library(self.meetings).await?;
        let found = match &args.meeting {
            Some(value) => {
                let session = session_id(value)?;
                visible_meeting(&store, session)?;
                let read = untracked::meeting_untracked_actions(
                    self.app.state(),
                    self.app.state(),
                    session,
                )
                .await
                .map_err(|message| refusal(self.history, message))?;
                vec![(session, read.actions)]
            }
            None => {
                let kept = kept_local(&store)?;
                untracked::untracked_everywhere(self.history, self.meetings, &|id| kept.hides(id))
                    .await
                    .map_err(|message| refusal(self.history, message))?
            }
        };
        let mut meetings = Vec::with_capacity(found.len());
        for (session, actions) in found {
            let title = store
                .session_snapshot(session)
                .map_err(|error| meeting_refusal(session, error))?
                .title;
            meetings.push(ExternalUntrackedMeeting {
                meeting_id: session.uuid(),
                title,
                actions: actions
                    .into_iter()
                    .map(|row| ExternalUntrackedAction {
                        loop_id: row.loop_id.0,
                        text: row.text,
                        owner: row.owner_display_name.or(row.owner_text),
                        quote: row.quote,
                        at_ms: row.at_ms,
                    })
                    .collect(),
            });
        }
        Ok(ExternalUntracked {
            schema_version: QUERY_SCHEMA_VERSION,
            meetings,
        })
    }

    pub(super) async fn meeting_review(
        &self,
        verb: &MeetingReviewVerb,
    ) -> Result<WorkAnswer, ExternalError> {
        let meetings = self.meetings;
        let gate = |what: &str| self.applies(what);
        let answer = match verb {
            MeetingReviewVerb::MeetingAnalytics(args) => {
                ExternalMeetingReviewAnswer::Analytics(analytics(meetings, args).await?)
            }
            MeetingReviewVerb::MeetingPeople(args) => {
                ExternalMeetingReviewAnswer::People(people(meetings, args).await?)
            }
            MeetingReviewVerb::MeetingQuestions(args) => {
                ExternalMeetingReviewAnswer::Questions(questions(meetings, args).await?)
            }
            MeetingReviewVerb::MeetingQuestionForget(args) => {
                return Ok(changed(
                    question_forget(meetings, self.history, args, &gate).await?,
                ));
            }
            MeetingReviewVerb::MeetingLinks(args) => {
                ExternalMeetingReviewAnswer::Links(links(meetings, args).await?)
            }
            MeetingReviewVerb::MeetingLinkRemove(args) => {
                return Ok(changed(
                    link_remove(meetings, self.history, args, &gate).await?,
                ));
            }
            MeetingReviewVerb::MeetingContext(args) => {
                return Ok(changed(context(meetings, self.history, args, &gate).await?));
            }
            MeetingReviewVerb::ContextSuggestions(args) => {
                ExternalMeetingReviewAnswer::Suggestions(self.context_suggestions(args).await?)
            }
            MeetingReviewVerb::Untracked(args) => {
                ExternalMeetingReviewAnswer::Untracked(self.untracked(args).await?)
            }
            MeetingReviewVerb::CallNameDismiss(args) => {
                return Ok(changed(call_name_dismiss(meetings, args, &gate).await?));
            }
            MeetingReviewVerb::DocMarkedCopy(args) => {
                return Ok(changed(marked_copy(meetings, args, &gate).await?));
            }
        };
        Ok(WorkAnswer::MeetingReview(Box::new(answer)))
    }
}

fn kind_label(kind: ConversationKind) -> &'static str {
    match kind {
        ConversationKind::Unspecified => "Conversation",
        ConversationKind::Huddle => "Huddle",
        ConversationKind::OnlineCall => "Online call",
        ConversationKind::InPerson => "In person",
        ConversationKind::NoteSession => "Note session",
        ConversationKind::Imported => "Imported recording",
    }
}

fn target_line(target: &FilingTarget) -> String {
    match target {
        FilingTarget::Event {
            title,
            start_utc_ms,
            ..
        } => format!("the event {title} at {}", local_time(*start_utc_ms)),
        FilingTarget::Person { name, .. } => format!("with {name}"),
        FilingTarget::Project { title, .. } => format!("for the project {title}"),
        FilingTarget::Kind { conversation_kind } => {
            format!("a kind: {}", kind_label(*conversation_kind))
        }
        FilingTarget::Place { location } => format!("at {}", location.name),
        FilingTarget::Introducer { name, .. } => format!("introduced by {name}"),
        FilingTarget::Meeting {
            title,
            people,
            place,
            ..
        } => {
            let mut line = format!("the meeting {title}");
            if !people.is_empty() {
                let names: Vec<&str> = people.iter().map(|person| person.name.as_str()).collect();
                let _ = write!(line, " with {}", names.join(", "));
            }
            if let Some(place) = place {
                let _ = write!(line, " at {place}");
            }
            line
        }
    }
}

fn answer_state(state: MeetingAnswerState) -> &'static str {
    match state {
        MeetingAnswerState::Supported => "Answered from the transcript",
        MeetingAnswerState::InsufficientEvidence => "The transcript does not say",
        MeetingAnswerState::Unavailable => "No model was available to answer",
        MeetingAnswerState::OutOfDate => "Asked of an older transcript",
        MeetingAnswerState::Forgotten => "Forgotten",
    }
}

fn share(permille: u32) -> String {
    format!("{}.{}%", permille / 10, permille % 10)
}

pub(super) fn text(answer: &ExternalMeetingReviewAnswer, text: &mut String) {
    match answer {
        ExternalMeetingReviewAnswer::Analytics(analytics) => {
            let _ = writeln!(
                text,
                "{} of talking, {} turns, the floor changed hands {} times",
                clock(analytics.speaking_ms),
                analytics.turns,
                analytics.interactions
            );
            for speaker in &analytics.speakers {
                let _ = writeln!(
                    text,
                    "  {}  {}  {}  {} turns  longest {}",
                    speaker.name,
                    share(speaker.share_permille),
                    clock(speaker.speaking_ms),
                    speaker.turns,
                    clock(speaker.longest_monologue_ms)
                );
            }
            if let (Some(name), Some(at)) = (
                &analytics.longest_monologue_speaker,
                analytics.longest_monologue_at_ms,
            ) {
                let _ = writeln!(
                    text,
                    "Longest monologue: {name}, {} from {}",
                    clock(analytics.longest_monologue_ms),
                    clock(at)
                );
            }
            if let Some(gap) = analytics.median_switch_gap_ms {
                let _ = writeln!(text, "Median gap between speakers: {gap} ms");
            }
            for tracker in &analytics.trackers {
                let _ = writeln!(text, "Tracker {}: {} hits", tracker.name, tracker.hits);
            }
        }
        ExternalMeetingReviewAnswer::People(page) => {
            if page.people.is_empty() {
                text.push_str("No one Sona knows is linked to this meeting.\n");
            }
            for person in &page.people {
                let _ = writeln!(
                    text,
                    "{}  {} meetings before  {}",
                    person.display_name,
                    person.prior_meetings,
                    person.person_id.uuid()
                );
                if let Some(last) = &person.last_prior_meeting {
                    let _ = writeln!(text, "  last: {}, {}", last.title, day(last.at_utc_ms));
                }
                if let Some(open) = &person.top_open_loop {
                    let _ = writeln!(text, "  open: {}", open.text);
                }
            }
        }
        ExternalMeetingReviewAnswer::Questions(page) => {
            if page.questions.is_empty() {
                text.push_str("No questions asked of this meeting.\n");
            }
            for question in &page.questions {
                let _ = writeln!(
                    text,
                    "{}  {}",
                    question.question_id.uuid(),
                    question.question.as_deref().unwrap_or("(forgotten)")
                );
                let _ = writeln!(text, "  {}", answer_state(question.state));
                if let Some(said) = &question.answer {
                    let _ = writeln!(text, "  {said}");
                }
            }
            if page.left_out {
                text.push_str("Questions that read a meeting kept on this Mac are left out.\n");
            }
        }
        ExternalMeetingReviewAnswer::Links(page) => {
            if page.links.is_empty() {
                text.push_str("No links kept from this meeting.\n");
            }
            for link in &page.links {
                let _ = writeln!(text, "{}  {}", link.label, link.item_id.uuid());
                if let Some(address) = link.url.as_deref().or(link.document_path.as_deref()) {
                    let _ = writeln!(text, "  {address}");
                }
            }
        }
        ExternalMeetingReviewAnswer::Suggestions(page) => {
            if page.suggestions.is_empty() {
                text.push_str("Nothing waits on this recording.\n");
            }
            for suggestion in &page.suggestions {
                let _ = writeln!(
                    text,
                    "{}  {}",
                    suggestion.id,
                    target_line(&suggestion.target)
                );
            }
            if !page.suggestions.is_empty() {
                text.push_str(
                    "Decide one with: sona work filing ID (add --decline to turn it down)\n",
                );
            }
        }
        ExternalMeetingReviewAnswer::Untracked(page) => {
            if page.meetings.is_empty() {
                text.push_str("Every commitment is tracked or set aside.\n");
            }
            for meeting in &page.meetings {
                let _ = writeln!(text, "{}  ({})", meeting.title, meeting.meeting_id);
                for action in &meeting.actions {
                    let owner = action
                        .owner
                        .as_deref()
                        .map(|owner| format!("{owner}: "))
                        .unwrap_or_default();
                    let _ = writeln!(
                        text,
                        "  {}  {owner}{}  ({})",
                        clock(action.at_ms),
                        action.text,
                        action.loop_id
                    );
                }
            }
            if !page.meetings.is_empty() {
                text.push_str("Track one with: sona work action-track LOOP_ID --project PROJECT\n");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{people, MeetingRef};
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::session::{MeetingSessionManager, NoCaptureSources};
    use crate::meeting::store::workflow_core_tests::{link, person, reviewable_meeting};
    use crate::meeting::types::MeetingOperationId;
    use crate::secrets::{MemorySecretBackend, SecretManager};
    use std::sync::Arc;
    use tempfile::TempDir;

    const NOW: i64 = 1_767_000_000_000;

    #[test]
    fn a_meeting_kept_on_this_mac_is_not_among_the_meetings_before() {
        tauri::async_runtime::block_on(async {
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
            let alice = person(&store, "Alice Doe", &[], &[]);
            let first = reviewable_meeting(&store, "Pricing", NOW);
            let board = reviewable_meeting(&store, "Board prep", NOW + 1);
            let newest = reviewable_meeting(&store, "Pricing again", NOW + 2);
            for meeting in [first, board, newest] {
                link(&store, meeting, alice, "manual", "confirmed");
            }
            store
                .remember_calendar_facts(
                    board,
                    &CalendarEventSummary {
                        event_key: format!("board#{}", NOW + 1),
                        series_key: "board".to_owned(),
                        title: "Board prep".to_owned(),
                        attendee_count: 2,
                        start_utc_ms: NOW + 1,
                        end_utc_ms: NOW + 1_800_001,
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
                        series_key: "board".to_owned(),
                        remote_intelligence_opt_out: true,
                        expected_revision: 0,
                    },
                    NOW,
                )
                .unwrap();

            let listed = people(
                &meetings,
                &MeetingRef {
                    meeting: newest.uuid().to_string(),
                },
            )
            .await
            .unwrap();

            assert_eq!(listed.people[0].prior_meetings, 1);
            assert_eq!(
                listed.people[0]
                    .last_prior_meeting
                    .as_ref()
                    .map(|last| last.id),
                Some(first)
            );
        });
    }
}
