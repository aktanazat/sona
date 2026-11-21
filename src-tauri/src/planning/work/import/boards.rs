//! Tasks from JSON exports: GitHub issues, as `gh issue list --json` or the
//! REST API writes them, and a Trello board's JSON export.

use super::super::types::WorkStatus;
use super::batch::{
    due, hue, moment, present, status_named, Batch, SourceComment, SourceLabel, SourceProject,
    SourceTask,
};
use crate::planning::types::ProjectStatus;
use serde_json::Value;

fn text(object: &Value, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).and_then(present)
}

fn list<'a>(object: &'a Value, key: &str) -> &'a [Value] {
    match object.get(key) {
        Some(Value::Array(items)) => items,
        Some(Value::Object(inner)) => match inner.get("nodes") {
            Some(Value::Array(items)) => items,
            _ => &[],
        },
        _ => &[],
    }
}

/// The repository a GitHub issue link names, `owner/repo`.
fn repository(url: &str) -> Option<String> {
    let path = url.split("github.com/").nth(1)?;
    let mut parts = path.split('/');
    Some(format!("{}/{}", parts.next()?, parts.next()?))
}

/// Reads GitHub issues: a list of them, from one repository or several.
/// Pull requests are left out; milestones come in as projects and labels
/// keep their colors.
pub(crate) fn read_github(json: &str) -> Result<Batch, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|_| "The file is not JSON Sona can read.".to_owned())?;
    let Some(issues) = value.as_array() else {
        return Err("This is not a GitHub issues export: save `gh issue list --json number,title,body,state,labels,url,createdAt,closedAt,milestone,assignees,comments` to a file.".to_owned());
    };
    let mut batch = Batch::default();
    for issue in issues {
        if issue.get("pull_request").is_some() {
            continue;
        }
        let (Some(url), Some(title)) = (
            text(issue, "url")
                .filter(|url| url.contains("github.com/") && !url.contains("api.github.com"))
                .or_else(|| text(issue, "html_url")),
            text(issue, "title"),
        ) else {
            continue;
        };
        let mut task = SourceTask::new(url.clone(), title);
        task.number = issue
            .get("number")
            .and_then(Value::as_i64)
            .filter(|n| *n > 0);
        task.reference = task
            .number
            .map(|number| format!("{}#{number}", repository(&url).unwrap_or_default()));
        task.details = text(issue, "body");
        let closed = text(issue, "state").is_some_and(|state| state.eq_ignore_ascii_case("closed"));
        let not_planned = text(issue, "stateReason")
            .or_else(|| text(issue, "state_reason"))
            .is_some_and(|reason| reason.eq_ignore_ascii_case("not_planned"));
        task.status = match (closed, not_planned) {
            (true, true) => WorkStatus::Canceled,
            (true, false) => WorkStatus::Done,
            (false, _) => WorkStatus::NotStarted,
        };
        task.created_at_ms = text(issue, "createdAt")
            .or_else(|| text(issue, "created_at"))
            .as_deref()
            .and_then(moment);
        task.updated_at_ms = text(issue, "updatedAt")
            .or_else(|| text(issue, "updated_at"))
            .as_deref()
            .and_then(moment);
        task.completed_at_ms = text(issue, "closedAt")
            .or_else(|| text(issue, "closed_at"))
            .as_deref()
            .and_then(moment);
        task.assignee = list(issue, "assignees")
            .first()
            .or_else(|| issue.get("assignee").filter(|a| a.is_object()))
            .and_then(|person| text(person, "login").or_else(|| text(person, "name")));
        for label in list(issue, "labels") {
            let Some(name) = text(label, "name") else {
                continue;
            };
            let id = name.to_lowercase();
            if !batch.labels.iter().any(|known| known.id == id) {
                batch.labels.push(SourceLabel {
                    id: id.clone(),
                    name,
                    group: None,
                    hue: text(label, "color")
                        .and_then(|color| hue(&format!("#{}", color.trim_start_matches('#')))),
                });
            }
            task.labels.push(id);
        }
        if let Some(milestone) = issue.get("milestone").filter(|m| m.is_object()) {
            if let Some(name) = text(milestone, "title") {
                let id = format!(
                    "{} {}",
                    repository(&url).unwrap_or_default(),
                    name.to_lowercase()
                );
                if !batch.projects.iter().any(|project| project.id == id) {
                    let open = text(milestone, "state")
                        .is_none_or(|state| state.eq_ignore_ascii_case("open"));
                    batch.projects.push(SourceProject {
                        id: id.clone(),
                        name,
                        description: text(milestone, "description"),
                        status: Some(if open {
                            ProjectStatus::Active
                        } else {
                            ProjectStatus::Completed
                        }),
                        priority: 0,
                        start_at_ms: None,
                        target_at_ms: text(milestone, "dueOn")
                            .or_else(|| text(milestone, "due_on"))
                            .as_deref()
                            .and_then(due),
                        initiative: None,
                        milestones: Vec::new(),
                    });
                }
                task.project = Some(id);
            }
        }
        task.comments = list(issue, "comments")
            .iter()
            .enumerate()
            .filter_map(|(place, comment)| {
                Some(SourceComment {
                    id: text(comment, "id").unwrap_or_else(|| format!("{url} comment {place}")),
                    author: comment
                        .get("author")
                        .or_else(|| comment.get("user"))
                        .and_then(|person| text(person, "login")),
                    body: text(comment, "body")?,
                    created_at_ms: text(comment, "createdAt")
                        .or_else(|| text(comment, "created_at"))
                        .as_deref()
                        .and_then(moment),
                    parent: None,
                })
            })
            .collect();
        task.url = Some(url);
        batch.tasks.push(task);
    }
    Ok(batch)
}

