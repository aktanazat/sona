//! The scratchpad: dictations and typed drafts kept as editable notes.
//!
//! Notes are user content, so they live in the dictation history database and
//! inherit its encryption at rest and its startup unlock
//! (`managers/history/storage.rs`): one key, one lock, one file. The two
//! tables arrive through the history `MIGRATIONS`; this module owns every read
//! and write against them, the automatic title, and the version history.
//!
//! Nothing here logs a note body or writes one anywhere but that database. A
//! command failure reaches the screen as one plain sentence; the SQLite
//! message, which can name a path or a bound value, goes to the log.

use crate::managers::history::HistoryManager;
use anyhow::Result;
use log::error;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use tauri_specta::Event;
use uuid::Uuid;

/// How many saved states one note keeps. The oldest goes when a new one lands.
pub const MAX_VERSIONS_PER_NOTE: usize = 50;

/// Saves closer together than this fold into the newest version instead of
/// each becoming one: an autosave fires after every pause in typing, and a
/// history of every pause is not a history anyone can read. A restore never
/// folds, and a save never folds into a restore.
const VERSION_MERGE_WINDOW_MS: i64 = 10 * 60 * 1_000;

/// A body with fewer characters than this *and* fewer words than
/// [`TITLE_MIN_WORDS`] is too short to be titled by its own words.
const TITLE_MIN_CHARS: usize = 10;
const TITLE_MIN_WORDS: usize = 3;
/// How much of the first line becomes the title.
const TITLE_MAX_WORDS: usize = 8;
const TITLE_MAX_CHARS: usize = 60;
/// The title of a note whose body is too short to name it.
pub const UNTITLED: &str = "Untitled";

const NOTE_COLUMNS: &str = "id, title, body, pinned, created_at_ms, updated_at_ms";

/// One note, as the screen and every other slice read it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ScratchNote {
    /// UUID v4, minted when the note is first saved.
    pub id: String,
    /// The first words of the body, or [`UNTITLED`] while the body is too
    /// short to name it. Derived on every write, never typed.
    pub title: String,
    pub body: String,
    pub pinned: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// How a version came to be.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ScratchVersionKind {
    /// Typed or dictated into the editor.
    Edit,
    /// An earlier version put back.
    Restore,
}

impl ScratchVersionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Restore => "restore",
        }
    }

    fn from_stored(value: &str) -> Self {
        match value {
            "restore" => Self::Restore,
            _ => Self::Edit,
        }
    }
}

/// One saved state of a note. The newest is the body the note has now.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ScratchNoteVersion {
    pub id: i64,
    pub note_id: String,
    pub kind: ScratchVersionKind,
    pub body: String,
    pub saved_at_ms: i64,
}

/// A note was created, rewritten, pinned, restored or deleted, by any caller.
/// A listener re-reads the list; one that has the note open re-reads it when
/// it holds no unsaved words of its own.
#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct ScratchpadChangedEvent {
    pub note_id: String,
    pub deleted: bool,
}

/// A write the store turned down on purpose, as opposed to one that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScratchpadRefusal {
    /// A blank body is never written: not as a new note, not over a note.
    Blank,
    /// The note, or the version, is gone.
    NotFound,
}

/// Why a store call did not succeed: a refusal the store made on purpose,
/// or the storage under it failing, the lock included.
#[derive(Debug)]
pub(crate) enum ScratchpadError {
    Refused(ScratchpadRefusal),
    Storage(anyhow::Error),
}

impl From<ScratchpadRefusal> for ScratchpadError {
    fn from(refusal: ScratchpadRefusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<anyhow::Error> for ScratchpadError {
    fn from(error: anyhow::Error) -> Self {
        Self::Storage(error)
    }
}

impl From<rusqlite::Error> for ScratchpadError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.into())
    }
}

// MARK: - Title

