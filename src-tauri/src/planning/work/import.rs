//! Imports from other trackers: Linear first, then a CSV sheet, Jira,
//! GitHub, Asana, Shortcut and Trello. A reader turns the export into a
//! [`batch::Batch`]; [`write`] lands it, and the same call inside a
//! transaction that is never committed is the preview. Each import keeps a
//! record of what it made, so a second run skips what the first brought in
//! and an import can be undone: its tasks and projects go to the bin, and
//! the labels, initiatives, notes and views it made are removed.

pub(crate) mod batch;
mod boards;
mod linear;
pub(crate) mod linear_api;
pub(crate) mod sheets;
mod table;
#[cfg(test)]
mod tests;

use self::batch::{Batch, ImportSource, SourceTask, ViewFilter};
use self::sheets::CsvMap;
use super::comments::{self, Author};
use super::initiatives::{self, InitiativeSaveInput};
use super::items::{self, NewItem};
use super::types::{
    WorkActivityKind, WorkActor, WorkCommentSubject, WorkDisplay, WorkOwner, WorkPatch, WorkQuery,
    WorkStatus,
};
use super::{attachments, bin, cycles, inbox, now_ms, run_history, Change, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::planning::catalog::{self, MilestoneEdit, ProjectEdit};
use crate::planning::store::{self, Refusal, SourceLinkDraft};
use crate::planning::types::{EvidenceSourceKind, EvidenceTargetKind, ProjectStatus};
use crate::scratchpad;
use crate::secrets::{SecretAccount, SecretManager, SecretResolveError, SecretState};
use anyhow::Result;
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;
use zeroize::Zeroizing;

/// Imports and what each made or matched. A record from an import that was
/// undone no longer counts as imported, so importing again brings it back.
pub(crate) const IMPORTS_MIGRATION: &str = "CREATE TABLE planning_imports (
    id TEXT PRIMARY KEY NOT NULL,
    source TEXT NOT NULL CHECK(source IN ('linear', 'csv', 'jira', 'github', 'asana', 'shortcut', 'trello')),
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    totals TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    reverted_at_ms INTEGER
);
CREATE TABLE planning_import_items (
    import_id TEXT NOT NULL REFERENCES planning_imports(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('task', 'project', 'milestone', 'label', 'initiative',
        'comment', 'note', 'view')),
    source_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    made INTEGER NOT NULL CHECK(made IN (0, 1)),
    PRIMARY KEY(import_id, kind, source_id)
);
CREATE INDEX planning_import_items_source ON planning_import_items(kind, source_id);";

/// Open work untouched this long reads as stale.
const STALE_MS: i64 = 182 * 24 * 60 * 60 * 1000;

/// The label every imported task gets unless told otherwise.
pub(crate) const DEFAULT_LABEL: &str = "Migrated";

/// What to copy beside the tasks.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportCopy {
    pub labels: bool,
    pub projects: bool,
    pub milestones: bool,
    pub cycles: bool,
    pub initiatives: bool,
    pub comments: bool,
    pub documents: bool,
    pub views: bool,
}

impl ImportCopy {
    pub(crate) const ALL: Self = Self {
        labels: true,
        projects: true,
        milestones: true,
        cycles: true,
        initiatives: true,
        comments: true,
        documents: true,
        views: true,
    };
}

/// Which tasks to bring: open work touched in the last six months, open
/// work older than that, and finished work. Stale and finished work lands
/// in the archive.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportInclude {
    pub active: bool,
    pub stale: bool,
    pub completed: bool,
}

impl ImportInclude {
    pub(crate) const ALL: Self = Self {
        active: true,
        stale: true,
        completed: true,
    };

    const fn takes(self, state: ImportState) -> bool {
        match state {
            ImportState::Active => self.active,
            ImportState::Stale => self.stale,
            ImportState::Completed => self.completed,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    pub copy: ImportCopy,
    pub include: ImportInclude,
    /// Refresh tasks an earlier import brought in instead of skipping them.
    pub update_existing: bool,
    /// Give each task the number it had there; only while Sona has no tasks.
    pub keep_numbers: bool,
    /// The label every imported task gets; none for no label.
    pub label: Option<String>,
    /// Names the person importing goes by there: their tasks become yours.
    pub me: Vec<String>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            copy: ImportCopy::ALL,
            include: ImportInclude::ALL,
            update_existing: false,
            keep_numbers: false,
            label: Some(DEFAULT_LABEL.to_owned()),
            me: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ImportState {
    Active,
    Stale,
    Completed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ImportKind {
    Task,
    Project,
    Milestone,
    Label,
    Initiative,
    Comment,
    Note,
    View,
}

impl ImportKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Project => "project",
            Self::Milestone => "milestone",
            Self::Label => "label",
            Self::Initiative => "initiative",
            Self::Comment => "comment",
            Self::Note => "note",
            Self::View => "view",
        }
    }
}

/// What an import does with one record it read.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ImportAction {
    /// Made new.
    Made,
    /// Made new, straight into the archive.
    Archived,
    /// Already in Sona under the same name; used as it is.
    Matched,
    /// Brought in before and refreshed.
    Updated,
    /// Brought in before and left as it is.
    Skipped,
    /// Not brought in: the choices left it out, or Sona cannot hold it.
    LeftOut,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportRow {
    pub kind: ImportKind,
    pub title: String,
    /// The task's number there, as in `ENG-12`, when it had one.
    pub source_ref: Option<String>,
    pub state: Option<ImportState>,
    pub action: ImportAction,
    pub reason: Option<String>,
}

/// How many of one kind each action took.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportTally {
    pub made: u32,
    pub archived: u32,
    pub matched: u32,
    pub updated: u32,
    pub skipped: u32,
    pub left_out: u32,
}

impl ImportTally {
    fn count(&mut self, action: ImportAction) {
        let slot = match action {
            ImportAction::Made => &mut self.made,
            ImportAction::Archived => &mut self.archived,
            ImportAction::Matched => &mut self.matched,
            ImportAction::Updated => &mut self.updated,
            ImportAction::Skipped => &mut self.skipped,
            ImportAction::LeftOut => &mut self.left_out,
        };
        *slot += 1;
    }
}

