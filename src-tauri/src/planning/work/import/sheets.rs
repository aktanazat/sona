//! Tasks from CSV files: Sona's own column template, any sheet with its
//! columns named or mapped, and the CSV exports of Jira, Asana and
//! Shortcut. Epics come in as projects, tags as labels, and each task keeps
//! a link back to the original where the export has one.

use super::super::types::WorkStatus;
use super::batch::{
    due, estimate, listed, moment, number_in, present, priority_named, status_named, Batch,
    SourceComment, SourceProject, SourceTask,
};
use super::table::{Row, Table};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use std::collections::BTreeMap;

/// One field a CSV column can fill.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CsvField {
    Id,
    Title,
    Details,
    Status,
    Priority,
    Owner,
    Labels,
    Estimate,
    Due,
    Created,
    Completed,
    Project,
    Parent,
    Url,
}

impl CsvField {
    pub(crate) const ALL: [Self; 14] = [
        Self::Id,
        Self::Title,
        Self::Details,
        Self::Status,
        Self::Priority,
        Self::Owner,
        Self::Labels,
        Self::Estimate,
        Self::Due,
        Self::Created,
        Self::Completed,
        Self::Project,
        Self::Parent,
        Self::Url,
    ];

    /// The column the template names it by.
    pub(crate) const fn header(self) -> &'static str {
        match self {
            Self::Id => "ID",
            Self::Title => "Title",
            Self::Details => "Details",
            Self::Status => "Status",
            Self::Priority => "Priority",
            Self::Owner => "Owner",
            Self::Labels => "Labels",
            Self::Estimate => "Estimate",
            Self::Due => "Due",
            Self::Created => "Created",
            Self::Completed => "Completed",
            Self::Project => "Project",
            Self::Parent => "Parent",
            Self::Url => "Link",
        }
    }

    /// Headers other sheets give it, tried when no map names a column.
    const fn known(self) -> &'static [&'static str] {
        match self {
            Self::Id => &["id", "key", "issue key", "task id", "number"],
            Self::Title => &["title", "name", "summary", "task", "task name"],
            Self::Details => &["details", "description", "notes", "body"],
            Self::Status => &[
                "status",
                "state",
                "column",
                "list",
                "section/column",
                "section",
            ],
            Self::Priority => &["priority"],
            Self::Owner => &["owner", "assignee", "assigned to", "owners"],
            Self::Labels => &["labels", "tags", "label", "tag"],
            Self::Estimate => &["estimate", "points", "story points"],
            Self::Due => &["due", "due date", "deadline", "due on"],
            Self::Created => &["created", "created at", "created on", "date created"],
            Self::Completed => &[
                "completed",
                "completed at",
                "completed on",
                "closed at",
                "resolved",
            ],
            Self::Project => &["project", "projects", "epic"],
            Self::Parent => &["parent", "parent task", "parent id", "parent issue"],
            Self::Url => &["link", "url", "app url"],
        }
    }
}

/// Which column fills which field. Fields left out are found by their
/// usual headers.
pub type CsvMap = BTreeMap<CsvField, String>;

/// Sona's import template: its header and one example row.
pub(crate) fn template() -> String {
    let header: Vec<&str> = CsvField::ALL.iter().map(|field| field.header()).collect();
    format!(
        "{}\nT-1,Book the venue,Three quotes before Friday,Todo,High,,\"Offsite, Venue\",2,2027-03-12,2027-02-01,,Spring offsite,,https://example.com/tasks/1\n",
        header.join(",")
    )
}

fn columns(table: &Table, map: &CsvMap) -> Result<BTreeMap<CsvField, usize>, String> {
    let mut found = BTreeMap::new();
    for field in CsvField::ALL {
        let column = match map.get(&field) {
            Some(header) => Some(
                table
                    .column(header)
                    .ok_or_else(|| format!("The CSV file has no column called {header:?}."))?,
            ),
            None => table.first_column(field.known()),
        };
        if let Some(column) = column {
            found.insert(field, column);
        }
    }
    if !found.contains_key(&CsvField::Title) {
        return Err(format!(
            "The CSV file needs a column with each task's title. Its columns: {}. Name one with the column map.",
            table.headers().join(", ")
        ));
    }
    Ok(found)
}

