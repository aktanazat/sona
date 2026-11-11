//! Project updates, the full project form, and milestone order, as the app
//! and the command line change them.

use super::super::catalog::{self, LinkEdit, ProjectEdit};
use super::super::types::{
    DateResolution, Milestone, Project, ProjectHealth, ProjectStatus, ProjectUpdate,
    ProjectUpdatePage, TaskStatus,
};
use super::super::updates;
use super::comments::Author;
use super::types::{nullable, WorkActor, WorkOwner};
use super::{now_ms, run_history, store, Done, Effects};
use crate::managers::history::HistoryManager;
use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, State};

const ME: Author<'static> = Author {
    actor: WorkActor::Me,
    agent: None,
};

fn project_change<T: serde::de::DeserializeOwned + Serialize>(
    app: &AppHandle,
    operation_id: &str,
    command: &str,
    request: &impl Serialize,
    change: impl FnOnce(&Connection, i64) -> Result<(String, T)>,
) -> Result<T, String> {
    let now = now_ms();
    run_history(app, |conn| {
        store::mutate(conn, operation_id, command, request, now, |tx| {
            let (project_id, value) = change(tx, now)?;
            Ok(Done {
                value,
                effects: Effects {
                    projects: vec![project_id],
                    ..Effects::default()
                },
            })
        })
    })
}

/// Posts an update; without `body`, its text is drafted from what moved
/// since the last one, so a replayed operation keeps the first draft.
pub(crate) fn update_add_as(
    app: &AppHandle,
    author: Author<'_>,
    operation_id: &str,
    project_id: &str,
    health: ProjectHealth,
    body: Option<&str>,
) -> Result<ProjectUpdate, String> {
    project_change(
        app,
        operation_id,
        "planning_project_update_add",
        &(project_id, health, body, author.actor, author.agent),
        |tx, now| {
            let drafted;
            let body = match body {
                Some(body) => body,
                None => {
                    drafted = updates::draft(tx, project_id)?;
                    drafted.as_str()
                }
            };
            let update = updates::add(tx, author, project_id, health, body, now)?;
            Ok((update.project_id.clone(), update))
        },
    )
}

pub(crate) fn update_edit_as(
    app: &AppHandle,
    author: Author<'_>,
    operation_id: &str,
    id: &str,
    health: Option<ProjectHealth>,
    body: Option<&str>,
) -> Result<ProjectUpdate, String> {
    project_change(
        app,
        operation_id,
        "planning_project_update_edit",
        &(id, health, body, author.actor, author.agent),
        |tx, now| {
            let update = updates::edit(tx, author, id, health, body, now)?;
            Ok((update.project_id.clone(), update))
        },
    )
}

pub(crate) fn update_delete_as(
    app: &AppHandle,
    author: Author<'_>,
    operation_id: &str,
    id: &str,
) -> Result<ProjectUpdate, String> {
    project_change(
        app,
        operation_id,
        "planning_project_update_delete",
        &(id, author.actor, author.agent),
        |tx, now| {
            let update = updates::delete(tx, author, id, now)?;
            Ok((update.project_id.clone(), update))
        },
    )
}

pub(crate) fn milestone_reorder(
    app: &AppHandle,
    operation_id: &str,
    project_id: &str,
    order: &[String],
) -> Result<Vec<Milestone>, String> {
    project_change(
        app,
        operation_id,
        "planning_milestone_reorder",
        &(project_id, order),
        |tx, now| {
            let milestones = catalog::reorder_milestones(tx, project_id, order, now)?;
            Ok((store::uuid(project_id)?, milestones))
        },
    )
}

fn page_size(limit: Option<u32>) -> usize {
    limit.map_or(updates::PAGE, |limit| {
        usize::try_from(limit).map_or(updates::MAX_PAGE, |limit| limit.clamp(1, updates::MAX_PAGE))
    })
}

