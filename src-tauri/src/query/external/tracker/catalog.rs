//! `sona work` verbs for the Projects and People pages, and the read-only
//! meetings feed: what an organizer agent needs to keep the tracker tidy.

use super::{
    conversations_closed, day_or_none, day_text, now_ms, pick, Board, DayArg, Priority, Tracker,
    WorkAnswer, WorkPersonChoice, WriteArgs,
};
use crate::meeting::people_types::{
    PersonId, PersonListEntry, PersonMergeRequest, VoiceProfileMergeResolution,
};
use crate::meeting::session::{map_store_error, MeetingSessionManager};
use crate::meeting::store::PersonProfileEdit;
use crate::meeting::types::{MeetingCommandError, MeetingPhase, ProcessingStatus};
use crate::planning;
use crate::planning::catalog::{LinkEdit, MilestoneEdit, ProjectEdit};
use crate::planning::store as plans;
use crate::planning::work;
use crate::planning::work::types::{WorkOwner, WorkStatus};
use crate::planning::{
    DateResolution, Milestone, Project, ProjectLink, ProjectStatus, TaskStatus,
};
use crate::planning::ProjectStatusName;
use crate::query::external::{
    meeting_detail, meeting_summaries, session_id, store, transcript, ExternalError,
    ExternalErrorCode, ExternalMeetingDetail, ExternalTranscript,
};
use crate::query::{QueryError, QUERY_SCHEMA_VERSION};
use chrono::NaiveDate;
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::fmt::Write as _;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ProjectState {
    Backlog,
    Planned,
    #[value(alias = "started")]
    Active,
    Paused,
    Completed,
    Canceled,
}

impl From<ProjectState> for ProjectStatus {
    fn from(state: ProjectState) -> Self {
        match state {
            ProjectState::Backlog => Self::Backlog,
            ProjectState::Planned => Self::Planned,
            ProjectState::Active => Self::Active,
            ProjectState::Paused => Self::Paused,
            ProjectState::Completed => Self::Completed,
            ProjectState::Canceled => Self::Canceled,
        }
    }
}

/// How exactly a project's start or target day is meant, or none to clear it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ResolutionArg {
    Day,
    Month,
    Quarter,
    #[value(alias = "halfYear")]
    HalfYear,
    Year,
    None,
}

impl ResolutionArg {
    const fn resolution(self) -> Option<DateResolution> {
        match self {
            Self::Day => Some(DateResolution::Day),
            Self::Month => Some(DateResolution::Month),
            Self::Quarter => Some(DateResolution::Quarter),
            Self::HalfYear => Some(DateResolution::HalfYear),
            Self::Year => Some(DateResolution::Year),
            Self::None => None,
        }
    }
}

/// A link to keep on a project: `TITLE=URL`, a bare URL, or none.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkArg {
    Clear,
    Link(LinkEdit),
}