/// The tasks read, by whether they are open and recent, open and stale,
/// or finished.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportStates {
    pub active: u32,
    pub stale: u32,
    pub completed: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportTotals {
    pub found: ImportStates,
    pub tasks: ImportTally,
    pub projects: ImportTally,
    pub milestones: ImportTally,
    pub labels: ImportTally,
    pub initiatives: ImportTally,
    pub comments: ImportTally,
    pub notes: ImportTally,
    pub views: ImportTally,
    /// Tasks placed in one of Sona's cycles that covers their cycle's days.
    pub in_cycles: u32,
}

/// What an import did, or with a dry run what it would do.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    /// The import's id, to undo it by; none in a preview.
    pub id: Option<String>,
    pub source: ImportSource,
    pub name: String,
    pub label: Option<String>,
    pub totals: ImportTotals,
    pub rows: Vec<ImportRow>,
    /// Things the import could not do as asked, in a sentence each.
    pub notes: Vec<String>,
    /// Whether Sona has no tasks yet, so numbers can be kept.
    pub numbers_keepable: bool,
}

/// One past import, newest first in [`imports`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportRecord {
    pub id: String,
    pub source: ImportSource,
    pub name: String,
    pub totals: ImportTotals,
    pub created_at_ms: i64,
    pub reverted_at_ms: Option<i64>,
}

/// What undoing an import did, or with a dry run would do.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportUndone {
    pub tasks: u32,
    pub projects: u32,
    pub milestones: u32,
    pub labels: u32,
    pub initiatives: u32,
    pub notes: u32,
    pub views: u32,
    /// Notes changed since the import, kept.
    pub notes_kept: u32,
    /// Labels now on tasks the import did not make, kept.
    pub labels_kept: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportReverted {
    pub import: ImportRecord,
    pub undone: ImportUndone,
}

fn read_file(path: &Path) -> Result<String, String> {
    let bytes =
        std::fs::read(path).map_err(|_| format!("{} could not be read.", path.display()))?;
    String::from_utf8(bytes).map_err(|_| format!("{} is not a text file.", path.display()))
}

