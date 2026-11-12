//! Project templates: a project's description, lead and members, its
//! milestones as days after its start, and its tasks with their labels and
//! estimates, kept by name to make new projects from on a chosen start day.
//! Work drawn from a meeting kept on this Mac stays out of them.

use super::items::{self, NewItem, MAX_ESTIMATE};
use super::types::{WorkActivityKind, WorkActor, WorkItem, WorkOwner, WorkPatch, WorkStatus};
use super::{now_ms, run_history, run_with_meetings, Change, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use crate::planning::catalog::{self, MilestoneEdit, ProjectEdit};
use crate::planning::sources;
use crate::planning::store::{self, Refusal};
use crate::planning::types::{EvidenceSourceKind, Milestone, Project};
use crate::query::KeptLocal;
use anyhow::Result;
use chrono::{NaiveDate, TimeDelta};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

/// Templates are named uniquely, ignoring case; `fields` is
/// [`ProjectTemplateFields`] as JSON.
pub(crate) const PROJECT_TEMPLATES_MIGRATION: &str = "CREATE TABLE planning_project_templates (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    fields TEXT NOT NULL,
    revision INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX planning_project_templates_name ON planning_project_templates(lower(name));";

/// A milestone a template makes, due `offset_days` after the new project's
/// start day; none leaves it without a target day.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTemplateMilestone {
    pub title: String,
    pub description: Option<String>,
    pub offset_days: Option<i32>,
}

/// A task a template makes, under the template's milestone of that title.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTemplateTask {
    pub title: String,
    pub details: Option<String>,
    #[serde(default)]
    pub label_ids: Vec<String>,
    pub estimate: Option<u8>,
    /// 0 for none, then 1 urgent to 4 low, as a task's.
    #[serde(default)]
    pub priority: u8,
    pub milestone: Option<String>,
    /// The task it sits under, by its place in the template's tasks.
    #[serde(default)]
    pub parent: Option<usize>,
    /// The meetings and meeting commitments the task was saved from.
    #[serde(default)]
    pub drawn_from: Vec<ProjectTemplateSource>,
}

/// A meeting, or a commitment in one, that a template's task was drawn
/// from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTemplateSource {
    pub kind: EvidenceSourceKind,
    pub id: String,
}

