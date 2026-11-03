use super::{note_with_connection, ScratchNote};
use crate::managers::history::HistoryManager;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::State;
use uuid::Uuid;

pub const MIGRATION: &str = "CREATE TABLE scratch_subjects (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    current_note_id TEXT,
    created_at_ms INTEGER NOT NULL
);
CREATE TABLE scratch_subject_notes (
    subject_id TEXT NOT NULL REFERENCES scratch_subjects(id),
    note_id TEXT NOT NULL REFERENCES scratch_notes(id),
    added_at_ms INTEGER NOT NULL,
    PRIMARY KEY(subject_id, note_id)
);
CREATE INDEX scratch_subject_notes_note ON scratch_subject_notes(note_id);
CREATE TRIGGER scratch_subject_note_delete AFTER DELETE ON scratch_notes BEGIN
    DELETE FROM scratch_subject_notes WHERE note_id = old.id;
    UPDATE scratch_subjects SET current_note_id = NULL WHERE current_note_id = old.id;
END;
CREATE TRIGGER scratch_subject_delete AFTER DELETE ON scratch_subjects BEGIN
    DELETE FROM scratch_subject_notes WHERE subject_id = old.id;
END;";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ScratchSubject {
    pub id: String,
    pub title: String,
    pub current_note_id: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ScratchSubjectPage {
    pub subject: ScratchSubject,
    pub current: Option<ScratchNote>,
    pub history: Vec<ScratchNote>,
}

fn subject_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScratchSubject> {
    Ok(ScratchSubject {
        id: row.get(0)?,
        title: row.get(1)?,
        current_note_id: row.get(2)?,
        created_at_ms: row.get(3)?,
    })
}

pub(crate) fn read(
    history: &HistoryManager,
    id: &str,
) -> Result<Option<ScratchSubjectPage>, String> {
    history.with_connection(|conn| {
        let subject = conn.query_row("SELECT id, title, current_note_id, created_at_ms FROM scratch_subjects WHERE id = ?1", params![id], subject_row).optional()?;
        let Some(subject) = subject else { return Ok(None) };
        let current = subject.current_note_id.as_deref().map(|id| note_with_connection(conn, id)).transpose()?.flatten();
        let mut statement = conn.prepare("SELECT n.id, n.title, n.body, n.pinned, n.created_at_ms, n.updated_at_ms FROM scratch_notes n JOIN scratch_subject_notes s ON s.note_id = n.id WHERE s.subject_id = ?1 ORDER BY s.added_at_ms DESC, n.id")?;
        let history = statement.query_map(params![id], super::read_note_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(ScratchSubjectPage { subject, current, history }))
    }).map_err(|_| "The subject could not be read.".to_string())
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_subjects(
    history: State<'_, Arc<HistoryManager>>,
) -> Result<Vec<ScratchSubject>, String> {
    history.with_connection(|conn| {
        let mut statement = conn.prepare("SELECT id, title, current_note_id, created_at_ms FROM scratch_subjects ORDER BY title COLLATE NOCASE, id")?;
        let subjects = statement.query_map([], subject_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(subjects)
    }).map_err(|_| "Subjects could not be read.".to_string())
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_subject(
    history: State<'_, Arc<HistoryManager>>,
    id: String,
) -> Result<Option<ScratchSubjectPage>, String> {
    read(&history, &id)
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_subject_create(
    history: State<'_, Arc<HistoryManager>>,
    title: String,
) -> Result<ScratchSubject, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Name the subject.".to_string());
    }
    let subject = ScratchSubject {
        id: Uuid::new_v4().to_string(),
        title: title.to_string(),
        current_note_id: None,
        created_at_ms: super::now_ms(),
    };
    history
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO scratch_subjects (id, title, created_at_ms) VALUES (?1, ?2, ?3)",
                params![subject.id, subject.title, subject.created_at_ms],
            )?;
            Ok(())
        })
        .map_err(|_| "The subject could not be saved.".to_string())?;
    Ok(subject)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SubjectNoteAction {
    Add,
    SetCurrent,
    Remove,
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_subject_note(
    history: State<'_, Arc<HistoryManager>>,
    id: String,
    note_id: String,
    action: SubjectNoteAction,
) -> Result<ScratchSubjectPage, String> {
    history.with_connection(|conn| {
        let transaction = conn.transaction()?;
        if !transaction.query_row("SELECT EXISTS(SELECT 1 FROM scratch_subjects WHERE id = ?1)", params![id], |row| row.get::<_, bool>(0))? || note_with_connection(&transaction, &note_id)?.is_none() {
            anyhow::bail!("missing subject or note");
        }
        match action {
            SubjectNoteAction::Add | SubjectNoteAction::SetCurrent => {
                transaction.execute("INSERT INTO scratch_subject_notes (subject_id, note_id, added_at_ms) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING", params![id, note_id, super::now_ms()])?;
                if action == SubjectNoteAction::SetCurrent { transaction.execute("UPDATE scratch_subjects SET current_note_id = ?2 WHERE id = ?1", params![id, note_id])?; }
            }
            SubjectNoteAction::Remove => {
                transaction.execute("DELETE FROM scratch_subject_notes WHERE subject_id = ?1 AND note_id = ?2", params![id, note_id])?;
                transaction.execute("UPDATE scratch_subjects SET current_note_id = NULL WHERE id = ?1 AND current_note_id = ?2", params![id, note_id])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }).map_err(|_| "The subject could not be updated.".to_string())?;
    read(&history, &id)?.ok_or_else(|| "This subject no longer exists.".to_string())
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_subject_delete(
    history: State<'_, Arc<HistoryManager>>,
    id: String,
) -> Result<(), String> {
    history
        .with_connection(|conn| {
            conn.execute("DELETE FROM scratch_subjects WHERE id = ?1", params![id])?;
            Ok(())
        })
        .map_err(|_| "The subject could not be deleted.".to_string())
}