fn is_json(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

/// Reads `paths` as `source` exports them into one batch, with the name
/// the import is kept under.
pub(crate) fn read_paths(
    source: ImportSource,
    paths: &[PathBuf],
    map: &CsvMap,
) -> Result<(Batch, String), String> {
    if paths.is_empty() {
        return Err("Choose a file to import.".to_owned());
    }
    let mut batch = Batch::default();
    let mut names = Vec::with_capacity(paths.len());
    for path in paths {
        let text = read_file(path)?;
        let read = match source {
            ImportSource::Linear if is_json(path) => linear::read_json(&text),
            ImportSource::Linear => linear::read_csv(&text),
            ImportSource::Csv => sheets::read_csv(&text, map),
            ImportSource::Jira => sheets::read_jira(&text),
            ImportSource::Asana => sheets::read_asana(&text),
            ImportSource::Shortcut => sheets::read_shortcut(&text),
            ImportSource::Github => boards::read_github(&text),
            ImportSource::Trello => boards::read_trello(&text),
        }
        .map_err(|message| format!("{}: {message}", file_name(path)))?;
        batch.merge(read);
        names.push(file_name(path));
    }
    Ok((batch, names.join(", ")))
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// A short fingerprint of what was read, so a replayed import is known by
/// its content rather than carried whole in the change log.
pub(crate) fn digest(batch: &Batch) -> String {
    let mut hash = Sha256::new();
    hash.update(format!("{batch:?}"));
    hex::encode(&hash.finalize()[..16])
}

fn state_of(task: &SourceTask, now_ms: i64) -> ImportState {
    if task.status.is_closed() || task.status == WorkStatus::Duplicate {
        return ImportState::Completed;
    }
    let touched = task.updated_at_ms.or(task.created_at_ms).unwrap_or(now_ms);
    if task.archived_at_ms.is_some() || touched < now_ms - STALE_MS {
        ImportState::Stale
    } else {
        ImportState::Active
    }
}

/// The Sona record an earlier import, not undone, made or matched for
/// `source_id`, while it is still there.
fn known(
    conn: &Connection,
    source: ImportSource,
    kind: ImportKind,
    source_id: &str,
) -> Result<Option<String>> {
    let target: Option<String> = conn
        .query_row(
            "SELECT i.target_id FROM planning_import_items i
               JOIN planning_imports r ON r.id = i.import_id
              WHERE r.reverted_at_ms IS NULL AND r.source = ?1 AND i.kind = ?2 AND i.source_id = ?3
              ORDER BY r.created_at_ms DESC LIMIT 1",
            params![source.as_str(), kind.as_str(), source_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(target) = target else {
        return Ok(None);
    };
    let table = match kind {
        ImportKind::Task => "planning_tasks",
        ImportKind::Project => "planning_projects",
        ImportKind::Milestone => "planning_milestones",
        ImportKind::Label => "planning_labels",
        ImportKind::Initiative => "planning_initiatives",
        ImportKind::Comment => "planning_task_comments",
        ImportKind::Note => "scratch_notes",
        ImportKind::View => "planning_views",
    };
    let there: bool = conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id = ?1)"),
        [&target],
        |row| row.get(0),
    )?;
    Ok(there.then_some(target))
}

fn lowercase_match(conn: &Connection, sql: &str, name: &str) -> Result<Option<String>> {
    Ok(conn.query_row(sql, [name], |row| row.get(0)).optional()?)
}

fn default_hue(name: &str) -> f64 {
    let sum: u32 = name.bytes().map(u32::from).sum();
    f64::from(sum % 12) * 30.0
}

struct Writer<'a> {
    tx: &'a Connection,
    root: &'a Path,
    change: Change<'a>,
    source: ImportSource,
    options: &'a ImportOptions,
    import_id: String,
    report: ImportReport,
    effects: Effects,
    labels: HashMap<String, String>,
    initiatives: HashMap<String, String>,
    projects: HashMap<String, String>,
    milestones: HashMap<String, (String, String)>,
    tasks: HashMap<String, String>,
    /// Names the person importing goes by there.
    me: Vec<String>,
}

impl Writer<'_> {
    fn keep(&self, kind: ImportKind, source_id: &str, target_id: &str, made: bool) -> Result<()> {
        self.tx.execute(
            "INSERT INTO planning_import_items (import_id, kind, source_id, target_id, made)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(import_id, kind, source_id) DO NOTHING",
            params![self.import_id, kind.as_str(), source_id, target_id, made],
        )?;
        Ok(())
    }

    fn row(&mut self, kind: ImportKind, title: &str, action: ImportAction, reason: Option<String>) {
        let tally = match kind {
            ImportKind::Task => &mut self.report.totals.tasks,
            ImportKind::Project => &mut self.report.totals.projects,
            ImportKind::Milestone => &mut self.report.totals.milestones,
            ImportKind::Label => &mut self.report.totals.labels,
            ImportKind::Initiative => &mut self.report.totals.initiatives,
            ImportKind::Comment => &mut self.report.totals.comments,
            ImportKind::Note => &mut self.report.totals.notes,
            ImportKind::View => &mut self.report.totals.views,
        };
        tally.count(action);
        if !matches!(
            kind,
            ImportKind::Comment | ImportKind::Milestone | ImportKind::Label
        ) {
            self.report.rows.push(ImportRow {
                kind,
                title: title.to_owned(),
                source_ref: None,
                state: None,
                action,
                reason,
            });
        }
    }

    fn label(
        &mut self,
        source_id: &str,
        name: &str,
        group: Option<&str>,
        hue: Option<f64>,
    ) -> Result<String> {
        if let Some(id) = known(self.tx, self.source, ImportKind::Label, source_id)? {
            self.keep(ImportKind::Label, source_id, &id, false)?;
            self.row(ImportKind::Label, name, ImportAction::Skipped, None);
            return Ok(id);
        }
        if let Some(id) = lowercase_match(
            self.tx,
            "SELECT id FROM planning_labels WHERE lower(name) = lower(?1)",
            name.trim(),
        )? {
            self.keep(ImportKind::Label, source_id, &id, false)?;
            self.row(ImportKind::Label, name, ImportAction::Matched, None);
            return Ok(id);
        }
        let made = items::save_label(
            self.tx,
            None,
            name,
            hue.unwrap_or_else(|| default_hue(name)),
            self.change.at_ms,
        )?;
        if let Some(group) = group {
            super::routines::group_label(self.tx, &made.id, Some(group), self.change.at_ms)?;
        }
        self.keep(ImportKind::Label, source_id, &made.id, true)?;
        self.row(ImportKind::Label, name, ImportAction::Made, None);
        Ok(made.id)
    }

    fn labels(&mut self, batch: &Batch) -> Result<Option<String>> {
        if self.options.copy.labels {
            for label in &batch.labels {
                let id = self.label(&label.id, &label.name, label.group.as_deref(), label.hue)?;
                self.labels.insert(label.id.clone(), id);
            }
        }
        let Some(name) = self.options.label.as_deref().and_then(batch::present) else {
            return Ok(None);
        };
        let id = self.label(
            &format!("sona import label {}", name.to_lowercase()),
            &name,
            None,
            None,
        )?;
        Ok(Some(id))
    }

    fn initiatives(&mut self, batch: &Batch) -> Result<()> {
        if !self.options.copy.initiatives || !self.options.copy.projects {
            return Ok(());
        }
        for initiative in &batch.initiatives {
            if let Some(id) = known(self.tx, self.source, ImportKind::Initiative, &initiative.id)? {
                self.keep(ImportKind::Initiative, &initiative.id, &id, false)?;
                self.initiatives.insert(initiative.id.clone(), id);
                self.row(
                    ImportKind::Initiative,
                    &initiative.name,
                    ImportAction::Skipped,
                    Some("imported before".to_owned()),
                );
                continue;
            }
            if let Some(id) = lowercase_match(
                self.tx,
                "SELECT id FROM planning_initiatives WHERE lower(name) = lower(?1)",
                initiative.name.trim(),
            )? {
                self.keep(ImportKind::Initiative, &initiative.id, &id, false)?;
                self.initiatives.insert(initiative.id.clone(), id);
                self.row(
                    ImportKind::Initiative,
                    &initiative.name,
                    ImportAction::Matched,
                    None,
                );
                continue;
            }
            let made = initiatives::save(
                self.tx,
                None,
                &InitiativeSaveInput {
                    name: Some(initiative.name.clone()),
                    description: Some(initiative.description.clone()),
                    owner: None,
                    target_at_ms: Some(initiative.target_at_ms),
                    status: initiative.status,
                },
                self.change.at_ms,
            )?;
            self.keep(ImportKind::Initiative, &initiative.id, &made.id, true)?;
            self.initiatives.insert(initiative.id.clone(), made.id);
            self.row(
                ImportKind::Initiative,
                &initiative.name,
                ImportAction::Made,
                None,
            );
        }
        Ok(())
    }

    fn projects(&mut self, batch: &Batch, wanted: &[&SourceTask]) -> Result<()> {
        if !self.options.copy.projects {
            return Ok(());
        }
        for project in &batch.projects {
            let status = project.status.unwrap_or(ProjectStatus::Active);
            let closed = matches!(status, ProjectStatus::Completed | ProjectStatus::Canceled);
            let used = wanted
                .iter()
                .any(|task| task.project.as_deref() == Some(&project.id));
            if closed && !self.options.include.completed && !used {
                self.row(
                    ImportKind::Project,
                    &project.name,
                    ImportAction::LeftOut,
                    Some("finished; finished work was left out".to_owned()),
                );
                continue;
            }
            let id =
                if let Some(id) = known(self.tx, self.source, ImportKind::Project, &project.id)? {
                    self.keep(ImportKind::Project, &project.id, &id, false)?;
                    self.row(
                        ImportKind::Project,
                        &project.name,
                        ImportAction::Skipped,
                        Some("imported before".to_owned()),
                    );
                    id
                } else if let Some(id) = lowercase_match(
                    self.tx,
                    "SELECT id FROM planning_projects WHERE lower(title) = lower(?1)",
                    project.name.trim(),
                )? {
                    self.keep(ImportKind::Project, &project.id, &id, false)?;
                    self.row(
                        ImportKind::Project,
                        &project.name,
                        ImportAction::Matched,
                        None,
                    );
                    id
                } else {
                    let start = project
                        .start_at_ms
                        .filter(|start| project.target_at_ms.is_none_or(|target| *start <= target));
                    let made = catalog::save_project(
                        self.tx,
                        None,
                        &ProjectEdit {
                            title: Some(project.name.clone()),
                            description: project.description.clone(),
                            status: Some(status),
                            priority: Some(project.priority),
                            start_at_ms: Some(start),
                            target_at_ms: Some(project.target_at_ms),
                            archived: Some(closed),
                            ..ProjectEdit::default()
                        },
                        self.change.at_ms,
                    )?;
                    if let Some(initiative) = project
                        .initiative
                        .as_ref()
                        .and_then(|id| self.initiatives.get(id))
                    {
                        initiatives::add_project(self.tx, initiative, &made.id, self.change.at_ms)?;
                    }
                    self.keep(ImportKind::Project, &project.id, &made.id, true)?;
                    let action = if closed {
                        ImportAction::Archived
                    } else {
                        ImportAction::Made
                    };
                    self.row(ImportKind::Project, &project.name, action, None);
                    self.effects.projects.push(made.id.clone());
                    made.id
                };
            self.projects.insert(project.id.clone(), id.clone());
            if self.options.copy.milestones {
                self.milestones_of(project, &id)?;
            }
        }
        Ok(())
    }

    fn milestones_of(&mut self, project: &batch::SourceProject, project_id: &str) -> Result<()> {
        for milestone in &project.milestones {
            let id = if let Some(id) = known(self.tx, self.source, ImportKind::Milestone, &milestone.id)? {
                self.keep(ImportKind::Milestone, &milestone.id, &id, false)?;
                self.row(ImportKind::Milestone, &milestone.name, ImportAction::Skipped, None);
                id
            } else if let Some(id) = self
                .tx
                .query_row(
                    "SELECT id FROM planning_milestones WHERE project_id = ?1 AND lower(title) = lower(?2)",
                    params![project_id, milestone.name.trim()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                self.keep(ImportKind::Milestone, &milestone.id, &id, false)?;
                self.row(ImportKind::Milestone, &milestone.name, ImportAction::Matched, None);
                id
            } else {
                let made = catalog::save_milestone(
                    self.tx,
                    project_id,
                    None,
                    &MilestoneEdit {
                        title: Some(milestone.name.clone()),
                        description: milestone.description.clone(),
                        due_at_ms: Some(milestone.due_at_ms),
                        status: None,
                    },
                    self.change.at_ms,
                )?;
                self.keep(ImportKind::Milestone, &milestone.id, &made.id, true)?;
                self.row(ImportKind::Milestone, &milestone.name, ImportAction::Made, None);
                made.id
            };
            self.milestones
                .insert(milestone.id.clone(), (project_id.to_owned(), id));
        }
        Ok(())
    }

    /// The Sona cycle each source cycle not yet over falls in: the one not
    /// yet ended that holds its first day still to come.
    fn cycles(&self, batch: &Batch) -> Result<HashMap<String, i64>> {
        let mut found = HashMap::new();
        if !self.options.copy.cycles {
            return Ok(found);
        }
        let today = items::local_today(self.change.at_ms)?;
        for cycle in &batch.cycles {
            if cycle.end <= today {
                continue;
            }
            let day: NaiveDate = cycle.start.max(today);
            if let Some(number) = cycles::holding(self.tx, day, self.change.at_ms)? {
                found.insert(cycle.id.clone(), number);
            }
        }
        Ok(found)
    }

    fn is_me(&self, name: &str) -> bool {
        self.me
            .iter()
            .any(|mine| mine.eq_ignore_ascii_case(name.trim()))
    }

    fn owner(&self, task: &SourceTask) -> WorkOwner {
        match &task.assignee {
            Some(name) if self.is_me(name) => WorkOwner::Myself,
            _ => WorkOwner::Nobody,
        }
    }

    fn task_labels(&self, task: &SourceTask, migrated: Option<&String>) -> Vec<String> {
        let mut ids: Vec<String> = task
            .labels
            .iter()
            .filter_map(|label| self.labels.get(label).cloned())
            .collect();
        if let Some(migrated) = migrated {
            ids.push(migrated.clone());
        }
        ids.sort();
        ids.dedup();
        ids
    }

    fn place(&self, task: &SourceTask) -> (Option<String>, Option<String>) {
        let project = task
            .project
            .as_ref()
            .and_then(|id| self.projects.get(id))
            .cloned();
        let milestone = task
            .milestone
            .as_ref()
            .and_then(|id| self.milestones.get(id))
            .filter(|(project_id, _)| project.as_deref() == Some(project_id))
            .map(|(_, id)| id.clone());
        (project, milestone)
    }

    fn update(&mut self, id: &str, task: &SourceTask, migrated: Option<&String>) -> Result<bool> {
        let (project, milestone) = self.place(task);
        let status = if task.status == WorkStatus::Duplicate {
            WorkStatus::Canceled
        } else {
            task.status
        };
        let changed = items::apply(
            self.tx,
            id,
            &WorkPatch {
                title: Some(task.title.clone()),
                details: Some(task.details.clone()),
                status: Some(status),
                priority: Some(task.priority),
                due_at_ms: Some(task.due_at_ms),
                label_ids: Some(self.task_labels(task, migrated)),
                project_id: Some(project),
                milestone_id: Some(milestone),
                estimate: Some(task.estimate),
                ..WorkPatch::default()
            },
            WorkActivityKind::Status,
            self.change.actor,
            self.change.at_ms,
        )?;
        let moved = changed.before != changed.after;
        if moved {
            self.effects.changed(&changed);
        }
        Ok(moved)
    }

    fn tasks(
        &mut self,
        wanted: &[(&SourceTask, ImportState)],
        migrated: Option<&String>,
        sona_cycles: &HashMap<String, i64>,
    ) -> Result<()> {
        let keep_numbers = self.options.keep_numbers && self.numbers_fit(wanted)?;
        if let Some(top) = wanted
            .iter()
            .filter_map(|(task, _)| task.number)
            .max()
            .filter(|_| keep_numbers)
        {
            self.tx.execute(
                "UPDATE planning_work_counter SET next_number = max(next_number, ?1) WHERE id = 1",
                [top + 1],
            )?;
        }
        let mut made = Vec::new();
        for (task, state) in wanted {
            let reference = task.reference.clone();
            if let Some(id) = known(self.tx, self.source, ImportKind::Task, &task.id)? {
                self.keep(ImportKind::Task, &task.id, &id, false)?;
                self.tasks.insert(task.id.clone(), id.clone());
                let action = if self.options.update_existing && self.update(&id, task, migrated)? {
                    ImportAction::Updated
                } else {
                    ImportAction::Skipped
                };
                let reason =
                    (action == ImportAction::Skipped).then(|| "imported before".to_owned());
                self.task_row(task, reference, *state, action, reason);
                continue;
            }
            let (project, milestone) = self.place(task);
            let labels = self.task_labels(task, migrated);
            let owner = self.owner(task);
            let status = if task.status == WorkStatus::Duplicate {
                WorkStatus::Canceled
            } else {
                task.status
            };
            let item = items::insert(
                self.tx,
                &NewItem {
                    title: &task.title,
                    details: task.details.as_deref(),
                    status,
                    priority: task.priority.min(4),
                    project_id: project.as_deref(),
                    milestone_id: milestone.as_deref(),
                    owner: &owner,
                    due_at_ms: task.due_at_ms,
                    label_ids: &labels,
                    source_loop_id: None,
                    recurrence: None,
                },
                self.change.actor,
                self.change.at_ms,
            )?;
            if task.estimate.is_some() {
                items::apply(
                    self.tx,
                    &item.id,
                    &WorkPatch {
                        estimate: Some(task.estimate),
                        ..WorkPatch::default()
                    },
                    WorkActivityKind::Estimate,
                    self.change.actor,
                    self.change.at_ms,
                )?;
            }
            items::backdate(self.tx, &item.id, task.created_at_ms, task.completed_at_ms)?;
            if let Some(number) = task.number.filter(|_| keep_numbers) {
                self.tx.execute(
                    "UPDATE planning_tasks SET number = ?2 WHERE id = ?1",
                    params![item.id, number],
                )?;
            }
            let cycle = task
                .cycle
                .as_ref()
                .and_then(|id| sona_cycles.get(id))
                .copied();
            if let (Some(cycle), true) = (cycle, status.is_open() || status == WorkStatus::Triage) {
                if item.cycle != Some(cycle) {
                    items::place_in_cycle(
                        self.tx,
                        &item.id,
                        item.cycle,
                        Some(cycle),
                        self.change.actor,
                        self.change.at_ms,
                    )?;
                }
                self.report.totals.in_cycles += 1;
            }
            let archive = *state != ImportState::Active;
            if archive {
                items::set_archived(
                    self.tx,
                    &item.id,
                    true,
                    self.change.actor,
                    self.change.at_ms,
                )?;
            }
            if let Some(url) = task
                .url
                .as_deref()
                .filter(|url| attachments::link(url).is_ok())
            {
                attachments::attach_link(
                    self.tx,
                    self.root,
                    &item.id,
                    url,
                    attachments::Naming {
                        title: Some(&format!(
                            "{} {}",
                            self.source.name(),
                            task.reference.as_deref().unwrap_or(&task.id)
                        )),
                        subtitle: None,
                        metadata: None,
                        comment_id: None,
                    },
                    self.author(None),
                    self.change.at_ms,
                )?;
            }
            if self.options.copy.comments {
                self.comments(&item.id, task)?;
            }
            self.keep(ImportKind::Task, &task.id, &item.id, true)?;
            self.tasks.insert(task.id.clone(), item.id.clone());
            self.effects.items.push(item.id.clone());
            let action = if archive {
                ImportAction::Archived
            } else {
                ImportAction::Made
            };
            self.task_row(task, reference, *state, action, None);
            made.push(*task);
        }
        for task in made {
            let Some(parent) = task.parent.as_ref().and_then(|id| self.tasks.get(id)) else {
                continue;
            };
            let Some(id) = self.tasks.get(&task.id) else {
                continue;
            };
            items::apply(
                self.tx,
                id,
                &WorkPatch {
                    parent_id: Some(Some(parent.clone())),
                    ..WorkPatch::default()
                },
                WorkActivityKind::Status,
                self.change.actor,
                self.change.at_ms,
            )?;
        }
        if keep_numbers {
            self.tx.execute(
                "UPDATE planning_work_counter SET next_number =
                    (SELECT coalesce(max(number), 0) + 1 FROM planning_tasks) WHERE id = 1",
                [],
            )?;
        }
        Ok(())
    }

    /// Whether numbers can be kept: Sona has no tasks and no two of these
    /// share a number. A note says why not.
    fn numbers_fit(&mut self, wanted: &[(&SourceTask, ImportState)]) -> Result<bool> {
        let mut seen = HashMap::new();
        for (task, _) in wanted {
            if let Some(number) = task.number {
                if let Some(other) = seen.insert(number, task.id.as_str()) {
                    self.report.notes.push(format!(
                        "{other} and {} share the number {number}, so numbers were not kept.",
                        task.id
                    ));
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn task_row(
        &mut self,
        task: &SourceTask,
        source_ref: Option<String>,
        state: ImportState,
        action: ImportAction,
        reason: Option<String>,
    ) {
        self.report.totals.tasks.count(action);
        self.report.rows.push(ImportRow {
            kind: ImportKind::Task,
            title: task.title.clone(),
            source_ref,
            state: Some(state),
            action,
            reason,
        });
    }

    fn author<'n>(&self, name: Option<&'n str>) -> Author<'n> {
        match name {
            Some(name) if self.is_me(name) => Author {
                actor: WorkActor::Me,
                agent: None,
            },
            Some(name) => Author {
                actor: WorkActor::Agent,
                agent: Some(name),
            },
            None => Author {
                actor: self.change.actor,
                agent: None,
            },
        }
    }

    fn comments(&mut self, task_id: &str, task: &SourceTask) -> Result<()> {
        let mut made: HashMap<&str, String> = HashMap::new();
        for comment in &task.comments {
            let name = comment.author.as_deref().unwrap_or(self.source.name());
            let reply_to = comment
                .parent
                .as_deref()
                .and_then(|parent| made.get(parent))
                .cloned();
            let added = comments::add(
                self.tx,
                WorkCommentSubject::Task,
                task_id,
                reply_to.as_deref(),
                self.author(Some(name)),
                &comment.body,
                comment.created_at_ms.unwrap_or(self.change.at_ms),
            )?;
            self.keep(ImportKind::Comment, &comment.id, &added.id, true)?;
            self.report.totals.comments.count(ImportAction::Made);
            made.insert(comment.id.as_str(), added.id);
        }
        Ok(())
    }

    fn documents(&mut self, batch: &Batch) -> Result<()> {
        if !self.options.copy.documents {
            return Ok(());
        }
        for document in &batch.documents {
            if let Some(id) = known(self.tx, self.source, ImportKind::Note, &document.id)? {
                self.keep(ImportKind::Note, &document.id, &id, false)?;
                self.row(
                    ImportKind::Note,
                    &document.title,
                    ImportAction::Skipped,
                    Some("imported before".to_owned()),
                );
                continue;
            }
            let body = if document.body.trim_start().starts_with('#') {
                document.body.clone()
            } else {
                format!("# {}\n\n{}", document.title, document.body.trim())
            };
            let note = scratchpad::create_note_in(
                self.tx,
                body.trim_end(),
                scratchpad::Author::Agent(Some(self.source.name())),
                document.created_at_ms.unwrap_or(self.change.at_ms),
            )
            .map_err(|error| match error {
                scratchpad::ScratchpadError::Refused(_) => anyhow::Error::from(Refusal::Invalid),
                scratchpad::ScratchpadError::Storage(error) => error,
            })?;
            if let Some(project) = document
                .project
                .as_ref()
                .and_then(|id| self.projects.get(id))
            {
                store::insert_link(
                    self.tx,
                    &SourceLinkDraft {
                        target_kind: EvidenceTargetKind::Project,
                        target_id: project,
                        source_kind: EvidenceSourceKind::Note,
                        source_id: &note.id,
                        at_ms: None,
                        meeting_note_id: None,
                    },
                    self.change.at_ms,
                )?;
            }
            self.keep(ImportKind::Note, &document.id, &note.id, true)?;
            self.row(ImportKind::Note, &document.title, ImportAction::Made, None);
        }
        Ok(())
    }

    fn views(&mut self, batch: &Batch) -> Result<()> {
        if !self.options.copy.views {
            return Ok(());
        }
        for view in &batch.views {
            if let Some(id) = known(self.tx, self.source, ImportKind::View, &view.id)? {
                self.keep(ImportKind::View, &view.id, &id, false)?;
                self.row(
                    ImportKind::View,
                    &view.name,
                    ImportAction::Skipped,
                    Some("imported before".to_owned()),
                );
                continue;
            }
            let query = match view
                .filter
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|filter| self.query(batch, filter))
            {
                Ok(query) => query,
                Err(reason) => {
                    self.row(
                        ImportKind::View,
                        &view.name,
                        ImportAction::LeftOut,
                        Some(reason),
                    );
                    continue;
                }
            };
            let made = items::save_view(
                self.tx,
                None,
                &view.name,
                &query,
                &WorkDisplay::standard(),
                self.change.at_ms,
            )?;
            self.keep(ImportKind::View, &view.id, &made.id, true)?;
            self.row(ImportKind::View, &view.name, ImportAction::Made, None);
        }
        Ok(())
    }

    fn query(&self, batch: &Batch, filter: &ViewFilter) -> Result<WorkQuery, String> {
        let mut query = WorkQuery::default();
        if !filter.statuses.is_empty() {
            query.statuses = Some(filter.statuses.clone());
        }
        if !filter.priorities.is_empty() {
            query.priorities = Some(filter.priorities.clone());
        }
        if !filter.projects.is_empty() {
            let ids = filter
                .projects
                .iter()
                .map(|id| self.projects.get(id).cloned())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| "a project it shows was not imported".to_owned())?;
            query.project_ids = Some(ids);
        }
        if !filter.labels.is_empty() {
            let ids = filter
                .labels
                .iter()
                .map(|wanted| {
                    batch
                        .labels
                        .iter()
                        .find(|label| label.id == *wanted || label.name.to_lowercase() == *wanted)
                        .and_then(|label| self.labels.get(&label.id).cloned())
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| "a label it shows was not imported".to_owned())?;
            query.label_ids = Some(ids);
        }
        Ok(query)
    }
}

fn has_tasks(conn: &Connection) -> Result<bool> {
    Ok(
        conn.query_row("SELECT EXISTS(SELECT 1 FROM planning_tasks)", [], |row| {
            row.get(0)
        })?,
    )
}

/// Lands `batch` as `options` say, recording what it made under a new
/// import. Run inside a transaction never committed, it is the preview.
fn write(
    tx: &Connection,
    root: &Path,
    source: ImportSource,
    name: &str,
    batch: &Batch,
    options: &ImportOptions,
    change: Change<'_>,
) -> Result<(ImportReport, Effects)> {
    let numbers_keepable = !has_tasks(tx)?;
    if options.keep_numbers && !numbers_keepable {
        return Err(Refusal::HasTasks.into());
    }
    let import_id = Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO planning_imports (id, source, name, totals, created_at_ms)
         VALUES (?1, ?2, ?3, '{}', ?4)",
        params![import_id, source.as_str(), name, change.at_ms],
    )?;
    let quiet = inbox::last_posted(tx)?;
    let mut writer = Writer {
        tx,
        root,
        change,
        source,
        options,
        import_id,
        report: ImportReport {
            id: None,
            source,
            name: name.to_owned(),
            label: options.label.as_deref().and_then(batch::present),
            totals: ImportTotals::default(),
            rows: Vec::new(),
            notes: Vec::new(),
            numbers_keepable,
        },
        effects: Effects::default(),
        labels: HashMap::new(),
        initiatives: HashMap::new(),
        projects: HashMap::new(),
        milestones: HashMap::new(),
        tasks: HashMap::new(),
        me: options.me.iter().chain(&batch.me).cloned().collect(),
    };
    let mut wanted = Vec::new();
    let mut left_out = Vec::new();
    for task in &batch.tasks {
        let state = state_of(task, change.at_ms);
        let found = &mut writer.report.totals.found;
        match state {
            ImportState::Active => found.active += 1,
            ImportState::Stale => found.stale += 1,
            ImportState::Completed => found.completed += 1,
        }
        if options.include.takes(state) {
            wanted.push((task, state));
        } else {
            left_out.push((task, state));
        }
    }
    let migrated = writer.labels(batch)?;
    writer.initiatives(batch)?;
    let wanted_tasks: Vec<&SourceTask> = wanted.iter().map(|(task, _)| *task).collect();
    writer.projects(batch, &wanted_tasks)?;
    let sona_cycles = writer.cycles(batch)?;
    writer.tasks(&wanted, migrated.as_ref(), &sona_cycles)?;
    for (task, state) in left_out {
        let reason = match state {
            ImportState::Active => "open work was left out",
            ImportState::Stale => "untouched for six months; stale work was left out",
            ImportState::Completed => "finished; finished work was left out",
        };
        let reference = task.reference.clone();
        writer.task_row(
            task,
            reference,
            state,
            ImportAction::LeftOut,
            Some(reason.to_owned()),
        );
    }
    writer.documents(batch)?;
    writer.views(batch)?;
    inbox::forget_since(tx, quiet)?;
    let Writer {
        mut report,
        effects,
        import_id,
        ..
    } = writer;
    tx.execute(
        "UPDATE planning_imports SET totals = ?2 WHERE id = ?1",
        params![import_id, serde_json::to_string(&report.totals)?],
    )?;
    report.id = Some(import_id);
    Ok((report, effects))
}

fn record(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, String, String, String, i64, Option<i64>)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn record_of(
    (id, source, name, totals, created_at_ms, reverted_at_ms): (
        String,
        String,
        String,
        String,
        i64,
        Option<i64>,
    ),
) -> Result<ImportRecord> {
    Ok(ImportRecord {
        id,
        source: ImportSource::parse(&source).ok_or(Refusal::Invalid)?,
        name,
        totals: serde_json::from_str(&totals).unwrap_or_default(),
        created_at_ms,
        reverted_at_ms,
    })
}

/// The tasks an import made.
pub(crate) fn made_tasks(conn: &Connection, import_id: &str) -> Result<Vec<String>> {
    made_by(conn, import_id, ImportKind::Task)
}

/// Every import, newest first.
pub(crate) fn imports(conn: &Connection) -> Result<Vec<ImportRecord>> {
    let mut statement = conn.prepare(
        "SELECT id, source, name, totals, created_at_ms, reverted_at_ms FROM planning_imports
         ORDER BY created_at_ms DESC, id",
    )?;
    let rows = statement
        .query_map([], record)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(record_of).collect()
}

pub(crate) fn import(conn: &Connection, id: &str) -> Result<ImportRecord> {
    let row = conn
        .query_row(
            "SELECT id, source, name, totals, created_at_ms, reverted_at_ms FROM planning_imports
             WHERE id = ?1",
            [store::uuid(id)?],
            record,
        )
        .optional()?
        .ok_or(Refusal::NotFound)?;
    record_of(row)
}

fn made_by(conn: &Connection, import_id: &str, kind: ImportKind) -> Result<Vec<String>> {
    let mut statement = conn.prepare_cached(
        "SELECT target_id FROM planning_import_items WHERE import_id = ?1 AND kind = ?2 AND made = 1
         ORDER BY target_id",
    )?;
    let ids = statement
        .query_map(params![import_id, kind.as_str()], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

fn exists(conn: &Connection, sql: &str, id: &str) -> Result<bool> {
    Ok(conn.query_row(sql, [id], |row| row.get(0))?)
}

/// Undoes an import: its tasks and projects go to the bin for 30 days,
/// milestones it added to projects it did not make are removed, and so are
/// its labels no other task carries, its initiatives, its views and its
/// notes nobody changed since. An import already undone changes nothing.
fn revert(
    conn: &Connection,
    id: &str,
    actor: WorkActor,
    now_ms: i64,
) -> Result<(ImportReverted, Effects)> {
    let found = import(conn, id)?;
    let mut undone = ImportUndone::default();
    let mut effects = Effects::default();
    if found.reverted_at_ms.is_some() {
        return Ok((
            ImportReverted {
                import: found,
                undone,
            },
            effects,
        ));
    }
    for task in made_by(conn, &found.id, ImportKind::Task)? {
        if exists(
            conn,
            "SELECT EXISTS(SELECT 1 FROM planning_tasks WHERE id = ?1 AND trashed_at_ms IS NULL)",
            &task,
        )? {
            let changed = bin::trash(conn, &task, actor, now_ms)?;
            effects.changed(&changed);
            undone.tasks += 1;
        }
    }
    let projects = made_by(conn, &found.id, ImportKind::Project)?;
    for milestone in made_by(conn, &found.id, ImportKind::Milestone)? {
        let Ok(kept) = store::milestone(conn, &milestone) else {
            continue;
        };
        if !projects.contains(&kept.project_id) {
            catalog::delete_milestone(conn, &milestone, now_ms)?;
            undone.milestones += 1;
        }
    }
    for project in &projects {
        if exists(conn, "SELECT EXISTS(SELECT 1 FROM planning_projects WHERE id = ?1 AND trashed_at_ms IS NULL)", project)? {
            catalog::save_project(
                conn,
                Some(project),
                &ProjectEdit {
                    trashed: Some(true),
                    ..ProjectEdit::default()
                },
                now_ms,
            )?;
            effects.projects.push(project.clone());
            undone.projects += 1;
        }
    }
    for label in made_by(conn, &found.id, ImportKind::Label)? {
        if !exists(
            conn,
            "SELECT EXISTS(SELECT 1 FROM planning_labels WHERE id = ?1)",
            &label,
        )? {
            continue;
        }
        let in_use = exists(
            conn,
            "SELECT EXISTS(SELECT 1 FROM planning_task_labels l JOIN planning_tasks t ON t.id = l.task_id
                WHERE l.label_id = ?1 AND t.trashed_at_ms IS NULL)
             OR EXISTS(SELECT 1 FROM planning_project_labels l JOIN planning_projects p
                ON p.id = l.project_id WHERE l.label_id = ?1 AND p.trashed_at_ms IS NULL)",
            &label,
        )?;
        if in_use {
            undone.labels_kept += 1;
        } else {
            items::delete_label(conn, &label)?;
            undone.labels += 1;
        }
    }
    for initiative in made_by(conn, &found.id, ImportKind::Initiative)? {
        if exists(
            conn,
            "SELECT EXISTS(SELECT 1 FROM planning_initiatives WHERE id = ?1)",
            &initiative,
        )? {
            initiatives::delete(conn, &initiative)?;
            undone.initiatives += 1;
        }
    }
    for note in made_by(conn, &found.id, ImportKind::Note)? {
        let Some(kept) = scratchpad::note_with_connection(conn, &note)? else {
            continue;
        };
        if kept.updated_at_ms == kept.created_at_ms {
            conn.execute("DELETE FROM scratch_notes WHERE id = ?1", [&note])?;
            undone.notes += 1;
        } else {
            undone.notes_kept += 1;
        }
    }
    for view in made_by(conn, &found.id, ImportKind::View)? {
        if exists(
            conn,
            "SELECT EXISTS(SELECT 1 FROM planning_views WHERE id = ?1)",
            &view,
        )? {
            items::delete_view(conn, &view)?;
            undone.views += 1;
        }
    }
    conn.execute(
        "UPDATE planning_imports SET reverted_at_ms = ?2 WHERE id = ?1",
        params![found.id, now_ms],
    )?;
    Ok((
        ImportReverted {
            import: import(conn, &found.id)?,
            undone,
        },
        effects,
    ))
}

/// Where an import's batch comes from: files on this Mac, or Linear's API
/// read with the key Sona keeps in the Keychain.
pub(crate) enum Origin<'a> {
    Files {
        paths: &'a [PathBuf],
        map: &'a CsvMap,
    },
    LinearApi,
}

impl<'a> Origin<'a> {
    /// Linear's API when the app asks to read it live, else the files.
    pub(crate) fn of(paths: &'a [PathBuf], map: &'a CsvMap, live: bool) -> Self {
        if live {
            Self::LinearApi
        } else {
            Self::Files { paths, map }
        }
    }
}

async fn gather(
    app: &AppHandle,
    source: ImportSource,
    origin: Origin<'_>,
) -> Result<(Batch, String), String> {
    match origin {
        Origin::Files { paths, map } => read_paths(source, paths, map),
        Origin::LinearApi if source == ImportSource::Linear => {
            let key = app
                .state::<Arc<SecretManager>>()
                .resolve(SecretAccount::linear_import())
                .await
                .map_err(|error| match error {
                    SecretResolveError::NotFound => {
                        "Paste a Linear personal API key first.".to_owned()
                    }
                    SecretResolveError::Store(_) => {
                        "Keychain could not read the Linear key. Unlock it and try again."
                            .to_owned()
                    }
                })?;
            let batch = linear_api::fetch(linear_api::ENDPOINT, key.expose()).await?;
            Ok((batch, "Linear".to_owned()))
        }
        Origin::LinearApi => Err("Only Linear is read through its API.".to_owned()),
    }
}

/// Whether a Linear key is saved. The app's `work_import_linear_key`.
pub(crate) async fn linear_key_state(app: &AppHandle) -> SecretState {
    app.state::<Arc<SecretManager>>()
        .state(SecretAccount::linear_import(), None)
        .await
}

/// Saves the Linear personal API key in the Keychain. The app's
/// `work_import_linear_key_set`.
pub(crate) async fn linear_key_set(app: &AppHandle, token: String) -> Result<SecretState, String> {
    let token = Zeroizing::new(token);
    if token.is_empty() || token.len() > 4_096 || !token.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err("Paste the Linear API key without spaces or line breaks.".to_owned());
    }
    app.state::<Arc<SecretManager>>()
        .replace(SecretAccount::linear_import(), token)
        .await
        .map_err(|_| {
            "The Linear key could not be saved in Keychain. Unlock Keychain and try again."
                .to_owned()
        })
}

