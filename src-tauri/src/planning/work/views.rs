//! What saved views carry beyond their filters: an icon, the project whose
//! page shows a task view, and the saved views of the Projects page.

use super::super::store::{self, Refusal};
use super::super::types::ProjectStatus;
use anyhow::Result;
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

/// Task views gain an icon and a project, and the Projects page gains
/// views of its own. A task view on a project goes when the project does.
pub(crate) const VIEWS_MIGRATION: &str = "ALTER TABLE planning_views ADD COLUMN icon TEXT;
ALTER TABLE planning_views ADD COLUMN project_id TEXT;
CREATE INDEX planning_views_project ON planning_views(project_id);
CREATE TRIGGER planning_project_views_go_with_project AFTER DELETE ON planning_projects BEGIN
    DELETE FROM planning_views WHERE project_id = old.id;
END;
CREATE TABLE planning_project_views (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    icon TEXT,
    filter_json TEXT NOT NULL,
    display_json TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);";

/// The longest icon name kept: SF Symbol names run well short of it.
const ICON_LIMIT: usize = 64;

/// An SF Symbol name as a view keeps it: lowercase letters, digits and
/// dots. Empty clears it.
pub(crate) fn clean_icon(icon: Option<&str>) -> Result<Option<String>> {
    let Some(icon) = icon.map(str::trim).filter(|icon| !icon.is_empty()) else {
        return Ok(None);
    };
    let symbol = icon.len() <= ICON_LIMIT
        && !icon.starts_with('.')
        && !icon.ends_with('.')
        && icon
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.');
    if !symbol {
        return Err(Refusal::Invalid.into());
    }
    Ok(Some(icon.to_owned()))
}

/// A project's id, refused when no project has it.
pub(crate) fn existing_project(conn: &Connection, id: &str) -> Result<String> {
    let id = store::uuid(id)?;
    conn.query_row(
        "SELECT id FROM planning_projects WHERE id = ?1",
        [&id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProjectViewLayout {
    #[default]
    List,
    Board,
    Timeline,
}

/// What a project list groups by. A quarter is the one the target day falls
/// in.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProjectGroupBy {
    #[default]
    Status,
    Lead,
    Priority,
    Quarter,
    None,
}

/// Which side of a dependency a project has to be on.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProjectLinkFilter {
    Blocked,
    Blocking,
    Broken,
}

/// How long ago a project's latest update has to be, at least: a week,
/// two, a month, or before a local day, YYYY-MM-DD.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ProjectUpdateBefore {
    OneWeek,
    TwoWeeks,
    OneMonth,
    Day { day: String },
}

/// Which projects a project view keeps; a field left out keeps them all.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectViewFilter {
    pub status: Option<ProjectStatus>,
    /// Only projects with an open task past its due day.
    pub overdue_only: bool,
    /// Only projects with open work the user owns.
    pub mine_only: bool,
    /// Only projects whose latest update is older, and those with none.
    pub update_before: Option<ProjectUpdateBefore>,
    pub link: Option<ProjectLinkFilter>,
    /// An initiative's id, or `none` for projects in no initiative.
    pub initiative: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectViewDisplay {
    pub layout: ProjectViewLayout,
    pub group_by: ProjectGroupBy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub filter: ProjectViewFilter,
    pub display: ProjectViewDisplay,
    pub revision: i64,
}

/// A project view as it is saved: everything but its id and revision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectViewInput {
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub filter: ProjectViewFilter,
    #[serde(default)]
    pub display: ProjectViewDisplay,
}

const PROJECT_VIEW_COLUMNS: &str = "id, name, icon, filter_json, display_json, revision";

fn read_project_view(
    row: &Row<'_>,
) -> rusqlite::Result<(String, String, Option<String>, String, String, i64)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn project_view_from(
    (id, name, icon, filter, display, revision): (
        String,
        String,
        Option<String>,
        String,
        String,
        i64,
    ),
) -> Result<ProjectView> {
    Ok(ProjectView {
        id,
        name,
        icon,
        filter: serde_json::from_str(&filter)?,
        display: serde_json::from_str(&display)?,
        revision,
    })
}

pub(crate) fn project_views(conn: &Connection) -> Result<Vec<ProjectView>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {PROJECT_VIEW_COLUMNS} FROM planning_project_views ORDER BY created_at_ms, id"
    ))?;
    let rows = statement
        .query_map([], read_project_view)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(project_view_from).collect()
}

pub(crate) fn save_project_view(
    conn: &Connection,
    id: Option<&str>,
    view: &ProjectViewInput,
    now_ms: i64,
) -> Result<ProjectView> {
    let name = view.name.trim();
    if name.is_empty() {
        return Err(Refusal::Invalid.into());
    }
    let icon = clean_icon(view.icon.as_deref())?;
    let mut filter = view.filter.clone();
    filter.initiative = match filter.initiative.as_deref().map(str::trim) {
        None => None,
        Some(none) if none.eq_ignore_ascii_case("none") => Some("none".to_owned()),
        Some(initiative) => Some(store::uuid(initiative)?),
    };
    if let Some(ProjectUpdateBefore::Day { day }) = &mut filter.update_before {
        *day = NaiveDate::parse_from_str(day.trim(), "%Y-%m-%d")
            .map_err(|_| Refusal::Invalid)?
            .format("%Y-%m-%d")
            .to_string();
    }
    let id = match id {
        Some(id) => store::uuid(id)?,
        None => Uuid::new_v4().to_string(),
    };
    conn.execute(
        "INSERT INTO planning_project_views (id, name, icon, filter_json, display_json, revision,
            created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, icon = excluded.icon,
            filter_json = excluded.filter_json, display_json = excluded.display_json,
            revision = revision + 1, updated_at_ms = excluded.updated_at_ms",
        params![
            id,
            name,
            icon,
            serde_json::to_string(&filter)?,
            serde_json::to_string(&view.display)?,
            now_ms
        ],
    )?;
    project_view_from(conn.query_row(
        &format!("SELECT {PROJECT_VIEW_COLUMNS} FROM planning_project_views WHERE id = ?1"),
        [&id],
        read_project_view,
    )?)
}

pub(crate) fn delete_project_view(conn: &Connection, id: &str) -> Result<()> {
    if conn.execute(
        "DELETE FROM planning_project_views WHERE id = ?1",
        [store::uuid(id)?],
    )? == 0
    {
        return Err(Refusal::NotFound.into());
    }
    Ok(())
}
