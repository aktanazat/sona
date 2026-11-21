//! Linear's exports: the issues, projects and initiatives CSV files Linear
//! writes (linear.app/docs/exporting-data), and the JSON its GraphQL API
//! answers with, which [`super::linear_api`] saves and a script can too.

use super::super::initiatives::InitiativeStatus;
use super::super::types::WorkStatus;
use super::batch::{
    day, due, estimate, hue, listed, moment, number_in, present, priority_named, status_named,
    Batch, SourceComment, SourceCycle, SourceDocument, SourceInitiative, SourceLabel,
    SourceMilestone, SourceProject, SourceTask, SourceView, ViewFilter,
};
use super::table::{Row, Table};
use crate::planning::types::ProjectStatus;
use serde_json::{Map, Value};

/// The issues file's columns Linear documents; enough of them say it is
/// one.
const ISSUE_COLUMNS: [&str; 4] = ["ID", "Title", "Status", "Priority"];
const PROJECT_COLUMNS: [&str; 3] = ["Name", "Lead", "Milestones"];
const INITIATIVE_COLUMNS: [&str; 3] = ["Name", "Owner", "Projects"];

/// Reads one Linear CSV export: issues, projects or initiatives.
pub(crate) fn read_csv(text: &str) -> Result<Batch, String> {
    let table = Table::read(text)?;
    if table.has(&ISSUE_COLUMNS) {
        Ok(issues(&table))
    } else if table.has(&PROJECT_COLUMNS) {
        Ok(projects(&table))
    } else if table.has(&INITIATIVE_COLUMNS) {
        Ok(initiatives(&table))
    } else {
        Err("This is not a Linear export. Export issues, projects or initiatives as CSV from Linear, or save its API answer as JSON.".to_owned())
    }
}

/// A project or initiative named in a CSV file goes by its name there,
/// since only the issues file carries ids and the others are joined to it
/// by name.
fn by_name(name: &str) -> String {
    name.trim().to_lowercase()
}

fn milestone_key(project: &str, milestone: &str) -> String {
    format!("{}\u{1f}{}", by_name(project), by_name(milestone))
}

fn project_named(batch: &mut Batch, name: &str) -> String {
    let id = by_name(name);
    if !batch.projects.iter().any(|project| project.id == id) {
        batch.projects.push(SourceProject {
            id: id.clone(),
            name: name.trim().to_owned(),
            description: None,
            status: None,
            priority: 0,
            start_at_ms: None,
            target_at_ms: None,
            initiative: None,
            milestones: Vec::new(),
        });
    }
    id
}

fn initiative_named(batch: &mut Batch, name: &str) -> String {
    let id = by_name(name);
    if !batch
        .initiatives
        .iter()
        .any(|initiative| initiative.id == id)
    {
        batch.initiatives.push(SourceInitiative {
            id: id.clone(),
            name: name.trim().to_owned(),
            description: None,
            status: None,
            target_at_ms: None,
        });
    }
    id
}

fn label_named(batch: &mut Batch, name: &str) -> String {
    let id = by_name(name);
    if !batch.labels.iter().any(|label| label.id == id) {
        batch.labels.push(SourceLabel {
            id: id.clone(),
            name: name.trim().to_owned(),
            group: None,
            hue: None,
        });
    }
    id
}

/// An issue's status from its workflow state's name, read with the times
/// Linear stamps when it closes or starts one: a renamed "Done" column
/// still reads as done.
fn issue_status(row: Row<'_>) -> WorkStatus {
    if row.get(&["Canceled"]).is_some() {
        return WorkStatus::Canceled;
    }
    if row.get(&["Completed"]).is_some() {
        return WorkStatus::Done;
    }
    if let Some(status) = row.get(&["Status"]).and_then(status_named) {
        return status;
    }
    if row.get(&["Started"]).is_some() {
        WorkStatus::InProgress
    } else {
        WorkStatus::NotStarted
    }
}

