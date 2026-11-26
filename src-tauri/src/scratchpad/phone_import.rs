use super::{derive_title, emit_changed, record_version, ScratchVersionKind};
use crate::managers::history::HistoryManager;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::AppHandle;
use uuid::Uuid;

pub const MIGRATION: &str = "CREATE TABLE scratch_phone_thoughts (
    vault_id TEXT NOT NULL,
    object_id TEXT NOT NULL,
    revision_id TEXT NOT NULL,
    note_id TEXT NOT NULL,
    captured_at_utc_ms INTEGER NOT NULL,
    origin TEXT NOT NULL,
    device_id TEXT NOT NULL,
    link TEXT,
    change_sequence INTEGER NOT NULL,
    PRIMARY KEY(vault_id, object_id, revision_id)
);
CREATE INDEX scratch_phone_thoughts_note ON scratch_phone_thoughts(note_id);
CREATE TABLE scratch_phone_thought_deletions (
    vault_id TEXT NOT NULL,
    object_id TEXT NOT NULL,
    revision_id TEXT NOT NULL,
    change_sequence INTEGER NOT NULL,
    PRIMARY KEY(vault_id, object_id)
);
CREATE TABLE scratch_phone_thought_backfill (vault_id TEXT PRIMARY KEY NOT NULL);";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PhoneThoughtReference {
    pub vault_id: String,
    pub object_id: String,
    pub revision_id: String,
    pub note_id: String,
    pub captured_at_utc_ms: i64,
    pub origin: String,
    pub device_id: String,
    pub link: Option<String>,
    pub source_deleted: bool,
    pub change_sequence: i64,
}

pub(crate) struct PhoneThoughtNote<'a> {
    pub vault_id: &'a str,
    pub object_id: &'a str,
    pub revision_id: &'a str,
    pub captured_at_utc_ms: i64,
    pub origin: &'a str,
    pub device_id: &'a str,
    pub text: &'a str,
    pub link: Option<&'a str>,
    pub has_audio: bool,
    pub image_count: usize,
    pub change_sequence: i64,
}

const REFERENCE_COLUMNS: &str = "t.vault_id, t.object_id, t.revision_id, t.note_id,
    t.captured_at_utc_ms, t.origin, t.device_id, t.link,
    EXISTS(SELECT 1 FROM scratch_phone_thought_deletions d
        WHERE d.vault_id = t.vault_id AND d.object_id = t.object_id), t.change_sequence";

fn read_reference(row: &rusqlite::Row<'_>) -> rusqlite::Result<PhoneThoughtReference> {
    Ok(PhoneThoughtReference {
        vault_id: row.get(0)?,
        object_id: row.get(1)?,
        revision_id: row.get(2)?,
        note_id: row.get(3)?,
        captured_at_utc_ms: row.get(4)?,
        origin: row.get(5)?,
        device_id: row.get(6)?,
        link: row.get(7)?,
        source_deleted: row.get(8)?,
        change_sequence: row.get(9)?,
    })
}

pub(crate) fn reference(
    history: &HistoryManager,
    vault_id: &str,
    object_id: &str,
    revision_id: Option<&str>,
) -> anyhow::Result<Option<PhoneThoughtReference>> {
    history.with_connection(|conn| {
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {REFERENCE_COLUMNS} FROM scratch_phone_thoughts t
                WHERE t.vault_id = ?1 AND t.object_id = ?2
                    AND (?3 IS NULL OR t.revision_id = ?3)
                ORDER BY t.change_sequence DESC, t.revision_id LIMIT 1"
                ),
                params![vault_id, object_id, revision_id],
                read_reference,
            )
            .optional()?)
    })
}

pub(crate) fn sources_for_note(
    history: &HistoryManager,
    note_id: &str,
) -> anyhow::Result<Vec<PhoneThoughtReference>> {
    history.with_connection(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {REFERENCE_COLUMNS} FROM scratch_phone_thoughts t
             WHERE t.note_id = ?1 ORDER BY t.captured_at_utc_ms, t.object_id"
        ))?;
        let references = statement
            .query_map(params![note_id], read_reference)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(references)
    })
}