impl ProjectTemplateTask {
    /// Whether `hides` holds back a source the task was drawn from.
    pub(crate) fn drawn_from_any(&self, hides: &dyn Fn(EvidenceSourceKind, &str) -> bool) -> bool {
        self.drawn_from.iter().any(|source| hides(source.kind, &source.id))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTemplateFields {
    pub description: Option<String>,
    pub lead: WorkOwner,
    #[serde(default)]
    pub member_person_ids: Vec<String>,
    #[serde(default)]
    pub members_include_self: bool,
    #[serde(default)]
    pub milestones: Vec<ProjectTemplateMilestone>,
    #[serde(default)]
    pub tasks: Vec<ProjectTemplateTask>,
}

impl ProjectTemplateFields {
    /// Leaves out each task drawn from a source `hides` holds back; a
    /// sub-task of one left out becomes a task of its own.
    fn leave_out(&mut self, hides: &dyn Fn(EvidenceSourceKind, &str) -> bool) {
        let mut places = Vec::with_capacity(self.tasks.len());
        let mut kept = 0;
        for task in &self.tasks {
            if task.drawn_from_any(hides) {
                places.push(None);
            } else {
                places.push(Some(kept));
                kept += 1;
            }
        }
        let mut tasks = Vec::with_capacity(kept);
        for (mut task, place) in std::mem::take(&mut self.tasks).into_iter().zip(&places) {
            if place.is_some() {
                task.parent = task
                    .parent
                    .and_then(|parent| places.get(parent).copied().flatten());
                tasks.push(task);
            }
        }
        self.tasks = tasks;
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTemplate {
    pub id: String,
    pub name: String,
    pub fields: ProjectTemplateFields,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// A project made from a template, with the milestones and tasks it made.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFromTemplate {
    pub project: Project,
    pub milestones: Vec<Milestone>,
    pub tasks: Vec<WorkItem>,
}

const COLUMNS: &str = "id, name, fields, revision, created_at_ms, updated_at_ms";

type TemplateRow = (String, String, String, i64, i64, i64);

fn read(row: &Row<'_>) -> rusqlite::Result<TemplateRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn template_of(
    (id, name, fields, revision, created_at_ms, updated_at_ms): TemplateRow,
) -> Result<ProjectTemplate> {
    Ok(ProjectTemplate {
        id,
        name,
        fields: serde_json::from_str(&fields)?,
        revision,
        created_at_ms,
        updated_at_ms,
    })
}

pub(crate) fn templates(conn: &Connection) -> Result<Vec<ProjectTemplate>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM planning_project_templates ORDER BY lower(name), id"
    ))?;
    let rows = statement
        .query_map([], read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(template_of).collect()
}

pub(crate) fn template(conn: &Connection, id: &str) -> Result<ProjectTemplate> {
    let row = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM planning_project_templates WHERE id = ?1"),
            [id],
            read,
        )
        .optional()?
        .ok_or(Refusal::NotFound)?;
    template_of(row)
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Calendar days on this Mac's clock from the day of `from_ms` to the day
/// of `to_ms`.
fn days_between(from_ms: i64, to_ms: i64) -> Result<i32> {
    let days = (items::local_today(to_ms)? - items::local_today(from_ms)?).num_days();
    Ok(i32::try_from(days)?)
}

fn lead(project: &Project) -> WorkOwner {
    match (&project.lead_person_id, project.lead_is_self) {
        (_, true) => WorkOwner::Myself,
        (Some(person_id), false) => WorkOwner::Person {
            person_id: person_id.clone(),
        },
        (None, false) => WorkOwner::Nobody,
    }
}

/// The meetings and meeting commitments `task` was drawn from.
fn drawn_from(conn: &Connection, task: &WorkItem) -> Result<Vec<ProjectTemplateSource>> {
    let mut sources: Vec<ProjectTemplateSource> = task
        .source_loop_id
        .iter()
        .map(|id| ProjectTemplateSource {
            kind: EvidenceSourceKind::Loop,
            id: id.clone(),
        })
        .collect();
    let mut statement = conn.prepare_cached(
        "SELECT source_kind, source_id FROM planning_evidence_links
          WHERE target_kind = 'task' AND target_id = ?1 AND source_kind IN ('meeting', 'loop')",
    )?;
    for source in statement.query_map([&task.id], |row| {
        Ok(ProjectTemplateSource {
            kind: row.get(0)?,
            id: row.get(1)?,
        })
    })? {
        sources.push(source?);
    }
    Ok(sources)
}

/// What a template of the project holds. Milestone offsets count from its
/// start day, or from the day it was made when it has none. Tasks waiting
/// in triage, canceled, duplicate and binned are left out, and so is each
/// drawn from a source `hides` holds back; a sub-task stays under its
/// parent when the template holds that too.
pub(crate) fn fields_of(
    conn: &Connection,
    project_id: &str,
    hides: &dyn Fn(EvidenceSourceKind, &str) -> bool,
) -> Result<ProjectTemplateFields> {
    let records = store::records(conn, &store::uuid(project_id)?)?;
    let project = &records.project;
    let start = project.start_at_ms.unwrap_or(project.created_at_ms);
    let milestones = records
        .milestones
        .iter()
        .map(|milestone| {
            Ok(ProjectTemplateMilestone {
                title: milestone.title.clone(),
                description: milestone.description.clone(),
                offset_days: milestone
                    .due_at_ms
                    .map(|due| days_between(start, due))
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let titles: HashMap<&str, &str> = records
        .milestones
        .iter()
        .map(|milestone| (milestone.id.as_str(), milestone.title.as_str()))
        .collect();
    let mut statement = conn.prepare(
        "SELECT id FROM planning_tasks
          WHERE project_id = ?1 AND trashed_at_ms IS NULL
            AND status NOT IN ('triage', 'canceled', 'duplicate')
          ORDER BY number",
    )?;
    let ids = statement
        .query_map([&project.id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    let mut kept = Vec::with_capacity(ids.len());
    for id in &ids {
        let item = items::item(conn, id)?;
        let task = ProjectTemplateTask {
            milestone: item
                .milestone_id
                .as_deref()
                .and_then(|id| titles.get(id))
                .map(|title| (*title).to_owned()),
            drawn_from: drawn_from(conn, &item)?,
            title: item.title,
            details: item.details,
            label_ids: item.label_ids,
            estimate: item.estimate,
            priority: item.priority,
            parent: None,
        };
        if !task.drawn_from_any(hides) {
            kept.push((id.as_str(), item.parent_id, task));
        }
    }
    let places: HashMap<&str, usize> = kept
        .iter()
        .enumerate()
        .map(|(place, (id, ..))| (*id, place))
        .collect();
    let tasks = kept
        .into_iter()
        .map(|(_, parent_id, task)| ProjectTemplateTask {
            parent: parent_id.and_then(|parent_id| places.get(parent_id.as_str()).copied()),
            ..task
        })
        .collect();
    Ok(ProjectTemplateFields {
        description: project.description.clone(),
        lead: lead(project),
        member_person_ids: project.member_person_ids.clone(),
        members_include_self: project.members_include_self,
        milestones,
        tasks,
    })
}

/// Refuses a blank title, two milestones of one title, an estimate past
/// the scale, a task under a milestone the template does not make, or one
/// under a task it does not hold.
fn cleaned(fields: &ProjectTemplateFields) -> Result<ProjectTemplateFields> {
    let mut milestones: Vec<ProjectTemplateMilestone> = Vec::with_capacity(fields.milestones.len());
    for milestone in &fields.milestones {
        let title = trimmed(Some(&milestone.title)).ok_or(Refusal::Invalid)?;
        if milestones
            .iter()
            .any(|kept| kept.title.to_lowercase() == title.to_lowercase())
        {
            return Err(Refusal::Invalid.into());
        }
        milestones.push(ProjectTemplateMilestone {
            title,
            description: trimmed(milestone.description.as_deref()),
            offset_days: milestone.offset_days,
        });
    }
    let tasks = fields
        .tasks
        .iter()
        .map(|task| {
            if task.estimate.is_some_and(|points| points > MAX_ESTIMATE)
                || task.priority > 4
                || task.parent.is_some_and(|parent| parent >= fields.tasks.len())
            {
                return Err(Refusal::Invalid.into());
            }
            let milestone = match trimmed(task.milestone.as_deref()) {
                Some(title) => Some(
                    milestones
                        .iter()
                        .find(|kept| kept.title.to_lowercase() == title.to_lowercase())
                        .ok_or(Refusal::Invalid)?
                        .title
                        .clone(),
                ),
                None => None,
            };
            let mut label_ids = Vec::with_capacity(task.label_ids.len());
            for id in &task.label_ids {
                let id = store::uuid(id)?;
                if !label_ids.contains(&id) {
                    label_ids.push(id);
                }
            }
            Ok(ProjectTemplateTask {
                title: trimmed(Some(&task.title)).ok_or(Refusal::Invalid)?,
                details: trimmed(task.details.as_deref()),
                label_ids,
                estimate: task.estimate,
                priority: task.priority,
                milestone,
                parent: task.parent,
                drawn_from: task.drawn_from.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut member_person_ids = fields.member_person_ids.clone();
    member_person_ids.sort();
    member_person_ids.dedup();
    Ok(ProjectTemplateFields {
        description: trimmed(fields.description.as_deref()),
        lead: fields.lead.clone(),
        member_person_ids,
        members_include_self: fields.members_include_self,
        milestones,
        tasks,
    })
}

/// Saves `fields` as the template called `name`, refilling the one of that
/// name, ignoring case, when there is one.
pub(crate) fn save_template(
    conn: &Connection,
    name: &str,
    fields: &ProjectTemplateFields,
    now_ms: i64,
) -> Result<ProjectTemplate> {
    let name = trimmed(Some(name)).ok_or(Refusal::Invalid)?;
    let fields = serde_json::to_string(&cleaned(fields)?)?;
    let existing = templates(conn)?
        .into_iter()
        .find(|template| template.name.to_lowercase() == name.to_lowercase());
    let id = existing.map_or_else(|| Uuid::new_v4().to_string(), |template| template.id);
    conn.execute(
        "INSERT INTO planning_project_templates
            (id, name, fields, revision, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, fields = excluded.fields,
            revision = revision + 1, updated_at_ms = excluded.updated_at_ms",
        params![id, name, fields, now_ms],
    )?;
    template(conn, &id)
}

/// Saves the project as the template called `name`, leaving out work drawn
/// from a source `hides` holds back.
pub(crate) fn save_from_project(
    conn: &Connection,
    name: &str,
    project_id: &str,
    hides: &dyn Fn(EvidenceSourceKind, &str) -> bool,
    now_ms: i64,
) -> Result<ProjectTemplate> {
    save_template(conn, name, &fields_of(conn, project_id, hides)?, now_ms)
}

pub(crate) fn delete_template(conn: &Connection, id: &str) -> Result<ProjectTemplate> {
    let found = template(conn, &store::uuid(id)?)?;
    conn.execute(
        "DELETE FROM planning_project_templates WHERE id = ?1",
        [&found.id],
    )?;
    Ok(found)
}

/// The labels a template's task can still take: there, not retired, and
/// not kept to projects.
fn usable_labels(conn: &Connection, ids: &[String]) -> Result<Vec<String>> {
    let mut statement = conn.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM planning_labels
          WHERE id = ?1 AND retired_at_ms IS NULL AND scope != 'projects')",
    )?;
    let mut usable = Vec::with_capacity(ids.len());
    for id in ids {
        if statement.query_row([id], |row| row.get::<_, bool>(0))? {
            usable.push(id.clone());
        }
    }
    Ok(usable)
}

fn day_after(start: NaiveDate, days: i32) -> Result<i64> {
    let day = start
        .checked_add_signed(TimeDelta::days(i64::from(days)))
        .ok_or(Refusal::Invalid)?;
    items::local_midnight_ms(day)
}

/// The template's lead and members as they are now: someone merged into
/// another person under that person, and someone since deleted left off.
/// Without the conversations they stay as saved.
fn present_people(
    meetings: Option<&Connection>,
    fields: &ProjectTemplateFields,
) -> Result<(WorkOwner, Vec<String>)> {
    let Some(meetings) = meetings else {
        return Ok((fields.lead.clone(), fields.member_person_ids.clone()));
    };
    let lead = match &fields.lead {
        WorkOwner::Person { person_id } => sources::person(meetings, person_id)?
            .map_or(WorkOwner::Nobody, |person_id| WorkOwner::Person { person_id }),
        other => other.clone(),
    };
    let mut members = Vec::with_capacity(fields.member_person_ids.len());
    for id in &fields.member_person_ids {
        if let Some(id) = sources::person(meetings, id)? {
            if !members.contains(&id) {
                members.push(id);
            }
        }
    }
    Ok((lead, members))
}

/// Makes a project called `title` from a template, starting on the local
/// day of `start_at_ms`: its description, lead and members as
/// [`present_people`] finds them, each milestone due its offset after that
/// day, and each task, not started and owned by nobody, in its milestone
/// and under its parent, with its labels and estimate. A task drawn from a
/// source `hides` holds back is left out, and so are labels since deleted,
/// retired or kept to projects; a sub-task of a task left out becomes a
/// task of its own.
fn make_project(
    tx: &Connection,
    meetings: Option<&Connection>,
    hides: &dyn Fn(EvidenceSourceKind, &str) -> bool,
    change: Change<'_>,
    template_id: &str,
    title: &str,
    start_at_ms: i64,
    effects: &mut Effects,
) -> Result<ProjectFromTemplate> {
    let start = items::local_today(start_at_ms)?;
    let mut template = template(tx, &store::uuid(template_id)?)?;
    template.fields.leave_out(hides);
    let fields = &template.fields;
    let (lead, member_person_ids) = present_people(meetings, fields)?;
    let project = catalog::save_project(
        tx,
        None,
        &ProjectEdit {
            title: Some(title.to_owned()),
            description: fields.description.clone(),
            lead: Some(lead),
            member_person_ids: Some(member_person_ids),
            members_include_self: Some(fields.members_include_self),
            start_at_ms: Some(Some(items::local_midnight_ms(start)?)),
            ..ProjectEdit::default()
        },
        change.at_ms,
    )?;
    effects.projects.push(project.id.clone());
    let mut milestones = Vec::with_capacity(fields.milestones.len());
    for milestone in &fields.milestones {
        milestones.push(catalog::save_milestone(
            tx,
            &project.id,
            None,
            &MilestoneEdit {
                title: Some(milestone.title.clone()),
                description: milestone.description.clone(),
                due_at_ms: Some(
                    milestone
                        .offset_days
                        .map(|days| day_after(start, days))
                        .transpose()?,
                ),
                status: None,
            },
            change.at_ms,
        )?);
    }
    let mut tasks = Vec::with_capacity(fields.tasks.len());
    for task in &fields.tasks {
        let milestone_id = task.milestone.as_deref().and_then(|title| {
            milestones
                .iter()
                .find(|milestone| milestone.title.to_lowercase() == title.to_lowercase())
                .map(|milestone| milestone.id.as_str())
        });
        let label_ids = usable_labels(tx, &task.label_ids)?;
        let made = items::insert(
            tx,
            &NewItem {
                title: &task.title,
                details: task.details.as_deref(),
                status: WorkStatus::NotStarted,
                priority: task.priority,
                project_id: Some(&project.id),
                milestone_id,
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: &label_ids,
                source_loop_id: None,
                recurrence: None,
            },
            change.actor,
            change.at_ms,
        )?;
        let made = match task.estimate {
            Some(points) => {
                items::apply(
                    tx,
                    &made.id,
                    &WorkPatch {
                        estimate: Some(Some(points)),
                        ..WorkPatch::default()
                    },
                    WorkActivityKind::Estimate,
                    change.actor,
                    change.at_ms,
                )?
                .after
            }
            None => made,
        };
        effects.item(&made);
        tasks.push(made);
    }
    for (place, task) in fields.tasks.iter().enumerate() {
        if let Some(parent) = task.parent {
            tasks[place] = items::apply(
                tx,
                &tasks[place].id,
                &WorkPatch {
                    parent_id: Some(Some(tasks[parent].id.clone())),
                    ..WorkPatch::default()
                },
                WorkActivityKind::Status,
                change.actor,
                change.at_ms,
            )?
            .after;
        }
    }
    Ok(ProjectFromTemplate {
        project: store::project(tx, &project.id)?,
        milestones,
        tasks,
    })
}

/// What making a project from a template as an agent would leave, for a
/// dry run inside a transaction that is never committed. Its lead and
/// members are shown as saved.
pub(crate) fn rehearse_project(
    conn: &Connection,
    hides: &dyn Fn(EvidenceSourceKind, &str) -> bool,
    template_id: &str,
    title: &str,
    start_at_ms: i64,
) -> Result<ProjectFromTemplate> {
    make_project(
        conn,
        None,
        hides,
        Change::now("", WorkActor::Agent),
        template_id,
        title,
        start_at_ms,
        &mut Effects::default(),
    )
}

/// The meetings kept on this Mac, or none when the conversations cannot
/// say which.
async fn kept_local(app: &AppHandle) -> Option<KeptLocal> {
    let store = app.state::<Arc<MeetingSessionManager>>().store().await.ok()?;
    KeptLocal::read(&store).ok()
}

/// Whether work drawn from `source_id` stays out of a template: a meeting
/// kept on this Mac, and every meeting while `kept` cannot say which.
fn kept_back(kept: Option<&KeptLocal>, kind: EvidenceSourceKind, source_id: &str) -> bool {
    match kept {
        Some(kept) => crate::query::external::evidence_kept_local(kind, source_id, kept),
        None => matches!(kind, EvidenceSourceKind::Meeting | EvidenceSourceKind::Loop),
    }
}

/// Every project template, without the tasks a project made from it would
/// leave out. The app's `planning_project_templates`.
pub(crate) async fn templates_read(app: &AppHandle) -> Result<Vec<ProjectTemplate>, String> {
    let kept = kept_local(app).await;
    let hides = |kind, source_id: &str| kept_back(kept.as_ref(), kind, source_id);
    let mut saved =
        crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| templates(conn))?;
    for template in &mut saved {
        template.fields.leave_out(&hides);
    }
    Ok(saved)
}

/// Saves a project as the template called `name`. The app's
/// `planning_project_template_save`.
pub(crate) async fn template_save_as(
    app: &AppHandle,
    operation_id: &str,
    name: &str,
    project_id: &str,
) -> Result<ProjectTemplate, String> {
    let kept = kept_local(app).await;
    let hides = |kind, source_id: &str| kept_back(kept.as_ref(), kind, source_id);
    let at_ms = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "planning_project_template_save",
            &(name, project_id),
            at_ms,
            |tx| {
                Ok(Done {
                    value: save_from_project(tx, name, project_id, &hides, at_ms)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

/// Deletes a template; projects made from it stay. The app's
/// `planning_project_template_delete`.
pub(crate) fn template_delete_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
) -> Result<ProjectTemplate, String> {
    let at_ms = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "planning_project_template_delete",
            &id,
            at_ms,
            |tx| {
                Ok(Done {
                    value: delete_template(tx, id)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

/// Makes a project from a template as `actor`. The app's
/// `planning_project_from_template`.
pub(crate) async fn project_from_template_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    template_id: &str,
    title: &str,
    start_at_ms: i64,
) -> Result<ProjectFromTemplate, String> {
    let kept = kept_local(app).await;
    let hides = |kind, source_id: &str| kept_back(kept.as_ref(), kind, source_id);
    let change = Change::now(operation_id, actor);
    run_with_meetings(app, |history, meetings| {
        store::mutate(
            history,
            change.operation_id,
            "planning_project_from_template",
            &(template_id, title, start_at_ms),
            change.at_ms,
            |tx| {
                let mut effects = Effects::default();
                let value = make_project(
                    tx,
                    meetings,
                    &hides,
                    change,
                    template_id,
                    title,
                    start_at_ms,
                    &mut effects,
                )?;
                Ok(Done { value, effects })
            },
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::super::super::schema;
    use super::super::items::{self, NewItem};
    use super::super::types::{WorkActivityKind, WorkActor, WorkOwner, WorkPatch, WorkStatus};
    use super::super::{Change, Effects};
    use super::{make_project, save_from_project};
    use crate::planning::types::EvidenceSourceKind;
    use crate::planning::catalog::{self, MilestoneEdit, ProjectEdit};
    use chrono::NaiveDate;
    use rusqlite::Connection;

    const AT: i64 = 1_700_000_000_000;

    fn midnight(year: i32, month: u32, day: u32) -> i64 {
        let day = NaiveDate::from_ymd_opt(year, month, day).expect("a calendar day");
        items::local_midnight_ms(day).expect("a local midnight")
    }

    fn task(conn: &Connection, title: &str, status: WorkStatus, project: &str, milestone: &str, labels: &[String]) -> String {
        items::insert(
            conn,
            &NewItem {
                title,
                details: None,
                status,
                priority: 2,
                project_id: Some(project),
                milestone_id: Some(milestone),
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: labels,
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Me,
            AT,
        )
        .expect("a task")
        .id
    }

    #[test]
    fn a_project_from_a_template_dates_its_milestones_from_the_new_start() {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        let venue = items::save_label(&conn, None, "Venue", 30.0, AT).expect("a label");
        let source = catalog::save_project(
            &conn,
            None,
            &ProjectEdit {
                title: Some("Launch kit source".to_owned()),
                start_at_ms: Some(Some(midnight(2027, 1, 4))),
                ..ProjectEdit::default()
            },
            AT,
        )
        .expect("the source project");
        let design = catalog::save_milestone(
            &conn,
            &source.id,
            None,
            &MilestoneEdit {
                title: Some("Design".to_owned()),
                description: None,
                due_at_ms: Some(Some(midnight(2027, 1, 11))),
                status: None,
            },
            AT,
        )
        .expect("a milestone");
        let labels = [venue.id.clone()];
        let booked = task(&conn, "Book the venue", WorkStatus::NotStarted, &source.id, &design.id, &labels);
        items::apply(
            &conn,
            &booked,
            &WorkPatch {
                estimate: Some(Some(3)),
                ..WorkPatch::default()
            },
            WorkActivityKind::Estimate,
            WorkActor::Me,
            AT,
        )
        .expect("an estimate");
        task(&conn, "An agent's suggestion", WorkStatus::Triage, &source.id, &design.id, &[]);

        let template = save_from_project(&conn, "Launch kit", &source.id, &|_, _| false, AT).expect("a template");
        let made = make_project(
            &conn,
            None,
            &|_, _| false,
            Change {
                operation_id: "op",
                actor: WorkActor::Me,
                at_ms: AT,
            },
            &template.id,
            "Spring launch",
            midnight(2027, 3, 1),
            &mut Effects::default(),
        )
        .expect("a project from the template");

        assert_eq!(made.project.start_at_ms, Some(midnight(2027, 3, 1)));
        assert_eq!(made.milestones.len(), 1);
        assert_eq!(made.milestones[0].due_at_ms, Some(midnight(2027, 3, 8)));
        assert_eq!(made.tasks.len(), 1);
        let made_task = &made.tasks[0];
        assert_eq!(made_task.title, "Book the venue");
        assert_eq!(made_task.status, WorkStatus::NotStarted);
        assert_eq!(made_task.label_ids, labels);
        assert_eq!(made_task.estimate, Some(3));
        assert_eq!(made_task.priority, 2);
        assert_eq!(made_task.milestone_id.as_deref(), Some(made.milestones[0].id.as_str()));
    }

    /// While the conversations cannot say which meetings are kept on this
    /// Mac, a template holds no work from any meeting, and a template saved
    /// before makes none.
    #[test]
    fn a_template_leaves_out_work_from_a_meeting_kept_on_this_mac() {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        let source = catalog::save_project(
            &conn,
            None,
            &ProjectEdit {
                title: Some("Board prep".to_owned()),
                ..ProjectEdit::default()
            },
            AT,
        )
        .expect("the source project");
        let add = |title: &str, source_loop_id: Option<&str>| {
            items::insert(
                &conn,
                &NewItem {
                    title,
                    details: None,
                    status: WorkStatus::NotStarted,
                    priority: 0,
                    project_id: Some(&source.id),
                    milestone_id: None,
                    owner: &WorkOwner::Nobody,
                    due_at_ms: None,
                    label_ids: &[],
                    source_loop_id,
                    recurrence: None,
                },
                WorkActor::Me,
                AT,
            )
            .expect("a task");
        };
        add("Book the venue", None);
        add(
            "Send the acquisition deck",
            Some("a6f47cca-0000-4000-8000-000000000001:loop:0123456789abcdef"),
        );

        let closed = |kind: EvidenceSourceKind, _: &str| {
            matches!(kind, EvidenceSourceKind::Meeting | EvidenceSourceKind::Loop)
        };
        let template = save_from_project(&conn, "Board prep", &source.id, &closed, AT).expect("a template");
        let titles: Vec<&str> = template.fields.tasks.iter().map(|task| task.title.as_str()).collect();
        assert_eq!(titles, ["Book the venue"]);

        let earlier = save_from_project(&conn, "Earlier", &source.id, &|_, _| false, AT).expect("a template");
        let made = make_project(
            &conn,
            None,
            &closed,
            Change {
                operation_id: "op",
                actor: WorkActor::Me,
                at_ms: AT,
            },
            &earlier.id,
            "Board prep again",
            midnight(2027, 3, 1),
            &mut Effects::default(),
        )
        .expect("a project from the template");
        let titles: Vec<&str> = made.tasks.iter().map(|task| task.title.as_str()).collect();
        assert_eq!(titles, ["Book the venue"]);
    }

    /// A sub-task lands under its parent even when it comes first and a task
    /// before both is left out.
    #[test]
    fn a_project_from_a_template_keeps_each_sub_task_under_its_parent() {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        let source = catalog::save_project(
            &conn,
            None,
            &ProjectEdit {
                title: Some("Offsite source".to_owned()),
                ..ProjectEdit::default()
            },
            AT,
        )
        .expect("the source project");
        let travel = catalog::save_milestone(
            &conn,
            &source.id,
            None,
            &MilestoneEdit {
                title: Some("Travel".to_owned()),
                description: None,
                due_at_ms: None,
                status: None,
            },
            AT,
        )
        .expect("a milestone");
        items::insert(
            &conn,
            &NewItem {
                title: "Send the acquisition deck",
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: Some(&source.id),
                milestone_id: None,
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: Some("a6f47cca-0000-4000-8000-000000000001:loop:0123456789abcdef"),
                recurrence: None,
            },
            WorkActor::Me,
            AT,
        )
        .expect("a task from a meeting");
        let flights = task(&conn, "Book the flights", WorkStatus::NotStarted, &source.id, &travel.id, &[]);
        let plan = task(&conn, "Plan the trip", WorkStatus::NotStarted, &source.id, &travel.id, &[]);
        items::apply(
            &conn,
            &flights,
            &WorkPatch {
                parent_id: Some(Some(plan)),
                ..WorkPatch::default()
            },
            WorkActivityKind::Status,
            WorkActor::Me,
            AT,
        )
        .expect("a sub-task");

        let template = save_from_project(&conn, "Offsite", &source.id, &|_, _| false, AT).expect("a template");
        let closed = |kind: EvidenceSourceKind, _: &str| {
            matches!(kind, EvidenceSourceKind::Meeting | EvidenceSourceKind::Loop)
        };
        let made = make_project(
            &conn,
            None,
            &closed,
            Change {
                operation_id: "op",
                actor: WorkActor::Me,
                at_ms: AT,
            },
            &template.id,
            "Autumn offsite",
            midnight(2027, 9, 6),
            &mut Effects::default(),
        )
        .expect("a project from the template");

        let made_plan = made
            .tasks
            .iter()
            .find(|task| task.title == "Plan the trip")
            .expect("the parent made");
        assert_eq!(made_plan.parent_id, None);
        let under: Vec<String> = items::sub_tasks(&conn, &made_plan.id)
            .expect("its sub-tasks")
            .into_iter()
            .map(|task| task.title)
            .collect();
        assert_eq!(under, ["Book the flights"]);
    }
}