/// A row's own id, or one made from what it says, so the same row read
/// again is known again.
fn row_id(row: Row<'_>, columns: &BTreeMap<CsvField, usize>) -> String {
    if let Some(id) = columns.get(&CsvField::Id).and_then(|c| row.at(*c)) {
        return id.to_owned();
    }
    let mut hash = Sha256::new();
    for field in [
        CsvField::Title,
        CsvField::Project,
        CsvField::Created,
        CsvField::Details,
    ] {
        hash.update(
            columns
                .get(&field)
                .and_then(|c| row.at(*c))
                .unwrap_or_default(),
        );
        hash.update([0]);
    }
    format!("row {}", hex::encode(&hash.finalize()[..12]))
}

fn project_entry(batch: &mut Batch, name: &str) -> String {
    let id = name.trim().to_lowercase();
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

fn label_entries(batch: &mut Batch, names: &[String]) -> Vec<String> {
    names
        .iter()
        .map(|name| {
            let id = name.to_lowercase();
            if !batch.labels.iter().any(|label| label.id == id) {
                batch.labels.push(super::batch::SourceLabel {
                    id: id.clone(),
                    name: name.clone(),
                    group: None,
                    hue: None,
                });
            }
            id
        })
        .collect()
}

/// Reads a sheet of tasks with Sona's columns, its usual synonyms, or the
/// columns `map` names.
pub(crate) fn read_csv(text: &str, map: &CsvMap) -> Result<Batch, String> {
    let table = Table::read(text)?;
    let columns = columns(&table, map)?;
    let mut batch = Batch::default();
    for row in table.rows() {
        let cell = |field| columns.get(&field).and_then(|column| row.at(*column));
        let Some(title) = cell(CsvField::Title) else {
            continue;
        };
        let mut task = SourceTask::new(row_id(row, &columns), title.to_owned());
        task.reference = cell(CsvField::Id).map(str::to_owned);
        task.number = cell(CsvField::Id).and_then(number_in);
        task.details = cell(CsvField::Details).and_then(present);
        task.completed_at_ms = cell(CsvField::Completed).and_then(moment);
        task.status = match cell(CsvField::Status).and_then(status_named) {
            Some(status) => status,
            None if task.completed_at_ms.is_some() => WorkStatus::Done,
            None => WorkStatus::NotStarted,
        };
        task.priority = cell(CsvField::Priority).map_or(0, priority_named);
        task.assignee = cell(CsvField::Owner).and_then(present);
        let labels = cell(CsvField::Labels).map(listed).unwrap_or_default();
        task.labels = label_entries(&mut batch, &labels);
        task.estimate = cell(CsvField::Estimate).and_then(estimate);
        task.due_at_ms = cell(CsvField::Due).and_then(due);
        task.created_at_ms = cell(CsvField::Created).and_then(moment);
        task.project = cell(CsvField::Project)
            .and_then(|names| listed(names).into_iter().next())
            .map(|name| project_entry(&mut batch, &name));
        task.parent = cell(CsvField::Parent).and_then(present);
        task.url = cell(CsvField::Url).and_then(present);
        batch.tasks.push(task);
    }
    resolve_parents_by_title(&mut batch);
    Ok(batch)
}

/// A parent a sheet names by title rather than by id points at the task
/// with that title.
fn resolve_parents_by_title(batch: &mut Batch) {
    let titles: Vec<(String, String)> = batch
        .tasks
        .iter()
        .map(|task| (task.title.to_lowercase(), task.id.clone()))
        .collect();
    for task in &mut batch.tasks {
        let Some(parent) = &task.parent else { continue };
        if titles.iter().any(|(_, id)| id == parent) {
            continue;
        }
        let wanted = parent.to_lowercase();
        task.parent = titles
            .iter()
            .find(|(title, _)| *title == wanted)
            .map(|(_, id)| id.clone());
    }
}

/// Every value a row holds under a column a tracker repeats, as Jira does
/// for labels, sprints and comments.
fn repeated<'a>(table: &Table, row: Row<'a>, name: &str) -> Vec<&'a str> {
    table
        .columns_named(name)
        .into_iter()
        .filter_map(|column| row.at(column))
        .collect()
}

