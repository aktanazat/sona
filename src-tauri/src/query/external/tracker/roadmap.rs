//! `sona work` verbs for project updates and health, a project's progress
//! and forecast, the bin, milestone order, and running cycles.

use super::catalog::ExternalWorkProject;
use super::{
    cycle_ref, external_cycles, named_cycle, now_ms, refusal, ExternalWorkCycles, MeetingFence,
    Tracker, WorkAnswer, WriteArgs,
};
use crate::planning::catalog::ProjectEdit;
use crate::planning::store as plans;
use crate::planning::work::comments::Author;
use crate::planning::work::types::{
    WorkActor, WorkCycleRef, WorkCycleSettings, WorkCycleState, WorkCycles,
};
use crate::planning::work::{self, cycles, plans as project_plans};
use crate::planning::{
    self, catalog, progress, updates, Project, ProjectHealth, ProjectProgress, ProjectUpdate,
};
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use chrono::NaiveDate;
use clap::{Args, ValueEnum};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashSet;
use std::fmt::Write as _;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    #[value(alias = "onTrack")]
    OnTrack,
    #[value(alias = "atRisk")]
    AtRisk,
    #[value(alias = "offTrack")]
    OffTrack,
}

impl Health {
    const fn of(health: ProjectHealth) -> Self {
        match health {
            ProjectHealth::OnTrack => Self::OnTrack,
            ProjectHealth::AtRisk => Self::AtRisk,
            ProjectHealth::OffTrack => Self::OffTrack,
        }
    }

    const fn health(self) -> ProjectHealth {
        match self {
            Self::OnTrack => ProjectHealth::OnTrack,
            Self::AtRisk => ProjectHealth::AtRisk,
            Self::OffTrack => ProjectHealth::OffTrack,
        }
    }