/// The newest revision of every thought from this vault whose source the
/// phone has not deleted, oldest capture first.
pub(crate) fn current_references(
    history: &HistoryManager,
    vault_id: &str,
) -> anyhow::Result<Vec<PhoneThoughtReference>> {
    history.with_connection(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {REFERENCE_COLUMNS} FROM scratch_phone_thoughts t
             WHERE t.vault_id = ?1
                AND NOT EXISTS(SELECT 1 FROM scratch_phone_thought_deletions d
                    WHERE d.vault_id = t.vault_id AND d.object_id = t.object_id)
                AND t.change_sequence = (SELECT MAX(u.change_sequence)
                    FROM scratch_phone_thoughts u
                    WHERE u.vault_id = t.vault_id AND u.object_id = t.object_id)
             ORDER BY t.captured_at_utc_ms, t.object_id"
        ))?;
        let references = statement
            .query_map(params![vault_id], read_reference)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(references)
    })
}

pub(crate) fn is_deleted(
    history: &HistoryManager,
    vault_id: &str,
    object_id: &str,
) -> anyhow::Result<bool> {
    history.with_connection(|conn| {
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM scratch_phone_thought_deletions
            WHERE vault_id = ?1 AND object_id = ?2)",
            params![vault_id, object_id],
            |row| row.get(0),
        )?)
    })
}

pub(crate) fn import(
    app: &AppHandle,
    history: &HistoryManager,
    input: PhoneThoughtNote<'_>,
) -> anyhow::Result<Option<String>> {
    let changed = history.with_connection(|conn| {
        let transaction = conn.transaction()?;
        let deleted: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM scratch_phone_thought_deletions
                WHERE vault_id = ?1 AND object_id = ?2)",
            params![input.vault_id, input.object_id],
            |row| row.get(0),
        )?;
        if deleted {
            return Ok((None, false));
        }
        let existing: Option<String> = transaction
            .query_row(
                "SELECT note_id FROM scratch_phone_thoughts
             WHERE vault_id = ?1 AND object_id = ?2 AND revision_id = ?3",
                params![input.vault_id, input.object_id, input.revision_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(note_id) = existing {
            return Ok((Some(note_id), false));
        }
        let body = capture_body(input.text, input.link, input.has_audio, input.image_count);
        let note_id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO scratch_notes (id, title, body, pinned, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, 0, ?4, ?4)",
            params![note_id, derive_title(&body), body, input.captured_at_utc_ms],
        )?;
        record_version(
            &transaction,
            &note_id,
            &body,
            input.captured_at_utc_ms,
            ScratchVersionKind::Edit,
            super::Author::Me,
        )?;
        transaction.execute(
            "INSERT INTO scratch_phone_thoughts (vault_id, object_id, revision_id, note_id,
                captured_at_utc_ms, origin, device_id, link, change_sequence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                input.vault_id,
                input.object_id,
                input.revision_id,
                note_id,
                input.captured_at_utc_ms,
                input.origin,
                input.device_id,
                input.link,
                input.change_sequence
            ],
        )?;
        transaction.commit()?;
        Ok((Some(note_id), true))
    })?;
    if changed.1 {
        if let Some(note_id) = changed.0.as_deref() {
            emit_changed(app, note_id, false);
        }
    }
    Ok(changed.0)
}

pub(crate) fn mark_deleted(
    app: &AppHandle,
    history: &HistoryManager,
    vault_id: &str,
    object_id: &str,
    revision_id: &str,
    sequence: i64,
) -> anyhow::Result<()> {
    let note_ids = history.with_connection(|conn| {
        conn.execute(
            "INSERT INTO scratch_phone_thought_deletions (vault_id, object_id, revision_id, change_sequence)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(vault_id, object_id) DO UPDATE SET
                revision_id = excluded.revision_id, change_sequence = excluded.change_sequence
             WHERE excluded.change_sequence >= scratch_phone_thought_deletions.change_sequence",
            params![vault_id, object_id, revision_id, sequence],
        )?;
        let mut statement = conn.prepare(
            "SELECT t.note_id FROM scratch_phone_thoughts t JOIN scratch_notes n ON n.id = t.note_id
             WHERE t.vault_id = ?1 AND t.object_id = ?2"
        )?;
        let note_ids = statement.query_map(params![vault_id, object_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(note_ids)
    })?;
    for note_id in note_ids {
        emit_changed(app, &note_id, false);
    }
    Ok(())
}

pub(crate) fn backfill_complete(history: &HistoryManager, vault_id: &str) -> anyhow::Result<bool> {
    history.with_connection(|conn| {
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM scratch_phone_thought_backfill WHERE vault_id = ?1)",
            params![vault_id],
            |row| row.get(0),
        )?)
    })
}

pub(crate) fn finish_backfill(history: &HistoryManager, vault_id: &str) -> anyhow::Result<()> {
    history.with_connection(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO scratch_phone_thought_backfill (vault_id) VALUES (?1)",
            params![vault_id],
        )?;
        Ok(())
    })
}