/// A Jira comment cell: `date;author;text`.
fn jira_comment(cell: &str, place: usize, key: &str) -> Option<SourceComment> {
    let mut parts = cell.splitn(3, ';');
    let (date, author, body) = (parts.next()?, parts.next()?, parts.next()?);
    Some(SourceComment {
        id: format!("{key} comment {place}"),
        author: present(author),
        body: present(body)?,
        created_at_ms: moment(date).or_else(|| jira_moment(date)),
        parent: None,
    })
}

/// Jira's own moment text, `05/Mar/24 2:22 PM`.
fn jira_moment(value: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(value.trim(), "%d/%b/%y %I:%M %p")
        .ok()
        .map(|at| at.and_utc().timestamp_millis())
}

/// Reads Jira's "Export Excel CSV (all fields)": epics become projects,
/// repeated Labels columns labels, and repeated Comment columns comments.
pub(crate) fn read_jira(text: &str) -> Result<Batch, String> {
    let table = Table::read(text)?;
    if !table.has(&["Summary", "Issue key"]) {
        return Err(
            "This is not a Jira CSV export: it has no Summary and Issue key columns.".to_owned(),
        );
    }
    let when = |value: &str| moment(value).or_else(|| jira_moment(value));
    let mut batch = Batch::default();
    let mut epics_by_id = BTreeMap::new();
    for row in table.rows() {
        if row
            .get(&["Issue Type"])
            .is_some_and(|kind| kind.eq_ignore_ascii_case("epic"))
        {
            let (Some(key), Some(summary)) = (row.get(&["Issue key"]), row.get(&["Summary"]))
            else {
                continue;
            };
            let project = project_entry(&mut batch, summary);
            if let Some(found) = batch.projects.iter_mut().find(|p| p.id == project) {
                found.description = row.text(&["Description"]);
            }
            epics_by_id.insert(key.to_owned(), project.clone());
            if let Some(id) = row.get(&["Issue id"]) {
                epics_by_id.insert(id.to_owned(), project);
            }
        }
    }
    for row in table.rows() {
        let (Some(key), Some(summary)) = (row.get(&["Issue key"]), row.get(&["Summary"])) else {
            continue;
        };
        if epics_by_id.contains_key(key) {
            continue;
        }
        let mut task = SourceTask::new(key.to_owned(), summary.to_owned());
        task.reference = Some(key.to_owned());
        task.number = number_in(key);
        task.details = row.text(&["Description"]);
        task.completed_at_ms = row.get(&["Resolved"]).and_then(when);
        task.status = row
            .get(&["Status"])
            .and_then(status_named)
            .or_else(|| row.get(&["Status Category"]).and_then(status_named))
            .unwrap_or(if task.completed_at_ms.is_some() {
                WorkStatus::Done
            } else {
                WorkStatus::NotStarted
            });
        task.priority = row.get(&["Priority"]).map_or(0, priority_named);
        task.assignee = row.text(&["Assignee"]);
        task.created_at_ms = row.get(&["Created"]).and_then(when);
        task.updated_at_ms = row.get(&["Updated"]).and_then(when);
        task.due_at_ms = row.get(&["Due date", "Due Date"]).and_then(due);
        task.estimate = row
            .get(&[
                "Custom field (Story Points)",
                "Custom field (Story point estimate)",
                "Story Points",
            ])
            .and_then(estimate);
        let labels: Vec<String> = repeated(&table, row, "Labels")
            .into_iter()
            .flat_map(listed)
            .collect();
        task.labels = label_entries(&mut batch, &labels);
        let parent = row
            .get(&[
                "Parent",
                "Parent id",
                "Custom field (Epic Link)",
                "Parent key",
            ])
            .map(str::to_owned);
        match parent.as_deref().and_then(|parent| epics_by_id.get(parent)) {
            Some(project) => task.project = Some(project.clone()),
            None => task.parent = parent,
        }
        task.comments = repeated(&table, row, "Comment")
            .into_iter()
            .enumerate()
            .filter_map(|(place, cell)| jira_comment(cell, place, key))
            .collect();
        batch.tasks.push(task);
    }
    let ids: BTreeMap<String, String> = table
        .rows()
        .filter_map(|row| {
            Some((
                row.get(&["Issue id"])?.to_owned(),
                row.get(&["Issue key"])?.to_owned(),
            ))
        })
        .collect();
    for task in &mut batch.tasks {
        if let Some(key) = task.parent.as_ref().and_then(|parent| ids.get(parent)) {
            task.parent = Some(key.clone());
        }
    }
    Ok(batch)
}