fn issues(table: &Table) -> Batch {
    let mut batch = Batch::default();
    for row in table.rows() {
        let (Some(id), Some(title)) = (row.get(&["ID"]), row.get(&["Title"])) else {
            continue;
        };
        let mut task = SourceTask::new(id.to_owned(), title.to_owned());
        task.reference = Some(id.to_owned());
        task.number = number_in(id);
        task.details = row.text(&["Description"]);
        task.status = issue_status(row);
        task.priority = row.get(&["Priority"]).map_or(0, priority_named);
        task.estimate = row.get(&["Estimate"]).and_then(estimate);
        task.due_at_ms = row.get(&["Due Date"]).and_then(due);
        task.created_at_ms = row.get(&["Created"]).and_then(moment);
        task.updated_at_ms = row.get(&["Updated"]).and_then(moment);
        task.completed_at_ms = row
            .get(&["Completed"])
            .or_else(|| row.get(&["Canceled"]))
            .and_then(moment);
        task.archived_at_ms = row.get(&["Archived"]).and_then(moment);
        task.assignee = row.text(&["Assignee"]);
        task.parent = row.text(&["Parent issue"]);
        task.labels = row
            .get(&["Labels"])
            .map(listed)
            .unwrap_or_default()
            .iter()
            .map(|name| label_named(&mut batch, name))
            .collect();
        if let Some(project) = row.get(&["Project"]) {
            let project_id = project_named(&mut batch, project);
            if let Some(initiative) = row
                .get(&["Initiatives"])
                .map(listed)
                .and_then(|names| names.into_iter().next())
            {
                let initiative_id = initiative_named(&mut batch, &initiative);
                if let Some(found) = batch.projects.iter_mut().find(|p| p.id == project_id) {
                    found.initiative.get_or_insert(initiative_id);
                }
            }
            if let Some(milestone) = row.get(&["Project Milestone"]) {
                let key = milestone_key(project, milestone);
                if let Some(found) = batch.projects.iter_mut().find(|p| p.id == project_id) {
                    if !found.milestones.iter().any(|m| m.id == key) {
                        found.milestones.push(SourceMilestone {
                            id: key.clone(),
                            name: milestone.to_owned(),
                            description: None,
                            due_at_ms: None,
                        });
                    }
                }
                task.milestone = Some(key);
            }
            task.project = Some(project_id);
        }
        if let (Some(number), Some(start), Some(end)) = (
            row.get(&["Cycle Number"]),
            row.get(&["Cycle Start"]).and_then(day),
            row.get(&["Cycle End"]).and_then(day),
        ) {
            let id = format!("cycle {number}");
            if !batch.cycles.iter().any(|cycle| cycle.id == id) && start < end {
                batch.cycles.push(SourceCycle {
                    id: id.clone(),
                    start,
                    end,
                });
            }
            task.cycle = Some(id);
        }
        batch.tasks.push(task);
    }
    batch
}

fn project_status(value: &str) -> ProjectStatus {
    match value.trim().to_lowercase().as_str() {
        "backlog" => ProjectStatus::Backlog,
        "planned" => ProjectStatus::Planned,
        "paused" => ProjectStatus::Paused,
        "completed" => ProjectStatus::Completed,
        "canceled" | "cancelled" => ProjectStatus::Canceled,
        _ => ProjectStatus::Active,
    }
}

fn projects(table: &Table) -> Batch {
    let mut batch = Batch::default();
    for row in table.rows() {
        let Some(name) = row.get(&["Name"]) else {
            continue;
        };
        let id = project_named(&mut batch, name);
        let initiative = row
            .get(&["Initiatives"])
            .map(listed)
            .and_then(|names| names.into_iter().next())
            .map(|name| initiative_named(&mut batch, &name));
        let status = if row.get(&["Canceled At"]).is_some() {
            ProjectStatus::Canceled
        } else if row.get(&["Completed At"]).is_some() {
            ProjectStatus::Completed
        } else {
            row.get(&["Status"])
                .map_or(ProjectStatus::Active, project_status)
        };
        let milestones = row
            .get(&["Milestones"])
            .map(listed)
            .unwrap_or_default()
            .into_iter()
            .map(|milestone| SourceMilestone {
                id: milestone_key(name, &milestone),
                name: milestone,
                description: None,
                due_at_ms: None,
            })
            .collect();
        if let Some(project) = batch.projects.iter_mut().find(|p| p.id == id) {
            project.description = row.text(&["Summary"]);
            project.status = Some(status);
            project.start_at_ms = row.get(&["Started At"]).and_then(due);
            project.target_at_ms = row.get(&["Target Date"]).and_then(due);
            project.initiative = initiative;
            project.milestones = milestones;
        }
    }
    batch
}

fn initiative_status(value: &str) -> InitiativeStatus {
    match value.trim().to_lowercase().as_str() {
        "completed" | "canceled" | "cancelled" => InitiativeStatus::Completed,
        "planned" | "proposed" => InitiativeStatus::Planned,
        _ => InitiativeStatus::Active,
    }
}

