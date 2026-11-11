//! Favorite tasks, projects, saved views and people, in the user's order,
//! loose or in folders. Folders hold favorites only, never other folders.
//! Folders and loose favorites share one order at the top.

use super::types::WorkActor;
use super::{run_history, run_with_meetings, store, Change, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::planning::sources;
use crate::planning::store::Refusal;
use crate::planning::ConversationsClosed;
use anyhow::Result;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

/// People join the kinds, folders and an order arrive. The rebuild drops
/// and makes again the triggers that clear favorites, so the rename finds
/// no trigger naming a table that is gone.
pub(crate) const FAVORITE_FOLDERS_MIGRATION: &str = "DROP TRIGGER planning_task_lifecycle_delete;
DROP TRIGGER planning_favorite_project_delete;
DROP TRIGGER planning_favorite_view_delete;
CREATE TABLE planning_favorite_folders (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    position INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE TABLE planning_favorites_next (
    kind TEXT NOT NULL CHECK(kind IN ('task', 'project', 'view', 'person')),
    target_id TEXT NOT NULL,
    folder_id TEXT,
    position INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (kind, target_id)
);
INSERT INTO planning_favorites_next (kind, target_id, folder_id, position, created_at_ms)
    SELECT kind, target_id, NULL, ROW_NUMBER() OVER (ORDER BY created_at_ms, rowid) - 1,
        created_at_ms
    FROM planning_favorites;
DROP TABLE planning_favorites;
ALTER TABLE planning_favorites_next RENAME TO planning_favorites;
CREATE TRIGGER planning_task_lifecycle_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_task_versions WHERE task_id = old.id;
    DELETE FROM planning_task_subscriptions WHERE task_id = old.id;
    DELETE FROM planning_notifications WHERE task_id = old.id;
    DELETE FROM planning_favorites WHERE kind = 'task' AND target_id = old.id;
END;
CREATE TRIGGER planning_favorite_project_delete AFTER DELETE ON planning_projects BEGIN
    DELETE FROM planning_favorites WHERE kind = 'project' AND target_id = old.id;
END;
CREATE TRIGGER planning_favorite_view_delete AFTER DELETE ON planning_views BEGIN
    DELETE FROM planning_favorites WHERE kind = 'view' AND target_id = old.id;
END;";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FavoriteKind {
    Task,
    Project,
    View,
    Person,
}

crate::planning::types::sql_enum!(FavoriteKind {
    Task => "task",
    Project => "project",
    View => "view",
    Person => "person",
});

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Favorite {
    pub(crate) kind: FavoriteKind,
    pub(crate) target_id: String,
    /// The folder it sits in, or none at the top.
    pub(crate) folder_id: Option<String>,
    /// Its place in its folder, or at the top among folders and loose
    /// favorites.
    pub(crate) position: i64,
    pub(crate) created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FavoriteFolder {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) position: i64,
    pub(crate) created_at_ms: i64,
}

/// Every folder and every favorite, each list in its own order.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Favorites {
    pub(crate) folders: Vec<FavoriteFolder>,
    pub(crate) favorites: Vec<Favorite>,
}

/// One thing in the favorites' order: a favorite or a folder.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FavoriteEntry {
    Favorite { kind: FavoriteKind, id: String },
    Folder { id: String },
}

impl Favorites {
    /// The top of the list in order: folders and loose favorites.
    pub(crate) fn top(&self) -> Vec<FavoriteEntry> {
        let mut top: Vec<(i64, FavoriteEntry)> = self
            .folders
            .iter()
            .map(|folder| (folder.position, FavoriteEntry::Folder { id: folder.id.clone() }))
            .chain(
                self.favorites
                    .iter()
                    .filter(|favorite| favorite.folder_id.is_none())
                    .map(|favorite| {
                        (
                            favorite.position,
                            FavoriteEntry::Favorite {
                                kind: favorite.kind,
                                id: favorite.target_id.clone(),
                            },
                        )
                    }),
            )
            .collect();
        top.sort_by_key(|(position, _)| *position);
        top.into_iter().map(|(_, entry)| entry).collect()
    }

    /// The favorites in one folder, in order.
    pub(crate) fn inside<'a>(&'a self, folder: &'a str) -> impl Iterator<Item = &'a Favorite> {
        self.favorites
            .iter()
            .filter(move |favorite| favorite.folder_id.as_deref() == Some(folder))
    }

    pub(crate) fn contains(&self, kind: FavoriteKind, id: &str) -> bool {
        self.favorites
            .iter()
            .any(|favorite| favorite.kind == kind && favorite.target_id == id)
    }
}

