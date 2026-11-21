//! One feed of how the projects are going: every update posted on a project
//! and every weekly project digest, newest first, narrowed to some projects,
//! the projects of some initiatives or carrying some labels, some healths,
//! or a start. A narrowing can be saved under a name, as a feed.

use super::digest::ProjectDigest;
use super::{now_ms, run_history, store, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::planning::store::Refusal;
use crate::planning::types::{ProjectHealth, ProjectUpdate, ProjectUpdateAuthor};
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

pub(crate) const FEEDS_MIGRATION: &str = "CREATE TABLE planning_feeds (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    filter TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX planning_feeds_name ON planning_feeds(lower(name));";

/// What a feed shows; an empty list narrows nothing. A digest stays in
/// while it covers one of the projects asked for, and leaves once a health
/// is asked for.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct FeedFilter {
    pub(crate) project_ids: Vec<String>,
    pub(crate) initiative_ids: Vec<String>,
    pub(crate) label_ids: Vec<String>,
    pub(crate) healths: Vec<ProjectHealth>,
    pub(crate) since: Option<FeedSince>,
}

/// Where a feed starts: a fixed moment, or so many days back from when it
/// is read, so a saved "last 7 days" stays seven days long.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FeedSince {
    At(i64),
    LastDays(u32),
}