fn initiatives(table: &Table) -> Batch {
    let mut batch = Batch::default();
    for row in table.rows() {
        let Some(name) = row.get(&["Name"]) else {
            continue;
        };
        let id = initiative_named(&mut batch, name);
        for project in row.get(&["Projects"]).map(listed).unwrap_or_default() {
            let project_id = project_named(&mut batch, &project);
            if let Some(found) = batch.projects.iter_mut().find(|p| p.id == project_id) {
                found.initiative = Some(id.clone());
            }
        }
        if let Some(initiative) = batch.initiatives.iter_mut().find(|i| i.id == id) {
            initiative.description = row.text(&["Description"]);
            initiative.status = Some(if row.get(&["Completed At"]).is_some() {
                InitiativeStatus::Completed
            } else {
                row.get(&["Status"])
                    .map_or(InitiativeStatus::Active, initiative_status)
            });
            initiative.target_at_ms = row.get(&["Target Date"]).and_then(due);
        }
    }
    batch
}

/// A connection's nodes: `{"nodes": [...]}` as the API answers, or a plain
/// list.
fn nodes(value: Option<&Value>) -> &[Value] {
    match value {
        Some(Value::Array(items)) => items,
        Some(Value::Object(object)) => match object.get("nodes") {
            Some(Value::Array(items)) => items,
            _ => &[],
        },
        _ => &[],
    }
}

fn text(object: &Value, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).and_then(present)
}

fn id_of(object: &Value, key: &str) -> Option<String> {
    object.get(key).and_then(|inner| text(inner, "id"))
}

fn number(object: &Value, key: &str) -> Option<f64> {
    object.get(key).and_then(Value::as_f64)
}

/// Reads the JSON the Linear API answers with: the `data` of one or more
/// queries, with any of `viewer`, `issues`, `projects`, `cycles`,
/// `issueLabels`, `initiatives`, `documents` and `customViews`.
pub(crate) fn read_json(text: &str) -> Result<Batch, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|_| "The file is not JSON Sona can read.".to_owned())?;
    let root = value.get("data").unwrap_or(&value);
    let Some(root) = root.as_object() else {
        return Err("This JSON is not a Linear API answer.".to_owned());
    };
    let known = [
        "viewer",
        "issues",
        "projects",
        "cycles",
        "issueLabels",
        "initiatives",
        "documents",
        "customViews",
    ];
    if !known.iter().any(|key| root.contains_key(*key)) {
        return Err("This JSON is not a Linear API answer: it has none of issues, projects, cycles, labels, initiatives, documents or views.".to_owned());
    }
    Ok(api(root))
}

pub(crate) fn api(root: &Map<String, Value>) -> Batch {
    let mut batch = Batch::default();
    if let Some(viewer) = root.get("viewer") {
        for key in ["name", "displayName", "email"] {
            if let Some(name) = text(viewer, key) {
                if !batch.me.contains(&name) {
                    batch.me.push(name);
                }
            }
        }
    }
    for label in nodes(root.get("issueLabels")) {
        let (Some(id), Some(name)) = (text(label, "id"), text(label, "name")) else {
            continue;
        };
        if label.get("isGroup").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        batch.labels.push(SourceLabel {
            id,
            name,
            group: label.get("parent").and_then(|parent| text(parent, "name")),
            hue: text(label, "color").as_deref().and_then(hue),
        });
    }
    for initiative in nodes(root.get("initiatives")) {
        let (Some(id), Some(name)) = (text(initiative, "id"), text(initiative, "name")) else {
            continue;
        };
        batch.initiatives.push(SourceInitiative {
            id,
            name,
            description: text(initiative, "description").or_else(|| text(initiative, "content")),
            status: text(initiative, "status").map(|s| initiative_status(&s)),
            target_at_ms: text(initiative, "targetDate").as_deref().and_then(due),
        });
    }
    for project in nodes(root.get("projects")) {
        let (Some(id), Some(name)) = (text(project, "id"), text(project, "name")) else {
            continue;
        };
        let status = project
            .get("status")
            .and_then(|status| text(status, "type"))
            .or_else(|| text(project, "state"))
            .map_or(ProjectStatus::Active, |s| match s.as_str() {
                "started" => ProjectStatus::Active,
                other => project_status(other),
            });
        let initiative = nodes(project.get("initiatives"))
            .iter()
            .find_map(|initiative| text(initiative, "id"));
        batch.projects.push(SourceProject {
            id,
            name,
            description: text(project, "content").or_else(|| text(project, "description")),
            status: Some(status),
            priority: number(project, "priority").map_or(0, |p| priority_named(&format!("{p:.0}"))),
            start_at_ms: text(project, "startDate").as_deref().and_then(due),
            target_at_ms: text(project, "targetDate").as_deref().and_then(due),
            initiative,
            milestones: nodes(project.get("projectMilestones"))
                .iter()
                .filter_map(|milestone| {
                    Some(SourceMilestone {
                        id: text(milestone, "id")?,
                        name: text(milestone, "name")?,
                        description: text(milestone, "description"),
                        due_at_ms: text(milestone, "targetDate").as_deref().and_then(due),
                    })
                })
                .collect(),
        });
    }
    for initiative in nodes(root.get("initiatives")) {
        let Some(initiative_id) = text(initiative, "id") else {
            continue;
        };
        for project in nodes(initiative.get("projects")) {
            if let Some(found) =
                text(project, "id").and_then(|id| batch.projects.iter_mut().find(|p| p.id == id))
            {
                found
                    .initiative
                    .get_or_insert_with(|| initiative_id.clone());
            }
        }
    }
    for cycle in nodes(root.get("cycles")) {
        let (Some(id), Some(start), Some(end)) = (
            text(cycle, "id"),
            text(cycle, "startsAt").as_deref().and_then(day),
            text(cycle, "endsAt").as_deref().and_then(day),
        ) else {
            continue;
        };
        if start < end {
            batch.cycles.push(SourceCycle { id, start, end });
        }
    }
    for issue in nodes(root.get("issues")) {
        if let Some(task) = issue_from_api(issue, &mut batch) {
            batch.tasks.push(task);
        }
    }
    for document in nodes(root.get("documents")) {
        let (Some(id), Some(title)) = (text(document, "id"), text(document, "title")) else {
            continue;
        };
        batch.documents.push(SourceDocument {
            id,
            title,
            body: document
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            project: id_of(document, "project"),
            created_at_ms: text(document, "createdAt").as_deref().and_then(moment),
        });
    }
    for view in nodes(root.get("customViews")) {
        let (Some(id), Some(name)) = (text(view, "id"), text(view, "name")) else {
            continue;
        };
        let filter = match text(view, "modelName").as_deref() {
            Some("Issue") | None => view
                .get("filterData")
                .map_or_else(|| Ok(ViewFilter::default()), view_filter),
            Some(_) => Err("it lists projects, not issues".to_owned()),
        };
        batch.views.push(SourceView { id, name, filter });
    }
    batch
}

