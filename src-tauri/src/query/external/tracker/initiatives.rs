//! `sona work` verbs for initiatives: goals one level above projects.

use super::catalog::{ExternalWorkPersonRef, ExternalWorkProject};
use super::roadmap::ExternalProjectRef;
use super::{day_or_none, day_text, pick, refusal, Board, DayArg, Tracker, WorkAnswer, WriteArgs};
use crate::planning::work::initiatives::{
    self, Initiative, InitiativeSaveInput, InitiativeStatus,
};
use crate::planning::work::types::WorkOwner;
use crate::planning::{self, Project};
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum InitiativeState {
    Planned,
    Active,
    Completed,
}

impl From<InitiativeState> for InitiativeStatus {
    fn from(state: InitiativeState) -> Self {
        match state {
            InitiativeState::Planned => Self::Planned,
            InitiativeState::Active => Self::Active,
            InitiativeState::Completed => Self::Completed,
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

fn external(initiative: &Initiative, projects: &[Project], board: &Board) -> ExternalInitiative {
    let progress = initiative.progress;
    let tasks = progress.open_tasks + progress.done_tasks;
    ExternalInitiative {
        id: initiative.id.clone(),
        name: initiative.name.clone(),
        details: initiative.description.clone(),
        status: initiative.status,
        owner: initiative.owner_person_id.as_ref().map(|id| ExternalWorkPersonRef {
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
                name: projects
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
            percent: if tasks == 0 {
                0
            } else {
                (progress.done_tasks * 100 + tasks / 2) / tasks
            },
        },
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

    fn name_free(all: &[Initiative], name: &str, other_than: Option<&str>) -> Result<(), ExternalError> {
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

    fn initiative_answer(
        &self,
        initiative: &Initiative,
        board: &Board,
        dry_run: bool,
        operation_id: Option<String>,
        action: InitiativeAction,
        project: Option<&Project>,
    ) -> Result<WorkAnswer, ExternalError> {
        let projects = self.every_project()?;
        Ok(WorkAnswer::InitiativeChange(Box::new(
            ExternalWorkInitiativeChange {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run,
                operation_id,
                action,
                initiative: external(initiative, &projects, board),
                project: project.map(project_ref),
            },
        )))
    }

    pub(super) async fn initiative_list(&self) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let projects = self.every_project()?;
        Ok(WorkAnswer::Initiatives(ExternalWorkInitiatives {
            schema_version: QUERY_SCHEMA_VERSION,
            initiatives: self
                .all_initiatives()?
                .iter()
                .map(|initiative| external(initiative, &projects, &board))
                .collect(),
        }))
    }

    pub(super) async fn initiative_show(
        &self,
        args: &InitiativeArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let projects = self.every_project()?;
        let all = self.all_initiatives()?;
        let initiative = Self::pick_initiative(&args.initiative, &all)?;
        Ok(WorkAnswer::InitiativeDetail(Box::new(
            ExternalWorkInitiativeDetail {
                schema_version: QUERY_SCHEMA_VERSION,
                initiative: external(initiative, &projects, &board),
                projects: initiative
                    .project_ids
                    .iter()
                    .filter_map(|id| projects.iter().find(|project| &project.id == id))
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
        if !self.ran(&operation_id)? {
            Self::name_free(&self.all_initiatives()?, &args.name, None)?;
        }
        let input = InitiativeSaveInput {
            name: Some(args.name.clone()),
            description: args.details.clone().map(Some),
            owner: Self::owner(&board, args.owner.as_deref())?,
            target_at_ms: args.target.map(DayArg::ms).transpose()?,
            status: args.status.map(Into::into),
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
        };
        let after = self.rehearse(|conn| {
            initiatives::save(conn, Some(&current.id), &input, current.updated_at_ms)
        })?;
        if after == *current {
            return self.initiative_answer(current, &board, args.dry_run, None, InitiativeAction::Unchanged, None);
        }
        let operation_id = args
            .write
            .operation_id("initiative-update", &(&current.id, current.updated_at_ms, &input))?;
        let initiative = self.initiative_saved(Some(&current.id), &input, args.dry_run, &operation_id)?;
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
}

fn status_word(status: InitiativeStatus) -> &'static str {
    match status {
        InitiativeStatus::Planned => "Planned",
        InitiativeStatus::Active => "Active",
        InitiativeStatus::Completed => "Completed",
    }
}

fn line(initiative: &ExternalInitiative) -> String {
    let mut text = format!("{} · {}", initiative.name, status_word(initiative.status));
    if initiative.owner_is_self {
        text.push_str(" · owner you");
    } else if let Some(owner) = &initiative.owner {
        let _ = write!(text, " · owner {}", owner.name);
    }
    if let Some(target) = &initiative.target {
        let _ = write!(text, " · target {target}");
    }
    let progress = &initiative.progress;
    let _ = write!(
        text,
        " · {} of {} projects completed · {}% of tasks done",
        progress.completed_projects, progress.projects, progress.percent
    );
    text
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
        }
        WorkAnswer::InitiativeChange(change) => {
            if change.dry_run {
                text.push_str("Dry run. ");
            }
            let name = &change.initiative.name;
            let project = change.project.as_ref().map_or("", |project| project.name.as_str());
            let _ = match (change.action, change.dry_run) {
                (InitiativeAction::Created, false) => writeln!(text, "Made initiative {name}."),
                (InitiativeAction::Created, true) => writeln!(text, "Would make initiative {name}."),
                (InitiativeAction::Updated, false) => writeln!(text, "Saved initiative {name}."),
                (InitiativeAction::Updated, true) => writeln!(text, "Would save initiative {name}."),
                (InitiativeAction::Unchanged, _) => writeln!(text, "Nothing to change in {name}."),
                (InitiativeAction::Deleted, false) => {
                    writeln!(text, "Deleted initiative {name}. Its projects stay, in no initiative.")
                }
                (InitiativeAction::Deleted, true) => writeln!(
                    text,
                    "Would delete initiative {name}. Its projects would stay, in no initiative."
                ),
                (InitiativeAction::ProjectAdded, false) => writeln!(text, "Put {project} in {name}."),
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