/// The title a body earns: its first words once it has [`TITLE_MIN_CHARS`]
/// characters or [`TITLE_MIN_WORDS`] words, [`UNTITLED`] before that.
///
/// The words come from the first non-blank line, at most [`TITLE_MAX_WORDS`]
/// of them and [`TITLE_MAX_CHARS`] characters, so a note that opens with a
/// heading is titled by the heading and one that opens with a paragraph is
/// titled by how it starts.
pub fn derive_title(body: &str) -> String {
    let body = body.trim();
    if body.chars().count() < TITLE_MIN_CHARS && body.split_whitespace().count() < TITLE_MIN_WORDS {
        return UNTITLED.to_string();
    }
    let first_line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(body);
    let mut title = String::new();
    for word in first_line.split_whitespace().take(TITLE_MAX_WORDS) {
        if !title.is_empty() {
            if title.chars().count() + 1 + word.chars().count() > TITLE_MAX_CHARS {
                break;
            }
            title.push(' ');
        }
        title.push_str(word);
    }
    // One word longer than the whole allowance: cut it rather than run over.
    if title.chars().count() > TITLE_MAX_CHARS {
        title = title.chars().take(TITLE_MAX_CHARS - 1).collect();
        title.push('…');
    }
    title
}

// MARK: - Storage

fn read_note_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScratchNote> {
    Ok(ScratchNote {
        id: row.get(0)?,
        title: row.get(1)?,
        body: row.get(2)?,
        pinned: row.get(3)?,
        created_at_ms: row.get(4)?,
        updated_at_ms: row.get(5)?,
    })
}

/// Whether `query` appears, as one contiguous phrase and regardless of case,
/// in the note's title or body. A blank query matches every note.
pub(crate) fn matches_query(note: &ScratchNote, query: &str) -> bool {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    note.title.to_lowercase().contains(&needle) || note.body.to_lowercase().contains(&needle)
}

/// Every note that matches `query`, pinned ones first and each group newest
/// modified first. A pin is not a modification, so pinning never reorders the
/// group it moves the note into.
pub(crate) fn list_notes_with_connection(
    conn: &Connection,
    query: &str,
) -> Result<Vec<ScratchNote>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {NOTE_COLUMNS} FROM scratch_notes
         ORDER BY pinned DESC, updated_at_ms DESC, created_at_ms DESC, id"
    ))?;
    let notes = statement
        .query_map([], read_note_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(notes
        .into_iter()
        .filter(|note| matches_query(note, query))
        .collect())
}

pub(crate) fn note_with_connection(conn: &Connection, id: &str) -> Result<Option<ScratchNote>> {
    Ok(conn
        .query_row(
            &format!("SELECT {NOTE_COLUMNS} FROM scratch_notes WHERE id = ?1"),
            params![id],
            read_note_row,
        )
        .optional()?)
}

/// A new note around `body`, which becomes its first version. A blank body
/// is refused: an empty editor that is left is nothing to keep.
pub(crate) fn create_note_with_connection(
    conn: &mut Connection,
    body: &str,
    now_ms: i64,
) -> Result<ScratchNote, ScratchpadError> {
    if body.trim().is_empty() {
        return Err(ScratchpadRefusal::Blank.into());
    }
    let note = ScratchNote {
        id: Uuid::new_v4().to_string(),
        title: derive_title(body),
        body: body.to_string(),
        pinned: false,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    };
    let transaction = conn.transaction()?;
    transaction.execute(
        "INSERT INTO scratch_notes (id, title, body, pinned, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, 0, ?4, ?4)",
        params![note.id, note.title, note.body, now_ms],
    )?;
    record_version(
        &transaction,
        &note.id,
        &note.body,
        now_ms,
        ScratchVersionKind::Edit,
    )?;
    transaction.commit()?;
    Ok(note)
}

