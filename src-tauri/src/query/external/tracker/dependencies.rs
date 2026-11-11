//! `sona work` verbs for projects that wait on other projects.

use super::roadmap::ExternalProjectRef;
use super::{now_ms, refusal, Tracker, WorkAnswer, WriteArgs};
use crate::planning::dependencies::{self, ProjectDependency};
use crate::planning::{self, work, Project};
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::fmt::Write as _;

/// Which way the other project points.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ProjectLink {
    /// The project waits for the other one to end.
    BlockedBy,
    /// The other project waits for this one to end.
    Blocking,
}

/// Which projects a dependency filter keeps.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum LinkFilter {
    /// Projects that wait on another project.
    Blocked,
    /// Projects another project waits on.
    Blocking,
    /// Projects with a broken dependency on either side.
    Broken,
}

impl LinkFilter {
    pub(super) fn keeps(self, project_id: &str, all: &[ProjectDependency]) -> bool {
        all.iter().any(|dependency| match self {
            Self::Blocked => dependency.blocked_id == project_id,
            Self::Blocking => dependency.blocker_id == project_id,
            Self::Broken => {
                dependency.broken
                    && (dependency.blocked_id == project_id || dependency.blocker_id == project_id)
            }
        })
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectRelateArgs {
    /// The project, by name or id.
    pub project: String,
    #[arg(value_enum)]
    pub link: ProjectLink,
    /// The other project, by name or id.
    pub other: String,
    /// Show the dependency without saving it.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectDependenciesArgs {
    /// Only the dependencies this project is on either side of.
    pub project: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectDependency {
    pub id: String,
    pub blocker: ExternalProjectRef,
    pub blocked: ExternalProjectRef,
    /// The blocker is still open and ends after the blocked project starts.
    pub broken: bool,
}

/// The other side of one dependency, seen from a project.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectDependencyLink {
    pub dependency_id: String,
    pub project: ExternalProjectRef,
    pub broken: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectDependencies {
    pub schema_version: u32,
    pub dependencies: Vec<ExternalProjectDependency>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectDependencyChange {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: String,
    pub removed: bool,
    /// The link was there already, so nothing changed.
    pub already: bool,
    pub dependency: ExternalProjectDependency,
}

fn project_ref(id: &str, projects: &[Project]) -> ExternalProjectRef {
    ExternalProjectRef {
        id: id.to_owned(),
        name: projects
            .iter()
            .find(|project| project.id == id)
            .map_or_else(|| id.to_owned(), |project| project.title.clone()),
    }
}

pub(super) fn external(
    dependency: &ProjectDependency,
    projects: &[Project],
) -> ExternalProjectDependency {
    ExternalProjectDependency {
        id: dependency.id.clone(),
        blocker: project_ref(&dependency.blocker_id, projects),
        blocked: project_ref(&dependency.blocked_id, projects),
        broken: dependency.broken,
    }
}

/// What `project_id` waits on, then what waits on it.
pub(super) fn links(
    all: &[ProjectDependency],
    project_id: &str,
    projects: &[Project],
) -> (
    Vec<ExternalProjectDependencyLink>,
    Vec<ExternalProjectDependencyLink>,
) {
    let link = |dependency: &ProjectDependency, other: &str| ExternalProjectDependencyLink {
        dependency_id: dependency.id.clone(),
        project: project_ref(other, projects),
        broken: dependency.broken,
    };
    let blocked_by = all
        .iter()
        .filter(|dependency| dependency.blocked_id == project_id)
        .map(|dependency| link(dependency, &dependency.blocker_id))
        .collect();
    let blocking = all
        .iter()
        .filter(|dependency| dependency.blocker_id == project_id)
        .map(|dependency| link(dependency, &dependency.blocked_id))
        .collect();
    (blocked_by, blocking)
}

impl Tracker<'_> {
    pub(super) fn all_dependencies(&self) -> Result<Vec<ProjectDependency>, ExternalError> {
        planning::history(self.history, |conn| dependencies::dependencies(conn))
            .map_err(|message| refusal(self.history, message))
    }

    /// The blocker, then the project it blocks.
    fn ends<'a>(
        args: &ProjectRelateArgs,
        projects: &'a [Project],
    ) -> Result<(&'a Project, &'a Project), ExternalError> {
        let project = Self::pick_project(&args.project, projects)?;
        let other = Self::pick_project(&args.other, projects)?;
        Ok(match args.link {
            ProjectLink::BlockedBy => (other, project),
            ProjectLink::Blocking => (project, other),
        })
    }

    pub(super) fn project_dependencies(
        &self,
        args: &ProjectDependenciesArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let projects = self.every_project()?;
        let only = args
            .project
            .as_deref()
            .map(|value| Self::pick_project(value, &projects))
            .transpose()?;
        let dependencies = self
            .all_dependencies()?
            .iter()
            .filter(|dependency| {
                only.is_none_or(|project| {
                    dependency.blocker_id == project.id || dependency.blocked_id == project.id
                })
            })
            .map(|dependency| external(dependency, &projects))
            .collect();
        Ok(WorkAnswer::ProjectDependencies(
            ExternalWorkProjectDependencies {
                schema_version: QUERY_SCHEMA_VERSION,
                dependencies,
            },
        ))
    }

    pub(super) fn project_relate(&self, args: &ProjectRelateArgs) -> Result<WorkAnswer, ExternalError> {
        let projects = self.every_project()?;
        let (blocker, blocked) = Self::ends(args, &projects)?;
        let operation_id = args.write.fresh_operation_id();
        if let Some(existing) = self.all_dependencies()?.into_iter().find(|dependency| {
            dependency.blocker_id == blocker.id && dependency.blocked_id == blocked.id
        }) {
            return Ok(change(&existing, &projects, args.dry_run, operation_id, Change::Already));
        }
        let dependency = if args.dry_run {
            self.rehearse(|conn| dependencies::add(conn, &blocker.id, &blocked.id, now_ms()))?
        } else {
            self.applies("making one project wait on another")?;
            work::catalog_dependency_add(self.app, &operation_id, &blocker.id, &blocked.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The project"))?
        };
        Ok(change(&dependency, &projects, args.dry_run, operation_id, Change::Added))
    }

    pub(super) fn project_unrelate(
        &self,
        args: &ProjectRelateArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let projects = self.every_project()?;
        let (blocker, blocked) = Self::ends(args, &projects)?;
        let found = self
            .all_dependencies()?
            .into_iter()
            .find(|dependency| {
                dependency.blocker_id == blocker.id && dependency.blocked_id == blocked.id
            })
            .ok_or_else(|| {
                ExternalError::new(
                    ExternalErrorCode::NotFound,
                    format!("{} does not wait on {}.", blocked.title, blocker.title),
                )
            })?;
        let operation_id = args.write.operation_id("project-unrelate", &found.id)?;
        let dependency = if args.dry_run {
            found
        } else {
            self.applies("taking a project dependency off")?;
            work::catalog_dependency_remove(self.app, &operation_id, &found.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The project"))?
        };
        Ok(change(&dependency, &projects, args.dry_run, operation_id, Change::Removed))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Change {
    Added,
    Already,
    Removed,
}

fn change(
    dependency: &ProjectDependency,
    projects: &[Project],
    dry_run: bool,
    operation_id: String,
    kind: Change,
) -> WorkAnswer {
    WorkAnswer::ProjectDependency(Box::new(ExternalWorkProjectDependencyChange {
        schema_version: QUERY_SCHEMA_VERSION,
        dry_run,
        operation_id,
        removed: kind == Change::Removed,
        already: kind == Change::Already,
        dependency: external(dependency, projects),
    }))
}

pub(super) fn line(dependency: &ExternalProjectDependency) -> String {
    format!(
        "{} waits on {}{}",
        dependency.blocked.name,
        dependency.blocker.name,
        if dependency.broken {
            " · broken: it ends after this starts"
        } else {
            ""
        }
    )
}

pub(super) fn links_text(text: &mut String, label: &str, links: &[ExternalProjectDependencyLink]) {
    if links.is_empty() {
        return;
    }
    let names: Vec<String> = links
        .iter()
        .map(|link| {
            if link.broken {
                format!("{} (broken)", link.project.name)
            } else {
                link.project.name.clone()
            }
        })
        .collect();
    let _ = writeln!(text, "{label}: {}", names.join(", "));
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::ProjectDependencies(list) => {
            if list.dependencies.is_empty() {
                text.push_str("No project waits on another.\n");
            }
            for dependency in &list.dependencies {
                let _ = writeln!(text, "{}", line(dependency));
            }
        }
        WorkAnswer::ProjectDependency(change) => {
            let verb = match (change.dry_run, change.removed, change.already) {
                (_, _, true) => "Already linked",
                (true, false, false) => "Would add",
                (true, true, false) => "Would remove",
                (false, false, false) => "Added",
                (false, true, false) => "Removed",
            };
            let _ = writeln!(text, "{verb}: {}", line(&change.dependency));
        }
        _ => {}
    }
}