    const fn text(self) -> &'static str {
        match self {
            Self::OnTrack => "On track",
            Self::AtRisk => "At risk",
            Self::OffTrack => "Off track",
        }
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectArgs {
    pub project: String,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectUpdateArgs {
    pub project: String,
    #[arg(long, value_enum)]
    pub health: Health,
    /// Markdown. Left out, Sona drafts it from what moved since the last update.
    #[arg(long)]
    pub body: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

impl ProjectUpdateArgs {
    /// The caller's id, or one derived from the project, the health, the
    /// trimmed text and `day`: a retry that day posts the update once.
    fn operation_id(&self, project_id: &str, day: NaiveDate) -> Result<String, ExternalError> {
        self.write.operation_id(
            "project-update",
            &(
                project_id,
                self.health,
                self.body.as_deref().map(str::trim),
                day.to_string(),
            ),
        )
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectUpdatesArgs {
    pub project: String,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub limit: Option<u32>,
    /// The last update id of the page before.
    #[arg(long, value_name = "UPDATE")]
    pub cursor: Option<Uuid>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectUpdateEditArgs {
    pub update: Uuid,
    #[arg(long, value_enum)]
    pub health: Option<Health>,
    #[arg(long)]
    pub body: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectUpdateDeleteArgs {
    pub update: Uuid,
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectDeleteArgs {
    pub project: String,
    /// Take the project back out of the bin.
    #[arg(long)]
    pub restore: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MilestoneReorderArgs {
    pub project: String,
    /// Every milestone of the project, by name or id, in the new order.
    #[arg(required = true, num_args = 1..)]
    pub milestones: Vec<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CycleSettingsArgs {
    /// How many weeks each cycle runs, 1 to 8.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=8))]
    pub weeks: Option<u8>,
    /// The weekday cycles start on, 1 for Monday through 7 for Sunday.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=7))]
    pub start_weekday: Option<u8>,
    /// Weeks off between cycles, 0 to 4.
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=4))]
    pub cooldown: Option<u8>,
    /// How many cycles to make ahead, 1 to 15.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=15))]
    pub upcoming: Option<u8>,
    /// Whether started work with no cycle joins the cycle under way, or the
    /// next in a cooldown: true or false.
    #[arg(long, value_name = "BOOL")]
    pub auto_add_started: Option<bool>,
    /// Whether done work with no cycle joins the cycle under way, or the
    /// last one in a cooldown: true or false.
    #[arg(long, value_name = "BOOL")]
    pub auto_add_completed: Option<bool>,
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

impl CycleSettingsArgs {
    pub(super) const fn changes_something(&self) -> bool {
        self.weeks.is_some()
            || self.start_weekday.is_some()
            || self.cooldown.is_some()
            || self.upcoming.is_some()
            || self.auto_add_started.is_some()
            || self.auto_add_completed.is_some()
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CycleSaveArgs {
    /// current, next, or a cycle's number.
    #[arg(value_parser = cycle_ref, value_name = "CYCLE")]
    pub cycle: WorkCycleRef,
    /// A name for the cycle; none clears it.
    #[arg(long)]
    pub name: Option<String>,
    /// The first day, YYYY-MM-DD; only a cycle not yet started moves it.
    #[arg(long, value_name = "DAY")]
    pub start: Option<NaiveDate>,
    /// The last day, YYYY-MM-DD.
    #[arg(long, value_name = "DAY")]
    pub end: Option<NaiveDate>,
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CycleRolloverArgs {
    /// current, next, or a cycle's number.
    #[arg(value_parser = cycle_ref, value_name = "CYCLE")]
    pub cycle: WorkCycleRef,
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CycleStartTodayArgs {
    /// The first upcoming cycle; left out, that one.
    #[arg(value_parser = cycle_ref, value_name = "CYCLE")]
    pub cycle: Option<WorkCycleRef>,
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectRef {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalUpdateAuthor {
    pub kind: WorkActor,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectUpdate {
    pub id: String,
    pub health: Health,
    /// Markdown; absent once deleted.
    pub body: Option<String>,
    /// The project's progress, in percent, when the update was written.
    pub progress: u8,
    pub author: ExternalUpdateAuthor,
    pub created_at_ms: i64,
    pub edited_at_ms: Option<i64>,
    pub deleted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectWeek {
    pub week_start_ms: i64,
    pub scope: u32,
    pub started: u32,
    pub done: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectPrediction {
    pub expected_at_ms: i64,
    pub optimistic_at_ms: i64,
    pub pessimistic_at_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectProgress {
    pub scope: u32,
    pub started: u32,
    pub done: u32,
    /// Done counts fully and started a quarter, out of scope.
    pub percent: u8,
    pub weeks: Vec<ExternalProjectWeek>,
    pub prediction: Option<ExternalProjectPrediction>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalMilestoneProgress {
    pub id: String,
    pub name: String,
    pub position: Option<i64>,
    pub scope: u32,
    pub started: u32,
    pub done: u32,
    pub percent: u8,
    /// The first milestone, in order, still open.
    pub next_open: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectDetail {
    pub schema_version: u32,
    pub project: ExternalWorkProject,
    pub latest_update: Option<ExternalProjectUpdate>,
    /// An active project with no update for one cycle and three days.
    pub update_missing: bool,
    pub progress: ExternalProjectProgress,
    pub milestones: Vec<ExternalMilestoneProgress>,
    /// Notes, meetings, dictations and pages linked to the project, its
    /// milestones and open tasks. Meetings kept on this Mac stay out.
    pub sources: Vec<crate::query::external::ExternalPlanningEvidence>,
    /// Projects this one waits on, each marked broken when it ends after
    /// this one starts.
    pub blocked_by: Vec<super::dependencies::ExternalProjectDependencyLink>,
    /// Projects waiting on this one.
    pub blocking: Vec<super::dependencies::ExternalProjectDependencyLink>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectUpdates {
    pub schema_version: u32,
    pub project: ExternalProjectRef,
    pub updates: Vec<ExternalProjectUpdate>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateAction {
    Added,
    Edited,
    Deleted,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectUpdateChange {
    pub schema_version: u32,
    pub action: UpdateAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub project: ExternalProjectRef,
    pub update: ExternalProjectUpdate,
    /// True when this answered from an earlier run of the same operation.
    pub replayed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectDraft {
    pub schema_version: u32,
    pub project: ExternalProjectRef,
    pub body: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalMilestonePlace {
    pub id: String,
    pub name: String,
    pub position: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkMilestoneOrder {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub project: ExternalProjectRef,
    pub milestones: Vec<ExternalMilestonePlace>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleAction {
    Settings,
    Saved,
    RolledOver,
    StartedToday,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkCycleChange {
    pub schema_version: u32,
    pub action: CycleAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub number: Option<i64>,
    pub cycles: ExternalWorkCycles,
}

fn project_ref(project: &Project) -> ExternalProjectRef {
    ExternalProjectRef {
        id: project.id.clone(),
        name: project.title.clone(),
    }
}

/// The update as an agent reads it: without its `discussed` lines unless it
/// is in `quotes_shown`.
fn external_update(
    update: ProjectUpdate,
    quotes_shown: &HashSet<String>,
) -> ExternalProjectUpdate {
    let body = update
        .body
        .as_deref()
        .map(|body| updates::for_agents(&update.id, body, quotes_shown));
    ExternalProjectUpdate {
        id: update.id,
        health: Health::of(update.health),
        body,
        progress: update.progress,
        author: ExternalUpdateAuthor {
            kind: update.author.kind,
            name: update.author.name,
        },
        created_at_ms: update.created_at_ms,
        edited_at_ms: update.edited_at_ms,
        deleted: update.deleted,
    }
}

fn external_progress(progress: ProjectProgress) -> ExternalProjectProgress {
    ExternalProjectProgress {
        scope: progress.scope,
        started: progress.started,
        done: progress.done,
        percent: progress.percent,
        weeks: progress
            .weeks
            .into_iter()
            .map(|week| ExternalProjectWeek {
                week_start_ms: week.week_start_ms,
                scope: week.scope,
                started: week.started,
                done: week.done,
            })
            .collect(),
        prediction: progress
            .prediction
            .map(|prediction| ExternalProjectPrediction {
                expected_at_ms: prediction.expected_at_ms,
                optimistic_at_ms: prediction.optimistic_at_ms,
                pessimistic_at_ms: prediction.pessimistic_at_ms,
            }),
    }
}

fn missing_update(id: &Uuid) -> ExternalError {
    ExternalError::new(
        ExternalErrorCode::NotFound,
        format!("No update {id}. List them with: sona work project-updates PROJECT"),
    )
}

/// Runs `change` in a transaction that is never committed.
pub(super) fn rehearsed<T>(
    conn: &mut Connection,
    change: impl FnOnce(&Connection) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let tx = conn.transaction()?;
    change(&tx)
}

impl Tracker<'_> {
    fn update_author(write: &WriteArgs) -> Author<'_> {
        Author {
            actor: WorkActor::Agent,
            agent: write.agent.as_deref(),
        }
    }

    /// Runs `change` against history and throws it away: what a dry run
    /// would leave, with nothing kept.
    pub(super) fn rehearse<T>(
        &self,
        change: impl FnOnce(&Connection) -> anyhow::Result<T>,
    ) -> Result<T, ExternalError> {
        planning::history(self.history, |conn| rehearsed(conn, change))
            .map_err(|message| refusal(self.history, message))
    }

    fn project_named(&self, value: &str) -> Result<Project, ExternalError> {
        let projects = planning::history(self.history, |conn| plans::all_projects(conn))
            .map_err(|message| refusal(self.history, message))?;
        Self::pick_project(value, &projects).cloned()
    }

    fn update_named(&self, id: &Uuid) -> Result<(ProjectUpdate, Project), ExternalError> {
        planning::history(self.history, |conn| {
            let update = updates::update(conn, &id.to_string())?;
            let project = plans::project(conn, &update.project_id)?;
            Ok((update, project))
        })
        .map_err(|_| missing_update(id))
    }

    pub(super) async fn project(&self, args: &ProjectArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let meetings = MeetingFence::read(self.meetings).await?;
        let project = self.project_named(&args.project)?;
        let now = now_ms();
        let (outlook, milestones, quotes_shown) = planning::history(self.history, |conn| {
            let outlook = progress::outlook(conn, &project, now)?;
            let milestones = progress::milestone_progress(conn, &project.id)?;
            let quotes_shown =
                updates::quotes_shown(conn, |meeting| meetings.hides_meeting(meeting))?;
            Ok((outlook, milestones, quotes_shown))
        })
        .map_err(|message| refusal(self.history, message))?;
        let (list, shares) = milestones;
        let (blocked_by, blocking) = super::dependencies::links(
            &self.all_dependencies()?,
            &project.id,
            &self.every_project()?,
        );
        let sources = planning::project_detail_without(
            self.history,
            self.meetings,
            project.id.clone(),
            |evidence| board.fence.hides_evidence(evidence),
        )
        .await
        .map_err(|message| refusal(self.history, message))?
        .evidence;
        Ok(WorkAnswer::ProjectDetail(Box::new(
            ExternalWorkProjectDetail {
                schema_version: QUERY_SCHEMA_VERSION,
                project: self.project_row(&project, &board),
                latest_update: outlook
                    .latest_update
                    .map(|update| external_update(update, &quotes_shown)),
                update_missing: outlook.update_missing,
                progress: external_progress(outlook.progress),
                milestones: list
                    .iter()
                    .zip(shares)
                    .map(|(milestone, share)| ExternalMilestoneProgress {
                        id: milestone.id.clone(),
                        name: milestone.title.clone(),
                        position: milestone.position,
                        scope: share.scope,
                        started: share.started,
                        done: share.done,
                        percent: share.percent,
                        next_open: share.next_open,
                    })
                    .collect(),
                sources: sources.into_iter().map(super::project_evidence).collect(),
                blocked_by,
                blocking,
            },
        )))
    }

    /// The updates whose `discussed` lines an agent may read.
    async fn quotes_shown(&self) -> Result<HashSet<String>, ExternalError> {
        let meetings = MeetingFence::read(self.meetings).await?;
        planning::history(self.history, |conn| {
            updates::quotes_shown(conn, |meeting| meetings.hides_meeting(meeting))
        })
        .map_err(|message| refusal(self.history, message))
    }

    pub(super) async fn project_updates(
        &self,
        args: &ProjectUpdatesArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let project = self.project_named(&args.project)?;
        let limit = args
            .limit
            .and_then(|limit| usize::try_from(limit).ok())
            .unwrap_or(updates::PAGE);
        let cursor = args.cursor.map(|cursor| cursor.to_string());
        let page = planning::history(self.history, |conn| {
            updates::page(conn, &project.id, limit, cursor.as_deref())
        })
        .map_err(|message| refusal(self.history, message))?;
        let quotes_shown = self.quotes_shown().await?;
        Ok(WorkAnswer::ProjectUpdates(ExternalWorkProjectUpdates {
            schema_version: QUERY_SCHEMA_VERSION,
            project: project_ref(&project),
            updates: page
                .updates
                .into_iter()
                .map(|update| external_update(update, &quotes_shown))
                .collect(),
            next_cursor: page.next_cursor,
        }))
    }

    async fn discussed(&self, project_id: &str) -> Result<Vec<updates::Discussed>, ExternalError> {
        project_plans::discussed_lines(self.history, self.meetings, project_id)
            .await
            .map_err(|message| refusal(self.history, message))
    }

    pub(super) async fn project_update_draft(
        &self,
        args: &ProjectArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let project = self.project_named(&args.project)?;
        let discussed = self.discussed(&project.id).await?;
        let body = planning::history(self.history, |conn| {
            updates::draft(conn, &project.id, &discussed)
        })
        .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::ProjectDraft(ExternalWorkProjectDraft {
            schema_version: QUERY_SCHEMA_VERSION,
            project: project_ref(&project),
            body,
        }))
    }

    fn update_change(
        action: UpdateAction,
        dry_run: bool,
        operation_id: Option<String>,
        project: &Project,
        update: ProjectUpdate,
        replayed: bool,
        quotes_shown: &HashSet<String>,
    ) -> WorkAnswer {
        WorkAnswer::ProjectUpdate(Box::new(ExternalWorkProjectUpdateChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run,
            operation_id,
            project: project_ref(project),
            update: external_update(update, quotes_shown),
            replayed,
        }))
    }

    pub(super) async fn project_update(
        &self,
        args: &ProjectUpdateArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let project = self.project_named(&args.project)?;
        let author = Self::update_author(&args.write);
        let health = args.health.health();
        let body = args.body.as_deref();
        let operation_id = args.operation_id(&project.id, chrono::Local::now().date_naive())?;
        let discussed = match body {
            Some(_) => Vec::new(),
            None => self.discussed(&project.id).await?,
        };
        let quoted = body.is_none().then_some(discussed.as_slice());
        if args.dry_run {
            let meetings = MeetingFence::read(self.meetings).await?;
            let (update, quotes_shown) = self.rehearse(|conn| {
                let drafted = match body {
                    Some(body) => body.to_owned(),
                    None => updates::draft(conn, &project.id, &discussed)?,
                };
                let update = updates::add(
                    conn,
                    author,
                    &project.id,
                    health,
                    &drafted,
                    quoted,
                    now_ms(),
                )?;
                let shown =
                    updates::quotes_shown(conn, |meeting| meetings.hides_meeting(meeting))?;
                Ok((update, shown))
            })?;
            return Ok(Self::update_change(
                UpdateAction::Added,
                true,
                Some(operation_id),
                &project,
                update,
                false,
                &quotes_shown,
            ));
        }
        self.applies("posting a project update")?;
        let started_ms = now_ms();
        let update = project_plans::update_add_as(
            self.app,
            author,
            &operation_id,
            &project.id,
            health,
            body,
            &discussed,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The project"))?;
        let replayed = update.created_at_ms < started_ms;
        Ok(Self::update_change(
            UpdateAction::Added,
            false,
            Some(operation_id),
            &project,
            update,
            replayed,
            &self.quotes_shown().await?,
        ))
    }

    pub(super) async fn project_update_edit(
        &self,
        args: &ProjectUpdateEditArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let (update, project) = self.update_named(&args.update)?;
        let author = Self::update_author(&args.write);
        let health = args.health.map(Health::health);
        let body = args.body.as_deref();
        let operation_id = args.write.operation_id(
            "project-update-edit",
            &(&update.id, update.edited_at_ms, health, body.map(str::trim)),
        )?;
        if args.dry_run {
            let edited = self
                .rehearse(|conn| updates::edit(conn, author, &update.id, health, body, now_ms()))?;
            return Ok(Self::update_change(
                UpdateAction::Edited,
                true,
                Some(operation_id),
                &project,
                edited,
                false,
                &self.quotes_shown().await?,
            ));
        }
        self.applies("editing a project update")?;
        let edited = project_plans::update_edit_as(
            self.app,
            author,
            &operation_id,
            &update.id,
            health,
            body,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The update"))?;
        Ok(Self::update_change(
            UpdateAction::Edited,
            false,
            Some(operation_id),
            &project,
            edited,
            false,
            &self.quotes_shown().await?,
        ))
    }

    pub(super) fn project_update_delete(
        &self,
        args: &ProjectUpdateDeleteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let (update, project) = self.update_named(&args.update)?;
        let author = Self::update_author(&args.write);
        let operation_id = args
            .write
            .operation_id("project-update-delete", &update.id)?;
        if args.dry_run {
            let deleted =
                self.rehearse(|conn| updates::delete(conn, author, &update.id, now_ms()))?;
            return Ok(Self::update_change(
                UpdateAction::Deleted,
                true,
                Some(operation_id),
                &project,
                deleted,
                false,
                &HashSet::new(),
            ));
        }
        self.applies("deleting a project update")?;
        let deleted = project_plans::update_delete_as(self.app, author, &operation_id, &update.id)
            .map_err(|message| self.write_refusal(message, &operation_id, "The update"))?;
        Ok(Self::update_change(
            UpdateAction::Deleted,
            false,
            Some(operation_id),
            &project,
            deleted,
            false,
            &HashSet::new(),
        ))
    }

    pub(super) async fn project_delete(
        &self,
        args: &ProjectDeleteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let current = self.project_named(&args.project)?;
        let trash = !args.restore;
        if current.trashed_at_ms.is_some() == trash {
            return Ok(WorkAnswer::Project(
                super::catalog::ExternalWorkProjectChange {
                    schema_version: QUERY_SCHEMA_VERSION,
                    dry_run: args.dry_run,
                    operation_id: None,
                    created: false,
                    project: self.project_row(&current, &board),
                },
            ));
        }
        let edit = ProjectEdit {
            trashed: Some(trash),
            ..ProjectEdit::default()
        };
        let operation_id = args
            .write
            .operation_id("project-delete", &(&current.id, current.revision, trash))?;
        let project = if args.dry_run {
            self.rehearse(|conn| catalog::save_project(conn, Some(&current.id), &edit, now_ms()))?
        } else {
            self.applies("deleting a project")?;
            work::catalog_project_save(self.app, &operation_id, Some(current.id.clone()), &edit)
                .map_err(|message| self.write_refusal(message, &operation_id, "The project"))?
        };
        Ok(WorkAnswer::Project(
            super::catalog::ExternalWorkProjectChange {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: args.dry_run,
                operation_id: Some(operation_id),
                created: false,
                project: self.project_row(&project, &board),
            },
        ))
    }

    pub(super) fn milestone_reorder(
        &self,
        args: &MilestoneReorderArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let project = self.project_named(&args.project)?;
        let milestones = planning::history(self.history, |conn| {
            Ok(plans::records(conn, &project.id)?.milestones)
        })
        .map_err(|message| refusal(self.history, message))?;
        let mut order = Vec::new();
        for value in &args.milestones {
            let milestone = super::catalog::pick_milestone(value, &milestones)?;
            order.push(milestone.id.clone());
        }
        let operation_id = args
            .write
            .operation_id("milestone-reorder", &(&project.id, &order))?;
        let placed = if args.dry_run {
            self.rehearse(|conn| catalog::reorder_milestones(conn, &project.id, &order, now_ms()))?
        } else {
            self.applies("reordering milestones")?;
            project_plans::milestone_reorder(self.app, &operation_id, &project.id, &order)
                .map_err(|message| self.write_refusal(message, &operation_id, "The project"))?
        };
        Ok(WorkAnswer::MilestoneOrder(ExternalWorkMilestoneOrder {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id: Some(operation_id),
            project: project_ref(&project),
            milestones: placed
                .into_iter()
                .map(|milestone| ExternalMilestonePlace {
                    id: milestone.id,
                    name: milestone.title,
                    position: milestone.position,
                })
                .collect(),
        }))
    }

    fn listing(&self) -> Result<WorkCycles, ExternalError> {
        cycles::work_cycles(self.app.clone()).map_err(|message| refusal(self.history, message))
    }

    fn cycle_change(
        action: CycleAction,
        dry_run: bool,
        operation_id: String,
        number: Option<i64>,
        listing: &WorkCycles,
    ) -> WorkAnswer {
        WorkAnswer::CycleChange(Box::new(ExternalWorkCycleChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run,
            operation_id: Some(operation_id),
            number,
            cycles: external_cycles(listing),
        }))
    }

    pub(super) fn cycle_settings(
        &self,
        args: &CycleSettingsArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let before = self.listing()?.settings;
        let settings = WorkCycleSettings {
            weeks: args.weeks.unwrap_or(before.weeks),
            start_weekday: args.start_weekday.unwrap_or(before.start_weekday),
            cooldown_weeks: args.cooldown.unwrap_or(before.cooldown_weeks),
            upcoming: args.upcoming.unwrap_or(before.upcoming),
            auto_add_started: args.auto_add_started.unwrap_or(before.auto_add_started),
            auto_add_completed: args.auto_add_completed.unwrap_or(before.auto_add_completed),
        };
        let today = chrono::Local::now().date_naive().to_string();
        let operation_id = args
            .write
            .operation_id("cycle-settings", &(before, settings, today))?;
        let listing = if args.dry_run {
            self.rehearse(|conn| {
                let now = now_ms();
                cycles::save_settings(conn, settings, now)?;
                cycles::listing(conn, now)
            })?
        } else {
            self.applies("changing how cycles run")?;
            cycles::work_cycle_settings_save(self.app.clone(), operation_id.clone(), settings)
                .map_err(|message| self.write_refusal(message, &operation_id, "Cycles"))?
        };
        Ok(Self::cycle_change(
            CycleAction::Settings,
            args.dry_run,
            operation_id,
            None,
            &listing,
        ))
    }

    pub(super) fn cycle_save(&self, args: &CycleSaveArgs) -> Result<WorkAnswer, ExternalError> {
        let number = named_cycle(&self.listing()?.cycles, args.cycle)?;
        let edit = cycles::CycleEdit {
            name: args.name.as_deref().map(|name| {
                if super::catalog::cleared(name) {
                    String::new()
                } else {
                    name.trim().to_owned()
                }
            }),
            start_day: args.start,
            end_day: args.end.map(|end| end + chrono::Days::new(1)),
        };
        let operation_id = args.write.operation_id(
            "cycle-save",
            &(
                number,
                &edit.name,
                args.start.map(|day| day.to_string()),
                args.end.map(|day| day.to_string()),
            ),
        )?;
        let listing = if args.dry_run {
            self.rehearse(|conn| {
                let now = now_ms();
                cycles::save_cycle(conn, number, &edit, now)?;
                cycles::listing(conn, now)
            })?
        } else {
            self.applies("changing a cycle")?;
            cycles::cycle_save(self.app, &operation_id, number, &edit)
                .map_err(|message| self.write_refusal(message, &operation_id, "The cycle"))?
        };
        Ok(Self::cycle_change(
            CycleAction::Saved,
            args.dry_run,
            operation_id,
            Some(number),
            &listing,
        ))
    }

    pub(super) fn cycle_rollover(
        &self,
        args: &CycleRolloverArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let number = named_cycle(&self.listing()?.cycles, args.cycle)?;
        let today = chrono::Local::now().date_naive().to_string();
        let operation_id = args
            .write
            .operation_id("cycle-rollover", &(number, today))?;
        let listing = if args.dry_run {
            self.rehearse(|conn| {
                let now = now_ms();
                cycles::rollover(conn, number, now)?;
                cycles::listing(conn, now)
            })?
        } else {
            self.applies("moving a cycle's work on")?;
            cycles::cycle_rollover(self.app, &operation_id, number)
                .map_err(|message| self.write_refusal(message, &operation_id, "The cycle"))?
        };
        Ok(Self::cycle_change(
            CycleAction::RolledOver,
            args.dry_run,
            operation_id,
            Some(number),
            &listing,
        ))
    }

    pub(super) fn cycle_start_today(
        &self,
        args: &CycleStartTodayArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let all = self.listing()?.cycles;
        let number = match args.cycle {
            Some(cycle) => named_cycle(&all, cycle)?,
            None => all
                .iter()
                .find(|cycle| cycle.state == WorkCycleState::Upcoming)
                .map(|cycle| cycle.number)
                .ok_or_else(|| {
                    ExternalError::new(ExternalErrorCode::NotFound, "No upcoming cycle.")
                })?,
        };
        let today = chrono::Local::now().date_naive().to_string();
        let operation_id = args
            .write
            .operation_id("cycle-start-today", &(number, today))?;
        let listing = if args.dry_run {
            self.rehearse(|conn| {
                let now = now_ms();
                cycles::start_today(conn, number, now)?;
                cycles::listing(conn, now)
            })?
        } else {
            self.applies("starting a cycle")?;
            cycles::cycle_start_today(self.app, &operation_id, number)
                .map_err(|message| self.write_refusal(message, &operation_id, "The cycle"))?
        };
        Ok(Self::cycle_change(
            CycleAction::StartedToday,
            args.dry_run,
            operation_id,
            Some(number),
            &listing,
        ))
    }
}

fn update_text(text: &mut String, update: &ExternalProjectUpdate) {
    let _ = writeln!(
        text,
        "{}  {} · {}% · {} · {}",
        update.id,
        update.health.text(),
        update.progress,
        update.author.name.as_deref().unwrap_or("You"),
        super::day_text(Some(update.created_at_ms))
    );
    if let Some(body) = &update.body {
        for line in body.lines() {
            let _ = writeln!(text, "  {line}");
        }
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::ProjectDetail(detail) => {
            let project = &detail.project;
            let progress = &detail.progress;
            let _ = writeln!(
                text,
                "{} · {} · {}% ({} done, {} started of {})",
                project.name,
                project
                    .status_name
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", project.status)),
                progress.percent,
                progress.done,
                progress.started,
                progress.scope
            );
            if let Some(summary) = &project.summary {
                let _ = writeln!(text, "{summary}");
            }
            if let Some(initiative) = &project.initiative {
                let _ = writeln!(text, "In initiative {}", initiative.name);
            }
            if let Some(prediction) = &progress.prediction {
                let _ = writeln!(
                    text,
                    "Finishes around {} (between {} and {})",
                    super::day_text(Some(prediction.expected_at_ms)),
                    super::day_text(Some(prediction.optimistic_at_ms)),
                    super::day_text(Some(prediction.pessimistic_at_ms))
                );
            }
            match &detail.latest_update {
                Some(update) => {
                    text.push_str("Latest update:\n");
                    update_text(text, update);
                }
                None => text.push_str("No updates yet.\n"),
            }
            if detail.update_missing {
                text.push_str("An update is due.\n");
            }
            for milestone in &detail.milestones {
                let _ = writeln!(
                    text,
                    "{}{} · {}%",
                    if milestone.next_open { "> " } else { "  " },
                    milestone.name,
                    milestone.percent
                );
            }
            super::dependencies::links_text(text, "Blocked by", &detail.blocked_by);
            super::dependencies::links_text(text, "Blocking", &detail.blocking);
            if !detail.sources.is_empty() {
                text.push_str("Sources:\n");
            }
            for source in &detail.sources {
                let _ = writeln!(
                    text,
                    "  {:?} {}",
                    source.source_kind,
                    source.title.as_deref().unwrap_or("(gone)")
                );
            }
        }
        WorkAnswer::ProjectUpdates(page) => {
            if page.updates.is_empty() {
                let _ = writeln!(text, "{} has no updates.", page.project.name);
            }
            for update in &page.updates {
                update_text(text, update);
            }
            if let Some(cursor) = &page.next_cursor {
                let _ = writeln!(text, "More: --cursor {cursor}");
            }
        }
        WorkAnswer::ProjectUpdate(change) => {
            let verb = match (change.action, change.dry_run) {
                (UpdateAction::Added, false) => "Posted an update on",
                (UpdateAction::Added, true) => "Would post an update on",
                (UpdateAction::Edited, false) => "Edited an update on",
                (UpdateAction::Edited, true) => "Would edit an update on",
                (UpdateAction::Deleted, false) => "Deleted an update on",
                (UpdateAction::Deleted, true) => "Would delete an update on",
            };
            let _ = writeln!(
                text,
                "{verb} {}{}",
                change.project.name,
                if change.replayed {
                    " (already done)"
                } else {
                    ""
                }
            );
            update_text(text, &change.update);
        }
        WorkAnswer::ProjectDraft(draft) => {
            text.push_str(&draft.body);
            if !draft.body.ends_with('\n') {
                text.push('\n');
            }
        }
        WorkAnswer::MilestoneOrder(order) => {
            let _ = writeln!(
                text,
                "{} milestones of {}:",
                if order.dry_run {
                    "Would order the"
                } else {
                    "Ordered the"
                },
                order.project.name
            );
            for milestone in &order.milestones {
                let _ = writeln!(text, "  {}", milestone.name);
            }
        }
        WorkAnswer::CycleChange(change) => {
            let verb = match change.action {
                CycleAction::Settings => "Cycles now run",
                CycleAction::Saved => "Saved the cycle",
                CycleAction::RolledOver => "Moved the cycle's unfinished work on",
                CycleAction::StartedToday => "Started the cycle today",
            };
            let _ = writeln!(
                text,
                "{}{verb}: {} weeks, {} cooldown, {} ahead.",
                if change.dry_run { "Dry run. " } else { "" },
                change.cycles.weeks,
                change.cycles.cooldown_weeks,
                change.cycles.upcoming
            );
            for cycle in &change.cycles.cycles {
                super::cycle_line(text, cycle);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::{WorkCommand, WorkVerb};
    use super::ProjectUpdateArgs;
    use crate::cli::{CliArgs, Command};
    use chrono::NaiveDate;
    use clap::Parser;

    const PROJECT: &str = "5b0c1e0e-8f1d-4c1e-9d43-0a6f0e3c2a11";

    fn update(health: &str, body: &str) -> ProjectUpdateArgs {
        let argv = [
            "sona", "work", "project-update", "Harbor launch", "--health", health, "--body", body,
        ];
        match CliArgs::try_parse_from(argv) {
            Ok(CliArgs {
                command:
                    Some(Command::Work(WorkCommand {
                        verb: WorkVerb::ProjectUpdate(args),
                        ..
                    })),
                ..
            }) => args,
            other => panic!("`work project-update` parsed as {other:?}"),
        }
    }

    #[test]
    fn a_retried_project_update_is_posted_under_the_same_id() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 11).expect("a day");
        let first = update("at-risk", "Venue held.").operation_id(PROJECT, day);
        let retry = update("at-risk", " Venue held.").operation_id(PROJECT, day);
        let other = update("on-track", "Venue held.").operation_id(PROJECT, day);
        let first = first.expect("an id");
        assert_eq!(first, retry.expect("an id"));
        assert_ne!(first, other.expect("an id"));
    }
}