fn issue_status_from_api(issue: &Value) -> WorkStatus {
    let state = issue.get("state");
    let kind = state.and_then(|state| text(state, "type"));
    let name = state.and_then(|state| text(state, "name"));
    match kind.as_deref() {
        Some("triage") => WorkStatus::Triage,
        Some("backlog") => WorkStatus::Backlog,
        Some("unstarted") => WorkStatus::NotStarted,
        Some("started") => WorkStatus::InProgress,
        Some("completed") => WorkStatus::Done,
        Some("canceled") => WorkStatus::Canceled,
        _ => name
            .as_deref()
            .and_then(status_named)
            .unwrap_or(WorkStatus::NotStarted),
    }
}

fn issue_from_api(issue: &Value, batch: &mut Batch) -> Option<SourceTask> {
    let id = text(issue, "id")?;
    let mut task = SourceTask::new(id, text(issue, "title")?);
    task.reference = text(issue, "identifier");
    task.number = number(issue, "number")
        .filter(|n| n.fract() == 0.0 && *n >= 1.0)
        .and_then(|n| format!("{n:.0}").parse().ok())
        .or_else(|| text(issue, "identifier").as_deref().and_then(number_in));
    task.details = text(issue, "description");
    task.status = issue_status_from_api(issue);
    task.priority = number(issue, "priority").map_or(0, |p| priority_named(&format!("{p:.0}")));
    task.estimate = number(issue, "estimate").and_then(|points| estimate(&points.to_string()));
    task.due_at_ms = text(issue, "dueDate").as_deref().and_then(due);
    task.created_at_ms = text(issue, "createdAt").as_deref().and_then(moment);
    task.updated_at_ms = text(issue, "updatedAt").as_deref().and_then(moment);
    task.completed_at_ms = text(issue, "completedAt")
        .or_else(|| text(issue, "canceledAt"))
        .as_deref()
        .and_then(moment);
    task.archived_at_ms = text(issue, "archivedAt").as_deref().and_then(moment);
    task.assignee = issue
        .get("assignee")
        .and_then(|person| text(person, "name").or_else(|| text(person, "displayName")));
    task.url = text(issue, "url");
    task.project = id_of(issue, "project");
    task.milestone = id_of(issue, "projectMilestone");
    task.cycle = id_of(issue, "cycle");
    task.parent = id_of(issue, "parent");
    for label in nodes(issue.get("labels")) {
        let Some(label_id) = text(label, "id") else {
            continue;
        };
        if !batch.labels.iter().any(|known| known.id == label_id) {
            let Some(name) = text(label, "name") else {
                continue;
            };
            batch.labels.push(SourceLabel {
                id: label_id.clone(),
                name,
                group: label.get("parent").and_then(|parent| text(parent, "name")),
                hue: text(label, "color").as_deref().and_then(hue),
            });
        }
        task.labels.push(label_id);
    }
    task.comments = nodes(issue.get("comments"))
        .iter()
        .filter_map(|comment| {
            Some(SourceComment {
                id: text(comment, "id")?,
                author: comment
                    .get("user")
                    .and_then(|user| text(user, "name").or_else(|| text(user, "displayName")))
                    .or_else(|| comment.get("botActor").and_then(|bot| text(bot, "name")))
                    .or_else(|| {
                        comment
                            .get("externalUser")
                            .and_then(|user| text(user, "name"))
                    }),
                body: text(comment, "body")?,
                created_at_ms: text(comment, "createdAt").as_deref().and_then(moment),
                parent: id_of(comment, "parent"),
            })
        })
        .collect();
    task.comments.sort_by_key(|comment| comment.created_at_ms);
    Some(task)
}

