//! `sona work` verbs for one person's page and the companies pages: read a
//! person whole, link their meetings, split, delete, set or take off the
//! photo, take off the contact, and answer who introduced you.

use super::{
    conversations_closed, now_ms, pick_person, refusal, Tracker, WorkAnswer, WorkPersonChoice,
    WriteArgs,
};
use crate::meeting::document_types::{DocumentId, DocumentSummary};
use crate::meeting::loop_types::{MeetingLoopDirection, MeetingLoopStatus};
use crate::meeting::people_types::{
    CompanySummary, PersonAvatarSetRequest, PersonContactCard, PersonContactSetRequest,
    PersonDeleteRequest, PersonDetail, PersonId, PersonLinkConfidence, PersonLinkRequest,
    PersonLinkSource, PersonPhotoProvenance, PersonSplitRequest, PersonSplitTarget,
};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::MeetingCommandError;
use crate::planning;
use crate::query::external::{
    kept_local, local_time, missing_meeting, session_id, store, ExternalError, ExternalErrorCode,
};
use crate::query::{KeptLocal, QueryError, QUERY_SCHEMA_VERSION};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use clap::{Args, ValueEnum};
use image::imageops::FilterType;
use image::ImageFormat;
use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::future::Future;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

const PHOTO_EDGE: u32 = 512;
const PHOTO_MAX_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonArgs {
    /// The person: a name, another name they go by, or their id.
    pub person: String,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CompaniesArgs {}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CompanyArgs {
    /// The company as its people carry it, or its slug from `companies`.
    pub name: String,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FirstMetArgs {
    pub person: String,
    /// yes takes the introducer Sona heard as who introduced you; no turns it down.
    #[arg(value_enum, value_name = "ANSWER")]
    pub answer: FirstMetChoice,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum FirstMetChoice {
    Yes,
    No,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonLinkArgs {
    pub person: String,
    /// The meeting's id, from `meetings`.
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    /// Confirm a link Sona suggested.
    #[arg(long, conflicts_with = "remove")]
    pub confirm: bool,
    /// Take the person off the meeting.
    #[arg(long)]
    pub remove: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonSplitArgs {
    /// The person to split.
    pub person: String,
    /// Move the picked parts to this existing person.
    #[arg(long, value_name = "PERSON", required_unless_present = "new", conflicts_with = "new")]
    pub into: Option<String>,
    /// Move the picked parts to a new person with this name.
    #[arg(long, value_name = "NAME")]
    pub new: Option<String>,
    /// Meeting ids to move, comma-separated.
    #[arg(long, value_delimiter = ',', value_name = "MEETINGS")]
    pub meetings: Vec<String>,
    /// Other names to move, comma-separated.
    #[arg(long, value_delimiter = ',', value_name = "NAMES")]
    pub aliases: Vec<String>,
    /// Email addresses to move, comma-separated.
    #[arg(long, value_delimiter = ',', value_name = "EMAILS")]
    pub emails: Vec<String>,
    /// Document ids to move, comma-separated, from `person`.
    #[arg(long, value_delimiter = ',', value_name = "DOCUMENTS")]
    pub documents: Vec<String>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonDeleteArgs {
    /// The person to delete, with their meeting links and saved voice.
    pub person: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonPhotoArgs {
    pub person: String,
    /// A PNG or JPEG. Cropped to its middle square, at most 512 pixels across.
    #[arg(long, value_name = "PATH", required_unless_present = "remove", conflicts_with = "remove")]
    pub file: Option<PathBuf>,
    /// Take the photo off.
    #[arg(long)]
    pub remove: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonContactRemoveArgs {
    pub person: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalPersonMeeting {
    pub meeting_id: String,
    pub title: String,
    pub at_ms: i64,
    pub at: String,
    pub source: PersonLinkSource,
    pub confirmed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalPersonLoop {
    pub loop_id: String,
    pub meeting_id: String,
    pub text: String,
    pub direction: MeetingLoopDirection,
    pub status: MeetingLoopStatus,
    pub at_ms: i64,
}

/// One person's page. The written summary is left out while any of their
/// meetings is kept on this Mac: it may draw on that meeting.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalPersonPage {
    pub schema_version: u32,
    pub revision: u64,
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub role: Option<String>,
    pub organization: Option<String>,
    pub emails: Vec<String>,
    pub notes: Option<String>,
    pub summary: Option<String>,
    pub summary_left_out: bool,
    pub meetings: Vec<ExternalPersonMeeting>,
    pub open_loops: Vec<ExternalPersonLoop>,
    pub documents: Vec<DocumentSummary>,
    pub contact: Option<PersonContactCard>,
    pub photo: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalCompanies {
    pub schema_version: u32,
    pub companies: Vec<CompanySummary>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalCompanyPerson {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub meetings: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalCompanyMeeting {
    pub meeting_id: String,
    pub title: String,
    pub at_ms: i64,
    pub at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalCompany {
    pub schema_version: u32,
    pub name: String,
    pub slug: String,
    pub people: Vec<ExternalCompanyPerson>,
    pub meetings: Vec<ExternalCompanyMeeting>,
    pub open_loops: Vec<ExternalPersonLoop>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalIntroduction {
    pub suggestion_id: String,
    pub introducer: String,
    pub quote: String,
    pub confirmed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstMetAnswer {
    Took,
    TurnedDown,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalFirstMet {
    pub schema_version: u32,
    pub person_id: String,
    pub name: String,
    /// True when the first meeting is kept on this Mac, so it is left out.
    pub left_out: bool,
    pub meeting_id: Option<String>,
    pub at_ms: Option<i64>,
    pub at: Option<String>,
    pub place: Option<String>,
    pub introduction: Option<ExternalIntroduction>,
    pub answered: Option<FirstMetAnswer>,
    pub dry_run: bool,
    pub operation_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeopleAction {
    Linked,
    LinkConfirmed,
    Unlinked,
    Split,
    Deleted,
    PhotoSet,
    PhotoRemoved,
    ContactRemoved,
}

impl PeopleAction {
    const fn text(self, dry_run: bool) -> &'static str {
        match (self, dry_run) {
            (Self::Linked, false) => "Linked the person to the meeting",
            (Self::Linked, true) => "Would link the person to the meeting",
            (Self::LinkConfirmed, false) => "Confirmed the meeting link",
            (Self::LinkConfirmed, true) => "Would confirm the meeting link",
            (Self::Unlinked, false) => "Took the person off the meeting",
            (Self::Unlinked, true) => "Would take the person off the meeting",
            (Self::Split, false) => "Split the person",
            (Self::Split, true) => "Would split the person",
            (Self::Deleted, false) => "Deleted the person",
            (Self::Deleted, true) => "Would delete the person",
            (Self::PhotoSet, false) => "Set the photo",
            (Self::PhotoSet, true) => "Would set the photo",
            (Self::PhotoRemoved, false) => "Took the photo off",
            (Self::PhotoRemoved, true) => "Would take the photo off",
            (Self::ContactRemoved, false) => "Took the contact off",
            (Self::ContactRemoved, true) => "Would take the contact off",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExternalPersonRef {
    /// Absent on a dry run that makes a new person.
    pub id: Option<String>,
    pub name: String,
}

/// One answer from a write on a person. The people store keeps no author,
/// so the change is the user's in Sona; the operation id is filed with the
/// agent's request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExternalPeopleChange {
    pub schema_version: u32,
    pub action: PeopleAction,
    pub dry_run: bool,
    pub operation_id: String,
    /// True when this answered from an earlier run with the same operation
    /// id, or the person already reads that way.
    pub replayed: bool,
    pub person_id: String,
    pub name: String,
    pub meeting_id: Option<String>,
    pub into: Option<ExternalPersonRef>,
    pub moved_meetings: Vec<String>,
    pub moved_aliases: Vec<String>,
    pub moved_emails: Vec<String>,
    pub moved_documents: Vec<String>,
    /// The people revision after the change; before it on a dry run.
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkChange {
    Add,
    Confirm,
    Remove,
}

enum PeopleWrite {
    Link(PersonLinkRequest, LinkChange),
    Split(PersonSplitRequest),
    Delete(PersonDeleteRequest),
    Photo(Box<PersonAvatarSetRequest>),
    Contact(PersonContactSetRequest),
}

/// The write a change makes, and the answer it gives before anything is
/// written.
pub(super) struct PeoplePlan {
    write: PeopleWrite,
    change: ExternalPeopleChange,
}

struct Found {
    id: PersonId,
    name: String,
    revision: u64,
}

async fn opened(
    meetings: &Arc<MeetingSessionManager>,
) -> Result<(Arc<MeetingStore>, KeptLocal), ExternalError> {
    let store = store(meetings).await.map_err(|_| conversations_closed())?;
    let kept = kept_local(&store)?;
    Ok((store, kept))
}

fn find(store: &MeetingStore, value: &str) -> Result<Found, ExternalError> {
    let list = store.people_list().map_err(QueryError::from)?;
    let choices: Vec<WorkPersonChoice> = list
        .entries
        .iter()
        .map(|entry| WorkPersonChoice {
            id: entry.person.id.uuid().to_string(),
            name: entry.person.display_name.clone(),
            aliases: entry.person.aliases.clone(),
        })
        .collect();
    let found = pick_person(value, &choices)
        .map_err(|refusal| refusal.hint("Look people up with: sona work people --query NAME"))?;
    let id = Uuid::parse_str(&found.id)
        .map(PersonId::from_uuid)
        .map_err(|_| ExternalError::new(ExternalErrorCode::Failed, "A person's id is unreadable."))?;
    Ok(Found {
        id,
        name: found.name.clone(),
        revision: list.revision,
    })
}

fn detail(store: &MeetingStore, person: &Found, hidden: &KeptLocal) -> Result<PersonDetail, ExternalError> {
    store
        .person_detail_without(person.id, hidden.sessions())
        .map(|result| result.detail)
        .map_err(|error| person_refusal(&person.name, error))
}

fn every_meeting(store: &MeetingStore, person: &Found) -> Result<PersonDetail, ExternalError> {
    store
        .person_detail(person.id)
        .map(|result| result.detail)
        .map_err(|error| person_refusal(&person.name, error))
}

fn person_refusal(name: &str, error: StoreError) -> ExternalError {
    match error {
        StoreError::NotFound | StoreError::PersonNotFound => ExternalError::new(
            ExternalErrorCode::NotFound,
            format!("{name} is no longer on the People page."),
        ),
        error => QueryError::from(error).into(),
    }
}

fn write_refusal(error: MeetingCommandError) -> ExternalError {
    match error {
        MeetingCommandError::StaleRevision => ExternalError::new(
            ExternalErrorCode::Failed,
            "The People page changed while this ran. Run it again.",
        ),
        MeetingCommandError::NotFound => ExternalError::new(
            ExternalErrorCode::NotFound,
            "That person or meeting is no longer there.",
        ),
        error => QueryError::from(error).into(),
    }
}

fn loop_row(open: &crate::meeting::people_types::PersonOpenLoop) -> ExternalPersonLoop {
    ExternalPersonLoop {
        loop_id: open.loop_id.as_str().to_owned(),
        meeting_id: open.meeting_id.uuid().to_string(),
        text: open.text.clone(),
        direction: open.direction,
        status: open.status,
        at_ms: open.at_utc_ms,
    }
}

pub(super) async fn person_page(
    meetings: &Arc<MeetingSessionManager>,
    value: &str,
) -> Result<ExternalPersonPage, ExternalError> {
    let (store, kept) = opened(meetings).await?;
    let person = find(&store, value)?;
    let page = detail(&store, &person, &kept)?;
    let summary_left_out = every_meeting(&store, &person)?
        .links
        .iter()
        .any(|link| kept.hides(link.meeting.id));
    let revision = store.people_list().map_err(QueryError::from)?.revision;
    let photo = store
        .person_avatar(person.id)
        .map_err(|error| person_refusal(&person.name, error))?
        .avatar
        .is_some();
    let profile = page.person;
    Ok(ExternalPersonPage {
        schema_version: QUERY_SCHEMA_VERSION,
        revision,
        id: profile.id.uuid().to_string(),
        name: profile.display_name,
        aliases: profile.aliases,
        role: profile.role,
        organization: profile.organization,
        emails: profile.calendar_emails,
        notes: profile.notes,
        summary: if summary_left_out {
            None
        } else {
            profile.summary.map(|summary| summary.text)
        },
        summary_left_out,
        meetings: page
            .links
            .iter()
            .map(|link| ExternalPersonMeeting {
                meeting_id: link.meeting.id.uuid().to_string(),
                title: link.meeting.title.clone(),
                at_ms: link.meeting.at_utc_ms,
                at: local_time(link.meeting.at_utc_ms),
                source: link.source,
                confirmed: link.confidence == PersonLinkConfidence::Confirmed,
            })
            .collect(),
        open_loops: page.open_loops.iter().map(loop_row).collect(),
        documents: page.documents,
        contact: page.contact,
        photo,
    })
}

pub(super) async fn companies(
    meetings: &Arc<MeetingSessionManager>,
) -> Result<ExternalCompanies, ExternalError> {
    let (store, kept) = opened(meetings).await?;
    Ok(ExternalCompanies {
        schema_version: QUERY_SCHEMA_VERSION,
        companies: store
            .companies_list_without(kept.sessions())
            .map_err(QueryError::from)?
            .companies,
    })
}

pub(super) async fn company(
    meetings: &Arc<MeetingSessionManager>,
    name: &str,
) -> Result<ExternalCompany, ExternalError> {
    let (store, kept) = opened(meetings).await?;
    let page = match store.organization_detail_without(name, kept.sessions()) {
        Ok(page) => page.detail,
        Err(StoreError::NotFound) => {
            return Err(ExternalError::new(
                ExternalErrorCode::NotFound,
                format!("No company is called {:?}.", name.trim()),
            )
            .hint("List them with: sona work companies"))
        }
        Err(error) => return Err(QueryError::from(error).into()),
    };
    Ok(ExternalCompany {
        schema_version: QUERY_SCHEMA_VERSION,
        slug: crate::meeting::people_types::organization_slug(&page.name),
        name: page.name,
        people: page
            .people
            .iter()
            .map(|entry| ExternalCompanyPerson {
                id: entry.person.id.uuid().to_string(),
                name: entry.person.display_name.clone(),
                role: entry.person.role.clone(),
                meetings: entry.meetings_count,
            })
            .collect(),
        meetings: page
            .recent_meetings
            .iter()
            .map(|meeting| ExternalCompanyMeeting {
                meeting_id: meeting.id.uuid().to_string(),
                title: meeting.title.clone(),
                at_ms: meeting.at_utc_ms,
                at: local_time(meeting.at_utc_ms),
            })
            .collect(),
        open_loops: page.open_loops.iter().map(loop_row).collect(),
    })
}

/// When the user first met the person, unless that meeting is kept on this
/// Mac. Reading it the first time offers the introducer it heard, as the
/// person page does.
pub(super) async fn first_met(
    meetings: &Arc<MeetingSessionManager>,
    value: &str,
) -> Result<ExternalFirstMet, ExternalError> {
    let (store, kept) = opened(meetings).await?;
    let person = find(&store, value)?;
    let mut answer = ExternalFirstMet {
        schema_version: QUERY_SCHEMA_VERSION,
        person_id: person.id.uuid().to_string(),
        name: person.name.clone(),
        left_out: false,
        meeting_id: None,
        at_ms: None,
        at: None,
        place: None,
        introduction: None,
        answered: None,
        dry_run: false,
        operation_id: None,
    };
    let earliest = every_meeting(&store, &person)?
        .links
        .into_iter()
        .filter(|link| link.confidence == PersonLinkConfidence::Confirmed)
        .min_by_key(|link| (link.meeting.at_utc_ms, link.meeting.id.uuid()));
    if earliest.is_some_and(|link| kept.hides(link.meeting.id)) {
        answer.left_out = true;
        return Ok(answer);
    }
    let Some(first) = store
        .person_first_met(person.id, now_ms())
        .map_err(|error| person_refusal(&person.name, error))?
    else {
        return Ok(answer);
    };
    if kept.hides(first.meeting_id) {
        answer.left_out = true;
        return Ok(answer);
    }
    answer.meeting_id = Some(first.meeting_id.uuid().to_string());
    answer.at_ms = Some(first.at_utc_ms);
    answer.at = Some(local_time(first.at_utc_ms));
    answer.place = first.place;
    answer.introduction = first.introduction.map(|introduction| ExternalIntroduction {
        suggestion_id: introduction.suggestion_id,
        introducer: introduction.introducer_name,
        quote: introduction.quote,
        confirmed: introduction.confirmed,
    });
    Ok(answer)
}

fn change(action: PeopleAction, person: &Found, operation_id: String, dry_run: bool) -> ExternalPeopleChange {
    ExternalPeopleChange {
        schema_version: QUERY_SCHEMA_VERSION,
        action,
        dry_run,
        operation_id,
        replayed: false,
        person_id: person.id.uuid().to_string(),
        name: person.name.clone(),
        meeting_id: None,
        into: None,
        moved_meetings: Vec::new(),
        moved_aliases: Vec::new(),
        moved_emails: Vec::new(),
        moved_documents: Vec::new(),
        revision: person.revision,
    }
}

pub(super) async fn plan_link(
    meetings: &Arc<MeetingSessionManager>,
    args: &PersonLinkArgs,
) -> Result<PeoplePlan, ExternalError> {
    let (store, kept) = opened(meetings).await?;
    let person = find(&store, &args.person)?;
    let session = session_id(&args.meeting)?;
    if kept.hides(session) {
        return Err(missing_meeting(session));
    }
    store
        .session_snapshot(session)
        .map_err(|error| crate::query::external::meeting_refusal(session, error))?;
    let kind = match (args.confirm, args.remove) {
        (true, _) => LinkChange::Confirm,
        (false, true) => LinkChange::Remove,
        (false, false) => LinkChange::Add,
    };
    let current = detail(&store, &person, &kept)?
        .links
        .into_iter()
        .find(|link| link.meeting.id == session)
        .map(|link| link.confidence);
    let already = match (kind, current) {
        (LinkChange::Confirm, None) => {
            return Err(ExternalError::invalid(format!(
                "{} is not on that meeting yet. Link them without --confirm.",
                person.name
            )))
        }
        (LinkChange::Add | LinkChange::Confirm, Some(confidence)) => {
            confidence == PersonLinkConfidence::Confirmed
        }
        (LinkChange::Add, None) => false,
        (LinkChange::Remove, current) => current.is_none(),
    };
    let action = match kind {
        LinkChange::Add => PeopleAction::Linked,
        LinkChange::Confirm => PeopleAction::LinkConfirmed,
        LinkChange::Remove => PeopleAction::Unlinked,
    };
    let operation_id = args.write.operation_id(
        "person-link",
        &(person.id.uuid(), session.uuid(), action, person.revision),
    )?;
    let mut answer = change(action, &person, operation_id, args.dry_run);
    answer.replayed = already;
    answer.meeting_id = Some(session.uuid().to_string());
    Ok(PeoplePlan {
        write: PeopleWrite::Link(
            PersonLinkRequest {
                meeting_id: session,
                person_id: person.id,
                expected_revision: person.revision,
            },
            kind,
        ),
        change: answer,
    })
}

fn picked(
    asked: &[String],
    held: &[String],
    what: &str,
    name: &str,
) -> Result<Vec<String>, ExternalError> {
    asked
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| {
            held.iter()
                .find(|kept| kept.eq_ignore_ascii_case(value))
                .cloned()
                .ok_or_else(|| ExternalError::invalid(format!("{name} has no {what} {value:?}.")))
        })
        .collect()
}

pub(super) async fn plan_split(
    meetings: &Arc<MeetingSessionManager>,
    args: &PersonSplitArgs,
) -> Result<PeoplePlan, ExternalError> {
    let (store, kept) = opened(meetings).await?;
    let person = find(&store, &args.person)?;
    let page = detail(&store, &person, &kept)?;
    let (target, into) = match (&args.into, &args.new) {
        (Some(into), _) => {
            let target = find(&store, into)?;
            if target.id == person.id {
                return Err(ExternalError::invalid("Split into a different person."));
            }
            (
                PersonSplitTarget::Existing { person_id: target.id },
                ExternalPersonRef {
                    id: Some(target.id.uuid().to_string()),
                    name: target.name,
                },
            )
        }
        (None, Some(name)) => (
            PersonSplitTarget::Create {
                display_name: name.trim().to_owned(),
            },
            ExternalPersonRef {
                id: None,
                name: name.trim().to_owned(),
            },
        ),
        (None, None) => return Err(ExternalError::invalid("Pass --into PERSON or --new NAME.")),
    };
    let mut meeting_ids = Vec::new();
    for value in args.meetings.iter().filter(|value| !value.trim().is_empty()) {
        let session = session_id(value)?;
        if kept.hides(session) {
            return Err(missing_meeting(session));
        }
        if !page.links.iter().any(|link| link.meeting.id == session) {
            return Err(ExternalError::invalid(format!(
                "{} is not on meeting {}.",
                person.name,
                session.uuid()
            )));
        }
        meeting_ids.push(session);
    }
    let aliases = picked(&args.aliases, &page.person.aliases, "other name", &person.name)?;
    let emails = picked(&args.emails, &page.person.calendar_emails, "email", &person.name)?;
    let mut document_ids = Vec::new();
    for value in args.documents.iter().filter(|value| !value.trim().is_empty()) {
        let document = Uuid::parse_str(value.trim())
            .map(DocumentId)
            .map_err(|_| ExternalError::invalid(format!("{value:?} is not a document id.")))?;
        if !page.documents.iter().any(|held| held.id == document) {
            return Err(ExternalError::invalid(format!(
                "{} has no document {value}.",
                person.name
            )));
        }
        document_ids.push(document);
    }
    if meeting_ids.is_empty() && aliases.is_empty() && emails.is_empty() && document_ids.is_empty() {
        return Err(ExternalError::invalid(
            "Pick what moves: --meetings, --aliases, --emails or --documents.",
        ));
    }
    let operation_id = args.write.operation_id(
        "person-split",
        &(
            person.id.uuid(),
            &into,
            meeting_ids.iter().map(|id| id.uuid()).collect::<Vec<_>>(),
            &aliases,
            &emails,
            document_ids.iter().map(|id| id.uuid()).collect::<Vec<_>>(),
            person.revision,
        ),
    )?;
    let mut answer = change(PeopleAction::Split, &person, operation_id, args.dry_run);
    answer.into = Some(into);
    answer.moved_meetings = meeting_ids.iter().map(|id| id.uuid().to_string()).collect();
    answer.moved_aliases.clone_from(&aliases);
    answer.moved_emails.clone_from(&emails);
    answer.moved_documents = document_ids.iter().map(|id| id.uuid().to_string()).collect();
    Ok(PeoplePlan {
        write: PeopleWrite::Split(PersonSplitRequest {
            source_person_id: person.id,
            target,
            meeting_ids,
            aliases,
            calendar_emails: emails,
            document_ids,
            expected_revision: person.revision,
        }),
        change: answer,
    })
}

/// Deleting a person also changes every meeting they are in, so a person in
/// a meeting kept on this Mac is deleted only in Sona.
pub(super) async fn plan_delete(
    meetings: &Arc<MeetingSessionManager>,
    args: &PersonDeleteArgs,
) -> Result<PeoplePlan, ExternalError> {
    let (store, kept) = opened(meetings).await?;
    let person = find(&store, &args.person)?;
    if every_meeting(&store, &person)?
        .links
        .iter()
        .any(|link| kept.hides(link.meeting.id))
    {
        return Err(ExternalError::invalid(format!(
            "{} is in a meeting kept on this Mac. Delete them on Sona's People page.",
            person.name
        )));
    }
    let operation_id = args
        .write
        .operation_id("person-delete", &(person.id.uuid(), person.revision))?;
    Ok(PeoplePlan {
        write: PeopleWrite::Delete(PersonDeleteRequest {
            person_id: person.id,
            expected_revision: person.revision,
        }),
        change: change(PeopleAction::Deleted, &person, operation_id, args.dry_run),
    })
}

/// The photo as the store keeps it: the middle square of the picture, at
/// most 512 pixels across, as PNG.
fn portrait(bytes: &[u8]) -> Result<Vec<u8>, ExternalError> {
    let unreadable = || ExternalError::invalid("The photo must be a readable PNG or JPEG.");
    let format = image::guess_format(bytes).map_err(|_| unreadable())?;
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg) {
        return Err(unreadable());
    }
    let picture = image::load_from_memory_with_format(bytes, format).map_err(|_| unreadable())?;
    let edge = picture.width().min(picture.height());
    if edge == 0 {
        return Err(unreadable());
    }
    let square = picture.crop_imm(
        (picture.width() - edge) / 2,
        (picture.height() - edge) / 2,
        edge,
        edge,
    );
    let square = if edge > PHOTO_EDGE {
        square.resize_exact(PHOTO_EDGE, PHOTO_EDGE, FilterType::Lanczos3)
    } else {
        square
    };
    let mut png = Cursor::new(Vec::new());
    square
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|_| unreadable())?;
    Ok(png.into_inner())
}

pub(super) async fn plan_photo(
    meetings: &Arc<MeetingSessionManager>,
    args: &PersonPhotoArgs,
    now_ms: i64,
) -> Result<PeoplePlan, ExternalError> {
    let (store, _) = opened(meetings).await?;
    let person = find(&store, &args.person)?;
    let current = meetings
        .person_avatar(person.id)
        .await
        .map_err(write_refusal)?
        .avatar
        .map(|avatar| avatar.png_base64);
    let (request, action, already, key) = match &args.file {
        Some(path) => {
            let size = std::fs::metadata(path)
                .map_err(|_| ExternalError::invalid(format!("{} cannot be read.", path.display())))?
                .len();
            if size > PHOTO_MAX_BYTES {
                return Err(ExternalError::invalid("The photo must be 10 MB or smaller."));
            }
            let original = std::fs::read(path)
                .map_err(|_| ExternalError::invalid(format!("{} cannot be read.", path.display())))?;
            let png = STANDARD.encode(portrait(&original)?);
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("photo")
                .to_owned();
            let already = current.as_deref() == Some(png.as_str());
            let key = Uuid::new_v5(&Uuid::NAMESPACE_OID, png.as_bytes());
            (
                PersonAvatarSetRequest {
                    person_id: person.id,
                    expected_revision: person.revision,
                    png_base64: Some(png),
                    provenance: Some(PersonPhotoProvenance::LocalFile {
                        file_name,
                        confirmed_at_utc_ms: now_ms,
                    }),
                    original_base64: Some(STANDARD.encode(&original)),
                },
                PeopleAction::PhotoSet,
                already,
                Some(key),
            )
        }
        None => (
            PersonAvatarSetRequest {
                person_id: person.id,
                expected_revision: person.revision,
                png_base64: None,
                provenance: None,
                original_base64: None,
            },
            PeopleAction::PhotoRemoved,
            current.is_none(),
            None,
        ),
    };
    let operation_id = args
        .write
        .operation_id("person-photo", &(person.id.uuid(), key, person.revision))?;
    let mut answer = change(action, &person, operation_id, args.dry_run);
    answer.replayed = already;
    Ok(PeoplePlan {
        write: PeopleWrite::Photo(Box::new(request)),
        change: answer,
    })
}

pub(super) async fn plan_contact_remove(
    meetings: &Arc<MeetingSessionManager>,
    args: &PersonContactRemoveArgs,
) -> Result<PeoplePlan, ExternalError> {
    let (store, kept) = opened(meetings).await?;
    let person = find(&store, &args.person)?;
    let attached = detail(&store, &person, &kept)?.contact.is_some();
    let operation_id = args
        .write
        .operation_id("person-contact-remove", &(person.id.uuid(), person.revision))?;
    let mut answer = change(PeopleAction::ContactRemoved, &person, operation_id, args.dry_run);
    answer.replayed = !attached;
    Ok(PeoplePlan {
        write: PeopleWrite::Contact(PersonContactSetRequest {
            person_id: person.id,
            expected_revision: person.revision,
            contact: None,
        }),
        change: answer,
    })
}

impl PeoplePlan {
    /// Whether the change has nothing to write: a dry run, or a person who
    /// already reads that way.
    pub(super) const fn settled(&self) -> bool {
        self.change.dry_run || self.change.replayed
    }

    pub(super) fn answer(self) -> ExternalPeopleChange {
        self.change
    }

    /// Makes the change through the same step the People page takes.
    pub(super) async fn apply(
        self,
        meetings: &Arc<MeetingSessionManager>,
    ) -> Result<ExternalPeopleChange, ExternalError> {
        let Self { write, mut change } = self;
        change.revision = match write {
            PeopleWrite::Link(request, LinkChange::Add) => meetings.link_add_manual(request).await,
            PeopleWrite::Link(request, LinkChange::Confirm) => meetings.link_confirm(request).await,
            PeopleWrite::Link(request, LinkChange::Remove) => meetings.link_remove(request).await,
            PeopleWrite::Split(request) => meetings.person_split(request).await.map(|result| {
                if let (Some(into), Some(person)) = (change.into.as_mut(), result.person.as_ref()) {
                    into.id = Some(person.id.uuid().to_string());
                    into.name.clone_from(&person.display_name);
                }
                result
            }),
            PeopleWrite::Delete(request) => meetings.person_delete(request).await,
            PeopleWrite::Contact(request) => meetings.person_contact_set(request).await,
            PeopleWrite::Photo(request) => {
                change.revision = meetings
                    .person_avatar_set(*request)
                    .await
                    .map_err(write_refusal)?
                    .revision;
                return Ok(change);
            }
        }
        .map_err(write_refusal)?
        .revision;
        Ok(change)
    }
}

impl Tracker<'_> {
    pub(super) async fn person_page(&self, args: &PersonArgs) -> Result<WorkAnswer, ExternalError> {
        Ok(WorkAnswer::PersonPage(Box::new(
            person_page(self.meetings, &args.person).await?,
        )))
    }

    pub(super) async fn companies(&self, _: &CompaniesArgs) -> Result<WorkAnswer, ExternalError> {
        Ok(WorkAnswer::Companies(companies(self.meetings).await?))
    }

    pub(super) async fn company(&self, args: &CompanyArgs) -> Result<WorkAnswer, ExternalError> {
        Ok(WorkAnswer::Company(Box::new(
            company(self.meetings, &args.name).await?,
        )))
    }

    pub(super) async fn first_met(&self, args: &PersonArgs) -> Result<WorkAnswer, ExternalError> {
        Ok(WorkAnswer::FirstMet(Box::new(
            first_met(self.meetings, &args.person).await?,
        )))
    }

    pub(super) async fn first_met_answer(
        &self,
        args: &FirstMetArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let mut answer = first_met(self.meetings, &args.person).await?;
        let accept = args.answer == FirstMetChoice::Yes;
        let Some(introduction) = answer.introduction.as_ref() else {
            return Err(ExternalError::invalid(format!(
                "Sona heard nobody introduce {}, so there is nothing to answer.",
                answer.name
            )));
        };
        if introduction.confirmed {
            return Err(ExternalError::invalid(format!(
                "{} is already named as who introduced {}.",
                introduction.introducer, answer.name
            )));
        }
        let suggestion = introduction.suggestion_id.clone();
        let operation_id = args
            .write
            .operation_id("first-met-answer", &(&answer.person_id, &suggestion, accept))?;
        answer.operation_id = Some(operation_id.clone());
        answer.answered = Some(if accept {
            FirstMetAnswer::Took
        } else {
            FirstMetAnswer::TurnedDown
        });
        if args.dry_run {
            answer.dry_run = true;
            return Ok(WorkAnswer::FirstMet(Box::new(answer)));
        }
        self.applies("answering who introduced a person")?;
        self.meetings
            .decide_filing(self.app, &suggestion, accept, None)
            .await
            .map_err(ExternalError::invalid)?;
        let mut after = first_met(self.meetings, &args.person).await?;
        after.answered = answer.answered;
        after.operation_id = Some(operation_id);
        Ok(WorkAnswer::FirstMet(Box::new(after)))
    }

    pub(super) async fn person_link(&self, args: &PersonLinkArgs) -> Result<WorkAnswer, ExternalError> {
        let key = format!("{}\0{}\0{}\0{}", args.person, args.meeting, args.confirm, args.remove);
        self.people_write("person-link", "linking people to meetings", &args.write, args.dry_run, &key, || {
            plan_link(self.meetings, args)
        })
        .await
    }

    pub(super) async fn person_split(&self, args: &PersonSplitArgs) -> Result<WorkAnswer, ExternalError> {
        let key = format!("{args:?}");
        self.people_write("person-split", "splitting a person", &args.write, args.dry_run, &key, || {
            plan_split(self.meetings, args)
        })
        .await
    }

    pub(super) async fn person_delete(&self, args: &PersonDeleteArgs) -> Result<WorkAnswer, ExternalError> {
        self.people_write("person-delete", "deleting a person", &args.write, args.dry_run, &args.person, || {
            plan_delete(self.meetings, args)
        })
        .await
    }

    pub(super) async fn person_photo(&self, args: &PersonPhotoArgs) -> Result<WorkAnswer, ExternalError> {
        let key = format!("{}\0{:?}\0{}", args.person, args.file, args.remove);
        self.people_write("person-photo", "changing a person's photo", &args.write, args.dry_run, &key, || {
            plan_photo(self.meetings, args, now_ms())
        })
        .await
    }

    pub(super) async fn person_contact_remove(
        &self,
        args: &PersonContactRemoveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        self.people_write(
            "person-contact-remove",
            "taking a contact off a person",
            &args.write,
            args.dry_run,
            &args.person,
            || plan_contact_remove(self.meetings, args),
        )
        .await
    }

    /// Answers a write from an earlier run with the caller's operation id, or
    /// plans it, makes it, and files the answer under that id.
    async fn people_write<F, P>(
        &self,
        command: &str,
        what: &str,
        write: &WriteArgs,
        dry_run: bool,
        key: &str,
        plan: P,
    ) -> Result<WorkAnswer, ExternalError>
    where
        P: FnOnce() -> F,
        F: Future<Output = Result<PeoplePlan, ExternalError>>,
    {
        if let (Some(operation_id), false) = (write.operation_id, dry_run) {
            if let Some(mut earlier) = self.people_receipt(&operation_id.to_string(), command, key)? {
                earlier.replayed = true;
                return Ok(WorkAnswer::PeopleChange(Box::new(earlier)));
            }
        }
        let plan = plan().await?;
        if plan.settled() {
            return Ok(WorkAnswer::PeopleChange(Box::new(plan.answer())));
        }
        self.applies(what)?;
        let change = plan.apply(self.meetings).await?;
        let response = serde_json::to_string(&change).map_err(|_| {
            ExternalError::new(ExternalErrorCode::Failed, "The change could not be filed.")
        })?;
        planning::history(self.history, |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO planning_operations (operation_id, command, request_json, response_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![change.operation_id, command, key, response, now_ms()],
            )?;
            Ok(())
        })
        .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::PeopleChange(Box::new(change)))
    }

    fn people_receipt(
        &self,
        operation_id: &str,
        command: &str,
        key: &str,
    ) -> Result<Option<ExternalPeopleChange>, ExternalError> {
        let found: Option<(String, String, String)> = planning::history(self.history, |conn| {
            Ok(conn
                .query_row(
                    "SELECT command, request_json, response_json FROM planning_operations WHERE operation_id = ?1",
                    [operation_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?)
        })
        .map_err(|message| refusal(self.history, message))?;
        match found {
            None => Ok(None),
            Some((stored, asked, response)) if stored == command && asked == key => {
                serde_json::from_str(&response).map(Some).map_err(|_| {
                    ExternalError::new(ExternalErrorCode::Failed, "The earlier run's answer is unreadable.")
                })
            }
            Some(_) => Err(ExternalError::invalid(format!(
                "Operation id {operation_id} was already used for a different change. Pass a new --operation-id, or leave it out."
            ))),
        }
    }
}

fn meeting_line(text: &mut String, title: &str, at: &str, id: &str) {
    let _ = writeln!(text, "  {at}  {title}  {id}");
}

fn loops_text(open_loops: &[ExternalPersonLoop], text: &mut String) {
    if open_loops.is_empty() {
        return;
    }
    text.push_str("Open:\n");
    for open in open_loops {
        let _ = writeln!(
            text,
            "  [{}] {}  ({})",
            crate::query::external::label(&open.direction),
            open.text,
            open.loop_id
        );
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::PersonPage(page) => {
            let _ = writeln!(text, "{}  {}", page.name, page.id);
            let about = [page.role.as_deref(), page.organization.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            if !about.is_empty() {
                let _ = writeln!(text, "  {}", about.join(", "));
            }
            if !page.aliases.is_empty() {
                let _ = writeln!(text, "  Also: {}", page.aliases.join(", "));
            }
            if !page.emails.is_empty() {
                let _ = writeln!(text, "  Email: {}", page.emails.join(", "));
            }
            if let Some(notes) = &page.notes {
                let _ = writeln!(text, "  Notes: {notes}");
            }
            if page.photo {
                text.push_str("  Photo: set\n");
            }
            match (&page.summary, page.summary_left_out) {
                (_, true) => text.push_str(
                    "Summary: left out, since one of their meetings is kept on this Mac.\n",
                ),
                (Some(summary), false) => {
                    let _ = writeln!(text, "Summary: {summary}");
                }
                (None, false) => {}
            }
            if !page.meetings.is_empty() {
                text.push_str("Meetings:\n");
                for meeting in &page.meetings {
                    let _ = writeln!(
                        text,
                        "  {}  {}{}  {}",
                        meeting.at,
                        meeting.title,
                        if meeting.confirmed { "" } else { " (suggested)" },
                        meeting.meeting_id
                    );
                }
            }
            loops_text(&page.open_loops, text);
            if !page.documents.is_empty() {
                text.push_str("Documents:\n");
                for document in &page.documents {
                    let _ = writeln!(text, "  {}  {}", document.title, document.id.uuid());
                }
            }
            if let Some(contact) = &page.contact {
                let _ = writeln!(text, "Contact: {}", contact.name);
                for value in contact.emails.iter().chain(&contact.phones) {
                    let _ = writeln!(text, "  {}", value.value);
                }
            }
        }
        WorkAnswer::Companies(list) => {
            if list.companies.is_empty() {
                text.push_str("No companies yet. Set one with: sona work person-save NAME --org COMPANY\n");
            }
            for company in &list.companies {
                let _ = writeln!(
                    text,
                    "{}  {} people, {} meetings, {} open  ({})",
                    company.name,
                    company.people_count,
                    company.meetings_count,
                    company.open_loops_count,
                    company.slug
                );
            }
        }
        WorkAnswer::Company(company) => {
            let _ = writeln!(text, "{}  ({})", company.name, company.slug);
            text.push_str("People:\n");
            for person in &company.people {
                let _ = writeln!(
                    text,
                    "  {}{}  {}",
                    person.name,
                    person.role.as_deref().map(|role| format!(", {role}")).unwrap_or_default(),
                    person.id
                );
            }
            if !company.meetings.is_empty() {
                text.push_str("Meetings:\n");
                for meeting in &company.meetings {
                    meeting_line(text, &meeting.title, &meeting.at, &meeting.meeting_id);
                }
            }
            loops_text(&company.open_loops, text);
        }
        WorkAnswer::FirstMet(first) => {
            if first.left_out {
                let _ = writeln!(
                    text,
                    "When you first met {} is left out: that meeting is kept on this Mac.",
                    first.name
                );
                return;
            }
            let (Some(at), Some(meeting)) = (&first.at, &first.meeting_id) else {
                let _ = writeln!(text, "No confirmed meeting with {} yet.", first.name);
                return;
            };
            let _ = writeln!(text, "First met {} {at}  {meeting}", first.name);
            if let Some(place) = &first.place {
                let _ = writeln!(text, "  at {place}");
            }
            if let Some(introduction) = &first.introduction {
                let _ = writeln!(
                    text,
                    "  Introduced by {}{}: \"{}\"",
                    introduction.introducer,
                    if introduction.confirmed { "" } else { " (waiting for yes or no)" },
                    introduction.quote
                );
            }
            match (first.answered, first.dry_run) {
                (Some(FirstMetAnswer::Took), true) => text.push_str("Would take the introducer.\n"),
                (Some(FirstMetAnswer::TurnedDown), true) => {
                    text.push_str("Would turn the introducer down.\n");
                }
                (Some(FirstMetAnswer::Took), false) => text.push_str("Took the introducer.\n"),
                (Some(FirstMetAnswer::TurnedDown), false) => {
                    text.push_str("Turned the introducer down.\n");
                }
                (None, _) => {}
            }
        }
        WorkAnswer::PeopleChange(change) => {
            let _ = writeln!(
                text,
                "{}{}",
                change.action.text(change.dry_run),
                if change.replayed && !change.dry_run { " (already so)" } else { "" }
            );
            let _ = writeln!(text, "  {}  {}", change.name, change.person_id);
            if let Some(meeting) = &change.meeting_id {
                let _ = writeln!(text, "  meeting {meeting}");
            }
            if let Some(into) = &change.into {
                let _ = writeln!(
                    text,
                    "  into {}{}",
                    into.name,
                    into.id.as_deref().map(|id| format!("  {id}")).unwrap_or_default()
                );
            }
            for (what, moved) in [
                ("meetings", &change.moved_meetings),
                ("other names", &change.moved_aliases),
                ("emails", &change.moved_emails),
                ("documents", &change.moved_documents),
            ] {
                if !moved.is_empty() {
                    let _ = writeln!(text, "  {what}: {}", moved.join(", "));
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        companies, company, first_met, person_page, plan_delete, plan_link, plan_split,
        PeopleAction, PersonDeleteArgs, PersonLinkArgs, PersonSplitArgs,
    };
    use super::super::catalog::merge_people;
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::people_types::PersonId;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::session::{MeetingSessionManager, NoCaptureSources};
    use crate::meeting::store::workflow_core_tests::{link, person, recorded_meeting};
    use crate::meeting::store::{MeetingStore, PersonProfileEdit};
    use crate::meeting::types::{MeetingOperationId, MeetingSessionId};
    use crate::query::external::tracker::WriteArgs;
    use crate::query::external::ExternalErrorCode;
    use crate::secrets::{MemorySecretBackend, SecretManager};
    use std::sync::Arc;
    use tempfile::TempDir;

    const NOW: i64 = 1_767_000_000_000;
    const DAY: i64 = 24 * 60 * 60 * 1_000;

    struct Corpus {
        _directory: TempDir,
        meetings: Arc<MeetingSessionManager>,
        store: Arc<MeetingStore>,
        open: MeetingSessionId,
        kept: MeetingSessionId,
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
        let open = recorded_meeting(&store, "Pricing sync", NOW);
        let kept = recorded_meeting(&store, "Board prep", NOW - DAY);
        let dana = person(&store, "Dana Reyes", &[], &[]);
        meetings
            .person_profile_update(
                dana,
                PersonProfileEdit {
                    display_name: None,
                    aliases: None,
                    role: None,
                    organization: Some("Acme".to_owned()),
                    calendar_emails: None,
                    notes: None,
                },
            )
            .await
            .unwrap();
        link(&store, open, dana, "manual", "confirmed");
        link(&store, kept, dana, "manual", "confirmed");
        Corpus {
            _directory: directory,
            meetings,
            store,
            open,
            kept,
            dana,
        }
    }

    fn keep_on_this_mac(corpus: &Corpus) {
        corpus
            .store
            .remember_calendar_facts(
                corpus.kept,
                &CalendarEventSummary {
                    event_key: format!("board#{}", NOW - DAY),
                    series_key: "board".to_owned(),
                    title: "Board prep".to_owned(),
                    attendee_count: 2,
                    start_utc_ms: NOW - DAY,
                    end_utc_ms: NOW - DAY + 30 * 60 * 1_000,
                    attendees: Vec::new(),
                    notes: None,
                    calendar_name: None,
                    location: None,
                    url: None,
                    place: None,
                },
            )
            .unwrap();
        corpus
            .store
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
    }

    fn organizer() -> WriteArgs {
        WriteArgs {
            operation_id: None,
            agent: Some("Organizer".to_owned()),
        }
    }

    fn linking(person: &str, meeting: MeetingSessionId, confirm: bool, remove: bool) -> PersonLinkArgs {
        PersonLinkArgs {
            person: person.to_owned(),
            meeting: meeting.uuid().to_string(),
            confirm,
            remove,
            dry_run: false,
            write: organizer(),
        }
    }

    fn page_meetings(page: &super::ExternalPersonPage) -> Vec<(String, bool)> {
        page.meetings
            .iter()
            .map(|meeting| (meeting.meeting_id.clone(), meeting.confirmed))
            .collect()
    }

    #[test]
    fn a_person_page_leaves_out_a_meeting_kept_on_this_mac_and_the_summary() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let before = person_page(&corpus.meetings, "Dana Reyes").await.unwrap();
            assert_eq!(before.meetings.len(), 2);
            assert!(!before.summary_left_out);

            keep_on_this_mac(&corpus);
            let after = person_page(&corpus.meetings, "dana reyes").await.unwrap();
            assert_eq!(
                page_meetings(&after),
                vec![(corpus.open.uuid().to_string(), true)]
            );
            assert!(after.summary_left_out);
            assert_eq!(after.summary, None);
            assert_eq!(after.organization.as_deref(), Some("Acme"));
        });
    }

    #[test]
    fn companies_leave_out_meetings_kept_on_this_mac() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let listed = companies(&corpus.meetings).await.unwrap().companies;
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].meetings_count, 2);

            keep_on_this_mac(&corpus);
            let listed = companies(&corpus.meetings).await.unwrap().companies;
            assert_eq!(listed[0].meetings_count, 1);
            let acme = company(&corpus.meetings, &listed[0].slug).await.unwrap();
            assert_eq!(acme.people.len(), 1);
            assert_eq!(
                acme.meetings
                    .iter()
                    .map(|meeting| meeting.meeting_id.clone())
                    .collect::<Vec<_>>(),
                vec![corpus.open.uuid().to_string()]
            );
        });
    }

    #[test]
    fn person_link_adds_confirms_and_removes_a_meeting_and_a_dry_run_writes_nothing() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let lee = person(&corpus.store, "Lee Park", &[], &[]);

            let mut dry = linking("Lee Park", corpus.open, false, false);
            dry.dry_run = true;
            let planned = plan_link(&corpus.meetings, &dry).await.unwrap();
            assert!(planned.settled());
            assert_eq!(planned.answer().action, PeopleAction::Linked);
            let page = person_page(&corpus.meetings, "Lee Park").await.unwrap();
            assert!(page.meetings.is_empty());

            let linked = plan_link(&corpus.meetings, &linking("Lee Park", corpus.open, false, false))
                .await
                .unwrap()
                .apply(&corpus.meetings)
                .await
                .unwrap();
            assert!(!linked.replayed);
            let page = person_page(&corpus.meetings, "Lee Park").await.unwrap();
            assert_eq!(
                page_meetings(&page),
                vec![(corpus.open.uuid().to_string(), true)]
            );

            let again = plan_link(&corpus.meetings, &linking("Lee Park", corpus.open, false, false))
                .await
                .unwrap();
            assert!(again.settled());

            plan_link(&corpus.meetings, &linking("Lee Park", corpus.open, false, true))
                .await
                .unwrap()
                .apply(&corpus.meetings)
                .await
                .unwrap();
            let page = person_page(&corpus.meetings, "Lee Park").await.unwrap();
            assert!(page.meetings.is_empty());

            link(&corpus.store, corpus.kept, lee, "calendar", "suggested");
            let page = person_page(&corpus.meetings, "Lee Park").await.unwrap();
            assert_eq!(
                page_meetings(&page),
                vec![(corpus.kept.uuid().to_string(), false)]
            );
            plan_link(&corpus.meetings, &linking("Lee Park", corpus.kept, true, false))
                .await
                .unwrap()
                .apply(&corpus.meetings)
                .await
                .unwrap();
            let page = person_page(&corpus.meetings, "Lee Park").await.unwrap();
            assert_eq!(
                page_meetings(&page),
                vec![(corpus.kept.uuid().to_string(), true)]
            );
        });
    }

    #[test]
    fn person_link_refuses_a_meeting_kept_on_this_mac() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            person(&corpus.store, "Lee Park", &[], &[]);
            keep_on_this_mac(&corpus);
            let refused = plan_link(&corpus.meetings, &linking("Lee Park", corpus.kept, false, false))
                .await
                .err()
                .unwrap();
            assert_eq!(refused.error, ExternalErrorCode::NotFound);
        });
    }

    #[test]
    fn person_split_moves_a_meeting_to_a_new_person() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let split = plan_split(
                &corpus.meetings,
                &PersonSplitArgs {
                    person: "Dana Reyes".to_owned(),
                    into: None,
                    new: Some("Dana Ortiz".to_owned()),
                    meetings: vec![corpus.kept.uuid().to_string()],
                    aliases: Vec::new(),
                    emails: Vec::new(),
                    documents: Vec::new(),
                    dry_run: false,
                    write: organizer(),
                },
            )
            .await
            .unwrap()
            .apply(&corpus.meetings)
            .await
            .unwrap();
            assert_eq!(split.action, PeopleAction::Split);
            let dana = person_page(&corpus.meetings, "Dana Reyes").await.unwrap();
            assert_eq!(dana.id, corpus.dana.uuid().to_string());
            assert_eq!(
                page_meetings(&dana),
                vec![(corpus.open.uuid().to_string(), true)]
            );
            let ortiz = person_page(&corpus.meetings, "Dana Ortiz").await.unwrap();
            assert_eq!(split.into.and_then(|into| into.id), Some(ortiz.id.clone()));
            assert_eq!(
                ortiz.meetings
                    .iter()
                    .map(|meeting| meeting.meeting_id.clone())
                    .collect::<Vec<_>>(),
                vec![corpus.kept.uuid().to_string()]
            );
        });
    }

    #[test]
    fn person_delete_leaves_a_person_in_a_meeting_kept_on_this_mac_alone() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            keep_on_this_mac(&corpus);
            let deleting = |name: &str| PersonDeleteArgs {
                person: name.to_owned(),
                dry_run: false,
                write: organizer(),
            };
            let refused = plan_delete(&corpus.meetings, &deleting("Dana Reyes"))
                .await
                .err()
                .unwrap();
            assert_eq!(refused.error, ExternalErrorCode::InvalidRequest);
            assert!(person_page(&corpus.meetings, "Dana Reyes").await.is_ok());

            person(&corpus.store, "Lee Park", &[], &[]);
            let deleted = plan_delete(&corpus.meetings, &deleting("Lee Park"))
                .await
                .unwrap()
                .apply(&corpus.meetings)
                .await
                .unwrap();
            assert_eq!(deleted.action, PeopleAction::Deleted);
            assert!(!corpus
                .store
                .people_list()
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.person.display_name == "Lee Park"));
        });
    }

    #[test]
    fn first_met_is_left_out_when_the_first_meeting_is_kept_on_this_mac() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let before = first_met(&corpus.meetings, "Dana Reyes").await.unwrap();
            assert!(!before.left_out);
            assert_eq!(before.meeting_id, Some(corpus.kept.uuid().to_string()));

            keep_on_this_mac(&corpus);
            let after = first_met(&corpus.meetings, "Dana Reyes").await.unwrap();
            assert!(after.left_out);
            assert_eq!(after.meeting_id, None);
            assert_eq!(after.at_ms, None);
        });
    }

    #[test]
    fn a_merge_dry_run_shows_the_merged_person_and_writes_nothing() {
        tauri::async_runtime::block_on(async {
            let corpus = corpus().await;
            let walk = recorded_meeting(&corpus.store, "Harbor walk", NOW - 2 * DAY);
            let lee = person(&corpus.store, "Lee Park", &["Lee"], &["lee@acme.invalid"]);
            link(&corpus.store, corpus.open, lee, "manual", "confirmed");
            link(&corpus.store, walk, lee, "manual", "confirmed");
            let before = corpus.store.people_list().unwrap();

            let preview = merge_people(&corpus.meetings, lee, corpus.dana, None, true)
                .await
                .unwrap();
            assert_eq!(preview.person.aliases, ["Lee Park", "Lee"]);
            assert_eq!(preview.person.calendar_emails, ["lee@acme.invalid"]);
            assert_eq!(preview.meetings_count, 3);
            assert_eq!(corpus.store.people_list().unwrap(), before);

            let merged = merge_people(&corpus.meetings, lee, corpus.dana, None, false)
                .await
                .unwrap();
            assert_eq!(merged.person.aliases, preview.person.aliases);
            assert_eq!(merged.person.calendar_emails, preview.person.calendar_emails);
            assert_eq!(merged.meetings_count, preview.meetings_count);
            assert!(!corpus
                .store
                .people_list()
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.person.id == lee));
        });
    }
}