/// Reads a Trello board's JSON export: the board as a project, each list's
/// name as a status, archived cards as archived, checklists as sub-tasks,
/// labels and comments with them.
pub(crate) fn read_trello(json: &str) -> Result<Batch, String> {
    let board: Value =
        serde_json::from_str(json).map_err(|_| "The file is not JSON Sona can read.".to_owned())?;
    if board.get("cards").is_none() || board.get("lists").is_none() {
        return Err("This is not a Trello board export: it has no cards and lists.".to_owned());
    }
    let mut batch = Batch::default();
    let project = text(&board, "id")
        .zip(text(&board, "name"))
        .map(|(id, name)| {
            batch.projects.push(SourceProject {
                id: id.clone(),
                name,
                description: text(&board, "desc"),
                status: Some(
                    if board.get("closed").and_then(Value::as_bool) == Some(true) {
                        ProjectStatus::Completed
                    } else {
                        ProjectStatus::Active
                    },
                ),
                priority: 0,
                start_at_ms: None,
                target_at_ms: None,
                initiative: None,
                milestones: Vec::new(),
            });
            id
        });
    let list_names: Vec<(String, String)> = list(&board, "lists")
        .iter()
        .filter_map(|list| Some((text(list, "id")?, text(list, "name")?)))
        .collect();
    for label in list(&board, "labels") {
        let (Some(id), Some(name)) = (text(label, "id"), text(label, "name")) else {
            continue;
        };
        batch.labels.push(SourceLabel {
            id,
            name,
            group: None,
            hue: None,
        });
    }
    let comments: Vec<(String, SourceComment)> = list(&board, "actions")
        .iter()
        .filter(|action| text(action, "type").as_deref() == Some("commentCard"))
        .filter_map(|action| {
            let data = action.get("data")?;
            let card = data.get("card").and_then(|card| text(card, "id"))?;
            Some((
                card,
                SourceComment {
                    id: text(action, "id")?,
                    author: action
                        .get("memberCreator")
                        .and_then(|member| text(member, "fullName")),
                    body: text(data, "text")?,
                    created_at_ms: text(action, "date").as_deref().and_then(moment),
                    parent: None,
                },
            ))
        })
        .collect();
    for card in list(&board, "cards") {
        let (Some(id), Some(name)) = (text(card, "id"), text(card, "name")) else {
            continue;
        };
        let mut task = SourceTask::new(id.clone(), name);
        task.details = text(card, "desc");
        let list_name = text(card, "idList").and_then(|list| {
            list_names
                .iter()
                .find(|(known, _)| *known == list)
                .map(|(_, name)| name.clone())
        });
        task.status = if card.get("dueComplete").and_then(Value::as_bool) == Some(true) {
            WorkStatus::Done
        } else {
            list_name
                .as_deref()
                .and_then(status_named)
                .unwrap_or(WorkStatus::NotStarted)
        };
        task.due_at_ms = text(card, "due").as_deref().and_then(due);
        task.updated_at_ms = text(card, "dateLastActivity").as_deref().and_then(moment);
        if card.get("closed").and_then(Value::as_bool) == Some(true) {
            task.archived_at_ms = task.updated_at_ms;
        }
        task.labels = card
            .get("idLabels")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        task.project.clone_from(&project);
        task.url = text(card, "shortUrl").or_else(|| text(card, "url"));
        task.comments = comments
            .iter()
            .filter(|(card, _)| *card == id)
            .map(|(_, comment)| comment.clone())
            .collect();
        task.comments.sort_by_key(|comment| comment.created_at_ms);
        batch.tasks.push(task);
    }
    for checklist in list(&board, "checklists") {
        let Some(card) = text(checklist, "idCard") else {
            continue;
        };
        if !batch.tasks.iter().any(|task| task.id == card) {
            continue;
        }
        for item in list(checklist, "checkItems") {
            let (Some(id), Some(name)) = (text(item, "id"), text(item, "name")) else {
                continue;
            };
            let mut task = SourceTask::new(id, name);
            task.status = if text(item, "state").as_deref() == Some("complete") {
                WorkStatus::Done
            } else {
                WorkStatus::NotStarted
            };
            task.project.clone_from(&project);
            task.parent = Some(card.clone());
            batch.tasks.push(task);
        }
    }
    Ok(batch)
}