fn exists(
    conn: &Connection,
    meetings: Option<&Connection>,
    kind: FavoriteKind,
    id: &str,
) -> Result<Option<String>> {
    let sql = match kind {
        FavoriteKind::Task => {
            "SELECT EXISTS(SELECT 1 FROM planning_tasks WHERE id = ?1 AND trashed_at_ms IS NULL)"
        }
        FavoriteKind::Project => {
            "SELECT EXISTS(SELECT 1 FROM planning_projects WHERE id = ?1 AND trashed_at_ms IS NULL)"
        }
        FavoriteKind::View => "SELECT EXISTS(SELECT 1 FROM planning_views WHERE id = ?1)",
        FavoriteKind::Person => {
            return sources::person(meetings.ok_or(ConversationsClosed)?, id);
        }
    };
    let found: bool = conn.query_row(sql, [id], |row| row.get(0))?;
    Ok(found.then(|| id.to_owned()))
}

/// The next free place at the end of a folder, or of the top.
fn next_position(conn: &Connection, folder: Option<&str>) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM (
            SELECT position FROM planning_favorite_folders WHERE ?1 IS NULL
            UNION ALL
            SELECT position FROM planning_favorites WHERE folder_id IS ?1
         )",
        [folder],
        |row| row.get(0),
    )?)
}

/// Adds or removes one favorite; a new one goes last at the top. A person
/// is checked against the conversations, which must be open to name one.
/// True when it changed.
pub(crate) fn set(
    conn: &Connection,
    meetings: Option<&Connection>,
    kind: FavoriteKind,
    id: &str,
    on: bool,
    now_ms: i64,
) -> Result<bool> {
    let id = store::uuid(id)?;
    if !on {
        return Ok(conn.execute(
            "DELETE FROM planning_favorites WHERE kind = ?1 AND target_id = ?2",
            params![kind.as_str(), id],
        )? > 0);
    }
    let id = exists(conn, meetings, kind, &id)?.ok_or(Refusal::NotFound)?;
    Ok(conn.execute(
        "INSERT INTO planning_favorites (kind, target_id, folder_id, position, created_at_ms)
         VALUES (?1, ?2, NULL, ?3, ?4)
         ON CONFLICT(kind, target_id) DO NOTHING",
        params![kind.as_str(), id, next_position(conn, None)?, now_ms],
    )? > 0)
}

