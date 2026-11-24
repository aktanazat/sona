//! `sona work` verbs for initiatives: goals one level above projects.

use super::catalog::{ExternalWorkPersonRef, ExternalWorkProject};
use super::roadmap::{ExternalProjectRef, ExternalProjectUpdate, ExternalUpdateAuthor, Health};
use super::{
    day_or_none, day_text, pick, refusal, Board, DayArg, Priority, Tracker, WorkAnswer, WriteArgs,
};
use crate::planning::work::comments::Author;
use crate::planning::work::initiatives::{
    self, Initiative, InitiativeSaveInput, InitiativeStatus, InitiativeUpdate,
};
use crate::planning::work::types::{WorkActor, WorkLabel, WorkOwner};
use crate::planning::{self, Project};
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::fmt::Write as _;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum InitiativeState {
    Backlog,
    Planned,
    Active,
    Paused,
    Completed,
    Canceled,
}

impl From<InitiativeState> for InitiativeStatus {
    fn from(state: InitiativeState) -> Self {
        match state {
            InitiativeState::Backlog => Self::Backlog,
            InitiativeState::Planned => Self::Planned,
            InitiativeState::Active => Self::Active,
            InitiativeState::Paused => Self::Paused,
            InitiativeState::Completed => Self::Completed,
            InitiativeState::Canceled => Self::Canceled,
        }
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct InitiativeArgs {
    /// The initiative, by name or id.
    pub initiative: String,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct InitiativeNewArgs {
    pub name: String,
    #[arg(long)]
    pub details: Option<String>,
    /// me, a person from the People page, or nobody.
    #[arg(long, value_name = "PERSON")]
    pub owner: Option<String>,
    /// YYYY-MM-DD, today, tomorrow or none.
    #[arg(long, value_parser = day_or_none, value_name = "DAY")]
    pub target: Option<DayArg>,
    /// planned unless told.
    #[arg(long, value_enum)]
    pub status: Option<InitiativeState>,
    /// The initiative it sits under, by name or id.
    #[arg(long, value_name = "INITIATIVE")]
    pub parent: Option<String>,
    #[arg(long, value_enum)]
    pub priority: Option<Priority>,
    /// Comma-separated labels that may go on projects.
    #[arg(long, value_delimiter = ',', value_name = "LABELS")]
    pub labels: Option<Vec<String>>,
    /// Show the initiative without saving it.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct InitiativeUpdateArgs {
    /// The initiative, by name or id.
    pub initiative: String,
    /// Rename it.
    #[arg(long)]
    pub name: Option<String>,
    /// New details; none clears them.
    #[arg(long)]
    pub details: Option<String>,
    /// me, a person from the People page, or nobody.
    #[arg(long, value_name = "PERSON")]
    pub owner: Option<String>,
    /// YYYY-MM-DD, today, tomorrow or none.
    #[arg(long, value_parser = day_or_none, value_name = "DAY")]
    pub target: Option<DayArg>,
    #[arg(long, value_enum)]
    pub status: Option<InitiativeState>,
    /// The initiative it sits under, by name or id; none takes it to the top.
    #[arg(long, value_name = "INITIATIVE")]
    pub parent: Option<String>,
    #[arg(long, value_enum)]
    pub priority: Option<Priority>,
    /// Comma-separated labels that may go on projects; none clears them.
    #[arg(long, value_delimiter = ',', value_name = "LABELS")]
    pub labels: Option<Vec<String>>,
    /// Show the change without saving it.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct InitiativeDeleteArgs {
    /// The initiative, by name or id.
    pub initiative: String,
    /// Show what would be deleted without deleting it.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct InitiativeProjectArgs {
    /// The initiative, by name or id.
    pub initiative: String,
    /// The project, by name or id.
    pub project: String,
    /// Show the change without saving it.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct InitiativeUpdatePostArgs {
    /// The initiative, by name or id.
    pub initiative: String,
    #[arg(long, value_enum)]
    pub health: Health,
    /// Markdown.
    #[arg(long)]
    pub body: String,
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct InitiativeUpdateListArgs {
    /// The initiative, by name or id.
    pub initiative: String,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub limit: Option<u32>,
    /// The last update id of the page before.
    #[arg(long, value_name = "UPDATE")]
    pub cursor: Option<Uuid>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct InitiativeUpdateEditArgs {
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
pub struct InitiativeUpdateDeleteArgs {
    pub update: Uuid,
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

/// How far an initiative's projects have come; archived and deleted
/// projects are left out.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ExternalInitiativeProgress {
    pub projects: u32,
    pub completed_projects: u32,
    pub open_tasks: u32,
    pub done_tasks: u32,
    /// Done tasks out of open and done ones, 0 to 100.
    pub percent: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalInitiative {
    pub id: String,
    pub name: String,
    pub details: Option<String>,
    pub status: InitiativeStatus,
    pub owner: Option<ExternalWorkPersonRef>,
    pub owner_is_self: bool,
    pub target: Option<String>,
    pub target_at_ms: Option<i64>,
    pub projects: Vec<ExternalProjectRef>,
    pub progress: ExternalInitiativeProgress,
    /// The initiative it sits under.
    pub parent: Option<ExternalProjectRef>,
    /// The initiatives directly under it.
    pub children: Vec<ExternalProjectRef>,
    pub priority: Priority,
    /// Label names.
    pub labels: Vec<String>,
    pub latest_update: Option<ExternalProjectUpdate>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkInitiativeUpdates {
    pub schema_version: u32,
    pub initiative: ExternalProjectRef,
    /// Newest first.
    pub updates: Vec<ExternalProjectUpdate>,
    /// Pass as --cursor for the next page; none on the last.
    pub next_cursor: Option<String>,
}

/// What an initiative update write did.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InitiativeUpdateAction {
    Added,
    Edited,
    Deleted,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkInitiativeUpdateChange {
    pub schema_version: u32,
    pub action: InitiativeUpdateAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub initiative: ExternalProjectRef,
    pub update: ExternalProjectUpdate,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkInitiatives {
    pub schema_version: u32,
    pub initiatives: Vec<ExternalInitiative>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkInitiativeDetail {
    pub schema_version: u32,
    pub initiative: ExternalInitiative,
    pub projects: Vec<ExternalWorkProject>,
}

/// What an initiative write did.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InitiativeAction {
    Created,
    Updated,
    Unchanged,
    Deleted,
    ProjectAdded,
    ProjectRemoved,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkInitiativeChange {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub action: InitiativeAction,
    pub initiative: ExternalInitiative,
    pub project: Option<ExternalProjectRef>,
}

/// What names an initiative's projects, initiatives and labels.
struct Known {
    projects: Vec<Project>,
    initiatives: Vec<Initiative>,
    labels: Vec<WorkLabel>,
}

impl Known {
    fn initiative_ref(&self, id: &str) -> ExternalProjectRef {
        ExternalProjectRef {
            id: id.to_owned(),
            name: self
                .initiatives
                .iter()
                .find(|initiative| initiative.id == id)
                .map_or_else(|| id.to_owned(), |initiative| initiative.name.clone()),
        }
    }
}

fn external_update(update: InitiativeUpdate) -> ExternalProjectUpdate {
    ExternalProjectUpdate {
        id: update.id,
        health: Health::of(update.health),
        body: update.body,
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

fn external(initiative: &Initiative, known: &Known, board: &Board) -> ExternalInitiative {
    let progress = initiative.progress;
    ExternalInitiative {
        id: initiative.id.clone(),
        name: initiative.name.clone(),
        details: initiative.description.clone(),
        status: initiative.status,
        owner: initiative
            .owner_person_id
            .as_ref()
            .map(|id| ExternalWorkPersonRef {
                id: id.clone(),
                name: board
                    .people
                    .iter()
                    .find(|person| &person.id == id)
                    .map_or_else(|| id.clone(), |person| person.name.clone()),
            }),
        owner_is_self: initiative.owner_is_self,
        target: initiative.target_at_ms.map(|ms| day_text(Some(ms))),
        target_at_ms: initiative.target_at_ms,
        projects: initiative
            .project_ids
            .iter()
            .map(|id| ExternalProjectRef {
                id: id.clone(),
                name: known
                    .projects
                    .iter()
                    .find(|project| &project.id == id)
                    .map_or_else(|| id.clone(), |project| project.title.clone()),
            })
            .collect(),
        progress: ExternalInitiativeProgress {
            projects: progress.projects,
            completed_projects: progress.completed_projects,
            open_tasks: progress.open_tasks,
            done_tasks: progress.done_tasks,
            percent: u32::from(initiatives::percent(progress)),
        },
        parent: initiative
            .parent_id
            .as_deref()
            .map(|id| known.initiative_ref(id)),
        children: initiative
            .child_ids
            .iter()
            .map(|id| known.initiative_ref(id))
            .collect(),
        priority: Priority::of(initiative.priority),
        labels: initiative
            .label_ids
            .iter()
            .filter_map(|id| known.labels.iter().find(|label| &label.id == id))
            .map(|label| label.name.clone())
            .collect(),
        latest_update: initiative.latest_update.clone().map(external_update),
    }
}

fn project_ref(project: &Project) -> ExternalProjectRef {
    ExternalProjectRef {
        id: project.id.clone(),
        name: project.title.clone(),
    }
}

fn cleared(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || value.eq_ignore_ascii_case("none")
}

impl Tracker<'_> {
    pub(super) fn all_initiatives(&self) -> Result<Vec<Initiative>, ExternalError> {
        planning::history(self.history, |conn| initiatives::initiatives(conn))
            .map_err(|message| refusal(self.history, message))
    }

    pub(super) fn pick_initiative<'a>(
        value: &str,
        all: &'a [Initiative],
    ) -> Result<&'a Initiative, ExternalError> {
        pick(
            "initiative",
            value,
            all,
            |initiative| &initiative.id,
            |initiative| &initiative.name,
        )
        .map_err(|refusal| refusal.hint("List them with: sona work initiative-list"))
    }

    fn name_free(
        all: &[Initiative],
        name: &str,
        other_than: Option<&str>,
    ) -> Result<(), ExternalError> {
        match all.iter().find(|initiative| {
            initiative.name.eq_ignore_ascii_case(name.trim())
                && other_than != Some(initiative.id.as_str())
        }) {
            Some(taken) => Err(ExternalError::invalid(format!(
                "An initiative called {:?} already exists.",
                taken.name
            ))),
            None => Ok(()),
        }
    }

    fn known(&self) -> Result<Known, ExternalError> {
        Ok(Known {
            projects: self.every_project()?,
            initiatives: self.all_initiatives()?,
            labels: self.all_labels()?,
        })
    }

    /// `--parent` as an id to save: a nested initiative's parent, or none
    /// for the top. Refused when it would sit under itself.
    fn parent_input(
        all: &[Initiative],
        value: Option<&str>,
        own_id: Option<&str>,
    ) -> Result<Option<Option<String>>, ExternalError> {
        let Some(value) = value else {
            return Ok(None);
        };
        if cleared(value) {
            return Ok(Some(None));
        }
        let parent = Self::pick_initiative(value, all)?;
        if let Some(own_id) = own_id {
            let mut at = Some(parent);
            while let Some(step) = at {
                if step.id == own_id {
                    return Err(ExternalError::invalid(
                        "An initiative cannot sit under itself or one of its own.",
                    ));
                }
                at = step
                    .parent_id
                    .as_deref()
                    .and_then(|id| all.iter().find(|initiative| initiative.id == id));
            }
        }
        Ok(Some(Some(parent.id.clone())))
    }

    /// `--labels` as ids: names or ids of labels that may go on projects.
    fn initiative_labels(
        &self,
        values: Option<&[String]>,
    ) -> Result<Option<Vec<String>>, ExternalError> {
        let Some(values) = values else {
            return Ok(None);
        };
        let names: Vec<&str> = values
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .collect();
        if names.iter().any(|name| cleared(name)) {
            return Ok(Some(Vec::new()));
        }
        let labels = self.all_labels()?;
        let mut ids = Vec::new();
        for name in names {
            let label = pick(
                "label",
                name,
                &labels,
                |label| &label.id,
                |label| &label.name,
            )
            .map_err(|refusal| refusal.hint("List them with: sona work labels"))?;
            if label.scope == crate::planning::LabelScope::Tasks {
                return Err(ExternalError::invalid(format!(
                    "The label {:?} is kept for tasks. Let it on projects with: sona work label-scope {:?} all",
                    label.name, label.name
                )));
            }
            if !ids.contains(&label.id) {
                ids.push(label.id.clone());
            }
        }
        Ok(Some(ids))
    }

    fn initiative_answer(
        &self,
        initiative: &Initiative,
        board: &Board,
        dry_run: bool,
        operation_id: Option<String>,
        action: InitiativeAction,
        project: Option<&Project>,
    ) -> Result<WorkAnswer, ExternalError> {
        let known = self.known()?;
        Ok(WorkAnswer::InitiativeChange(Box::new(
            ExternalWorkInitiativeChange {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run,
                operation_id,
                action,
                initiative: external(initiative, &known, board),
                project: project.map(project_ref),
            },
        )))
    }

    pub(super) async fn initiative_list(&self) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let known = self.known()?;
        Ok(WorkAnswer::Initiatives(ExternalWorkInitiatives {
            schema_version: QUERY_SCHEMA_VERSION,
            initiatives: known
                .initiatives
                .iter()
                .map(|initiative| external(initiative, &known, &board))
                .collect(),
        }))
    }

    pub(super) async fn initiative_show(
        &self,
        args: &InitiativeArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let known = self.known()?;
        let initiative = Self::pick_initiative(&args.initiative, &known.initiatives)?;
        Ok(WorkAnswer::InitiativeDetail(Box::new(
            ExternalWorkInitiativeDetail {
                schema_version: QUERY_SCHEMA_VERSION,
                initiative: external(initiative, &known, &board),
                projects: initiative
                    .project_ids
                    .iter()
                    .filter_map(|id| known.projects.iter().find(|project| &project.id == id))
                    .map(|project| self.project_row(project, &board))
                    .collect(),
            },
        )))
    }

    fn owner(board: &Board, value: Option<&str>) -> Result<Option<WorkOwner>, ExternalError> {
        value
            .map(|value| {
                if cleared(value) {
                    Ok(WorkOwner::Nobody)
                } else {
                    board.owner(value)
                }
            })
            .transpose()
    }

    /// Runs a save for real or as a rehearsal.
    fn initiative_saved(
        &self,
        id: Option<&str>,
        input: &InitiativeSaveInput,
        dry_run: bool,
        operation_id: &str,
    ) -> Result<Initiative, ExternalError> {
        if dry_run {
            return self.rehearse(|conn| initiatives::save(conn, id, input, super::now_ms()));
        }
        self.applies("saving an initiative")?;
        initiatives::save_written(self.app, operation_id, id, input)
            .map_err(|message| self.write_refusal(message, operation_id, "The initiative"))
    }

    pub(super) async fn initiative_new(
        &self,
        args: &InitiativeNewArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let operation_id = args.write.fresh_operation_id();
        let all = self.all_initiatives()?;
        if !self.ran(&operation_id)? {
            Self::name_free(&all, &args.name, None)?;
        }
        let input = InitiativeSaveInput {
            name: Some(args.name.clone()),
            description: args.details.clone().map(Some),
            owner: Self::owner(&board, args.owner.as_deref())?,
            target_at_ms: args.target.map(DayArg::ms).transpose()?,
            status: args.status.map(Into::into),
            parent_id: Self::parent_input(&all, args.parent.as_deref(), None)?,
            priority: args.priority.map(Priority::value),
            label_ids: self.initiative_labels(args.labels.as_deref())?,
        };
        let initiative = self.initiative_saved(None, &input, args.dry_run, &operation_id)?;
        self.initiative_answer(
            &initiative,
            &board,
            args.dry_run,
            Some(operation_id),
            InitiativeAction::Created,
            None,
        )
    }

    pub(super) async fn initiative_update(
        &self,
        args: &InitiativeUpdateArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let all = self.all_initiatives()?;
        let current = Self::pick_initiative(&args.initiative, &all)?;
        if let Some(name) = &args.name {
            Self::name_free(&all, name, Some(&current.id))?;
        }
        let input = InitiativeSaveInput {
            name: args.name.clone(),
            description: args
                .details
                .as_deref()
                .map(|text| (!cleared(text)).then(|| text.to_owned())),
            owner: Self::owner(&board, args.owner.as_deref())?,
            target_at_ms: args.target.map(DayArg::ms).transpose()?,
            status: args.status.map(Into::into),
            parent_id: Self::parent_input(&all, args.parent.as_deref(), Some(&current.id))?,
            priority: args.priority.map(Priority::value),
            label_ids: self.initiative_labels(args.labels.as_deref())?,
        };
        let after = self.rehearse(|conn| {
            initiatives::save(conn, Some(&current.id), &input, current.updated_at_ms)
        })?;
        if after == *current {
            return self.initiative_answer(
                current,
                &board,
                args.dry_run,
                None,
                InitiativeAction::Unchanged,
                None,
            );
        }
        let operation_id = args.write.operation_id(
            "initiative-update",
            &(&current.id, current.updated_at_ms, &input),
        )?;
        let initiative =
            self.initiative_saved(Some(&current.id), &input, args.dry_run, &operation_id)?;
        self.initiative_answer(
            &initiative,
            &board,
            args.dry_run,
            Some(operation_id),
            InitiativeAction::Updated,
            None,
        )
    }

    pub(super) async fn initiative_delete(
        &self,
        args: &InitiativeDeleteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let all = self.all_initiatives()?;
        let current = Self::pick_initiative(&args.initiative, &all)?;
        let operation_id = args.write.operation_id("initiative-delete", &current.id)?;
        let initiative = if args.dry_run {
            current.clone()
        } else {
            self.applies("deleting an initiative")?;
            initiatives::delete_written(self.app, &operation_id, &current.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The initiative"))?
        };
        self.initiative_answer(
            &initiative,
            &board,
            args.dry_run,
            Some(operation_id),
            InitiativeAction::Deleted,
            None,
        )
    }

    pub(super) async fn initiative_add_project(
        &self,
        args: &InitiativeProjectArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let all = self.all_initiatives()?;
        let target = Self::pick_initiative(&args.initiative, &all)?;
        let projects = self.every_project()?;
        let project = Self::pick_project(&args.project, &projects)?;
        let operation_id = args
            .write
            .operation_id("initiative-add-project", &(&target.id, &project.id))?;
        let initiative = if args.dry_run {
            self.rehearse(|conn| {
                initiatives::add_project(conn, &target.id, &project.id, super::now_ms())
            })?
        } else {
            self.applies("putting a project in an initiative")?;
            initiatives::add_project_written(self.app, &operation_id, &target.id, &project.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The initiative"))?
        };
        self.initiative_answer(
            &initiative,
            &board,
            args.dry_run,
            Some(operation_id),
            InitiativeAction::ProjectAdded,
            Some(project),
        )
    }

    pub(super) async fn initiative_remove_project(
        &self,
        args: &InitiativeProjectArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let all = self.all_initiatives()?;
        let target = Self::pick_initiative(&args.initiative, &all)?;
        let projects = self.every_project()?;
        let project = Self::pick_project(&args.project, &projects)?;
        let holder = planning::history(self.history, |conn| {
            initiatives::initiative_of(conn, &project.id)
        })
        .map_err(|message| refusal(self.history, message))?;
        if holder.as_ref().is_none_or(|holder| holder.id != target.id) {
            return Err(ExternalError::new(
                ExternalErrorCode::NotFound,
                format!("{} is not in {}.", project.title, target.name),
            ));
        }
        let operation_id = args
            .write
            .operation_id("initiative-remove-project", &(&target.id, &project.id))?;
        let initiative = if args.dry_run {
            self.rehearse(|conn| initiatives::remove_project(conn, &project.id))?
        } else {
            self.applies("taking a project out of an initiative")?;
            initiatives::remove_project_written(self.app, &operation_id, &project.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The initiative"))?
        };
        self.initiative_answer(
            &initiative,
            &board,
            args.dry_run,
            Some(operation_id),
            InitiativeAction::ProjectRemoved,
            Some(project),
        )
    }

    fn initiative_update_named(
        &self,
        id: &Uuid,
    ) -> Result<(InitiativeUpdate, Initiative), ExternalError> {
        planning::history(self.history, |conn| {
            let update = initiatives::update(conn, &id.to_string())?;
            let initiative = initiatives::initiative(conn, &update.initiative_id)?;
            Ok((update, initiative))
        })
        .map_err(|_| {
            ExternalError::new(
                ExternalErrorCode::NotFound,
                format!("No initiative update {id}."),
            )
            .hint("List them with: sona work initiative-update-list INITIATIVE")
        })
    }

    fn initiative_update_answer(
        action: InitiativeUpdateAction,
        dry_run: bool,
        operation_id: String,
        initiative: &Initiative,
        update: InitiativeUpdate,
    ) -> WorkAnswer {
        WorkAnswer::InitiativeUpdateChange(Box::new(ExternalWorkInitiativeUpdateChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run,
            operation_id: Some(operation_id),
            initiative: ExternalProjectRef {
                id: initiative.id.clone(),
                name: initiative.name.clone(),
            },
            update: external_update(update),
        }))
    }

    fn update_writer(write: &WriteArgs) -> Author<'_> {
        Author {
            actor: WorkActor::Agent,
            agent: write.agent.as_deref(),
        }
    }

    pub(super) fn initiative_update_list(
        &self,
        args: &InitiativeUpdateListArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let all = self.all_initiatives()?;
        let initiative = Self::pick_initiative(&args.initiative, &all)?;
        let limit = args
            .limit
            .and_then(|limit| usize::try_from(limit).ok())
            .unwrap_or(initiatives::UPDATES_PAGE);
        let cursor = args.cursor.map(|cursor| cursor.to_string());
        let page = planning::history(self.history, |conn| {
            initiatives::updates_page(conn, &initiative.id, limit, cursor.as_deref())
        })
        .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::InitiativeUpdates(
            ExternalWorkInitiativeUpdates {
                schema_version: QUERY_SCHEMA_VERSION,
                initiative: ExternalProjectRef {
                    id: initiative.id.clone(),
                    name: initiative.name.clone(),
                },
                updates: page.updates.into_iter().map(external_update).collect(),
                next_cursor: page.next_cursor,
            },
        ))
    }

    pub(super) fn initiative_update_post(
        &self,
        args: &InitiativeUpdatePostArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let all = self.all_initiatives()?;
        let initiative = Self::pick_initiative(&args.initiative, &all)?;
        let author = Self::update_writer(&args.write);
        let health = args.health.health();
        let operation_id = args.write.operation_id(
            "initiative-update-post",
            &(
                &initiative.id,
                args.health,
                args.body.trim(),
                chrono::Local::now().date_naive().to_string(),
            ),
        )?;
        let update = if args.dry_run {
            self.rehearse(|conn| {
                initiatives::add_update(
                    conn,
                    author,
                    &initiative.id,
                    health,
                    &args.body,
                    super::now_ms(),
                )
            })?
        } else {
            self.applies("posting an initiative update")?;
            initiatives::update_add_written(
                self.app,
                author,
                &operation_id,
                &initiative.id,
                health,
                &args.body,
            )
            .map_err(|message| self.write_refusal(message, &operation_id, "The initiative"))?
        };
        Ok(Self::initiative_update_answer(
            InitiativeUpdateAction::Added,
            args.dry_run,
            operation_id,
            initiative,
            update,
        ))
    }

    pub(super) fn initiative_update_edit(
        &self,
        args: &InitiativeUpdateEditArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let (update, initiative) = self.initiative_update_named(&args.update)?;
        let author = Self::update_writer(&args.write);
        let health = args.health.map(Health::health);
        let body = args.body.as_deref();
        let operation_id = args.write.operation_id(
            "initiative-update-edit",
            &(&update.id, update.edited_at_ms, health, body.map(str::trim)),
        )?;
        let edited = if args.dry_run {
            self.rehearse(|conn| {
                initiatives::edit_update(conn, author, &update.id, health, body, super::now_ms())
            })?
        } else {
            self.applies("editing an initiative update")?;
            initiatives::update_edit_written(
                self.app,
                author,
                &operation_id,
                &update.id,
                health,
                body,
            )
            .map_err(|message| self.write_refusal(message, &operation_id, "The update"))?
        };
        Ok(Self::initiative_update_answer(
            InitiativeUpdateAction::Edited,
            args.dry_run,
            operation_id,
            &initiative,
            edited,
        ))
    }

    pub(super) fn initiative_update_delete(
        &self,
        args: &InitiativeUpdateDeleteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let (update, initiative) = self.initiative_update_named(&args.update)?;
        let author = Self::update_writer(&args.write);
        let operation_id = args
            .write
            .operation_id("initiative-update-delete", &update.id)?;
        let deleted = if args.dry_run {
            self.rehearse(|conn| {
                initiatives::delete_update(conn, author, &update.id, super::now_ms())
            })?
        } else {
            self.applies("deleting an initiative update")?;
            initiatives::update_delete_written(self.app, author, &operation_id, &update.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The update"))?
        };
        Ok(Self::initiative_update_answer(
            InitiativeUpdateAction::Deleted,
            args.dry_run,
            operation_id,
            &initiative,
            deleted,
        ))
    }
}

fn status_word(status: InitiativeStatus) -> &'static str {
    match status {
        InitiativeStatus::Backlog => "Backlog",
        InitiativeStatus::Planned => "Planned",
        InitiativeStatus::Active => "Active",
        InitiativeStatus::Paused => "Paused",
        InitiativeStatus::Completed => "Completed",
        InitiativeStatus::Canceled => "Canceled",
    }
}

fn line(initiative: &ExternalInitiative) -> String {
    let mut text = format!("{} · {}", initiative.name, status_word(initiative.status));
    if initiative.priority != Priority::None {
        let _ = write!(text, " · {}", initiative.priority.text());
    }
    if let Some(parent) = &initiative.parent {
        let _ = write!(text, " · in {}", parent.name);
    }
    if initiative.owner_is_self {
        text.push_str(" · owner you");
    } else if let Some(owner) = &initiative.owner {
        let _ = write!(text, " · owner {}", owner.name);
    }
    if let Some(target) = &initiative.target {
        let _ = write!(text, " · target {target}");
    }
    if !initiative.labels.is_empty() {
        let _ = write!(text, " · {}", initiative.labels.join(", "));
    }
    if let Some(update) = &initiative.latest_update {
        let _ = write!(text, " · {}", update.health.text());
    }
    let progress = &initiative.progress;
    let _ = write!(
        text,
        " · {} of {} projects completed · {}% of tasks done",
        progress.completed_projects, progress.projects, progress.percent
    );
    text
}

fn update_text(text: &mut String, update: &ExternalProjectUpdate) {
    let _ = writeln!(
        text,
        "{}  {} · {}% · {} · {}",
        update.id,
        update.health.text(),
        update.progress,
        update.author.name.as_deref().unwrap_or("You"),
        day_text(Some(update.created_at_ms))
    );
    if let Some(body) = &update.body {
        for line in body.lines() {
            let _ = writeln!(text, "  {line}");
        }
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::Initiatives(list) => {
            if list.initiatives.is_empty() {
                text.push_str("No initiatives yet. Make one with: sona work initiative-new NAME\n");
            }
            for initiative in &list.initiatives {
                let _ = writeln!(text, "{}", line(initiative));
            }
        }
        WorkAnswer::InitiativeDetail(detail) => {
            let _ = writeln!(text, "{}", line(&detail.initiative));
            if let Some(details) = &detail.initiative.details {
                let _ = writeln!(text, "{details}");
            }
            if detail.projects.is_empty() {
                text.push_str("No projects yet. Add one with: sona work initiative-add-project INITIATIVE PROJECT\n");
            }
            for project in &detail.projects {
                let _ = writeln!(
                    text,
                    "- {} · {:?} · {} open, {} done",
                    project.name, project.status, project.open_tasks, project.done_tasks
                );
            }
            for child in &detail.initiative.children {
                let _ = writeln!(text, "- initiative {}", child.name);
            }
        }
        WorkAnswer::InitiativeUpdates(page) => {
            let _ = writeln!(text, "Updates on {}", page.initiative.name);
            if page.updates.is_empty() {
                text.push_str(
                    "No updates yet. Post one with: sona work initiative-update-post INITIATIVE --health on-track --body TEXT\n",
                );
            }
            for update in &page.updates {
                update_text(text, update);
            }
            if let Some(cursor) = &page.next_cursor {
                let _ = writeln!(text, "More: --cursor {cursor}");
            }
        }
        WorkAnswer::InitiativeUpdateChange(change) => {
            let name = &change.initiative.name;
            let _ = match (change.action, change.dry_run) {
                (InitiativeUpdateAction::Added, false) => {
                    writeln!(text, "Posted an update on {name}.")
                }
                (InitiativeUpdateAction::Added, true) => {
                    writeln!(text, "Dry run. Would post an update on {name}.")
                }
                (InitiativeUpdateAction::Edited, false) => {
                    writeln!(text, "Edited an update on {name}.")
                }
                (InitiativeUpdateAction::Edited, true) => {
                    writeln!(text, "Dry run. Would edit an update on {name}.")
                }
                (InitiativeUpdateAction::Deleted, false) => {
                    writeln!(text, "Deleted an update on {name}.")
                }
                (InitiativeUpdateAction::Deleted, true) => {
                    writeln!(text, "Dry run. Would delete an update on {name}.")
                }
            };
            update_text(text, &change.update);
        }
        WorkAnswer::InitiativeChange(change) => {
            if change.dry_run {
                text.push_str("Dry run. ");
            }
            let name = &change.initiative.name;
            let project = change
                .project
                .as_ref()
                .map_or("", |project| project.name.as_str());
            let _ = match (change.action, change.dry_run) {
                (InitiativeAction::Created, false) => writeln!(text, "Made initiative {name}."),
                (InitiativeAction::Created, true) => {
                    writeln!(text, "Would make initiative {name}.")
                }
                (InitiativeAction::Updated, false) => writeln!(text, "Saved initiative {name}."),
                (InitiativeAction::Updated, true) => {
                    writeln!(text, "Would save initiative {name}.")
                }
                (InitiativeAction::Unchanged, _) => writeln!(text, "Nothing to change in {name}."),
                (InitiativeAction::Deleted, false) => {
                    writeln!(
                        text,
                        "Deleted initiative {name}. Its projects stay, in no initiative."
                    )
                }
                (InitiativeAction::Deleted, true) => writeln!(
                    text,
                    "Would delete initiative {name}. Its projects would stay, in no initiative."
                ),
                (InitiativeAction::ProjectAdded, false) => {
                    writeln!(text, "Put {project} in {name}.")
                }
                (InitiativeAction::ProjectAdded, true) => {
                    writeln!(text, "Would put {project} in {name}.")
                }
                (InitiativeAction::ProjectRemoved, false) => {
                    writeln!(text, "Took {project} out of {name}.")
                }
                (InitiativeAction::ProjectRemoved, true) => {
                    writeln!(text, "Would take {project} out of {name}.")
                }
            };
            let _ = writeln!(text, "{}", line(&change.initiative));
        }
        _ => {}
    }
}