/// The body the editor holds now, with the title that follows from it. A
/// blank body is refused and the note keeps its words; the same body again is
/// a save of nothing and changes nothing, not even the modified time.
pub(crate) fn update_body_with_connection(
    conn: &mut Connection,
    id: &str,
    body: &str,
    now_ms: i64,
) -> Result<ScratchNote, ScratchpadError> {
    if body.trim().is_empty() {
        return Err(ScratchpadRefusal::Blank.into());
    }
    let transaction = conn.transaction()?;
    let Some(mut note) = note_with_connection(&transaction, id)? else {
        return Err(ScratchpadRefusal::NotFound.into());
    };
    if note.body != body {
        note.body = body.to_string();
        note.title = derive_title(body);
        note.updated_at_ms = now_ms;
        transaction.execute(
            "UPDATE scratch_notes SET title = ?2, body = ?3, updated_at_ms = ?4 WHERE id = ?1",
            params![id, note.title, note.body, now_ms],
        )?;
        record_version(&transaction, id, body, now_ms, ScratchVersionKind::Edit)?;
    }
    transaction.commit()?;
    Ok(note)
}

pub(crate) fn set_pinned_with_connection(
    conn: &Connection,
    id: &str,
    pinned: bool,
) -> Result<ScratchNote, ScratchpadError> {
    let changed = conn.execute(
        "UPDATE scratch_notes SET pinned = ?2 WHERE id = ?1",
        params![id, pinned],
    )?;
    if changed == 0 {
        return Err(ScratchpadRefusal::NotFound.into());
    }
    note_with_connection(conn, id)?.ok_or_else(|| ScratchpadRefusal::NotFound.into())
}

/// Removes the note; the migration's trigger removes its versions with it.
pub(crate) fn delete_note_with_connection(
    conn: &Connection,
    id: &str,
) -> Result<(), ScratchpadError> {
    let changed = conn.execute("DELETE FROM scratch_notes WHERE id = ?1", params![id])?;
    if changed == 0 {
        return Err(ScratchpadRefusal::NotFound.into());
    }
    Ok(())
}