/// Forgets the Linear key. The app's `work_import_linear_key_remove`.
pub(crate) async fn linear_key_remove(app: &AppHandle) -> Result<SecretState, String> {
    app.state::<Arc<SecretManager>>()
        .remove(SecretAccount::linear_import())
        .await
        .map_err(|_| {
            "The Linear key could not be removed from Keychain. Unlock it and try again.".to_owned()
        })
}

fn history(app: &AppHandle) -> tauri::State<'_, Arc<HistoryManager>> {
    app.state::<Arc<HistoryManager>>()
}

/// What importing would do, with nothing kept. The app's
/// `work_import_preview`.
pub(crate) async fn preview(
    app: &AppHandle,
    source: ImportSource,
    origin: Origin<'_>,
    options: &ImportOptions,
) -> Result<ImportReport, String> {
    let (batch, name) = gather(app, source, origin).await?;
    let root = super::attachment_root(app)?;
    rehearse(app, &root, source, &name, &batch, options)
}

pub(crate) fn rehearse(
    app: &AppHandle,
    root: &Path,
    source: ImportSource,
    name: &str,
    batch: &Batch,
    options: &ImportOptions,
) -> Result<ImportReport, String> {
    crate::planning::history(&history(app), |conn| {
        let tx = conn.transaction()?;
        let (mut report, _) = write(
            &tx,
            root,
            source,
            name,
            batch,
            options,
            Change::now("", WorkActor::Me),
        )?;
        report.id = None;
        Ok(report)
    })
}

