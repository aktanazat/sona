use super::note_evidence::{NoteEvidenceError, NoteEvidenceInput, NoteEvidenceRemoval};
use super::workflow_core_tests::{reviewable_meeting, store, transcript};
use super::*;
use crate::managers::history::MIGRATIONS as HISTORY_MIGRATIONS;
use crate::scratchpad::links::{insert_source, ResolvedSource, ScratchSource, ScratchSourceKind};
use crate::scratchpad::{create_note_with_connection, note_with_connection};

const DAY_MS: i64 = 86_400_000;
const PURGE_AT: i64 = 20 * DAY_MS;
const NOTE_BODY: &str = "Keep the derived decisions, not a second transcript.";

fn history_at(version: usize) -> Connection {
    let mut history = Connection::open_in_memory().expect("open fresh history");
    history
        .pragma_update(None, "foreign_keys", true)
        .expect("enable history foreign keys");
    Migrations::new(HISTORY_MIGRATIONS.to_vec())
        .to_version(&mut history, version)
        .expect("apply the real history migrations");
    history
}

fn conversation_source(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    text: &str,
) -> ResolvedSource {
    let session = store
        .session_snapshot(session_id)
        .expect("read fixture session");
    ResolvedSource {
        source: ScratchSource {
            kind: ScratchSourceKind::Meeting,
            id: id(session_id),
            title: session.title,
            link: format!("sona://meeting/{}", id(session_id)),
            revision: Some(i64::try_from(session.revision).expect("fixture revision fits")),
            available: true,
        },
        evidence: Some(text.to_string()),
    }
}

fn input(session_id: MeetingSessionId, source: &ResolvedSource) -> NoteEvidenceInput<'_> {
    NoteEvidenceInput {
        session_id,
        input_revision: u64::try_from(source.source.revision.expect("fixture source revision"))
            .expect("fixture revision is nonnegative"),
        title: &source.source.title,
        text: source.evidence.as_deref().expect("fixture source text"),
    }
}

fn persist_note(
    history: &mut Connection,
    note_id: Uuid,
    source: &ResolvedSource,
) -> rusqlite::Result<()> {
    let transaction = history.transaction()?;
    transaction.execute(
        "INSERT INTO scratch_notes (id, title, body, pinned, created_at_ms, updated_at_ms)
         VALUES (?1, 'Derived decisions', ?2, 0, 1, 1)",
        params![note_id.to_string(), NOTE_BODY],
    )?;
    insert_source(&transaction, &note_id.to_string(), source)?;
    transaction.commit()
}