pub(crate) fn favorites(conn: &Connection) -> Result<Favorites> {
    let mut statement = conn.prepare(
        "SELECT id, name, position, created_at_ms FROM planning_favorite_folders
         ORDER BY position, rowid",
    )?;
    let folders = statement
        .query_map([], |row| {
            Ok(FavoriteFolder {
                id: row.get(0)?,
                name: row.get(1)?,
                position: row.get(2)?,
                created_at_ms: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut statement = conn.prepare(
        "SELECT kind, target_id, folder_id, position, created_at_ms FROM planning_favorites
         ORDER BY position, rowid",
    )?;
    let favorites = statement
        .query_map([], |row| {
            Ok(Favorite {
                kind: row.get(0)?,
                target_id: row.get(1)?,
                folder_id: row.get(2)?,
                position: row.get(3)?,
                created_at_ms: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Favorites { folders, favorites })
}

fn folder_exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_favorite_folders WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?)
}

/// Writes `entries` as the whole order of a folder, or of the top.
fn arrange(conn: &Connection, folder: Option<&str>, entries: &[FavoriteEntry]) -> Result<()> {
    let mut favorite = conn.prepare_cached(
        "UPDATE planning_favorites SET folder_id = ?1, position = ?2
         WHERE kind = ?3 AND target_id = ?4",
    )?;
    let mut folders =
        conn.prepare_cached("UPDATE planning_favorite_folders SET position = ?1 WHERE id = ?2")?;
    for (position, entry) in (0_i64..).zip(entries) {
        match entry {
            FavoriteEntry::Favorite { kind, id } => {
                favorite.execute(params![folder, position, kind.as_str(), id])?;
            }
            FavoriteEntry::Folder { id } => {
                folders.execute(params![position, id])?;
            }
        }
    }
    Ok(())
}

/// A new folder, or a change to one.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FolderSave {
    /// The folder to change, or none for a new one.
    pub(crate) id: Option<String>,
    /// Its name. A new folder needs one.
    pub(crate) name: Option<String>,
    /// Its place at the top from 0. A new folder goes last without one.
    pub(crate) index: Option<usize>,
}

/// Makes a folder or changes one's name or place.
pub(crate) fn save_folder(conn: &Connection, save: &FolderSave, now_ms: i64) -> Result<FavoriteFolder> {
    let name = save.name.as_deref().map(str::trim);
    if name.is_some_and(str::is_empty) {
        return Err(Refusal::Invalid.into());
    }
    let id = match (&save.id, name) {
        (Some(id), name) => {
            let id = store::uuid(id)?;
            if !folder_exists(conn, &id)? {
                return Err(Refusal::NotFound.into());
            }
            if let Some(name) = name {
                conn.execute(
                    "UPDATE planning_favorite_folders SET name = ?1 WHERE id = ?2",
                    params![name, id],
                )?;
            }
            id
        }
        (None, Some(name)) => {
            let id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO planning_favorite_folders (id, name, position, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, name, next_position(conn, None)?, now_ms],
            )?;
            id
        }
        (None, None) => return Err(Refusal::Invalid.into()),
    };
    if let Some(index) = save.index {
        move_entry(conn, &FavoriteEntry::Folder { id: id.clone() }, None, Some(index))?;
    }
    Ok(conn.query_row(
        "SELECT id, name, position, created_at_ms FROM planning_favorite_folders WHERE id = ?1",
        [&id],
        |row| {
            Ok(FavoriteFolder {
                id: row.get(0)?,
                name: row.get(1)?,
                position: row.get(2)?,
                created_at_ms: row.get(3)?,
            })
        },
    )?)
}

/// Deletes a folder. Its favorites stay, at the top where it was.
pub(crate) fn delete_folder(conn: &Connection, id: &str) -> Result<()> {
    let id = store::uuid(id)?;
    if !folder_exists(conn, &id)? {
        return Err(Refusal::NotFound.into());
    }
    let all = favorites(conn)?;
    let top = all
        .top()
        .into_iter()
        .flat_map(|entry| match &entry {
            FavoriteEntry::Folder { id: folder } if *folder == id => all
                .inside(folder)
                .map(|favorite| FavoriteEntry::Favorite {
                    kind: favorite.kind,
                    id: favorite.target_id.clone(),
                })
                .collect(),
            _ => vec![entry],
        })
        .collect::<Vec<_>>();
    arrange(conn, None, &top)?;
    conn.execute("DELETE FROM planning_favorite_folders WHERE id = ?1", [&id])?;
    Ok(())
}

/// Moves a favorite into a folder, or to the top, at `index` there, or
/// last. A folder moves only at the top. True when the order changed.
pub(crate) fn move_entry(
    conn: &Connection,
    entry: &FavoriteEntry,
    into: Option<&str>,
    index: Option<usize>,
) -> Result<bool> {
    let entry = match entry {
        FavoriteEntry::Favorite { kind, id } => FavoriteEntry::Favorite {
            kind: *kind,
            id: store::uuid(id)?,
        },
        FavoriteEntry::Folder { id } => FavoriteEntry::Folder {
            id: store::uuid(id)?,
        },
    };
    let into = into.map(store::uuid).transpose()?;
    let all = favorites(conn)?;
    let found = match &entry {
        FavoriteEntry::Favorite { kind, id } => all.contains(*kind, id),
        FavoriteEntry::Folder { id } => {
            if into.is_some() {
                return Err(Refusal::Invalid.into());
            }
            all.folders.iter().any(|folder| folder.id == *id)
        }
    };
    if !found || into.as_deref().is_some_and(|folder| !all.folders.iter().any(|f| f.id == folder)) {
        return Err(Refusal::NotFound.into());
    }
    let before = match &into {
        Some(folder) => all
            .inside(folder)
            .map(|favorite| FavoriteEntry::Favorite {
                kind: favorite.kind,
                id: favorite.target_id.clone(),
            })
            .collect(),
        None => all.top(),
    };
    let mut after = before.clone();
    after.retain(|kept| *kept != entry);
    let at = index.unwrap_or(after.len()).min(after.len());
    after.insert(at, entry);
    if after == before {
        return Ok(false);
    }
    arrange(conn, into.as_deref(), &after)?;
    Ok(true)
}

/// Where the user moves one favorite or folder.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FavoriteMove {
    pub(crate) entry: FavoriteEntry,
    /// The folder it goes into, or none for the top.
    pub(crate) into: Option<String>,
    /// Its place there from 0, or none for last.
    pub(crate) index: Option<usize>,
}

fn setting(
    history: &mut Connection,
    meetings: Option<&Connection>,
    change: Change<'_>,
    kind: FavoriteKind,
    id: &str,
    on: bool,
) -> Result<Done<bool>> {
    store::mutate(
        history,
        change.operation_id,
        "work_favorite",
        &(kind, id, on),
        change.at_ms,
        |tx| {
            Ok(Done {
                value: set(tx, meetings, kind, id, on, change.at_ms)?,
                effects: Effects::default(),
            })
        },
    )
}

/// Adds or removes a favorite. True when it changed.
pub(crate) async fn set_as(
    app: &AppHandle,
    operation_id: &str,
    kind: FavoriteKind,
    id: &str,
    on: bool,
) -> Result<bool, String> {
    let change = Change::now(operation_id, WorkActor::Me);
    run_with_meetings(app, |history, meetings| {
        setting(history, meetings, change, kind, id, on)
    })
    .await
}

/// Makes a folder or changes one, as one operation.
pub(crate) fn save_folder_as(
    app: &AppHandle,
    operation_id: &str,
    save: &FolderSave,
) -> Result<FavoriteFolder, String> {
    let change = Change::now(operation_id, WorkActor::Me);
    run_history(app, |history| {
        store::mutate(
            history,
            change.operation_id,
            "work_favorite_folder_save",
            save,
            change.at_ms,
            |tx| {
                Ok(Done {
                    value: save_folder(tx, save, change.at_ms)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

/// Deletes a folder and keeps its favorites, as one operation.
pub(crate) fn delete_folder_as(app: &AppHandle, operation_id: &str, id: &str) -> Result<(), String> {
    let change = Change::now(operation_id, WorkActor::Me);
    run_history(app, |history| {
        store::mutate(
            history,
            change.operation_id,
            "work_favorite_folder_delete",
            &id,
            change.at_ms,
            |tx| {
                delete_folder(tx, id)?;
                Ok(Done {
                    value: (),
                    effects: Effects::default(),
                })
            },
        )
    })
}

/// Moves a favorite or a folder, as one operation. True when the order
/// changed.
pub(crate) fn move_as(
    app: &AppHandle,
    operation_id: &str,
    moving: &FavoriteMove,
) -> Result<bool, String> {
    let change = Change::now(operation_id, WorkActor::Me);
    run_history(app, |history| {
        store::mutate(
            history,
            change.operation_id,
            "work_favorite_move",
            moving,
            change.at_ms,
            |tx| {
                Ok(Done {
                    value: move_entry(tx, &moving.entry, moving.into.as_deref(), moving.index)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

pub(crate) async fn work_favorite_set(
    app: AppHandle,
    operation_id: String,
    kind: FavoriteKind,
    id: String,
    on: bool,
) -> Result<bool, String> {
    set_as(&app, &operation_id, kind, &id, on).await
}

pub(crate) fn work_favorite_folder_save(
    app: AppHandle,
    operation_id: String,
    save: FolderSave,
) -> Result<FavoriteFolder, String> {
    save_folder_as(&app, &operation_id, &save)
}

pub(crate) fn work_favorite_folder_delete(
    app: AppHandle,
    operation_id: String,
    id: String,
) -> Result<(), String> {
    delete_folder_as(&app, &operation_id, &id)
}

pub(crate) fn work_favorite_move(
    app: AppHandle,
    operation_id: String,
    moving: FavoriteMove,
) -> Result<bool, String> {
    move_as(&app, &operation_id, &moving)
}

pub(crate) fn work_favorites(app: AppHandle) -> Result<Favorites, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| favorites(conn))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str) -> FavoriteEntry {
        FavoriteEntry::Favorite {
            kind: FavoriteKind::Task,
            id: id.to_owned(),
        }
    }

    #[test]
    fn folders_keep_their_place_and_hand_it_back() {
        let conn = Connection::open_in_memory().unwrap();
        crate::planning::schema::apply_all(&conn).unwrap();
        let [a, b, c] = [(); 3].map(|()| Uuid::new_v4().to_string());
        for id in [&a, &b, &c] {
            conn.execute(
                "INSERT INTO planning_favorites (kind, target_id, folder_id, position, created_at_ms)
                 VALUES ('task', ?1, NULL, ?2, 0)",
                params![id, next_position(&conn, None).unwrap()],
            )
            .unwrap();
        }
        let launch = save_folder(
            &conn,
            &FolderSave {
                id: None,
                name: Some("Launch".to_owned()),
                index: Some(1),
            },
            0,
        )
        .unwrap();
        let folder = FavoriteEntry::Folder {
            id: launch.id.clone(),
        };
        assert_eq!(
            favorites(&conn).unwrap().top(),
            [task(&a), folder.clone(), task(&b), task(&c)]
        );

        assert!(move_entry(&conn, &task(&c), Some(&launch.id), None).unwrap());
        assert!(move_entry(&conn, &task(&a), Some(&launch.id), Some(0)).unwrap());
        assert!(!move_entry(&conn, &task(&a), Some(&launch.id), Some(0)).unwrap());
        assert!(move_entry(&conn, &folder, Some(&launch.id), None).is_err());
        let saved = favorites(&conn).unwrap();
        assert_eq!(saved.top(), [folder.clone(), task(&b)]);
        assert_eq!(
            saved.inside(&launch.id).map(|favorite| favorite.target_id.clone()).collect::<Vec<_>>(),
            [a.clone(), c.clone()]
        );

        assert!(move_entry(&conn, &task(&b), None, Some(0)).unwrap());
        delete_folder(&conn, &launch.id).unwrap();
        assert_eq!(favorites(&conn).unwrap().top(), [task(&b), task(&a), task(&c)]);
    }
}