pub(crate) fn capture_body(
    text: &str,
    link: Option<&str>,
    has_audio: bool,
    image_count: usize,
) -> String {
    let mut body = text.to_owned();
    if let Some(link) = link {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(link);
    }
    if body.trim().is_empty() {
        body = match (has_audio, image_count) {
            (true, 0) => "Voice thought".to_owned(),
            (true, count) => format!("Voice thought with {count} photos"),
            (false, 1) => "Photo thought".to_owned(),
            (false, count) if count > 1 => format!("Thought with {count} photos"),
            _ => "Phone thought".to_owned(),
        };
    }
    body
}

/// Keep attachment text in the imported note, but never replace words the
/// person has edited or a source the phone removed while the model ran.
pub(crate) fn enrich(
    app: &AppHandle,
    history: &HistoryManager,
    reference: &PhoneThoughtReference,
    expected_body: &str,
    text: &str,
    now_ms: i64,
) -> anyhow::Result<()> {
    let changed =
        history.with_connection(|conn| enrich_in(conn, reference, expected_body, text, now_ms))?;
    if changed {
        emit_changed(app, &reference.note_id, false);
    }
    Ok(())
}

fn enrich_in(
    conn: &mut rusqlite::Connection,
    reference: &PhoneThoughtReference,
    expected_body: &str,
    text: &str,
    now_ms: i64,
) -> anyhow::Result<bool> {
    let transaction = conn.transaction()?;
    let changed = transaction.execute(
        "UPDATE scratch_notes SET body = ?3, title = ?4, updated_at_ms = ?5
         WHERE id = ?1 AND body = ?2
            AND NOT EXISTS(SELECT 1 FROM scratch_phone_thought_deletions
                WHERE vault_id = ?6 AND object_id = ?7)",
        params![
            reference.note_id,
            expected_body,
            text,
            derive_title(text),
            now_ms,
            reference.vault_id,
            reference.object_id,
        ],
    )? > 0;
    if changed {
        record_version(
            &transaction,
            &reference.note_id,
            text,
            now_ms,
            ScratchVersionKind::Edit,
            super::Author::Me,
        )?;
    }
    transaction.commit()?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use rusqlite_migration::Migrations;

    fn fixture(body: &str) -> (Connection, PhoneThoughtReference) {
        let mut conn = Connection::open_in_memory().unwrap();
        Migrations::new(crate::managers::history::MIGRATIONS.to_vec())
            .to_latest(&mut conn)
            .unwrap();
        conn.execute(
            "INSERT INTO scratch_notes (id, title, body, pinned, created_at_ms, updated_at_ms)
             VALUES ('note', 'Voice thought', ?1, 0, 1, 1)",
            [body],
        )
        .unwrap();
        (
            conn,
            PhoneThoughtReference {
                vault_id: "vault".to_owned(),
                object_id: "thought".to_owned(),
                revision_id: "revision".to_owned(),
                note_id: "note".to_owned(),
                captured_at_utc_ms: 1,
                origin: "voice".to_owned(),
                device_id: "phone".to_owned(),
                link: None,
                source_deleted: false,
                change_sequence: 1,
            },
        )
    }

    fn body(conn: &Connection) -> String {
        conn.query_row(
            "SELECT body FROM scratch_notes WHERE id = 'note'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn attachment_text_replaces_the_untouched_import_and_is_saved_in_history() {
        let (mut conn, reference) = fixture("Voice thought");
        assert!(enrich_in(&mut conn, &reference, "Voice thought", "Plant tomatoes", 10).unwrap());
        assert_eq!(body(&conn), "Plant tomatoes");
        let saved: String = conn.query_row(
            "SELECT body FROM scratch_note_versions WHERE note_id = 'note' ORDER BY id DESC LIMIT 1",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(saved, "Plant tomatoes");
    }

    #[test]
    fn attachment_text_never_replaces_a_persons_edited_note() {
        let (mut conn, reference) = fixture("My edited plan");
        assert!(!enrich_in(&mut conn, &reference, "Voice thought", "Plant tomatoes", 10).unwrap());
        assert_eq!(body(&conn), "My edited plan");
    }

    #[test]
    fn attachment_text_never_restores_a_deleted_source() {
        let (mut conn, reference) = fixture("Voice thought");
        conn.execute(
            "INSERT INTO scratch_phone_thought_deletions VALUES ('vault', 'thought', 'revision', 2)",
            [],
        ).unwrap();
        assert!(!enrich_in(&mut conn, &reference, "Voice thought", "Plant tomatoes", 10).unwrap());
        assert_eq!(body(&conn), "Voice thought");
    }
}
