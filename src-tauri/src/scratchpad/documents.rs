//! Notes as documents: what a note was written for. A project, milestone or
//! task holds a note as one of its planning sources; a cycle holds the notes
//! filed under its number here.

use super::{
    emit_changed, failure_sentence, note_with_connection, read_note_row, run, ScratchNote,
    ScratchpadError, ScratchpadRefusal,
};
use crate::managers::history::HistoryManager;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, State};

pub const CYCLE_MIGRATION: &str = "CREATE TABLE scratch_note_cycles (
    note_id TEXT NOT NULL,
    cycle INTEGER NOT NULL CHECK(cycle > 0),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(note_id, cycle)
);
CREATE INDEX scratch_note_cycles_cycle ON scratch_note_cycles(cycle, created_at_ms);
CREATE TRIGGER scratch_note_cycles_note_delete AFTER DELETE ON scratch_notes BEGIN
    DELETE FROM scratch_note_cycles WHERE note_id = old.id;
END;";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ScratchDocumentKind {
    Project,
    Milestone,
    Task,
    Cycle,
}

/// One thing a note is a document of. `id` is a cycle's number for a cycle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ScratchDocumentOf {
    pub kind: ScratchDocumentKind,
    pub id: String,
    pub title: String,
}

/// The projects, milestones, tasks and cycles note `id` is a document of,
/// oldest link first.
pub(crate) fn documents_of(conn: &Connection, id: &str) -> rusqlite::Result<Vec<ScratchDocumentOf>> {
    let mut statement = conn.prepare_cached(
        "SELECT kind, target, title FROM (
            SELECT e.target_kind AS kind, e.target_id AS target,
                coalesce(p.title, m.title, t.title) AS title, e.created_at_ms AS at, e.id AS tie
            FROM planning_evidence_links e
            LEFT JOIN planning_projects p ON e.target_kind = 'project' AND p.id = e.target_id
            LEFT JOIN planning_milestones m ON e.target_kind = 'milestone' AND m.id = e.target_id
            LEFT JOIN planning_tasks t ON e.target_kind = 'task' AND t.id = e.target_id
            WHERE e.source_kind = 'note' AND e.source_id = ?1
            UNION ALL
            SELECT 'cycle', CAST(n.cycle AS TEXT),
                coalesce(c.name, 'Cycle ' || n.cycle), n.created_at_ms, ''
            FROM scratch_note_cycles n LEFT JOIN planning_cycles c ON c.number = n.cycle
            WHERE n.note_id = ?1
         ) WHERE title IS NOT NULL ORDER BY at, tie",
    )?;
    let documents = statement
        .query_map(params![id], |row| {
            let kind = match row.get::<_, String>(0)?.as_str() {
                "project" => ScratchDocumentKind::Project,
                "milestone" => ScratchDocumentKind::Milestone,
                "task" => ScratchDocumentKind::Task,
                _ => ScratchDocumentKind::Cycle,
            };
            Ok(ScratchDocumentOf {
                kind,
                id: row.get(1)?,
                title: row.get(2)?,
            })
        })?
        .collect();
    documents
}

/// Files note `id` under cycle `number`. Filing it twice changes nothing.
pub(crate) fn link_cycle_in(
    conn: &Connection,
    id: &str,
    number: i64,
    now_ms: i64,
) -> Result<bool, ScratchpadError> {
    if note_with_connection(conn, id)?.is_none() {
        return Err(ScratchpadRefusal::NotFound.into());
    }
    let cycle: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_cycles WHERE number = ?1)",
        [number],
        |row| row.get(0),
    )?;
    if !cycle {
        return Err(ScratchpadRefusal::NotFound.into());
    }
    let added = conn.execute(
        "INSERT OR IGNORE INTO scratch_note_cycles (note_id, cycle, created_at_ms) VALUES (?1, ?2, ?3)",
        params![id, number, now_ms],
    )?;
    Ok(added > 0)
}

/// Takes note `id` out of cycle `number`; true when it was there.
pub(crate) fn unlink_cycle_in(
    conn: &Connection,
    id: &str,
    number: i64,
) -> Result<bool, ScratchpadError> {
    if note_with_connection(conn, id)?.is_none() {
        return Err(ScratchpadRefusal::NotFound.into());
    }
    let removed = conn.execute(
        "DELETE FROM scratch_note_cycles WHERE note_id = ?1 AND cycle = ?2",
        params![id, number],
    )?;
    Ok(removed > 0)
}

/// The notes filed under cycle `number`, newest modified first.
pub(crate) fn cycle_notes_with_connection(
    conn: &Connection,
    number: i64,
) -> rusqlite::Result<Vec<ScratchNote>> {
    let mut statement = conn.prepare_cached(
        "SELECT n.id, n.title, n.body, n.pinned, n.created_at_ms, n.updated_at_ms
         FROM scratch_notes n JOIN scratch_note_cycles c ON c.note_id = n.id
         WHERE c.cycle = ?1 ORDER BY n.updated_at_ms DESC, n.id",
    )?;
    let notes = statement.query_map([number], read_note_row)?.collect();
    notes
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_cycle_notes(
    history: State<'_, Arc<HistoryManager>>,
    number: i64,
) -> Result<Vec<ScratchNote>, String> {
    run(&history, |conn| Ok(cycle_notes_with_connection(conn, number)?))
        .map_err(|error| failure_sentence(&history, error, "The cycle's notes could not be read."))
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_link_cycle(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    id: String,
    number: i64,
) -> Result<(), String> {
    run(&history, |conn| {
        link_cycle_in(conn, &id, number, chrono::Utc::now().timestamp_millis())
    })
    .map_err(|error| failure_sentence(&history, error, "The note could not be filed under the cycle."))?;
    emit_changed(&app, &id, false);
    Ok(())
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_unlink_cycle(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    id: String,
    number: i64,
) -> Result<(), String> {
    run(&history, |conn| unlink_cycle_in(conn, &id, number))
        .map_err(|error| failure_sentence(&history, error, "The note could not be taken off the cycle."))?;
    emit_changed(&app, &id, false);
    Ok(())
}