/// A project's updates, newest first.
#[tauri::command(async)]
#[specta::specta]
pub fn planning_project_updates(
    history_state: State<'_, Arc<HistoryManager>>,
    project_id: String,
    limit: Option<u32>,
    cursor: Option<String>,
) -> Result<ProjectUpdatePage, String> {
    super::super::history(&history_state, |conn| {
        updates::page(conn, &project_id, page_size(limit), cursor.as_deref())
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn planning_project_update_add(
    app: AppHandle,
    operation_id: String,
    project_id: String,
    health: ProjectHealth,
    body: String,
) -> Result<ProjectUpdate, String> {
    update_add_as(&app, ME, &operation_id, &project_id, health, Some(&body))
}

#[tauri::command(async)]
#[specta::specta]
pub fn planning_project_update_edit(
    app: AppHandle,
    operation_id: String,
    id: String,
    health: Option<ProjectHealth>,
    body: Option<String>,
) -> Result<ProjectUpdate, String> {
    update_edit_as(&app, ME, &operation_id, &id, health, body.as_deref())
}

#[tauri::command(async)]
#[specta::specta]
pub fn planning_project_update_delete(
    app: AppHandle,
    operation_id: String,
    id: String,
) -> Result<ProjectUpdate, String> {
    update_delete_as(&app, ME, &operation_id, &id)
}

/// A plain draft of what moved since the project's last update.
#[tauri::command(async)]
#[specta::specta]
pub fn planning_project_update_draft(
    history_state: State<'_, Arc<HistoryManager>>,
    project_id: String,
) -> Result<String, String> {
    super::super::history(&history_state, |conn| updates::draft(conn, &project_id))
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectLinkInput {
    pub title: Option<String>,
    pub url: String,
}

/// A project's fields to set. A key left out keeps its value; `null`
/// clears a lead, description, icon, date or resolution. Lists replace the
/// whole list. `leadIsSelf: true` makes the user the lead in place of a
/// person; `membersIncludeSelf` sets whether the user is a member.
#[derive(Clone, Debug, Default, Deserialize, Serialize, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectSaveInput {
    pub title: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub description: Option<Option<String>>,
    pub status: Option<ProjectStatus>,
    pub priority: Option<u8>,
    #[serde(deserialize_with = "nullable")]
    pub lead_person_id: Option<Option<String>>,
    pub lead_is_self: Option<bool>,
    pub member_person_ids: Option<Vec<String>>,
    pub members_include_self: Option<bool>,
    #[serde(deserialize_with = "nullable")]
    pub start_at_ms: Option<Option<i64>>,
    #[serde(deserialize_with = "nullable")]
    pub start_resolution: Option<Option<DateResolution>>,
    #[serde(deserialize_with = "nullable")]
    pub target_at_ms: Option<Option<i64>>,
    #[serde(deserialize_with = "nullable")]
    pub target_resolution: Option<Option<DateResolution>>,
    pub hue: Option<u16>,
    #[serde(deserialize_with = "nullable")]
    pub icon: Option<Option<String>>,
    pub label_ids: Option<Vec<String>>,
    pub links: Option<Vec<ProjectLinkInput>>,
    pub archived: Option<bool>,
    pub trashed: Option<bool>,
}

impl ProjectSaveInput {
    fn edit(self) -> Result<ProjectEdit, String> {
        let cleared = |value: Option<Option<String>>| value.map(Option::unwrap_or_default);
        let person = self
            .lead_person_id
            .map(|id| id.filter(|id| !id.trim().is_empty()));
        let lead = match (self.lead_is_self, person) {
            (Some(true), Some(Some(_))) => {
                return Err(crate::planning::failure(store::Refusal::Invalid.into()));
            }
            (Some(true), _) => Some(WorkOwner::Myself),
            (_, Some(Some(person_id))) => Some(WorkOwner::Person { person_id }),
            (_, Some(None)) => Some(WorkOwner::Nobody),
            (_, None) => None,
        };
        Ok(ProjectEdit {
            title: self.title,
            description: cleared(self.description),
            status: self.status,
            lead,
            member_person_ids: self.member_person_ids,
            members_include_self: self.members_include_self,
            target_at_ms: self.target_at_ms,
            hue: self.hue,
            archived: self.archived,
            priority: self.priority,
            start_at_ms: self.start_at_ms,
            start_resolution: self.start_resolution,
            target_resolution: self.target_resolution,
            icon: cleared(self.icon),
            label_ids: self.label_ids,
            links: self.links.map(|links| {
                links
                    .into_iter()
                    .map(|link| LinkEdit {
                        title: link.title,
                        url: link.url,
                    })
                    .collect()
            }),
            trashed: self.trashed,
        })
    }
}

/// Makes a project, or with `id` changes one, with every field the
/// overview shows; `trashed: true` puts it in the bin and `false` restores it.
#[tauri::command(async)]
#[specta::specta]
pub fn planning_project_save(
    app: AppHandle,
    operation_id: String,
    id: Option<String>,
    input: ProjectSaveInput,
) -> Result<Project, String> {
    super::catalog_project_save(&app, &operation_id, id, &input.edit()?)
}

/// A milestone's fields to set. A key left out keeps its value; `null`
/// clears the due date.
#[derive(Clone, Debug, Default, Deserialize, Serialize, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct MilestoneSaveInput {
    pub title: Option<String>,
    pub description: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub due_at_ms: Option<Option<i64>>,
    pub status: Option<TaskStatus>,
}

/// Makes a milestone in a project, or with `id` changes one: the same save
/// `work milestone-save` makes.
#[tauri::command(async)]
#[specta::specta]
pub fn planning_milestone_save(
    app: AppHandle,
    operation_id: String,
    project_id: String,
    id: Option<String>,
    input: MilestoneSaveInput,
) -> Result<Milestone, String> {
    super::catalog_milestone_save(
        &app,
        &operation_id,
        &project_id,
        id,
        &catalog::MilestoneEdit {
            title: input.title,
            description: input.description,
            due_at_ms: input.due_at_ms,
            status: input.status,
        },
    )
}

#[tauri::command(async)]
#[specta::specta]
pub fn planning_milestone_reorder(
    app: AppHandle,
    operation_id: String,
    project_id: String,
    milestone_ids: Vec<String>,
) -> Result<Vec<Milestone>, String> {
    milestone_reorder(&app, &operation_id, &project_id, &milestone_ids)
}