fn stored_evidence(
    store: &MeetingStore,
    note_id: Uuid,
    session_id: MeetingSessionId,
) -> Option<(u64, String, String)> {
    store
        .connection()
        .expect("open fixture meeting connection")
        .query_row(
            "SELECT input_revision, title, text FROM meeting_note_evidence
             WHERE note_id = ?1 AND session_id = ?2",
            params![note_id.to_string(), id(session_id)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .expect("inspect the stored evidence, not a filtered reader")
}

fn stored_history_source(
    history: &Connection,
    note_id: &str,
    kind: &str,
) -> (String, Option<i64>, Option<String>) {
    history
        .query_row(
            "SELECT title, revision, evidence FROM scratch_note_sources
             WHERE note_id = ?1 AND kind = ?2",
            params![note_id, kind],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("inspect persisted source fields")
}

#[test]
fn captured_conversation_stays_frozen_until_retention_erases_it_without_erasing_the_note() {
    let (_directory, store) = store();
    let session_id = reviewable_meeting(&store, "Original private title", 1);
    transcript(&store, session_id, "Original private words");
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_sessions SET ended_at_utc_ms = 2 WHERE id = ?1",
            params![id(session_id)],
        )
        .unwrap();
    let mut history = history_at(HISTORY_MIGRATIONS.len());
    let note_id = Uuid::new_v4();
    let source = conversation_source(&store, session_id, "Original private words");
    store
        .capture_note_evidence(note_id, &[input(session_id, &source)], || {
            persist_note(&mut history, note_id, &source)
        })
        .expect("capture evidence with the history note");
    assert_eq!(
        stored_history_source(&history, &note_id.to_string(), "meeting"),
        ("Conversation".to_string(), None, None),
        "history must never hold a conversation snapshot"
    );

    let renamed = store
        .set_title(
            MeetingOperationId::new(),
            3,
            session_id,
            0,
            "Edited private title".to_string(),
        )
        .unwrap();
    assert_eq!(renamed.result, OperationResult::Committed);
    let segment_id = store.review_snapshot(session_id).unwrap().transcript[0]
        .base
        .segment_id;
    let edited = store
        .edit_segment(SegmentEdit {
            mutation: StoreMutation {
                operation_id: MeetingOperationId::new(),
                actor: OperationActor::User,
                requested_at_utc_ms: 4,
                session_id,
                expected_revision: 1,
                command: MeetingCommandKind::SegmentEdit,
            },
            segment_id,
            replacement_text: "Edited private words".to_string(),
            removed: false,
        })
        .unwrap();
    assert_eq!(edited.result, OperationResult::Committed);
    let current = store.review_snapshot(session_id).unwrap();
    assert_eq!(current.session.title, "Edited private title");
    assert_eq!(current.session.revision, 2);
    assert_eq!(
        current.transcript[0].replacement_text.as_deref(),
        Some("Edited private words")
    );
    let frozen = store
        .captured_note_evidence(note_id, session_id)
        .unwrap()
        .expect("captured conversation remains available before purge");
    assert_eq!(frozen.source.session_id, session_id);
    assert_eq!(frozen.source.input_revision, 0);
    assert_eq!(frozen.source.title, "Original private title");
    assert_eq!(frozen.text, "Original private words");
    assert_eq!(
        stored_evidence(&store, note_id, session_id),
        Some((
            0,
            "Original private title".to_string(),
            "Original private words".to_string()
        ))
    );

    let policy = store.transcript_retention_policy().unwrap();
    let changed = store
        .set_transcript_retention_policy(
            MeetingOperationId::new(),
            PURGE_AT - 7 * DAY_MS,
            policy.revision,
            &MeetingRetentionPolicy::DeleteAfterDays { days: 1 },
        )
        .unwrap();
    assert_eq!(changed.result, OperationResult::Committed);
    assert!(store
        .purge_transcript_at(session_id, PURGE_AT)
        .unwrap()
        .is_some());
    assert_eq!(
        store
            .session_snapshot(session_id)
            .unwrap()
            .transcript_purged_at_utc_ms,
        Some(PURGE_AT)
    );
    assert_eq!(stored_evidence(&store, note_id, session_id), None);
    assert_eq!(
        stored_history_source(&history, &note_id.to_string(), "meeting"),
        ("Conversation".to_string(), None, None)
    );
    assert_eq!(
        note_with_connection(&history, &note_id.to_string())
            .unwrap()
            .unwrap()
            .body,
        NOTE_BODY
    );

    let late_note = Uuid::new_v4();
    let late_source = conversation_source(&store, session_id, "Original private words");
    let late = store.capture_note_evidence(late_note, &[input(session_id, &late_source)], || {
        persist_note(&mut history, late_note, &late_source)
    });
    assert!(matches!(
        late,
        Err(NoteEvidenceError::Store(StoreError::TranscriptDeleted))
    ));
    assert!(note_with_connection(&history, &late_note.to_string())
        .unwrap()
        .is_none());
    assert_eq!(stored_evidence(&store, late_note, session_id), None);
    let bypass = store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_note_evidence (note_id, session_id, input_revision, title, text)
         VALUES (?1, ?2, ?3, 'Original private title', 'Original private words')",
            params![
                late_note.to_string(),
                id(session_id),
                late_source.source.revision
            ],
        )
        .expect_err("SQL must also reject a late copy at the current revision");
    assert_eq!(
        bypass.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    );
    assert_eq!(stored_evidence(&store, late_note, session_id), None);
}

#[test]
fn starting_conversation_deletion_erases_captures_and_rejects_a_late_note() {
    let (_directory, store) = store();
    let session_id = reviewable_meeting(&store, "Private conversation", 1);
    let mut history = history_at(HISTORY_MIGRATIONS.len());
    let note_id = Uuid::new_v4();
    let source = conversation_source(&store, session_id, "Private source words");
    store
        .capture_note_evidence(note_id, &[input(session_id, &source)], || {
            persist_note(&mut history, note_id, &source)
        })
        .unwrap();
    assert!(stored_evidence(&store, note_id, session_id).is_some());
    let (receipt, _job) = store
        .reserve_deletion(
            MeetingOperationId::new(),
            2,
            session_id,
            0,
            DeletionCause::Discard,
        )
        .unwrap();
    assert_eq!(receipt.result, OperationResult::Committed);
    assert_eq!(
        store.session_snapshot(session_id).unwrap().phase,
        MeetingPhase::Deleting
    );
    assert_eq!(stored_evidence(&store, note_id, session_id), None);

    let late_note = Uuid::new_v4();
    let late_source = conversation_source(&store, session_id, "Private source words");
    let late = store.capture_note_evidence(late_note, &[input(session_id, &late_source)], || {
        persist_note(&mut history, late_note, &late_source)
    });
    assert!(matches!(
        late,
        Err(NoteEvidenceError::Store(StoreError::NotFound))
    ));
    assert!(note_with_connection(&history, &late_note.to_string())
        .unwrap()
        .is_none());
    assert_eq!(stored_evidence(&store, late_note, session_id), None);
    let bypass = store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_note_evidence (note_id, session_id, input_revision, title, text)
             VALUES (?1, ?2, ?3, 'Private conversation', 'Private source words')",
            params![
                late_note.to_string(),
                id(session_id),
                late_source.source.revision
            ],
        )
        .expect_err("SQL must reject a copy while deletion is pending");
    assert_eq!(
        bypass.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    );
    assert_eq!(stored_evidence(&store, late_note, session_id), None);
    assert_eq!(
        stored_history_source(&history, &note_id.to_string(), "meeting"),
        ("Conversation".to_string(), None, None)
    );
    assert_eq!(
        note_with_connection(&history, &note_id.to_string())
            .unwrap()
            .unwrap()
            .body,
        NOTE_BODY
    );
}

