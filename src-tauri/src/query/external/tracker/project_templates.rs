//! `sona work` verbs for project templates: a project's description, lead,
//! members, milestones and tasks, kept to start new projects from.

use super::catalog::ExternalWorkProject;
use super::{
    day_text, handle, now_ms, pick, refusal, Board, ExternalWorkRef, Priority, Tracker, WorkAnswer,
    WriteArgs,
};
use crate::planning;
use crate::planning::work::project_templates::{self, ProjectFromTemplate, ProjectTemplate};
use crate::planning::work::types::WorkActor;
use crate::query::external::ExternalError;
use crate::query::QUERY_SCHEMA_VERSION;
use chrono::{Local, NaiveDate};
use clap::Args;
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectTemplateSaveArgs {
    /// The template's name. Saving under a name already used refills that
    /// template.
    pub name: String,
    /// The project to copy, by name or id.
    #[arg(long)]
    pub project: String,
    /// Show the template without saving it.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectTemplateDeleteArgs {
    /// The template, by name or id.
    pub template: String,
    /// Show what would be deleted without deleting it.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectNewArgs {
    /// The new project's name.
    pub name: String,
    /// The template to start from, by name or id.
    #[arg(long)]
    pub template: String,
    /// The project's start day: YYYY-MM-DD, today or tomorrow. Milestones
    /// fall their template's days after it. Today unless told.
    #[arg(long, value_parser = super::day, value_name = "DAY")]
    pub start: Option<NaiveDate>,
    /// Show the project without making it.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectTemplateMilestone {
    pub name: String,
    pub details: Option<String>,
    /// Days after the project's start it is due; none leaves it undated.
    pub offset_days: Option<i32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectTemplateTask {
    pub title: String,
    pub details: Option<String>,
    pub labels: Vec<String>,
    pub estimate: Option<u8>,
    pub priority: Priority,
    pub milestone: Option<String>,
    /// The place in `tasks` of the task this one sits under.
    pub parent: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectTemplate {
    pub id: String,
    pub name: String,
    pub details: Option<String>,
    pub lead: String,
    pub members: Vec<String>,
    pub milestones: Vec<ExternalProjectTemplateMilestone>,
    pub tasks: Vec<ExternalProjectTemplateTask>,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectTemplates {
    pub schema_version: u32,
    pub templates: Vec<ExternalProjectTemplate>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectTemplateChange {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: String,
    pub deleted: bool,
    pub template: ExternalProjectTemplate,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectMilestoneMade {
    pub id: String,
    pub name: String,
    pub target: Option<String>,
    pub target_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProjectNew {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: String,
    pub template: String,
    pub project: ExternalWorkProject,
    pub milestones: Vec<ExternalProjectMilestoneMade>,
    pub tasks: Vec<ExternalWorkRef>,
}

fn external(template: &ProjectTemplate, board: &Board) -> ExternalProjectTemplate {
    let mut fields = template.fields.clone();
    fields.leave_out(&|kind, id| board.fence.hides_source(kind, id));
    let mut members: Vec<String> = fields
        .member_person_ids
        .iter()
        .map(|id| board.person_name(id))
        .collect();
    if fields.members_include_self {
        members.insert(0, "you".to_owned());
    }
    ExternalProjectTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        details: fields.description.clone(),
        lead: board.owner_text(&fields.lead),
        members,
        milestones: fields
            .milestones
            .iter()
            .map(|milestone| ExternalProjectTemplateMilestone {
                name: milestone.title.clone(),
                details: milestone.description.clone(),
                offset_days: milestone.offset_days,
            })
            .collect(),
        tasks: fields
            .tasks
            .iter()
            .map(|task| ExternalProjectTemplateTask {
                title: task.title.clone(),
                details: task.details.clone(),
                labels: task.label_ids.iter().map(|id| board.label_name(id)).collect(),
                estimate: task.estimate,
                priority: Priority::of(task.priority),
                milestone: task.milestone.clone(),
                parent: task.parent,
            })
            .collect(),
        revision: template.revision,
    }
}

impl Tracker<'_> {
    fn all_project_templates(&self) -> Result<Vec<ProjectTemplate>, ExternalError> {
        planning::history(self.history, |conn| project_templates::templates(conn))
            .map_err(|message| refusal(self.history, message))
    }

    fn project_template_named<'t>(
        templates: &'t [ProjectTemplate],
        value: &str,
    ) -> Result<&'t ProjectTemplate, ExternalError> {
        pick("template", value, templates, |t| &t.id, |t| &t.name)
            .map_err(|refusal| refusal.hint("List them with: sona work project-template-list"))
    }

    pub(super) async fn project_template_list(&self) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let templates = self.all_project_templates()?;
        Ok(WorkAnswer::ProjectTemplates(ExternalWorkProjectTemplates {
            schema_version: QUERY_SCHEMA_VERSION,
            templates: templates
                .iter()
                .map(|template| external(template, &board))
                .collect(),
        }))
    }

    pub(super) async fn project_template_save(
        &self,
        args: &ProjectTemplateSaveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let name = args.name.trim();
        if name.is_empty() {
            return Err(ExternalError::invalid("A template needs a name."));
        }
        let board = self.board().await?;
        let projects = self.every_project()?;
        let project = Self::pick_project(&args.project, &projects)?;
        let operation_id = args.write.fresh_operation_id();
        let hides = |kind, id: &str| board.fence.hides_source(kind, id);
        let template = if args.dry_run {
            self.rehearse(|conn| {
                project_templates::save_from_project(conn, name, &project.id, &hides, now_ms())
            })?
        } else {
            self.applies("saving a project as a template")?;
            project_templates::template_save_as(self.app, &operation_id, name, &project.id)
                .await
                .map_err(|message| self.write_refusal(message, &operation_id, "The template"))?
        };
        Ok(change(&template, &board, args.dry_run, operation_id, false))
    }

    pub(super) async fn project_template_delete(
        &self,
        args: &ProjectTemplateDeleteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let templates = self.all_project_templates()?;
        let template = Self::project_template_named(&templates, &args.template)?;
        let operation_id = args
            .write
            .operation_id("project-template-delete", &(&template.id, template.revision))?;
        if !args.dry_run {
            self.applies("deleting a project template")?;
            project_templates::template_delete_as(self.app, &operation_id, &template.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The template"))?;
        }
        Ok(change(template, &board, args.dry_run, operation_id, true))
    }

    pub(super) async fn project_new(&self, args: &ProjectNewArgs) -> Result<WorkAnswer, ExternalError> {
        let name = args.name.trim();
        if name.is_empty() {
            return Err(ExternalError::invalid("A project needs a name."));
        }
        let board = self.board().await?;
        let templates = self.all_project_templates()?;
        let template = Self::project_template_named(&templates, &args.template)?;
        let operation_id = args.write.fresh_operation_id();
        if !self.ran(&operation_id)? {
            if let Some(taken) = self
                .every_project()?
                .iter()
                .find(|project| project.title.to_lowercase() == name.to_lowercase())
            {
                return Err(ExternalError::invalid(format!(
                    "A project called {:?} already exists.",
                    taken.title
                )));
            }
        }
        let start_at_ms =
            super::midnight(args.start.unwrap_or_else(|| Local::now().date_naive()))?;
        let made = if args.dry_run {
            self.rehearse(|conn| {
                project_templates::rehearse_project(
                    conn,
                    &|kind, id| board.fence.hides_source(kind, id),
                    &template.id,
                    name,
                    start_at_ms,
                )
            })?
        } else {
            self.applies("making a project from a template")?;
            let started_ms = now_ms();
            let made = project_templates::project_from_template_as(
                self.app,
                WorkActor::Agent,
                &operation_id,
                &template.id,
                name,
                start_at_ms,
            )
            .await
            .map_err(|message| self.write_refusal(message, &operation_id, "The project"))?;
            let ids: Vec<String> = made.tasks.iter().map(|task| task.id.clone()).collect();
            self.credit(&args.write, &ids, started_ms)?;
            made
        };
        Ok(project_new(made, &template.name, &board, self, args.dry_run, operation_id))
    }
}

fn change(
    template: &ProjectTemplate,
    board: &Board,
    dry_run: bool,
    operation_id: String,
    deleted: bool,
) -> WorkAnswer {
    WorkAnswer::ProjectTemplate(Box::new(ExternalWorkProjectTemplateChange {
        schema_version: QUERY_SCHEMA_VERSION,
        dry_run,
        operation_id,
        deleted,
        template: external(template, board),
    }))
}

fn project_new(
    made: ProjectFromTemplate,
    template: &str,
    board: &Board,
    tracker: &Tracker<'_>,
    dry_run: bool,
    operation_id: String,
) -> WorkAnswer {
    WorkAnswer::ProjectNew(Box::new(ExternalWorkProjectNew {
        schema_version: QUERY_SCHEMA_VERSION,
        dry_run,
        operation_id,
        template: template.to_owned(),
        project: tracker.project_row(&made.project, board),
        milestones: made
            .milestones
            .into_iter()
            .map(|milestone| ExternalProjectMilestoneMade {
                id: milestone.id,
                name: milestone.title,
                target: milestone.due_at_ms.map(|ms| day_text(Some(ms))),
                target_at_ms: milestone.due_at_ms,
            })
            .collect(),
        tasks: made
            .tasks
            .into_iter()
            .map(|task| ExternalWorkRef {
                id: task.id,
                handle: handle(task.number),
                title: task.title,
                status: task.status,
            })
            .collect(),
    }))
}

fn template_text(text: &mut String, template: &ExternalProjectTemplate) {
    let _ = writeln!(
        text,
        "{} · lead {} · {} milestones · {} tasks",
        template.name,
        template.lead,
        template.milestones.len(),
        template.tasks.len()
    );
    for milestone in &template.milestones {
        let due = milestone
            .offset_days
            .map_or_else(|| "no target".to_owned(), |days| format!("day {days}"));
        let _ = writeln!(text, "  milestone {} · {due}", milestone.name);
    }
    for task in &template.tasks {
        let mut extra = Vec::new();
        if let Some(parent) = task.parent.and_then(|place| template.tasks.get(place)) {
            extra.push(format!("under {}", parent.title));
        }
        if let Some(milestone) = &task.milestone {
            extra.push(milestone.clone());
        }
        if !task.labels.is_empty() {
            extra.push(task.labels.join(", "));
        }
        if task.priority != Priority::None {
            extra.push(task.priority.text().to_owned());
        }
        if let Some(points) = task.estimate {
            extra.push(format!("{points} points"));
        }
        if extra.is_empty() {
            let _ = writeln!(text, "  task {}", task.title);
        } else {
            let _ = writeln!(text, "  task {} · {}", task.title, extra.join(" · "));
        }
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::ProjectTemplates(list) => {
            if list.templates.is_empty() {
                text.push_str(
                    "No project templates. Save one with: sona work project-template-save NAME --project PROJECT\n",
                );
            }
            for template in &list.templates {
                template_text(text, template);
            }
        }
        WorkAnswer::ProjectTemplate(change) => {
            let verb = match (change.dry_run, change.deleted) {
                (true, false) => "Would save",
                (true, true) => "Would delete",
                (false, false) => "Saved",
                (false, true) => "Deleted",
            };
            let _ = write!(text, "{verb} template ");
            template_text(text, &change.template);
        }
        WorkAnswer::ProjectNew(made) => {
            let verb = if made.dry_run { "Would make" } else { "Made" };
            let _ = writeln!(
                text,
                "{verb} {} from {} · starts {}",
                made.project.name,
                made.template,
                made.project.start.as_deref().unwrap_or("none")
            );
            for milestone in &made.milestones {
                let _ = writeln!(
                    text,
                    "  milestone {} · {}",
                    milestone.name,
                    milestone.target.as_deref().unwrap_or("no target")
                );
            }
            for task in &made.tasks {
                let _ = writeln!(text, "  {} {}", task.handle, task.title);
            }
        }
        _ => {}
    }
}