/// Imports as `actor`. The app's `work_import`.
pub(crate) async fn import_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    source: ImportSource,
    origin: Origin<'_>,
    options: &ImportOptions,
) -> Result<ImportReport, String> {
    let (batch, name) = gather(app, source, origin).await?;
    write_as(app, actor, operation_id, source, &name, &batch, options)
}

pub(crate) fn write_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    source: ImportSource,
    name: &str,
    batch: &Batch,
    options: &ImportOptions,
) -> Result<ImportReport, String> {
    let root = super::attachment_root(app)?;
    let change = Change::now(operation_id, actor);
    let request = (source, name, digest(batch), options);
    run_history(app, |history| {
        store::mutate(
            history,
            change.operation_id,
            "work_import",
            &request,
            change.at_ms,
            |tx| {
                let (value, effects) = write(tx, &root, source, name, batch, options, change)?;
                Ok(Done { value, effects })
            },
        )
    })
}

/// Every import, newest first. The app's `work_imports`.
pub(crate) fn imports_read(app: &AppHandle) -> Result<Vec<ImportRecord>, String> {
    crate::planning::history(&history(app), |conn| imports(conn))
}

/// What undoing an import would do, with nothing kept.
pub(crate) fn rehearse_revert(conn: &mut Connection, id: &str) -> Result<ImportReverted> {
    let tx = conn.transaction()?;
    Ok(revert(&tx, id, WorkActor::Me, now_ms())?.0)
}

/// Undoes an import as `actor`. The app's `work_import_revert`.
pub(crate) fn revert_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    id: &str,
) -> Result<ImportReverted, String> {
    let at_ms = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_import_revert",
            &id,
            at_ms,
            |tx| {
                let (value, effects) = revert(tx, id, actor, at_ms)?;
                Ok(Done { value, effects })
            },
        )
    })
}