#[test]
fn a_failed_history_transaction_rolls_back_the_conversation_capture_and_the_note() {
    let (_directory, store) = store();
    let session_id = reviewable_meeting(&store, "Private conversation", 1);
    let mut history = history_at(HISTORY_MIGRATIONS.len());
    history
        .execute_batch(
            "CREATE TEMP TRIGGER refuse_source_insert BEFORE INSERT ON scratch_note_sources
         BEGIN SELECT RAISE(ABORT, 'history source write refused'); END;",
        )
        .unwrap();
    let note_id = Uuid::new_v4();
    let source = conversation_source(&store, session_id, "Private source words");
    let failed = store.capture_note_evidence(note_id, &[input(session_id, &source)], || {
        persist_note(&mut history, note_id, &source)
    });
    let error = failed.expect_err("real history constraint must abort the save");
    assert!(
        matches!(&error, NoteEvidenceError::Persist(rusqlite::Error::SqliteFailure(code, message))
        if code.code == rusqlite::ErrorCode::ConstraintViolation
            && message.as_deref() == Some("history source write refused"))
    );
    assert_eq!(stored_evidence(&store, note_id, session_id), None);
    assert!(note_with_connection(&history, &note_id.to_string())
        .unwrap()
        .is_none());
    let sources: u64 = history
        .query_row("SELECT COUNT(*) FROM scratch_note_sources", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(sources, 0);

    history
        .execute_batch("DROP TRIGGER refuse_source_insert")
        .unwrap();
    store
        .capture_note_evidence(note_id, &[input(session_id, &source)], || {
            persist_note(&mut history, note_id, &source)
        })
        .expect("the same note and source can be saved after storage recovers");
    assert!(stored_evidence(&store, note_id, session_id).is_some());
    assert_eq!(
        note_with_connection(&history, &note_id.to_string())
            .unwrap()
            .unwrap()
            .body,
        NOTE_BODY
    );
}

#[test]
fn a_failed_unlink_still_durably_erases_the_captured_conversation() {
    let (_directory, store) = store();
    let session_id = reviewable_meeting(&store, "Private conversation", 1);
    let mut history = history_at(HISTORY_MIGRATIONS.len());
    let note_id = Uuid::new_v4();
    let source = conversation_source(&store, session_id, "Private source words");
    store
        .capture_note_evidence(note_id, &[input(session_id, &source)], || {
            persist_note(&mut history, note_id, &source)
        })
        .unwrap();
    assert!(stored_evidence(&store, note_id, session_id).is_some());
    history
        .execute_batch(
            "CREATE TEMP TRIGGER refuse_source_delete BEFORE DELETE ON scratch_note_sources
         BEGIN SELECT RAISE(ABORT, 'history source removal refused'); END;",
        )
        .unwrap();
    let failed = store.remove_note_evidence(
        NoteEvidenceRemoval::Source { note_id, session_id },
        || history.execute(
            "DELETE FROM scratch_note_sources WHERE note_id = ?1 AND kind = 'meeting' AND target_id = ?2",
            params![note_id.to_string(), id(session_id)],
        ),
    );
    let error = failed.expect_err("real history constraint must abort unlinking");
    assert!(
        matches!(&error, NoteEvidenceError::Persist(rusqlite::Error::SqliteFailure(code, message))
        if code.code == rusqlite::ErrorCode::ConstraintViolation
            && message.as_deref() == Some("history source removal refused"))
    );
    assert_eq!(stored_evidence(&store, note_id, session_id), None);
    assert!(store
        .captured_note_evidence(note_id, session_id)
        .unwrap()
        .is_none());
    assert_eq!(
        stored_history_source(&history, &note_id.to_string(), "meeting"),
        ("Conversation".to_string(), None, None),
        "only the content-free history reference may survive a failed unlink"
    );
    assert_eq!(
        note_with_connection(&history, &note_id.to_string())
            .unwrap()
            .unwrap()
            .body,
        NOTE_BODY
    );
}

#[test]
fn history_upgrade_scrubs_conversation_snapshots_and_blocks_new_copies_but_keeps_note_snapshots() {
    let migrations = Migrations::new(HISTORY_MIGRATIONS.to_vec());
    let mut probe = Connection::open_in_memory().unwrap();
    let links_version = (1..=HISTORY_MIGRATIONS.len())
        .find(|&version| {
            migrations.to_version(&mut probe, version).unwrap();
            probe
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master
                 WHERE type = 'table' AND name = 'scratch_note_sources')",
                    [],
                    |row| row.get::<_, bool>(0),
                )
                .unwrap()
        })
        .expect("history has the original note-source migration");
    let mut history = history_at(links_version);
    let note = create_note_with_connection(&mut history, NOTE_BODY, 1).unwrap();
    let original_note =
        create_note_with_connection(&mut history, "Original local note words", 2).unwrap();
    let meeting_id = Uuid::new_v4().to_string();
    history
        .execute(
            "INSERT INTO scratch_note_sources (note_id, kind, target_id, title, revision, evidence)
         VALUES (?1, 'meeting', ?2, 'Old private title', 7, 'Old private conversation words')",
            params![note.id, meeting_id],
        )
        .unwrap();
    insert_source(
        &history,
        &note.id,
        &ResolvedSource {
            source: ScratchSource {
                kind: ScratchSourceKind::Note,
                id: original_note.id.clone(),
                title: original_note.title.clone(),
                link: format!("sona://note/{}", original_note.id),
                revision: Some(original_note.updated_at_ms),
                available: true,
            },
            evidence: Some(original_note.body.clone()),
        },
    )
    .unwrap();
    assert_eq!(
        stored_history_source(&history, &note.id, "meeting"),
        (
            "Old private title".to_string(),
            Some(7),
            Some("Old private conversation words".to_string())
        )
    );

    migrations.to_latest(&mut history).unwrap();
    assert_eq!(
        stored_history_source(&history, &note.id, "meeting"),
        ("Conversation".to_string(), None, None)
    );
    assert_eq!(
        stored_history_source(&history, &note.id, "note"),
        (
            original_note.title.clone(),
            Some(2),
            Some(original_note.body.clone())
        )
    );
    assert_eq!(
        note_with_connection(&history, &note.id)
            .unwrap()
            .unwrap()
            .body,
        NOTE_BODY
    );

    let insert = history
        .execute(
            "INSERT INTO scratch_note_sources (note_id, kind, target_id, title, revision, evidence)
         VALUES (?1, 'meeting', ?2, 'Conversation', NULL, 'Late private conversation words')",
            params![note.id, Uuid::new_v4().to_string()],
        )
        .expect_err("history must reject a new conversation snapshot");
    assert_eq!(
        insert.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    );
    let update = history
        .execute(
            "UPDATE scratch_note_sources SET title = 'Late private title'
         WHERE note_id = ?1 AND kind = 'meeting'",
            params![note.id],
        )
        .expect_err("history must reject restoring a scrubbed conversation snapshot");
    assert_eq!(
        update.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    );
    assert_eq!(
        stored_history_source(&history, &note.id, "meeting"),
        ("Conversation".to_string(), None, None)
    );
    crate::scratchpad::update_body_with_connection(
        &mut history,
        &original_note.id,
        "Changed local note words",
        3,
    )
    .unwrap();
    assert_eq!(
        stored_history_source(&history, &note.id, "note"),
        (original_note.title, Some(2), Some(original_note.body)),
        "local note evidence keeps its captured version after the source is edited"
    );
}
