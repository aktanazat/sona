//! The transcript verbs: rename a meeting's speaker, merge two speakers, fix
//! the words of one line, and the corrections Sona keeps for how a person's
//! words are heard, each through the same step the transcript screen takes.

use super::library::{
    file_made, made_before, meeting_operation, operation, receipt_replayed, told, Change, Gate,
};
use super::speakers::{listed, pick_speaker};
use super::{Tracker, WorkAnswer, READ_SCOPES, WRITE_SCOPES};
use crate::managers::history::HistoryManager;
use crate::meeting::people_types::PersonId;
use crate::meeting::session::{
    MeetingSegmentEditRequest, MeetingSessionManager, MeetingSpeakerMergeRequest,
    MeetingSpeakerRenameRequest,
};
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::transcript_correction_types::{
    TranscriptCorrection, TranscriptCorrectionContext, TranscriptCorrectionKind,
    TranscriptCorrectionRemoveRequest, TranscriptCorrectionTeachRequest,
    TranscriptCorrectionUpdateRequest,
};
use crate::meeting::types::{
    MeetingCommandKind, MeetingOperationId, MeetingReviewSnapshot, MeetingSessionId,
    OperationResult, SpeakerId, TranscriptSegmentId,
};
use crate::query::external::{
    command_error, kept_local, meeting_refusal, person_id, session_id, store, visible_meeting,
    ExternalError, ExternalErrorCode, ExternalScope,
};
use crate::query::{QueryError, QUERY_SCHEMA_VERSION};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::fmt::Write as _;
use std::sync::Arc;
use uuid::Uuid;