impl FeedSince {
    pub(crate) fn start(self, now_ms: i64) -> i64 {
        match self {
            Self::At(at_ms) => at_ms,
            Self::LastDays(days) => now_ms - i64::from(days) * 24 * 60 * 60 * 1000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Feed {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) filter: FeedFilter,
    pub(crate) created_at_ms: i64,
    pub(crate) updated_at_ms: i64,
}

/// One line of the feed.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum FeedEntry {
    Update {
        update: ProjectUpdate,
        project_title: String,
    },
    Digest {
        notice_id: String,
        created_at_ms: i64,
        digest: ProjectDigest,
    },
}

impl FeedEntry {
    pub(crate) const fn created_at_ms(&self) -> i64 {
        match self {
            Self::Update { update, .. } => update.created_at_ms,
            Self::Digest { created_at_ms, .. } => *created_at_ms,
        }
    }
}

fn json_list<T: Serialize>(values: &[T]) -> Result<Option<String>> {
    Ok(if values.is_empty() {
        None
    } else {
        Some(serde_json::to_string(values)?)
    })
}

/// The projects the filter names, through themselves, their initiatives
/// or their labels; none when it names no project.
fn projects(conn: &Connection, filter: &FeedFilter) -> Result<Option<HashSet<String>>> {
    if filter.project_ids.is_empty()
        && filter.initiative_ids.is_empty()
        && filter.label_ids.is_empty()
    {
        return Ok(None);
    }
    let mut statement = conn.prepare(
        "SELECT p.id FROM planning_projects p
         WHERE (?1 IS NULL OR p.id IN (SELECT value FROM json_each(?1)))
           AND (?2 IS NULL OR EXISTS (SELECT 1 FROM planning_initiative_projects i
                WHERE i.project_id = p.id
                  AND i.initiative_id IN (SELECT value FROM json_each(?2))))
           AND (?3 IS NULL OR EXISTS (SELECT 1 FROM planning_project_labels l
                WHERE l.project_id = p.id
                  AND l.label_id IN (SELECT value FROM json_each(?3))))",
    )?;
    let ids = statement
        .query_map(
            params![
                json_list(&filter.project_ids)?,
                json_list(&filter.initiative_ids)?,
                json_list(&filter.label_ids)?
            ],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<HashSet<String>>>()?;
    Ok(Some(ids))
}

fn read_update(row: &Row<'_>) -> rusqlite::Result<FeedEntry> {
    Ok(FeedEntry::Update {
        update: ProjectUpdate {
            id: row.get(0)?,
            project_id: row.get(1)?,
            health: row.get(2)?,
            body: Some(row.get(3)?),
            progress: row.get(4)?,
            author: ProjectUpdateAuthor {
                kind: row.get(5)?,
                name: row.get(6)?,
            },
            created_at_ms: row.get(7)?,
            edited_at_ms: row.get(8)?,
            deleted: false,
        },
        project_title: row.get(9)?,
    })
}

/// The feed at `now_ms`, newest first, at most `limit` long.
pub(crate) fn entries(
    conn: &Connection,
    filter: &FeedFilter,
    limit: usize,
    now_ms: i64,
) -> Result<Vec<FeedEntry>> {
    let wanted = projects(conn, filter)?;
    let since = filter.since.map_or(i64::MIN, |since| since.start(now_ms));
    let mut statement = conn.prepare(
        "SELECT u.id, u.project_id, u.health, u.body, u.progress, u.author_kind, u.author_name,
                u.created_at_ms, u.edited_at_ms, p.title
         FROM planning_project_updates u JOIN planning_projects p ON p.id = u.project_id
         WHERE u.deleted_at_ms IS NULL AND p.trashed_at_ms IS NULL AND u.created_at_ms >= ?1
           AND (?2 IS NULL OR u.health IN (SELECT value FROM json_each(?2)))
         ORDER BY u.created_at_ms DESC, u.rowid DESC",
    )?;
    let healths: Vec<&str> = filter
        .healths
        .iter()
        .map(|health| health.as_str())
        .collect();
    let mut found = statement
        .query_map(params![since, json_list(&healths)?], read_update)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    found.retain(|entry| match (entry, &wanted) {
        (FeedEntry::Update { update, .. }, Some(ids)) => ids.contains(&update.project_id),
        _ => true,
    });
    if filter.healths.is_empty() {
        let mut statement = conn.prepare(
            "SELECT id, created_at_ms, digest FROM planning_notifications
             WHERE kind = 'digest' AND created_at_ms >= ?1",
        )?;
        let digests = statement
            .query_map([since], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (notice_id, created_at_ms, json) in digests {
            let digest: ProjectDigest = serde_json::from_str(&json)?;
            let covers = wanted.as_ref().is_none_or(|ids| {
                digest
                    .projects
                    .iter()
                    .any(|project| ids.contains(&project.id))
            });
            if covers {
                found.push(FeedEntry::Digest {
                    notice_id,
                    created_at_ms,
                    digest,
                });
            }
        }
    }
    found.sort_by_key(|entry| std::cmp::Reverse(entry.created_at_ms()));
    found.truncate(limit);
    Ok(found)
}

fn read_feed(row: &Row<'_>) -> rusqlite::Result<(String, String, String, i64, i64)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn feed_of(
    (id, name, filter, created_at_ms, updated_at_ms): (String, String, String, i64, i64),
) -> Result<Feed> {
    Ok(Feed {
        id,
        name,
        filter: serde_json::from_str(&filter)?,
        created_at_ms,
        updated_at_ms,
    })
}

pub(crate) fn feeds(conn: &Connection) -> Result<Vec<Feed>> {
    let mut statement = conn.prepare(
        "SELECT id, name, filter, created_at_ms, updated_at_ms FROM planning_feeds
         ORDER BY lower(name), id",
    )?;
    let rows = statement
        .query_map([], read_feed)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(feed_of).collect()
}

pub(crate) fn feed(conn: &Connection, id: &str) -> Result<Feed> {
    feed_of(
        conn.query_row(
            "SELECT id, name, filter, created_at_ms, updated_at_ms FROM planning_feeds
             WHERE id = ?1",
            [id],
            read_feed,
        )
        .optional()?
        .ok_or(Refusal::NotFound)?,
    )
}

/// Saves a feed: the one `id` names, else the one with that name ignoring
/// case, else a new one.
pub(crate) fn save_feed(
    conn: &Connection,
    id: Option<&str>,
    name: &str,
    filter: &FeedFilter,
    now_ms: i64,
) -> Result<Feed> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Refusal::Invalid.into());
    }
    let id = match id {
        Some(id) => feed(conn, &store::uuid(id)?)?.id,
        None => conn
            .query_row(
                "SELECT id FROM planning_feeds WHERE lower(name) = lower(?1)",
                [name],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
    };
    let taken: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_feeds WHERE lower(name) = lower(?1) AND id != ?2)",
        params![name, id],
        |row| row.get(0),
    )?;
    if taken {
        return Err(Refusal::Invalid.into());
    }
    conn.execute(
        "INSERT INTO planning_feeds (id, name, filter, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, filter = excluded.filter,
            updated_at_ms = excluded.updated_at_ms",
        params![id, name, serde_json::to_string(filter)?, now_ms],
    )?;
    feed(conn, &id)
}