fn state_type(kind: &str) -> Option<WorkStatus> {
    Some(match kind {
        "triage" => WorkStatus::Triage,
        "backlog" => WorkStatus::Backlog,
        "unstarted" => WorkStatus::NotStarted,
        "started" => WorkStatus::InProgress,
        "completed" => WorkStatus::Done,
        "canceled" => WorkStatus::Canceled,
        _ => return None,
    })
}

/// The values a comparator holds: `{"eq": x}` or `{"in": [x, y]}`.
fn compared(comparator: &Value) -> Result<Vec<Value>, String> {
    let Some(object) = comparator.as_object() else {
        return Err(unsupported());
    };
    let mut values = Vec::new();
    for (operator, value) in object {
        match (operator.as_str(), value) {
            ("eq", value) => values.push(value.clone()),
            ("in", Value::Array(items)) => values.extend(items.iter().cloned()),
            _ => return Err(unsupported()),
        }
    }
    Ok(values)
}

fn unsupported() -> String {
    "its filters use conditions Sona's saved views do not have".to_owned()
}

fn strings(values: Vec<Value>) -> Result<Vec<String>, String> {
    values
        .into_iter()
        .map(|value| value.as_str().map(str::to_owned).ok_or_else(unsupported))
        .collect()
}

/// A Linear issue filter in Sona's terms: workflow state, project, labels
/// and priority, joined by `and`. Anything else, `or` included, is a view
/// Sona cannot keep as it was.
fn view_filter(filter: &Value) -> Result<ViewFilter, String> {
    let mut kept = ViewFilter::default();
    add_filter(filter, &mut kept)?;
    Ok(kept)
}

fn add_filter(filter: &Value, kept: &mut ViewFilter) -> Result<(), String> {
    let Some(object) = filter.as_object() else {
        return Err(unsupported());
    };
    for (field, condition) in object {
        match field.as_str() {
            "and" => {
                for inner in condition.as_array().ok_or_else(unsupported)? {
                    add_filter(inner, kept)?;
                }
            }
            "state" => {
                let object = condition.as_object().ok_or_else(unsupported)?;
                for (key, comparator) in object {
                    let values = strings(compared(comparator)?)?;
                    let statuses = match key.as_str() {
                        "type" => values
                            .iter()
                            .map(|v| state_type(v))
                            .collect::<Option<Vec<_>>>(),
                        "name" => values.iter().map(|v| status_named(v)).collect(),
                        _ => None,
                    };
                    kept.statuses.extend(statuses.ok_or_else(unsupported)?);
                }
            }
            "project" => {
                let id = condition.get("id").ok_or_else(unsupported)?;
                kept.projects.extend(strings(compared(id)?)?);
            }
            "labels" => {
                let inner = condition.get("some").unwrap_or(condition);
                if let Some(id) = inner.get("id") {
                    kept.labels.extend(strings(compared(id)?)?);
                } else if let Some(name) = inner.get("name") {
                    kept.labels
                        .extend(strings(compared(name)?)?.iter().map(|name| by_name(name)));
                } else {
                    return Err(unsupported());
                }
            }
            "priority" => {
                for value in compared(condition)? {
                    let priority = value.as_f64().ok_or_else(unsupported)?;
                    kept.priorities
                        .push(priority_named(&format!("{priority:.0}")));
                }
            }
            _ => return Err(unsupported()),
        }
    }
    Ok(())
}