pub fn link_arg(value: &str) -> Result<LinkArg, String> {
    let value = value.trim();
    if cleared(value) {
        return Ok(LinkArg::Clear);
    }
    let bare = ["http://", "https://", "mailto:", "sona://"]
        .iter()
        .any(|scheme| value.to_lowercase().starts_with(scheme));
    let (title, url) = match value.split_once('=') {
        Some((title, url)) if !bare => (Some(title.trim().to_owned()), url.trim().to_owned()),
        _ => (None, value.to_owned()),
    };
    if url.is_empty() {
        return Err("expected TITLE=URL, a URL, or none".to_owned());
    }
    Ok(LinkArg::Link(LinkEdit {
        title: title.filter(|title| !title.is_empty()),
        url,
    }))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum MilestoneState {
    NotStarted,
    InProgress,
    Done,
    Canceled,
}

impl From<MilestoneState> for TaskStatus {
    fn from(state: MilestoneState) -> Self {
        match state {
            MilestoneState::NotStarted => Self::NotStarted,
            MilestoneState::InProgress => Self::InProgress,
            MilestoneState::Done => Self::Done,
            MilestoneState::Canceled => Self::Canceled,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SinceArg {
    Ms(i64),
    Day(NaiveDate),
}

pub fn since(value: &str) -> Result<SinceArg, String> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value
            .parse()
            .map(SinceArg::Ms)
            .map_err(|_| "that many milliseconds is out of range".to_owned());
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map(SinceArg::Day)
        .map_err(|_| "expected milliseconds since the epoch or YYYY-MM-DD".to_owned())
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectsArgs {
    /// Include projects that were put away.
    #[arg(long)]
    pub all: bool,
    /// Only the projects in the bin, deleted less than 30 days ago.
    #[arg(long)]
    pub trashed: bool,
    /// Only projects whose latest update is from before this day
    /// (YYYY-MM-DD), and those with no update yet.
    #[arg(long, value_parser = calendar_day, value_name = "DAY")]
    pub update_before: Option<NaiveDate>,
    /// Only projects that wait on another (blocked), that another waits on
    /// (blocking), or with a broken dependency on either side (broken).
    #[arg(long, value_enum, value_name = "LINK")]
    pub link: Option<super::dependencies::LinkFilter>,
    /// Only the projects in this initiative, by name or id; none keeps the
    /// projects in no initiative.
    #[arg(long, value_name = "INITIATIVE")]
    pub initiative: Option<String>,
}

fn calendar_day(value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").map_err(|_| "expected YYYY-MM-DD".to_owned())
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectSaveArgs {
    /// The project's name. Makes the project when none has it yet.
    pub name: String,
    /// Rename this existing project to NAME.
    #[arg(long, value_name = "PROJECT")]
    pub project: Option<String>,
    #[arg(long)]
    pub details: Option<String>,
    #[arg(long, value_enum)]
    pub status: Option<ProjectState>,
    /// me, a person from the People page, or nobody.
    #[arg(long, value_name = "PERSON")]
    pub lead: Option<String>,
    /// Comma-separated people, me among them for you; an empty value clears the list.
    #[arg(long, value_delimiter = ',', value_name = "PEOPLE")]
    pub members: Option<Vec<String>>,
    /// YYYY-MM-DD, today, tomorrow or none.
    #[arg(long, value_parser = day_or_none, value_name = "DAY")]
    pub target: Option<DayArg>,
    /// The project's colour, as a hue in degrees (0 to 360).
    #[arg(long, value_parser = clap::value_parser!(u16).range(0..=360))]
    pub hue: Option<u16>,
    #[arg(long, value_enum)]
    pub priority: Option<Priority>,
    /// YYYY-MM-DD, today, tomorrow or none.
    #[arg(long, value_parser = day_or_none, value_name = "DAY")]
    pub start: Option<DayArg>,
    /// How exactly --start is meant; day unless told.
    #[arg(long, value_enum, value_name = "RESOLUTION")]
    pub start_resolution: Option<ResolutionArg>,
    /// How exactly --target is meant; day unless told.
    #[arg(long, value_enum, value_name = "RESOLUTION")]
    pub target_resolution: Option<ResolutionArg>,
    /// A short symbol or emoji, or none.
    #[arg(long)]
    pub icon: Option<String>,
    /// Comma-separated labels; an empty value or none clears them.
    #[arg(long, value_delimiter = ',', value_name = "LABELS")]
    pub labels: Option<Vec<String>>,
    /// A link, TITLE=URL or a URL; repeat for more. The links given replace
    /// the project's links; none clears them.
    #[arg(long = "link", value_parser = link_arg, value_name = "LINK")]
    pub links: Vec<LinkArg>,
    /// A one-line summary shown under the name, or none.
    #[arg(long)]
    pub summary: Option<String>,
    /// A custom status from project-statuses; it sets --status to its
    /// kind. none clears it.
    #[arg(long, value_name = "NAME")]
    pub status_name: Option<String>,
    /// Anchored edits to the details or the summary, applied after
    /// --details and --summary, as JSON:
    /// [{"field":"description","op":"insert_after","anchor":"Scope:","value":" phase 2"}].
    /// field is description or summary; op is replace, append, prepend,
    /// insert_before, insert_after or replace_text, the last three with an
    /// anchor: text already in it. A null value clears the field.
    #[arg(long, value_name = "JSON")]
    pub patch_json: Option<String>,
    /// Show the project as it would be saved; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectArchiveArgs {
    pub project: String,
    /// Bring a put-away project back.
    #[arg(long)]
    pub restore: bool,
    /// Show the project as it would be left; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MilestonesArgs {
    pub project: String,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MilestoneSaveArgs {
    pub project: String,
    /// The milestone's name. Makes the milestone when the project has none by that name.
    pub name: String,
    /// Rename this existing milestone to NAME.
    #[arg(long, value_name = "MILESTONE")]
    pub milestone: Option<String>,
    /// YYYY-MM-DD, today, tomorrow or none.
    #[arg(long, value_parser = day_or_none, value_name = "DAY")]
    pub target: Option<DayArg>,
    #[arg(long)]
    pub details: Option<String>,
    #[arg(long, value_enum)]
    pub status: Option<MilestoneState>,
    /// Show the milestone as it would be saved; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MilestoneDeleteArgs {
    pub project: String,
    pub milestone: String,
    /// Show the milestone that would be deleted; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MilestoneArgs {
    pub project: String,
    pub milestone: String,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MilestoneToProjectArgs {
    pub project: String,
    /// The milestone that becomes a project; its tasks move with it.
    pub milestone: String,
    /// Show the project it would make; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectStatusSaveArgs {
    /// The status's name. Makes the status when none has that name.
    pub name: String,
    /// The built-in status it is a kind of. Changing it moves the projects
    /// that show this status.
    #[arg(long, value_enum)]
    pub category: Option<ProjectState>,
    /// Rename this existing status to NAME.
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,
    /// Show the status as it would be saved; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectStatusDeleteArgs {
    /// The status, by name or id. Its projects keep their built-in status.
    pub status: String,
    /// Show what would change; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectHistoryArgs {
    pub project: String,
    /// How many changes, newest first. Default 50, at most 200.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=200))]
    pub limit: Option<u16>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PeopleArgs {
    /// Only people whose name or alias contains this.
    #[arg(long, value_name = "TEXT")]
    pub query: Option<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonSaveArgs {
    /// The person's name. Makes the person when nobody goes by it yet.
    pub name: String,
    /// Rename this existing person to NAME. The old name stays as an alias.
    #[arg(long, value_name = "PERSON")]
    pub person: Option<String>,
    /// Other names they go by, comma-separated. Replaces the list; an empty value clears it.
    #[arg(long, value_delimiter = ',', value_name = "NAMES")]
    pub aliases: Option<Vec<String>>,
    #[arg(long)]
    pub role: Option<String>,
    #[arg(long, value_name = "ORGANIZATION")]
    pub org: Option<String>,
    /// Email addresses, comma-separated.
    #[arg(long, value_delimiter = ',', value_name = "EMAILS")]
    pub email: Option<Vec<String>>,
    #[arg(long)]
    pub notes: Option<String>,
    /// Show the person as they would be saved; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonMergeArgs {
    /// The person to fold away.
    pub person: String,
    /// The person who keeps everything.
    #[arg(long, value_name = "PERSON")]
    pub into: String,
    /// When both have a saved voice, which one stays: `into` keeps the voice
    /// of the person who keeps everything and deletes the other, `from` keeps
    /// the folded person's voice and deletes the other, `both` combines them.
    #[arg(long, value_enum, value_name = "WHICH")]
    pub voice: Option<MergeVoice>,
    /// Show the person as the merge would leave them; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum MergeVoice {
    Into,
    From,
    Both,
}

impl MergeVoice {
    const fn resolution(self) -> VoiceProfileMergeResolution {
        match self {
            Self::Into => VoiceProfileMergeResolution::DiscardSource,
            Self::From => VoiceProfileMergeResolution::ReplaceTargetWithSource,
            Self::Both => VoiceProfileMergeResolution::CombineCompatible,
        }
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MeetingsArgs {
    /// Only meetings from then on: milliseconds since the epoch, or YYYY-MM-DD.
    #[arg(long, value_parser = since, value_name = "WHEN")]
    pub since: Option<SinceArg>,
    #[arg(long)]
    pub limit: Option<usize>,
    /// Only meetings whose title contains this, ignoring case.
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,
    /// Only meetings in this state.
    #[arg(long, value_enum, value_name = "STATUS")]
    pub status: Option<super::library::MeetingStatus>,
    /// Only meetings filed in this folder, by name or id, from `folders`.
    #[arg(long, value_name = "FOLDER")]
    pub folder: Option<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MeetingArgs {
    pub id: String,
    /// Include the transcript, with who said each line.
    #[arg(long)]
    pub transcript: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkPersonRef {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProject {
    pub id: String,
    pub name: String,
    pub details: Option<String>,
    /// A one-line summary shown under the name.
    pub summary: Option<String>,
    pub status: ProjectStatus,
    /// The custom status it shows, a kind of `status`.
    pub status_name: Option<String>,
    pub priority: Priority,
    pub icon: Option<String>,
    pub lead: Option<ExternalWorkPersonRef>,
    pub lead_is_self: bool,
    pub members: Vec<ExternalWorkPersonRef>,
    pub members_include_self: bool,
    pub start: Option<String>,
    pub start_at_ms: Option<i64>,
    pub start_resolution: Option<DateResolution>,
    pub target: Option<String>,
    pub target_at_ms: Option<i64>,
    pub target_resolution: Option<DateResolution>,
    pub hue: Option<u16>,
    pub labels: Vec<String>,
    pub links: Vec<ProjectLink>,
    pub archived: bool,
    /// When it went to the bin; it is purged 30 days later.
    pub trashed_at_ms: Option<i64>,
    /// When its latest update was posted.
    pub latest_update_at_ms: Option<i64>,
    pub open_tasks: u32,
    pub done_tasks: u32,
    pub milestones: u32,
    /// The initiative it sits in, if any.
    pub initiative: Option<ExternalWorkPersonRef>,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjects {
    pub schema_version: u32,
    pub projects: Vec<ExternalWorkProject>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectChange {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub created: bool,
    pub project: ExternalWorkProject,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkMilestone {
    pub id: String,
    pub name: String,
    pub details: Option<String>,
    pub status: TaskStatus,
    pub target: Option<String>,
    pub target_at_ms: Option<i64>,
    pub open_tasks: u32,
    pub done_tasks: u32,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkMilestones {
    pub schema_version: u32,
    pub project: ExternalWorkPersonRef,
    pub milestones: Vec<ExternalWorkMilestone>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkMilestoneChange {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub created: bool,
    pub deleted: bool,
    pub project: ExternalWorkPersonRef,
    pub milestone: ExternalWorkMilestone,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkMilestoneTask {
    pub id: String,
    pub handle: String,
    pub title: String,
    pub status: WorkStatus,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkMilestoneDetail {
    pub schema_version: u32,
    pub project: ExternalWorkPersonRef,
    pub milestone: ExternalWorkMilestone,
    pub tasks: Vec<ExternalWorkMilestoneTask>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkMilestoneConverted {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: String,
    /// The project the milestone was in.
    pub from_project: ExternalWorkPersonRef,
    pub project: ExternalWorkProject,
    pub moved_tasks: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectStatus {
    pub id: String,
    pub name: String,
    /// The built-in status it is a kind of.
    pub category: ProjectStatus,
    pub position: i64,
    /// Projects that show it, the bin left out.
    pub projects: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectStatuses {
    pub schema_version: u32,
    pub statuses: Vec<ExternalWorkProjectStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectStatusChange {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub created: bool,
    pub deleted: bool,
    pub status: ExternalWorkProjectStatus,
    /// Projects whose status the change moved or cleared.
    pub changed_projects: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectChangeEntry {
    /// title, status, statusName, lead, members, startDate, targetDate,
    /// priority, milestoneAdded, milestoneRemoved or milestoneTarget.
    pub kind: String,
    /// The milestone a milestone change is about.
    pub subject: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub at_ms: i64,
    pub at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectHistory {
    pub schema_version: u32,
    pub project: ExternalWorkPersonRef,
    pub changes: Vec<ExternalWorkProjectChangeEntry>,
}

/// Custom statuses, a project's history and single milestones.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ProjectAdminAnswer {
    Statuses(ExternalWorkProjectStatuses),
    Status(ExternalWorkProjectStatusChange),
    History(ExternalWorkProjectHistory),
    Milestone(ExternalWorkMilestoneDetail),
    Converted(ExternalWorkMilestoneConverted),
}

fn admin(answer: ProjectAdminAnswer) -> WorkAnswer {
    WorkAnswer::ProjectAdmin(Box::new(answer))
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkPerson {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub role: Option<String>,
    pub organization: Option<String>,
    pub emails: Vec<String>,
    pub notes: Option<String>,
    pub open_tasks: u32,
    pub meetings: u64,
    pub last_meeting_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkPeople {
    pub schema_version: u32,
    pub people: Vec<ExternalWorkPerson>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkPersonChange {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub created: bool,
    /// Set when the person was merged away: who they were folded into.
    pub merged_from: Option<String>,
    pub moved_tasks: u32,
    pub person: ExternalWorkPerson,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkMeeting {
    pub id: Uuid,
    pub title: String,
    pub start_ms: i64,
    pub start: String,
    pub duration_ms: Option<i64>,
    pub phase: MeetingPhase,
    pub processing: ProcessingStatus,
    pub processed: bool,
    pub speakers: Vec<String>,
    pub people: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkMeetings {
    pub schema_version: u32,
    pub meetings: Vec<ExternalWorkMeeting>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkMeetingDetail {
    #[serde(flatten)]
    pub meeting: ExternalMeetingDetail,
    pub people: Vec<String>,
    /// Each speaker by the name the transcript shows, with the id
    /// `speaker-assign` takes and whether they still need a name.
    pub meeting_speakers: Vec<super::speakers::ExternalWorkSpeaker>,
    pub transcript: Option<ExternalTranscript>,
}

fn target_text(ms: Option<i64>) -> Option<String> {
    ms.map(|ms| day_text(Some(ms)))
}

fn words(values: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !kept.iter().any(|seen| seen.eq_ignore_ascii_case(value)) {
            kept.push(value.to_owned());
        }
    }
    kept
}

pub(super) fn cleared(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || value.eq_ignore_ascii_case("none")
}

fn is_open(status: WorkStatus) -> bool {
    matches!(
        status,
        WorkStatus::Backlog | WorkStatus::NotStarted | WorkStatus::InProgress
    )
}

impl Board {
    fn project_people(&self, args: &ProjectSaveArgs) -> Result<ProjectEdit, ExternalError> {
        let lead = match &args.lead {
            Some(value) if cleared(value) => Some(WorkOwner::Nobody),
            Some(value) => Some(self.owner(value)?),
            None => None,
        };
        let (member_person_ids, members_include_self) = match &args.members {
            Some(values) => {
                let mut ids = Vec::new();
                let mut include_self = false;
                for value in words(values) {
                    match self.owner(&value)? {
                        WorkOwner::Myself => include_self = true,
                        WorkOwner::Nobody => {}
                        WorkOwner::Person { person_id } => {
                            if !ids.contains(&person_id) {
                                ids.push(person_id);
                            }
                        }
                    }
                }
                ids.sort();
                (Some(ids), Some(include_self))
            }
            None => (None, None),
        };
        Ok(ProjectEdit {
            lead,
            member_person_ids,
            members_include_self,
            ..ProjectEdit::default()
        })
    }
}

impl Tracker<'_> {
    pub(super) fn every_project(&self) -> Result<Vec<Project>, ExternalError> {
        planning::history(self.history, |conn| plans::all_projects(conn))
            .map_err(|message| super::refusal(self.history, message))
    }

    pub(super) fn project_row(&self, project: &Project, board: &Board) -> ExternalWorkProject {
        let person = |id: &String| {
            board
                .people
                .iter()
                .find(|person| &person.id == id)
                .map(|person| ExternalWorkPersonRef {
                    id: person.id.clone(),
                    name: person.name.clone(),
                })
        };
        let (mut open, mut done) = (0, 0);
        for row in &board.rows {
            let item = &row.item;
            if item.project_id.as_deref() != Some(project.id.as_str())
                || item.archived_at_ms.is_some()
            {
                continue;
            }
            if is_open(item.status) {
                open += 1;
            } else if item.status == WorkStatus::Done {
                done += 1;
            }
        }
        let (milestones, latest_update_at_ms, status_name, initiative) = planning::history(self.history, |conn| {
            let milestones = plans::records(conn, &project.id)?.milestones.len();
            let latest = planning::updates::latest(conn, &project.id)?
                .map(|update| update.created_at_ms);
            let status_name = project
                .status_name_id
                .as_deref()
                .map(|id| planning::catalog::status_name(conn, id))
                .transpose()?
                .map(|name| name.name);
            let initiative = planning::work::initiatives::initiative_of(conn, &project.id)?
                .map(|initiative| ExternalWorkPersonRef {
                    id: initiative.id,
                    name: initiative.name,
                });
            Ok((milestones, latest, status_name, initiative))
        })
        .unwrap_or_default();
        let labels = self.all_labels().unwrap_or_default();
        ExternalWorkProject {
            id: project.id.clone(),
            name: project.title.clone(),
            details: project.description.clone(),
            summary: project.summary.clone(),
            status: project.status,
            status_name,
            priority: Priority::of(project.priority),
            icon: project.icon.clone(),
            lead: project.lead_person_id.as_ref().and_then(person),
            lead_is_self: project.lead_is_self,
            members: project
                .member_person_ids
                .iter()
                .filter_map(person)
                .collect(),
            members_include_self: project.members_include_self,
            start: target_text(project.start_at_ms),
            start_at_ms: project.start_at_ms,
            start_resolution: project.start_resolution,
            target: target_text(project.target_at_ms),
            target_at_ms: project.target_at_ms,
            target_resolution: project.target_resolution,
            hue: project.hue,
            labels: project
                .label_ids
                .iter()
                .filter_map(|id| labels.iter().find(|label| &label.id == id))
                .map(|label| label.name.clone())
                .collect(),
            links: project.links.clone(),
            archived: project.archived_at_ms.is_some(),
            trashed_at_ms: project.trashed_at_ms,
            latest_update_at_ms,
            open_tasks: open,
            done_tasks: done,
            milestones: u32::try_from(milestones).unwrap_or(u32::MAX),
            initiative,
            revision: project.revision,
        }
    }

    pub(super) fn pick_project<'a>(
        value: &str,
        projects: &'a [Project],
    ) -> Result<&'a Project, ExternalError> {
        pick(
            "project",
            value,
            projects,
            |project| &project.id,
            |project| &project.title,
        )
        .map_err(|refusal| refusal.hint("List them with: sona work projects --all"))
    }

    async fn people_board(&self) -> Result<Board, ExternalError> {
        let board = self.board().await?;
        if !board.fence.is_open() {
            return Err(conversations_closed());
        }
        Ok(board)
    }

    pub(super) async fn projects(&self, args: &ProjectsArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let mut projects = self.every_project()?;
        projects.retain(|project| {
            if args.trashed {
                project.trashed_at_ms.is_some()
            } else {
                project.trashed_at_ms.is_none() && (args.all || project.archived_at_ms.is_none())
            }
        });
        projects.sort_by_key(|project| project.title.to_lowercase());
        let before_ms = args.update_before.map(super::midnight).transpose()?;
        let links = args
            .link
            .map(|filter| self.all_dependencies().map(|all| (filter, all)))
            .transpose()?;
        let initiative = match args.initiative.as_deref().map(str::trim) {
            None => None,
            Some(value) if value.eq_ignore_ascii_case("none") => Some(None),
            Some(value) => {
                let all = self.all_initiatives()?;
                Some(Some(Self::pick_initiative(value, &all)?.id.clone()))
            }
        };
        Ok(WorkAnswer::Projects(ExternalWorkProjects {
            schema_version: QUERY_SCHEMA_VERSION,
            projects: projects
                .iter()
                .filter(|project| {
                    links
                        .as_ref()
                        .is_none_or(|(filter, all)| filter.keeps(&project.id, all))
                })
                .map(|project| self.project_row(project, &board))
                .filter(|row| {
                    before_ms.is_none_or(|before| {
                        row.latest_update_at_ms.is_none_or(|latest| latest < before)
                    }) && initiative.as_ref().is_none_or(|wanted| {
                        row.initiative.as_ref().map(|initiative| &initiative.id) == wanted.as_ref()
                    })
                })
                .collect(),
        }))
    }

    pub(super) async fn project_save(
        &self,
        args: &ProjectSaveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.people_board().await?;
        let projects = self.every_project()?;
        let name = args.name.trim();
        let existing = match &args.project {
            Some(value) => Some(Self::pick_project(value, &projects)?),
            None => projects
                .iter()
                .find(|project| project.title.to_lowercase() == name.to_lowercase()),
        };
        if let Some(taken) = projects.iter().find(|project| {
            project.title.to_lowercase() == name.to_lowercase()
                && existing.is_none_or(|existing| existing.id != project.id)
        }) {
            return Err(ExternalError::invalid(format!(
                "A project called {:?} already exists.",
                taken.title
            )));
        }
        let people = board.project_people(args)?;
        let label_ids = match &args.labels {
            Some(values) => {
                let names = words(values);
                let mut ids = Vec::new();
                if !names.iter().any(|name| cleared(name)) {
                    let labels = self.all_labels()?;
                    for name in names {
                        let label = pick(
                            "label",
                            &name,
                            &labels,
                            |label| &label.id,
                            |label| &label.name,
                        )
                        .map_err(|refusal| refusal.hint("List them with: sona work labels"))?;
                        if !ids.contains(&label.id) {
                            ids.push(label.id.clone());
                        }
                    }
                }
                Some(ids)
            }
            None => None,
        };
        let links = (!args.links.is_empty()).then(|| {
            args.links
                .iter()
                .filter_map(|link| match link {
                    LinkArg::Clear => None,
                    LinkArg::Link(link) => Some(link.clone()),
                })
                .collect::<Vec<_>>()
        });
        let status_name_id = self.status_named(args)?;
        let (description, summary) = patched(args, existing)?;
        let edit = ProjectEdit {
            title: (existing.is_none() || args.project.is_some()).then(|| name.to_owned()),
            description,
            status: args.status.map(Into::into),
            lead: people.lead,
            member_person_ids: people.member_person_ids,
            members_include_self: people.members_include_self,
            target_at_ms: args.target.map(DayArg::ms).transpose()?,
            hue: args.hue,
            archived: None,
            priority: args.priority.map(Priority::value),
            start_at_ms: args.start.map(DayArg::ms).transpose()?,
            start_resolution: args.start_resolution.map(ResolutionArg::resolution),
            target_resolution: args.target_resolution.map(ResolutionArg::resolution),
            icon: args
                .icon
                .as_deref()
                .map(|icon| if cleared(icon) { String::new() } else { icon.trim().to_owned() }),
            label_ids,
            links,
            trashed: None,
            summary,
            status_name_id,
        };
        if let Some(current) = existing {
            if !changes_project(current, &edit) {
                return Ok(WorkAnswer::Project(ExternalWorkProjectChange {
                    schema_version: QUERY_SCHEMA_VERSION,
                    dry_run: args.dry_run,
                    operation_id: None,
                    created: false,
                    project: self.project_row(current, &board),
                }));
            }
        }
        let operation_id = match existing {
            Some(current) => Some(
                args.write
                    .operation_id("project-save", &(&current.id, current.revision, &edit))?,
            ),
            None => args.write.operation_id.map(|id| id.to_string()),
        };
        let id = existing.map(|project| project.id.clone());
        if args.dry_run {
            let project = self.rehearse(|conn| {
                planning::catalog::save_project(conn, id.as_deref(), &edit, super::now_ms())
            })?;
            return Ok(WorkAnswer::Project(ExternalWorkProjectChange {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                operation_id,
                created: existing.is_none(),
                project: self.project_row(&project, &board),
            }));
        }
        let operation_id = operation_id.unwrap_or_else(|| args.write.fresh_operation_id());
        self.applies("saving a project")?;
        let project = work::catalog_project_save(self.app, &operation_id, id, &edit)
            .map_err(|message| self.write_refusal(message, &operation_id, "The project"))?;
        Ok(WorkAnswer::Project(ExternalWorkProjectChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: false,
            operation_id: Some(operation_id),
            created: existing.is_none(),
            project: self.project_row(&project, &board),
        }))
    }

    pub(super) async fn project_archive(
        &self,
        args: &ProjectArchiveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let projects = self.every_project()?;
        let current = Self::pick_project(&args.project, &projects)?;
        let archive = !args.restore;
        if current.archived_at_ms.is_some() != archive {
            let edit = ProjectEdit {
                archived: Some(archive),
                ..ProjectEdit::default()
            };
            let operation_id = args
                .write
                .operation_id("project-archive", &(&current.id, current.revision, archive))?;
            let project = if args.dry_run {
                self.rehearse(|conn| {
                    planning::catalog::save_project(conn, Some(&current.id), &edit, now_ms())
                })?
            } else {
                self.applies("putting a project away")?;
                work::catalog_project_save(self.app, &operation_id, Some(current.id.clone()), &edit)
                    .map_err(|message| self.write_refusal(message, &operation_id, "The project"))?
            };
            return Ok(WorkAnswer::Project(ExternalWorkProjectChange {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: args.dry_run,
                operation_id: Some(operation_id),
                created: false,
                project: self.project_row(&project, &board),
            }));
        }
        Ok(WorkAnswer::Project(ExternalWorkProjectChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id: None,
            created: false,
            project: self.project_row(current, &board),
        }))
    }

    pub(super) fn milestone_row(milestone: &Milestone, board: &Board) -> ExternalWorkMilestone {
        let (mut open, mut done) = (0, 0);
        for row in &board.rows {
            let item = &row.item;
            if item.milestone_id.as_deref() != Some(milestone.id.as_str())
                || item.archived_at_ms.is_some()
            {
                continue;
            }
            if is_open(item.status) {
                open += 1;
            } else if item.status == WorkStatus::Done {
                done += 1;
            }
        }
        ExternalWorkMilestone {
            id: milestone.id.clone(),
            name: milestone.title.clone(),
            details: milestone.description.clone(),
            status: milestone.status,
            target: target_text(milestone.due_at_ms),
            target_at_ms: milestone.due_at_ms,
            open_tasks: open,
            done_tasks: done,
            revision: milestone.revision,
        }
    }

    pub(super) fn milestones_of(&self, project: &Project) -> Result<Vec<Milestone>, ExternalError> {
        planning::history(self.history, |conn| {
            Ok(plans::records(conn, &project.id)?.milestones)
        })
        .map_err(|message| super::refusal(self.history, message))
    }

    pub(super) async fn milestones(
        &self,
        args: &MilestonesArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let projects = self.every_project()?;
        let project = Self::pick_project(&args.project, &projects)?;
        Ok(WorkAnswer::Milestones(ExternalWorkMilestones {
            schema_version: QUERY_SCHEMA_VERSION,
            project: ExternalWorkPersonRef {
                id: project.id.clone(),
                name: project.title.clone(),
            },
            milestones: self
                .milestones_of(project)?
                .iter()
                .map(|milestone| Self::milestone_row(milestone, &board))
                .collect(),
        }))
    }

    pub(super) async fn milestone_save(
        &self,
        args: &MilestoneSaveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let projects = self.every_project()?;
        let project = Self::pick_project(&args.project, &projects)?;
        let milestones = self.milestones_of(project)?;
        let name = args.name.trim();
        let existing = match &args.milestone {
            Some(value) => Some(pick_milestone(value, &milestones)?),
            None => milestones
                .iter()
                .find(|milestone| milestone.title.to_lowercase() == name.to_lowercase()),
        };
        if let Some(taken) = milestones.iter().find(|milestone| {
            milestone.title.to_lowercase() == name.to_lowercase()
                && existing.is_none_or(|existing| existing.id != milestone.id)
        }) {
            return Err(ExternalError::invalid(format!(
                "{:?} already has a milestone called {:?}.",
                project.title, taken.title
            )));
        }
        let edit = MilestoneEdit {
            title: (existing.is_none() || args.milestone.is_some()).then(|| name.to_owned()),
            description: args.details.clone(),
            due_at_ms: args.target.map(DayArg::ms).transpose()?,
            status: args.status.map(Into::into),
        };
        let project_ref = ExternalWorkPersonRef {
            id: project.id.clone(),
            name: project.title.clone(),
        };
        if let Some(current) = existing {
            if !changes_milestone(current, &edit) {
                return Ok(WorkAnswer::Milestone(ExternalWorkMilestoneChange {
                    schema_version: QUERY_SCHEMA_VERSION,
                    dry_run: args.dry_run,
                    operation_id: None,
                    created: false,
                    deleted: false,
                    project: project_ref,
                    milestone: Self::milestone_row(current, &board),
                }));
            }
        }
        let operation_id = match existing {
            Some(current) => args
                .write
                .operation_id("milestone-save", &(&current.id, current.revision, &edit))?,
            None => args.write.fresh_operation_id(),
        };
        let id = existing.map(|milestone| milestone.id.clone());
        let milestone = if args.dry_run {
            self.rehearse(|conn| {
                planning::catalog::save_milestone(conn, &project.id, id.as_deref(), &edit, now_ms())
            })?
        } else {
            self.applies("saving a milestone")?;
            work::catalog_milestone_save(self.app, &operation_id, &project.id, id, &edit)
                .map_err(|message| self.write_refusal(message, &operation_id, "The milestone"))?
        };
        Ok(WorkAnswer::Milestone(ExternalWorkMilestoneChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id: Some(operation_id),
            created: existing.is_none(),
            deleted: false,
            project: project_ref,
            milestone: Self::milestone_row(&milestone, &board),
        }))
    }

    pub(super) async fn milestone_delete(
        &self,
        args: &MilestoneDeleteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let projects = self.every_project()?;
        let project = Self::pick_project(&args.project, &projects)?;
        let milestones = self.milestones_of(project)?;
        let milestone = pick_milestone(&args.milestone, &milestones)?;
        let operation_id = args
            .write
            .operation_id("milestone-delete", &(&milestone.id, milestone.revision))?;
        if args.dry_run {
            self.rehearse(|conn| {
                planning::catalog::delete_milestone(conn, &milestone.id, now_ms())
            })?;
        } else {
            self.applies("deleting a milestone")?;
            work::catalog_milestone_delete(self.app, &operation_id, &milestone.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The milestone"))?;
        }
        Ok(WorkAnswer::Milestone(ExternalWorkMilestoneChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id: Some(operation_id),
            created: false,
            deleted: true,
            project: ExternalWorkPersonRef {
                id: project.id.clone(),
                name: project.title.clone(),
            },
            milestone: Self::milestone_row(milestone, &board),
        }))
    }

    fn person_row(
        board: &Board,
        sources: &[String],
        entry: &PersonListEntry,
    ) -> ExternalWorkPerson {
        let open = board
            .rows
            .iter()
            .filter(|row| {
                row.item.archived_at_ms.is_none()
                    && is_open(row.item.status)
                    && row
                        .item
                        .owner_person_id
                        .as_ref()
                        .is_some_and(|id| sources.contains(id))
            })
            .count();
        let person = &entry.person;
        ExternalWorkPerson {
            id: person.id.uuid().to_string(),
            name: person.display_name.clone(),
            aliases: person.aliases.clone(),
            role: person.role.clone(),
            organization: person.organization.clone(),
            emails: person.calendar_emails.clone(),
            notes: person.notes.clone(),
            open_tasks: u32::try_from(open).unwrap_or(u32::MAX),
            meetings: entry.meetings_count,
            last_meeting_at_ms: entry.last_meeting_at_utc_ms,
        }
    }

    pub(super) async fn people(&self, args: &PeopleArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.people_board().await?;
        let store = store(self.meetings).await?;
        let listed = store.people_list().map_err(QueryError::from)?;
        let wanted = args.query.as_ref().map(|text| text.trim().to_lowercase());
        let mut people = Vec::new();
        for entry in &listed.entries {
            let person = &entry.person;
            if let Some(wanted) = &wanted {
                let named = person.display_name.to_lowercase().contains(wanted)
                    || person
                        .aliases
                        .iter()
                        .any(|alias| alias.to_lowercase().contains(wanted));
                if !named {
                    continue;
                }
            }
            let sources = self.person_sources(&store, &person.id.uuid().to_string())?;
            people.push(Self::person_row(&board, &sources, entry));
        }
        people.sort_by_key(|person| person.name.to_lowercase());
        Ok(WorkAnswer::People(ExternalWorkPeople {
            schema_version: QUERY_SCHEMA_VERSION,
            people,
        }))
    }

    fn person_sources(
        &self,
        store: &crate::meeting::store::MeetingStore,
        id: &str,
    ) -> Result<Vec<String>, ExternalError> {
        let person_id = crate::meeting::people_types::PersonId::from_uuid(
            Uuid::parse_str(id).map_err(|_| ExternalError::invalid("Not a person id."))?,
        );
        Ok(store
            .person_source_ids(person_id)
            .map_err(QueryError::from)?)
    }

    fn claimed_elsewhere(
        people: &[WorkPersonChoice],
        owner: Option<&str>,
        names: &[String],
    ) -> Result<(), ExternalError> {
        for name in names {
            let lower = name.to_lowercase();
            if let Some(other) = people.iter().find(|person| {
                owner != Some(person.id.as_str())
                    && (person.name.to_lowercase() == lower
                        || person
                            .aliases
                            .iter()
                            .any(|alias| alias.to_lowercase() == lower))
            }) {
                return Err(ExternalError::invalid(format!(
                    "{name:?} already belongs to {}. Merge the two with: sona work person-merge",
                    other.name
                )));
            }
        }
        Ok(())
    }

    pub(super) async fn person_save(
        &self,
        args: &PersonSaveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.people_board().await?;
        let name = args.name.trim();
        let existing = match &args.person {
            Some(value) => Some(board.person(value)?),
            None => {
                let lower = name.to_lowercase();
                let found: Vec<&WorkPersonChoice> = board
                    .people
                    .iter()
                    .filter(|person| {
                        person.name.to_lowercase() == lower
                            || person
                                .aliases
                                .iter()
                                .any(|alias| alias.to_lowercase() == lower)
                    })
                    .collect();
                match found.as_slice() {
                    [] => None,
                    [person] => Some(*person),
                    _ => {
                        return Err(ExternalError::invalid(format!(
                            "More than one person goes by {name:?}. Pick one with --person."
                        )))
                    }
                }
            }
        };
        let aliases = args.aliases.as_deref().map(words);
        let mut claimed = aliases.clone().unwrap_or_default();
        if existing.is_none() || args.person.is_some() {
            claimed.push(name.to_owned());
        }
        Self::claimed_elsewhere(
            &board.people,
            existing.map(|person| person.id.as_str()),
            &claimed,
        )?;
        let edit = PersonProfileEdit {
            display_name: (existing.is_none() || args.person.is_some()).then(|| name.to_owned()),
            aliases,
            role: args.role.clone(),
            organization: args.org.clone(),
            calendar_emails: args.email.as_deref().map(words),
            notes: args.notes.clone(),
        };
        let operation_id = match existing {
            Some(person) => args
                .write
                .operation_id("person-save", &(&person.id, &edit))?,
            None => args.write.fresh_operation_id(),
        };
        let store = store(self.meetings).await?;
        let created = existing.is_none();
        if args.dry_run {
            let person_id = existing
                .map(|person| parse_person(&person.id))
                .transpose()?;
            let (saved, changed) = store
                .person_profile_preview(person_id, name.to_owned(), edit, now_ms())
                .map_err(|error| {
                    ExternalError::invalid(format!(
                        "The person could not be saved ({:?}).",
                        map_store_error(error)
                    ))
                })?;
            let sources = match existing {
                Some(person) => self.person_sources(&store, &person.id)?,
                None => Vec::new(),
            };
            return Ok(WorkAnswer::Person(ExternalWorkPersonChange {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                operation_id: changed.then_some(operation_id),
                created,
                merged_from: None,
                moved_tasks: 0,
                person: Self::person_row(&board, &sources, &saved),
            }));
        }
        let before = store.people_list().map_err(QueryError::from)?;
        self.applies("saving a person")?;
        match existing {
            Some(person) => {
                self.meetings
                    .person_profile_update(parse_person(&person.id)?, edit)
                    .await
            }
            None => {
                self.meetings
                    .person_profile_create(name.to_owned(), edit)
                    .await
            }
        }
        .map_err(|error| {
            ExternalError::invalid(format!("The person could not be saved ({error:?})."))
        })?;
        let after = store.people_list().map_err(QueryError::from)?;
        let saved =
            saved_person(&before.entries, &after.entries, name, existing).ok_or_else(|| {
                ExternalError::new(ExternalErrorCode::Failed, "The person was not saved.")
            })?;
        let sources = self.person_sources(&store, &saved.person.id.uuid().to_string())?;
        let changed = created || after.revision != before.revision;
        Ok(WorkAnswer::Person(ExternalWorkPersonChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: false,
            operation_id: changed.then_some(operation_id),
            created,
            merged_from: None,
            moved_tasks: 0,
            person: Self::person_row(&board, &sources, saved),
        }))
    }

    pub(super) async fn person_merge(
        &self,
        args: &PersonMergeArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.people_board().await?;
        let from = board.person(&args.person)?;
        let into = board.person(&args.into)?;
        if from.id == into.id {
            return Err(ExternalError::invalid(
                "Pick two different people to merge.",
            ));
        }
        let store = store(self.meetings).await?;
        let moved = {
            let sources = self.person_sources(&store, &from.id)?;
            board
                .rows
                .iter()
                .filter(|row| {
                    row.item
                        .owner_person_id
                        .as_ref()
                        .is_some_and(|id| sources.contains(id))
                })
                .count()
        };
        let operation_id = args
            .write
            .operation_id("person-merge", &(&from.id, &into.id))?;
        if !args.dry_run {
            self.applies("merging people")?;
        }
        let kept = merge_people(
            self.meetings,
            parse_person(&from.id)?,
            parse_person(&into.id)?,
            args.voice,
            args.dry_run,
        )
        .await?;
        let mut sources = self.person_sources(&store, &into.id)?;
        if args.dry_run {
            sources.extend(self.person_sources(&store, &from.id)?);
        } else {
            work::catalog_person_repoint(self.app, &operation_id, &from.id, &into.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The tracker"))?;
        }
        let kept = &kept;
        Ok(WorkAnswer::Person(ExternalWorkPersonChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id: Some(operation_id),
            created: false,
            merged_from: Some(from.name.clone()),
            moved_tasks: u32::try_from(moved).unwrap_or(u32::MAX),
            person: Self::person_row(&board, &sources, kept),
        }))
    }

    pub(super) async fn meetings(&self, args: &MeetingsArgs) -> Result<WorkAnswer, ExternalError> {
        let store = store(self.meetings).await?;
        let since_ms = match args.since {
            Some(SinceArg::Ms(ms)) => Some(ms),
            Some(SinceArg::Day(day)) => Some(super::super::day_start_utc_ms(
                &day.format("%Y-%m-%d").to_string(),
            )?),
            None => None,
        };
        let limit = args
            .limit
            .unwrap_or(super::DEFAULT_LIMIT)
            .clamp(1, super::MAX_LIMIT);
        let filter = crate::meeting::types::MeetingListFilter {
            status: args.status.map(Into::into).unwrap_or_default(),
            title_query: args.title.clone().unwrap_or_default(),
            folder_id: match &args.folder {
                Some(folder) => Some(super::library::folder(self.meetings, folder).await?.folder_id),
                None => None,
            },
            ..Default::default()
        };
        let (entries, has_more) = meeting_summaries(&store, since_ms, None, limit, &filter)?;
        let mut meetings = Vec::with_capacity(entries.len());
        for entry in entries {
            let people = meeting_people(&store, entry.session_id)?;
            meetings.push(ExternalWorkMeeting {
                id: entry.session_id.uuid(),
                title: entry.title,
                start_ms: entry.created_at_utc_ms,
                start: super::local_time(entry.created_at_utc_ms),
                duration_ms: entry.recorded_duration_ms,
                phase: entry.phase,
                processed: entry.phase == MeetingPhase::ReviewReady
                    && entry.processing_status == ProcessingStatus::Succeeded,
                processing: entry.processing_status,
                speakers: entry.speaker_labels,
                people,
            });
        }
        Ok(WorkAnswer::Meetings(ExternalWorkMeetings {
            schema_version: QUERY_SCHEMA_VERSION,
            meetings,
            has_more,
        }))
    }

    pub(super) async fn meeting(&self, args: &MeetingArgs) -> Result<WorkAnswer, ExternalError> {
        let store = store(self.meetings).await?;
        let id = session_id(&args.id)?;
        let meeting = meeting_detail(&store, id)?;
        let transcript = if args.transcript {
            Some(transcript(&store, id)?)
        } else {
            None
        };
        Ok(WorkAnswer::MeetingDetail(Box::new(
            ExternalWorkMeetingDetail {
                people: meeting_people(&store, id)?,
                meeting_speakers: super::speakers::listed(self.meetings, &store, id).await?,
                meeting,
                transcript,
            },
        )))
    }
}

fn parse_person(id: &str) -> Result<crate::meeting::people_types::PersonId, ExternalError> {
    Uuid::parse_str(id)
        .map(crate::meeting::people_types::PersonId::from_uuid)
        .map_err(|_| ExternalError::invalid("Not a person id."))
}

fn meeting_people(
    store: &crate::meeting::store::MeetingStore,
    id: crate::meeting::types::MeetingSessionId,
) -> Result<Vec<String>, ExternalError> {
    let context = store.meeting_people_context(id).map_err(QueryError::from)?;
    let mut names: Vec<String> = context
        .rows
        .into_iter()
        .map(|row| row.display_name)
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

fn saved_person<'a>(
    before: &'a [PersonListEntry],
    after: &'a [PersonListEntry],
    name: &str,
    existing: Option<&WorkPersonChoice>,
) -> Option<&'a PersonListEntry> {
    if let Some(existing) = existing {
        return after
            .iter()
            .find(|entry| entry.person.id.uuid().to_string() == existing.id);
    }
    after.iter().find(|entry| {
        entry.person.display_name == name
            && !before.iter().any(|old| old.person.id == entry.person.id)
    })
}

pub(super) fn pick_milestone<'a>(
    value: &str,
    milestones: &'a [Milestone],
) -> Result<&'a Milestone, ExternalError> {
    pick(
        "milestone",
        value,
        milestones,
        |milestone| &milestone.id,
        |milestone| &milestone.title,
    )
    .map_err(|refusal| refusal.hint("List them with: sona work milestones PROJECT"))
}

fn changes_project(current: &Project, edit: &ProjectEdit) -> bool {
    let text = |new: &Option<String>, old: &Option<String>| {
        new.as_ref()
            .is_some_and(|new| new.trim() != old.as_deref().unwrap_or_default())
    };
    edit.title
        .as_ref()
        .is_some_and(|title| title != &current.title)
        || text(&edit.description, &current.description)
        || edit.status.is_some_and(|status| status != current.status)
        || edit.lead.as_ref().is_some_and(|lead| {
            *lead != WorkOwner::of(current.lead_is_self, current.lead_person_id.as_deref())
        })
        || edit.member_person_ids.as_ref().is_some_and(|members| {
            let mut now = current.member_person_ids.clone();
            now.sort();
            &now != members
        })
        || edit
            .members_include_self
            .is_some_and(|include| include != current.members_include_self)
        || edit
            .target_at_ms
            .is_some_and(|target| target != current.target_at_ms)
        || edit.hue.is_some_and(|hue| Some(hue) != current.hue)
        || edit
            .archived
            .is_some_and(|archived| archived != current.archived_at_ms.is_some())
        || edit.priority.is_some_and(|priority| priority != current.priority)
        || edit
            .start_at_ms
            .is_some_and(|start| start != current.start_at_ms)
        || edit
            .start_resolution
            .is_some_and(|resolution| resolution != current.start_resolution)
        || edit
            .target_resolution
            .is_some_and(|resolution| resolution != current.target_resolution)
        || text(&edit.icon, &current.icon)
        || text(&edit.summary, &current.summary)
        || edit
            .status_name_id
            .as_ref()
            .is_some_and(|id| *id != current.status_name_id)
        || edit.label_ids.as_ref().is_some_and(|ids| {
            let (mut want, mut have) = (ids.clone(), current.label_ids.clone());
            want.sort();
            have.sort();
            want != have
        })
        || edit.links.as_ref().is_some_and(|links| {
            links.len() != current.links.len()
                || links.iter().zip(&current.links).any(|(new, old)| {
                    new.url.trim() != old.url
                        || new.title.as_deref().map(str::trim) != old.title.as_deref()
                })
        })
}

fn changes_milestone(current: &Milestone, edit: &MilestoneEdit) -> bool {
    edit.title
        .as_ref()
        .is_some_and(|title| title != &current.title)
        || edit
            .description
            .as_ref()
            .is_some_and(|text| text.trim() != current.description.as_deref().unwrap_or_default())
        || edit.status.is_some_and(|status| status != current.status)
        || edit.due_at_ms.is_some_and(|due| due != current.due_at_ms)
}

fn pick_status<'a>(
    value: &str,
    names: &'a [ProjectStatusName],
) -> Result<&'a ProjectStatusName, ExternalError> {
    pick("status", value, names, |name| &name.id, |name| &name.name)
        .map_err(|refusal| refusal.hint("List them with: sona work project-statuses"))
}

fn using(projects: &[Project], status: &ProjectStatusName) -> Vec<String> {
    projects
        .iter()
        .filter(|project| project.status_name_id.as_deref() == Some(status.id.as_str()))
        .map(|project| project.title.clone())
        .collect()
}

fn status_row(name: &ProjectStatusName, projects: &[Project]) -> ExternalWorkProjectStatus {
    let shown = projects
        .iter()
        .filter(|project| {
            project.trashed_at_ms.is_none()
                && project.status_name_id.as_deref() == Some(name.id.as_str())
        })
        .count();
    ExternalWorkProjectStatus {
        id: name.id.clone(),
        name: name.name.clone(),
        category: name.category,
        position: name.position,
        projects: u32::try_from(shown).unwrap_or(u32::MAX),
    }
}

/// The details and summary a save writes: the flags first, then each
/// `--patch-json` edit on top of them or of what is saved.
fn patched(
    args: &ProjectSaveArgs,
    existing: Option<&Project>,
) -> Result<(Option<String>, Option<String>), ExternalError> {
    let mut description = args.details.clone();
    let mut summary = args.summary.as_deref().map(|value| {
        if cleared(value) {
            String::new()
        } else {
            value.trim().to_owned()
        }
    });
    let Some(json) = &args.patch_json else {
        return Ok((description, summary));
    };
    let entries: Vec<super::save::PatchEntry> = serde_json::from_str(json).map_err(|error| {
        ExternalError::invalid(format!(
            "--patch-json must be a JSON list of {{field, op, value, anchor}}: {error}."
        ))
    })?;
    if entries.is_empty() {
        return Err(ExternalError::invalid("--patch-json names no change."));
    }
    for (at, entry) in entries.iter().enumerate() {
        let (slot, saved) = match entry.field.as_str() {
            "description" => (
                &mut description,
                existing.and_then(|project| project.description.clone()),
            ),
            "summary" => (
                &mut summary,
                existing.and_then(|project| project.summary.clone()),
            ),
            other => {
                return Err(ExternalError::invalid(format!(
                    "patch[{at}]: a project patch takes description or summary, not {other:?}."
                )))
            }
        };
        let current = match slot.as_deref() {
            Some(text) => (!text.is_empty()).then(|| text.to_owned()),
            None => saved,
        };
        *slot = Some(super::save::rewritten(at, entry, current)?.unwrap_or_default());
    }
    Ok((description, summary))
}

impl Tracker<'_> {
    fn status_names(&self) -> Result<Vec<ProjectStatusName>, ExternalError> {
        planning::history(self.history, |conn| planning::catalog::status_names(conn))
            .map_err(|message| super::refusal(self.history, message))
    }

    fn status_named(&self, args: &ProjectSaveArgs) -> Result<Option<Option<String>>, ExternalError> {
        let Some(value) = args.status_name.as_deref() else {
            return Ok(None);
        };
        if cleared(value) {
            return Ok(Some(None));
        }
        let names = self.status_names()?;
        let name = pick_status(value, &names)?;
        if let Some(status) = args.status.map(ProjectStatus::from) {
            if status != name.category {
                return Err(ExternalError::invalid(format!(
                    "{} is a kind of {:?}; leave out --status or pick a {:?} status.",
                    name.name, name.category, status
                )));
            }
        }
        Ok(Some(Some(name.id.clone())))
    }

    pub(super) fn project_statuses(&self) -> Result<WorkAnswer, ExternalError> {
        let projects = self.every_project()?;
        Ok(admin(ProjectAdminAnswer::Statuses(
            ExternalWorkProjectStatuses {
                schema_version: QUERY_SCHEMA_VERSION,
                statuses: self
                    .status_names()?
                    .iter()
                    .map(|name| status_row(name, &projects))
                    .collect(),
            },
        )))
    }

    pub(super) fn project_status_save(
        &self,
        args: &ProjectStatusSaveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let name = args.name.trim();
        let names = self.status_names()?;
        let existing = match &args.status {
            Some(value) => Some(pick_status(value, &names)?),
            None => names
                .iter()
                .find(|status| status.name.to_lowercase() == name.to_lowercase()),
        };
        if let Some(taken) = names.iter().find(|status| {
            status.name.to_lowercase() == name.to_lowercase()
                && existing.is_none_or(|existing| existing.id != status.id)
        }) {
            return Err(ExternalError::invalid(format!(
                "A status called {:?} already exists.",
                taken.name
            )));
        }
        let category = match (args.category, existing) {
            (Some(category), _) => category.into(),
            (None, Some(existing)) => existing.category,
            (None, None) => {
                return Err(ExternalError::invalid(
                    "A new status needs --category: backlog, planned, active, paused, completed or canceled.",
                ))
            }
        };
        let projects = self.every_project()?;
        let change = |dry_run, operation_id, status: &ProjectStatusName, changed_projects| {
            admin(ProjectAdminAnswer::Status(ExternalWorkProjectStatusChange {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run,
                operation_id,
                created: existing.is_none(),
                deleted: false,
                status: status_row(status, &projects),
                changed_projects,
            }))
        };
        if let Some(current) = existing {
            if current.name == name && current.category == category {
                return Ok(change(args.dry_run, None, current, Vec::new()));
            }
        }
        let moved = existing
            .filter(|current| current.category != category)
            .map(|current| using(&projects, current))
            .unwrap_or_default();
        let operation_id = match existing {
            Some(current) => args.write.operation_id(
                "project-status-save",
                &(&current.id, &current.name, current.category, category, name),
            )?,
            None => args.write.fresh_operation_id(),
        };
        let id = existing.map(|status| status.id.clone());
        let saved = if args.dry_run {
            self.rehearse(|conn| {
                planning::catalog::save_status_name(conn, id.as_deref(), category, name, now_ms())
            })?
            .0
        } else {
            self.applies("saving a project status")?;
            work::catalog_status_name_save(self.app, &operation_id, id, category, name)
                .map_err(|message| self.write_refusal(message, &operation_id, "The status"))?
        };
        Ok(change(args.dry_run, Some(operation_id), &saved, moved))
    }

    pub(super) fn project_status_delete(
        &self,
        args: &ProjectStatusDeleteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let names = self.status_names()?;
        let status = pick_status(&args.status, &names)?;
        let projects = self.every_project()?;
        let operation_id = args
            .write
            .operation_id("project-status-delete", &status.id)?;
        if args.dry_run {
            self.rehearse(|conn| {
                planning::catalog::delete_status_name(conn, &status.id, now_ms())
            })?;
        } else {
            self.applies("deleting a project status")?;
            work::catalog_status_name_delete(self.app, &operation_id, &status.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The status"))?;
        }
        Ok(admin(ProjectAdminAnswer::Status(
            ExternalWorkProjectStatusChange {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: args.dry_run,
                operation_id: Some(operation_id),
                created: false,
                deleted: true,
                status: status_row(status, &projects),
                changed_projects: using(&projects, status),
            },
        )))
    }

    pub(super) async fn project_history(
        &self,
        args: &ProjectHistoryArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let projects = self.every_project()?;
        let project = Self::pick_project(&args.project, &projects)?;
        let limit = usize::from(args.limit.unwrap_or(50));
        let activity = planning::history(self.history, |conn| {
            planning::catalog::history(conn, &project.id, limit)
        })
        .map_err(|message| super::refusal(self.history, message))?;
        let people = |value: &str| {
            value
                .split(',')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(|id| match id {
                    "me" => "you".to_owned(),
                    id => board
                        .people
                        .iter()
                        .find(|person| person.id == id)
                        .map_or_else(|| id.to_owned(), |person| person.name.clone()),
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let shown = |kind: &str, value: Option<String>| {
            value.map(|value| match kind {
                "lead" | "members" => people(&value),
                "startDate" | "targetDate" | "milestoneAdded" | "milestoneRemoved"
                | "milestoneTarget" => value
                    .parse::<i64>()
                    .map_or(value.clone(), |ms| day_text(Some(ms))),
                "priority" => value
                    .parse::<u8>()
                    .map_or(value.clone(), |priority| Priority::of(priority).text().to_owned()),
                _ => value,
            })
        };
        Ok(admin(ProjectAdminAnswer::History(
            ExternalWorkProjectHistory {
                schema_version: QUERY_SCHEMA_VERSION,
                project: ExternalWorkPersonRef {
                    id: project.id.clone(),
                    name: project.title.clone(),
                },
                changes: activity
                    .into_iter()
                    .map(|change| ExternalWorkProjectChangeEntry {
                        from: shown(&change.kind, change.from_value),
                        to: shown(&change.kind, change.to_value),
                        kind: change.kind,
                        subject: change.subject,
                        at: super::local_time(change.at_ms),
                        at_ms: change.at_ms,
                    })
                    .collect(),
            },
        )))
    }

    pub(super) async fn milestone(&self, args: &MilestoneArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let projects = self.every_project()?;
        let project = Self::pick_project(&args.project, &projects)?;
        let milestones = self.milestones_of(project)?;
        let milestone = pick_milestone(&args.milestone, &milestones)?;
        Ok(admin(ProjectAdminAnswer::Milestone(
            ExternalWorkMilestoneDetail {
                schema_version: QUERY_SCHEMA_VERSION,
                project: ExternalWorkPersonRef {
                    id: project.id.clone(),
                    name: project.title.clone(),
                },
                milestone: Self::milestone_row(milestone, &board),
                tasks: board
                    .rows
                    .iter()
                    .filter(|row| {
                        row.item.milestone_id.as_deref() == Some(milestone.id.as_str())
                            && row.item.archived_at_ms.is_none()
                    })
                    .map(|row| ExternalWorkMilestoneTask {
                        id: row.item.id.clone(),
                        handle: super::handle(row.item.number),
                        title: row.item.title.clone(),
                        status: row.item.status,
                    })
                    .collect(),
            },
        )))
    }

    pub(super) async fn milestone_to_project(
        &self,
        args: &MilestoneToProjectArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let projects = self.every_project()?;
        let project = Self::pick_project(&args.project, &projects)?;
        let milestones = self.milestones_of(project)?;
        let milestone = pick_milestone(&args.milestone, &milestones)?;
        if let Some(taken) = projects
            .iter()
            .find(|project| project.title.to_lowercase() == milestone.title.to_lowercase())
        {
            return Err(ExternalError::invalid(format!(
                "A project called {:?} already exists; rename the milestone first.",
                taken.title
            )));
        }
        let moved = board
            .rows
            .iter()
            .filter(|row| row.item.milestone_id.as_deref() == Some(milestone.id.as_str()))
            .count();
        let operation_id = args
            .write
            .operation_id("milestone-to-project", &(&milestone.id, milestone.revision))?;
        let made = if args.dry_run {
            self.rehearse(|conn| {
                planning::catalog::milestone_to_project(conn, &milestone.id, now_ms())
            })?
            .0
        } else {
            self.applies("turning a milestone into a project")?;
            work::catalog_milestone_to_project(self.app, &operation_id, &milestone.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The milestone"))?
        };
        let board = if args.dry_run {
            board
        } else {
            self.board().await?
        };
        Ok(admin(ProjectAdminAnswer::Converted(
            ExternalWorkMilestoneConverted {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: args.dry_run,
                operation_id,
                from_project: ExternalWorkPersonRef {
                    id: project.id.clone(),
                    name: project.title.clone(),
                },
                project: self.project_row(&made, &board),
                moved_tasks: u32::try_from(moved).unwrap_or(u32::MAX),
            },
        )))
    }
}

pub(super) fn admin_text(answer: &ProjectAdminAnswer, text: &mut String) {
    let dry = |text: &mut String, dry_run: bool| {
        if dry_run {
            text.push_str("Dry run. ");
        }
    };
    match answer {
        ProjectAdminAnswer::Statuses(list) => {
            if list.statuses.is_empty() {
                text.push_str(
                    "No custom statuses. Make one with: sona work project-status-save NAME --category active\n",
                );
            }
            for status in &list.statuses {
                let _ = writeln!(
                    text,
                    "{}  · kind of {:?} · {} projects",
                    status.name, status.category, status.projects
                );
            }
        }
        ProjectAdminAnswer::Status(change) => {
            dry(text, change.dry_run);
            let verb = match (change.deleted, change.created, &change.operation_id) {
                (true, _, _) => "Deleted",
                (false, true, _) => "Made",
                (false, false, None) => "Unchanged",
                (false, false, Some(_)) => "Saved",
            };
            let _ = writeln!(
                text,
                "{verb} status {} (kind of {:?}).",
                change.status.name, change.status.category
            );
            if !change.changed_projects.is_empty() {
                let _ = writeln!(text, "Projects changed: {}.", change.changed_projects.join(", "));
            }
        }
        ProjectAdminAnswer::History(history) => {
            if history.changes.is_empty() {
                let _ = writeln!(text, "{} has no recorded changes yet.", history.project.name);
            }
            for change in &history.changes {
                let _ = write!(text, "{}  {}", change.at, change.kind);
                if let Some(subject) = &change.subject {
                    let _ = write!(text, " {subject:?}");
                }
                let _ = writeln!(
                    text,
                    ": {} → {}",
                    change.from.as_deref().unwrap_or("none"),
                    change.to.as_deref().unwrap_or("none")
                );
            }
        }
        ProjectAdminAnswer::Milestone(detail) => {
            let milestone = &detail.milestone;
            let _ = write!(
                text,
                "{} in {}  · {:?} · {} open, {} done",
                milestone.name,
                detail.project.name,
                milestone.status,
                milestone.open_tasks,
                milestone.done_tasks
            );
            if let Some(target) = &milestone.target {
                let _ = write!(text, " · target {target}");
            }
            text.push('\n');
            if let Some(details) = &milestone.details {
                let _ = writeln!(text, "{details}");
            }
            for task in &detail.tasks {
                let _ = writeln!(text, "{}  {}  · {:?}", task.handle, task.title, task.status);
            }
        }
        ProjectAdminAnswer::Converted(converted) => {
            dry(text, converted.dry_run);
            let _ = writeln!(
                text,
                "Turned milestone {} of {} into a project; {} tasks moved.",
                converted.project.name, converted.from_project.name, converted.moved_tasks
            );
        }
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::Projects(projects) => {
            if projects.projects.is_empty() {
                text.push_str("No projects yet. Make one with: sona work project-save NAME\n");
            }
            for project in &projects.projects {
                let _ = write!(
                    text,
                    "{}  · {} · {} open, {} done",
                    project.name,
                    project
                        .status_name
                        .clone()
                        .unwrap_or_else(|| format!("{:?}", project.status)),
                    project.open_tasks,
                    project.done_tasks
                );
                if project.lead_is_self {
                    text.push_str(" · lead you");
                } else if let Some(lead) = &project.lead {
                    let _ = write!(text, " · lead {}", lead.name);
                }
                if let Some(target) = &project.target {
                    let _ = write!(text, " · target {target}");
                }
                if let Some(initiative) = &project.initiative {
                    let _ = write!(text, " · in {}", initiative.name);
                }
                if project.archived {
                    text.push_str(" · put away");
                }
                text.push('\n');
            }
        }
        WorkAnswer::Project(change) => {
            let verb = match (change.created, &change.operation_id) {
                (true, _) => "Made",
                (false, None) => "Unchanged",
                (false, Some(_)) => "Saved",
            };
            if change.dry_run {
                text.push_str("Dry run. ");
            }
            let _ = writeln!(text, "{verb} project {}.", change.project.name);
            if let Some(trashed) = change.project.trashed_at_ms {
                let _ = writeln!(
                    text,
                    "In the bin; deleted for good on {}.",
                    super::day_text(Some(trashed + planning::catalog::BIN_MS))
                );
            }
        }
        WorkAnswer::Milestones(milestones) => {
            if milestones.milestones.is_empty() {
                let _ = writeln!(text, "{} has no milestones.", milestones.project.name);
            }
            for milestone in &milestones.milestones {
                let _ = write!(
                    text,
                    "{}  · {:?} · {} open, {} done",
                    milestone.name, milestone.status, milestone.open_tasks, milestone.done_tasks
                );
                if let Some(target) = &milestone.target {
                    let _ = write!(text, " · target {target}");
                }
                text.push('\n');
            }
        }
        WorkAnswer::Milestone(change) => {
            let verb = match (change.deleted, change.created, &change.operation_id) {
                (true, _, _) => "Deleted",
                (false, true, _) => "Made",
                (false, false, None) => "Unchanged",
                (false, false, Some(_)) => "Saved",
            };
            if change.dry_run {
                text.push_str("Dry run. ");
            }
            let _ = writeln!(
                text,
                "{verb} milestone {} in {}.",
                change.milestone.name, change.project.name
            );
        }
        WorkAnswer::People(people) => {
            if people.people.is_empty() {
                text.push_str("Nobody yet. Add someone with: sona work person-save NAME\n");
            }
            for person in &people.people {
                let _ = write!(text, "{}  · {} open", person.name, person.open_tasks);
                if !person.aliases.is_empty() {
                    let _ = write!(text, " · also {}", person.aliases.join(", "));
                }
                if let Some(role) = &person.role {
                    let _ = write!(text, " · {role}");
                }
                if let Some(organization) = &person.organization {
                    let _ = write!(text, " · {organization}");
                }
                text.push('\n');
            }
        }
        WorkAnswer::Person(change) => {
            if let Some(from) = &change.merged_from {
                let (verb, moved) = if change.dry_run {
                    ("Would merge", "would move")
                } else {
                    ("Merged", "moved")
                };
                let _ = writeln!(
                    text,
                    "{verb} {from} into {}; {} tasks {moved}.",
                    change.person.name, change.moved_tasks
                );
                if change.dry_run {
                    let person = &change.person;
                    if !person.aliases.is_empty() {
                        let _ = writeln!(text, "  Also: {}", person.aliases.join(", "));
                    }
                    if !person.emails.is_empty() {
                        let _ = writeln!(text, "  Email: {}", person.emails.join(", "));
                    }
                    let _ = writeln!(text, "  Meetings: {}", person.meetings);
                }
            } else {
                let verb = match (change.created, &change.operation_id) {
                    (true, _) => "Added",
                    (false, None) => "Unchanged",
                    (false, Some(_)) => "Saved",
                };
                if change.dry_run {
                    text.push_str("Dry run. ");
                }
                let _ = writeln!(text, "{verb} {}.", change.person.name);
            }
        }
        WorkAnswer::Meetings(meetings) => {
            if meetings.meetings.is_empty() {
                text.push_str("No meetings in that range.\n");
            }
            for meeting in &meetings.meetings {
                let _ = write!(text, "{}  {}  {}", meeting.id, meeting.start, meeting.title);
                if let Some(duration) = meeting.duration_ms {
                    let _ = write!(text, " · {} min", duration / 60_000);
                }
                let _ = write!(
                    text,
                    " · {}",
                    if meeting.processed {
                        "processed"
                    } else {
                        "not processed"
                    }
                );
                if !meeting.people.is_empty() {
                    let _ = write!(text, " · {}", meeting.people.join(", "));
                }
                text.push('\n');
            }
            if meetings.has_more {
                text.push_str("More follow. Raise --limit or move --since later.\n");
            }
        }
        WorkAnswer::MeetingDetail(detail) => {
            let meeting = &detail.meeting;
            let _ = writeln!(text, "{}  ({})", meeting.title, meeting.id);
            if !detail.people.is_empty() {
                let _ = writeln!(text, "People: {}", detail.people.join(", "));
            }
            super::speakers::listed_text(&detail.meeting_speakers, text);
            if let Some(summary) = &meeting.summary {
                let _ = writeln!(text, "\n{summary}");
            }
            if !meeting.loops.is_empty() {
                text.push_str("\nCommitments:\n");
                for row in &meeting.loops {
                    let _ = writeln!(
                        text,
                        "- {}{}",
                        row.text,
                        row.owner
                            .as_ref()
                            .map_or_else(String::new, |owner| format!(" ({owner})"))
                    );
                }
            }
            if let Some(transcript) = &detail.transcript {
                text.push_str("\nTranscript:\n");
                for line in &transcript.lines {
                    let _ = writeln!(text, "{}: {}", line.speaker, line.text);
                }
            }
        }
        _ => {}
    }
}


pub(super) async fn merge_people(
    meetings: &Arc<MeetingSessionManager>,
    from: PersonId,
    into: PersonId,
    voice: Option<MergeVoice>,
    dry_run: bool,
) -> Result<PersonListEntry, ExternalError> {
    let store = store(meetings).await?;
    let voice = voice.map(MergeVoice::resolution);
    if dry_run {
        return store
            .person_merge_preview(from, into, voice, now_ms())
            .map_err(|error| merge_refusal(map_store_error(error)));
    }
    let revision = store.people_list().map_err(QueryError::from)?.revision;
    meetings
        .person_merge(PersonMergeRequest {
            source_person_id: from,
            target_person_id: into,
            expected_revision: revision,
            voice_profile_resolution: voice,
        })
        .await
        .map_err(merge_refusal)?;
    store
        .people_list()
        .map_err(QueryError::from)?
        .entries
        .into_iter()
        .find(|entry| entry.person.id == into)
        .ok_or_else(|| ExternalError::new(ExternalErrorCode::Failed, "The merge did not finish."))
}

fn merge_refusal(error: MeetingCommandError) -> ExternalError {
    match error {
        MeetingCommandError::ProfileMergeResolutionRequired => ExternalError::invalid(
            "A saved voice is involved. Pass --voice into, from or both to choose which one stays.",
        ),
        MeetingCommandError::ProfileModelIncompatible => ExternalError::invalid(
            "The two saved voices cannot be combined. Pass --voice into or from to keep one.",
        ),
        error => ExternalError::invalid(format!("The merge was refused ({error:?}).")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Fence, WorkCommand, WorkVerb};
    use super::*;
    use crate::cli::{CliArgs, Command};
    use crate::planning::schema;
    use crate::query::KeptLocal;
    use clap::Parser;
    use rusqlite::Connection;
    use std::collections::{BTreeMap, HashMap, HashSet};

    fn project_save(argv: &[&str]) -> ProjectSaveArgs {
        let argv = ["sona", "work", "project-save", "Harbor"].iter().chain(argv);
        match CliArgs::try_parse_from(argv) {
            Ok(CliArgs {
                command:
                    Some(Command::Work(WorkCommand {
                        verb: WorkVerb::ProjectSave(args),
                        ..
                    })),
                ..
            }) => args,
            other => panic!("`work project-save` parsed as {other:?}"),
        }
    }

    /// The user is nobody on the People page, so `me` leads or joins a
    /// project as the user, and `nobody` takes the user off as lead.
    #[test]
    fn me_leads_and_joins_a_project_and_nobody_clears_the_lead() {
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let board = Board {
            rows: Vec::new(),
            labels: Vec::new(),
            projects: Vec::new(),
            people: vec![WorkPersonChoice {
                id: "dana".to_owned(),
                name: "Dana Reyes".to_owned(),
                aliases: Vec::new(),
            }],
            suggestions: BTreeMap::new(),
            focus: Vec::new(),
            repeats: HashMap::new(),
            fence: Fence::Open(KeptLocal(HashSet::new())),
            cycles: Vec::new(),
        };
        let edit = |argv: &[&str]| {
            let mut edit = board
                .project_people(&project_save(argv))
                .expect("the people resolve");
            edit.title = Some("Harbor".to_owned());
            edit
        };
        let save = |id: Option<&str>, edit: &ProjectEdit| {
            planning::catalog::save_project(&conn, id, edit, 1).expect("the project saves")
        };

        let dry = project_save(&["--lead", "me", "--members", "me,Dana", "--dry-run"]);
        assert!(dry.dry_run);
        let rehearsal = conn.unchecked_transaction().expect("a rehearsal");
        save(None, &edit(&["--lead", "me", "--members", "me,Dana"]));
        drop(rehearsal);
        assert!(plans::all_projects(&conn).expect("the projects").is_empty());

        let mine = edit(&["--lead", "me", "--members", "me,Dana"]);
        let project = save(None, &mine);
        assert!(project.lead_is_self);
        assert_eq!(project.lead_person_id, None);
        assert!(project.members_include_self);
        assert_eq!(project.member_person_ids, ["dana"]);
        assert!(!changes_project(&project, &mine));

        let project = save(Some(&project.id), &edit(&["--lead", "Dana"]));
        assert!(!project.lead_is_self);
        assert_eq!(project.lead_person_id.as_deref(), Some("dana"));

        let project = save(Some(&project.id), &edit(&["--lead", "me"]));
        assert!(project.lead_is_self);
        assert_eq!(project.lead_person_id, None);

        let project = save(Some(&project.id), &edit(&["--lead", "nobody"]));
        assert!(!project.lead_is_self);
        assert_eq!(project.lead_person_id, None);
        assert!(project.members_include_self);
        assert_eq!(project.member_person_ids, ["dana"]);
    }

    fn scopes(argv: &[&str]) -> &'static [crate::query::external::ExternalScope] {
        match CliArgs::try_parse_from(["sona", "work"].iter().chain(argv)) {
            Ok(CliArgs {
                command: Some(Command::Work(WorkCommand { verb, .. })),
                ..
            }) => verb.required_scopes(),
            other => panic!("`work {argv:?}` parsed as {other:?}"),
        }
    }

    /// A dry run of putting a project away or saving a person needs only
    /// read access, shows the result and keeps nothing; the real run keeps it.
    #[test]
    fn a_dry_run_archive_or_person_save_keeps_nothing_until_the_real_run() {
        use super::super::roadmap::rehearsed;
        use crate::meeting::store::workflow_core_tests;
        use crate::planning::store as plans;
        use crate::query::external::{READ_SCOPES, WRITE_SCOPES};

        assert_eq!(
            scopes(&["project-archive", "Harbor", "--dry-run"]),
            READ_SCOPES
        );
        assert_eq!(
            scopes(&["person-save", "Dana Reyes", "--dry-run"]),
            READ_SCOPES
        );
        assert_eq!(scopes(&["project-archive", "Harbor"]), WRITE_SCOPES);
        assert_eq!(scopes(&["person-save", "Dana Reyes"]), WRITE_SCOPES);

        let mut conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let harbor = ProjectEdit {
            title: Some("Harbor".to_owned()),
            ..ProjectEdit::default()
        };
        let project = planning::catalog::save_project(&conn, None, &harbor, 1).expect("Harbor");
        let put_away = ProjectEdit {
            archived: Some(true),
            ..ProjectEdit::default()
        };
        let shown = rehearsed(&mut conn, |tx| {
            planning::catalog::save_project(tx, Some(&project.id), &put_away, 2)
        })
        .expect("a rehearsed archive");
        assert!(shown.archived_at_ms.is_some());
        let kept = plans::project(&conn, &project.id).expect("Harbor after the dry run");
        assert_eq!(kept.archived_at_ms, None);
        planning::catalog::save_project(&conn, Some(&project.id), &put_away, 3).expect("archive");
        let kept = plans::project(&conn, &project.id).expect("Harbor after the real run");
        assert!(kept.archived_at_ms.is_some());

        let (_directory, store) = workflow_core_tests::store();
        let (shown, changed) = store
            .person_profile_preview(
                None,
                "Dana Reyes".to_owned(),
                PersonProfileEdit::default(),
                1,
            )
            .expect("a previewed person");
        assert!(changed);
        assert_eq!(shown.person.display_name, "Dana Reyes");
        assert!(store.people_list().expect("people").entries.is_empty());
        let made = store
            .create_person_profile("Dana Reyes".to_owned(), PersonProfileEdit::default(), 2)
            .expect("Dana");
        let dana = made.person.expect("Dana's profile").id;
        let lead = PersonProfileEdit {
            role: Some("Lead".to_owned()),
            ..PersonProfileEdit::default()
        };
        let role = |store: &crate::meeting::store::MeetingStore| {
            store.people_list().expect("people").entries[0]
                .person
                .role
                .clone()
        };
        let (shown, changed) = store
            .person_profile_preview(Some(dana), String::new(), lead.clone(), 3)
            .expect("a previewed role");
        assert!(changed);
        assert_eq!(shown.person.role.as_deref(), Some("Lead"));
        assert_eq!(role(&store), None);
        store
            .update_person_profile(dana, lead, 4)
            .expect("Dana's role");
        assert_eq!(role(&store).as_deref(), Some("Lead"));
    }
}