const CORRECTION_REMOVE: &str = "cli_correction_remove";

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SpeakerRenameArgs {
    /// The meeting's id, from `meetings`.
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The speaker as the transcript names them ("Speaker 2"), or their id from `meeting`.
    #[arg(value_name = "SPEAKER")]
    pub speaker: String,
    /// The name the transcript shows for them from now on.
    #[arg(value_name = "NAME")]
    pub name: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SpeakerMergeArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The speaker whose lines move, by name or id.
    #[arg(value_name = "SPEAKER")]
    pub speaker: String,
    /// The speaker who keeps them, by name or id.
    #[arg(long, value_name = "SPEAKER")]
    pub into: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LineEditArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The line's segment_id, from `sona --transcript MEETING --json`.
    #[arg(value_name = "LINE")]
    pub line: Uuid,
    /// The words the line should read.
    #[arg(value_name = "TEXT")]
    pub text: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CorrectionsArgs {
    /// The person, by name or id.
    #[arg(value_name = "PERSON")]
    pub person: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CorrectionKind {
    Phrase,
    Name,
    Term,
    Dialect,
    CodeSwitching,
}

impl From<CorrectionKind> for TranscriptCorrectionKind {
    fn from(kind: CorrectionKind) -> Self {
        match kind {
            CorrectionKind::Phrase => Self::Phrase,
            CorrectionKind::Name => Self::Name,
            CorrectionKind::Term => Self::Term,
            CorrectionKind::Dialect => Self::Dialect,
            CorrectionKind::CodeSwitching => Self::CodeSwitching,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CorrectionWhere {
    /// Wherever this person says it.
    Person,
    /// Only in the meeting the line is from.
    Conversation,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CorrectionScopeArgs {
    /// Where the correction applies. Dialect and code-switching need conversation or --around.
    #[arg(long, value_enum, value_name = "WHERE", conflicts_with = "around")]
    pub context: Option<CorrectionWhere>,
    /// Apply it only where the line also says this phrase.
    #[arg(long, value_name = "PHRASE")]
    pub around: Option<String>,
}

impl CorrectionScopeArgs {
    fn context(&self) -> Option<TranscriptCorrectionContext> {
        match (&self.around, self.context) {
            (Some(phrase), _) => Some(TranscriptCorrectionContext::SurroundingPhrase {
                phrase: phrase.trim().to_owned(),
            }),
            (None, Some(CorrectionWhere::Person)) => Some(TranscriptCorrectionContext::Person),
            (None, Some(CorrectionWhere::Conversation)) => {
                Some(TranscriptCorrectionContext::Conversation)
            }
            (None, None) => None,
        }
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CorrectionAddArgs {
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// The line's segment_id. Its speaker must already be a confirmed person.
    #[arg(value_name = "LINE")]
    pub line: Uuid,
    /// The words the transcriber wrote, as they appear in the line.
    #[arg(long, value_name = "WORDS")]
    pub heard: String,
    /// The words the person said.
    #[arg(long, value_name = "WORDS")]
    pub fix: String,
    #[arg(long, value_enum, default_value = "phrase")]
    pub kind: CorrectionKind,
    #[command(flatten)]
    pub scope: CorrectionScopeArgs,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CorrectionEditArgs {
    /// The person, by name or id.
    #[arg(value_name = "PERSON")]
    pub person: String,
    /// The correction's id, from `corrections`.
    #[arg(value_name = "CORRECTION")]
    pub correction: Uuid,
    #[arg(long, value_name = "WORDS")]
    pub heard: Option<String>,
    #[arg(long, value_name = "WORDS")]
    pub fix: Option<String>,
    #[arg(long, value_enum)]
    pub kind: Option<CorrectionKind>,
    #[command(flatten)]
    pub scope: CorrectionScopeArgs,
    /// Stop applying it, without deleting it.
    #[arg(long, conflicts_with = "resume")]
    pub pause: bool,
    /// Apply it again.
    #[arg(long)]
    pub resume: bool,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CorrectionRemoveArgs {
    /// The person, by name or id.
    #[arg(value_name = "PERSON")]
    pub person: String,
    /// The correction's id, from `corrections`.
    #[arg(value_name = "CORRECTION")]
    pub correction: Uuid,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum TranscriptVerb {
    /// Rename a meeting's speaker, as the transcript's speaker menu does.
    SpeakerRename(SpeakerRenameArgs),
    /// Fold one speaker's lines into another's. Voice samples kept from the
    /// folded speaker are deleted.
    SpeakerMerge(SpeakerMergeArgs),
    /// Change the words of one transcript line.
    LineEdit(LineEditArgs),
    /// The corrections Sona applies to how a person's words are heard.
    Corrections(CorrectionsArgs),
    /// Teach a correction from one line: what was heard, and what was said.
    CorrectionAdd(CorrectionAddArgs),
    /// Change, pause or resume a correction.
    CorrectionEdit(CorrectionEditArgs),
    /// Delete a correction.
    CorrectionRemove(CorrectionRemoveArgs),
}

impl TranscriptVerb {
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self {
            Self::Corrections(_) => READ_SCOPES,
            Self::SpeakerRename(SpeakerRenameArgs { change, .. })
            | Self::SpeakerMerge(SpeakerMergeArgs { change, .. })
            | Self::LineEdit(LineEditArgs { change, .. })
            | Self::CorrectionAdd(CorrectionAddArgs { change, .. })
            | Self::CorrectionEdit(CorrectionEditArgs { change, .. })
            | Self::CorrectionRemove(CorrectionRemoveArgs { change, .. }) => {
                if change.dry_run {
                    READ_SCOPES
                } else {
                    WRITE_SCOPES
                }
            }
        }
    }

    pub(super) fn check(&self) -> Result<(), ExternalError> {
        let refusal = match self {
            Self::SpeakerRename(args) if args.name.trim().is_empty() => "A speaker needs a name.",
            Self::LineEdit(args) if args.text.trim().is_empty() => "A line needs some words.",
            Self::CorrectionAdd(args)
                if args.heard.trim().is_empty() || args.fix.trim().is_empty() =>
            {
                "A correction needs the words heard and the words said."
            }
            _ => return Ok(()),
        };
        Err(ExternalError::invalid(refusal))
    }
}

/// One change to a transcript or its corrections. The ids name what it touched.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalTranscriptChange {
    pub schema_version: u32,
    pub summary: String,
    pub dry_run: bool,
    /// True when an earlier run with the same operation id made it, or it
    /// already reads that way.
    pub replayed: bool,
    pub operation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meeting_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correction_id: Option<Uuid>,
}

/// A person's corrections, without those taught from a meeting kept on this Mac.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalCorrections {
    pub schema_version: u32,
    pub person_id: Uuid,
    pub corrections: Vec<TranscriptCorrection>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ExternalTranscriptAnswer {
    Change(ExternalTranscriptChange),
    Corrections(ExternalCorrections),
}

fn change(summary: String, dry_run: bool, operation_id: String) -> ExternalTranscriptChange {
    ExternalTranscriptChange {
        schema_version: QUERY_SCHEMA_VERSION,
        summary,
        dry_run,
        replayed: false,
        operation_id,
        meeting_id: None,
        speaker_id: None,
        line_id: None,
        person_id: None,
        correction_id: None,
    }
}

/// The meeting, refused as absent when it is kept on this Mac.
async fn meeting(
    meetings: &Arc<MeetingSessionManager>,
    value: &str,
) -> Result<(Arc<MeetingStore>, MeetingReviewSnapshot), ExternalError> {
    let session = session_id(value)?;
    let store = store(meetings).await?;
    visible_meeting(&store, session)?;
    let review = store
        .review_snapshot(session)
        .map_err(|error| meeting_refusal(session, error))?;
    Ok((store, review))
}

/// Whether the caller's operation id already made this kind of change to
/// this meeting. Asked before the speakers are read: a merge that ran leaves
/// its speaker gone.
fn ran_before(
    store: &MeetingStore,
    change: &Change,
    session: MeetingSessionId,
    command: MeetingCommandKind,
) -> Result<bool, ExternalError> {
    let Some(given) = change.write.operation_id else {
        return Ok(false);
    };
    let Some(receipt) = store
        .operation_receipt(MeetingOperationId::from_uuid(given))
        .map_err(|error| meeting_refusal(session, error))?
    else {
        return Ok(false);
    };
    if receipt.command != command || receipt.session_id != Some(session) {
        return Err(ExternalError::invalid(format!(
            "Operation id {given} was already used for a different change. Pass a new --operation-id, or leave it out."
        )));
    }
    Ok(receipt.result == OperationResult::Committed)
}

pub(super) async fn speaker_rename(
    meetings: &Arc<MeetingSessionManager>,
    args: &SpeakerRenameArgs,
    gate: Gate<'_>,
) -> Result<ExternalTranscriptChange, ExternalError> {
    let (store, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let revision = review.session.revision;
    let name = args.name.trim().to_owned();
    let speakers = listed(meetings, &store, session).await?;
    let found = pick_speaker(&speakers, &args.speaker, &args.meeting)?;
    let (id, operation_id) = meeting_operation(
        &store,
        &args.change.write,
        "speaker-rename",
        &(session.uuid(), found.speaker_id, revision, &name),
        revision,
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Renamed the speaker", "Would rename the speaker"),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    answer.speaker_id = Some(found.speaker_id);
    answer.replayed = found.name == name;
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("renaming a meeting's speaker")?;
    let result = meetings
        .speaker_rename(MeetingSpeakerRenameRequest {
            operation_id,
            session_id: session,
            expected_revision: revision,
            speaker_id: SpeakerId::from_uuid(found.speaker_id),
            display_name: name,
        })
        .await
        .map_err(command_error(session.uuid()))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    Ok(answer)
}

pub(super) async fn speaker_merge(
    meetings: &Arc<MeetingSessionManager>,
    args: &SpeakerMergeArgs,
    gate: Gate<'_>,
) -> Result<ExternalTranscriptChange, ExternalError> {
    let (store, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let revision = review.session.revision;
    let dry_run = args.change.dry_run;
    let summary = told(dry_run, "Merged the speakers", "Would merge the speakers");
    if ran_before(
        &store,
        &args.change,
        session,
        MeetingCommandKind::SpeakerMerge,
    )? {
        let mut answer = change(summary, dry_run, args.change.write.fresh_operation_id());
        answer.meeting_id = Some(session.uuid());
        answer.replayed = true;
        return Ok(answer);
    }
    let speakers = listed(meetings, &store, session).await?;
    let source = pick_speaker(&speakers, &args.speaker, &args.meeting)?.speaker_id;
    let target = pick_speaker(&speakers, &args.into, &args.meeting)?.speaker_id;
    if source == target {
        return Err(ExternalError::invalid(
            "A speaker cannot be merged into themselves.",
        ));
    }
    let (id, operation_id) = meeting_operation(
        &store,
        &args.change.write,
        "speaker-merge",
        &(session.uuid(), source, target, revision),
        revision,
    )?;
    let mut answer = change(summary, dry_run, id);
    answer.meeting_id = Some(session.uuid());
    answer.speaker_id = Some(target);
    if dry_run {
        return Ok(answer);
    }
    gate("merging a meeting's speakers")?;
    let result = meetings
        .speaker_merge(MeetingSpeakerMergeRequest {
            operation_id,
            session_id: session,
            expected_revision: revision,
            source_speaker_id: SpeakerId::from_uuid(source),
            target_speaker_id: SpeakerId::from_uuid(target),
        })
        .await
        .map_err(command_error(session.uuid()))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    Ok(answer)
}

pub(super) fn missing_line(line: Uuid, meeting: &str) -> ExternalError {
    ExternalError::new(
        ExternalErrorCode::NotFound,
        format!("No line {line} in this meeting."),
    )
    .hint(&format!(
        "List them with: sona --transcript {meeting} --json"
    ))
}

pub(super) async fn line_edit(
    meetings: &Arc<MeetingSessionManager>,
    args: &LineEditArgs,
    gate: Gate<'_>,
) -> Result<ExternalTranscriptChange, ExternalError> {
    let (store, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let revision = review.session.revision;
    let text = args.text.trim().to_owned();
    let segment = review
        .transcript
        .iter()
        .find(|segment| segment.base.segment_id.uuid() == args.line && !segment.removed)
        .ok_or_else(|| missing_line(args.line, &args.meeting))?;
    let (id, operation_id) = meeting_operation(
        &store,
        &args.change.write,
        "line-edit",
        &(session.uuid(), args.line, revision, &text),
        revision,
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(dry_run, "Changed the line", "Would change the line"),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    answer.line_id = Some(args.line);
    answer.replayed = segment
        .replacement_text
        .as_deref()
        .unwrap_or(&segment.base.text)
        .trim()
        == text;
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("changing a transcript line")?;
    let result = meetings
        .segment_edit(MeetingSegmentEditRequest {
            operation_id,
            session_id: session,
            expected_revision: revision,
            segment_id: TranscriptSegmentId::from_uuid(args.line),
            replacement_text: text,
            removed: false,
        })
        .await
        .map_err(command_error(session.uuid()))?;
    answer.replayed = receipt_replayed(&result.receipt, revision)?;
    Ok(answer)
}

fn correction_error(error: StoreError) -> ExternalError {
    match error {
        StoreError::NotFound => ExternalError::new(
            ExternalErrorCode::NotFound,
            "No such correction or person.",
        ),
        StoreError::Conflict | StoreError::TranscriptCorrectionConflict => ExternalError::invalid(
            "Another correction already changes those words differently, or the meeting cannot be edited now.",
        ),
        StoreError::ExplicitConsentRequired => ExternalError::invalid(
            "Only a line whose speaker is a confirmed person can teach a correction, and a correction whose line changed is resumed in Sona.",
        ),
        StoreError::StaleRevision => ExternalError::invalid(
            "The transcript or the corrections changed while this ran. Read them again and retry.",
        ),
        StoreError::Invalid => ExternalError::invalid(
            "The words heard must appear in the line, and dialect or code-switching needs --context conversation or --around.",
        ),
        error => QueryError::from(error).into(),
    }
}

/// A person's corrections an agent may see: none taught from a meeting kept
/// on this Mac.
fn visible_corrections(
    store: &MeetingStore,
    person: PersonId,
) -> Result<(u64, Vec<TranscriptCorrection>), ExternalError> {
    let kept = kept_local(store)?;
    let list = store
        .transcript_corrections(person)
        .map_err(correction_error)?;
    Ok((
        list.revision,
        list.rules
            .into_iter()
            .filter(|rule| !kept.hides(rule.source_session_id))
            .collect(),
    ))
}

fn missing_correction(correction: Uuid) -> ExternalError {
    ExternalError::new(
        ExternalErrorCode::NotFound,
        format!("No correction {correction} for this person."),
    )
    .hint("List them with: sona work corrections PERSON")
}

pub(super) async fn corrections(
    meetings: &Arc<MeetingSessionManager>,
    person: PersonId,
) -> Result<ExternalCorrections, ExternalError> {
    let store = store(meetings).await?;
    Ok(ExternalCorrections {
        schema_version: QUERY_SCHEMA_VERSION,
        person_id: person.uuid(),
        corrections: visible_corrections(&store, person)?.1,
    })
}

pub(super) async fn correction_add(
    meetings: &Arc<MeetingSessionManager>,
    args: &CorrectionAddArgs,
    gate: Gate<'_>,
) -> Result<ExternalTranscriptChange, ExternalError> {
    let (store, review) = meeting(meetings, &args.meeting).await?;
    let session = review.session.session_id;
    let lines = store
        .transcript_correction_review(session)
        .map_err(|error| meeting_refusal(session, error))?;
    let line = lines
        .segments
        .iter()
        .find(|segment| segment.segment_id.uuid() == args.line)
        .ok_or_else(|| missing_line(args.line, &args.meeting))?;
    let person = match line.person_id {
        Some(person) if line.identity_confirmed => person,
        _ => {
            return Err(ExternalError::invalid(
                "Only a line whose speaker is a confirmed person can teach a correction.",
            )
            .hint(&format!(
                "Say who the speaker is with: sona work speaker-assign {} SPEAKER --person P",
                args.meeting
            )))
        }
    };
    let heard = args.heard.trim().to_owned();
    let fix = args.fix.trim().to_owned();
    let context = args
        .scope
        .context()
        .unwrap_or(TranscriptCorrectionContext::Person);
    let kind = TranscriptCorrectionKind::from(args.kind);
    let (revision, rules) = visible_corrections(&store, person)?;
    let (id, _) = operation(
        &args.change.write,
        "correction-add",
        &(session.uuid(), args.line, &heard, &fix, kind, &context),
    )?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(
            dry_run,
            "Taught the correction",
            "Would teach the correction",
        ),
        dry_run,
        id,
    );
    answer.meeting_id = Some(session.uuid());
    answer.line_id = Some(args.line);
    answer.person_id = Some(person.uuid());
    let same = |rule: &TranscriptCorrection| {
        rule.original_phrase == heard
            && rule.replacement_phrase == fix
            && rule.context == context
            && (context != TranscriptCorrectionContext::Conversation
                || rule.source_session_id == session)
    };
    if let Some(existing) = rules.iter().find(|rule| same(rule)) {
        answer.correction_id = Some(existing.correction_id);
        answer.replayed = true;
        return Ok(answer);
    }
    if dry_run {
        return Ok(answer);
    }
    gate("teaching a transcript correction")?;
    let result = store
        .teach_transcript_correction(TranscriptCorrectionTeachRequest {
            person_id: person,
            session_id: session,
            segment_id: line.segment_id,
            expected_meeting_revision: lines.meeting_revision,
            expected_correction_revision: revision,
            original_phrase: heard.clone(),
            replacement_phrase: fix.clone(),
            kind,
            context: context.clone(),
        })
        .map_err(correction_error)?;
    meetings.emit_transcript_correction_changes(result.changed_sessions);
    answer.correction_id = result
        .corrections
        .rules
        .iter()
        .find(|rule| same(rule))
        .map(|rule| rule.correction_id);
    Ok(answer)
}

pub(super) async fn correction_edit(
    meetings: &Arc<MeetingSessionManager>,
    person: PersonId,
    args: &CorrectionEditArgs,
    gate: Gate<'_>,
) -> Result<ExternalTranscriptChange, ExternalError> {
    let store = store(meetings).await?;
    let (revision, rules) = visible_corrections(&store, person)?;
    let rule = rules
        .iter()
        .find(|rule| rule.correction_id == args.correction)
        .ok_or_else(|| missing_correction(args.correction))?;
    let request = TranscriptCorrectionUpdateRequest {
        correction_id: rule.correction_id,
        person_id: person,
        expected_correction_revision: revision,
        original_phrase: args.heard.as_deref().map_or_else(
            || rule.original_phrase.clone(),
            |heard| heard.trim().to_owned(),
        ),
        replacement_phrase: args.fix.as_deref().map_or_else(
            || rule.replacement_phrase.clone(),
            |fix| fix.trim().to_owned(),
        ),
        kind: args.kind.map_or(rule.kind, TranscriptCorrectionKind::from),
        context: args.scope.context().unwrap_or_else(|| rule.context.clone()),
        enabled: if args.pause {
            false
        } else {
            args.resume || rule.enabled
        },
    };
    let (id, _) = operation(&args.change.write, "correction-edit", &request)?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(
            dry_run,
            "Changed the correction",
            "Would change the correction",
        ),
        dry_run,
        id,
    );
    answer.person_id = Some(person.uuid());
    answer.correction_id = Some(rule.correction_id);
    answer.replayed = request.original_phrase == rule.original_phrase
        && request.replacement_phrase == rule.replacement_phrase
        && request.kind == rule.kind
        && request.context == rule.context
        && request.enabled == rule.enabled;
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("changing a transcript correction")?;
    let result = store
        .update_transcript_correction(request)
        .map_err(correction_error)?;
    meetings.emit_transcript_correction_changes(result.changed_sessions);
    Ok(answer)
}

pub(super) async fn correction_remove(
    meetings: &Arc<MeetingSessionManager>,
    history: &HistoryManager,
    person: PersonId,
    args: &CorrectionRemoveArgs,
    gate: Gate<'_>,
) -> Result<ExternalTranscriptChange, ExternalError> {
    let store = store(meetings).await?;
    let request = format!("{}\0{}", person.uuid(), args.correction);
    let (id, _) = operation(&args.change.write, "correction-remove", &request)?;
    let dry_run = args.change.dry_run;
    let mut answer = change(
        told(
            dry_run,
            "Deleted the correction",
            "Would delete the correction",
        ),
        dry_run,
        id,
    );
    answer.person_id = Some(person.uuid());
    answer.correction_id = Some(args.correction);
    if made_before(history, &answer.operation_id, CORRECTION_REMOVE, &request)? {
        answer.replayed = true;
        return Ok(answer);
    }
    let (revision, rules) = visible_corrections(&store, person)?;
    if !rules
        .iter()
        .any(|rule| rule.correction_id == args.correction)
    {
        return Err(missing_correction(args.correction));
    }
    if dry_run {
        return Ok(answer);
    }
    gate("deleting a transcript correction")?;
    let result = store
        .remove_transcript_correction(TranscriptCorrectionRemoveRequest {
            correction_id: args.correction,
            person_id: person,
            expected_correction_revision: revision,
        })
        .map_err(correction_error)?;
    meetings.emit_transcript_correction_changes(result.changed_sessions);
    file_made(history, &answer.operation_id, CORRECTION_REMOVE, &request)?;
    Ok(answer)
}

impl Tracker<'_> {
    pub(super) async fn person_named(&self, value: &str) -> Result<PersonId, ExternalError> {
        let board = self.board().await?;
        person_id(&board.person(value)?.id)
    }

    pub(super) async fn transcript(
        &self,
        verb: &TranscriptVerb,
    ) -> Result<WorkAnswer, ExternalError> {
        let meetings = self.meetings;
        let gate = |what: &str| self.applies(what);
        let answer = match verb {
            TranscriptVerb::SpeakerRename(args) => {
                ExternalTranscriptAnswer::Change(speaker_rename(meetings, args, &gate).await?)
            }
            TranscriptVerb::SpeakerMerge(args) => {
                ExternalTranscriptAnswer::Change(speaker_merge(meetings, args, &gate).await?)
            }
            TranscriptVerb::LineEdit(args) => {
                ExternalTranscriptAnswer::Change(line_edit(meetings, args, &gate).await?)
            }
            TranscriptVerb::Corrections(args) => ExternalTranscriptAnswer::Corrections(
                corrections(meetings, self.person_named(&args.person).await?).await?,
            ),
            TranscriptVerb::CorrectionAdd(args) => {
                ExternalTranscriptAnswer::Change(correction_add(meetings, args, &gate).await?)
            }
            TranscriptVerb::CorrectionEdit(args) => {
                let person = self.person_named(&args.person).await?;
                ExternalTranscriptAnswer::Change(
                    correction_edit(meetings, person, args, &gate).await?,
                )
            }
            TranscriptVerb::CorrectionRemove(args) => {
                let person = self.person_named(&args.person).await?;
                ExternalTranscriptAnswer::Change(
                    correction_remove(meetings, self.history, person, args, &gate).await?,
                )
            }
        };
        Ok(WorkAnswer::Transcript(Box::new(answer)))
    }
}

pub(super) fn text(answer: &ExternalTranscriptAnswer, text: &mut String) {
    match answer {
        ExternalTranscriptAnswer::Change(change) => {
            let _ = writeln!(
                text,
                "{}{}",
                change.summary,
                if change.replayed && !change.dry_run {
                    " (already so)"
                } else {
                    ""
                }
            );
            if let Some(correction) = change.correction_id {
                let _ = writeln!(text, "  correction {correction}");
            }
        }
        ExternalTranscriptAnswer::Corrections(list) => {
            if list.corrections.is_empty() {
                text.push_str("No corrections.\n");
            }
            for rule in &list.corrections {
                let _ = writeln!(
                    text,
                    "{:?} -> {:?}{}  {}",
                    rule.original_phrase,
                    rule.replacement_phrase,
                    if rule.enabled { "" } else { "  (paused)" },
                    rule.correction_id
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        line_edit, speaker_merge, speaker_rename, LineEditArgs, SpeakerMergeArgs, SpeakerRenameArgs,
    };
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::session::{MeetingSessionManager, NoCaptureSources};
    use crate::meeting::store::workflow_core_tests::{reviewable_meeting, transcript};
    use crate::meeting::types::{MeetingOperationId, MeetingSessionId, SourceKind};
    use crate::query::external::tracker::library::Change;
    use crate::query::external::tracker::speakers::listed;
    use crate::query::external::tracker::WriteArgs;
    use crate::query::external::{ExternalError, ExternalErrorCode};
    use crate::secrets::{MemorySecretBackend, SecretManager};
    use std::sync::Arc;
    use tempfile::TempDir;

    const NOW: i64 = 1_767_000_000_000;

    fn applies(_: &str) -> Result<(), ExternalError> {
        Ok(())
    }

    fn change(dry_run: bool) -> Change {
        Change {
            dry_run,
            write: WriteArgs {
                operation_id: None,
                agent: Some("Organizer".to_owned()),
            },
        }
    }

    async fn corpus() -> (
        TempDir,
        Arc<MeetingSessionManager>,
        MeetingSessionId,
        MeetingSessionId,
    ) {
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
        let pricing = reviewable_meeting(&store, "Pricing sync", NOW);
        transcript(&store, pricing, "We ship on Friday.");
        let kept = reviewable_meeting(&store, "Board prep", NOW + 1);
        transcript(&store, kept, "The board wants the numbers.");
        store
            .diarization_speaker(kept, SourceKind::SystemAudio, 1)
            .unwrap();
        store
            .remember_calendar_facts(
                kept,
                &CalendarEventSummary {
                    event_key: format!("board#{NOW}"),
                    series_key: "board".to_owned(),
                    title: "Board prep".to_owned(),
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
                    series_key: "board".to_owned(),
                    remote_intelligence_opt_out: true,
                    expected_revision: 0,
                },
                NOW,
            )
            .unwrap();
        (directory, meetings, pricing, kept)
    }

    #[test]
    fn speaker_writes_refuse_a_meeting_kept_on_this_mac() {
        tauri::async_runtime::block_on(async {
            let (_directory, meetings, _, kept) = corpus().await;
            let store = meetings.store().await.unwrap();
            let before = listed(&meetings, &store, kept).await.unwrap();
            assert_eq!(before.len(), 2);
            let meeting = kept.uuid().to_string();

            let renamed = speaker_rename(
                &meetings,
                &SpeakerRenameArgs {
                    meeting: meeting.clone(),
                    speaker: before[0].speaker_id.to_string(),
                    name: "Dana Reyes".to_owned(),
                    change: change(false),
                },
                &applies,
            )
            .await
            .unwrap_err();
            assert_eq!(renamed.error, ExternalErrorCode::NotFound);

            let merged = speaker_merge(
                &meetings,
                &SpeakerMergeArgs {
                    meeting,
                    speaker: before[0].speaker_id.to_string(),
                    into: before[1].speaker_id.to_string(),
                    change: change(false),
                },
                &applies,
            )
            .await
            .unwrap_err();
            assert_eq!(merged.error, ExternalErrorCode::NotFound);

            let after = listed(&meetings, &store, kept).await.unwrap();
            assert_eq!(after.len(), 2);
            assert_eq!(after[0].name, before[0].name);
        });
    }

    #[test]
    fn line_edit_dry_run_writes_nothing() {
        tauri::async_runtime::block_on(async {
            let (_directory, meetings, pricing, _) = corpus().await;
            let store = meetings.store().await.unwrap();
            let before = store.review_snapshot(pricing).unwrap();
            let line = before.transcript[0].base.segment_id.uuid();
            let args = |dry_run| LineEditArgs {
                meeting: pricing.uuid().to_string(),
                line,
                text: "We ship on Monday.".to_owned(),
                change: change(dry_run),
            };

            let preview = line_edit(&meetings, &args(true), &applies).await.unwrap();
            assert!(preview.dry_run);
            assert!(!preview.replayed);
            let untouched = store.review_snapshot(pricing).unwrap();
            assert_eq!(untouched.transcript[0].replacement_text, None);
            assert_eq!(untouched.session.revision, before.session.revision);

            let made = line_edit(&meetings, &args(false), &applies).await.unwrap();
            assert!(!made.replayed);
            assert_eq!(made.operation_id, preview.operation_id);
            let edited = store.review_snapshot(pricing).unwrap();
            assert_eq!(
                edited.transcript[0].replacement_text.as_deref(),
                Some("We ship on Monday.")
            );
        });
    }
}
