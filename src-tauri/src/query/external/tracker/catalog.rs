//! `sona work` verbs for the Projects and People pages, and the read-only
//! meetings feed: what an organizer agent needs to keep the tracker tidy.

use super::{
    conversations_closed, day_or_none, day_text, pick, Board, DayArg, Tracker, WorkAnswer,
    WorkPersonChoice, WriteArgs,
};
use crate::meeting::people_types::{PersonListEntry, PersonMergeRequest};
use crate::meeting::store::PersonProfileEdit;
use crate::meeting::types::{MeetingPhase, ProcessingStatus};
use crate::planning;
use crate::planning::catalog::{MilestoneEdit, ProjectEdit};
use crate::planning::store as plans;
use crate::planning::work;
use crate::planning::work::types::WorkStatus;
use crate::planning::{Milestone, Project, ProjectStatus, TaskStatus};
use crate::query::external::{
    meeting_detail, meeting_summaries, session_id, store, transcript, ExternalError,
    ExternalErrorCode, ExternalMeetingDetail, ExternalTranscript,
};
use crate::query::{QueryError, QUERY_SCHEMA_VERSION};
use chrono::NaiveDate;
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::fmt::Write as _;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ProjectState {
    Active,
    Paused,
    Completed,
}

impl From<ProjectState> for ProjectStatus {
    fn from(state: ProjectState) -> Self {
        match state {
            ProjectState::Active => Self::Active,
            ProjectState::Paused => Self::Paused,
            ProjectState::Completed => Self::Completed,
        }
    }
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
    /// A person from the People page, or "none".
    #[arg(long, value_name = "PERSON")]
    pub lead: Option<String>,
    /// Comma-separated people; an empty value clears the list.
    #[arg(long, value_delimiter = ',', value_name = "PEOPLE")]
    pub members: Option<Vec<String>>,
    /// YYYY-MM-DD, today, tomorrow or none.
    #[arg(long, value_parser = day_or_none, value_name = "DAY")]
    pub target: Option<DayArg>,
    /// The project's colour, as a hue in degrees (0 to 360).
    #[arg(long, value_parser = clap::value_parser!(u16).range(0..=360))]
    pub hue: Option<u16>,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectArchiveArgs {
    pub project: String,
    /// Bring a put-away project back.
    #[arg(long)]
    pub restore: bool,
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
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MilestoneDeleteArgs {
    pub project: String,
    pub milestone: String,
    #[command(flatten)]
    pub write: WriteArgs,
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
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MeetingsArgs {
    /// Only meetings from then on: milliseconds since the epoch, or YYYY-MM-DD.
    #[arg(long, value_parser = since, value_name = "WHEN")]
    pub since: Option<SinceArg>,
    #[arg(long)]
    pub limit: Option<usize>,
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
    pub status: ProjectStatus,
    pub lead: Option<ExternalWorkPersonRef>,
    pub members: Vec<ExternalWorkPersonRef>,
    pub target: Option<String>,
    pub target_at_ms: Option<i64>,
    pub hue: Option<u16>,
    pub archived: bool,
    pub open_tasks: u32,
    pub done_tasks: u32,
    pub milestones: u32,
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
    pub operation_id: Option<String>,
    pub created: bool,
    pub deleted: bool,
    pub project: ExternalWorkPersonRef,
    pub milestone: ExternalWorkMilestone,
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

fn cleared(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || value.eq_ignore_ascii_case("none")
}

fn is_open(status: WorkStatus) -> bool {
    matches!(
        status,
        WorkStatus::Backlog | WorkStatus::NotStarted | WorkStatus::InProgress
    )
}

impl Tracker<'_> {
    fn every_project(&self) -> Result<Vec<Project>, ExternalError> {
        planning::history(self.history, |conn| plans::all_projects(conn))
            .map_err(|message| super::refusal(self.history, message))
    }

    fn project_row(&self, project: &Project, board: &Board) -> ExternalWorkProject {
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
        let milestones = planning::history(self.history, |conn| {
            Ok(plans::records(conn, &project.id)?.milestones.len())
        })
        .unwrap_or_default();
        ExternalWorkProject {
            id: project.id.clone(),
            name: project.title.clone(),
            details: project.description.clone(),
            status: project.status,
            lead: project.lead_person_id.as_ref().and_then(person),
            members: project
                .member_person_ids
                .iter()
                .filter_map(person)
                .collect(),
            target: target_text(project.target_at_ms),
            target_at_ms: project.target_at_ms,
            hue: project.hue,
            archived: project.archived_at_ms.is_some(),
            open_tasks: open,
            done_tasks: done,
            milestones: u32::try_from(milestones).unwrap_or(u32::MAX),
            revision: project.revision,
        }
    }

    fn pick_project<'a>(
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
        projects.retain(|project| args.all || project.archived_at_ms.is_none());
        projects.sort_by_key(|project| project.title.to_lowercase());
        Ok(WorkAnswer::Projects(ExternalWorkProjects {
            schema_version: QUERY_SCHEMA_VERSION,
            projects: projects
                .iter()
                .map(|project| self.project_row(project, &board))
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
        let lead = match &args.lead {
            Some(value) if cleared(value) => Some(String::new()),
            Some(value) => Some(board.person(value)?.id.clone()),
            None => None,
        };
        let members = match &args.members {
            Some(values) => {
                let mut ids = Vec::new();
                for value in words(values) {
                    let id = board.person(&value)?.id.clone();
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                ids.sort();
                Some(ids)
            }
            None => None,
        };
        let edit = ProjectEdit {
            title: (existing.is_none() || args.project.is_some()).then(|| name.to_owned()),
            description: args.details.clone(),
            status: args.status.map(Into::into),
            lead_person_id: lead,
            member_person_ids: members,
            target_at_ms: args.target.map(DayArg::ms).transpose()?,
            hue: args.hue,
            archived: None,
        };
        if let Some(current) = existing {
            if !changes_project(current, &edit) {
                return Ok(WorkAnswer::Project(ExternalWorkProjectChange {
                    schema_version: QUERY_SCHEMA_VERSION,
                    operation_id: None,
                    created: false,
                    project: self.project_row(current, &board),
                }));
            }
        }
        let operation_id = match existing {
            Some(current) => args
                .write
                .operation_id("project-save", &(&current.id, current.revision, &edit))?,
            None => args.write.fresh_operation_id(),
        };
        self.applies("saving a project")?;
        let project = work::catalog_project_save(
            self.app,
            &operation_id,
            existing.map(|project| project.id.clone()),
            &edit,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The project"))?;
        Ok(WorkAnswer::Project(ExternalWorkProjectChange {
            schema_version: QUERY_SCHEMA_VERSION,
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
            self.applies("putting a project away")?;
            let project = work::catalog_project_save(
                self.app,
                &operation_id,
                Some(current.id.clone()),
                &edit,
            )
            .map_err(|message| self.write_refusal(message, &operation_id, "The project"))?;
            return Ok(WorkAnswer::Project(ExternalWorkProjectChange {
                schema_version: QUERY_SCHEMA_VERSION,
                operation_id: Some(operation_id),
                created: false,
                project: self.project_row(&project, &board),
            }));
        }
        Ok(WorkAnswer::Project(ExternalWorkProjectChange {
            schema_version: QUERY_SCHEMA_VERSION,
            operation_id: None,
            created: false,
            project: self.project_row(current, &board),
        }))
    }

    fn milestone_row(milestone: &Milestone, board: &Board) -> ExternalWorkMilestone {
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

    fn milestones_of(&self, project: &Project) -> Result<Vec<Milestone>, ExternalError> {
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
        if let Some(current) = existing {
            if !changes_milestone(current, &edit) {
                return Ok(WorkAnswer::Milestone(ExternalWorkMilestoneChange {
                    schema_version: QUERY_SCHEMA_VERSION,
                    operation_id: None,
                    created: false,
                    deleted: false,
                    project: ExternalWorkPersonRef {
                        id: project.id.clone(),
                        name: project.title.clone(),
                    },
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
        self.applies("saving a milestone")?;
        let milestone = work::catalog_milestone_save(
            self.app,
            &operation_id,
            &project.id,
            existing.map(|milestone| milestone.id.clone()),
            &edit,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The milestone"))?;
        Ok(WorkAnswer::Milestone(ExternalWorkMilestoneChange {
            schema_version: QUERY_SCHEMA_VERSION,
            operation_id: Some(operation_id),
            created: existing.is_none(),
            deleted: false,
            project: ExternalWorkPersonRef {
                id: project.id.clone(),
                name: project.title.clone(),
            },
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
        self.applies("deleting a milestone")?;
        work::catalog_milestone_delete(self.app, &operation_id, &milestone.id)
            .map_err(|message| self.write_refusal(message, &operation_id, "The milestone"))?;
        Ok(WorkAnswer::Milestone(ExternalWorkMilestoneChange {
            schema_version: QUERY_SCHEMA_VERSION,
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
        let before = store.people_list().map_err(QueryError::from)?;
        self.applies("saving a person")?;
        let created = existing.is_none();
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
        self.applies("merging people")?;
        let revision = store.people_list().map_err(QueryError::from)?.revision;
        self.meetings
            .person_merge(PersonMergeRequest {
                source_person_id: parse_person(&from.id)?,
                target_person_id: parse_person(&into.id)?,
                expected_revision: revision,
                voice_profile_resolution: None,
            })
            .await
        .map_err(|error| {
            ExternalError::invalid(format!(
                "The merge was refused ({error:?}). When both people have a voice profile, merge them on Sona's People page to choose which one to keep."
            ))
        })?;
        work::catalog_person_repoint(self.app, &operation_id, &from.id, &into.id)
            .map_err(|message| self.write_refusal(message, &operation_id, "The tracker"))?;
        let after = store.people_list().map_err(QueryError::from)?;
        let kept = after
            .entries
            .iter()
            .find(|entry| entry.person.id.uuid().to_string() == into.id)
            .ok_or_else(|| {
                ExternalError::new(ExternalErrorCode::Failed, "The merge did not finish.")
            })?;
        let sources = self.person_sources(&store, &into.id)?;
        Ok(WorkAnswer::Person(ExternalWorkPersonChange {
            schema_version: QUERY_SCHEMA_VERSION,
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
        let (entries, has_more) = meeting_summaries(&store, since_ms, None, limit)?;
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

fn pick_milestone<'a>(
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
        || edit.lead_person_id.as_ref().is_some_and(|lead| {
            lead.as_str() != current.lead_person_id.as_deref().unwrap_or_default()
        })
        || edit.member_person_ids.as_ref().is_some_and(|members| {
            let mut now = current.member_person_ids.clone();
            now.sort();
            &now != members
        })
        || edit
            .target_at_ms
            .is_some_and(|target| target != current.target_at_ms)
        || edit.hue.is_some_and(|hue| Some(hue) != current.hue)
        || edit
            .archived
            .is_some_and(|archived| archived != current.archived_at_ms.is_some())
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

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::Projects(projects) => {
            if projects.projects.is_empty() {
                text.push_str("No projects yet. Make one with: sona work project-save NAME\n");
            }
            for project in &projects.projects {
                let _ = write!(
                    text,
                    "{}  · {:?} · {} open, {} done",
                    project.name, project.status, project.open_tasks, project.done_tasks
                );
                if let Some(lead) = &project.lead {
                    let _ = write!(text, " · lead {}", lead.name);
                }
                if let Some(target) = &project.target {
                    let _ = write!(text, " · target {target}");
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
            let _ = writeln!(text, "{verb} project {}.", change.project.name);
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
                let _ = writeln!(
                    text,
                    "Merged {from} into {}; {} tasks moved.",
                    change.person.name, change.moved_tasks
                );
            } else {
                let verb = match (change.created, &change.operation_id) {
                    (true, _) => "Added",
                    (false, None) => "Unchanged",
                    (false, Some(_)) => "Saved",
                };
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