pub(crate) fn delete_feed(conn: &Connection, id: &str) -> Result<Feed> {
    let found = feed(conn, &store::uuid(id)?)?;
    conn.execute("DELETE FROM planning_feeds WHERE id = ?1", [&found.id])?;
    Ok(found)
}

fn reading<T>(app: &AppHandle, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| work(conn))
}

/// The feed. The app's `work_updates`.
pub(crate) fn work_updates(
    app: AppHandle,
    filter: FeedFilter,
    limit: Option<usize>,
) -> Result<Vec<FeedEntry>, String> {
    reading(&app, |conn| {
        entries(conn, &filter, limit.unwrap_or(200), now_ms())
    })
}

/// Every saved feed. The app's `work_feeds`.
pub(crate) fn work_feeds(app: AppHandle) -> Result<Vec<Feed>, String> {
    reading(&app, feeds)
}

/// Saves a feed. The app's `work_feed_save`.
pub(crate) fn feed_save_as(
    app: &AppHandle,
    operation_id: &str,
    id: Option<&str>,
    name: &str,
    filter: &FeedFilter,
) -> Result<Feed, String> {
    let now = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_feed_save",
            &(id, name, filter),
            now,
            |tx| {
                Ok(Done {
                    value: save_feed(tx, id, name, filter, now)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

/// Deletes a feed. The app's `work_feed_delete`.
pub(crate) fn feed_delete_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
) -> Result<Feed, String> {
    let now = now_ms();
    run_history(app, |history| {
        store::mutate(history, operation_id, "work_feed_delete", &id, now, |tx| {
            Ok(Done {
                value: delete_feed(tx, id)?,
                effects: Effects::default(),
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::schema;
    use super::{entries, feed, save_feed, FeedEntry, FeedFilter, FeedSince};
    use rusqlite::{params, Connection};

    const AT: i64 = 1_700_000_000_000;
    const DAY: i64 = 24 * 60 * 60 * 1000;

    fn update(conn: &Connection, project_id: &str, body: &str, at_ms: i64) {
        conn.execute(
            "INSERT INTO planning_project_updates
                (id, project_id, health, body, progress, author_kind, created_at_ms)
             VALUES (?1, ?2, 'onTrack', ?1, 0, 'agent', ?3)",
            params![body, project_id, at_ms],
        )
        .expect("an update");
    }

    #[test]
    fn a_saved_last_seven_days_feed_stays_seven_days_long() {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        let project = crate::planning::store::insert_project(&conn, "Harbor launch", None, AT)
            .expect("a project")
            .id;
        let filter = FeedFilter {
            since: Some(FeedSince::LastDays(7)),
            ..FeedFilter::default()
        };
        let saved = save_feed(&conn, None, "This week", &filter, AT).expect("the feed saved");
        update(&conn, &project, "venue booked", AT + DAY);
        update(&conn, &project, "badges printed", AT + 9 * DAY);

        let read = feed(&conn, &saved.id).expect("the feed");
        let shown: Vec<String> = entries(&conn, &read.filter, 50, AT + 10 * DAY)
            .expect("the entries")
            .into_iter()
            .filter_map(|entry| match entry {
                FeedEntry::Update { update, .. } => update.body,
                FeedEntry::Digest { .. } => None,
            })
            .collect();
        assert_eq!(shown, ["badges printed"]);
    }
}