/// The note's saved states, newest first.
pub(crate) fn versions_with_connection(
    conn: &Connection,
    note_id: &str,
) -> Result<Vec<ScratchNoteVersion>, ScratchpadError> {
    if note_with_connection(conn, note_id)?.is_none() {
        return Err(ScratchpadRefusal::NotFound.into());
    }
    let mut statement = conn.prepare(
        "SELECT id, note_id, kind, body, saved_at_ms FROM scratch_note_versions
         WHERE note_id = ?1 ORDER BY id DESC",
    )?;
    let versions = statement
        .query_map(params![note_id], |row| {
            Ok(ScratchNoteVersion {
                id: row.get(0)?,
                note_id: row.get(1)?,
                kind: ScratchVersionKind::from_stored(&row.get::<_, String>(2)?),
                body: row.get(3)?,
                saved_at_ms: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(versions)
}

/// Puts one saved state back as the body. The restore is itself a version, so
/// the words it replaced stay one step away.
pub(crate) fn restore_version_with_connection(
    conn: &mut Connection,
    note_id: &str,
    version_id: i64,
    now_ms: i64,
) -> Result<ScratchNote, ScratchpadError> {
    let transaction = conn.transaction()?;
    let Some(mut note) = note_with_connection(&transaction, note_id)? else {
        return Err(ScratchpadRefusal::NotFound.into());
    };
    let body: Option<String> = transaction
        .query_row(
            "SELECT body FROM scratch_note_versions WHERE id = ?1 AND note_id = ?2",
            params![version_id, note_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(body) = body else {
        return Err(ScratchpadRefusal::NotFound.into());
    };
    if note.body != body {
        note.title = derive_title(&body);
        note.body = body;
        note.updated_at_ms = now_ms;
        transaction.execute(
            "UPDATE scratch_notes SET title = ?2, body = ?3, updated_at_ms = ?4 WHERE id = ?1",
            params![note_id, note.title, note.body, now_ms],
        )?;
        record_version(
            &transaction,
            note_id,
            &note.body,
            now_ms,
            ScratchVersionKind::Restore,
        )?;
    }
    transaction.commit()?;
    Ok(note)
}

/// Writes one saved state, folding it into the newest version when both are
/// edits closer together than [`VERSION_MERGE_WINDOW_MS`], then drops what is
/// past [`MAX_VERSIONS_PER_NOTE`], oldest first.
fn record_version(
    conn: &Connection,
    note_id: &str,
    body: &str,
    now_ms: i64,
    kind: ScratchVersionKind,
) -> Result<()> {
    if kind == ScratchVersionKind::Edit {
        let newest: Option<(i64, String, i64)> = conn
            .query_row(
                "SELECT id, kind, saved_at_ms FROM scratch_note_versions
                 WHERE note_id = ?1 ORDER BY id DESC LIMIT 1",
                params![note_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((version_id, newest_kind, saved_at_ms)) = newest {
            if ScratchVersionKind::from_stored(&newest_kind) == ScratchVersionKind::Edit
                && now_ms.saturating_sub(saved_at_ms) < VERSION_MERGE_WINDOW_MS
            {
                conn.execute(
                    "UPDATE scratch_note_versions SET body = ?2, saved_at_ms = ?3 WHERE id = ?1",
                    params![version_id, body, now_ms],
                )?;
                return Ok(());
            }
        }
    }
    conn.execute(
        "INSERT INTO scratch_note_versions (note_id, kind, body, saved_at_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![note_id, kind.as_str(), body, now_ms],
    )?;
    conn.execute(
        "DELETE FROM scratch_note_versions
         WHERE note_id = ?1 AND id NOT IN (
            SELECT id FROM scratch_note_versions WHERE note_id = ?1 ORDER BY id DESC LIMIT ?2
         )",
        params![note_id, MAX_VERSIONS_PER_NOTE as i64],
    )?;
    Ok(())
}

// MARK: - Commands

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// One locked call against the store. A failure to take the lock or to open
/// the store is a storage failure like any other; a refusal passes through.
fn run<T>(
    history: &HistoryManager,
    action: impl FnOnce(&mut Connection) -> Result<T, ScratchpadError>,
) -> Result<T, ScratchpadError> {
    match history.with_connection(|conn| Ok(action(conn))) {
        Ok(outcome) => outcome,
        Err(error) => Err(ScratchpadError::Storage(error)),
    }
}

/// The sentence the screen shows for a failed call. A refusal names itself;
/// a storage failure is logged in full and reported as `failure`, or as the
/// unlock still running when that is what stands in the way.
fn failure_sentence(history: &HistoryManager, error: ScratchpadError, failure: &str) -> String {
    match error {
        ScratchpadError::Refused(ScratchpadRefusal::Blank) => {
            "A blank note is not saved.".to_string()
        }
        ScratchpadError::Refused(ScratchpadRefusal::NotFound) => {
            "This note no longer exists.".to_string()
        }
        ScratchpadError::Storage(error) => {
            error!("Scratchpad: {error:#}");
            if history.storage_is_ready() {
                failure.to_string()
            } else {
                "Your notes are still unlocking. Try again in a moment.".to_string()
            }
        }
    }
}

fn emit_changed(app: &AppHandle, note_id: &str, deleted: bool) {
    let _ = ScratchpadChangedEvent {
        note_id: note_id.to_string(),
        deleted,
    }
    .emit(app);
}

/// One note by id, for another slice that holds an id and needs the words:
/// `None` when it has been deleted.
pub fn read_note(app: &AppHandle, id: &str) -> Result<Option<ScratchNote>, String> {
    let history = app.state::<Arc<HistoryManager>>();
    run(&history, |conn| {
        note_with_connection(conn, id).map_err(ScratchpadError::Storage)
    })
    .map_err(|error| failure_sentence(&history, error, "Your notes could not be read."))
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_list(
    history: State<'_, Arc<HistoryManager>>,
    query: Option<String>,
) -> Result<Vec<ScratchNote>, String> {
    run(&history, |conn| {
        list_notes_with_connection(conn, query.as_deref().unwrap_or(""))
            .map_err(ScratchpadError::Storage)
    })
    .map_err(|error| failure_sentence(&history, error, "Your notes could not be read."))
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_get(
    history: State<'_, Arc<HistoryManager>>,
    id: String,
) -> Result<Option<ScratchNote>, String> {
    run(&history, |conn| {
        note_with_connection(conn, &id).map_err(ScratchpadError::Storage)
    })
    .map_err(|error| failure_sentence(&history, error, "Your notes could not be read."))
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_create(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    body: String,
) -> Result<ScratchNote, String> {
    let note = run(&history, |conn| {
        create_note_with_connection(conn, &body, now_ms())
    })
    .map_err(|error| failure_sentence(&history, error, "The note could not be saved."))?;
    emit_changed(&app, &note.id, false);
    Ok(note)
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_update_body(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    id: String,
    body: String,
) -> Result<ScratchNote, String> {
    let note = run(&history, |conn| {
        update_body_with_connection(conn, &id, &body, now_ms())
    })
    .map_err(|error| failure_sentence(&history, error, "The note could not be saved."))?;
    emit_changed(&app, &note.id, false);
    Ok(note)
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_set_pinned(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    id: String,
    pinned: bool,
) -> Result<ScratchNote, String> {
    let note = run(&history, |conn| {
        set_pinned_with_connection(conn, &id, pinned)
    })
    .map_err(|error| failure_sentence(&history, error, "The pin could not be changed."))?;
    emit_changed(&app, &note.id, false);
    Ok(note)
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_delete(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    id: String,
) -> Result<(), String> {
    run(&history, |conn| delete_note_with_connection(conn, &id))
        .map_err(|error| failure_sentence(&history, error, "The note could not be deleted."))?;
    emit_changed(&app, &id, true);
    Ok(())
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_versions(
    history: State<'_, Arc<HistoryManager>>,
    id: String,
) -> Result<Vec<ScratchNoteVersion>, String> {
    run(&history, |conn| versions_with_connection(conn, &id))
        .map_err(|error| failure_sentence(&history, error, "The note's history could not be read."))
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_restore_version(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    id: String,
    version_id: i64,
) -> Result<ScratchNote, String> {
    let note = run(&history, |conn| {
        restore_version_with_connection(conn, &id, version_id, now_ms())
    })
    .map_err(|error| failure_sentence(&history, error, "The version could not be restored."))?;
    emit_changed(&app, &note.id, false);
    Ok(note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite_migration::Migrations;

    const MINUTE_MS: i64 = 60 * 1_000;

    /// The real schema, so the tables, the trigger and the index under test
    /// are the ones a person's database has.
    fn connection() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        Migrations::new(crate::managers::history::MIGRATIONS.to_vec())
            .to_latest(&mut conn)
            .expect("apply history migrations");
        conn
    }

    fn refusal(error: &ScratchpadError) -> Option<ScratchpadRefusal> {
        match error {
            ScratchpadError::Refused(refusal) => Some(*refusal),
            ScratchpadError::Storage(_) => None,
        }
    }

    fn version_bodies(conn: &Connection, note_id: &str) -> Vec<String> {
        versions_with_connection(conn, note_id)
            .expect("versions")
            .into_iter()
            .map(|version| version.body)
            .collect()
    }

    fn note_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT count(*) FROM scratch_notes", [], |row| row.get(0))
            .expect("count notes")
    }

    #[test]
    fn title_is_untitled_until_the_body_has_ten_characters_or_three_words() {
        let cases = [
            ("", UNTITLED),
            ("   \n  ", UNTITLED),
            // 8 characters, 2 words.
            ("Hi there", UNTITLED),
            // 9 characters, 2 words: one short of the floor.
            ("Call Mom.", UNTITLED),
            // 10 characters, 2 words.
            ("Call Mom!!", "Call Mom!!"),
            // 5 characters, 3 words.
            ("a b c", "a b c"),
            ("Hello world", "Hello world"),
        ];
        for (body, expected) in cases {
            assert_eq!(derive_title(body), expected, "body {body:?}");
        }
    }

    #[test]
    fn title_takes_the_first_words_of_the_first_line() {
        assert_eq!(
            derive_title("\n\n  Groceries for the week  \nmilk, eggs, bread"),
            "Groceries for the week"
        );
        assert_eq!(
            derive_title("one two three four five six seven eight nine ten"),
            "one two three four five six seven eight"
        );
        let long_word = "x".repeat(80);
        let title = derive_title(&long_word);
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn a_blank_body_is_never_written() {
        let mut conn = connection();

        let created = create_note_with_connection(&mut conn, "  \n\t ", 1_000);
        assert_eq!(
            refusal(&created.expect_err("blank note refused")),
            Some(ScratchpadRefusal::Blank)
        );
        assert_eq!(note_count(&conn), 0);

        let note =
            create_note_with_connection(&mut conn, "Keep these words", 1_000).expect("create");
        let blanked = update_body_with_connection(&mut conn, &note.id, "   ", 2_000);
        assert_eq!(
            refusal(&blanked.expect_err("blank save refused")),
            Some(ScratchpadRefusal::Blank)
        );
        let kept = note_with_connection(&conn, &note.id)
            .expect("read")
            .expect("note still there");
        assert_eq!(kept.body, "Keep these words");
        assert_eq!(kept.updated_at_ms, 1_000);
    }

    #[test]
    fn a_note_keeps_fifty_versions_and_drops_the_oldest() {
        let mut conn = connection();
        let note = create_note_with_connection(&mut conn, "draft 1", 0).expect("create");
        // Every save lands outside the merge window, so each is its own version.
        for step in 2..=60_i64 {
            update_body_with_connection(
                &mut conn,
                &note.id,
                &format!("draft {step}"),
                step * 11 * MINUTE_MS,
            )
            .expect("save");
        }

        let bodies = version_bodies(&conn, &note.id);
        assert_eq!(bodies.len(), MAX_VERSIONS_PER_NOTE);
        assert_eq!(bodies.first().map(String::as_str), Some("draft 60"));
        assert_eq!(bodies.last().map(String::as_str), Some("draft 11"));
    }

    #[test]
    fn saves_within_ten_minutes_fold_into_one_version() {
        let mut conn = connection();
        let note = create_note_with_connection(&mut conn, "first words", 0).expect("create");
        update_body_with_connection(&mut conn, &note.id, "first words, more", MINUTE_MS)
            .expect("save");
        update_body_with_connection(
            &mut conn,
            &note.id,
            "first words, more, done",
            2 * MINUTE_MS,
        )
        .expect("save");
        assert_eq!(
            version_bodies(&conn, &note.id),
            vec!["first words, more, done"]
        );

        update_body_with_connection(&mut conn, &note.id, "a new sitting", 13 * MINUTE_MS)
            .expect("save");
        assert_eq!(
            version_bodies(&conn, &note.id),
            vec!["a new sitting", "first words, more, done"]
        );
    }

    #[test]
    fn notes_list_pinned_first_then_newest_modified() {
        let mut conn = connection();
        let a = create_note_with_connection(&mut conn, "note a body", 1_000).expect("a");
        let b = create_note_with_connection(&mut conn, "note b body", 2_000).expect("b");
        let c = create_note_with_connection(&mut conn, "note c body", 3_000).expect("c");

        let ids = |conn: &Connection| -> Vec<String> {
            list_notes_with_connection(conn, "")
                .expect("list")
                .into_iter()
                .map(|note| note.id)
                .collect()
        };
        assert_eq!(ids(&conn), vec![c.id.clone(), b.id.clone(), a.id.clone()]);

        set_pinned_with_connection(&conn, &a.id, true).expect("pin");
        assert_eq!(ids(&conn), vec![a.id.clone(), c.id.clone(), b.id.clone()]);

        update_body_with_connection(&mut conn, &b.id, "note b, edited", 4_000).expect("save");
        assert_eq!(ids(&conn), vec![a.id.clone(), b.id.clone(), c.id.clone()]);

        set_pinned_with_connection(&conn, &a.id, false).expect("unpin");
        assert_eq!(ids(&conn), vec![b.id, c.id, a.id]);
    }

    #[test]
    fn search_matches_a_case_insensitive_phrase_in_title_or_body() {
        let mut conn = connection();
        let groceries =
            create_note_with_connection(&mut conn, "Grocery list\nbuy MILK and eggs", 1_000)
                .expect("groceries");
        let meeting = create_note_with_connection(&mut conn, "Meeting notes for Monday", 2_000)
            .expect("meeting");

        let ids = |query: &str| -> Vec<String> {
            list_notes_with_connection(&conn, query)
                .expect("search")
                .into_iter()
                .map(|note| note.id)
                .collect()
        };
        assert_eq!(ids("milk and"), vec![groceries.id.clone()]);
        assert_eq!(ids("  MEETING "), vec![meeting.id.clone()]);
        assert_eq!(ids("grocery LIST"), vec![groceries.id.clone()]);
        assert!(ids("milk eggs").is_empty(), "words apart are not a phrase");
        assert_eq!(ids(""), vec![meeting.id, groceries.id]);
    }

    #[test]
    fn restoring_a_version_puts_its_words_back_as_a_new_version() {
        let mut conn = connection();
        let note = create_note_with_connection(&mut conn, "the first draft", 0).expect("create");
        update_body_with_connection(&mut conn, &note.id, "the second draft", 20 * MINUTE_MS)
            .expect("save");
        let first = versions_with_connection(&conn, &note.id)
            .expect("versions")
            .into_iter()
            .find(|version| version.body == "the first draft")
            .expect("first version kept");

        let restored =
            restore_version_with_connection(&mut conn, &note.id, first.id, 21 * MINUTE_MS)
                .expect("restore");
        assert_eq!(restored.body, "the first draft");
        assert_eq!(restored.title, "the first draft");
        assert_eq!(restored.updated_at_ms, 21 * MINUTE_MS);

        let versions = versions_with_connection(&conn, &note.id).expect("versions");
        assert_eq!(versions[0].kind, ScratchVersionKind::Restore);
        assert_eq!(versions[0].body, "the first draft");
        assert_eq!(
            versions.len(),
            3,
            "the replaced words are still one step away"
        );

        // An edit right after a restore stands on its own instead of folding
        // into the restore and erasing it.
        update_body_with_connection(
            &mut conn,
            &note.id,
            "the first draft, again",
            22 * MINUTE_MS,
        )
        .expect("save");
        let versions = versions_with_connection(&conn, &note.id).expect("versions");
        assert_eq!(versions.len(), 4);
        assert_eq!(versions[0].kind, ScratchVersionKind::Edit);
        assert_eq!(versions[1].kind, ScratchVersionKind::Restore);

        let missing = restore_version_with_connection(&mut conn, &note.id, 9_999, 23 * MINUTE_MS);
        assert_eq!(
            refusal(&missing.expect_err("unknown version refused")),
            Some(ScratchpadRefusal::NotFound)
        );
    }

    #[test]
    fn deleting_a_note_removes_it_and_its_versions() {
        let mut conn = connection();
        let note = create_note_with_connection(&mut conn, "soon to be gone", 0).expect("create");
        update_body_with_connection(
            &mut conn,
            &note.id,
            "soon to be gone, edited",
            11 * MINUTE_MS,
        )
        .expect("save");

        delete_note_with_connection(&conn, &note.id).expect("delete");

        assert_eq!(note_count(&conn), 0);
        let orphaned: i64 = conn
            .query_row(
                "SELECT count(*) FROM scratch_note_versions WHERE note_id = ?1",
                params![note.id],
                |row| row.get(0),
            )
            .expect("count versions");
        assert_eq!(orphaned, 0);
        assert_eq!(
            refusal(&delete_note_with_connection(&conn, &note.id).expect_err("gone")),
            Some(ScratchpadRefusal::NotFound)
        );
    }
}