/// Reads Asana's project CSV export: sections as status words, tags as
/// labels, the project column as the project, and a link to each task.
pub(crate) fn read_asana(text: &str) -> Result<Batch, String> {
    let table = Table::read(text)?;
    if !table.has(&["Task ID", "Name"]) {
        return Err(
            "This is not an Asana CSV export: it has no Task ID and Name columns.".to_owned(),
        );
    }
    let mut batch = Batch::default();
    for row in table.rows() {
        let (Some(id), Some(name)) = (row.get(&["Task ID"]), row.get(&["Name"])) else {
            continue;
        };
        let mut task = SourceTask::new(id.to_owned(), name.to_owned());
        task.details = row.text(&["Notes"]);
        task.completed_at_ms = row.get(&["Completed At"]).and_then(moment);
        task.status = if task.completed_at_ms.is_some() {
            WorkStatus::Done
        } else {
            row.get(&["Section/Column"])
                .and_then(status_named)
                .unwrap_or(WorkStatus::NotStarted)
        };
        task.assignee = row.text(&["Assignee"]);
        task.created_at_ms = row.get(&["Created At"]).and_then(moment);
        task.updated_at_ms = row.get(&["Last Modified"]).and_then(moment);
        task.due_at_ms = row.get(&["Due Date"]).and_then(due);
        let labels = row.get(&["Tags"]).map(listed).unwrap_or_default();
        task.labels = label_entries(&mut batch, &labels);
        task.project = row
            .get(&["Projects"])
            .and_then(|names| listed(names).into_iter().next())
            .map(|name| project_entry(&mut batch, &name));
        task.parent = row.text(&["Parent task"]);
        task.url = Some(format!("https://app.asana.com/0/0/{id}"));
        batch.tasks.push(task);
    }
    resolve_parents_by_title(&mut batch);
    Ok(batch)
}

/// Reads Shortcut's stories CSV export: epics as projects, workflow states
/// as status words, and each story's link.
pub(crate) fn read_shortcut(text: &str) -> Result<Batch, String> {
    let table = Table::read(text)?;
    if !table.has(&["id", "name", "state"]) {
        return Err(
            "This is not a Shortcut CSV export: it has no id, name and state columns.".to_owned(),
        );
    }
    let mut batch = Batch::default();
    for row in table.rows() {
        let (Some(id), Some(name)) = (row.get(&["id"]), row.get(&["name"])) else {
            continue;
        };
        let mut task = SourceTask::new(id.to_owned(), name.to_owned());
        task.reference = Some(format!("sc-{id}"));
        task.number = number_in(id);
        task.details = row.text(&["description"]);
        task.completed_at_ms = row.get(&["completed_at"]).and_then(moment);
        let completed = row
            .get(&["is_completed"])
            .is_some_and(|value| value.eq_ignore_ascii_case("true"));
        task.status = match row.get(&["state"]).and_then(status_named) {
            Some(status) => status,
            None if completed => WorkStatus::Done,
            None if row.get(&["started_at"]).is_some() => WorkStatus::InProgress,
            None => WorkStatus::NotStarted,
        };
        task.priority = row.get(&["priority"]).map_or(0, priority_named);
        task.assignee = row
            .get(&["owners"])
            .and_then(|owners| listed(owners).into_iter().next());
        task.estimate = row.get(&["estimate"]).and_then(estimate);
        task.created_at_ms = row.get(&["created_at"]).and_then(moment);
        task.updated_at_ms = row.get(&["updated_at"]).and_then(moment);
        task.due_at_ms = row.get(&["deadline", "due_date"]).and_then(due);
        task.archived_at_ms = row
            .get(&["is_archived"])
            .filter(|value| value.eq_ignore_ascii_case("true"))
            .and(task.updated_at_ms.or(task.created_at_ms));
        let labels = row.get(&["labels"]).map(listed).unwrap_or_default();
        task.labels = label_entries(&mut batch, &labels);
        task.project = row
            .get(&["epic"])
            .map(|epic| project_entry(&mut batch, epic));
        task.url = row.text(&["app_url", "story_url", "url"]);
        batch.tasks.push(task);
    }
    Ok(batch)
}
