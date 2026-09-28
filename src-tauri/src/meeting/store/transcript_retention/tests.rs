use super::super::workflow_core_tests::{
    current_artifact, link, person, reviewable_meeting, store, transcript_segments,
};
use super::*;

const DAY: i64 = 86_400_000;
const NOW: i64 = 200 * DAY;

fn ended(store: &MeetingStore, at: Option<i64>, phase: &str) -> MeetingSessionId {
    let session_id = reviewable_meeting(store, "Notes to keep", 1);
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_sessions SET ended_at_utc_ms = ?1, phase = ?2 WHERE id = ?3",
            params![at, phase, id(session_id)],
        )
        .unwrap();
    session_id
}

fn policy(store: &MeetingStore, days: Option<u32>, now: i64) {
    let current = store.transcript_retention_policy().unwrap();
    let policy = days.map_or(MeetingRetentionPolicy::Forever, |days| {
        MeetingRetentionPolicy::DeleteAfterDays { days }
    });
    let receipt = store
        .set_transcript_retention_policy(MeetingOperationId::new(), now, current.revision, &policy)
        .unwrap();
    assert_eq!(receipt.result, OperationResult::Committed);
}

fn count(store: &MeetingStore, table: &str) -> u64 {
    store
        .connection()
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn cooldown_is_fenced_idempotent_and_only_restarts_when_shortened() {
    let (_dir, store) = store();
    let original = store.transcript_retention_policy().unwrap();
    assert_eq!(original.policy, MeetingRetentionPolicy::Forever);
    assert_eq!(original.deletion_begins_at_utc_ms, None);
    let operation = MeetingOperationId::new();
    let shorter = MeetingRetentionPolicy::DeleteAfterDays { days: 90 };
    let receipt = store
        .set_transcript_retention_policy(operation, NOW, 0, &shorter)
        .unwrap();
    assert_eq!(receipt.result, OperationResult::Committed);
    assert_eq!(
        store
            .transcript_retention_policy()
            .unwrap()
            .deletion_begins_at_utc_ms,
        Some(NOW + COOLDOWN_MS)
    );
    let repeated = store
        .set_transcript_retention_policy(operation, NOW + DAY, 0, &shorter)
        .unwrap();
    assert_eq!(repeated, receipt);
    let rejected = store
        .set_transcript_retention_policy(
            MeetingOperationId::new(),
            NOW + DAY,
            0,
            &MeetingRetentionPolicy::DeleteAfterDays { days: 1 },
        )
        .unwrap();
    assert_eq!(rejected.result, OperationResult::Rejected);
    assert_eq!(store.transcript_retention_policy().unwrap().revision, 1);
    policy(&store, Some(30), NOW + DAY);
    assert_eq!(
        store
            .transcript_retention_policy()
            .unwrap()
            .deletion_begins_at_utc_ms,
        Some(NOW + 8 * DAY)
    );
    policy(&store, Some(180), NOW + 2 * DAY);
    assert_eq!(
        store
            .transcript_retention_policy()
            .unwrap()
            .deletion_begins_at_utc_ms,
        Some(NOW + 8 * DAY)
    );
    policy(&store, None, NOW + 3 * DAY);
    assert_eq!(
        store
            .transcript_retention_policy()
            .unwrap()
            .deletion_begins_at_utc_ms,
        None
    );
    policy(&store, Some(365), NOW + 4 * DAY);
    assert_eq!(
        store
            .transcript_retention_policy()
            .unwrap()
            .deletion_begins_at_utc_ms,
        Some(NOW + 11 * DAY)
    );
    for days in [1, 7, 30, 90, 180, 365] {
        policy(&store, Some(days), NOW + 5 * DAY);
    }
    let snapshot = store.transcript_retention_policy().unwrap();
    for days in [0, 2, 31, u32::MAX] {
        assert_eq!(
            store.set_transcript_retention_policy(
                MeetingOperationId::new(),
                NOW,
                snapshot.revision,
                &MeetingRetentionPolicy::DeleteAfterDays { days }
            ),
            Err(StoreError::Invalid)
        );
    }
    assert_eq!(store.transcript_retention_policy().unwrap(), snapshot);
}

#[test]
fn age_uses_meeting_end_and_excludes_active_and_unfinished_meetings() {
    let (_dir, store) = store();
    let due = ended(&store, Some(NOW - 30 * DAY), "review_ready");
    let recovery = ended(&store, Some(NOW - 31 * DAY), "recovery_required");
    ended(&store, Some(NOW - 30 * DAY + 1), "review_ready");
    ended(&store, Some(NOW), "review_ready");
    ended(&store, None, "recovery_required");
    for phase in ["capturing_recording", "processing", "stopping", "deleting"] {
        ended(&store, Some(1), phase);
    }
    assert!(store
        .due_transcript_retention_sessions(NOW)
        .unwrap()
        .is_empty());
    policy(&store, Some(30), NOW - COOLDOWN_MS);
    assert!(store
        .due_transcript_retention_sessions(NOW - 1)
        .unwrap()
        .is_empty());
    assert_eq!(
        store.due_transcript_retention_sessions(NOW).unwrap(),
        vec![recovery, due]
    );
    policy(&store, None, NOW);
    assert!(store.purge_transcript_at(due, NOW).unwrap().is_none());
    assert!(store.require_retained_transcript(due).is_ok());
}

#[test]
fn finishing_notes_keeps_the_recording_end_as_the_retention_clock() {
    let (_dir, store) = store();
    let session_id = ended(&store, Some(123), "processing");
    store
        .transition(StoreTransition {
            operation_id: None,
            actor: OperationActor::System,
            command: MeetingCommandKind::Stop,
            requested_at_utc_ms: 456,
            session_id,
            expected_revision: 0,
            allowed_from: &[MeetingPhase::Processing],
            next_phase: MeetingPhase::ReviewReady,
            event_kind: "processing_finished",
            reason_codes: Vec::new(),
        })
        .unwrap();
    let end: i64 = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT ended_at_utc_ms FROM meeting_sessions WHERE id=?1",
            params![id(session_id)],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(end, 123);
}

#[test]
fn imported_recording_end_uses_measured_duration_without_changing_whole_meeting_deadline() {
    let (_dir, store) = store();
    let session_id = ended(&store, Some(NOW), "stopping");
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_sessions SET started_at_utc_ms = ?1, delete_after_utc_ms = ?2,
            origin_kind = ?3 WHERE id = ?4",
            params![
                DAY,
                NOW + 30 * DAY,
                encode_json(&MeetingOrigin::Import).unwrap(),
                id(session_id)
            ],
        )
        .unwrap();
    store.open_capture_window(session_id, 0).unwrap();
    store
        .seal_imported_capture_window(session_id, 90_000_000_000)
        .unwrap();
    let (end, deadline): (i64, i64) = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT ended_at_utc_ms, delete_after_utc_ms FROM meeting_sessions WHERE id = ?1",
            params![id(session_id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(end, DAY + 90_000);
    assert_eq!(deadline, NOW + 30 * DAY);
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_sessions SET phase = 'review_ready' WHERE id = ?1",
            params![id(session_id)],
        )
        .unwrap();
    policy(&store, Some(1), NOW - COOLDOWN_MS);
    assert_eq!(
        store.due_transcript_retention_sessions(NOW).unwrap(),
        vec![session_id]
    );
}

#[test]
fn migration_recovers_import_end_only_when_recorded_duration_is_known() {
    let migrations = Migrations::new(MIGRATIONS.to_vec());
    let mut probe = Connection::open_in_memory().unwrap();
    let retention_version = (1..=MIGRATIONS.len()).find(|&version| {
        migrations.to_version(&mut probe, version).unwrap();
        probe.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'meeting_transcript_retention_policy')",
            [], |row| row.get::<_, bool>(0),
        ).unwrap()
    }).unwrap();
    let mut connection = Connection::open_in_memory().unwrap();
    migrations
        .to_version(&mut connection, retention_version)
        .unwrap();
    let mut expected = Vec::new();
    for (origin, phase, duration, expected_end) in [
        (
            MeetingOrigin::Import,
            "review_ready",
            Some(90_000_000_000_i64),
            DAY + 90_000,
        ),
        (MeetingOrigin::Import, "recovery_required", Some(0), DAY),
        (MeetingOrigin::Import, "review_ready", None, NOW),
        (
            MeetingOrigin::Manual,
            "review_ready",
            Some(90_000_000_000),
            NOW,
        ),
        (
            MeetingOrigin::Import,
            "capturing_recording",
            Some(90_000_000_000),
            NOW,
        ),
    ] {
        let session_id = id(MeetingSessionId::new());
        connection.execute(
            "INSERT INTO meeting_sessions
                (id,phase,revision,title,origin_kind,preflight_json,created_at_utc_ms,
                 started_at_utc_ms,ended_at_utc_ms,processing_status,retention_policy_json,delete_after_utc_ms)
             VALUES (?1,?2,0,'Imported notes',?3,'{}',?4,?4,?5,
                '{\"kind\":\"succeeded\"}','{\"kind\":\"forever\"}',?6)",
            params![session_id, phase, encode_json(&origin).unwrap(), DAY, NOW, NOW + 30 * DAY],
        ).unwrap();
        connection
            .execute(
                "INSERT INTO meeting_capture_windows VALUES (?1,0,0,?2,?3)",
                params![session_id, duration, duration.map(|_| "stopped")],
            )
            .unwrap();
        connection.execute(
            "INSERT INTO meeting_user_notes (session_id,body,template_id,note_revision,updated_at_utc_ms)
             VALUES (?1,'Keep my notes','meeting-general',1,1)", params![session_id],
        ).unwrap();
        expected.push((session_id, expected_end));
    }
    migrations.to_latest(&mut connection).unwrap();
    for (session_id, expected_end) in expected {
        let retained: (i64, i64, String) = connection.query_row(
            "SELECT m.ended_at_utc_ms,m.delete_after_utc_ms,n.body
             FROM meeting_sessions m JOIN meeting_user_notes n ON n.session_id = m.id WHERE m.id = ?1",
            params![session_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(
            retained,
            (expected_end, NOW + 30 * DAY, "Keep my notes".to_owned())
        );
    }
}

#[test]
fn purge_keeps_notes_people_action_ticks_and_metrics_but_removes_all_transcript_revisions() {
    let (_dir, store) = store();
    let session_id = ended(&store, Some(1), "review_ready");
    transcript_segments(
        &store,
        session_id,
        &["privateutterance", "another private utterance"],
    );
    let content = serde_json::json!({
        "summary": {"text": "Keep this summary", "citations": []}, "outline": [], "decisions": [],
        "action_items": [{"text": {"text": "Send the draft", "citations": []}, "owner_text": null, "due_text": null}],
        "key_questions": [], "risks": [], "follow_up_draft": {"text": "Keep this follow-up", "citations": []}
    });
    current_artifact(&store, session_id, &content, 2);
    let person_id = person(&store, "Alex", &[], &[]);
    link(&store, session_id, person_id, "manual", "confirmed");
    let before = MeetingAnalytics {
        talk: super::super::super::analytics::talk_metrics(
            &store.analytics_segments(session_id).unwrap(),
        ),
        trackers: Vec::new(),
    };
    store
        .store_conversation_metrics(session_id, 0, &before)
        .unwrap();
    let inputs = store.semantic_index_inputs(session_id).unwrap().unwrap();
    store
        .replace_semantic_chunks(
            session_id,
            &inputs.key,
            "test-model",
            NOW,
            &[("privateutterance".to_owned(), vec![1])],
        )
        .unwrap();
    {
        let connection = store.connection().unwrap();
        let old_revision = id(TranscriptRevisionId::new());
        connection.execute("INSERT INTO meeting_transcript_revisions
            SELECT ?1,session_id,engine_id,model_version,destination_json,source_set_json,language,state,created_at_utc_ms,completed_at_utc_ms,error_code
            FROM meeting_transcript_revisions LIMIT 1", params![old_revision]).unwrap();
        connection.execute("INSERT INTO meeting_transcript_segments
            SELECT ?1,?2,track_id,0,start_offset_ns,end_offset_ns,speaker_id,'old private utterance',confidence_milli
            FROM meeting_transcript_segments LIMIT 1", params![id(TranscriptSegmentId::new()), old_revision]).unwrap();
        connection.execute("INSERT INTO meeting_segment_edits SELECT segment_id, 1, 'privatecorrection', 0, 1 FROM meeting_transcript_segments LIMIT 1", []).unwrap();
        connection.execute("INSERT INTO meeting_search_documents(session_id,entity_kind,entity_id,content) SELECT ?1,'segment',segment_id,base_text FROM meeting_transcript_segments", params![id(session_id)]).unwrap();
        connection
            .execute(
                "INSERT INTO meeting_notes VALUES (?1, ?2, NULL, NULL, 'Manual note', 1, 1, 1)",
                params![id(ManualNoteId::new()), id(session_id)],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO meeting_user_notes
            (session_id,body,template_id,note_revision,updated_at_utc_ms)
            VALUES (?1, 'My context', 'meeting-review', 1, 1)",
                params![id(session_id)],
            )
            .unwrap();
        connection.execute("INSERT INTO meeting_action_item_states SELECT artifact_id,0,session_id,1,1 FROM meeting_artifact_revisions", []).unwrap();
        connection.execute("INSERT INTO learning_observations VALUES ('vocabulary_correction','candidate','2026-09-20',1,1,'word','privateutterance',?1)", params![id(session_id)]).unwrap();
        connection.execute("INSERT INTO learning_suggestions VALUES ('vocabulary_correction','candidate','{}','privateutterance',1)", []).unwrap();
        connection.execute("INSERT INTO meeting_snapshots
            (snapshot_id,session_id,offset_ns,captured_at_utc_ms,width,height,capture_trigger,app_bundle_id,byte_length)
            VALUES (?1,?2,0,1,2,2,'manual',NULL,15)",
            params![Uuid::new_v4().to_string(), id(session_id)]).unwrap();
    }
    let audio = store.root.join(id(session_id));
    fs::create_dir_all(&audio).unwrap();
    fs::write(audio.join("record.smr"), b"encrypted audio record").unwrap();
    fs::create_dir_all(audio.join("snapshots")).unwrap();
    fs::write(audio.join("snapshots/preview.jpg"), b"captured screen").unwrap();
    policy(&store, Some(1), NOW - COOLDOWN_MS);
    assert!(store
        .purge_transcript_at(session_id, NOW)
        .unwrap()
        .is_some());
    assert!(!audio.exists());
    for table in [
        "meeting_transcript_segments",
        "meeting_segment_edits",
        "meeting_semantic_chunks",
        "meeting_semantic_index_state",
        "learning_observations",
        "learning_suggestions",
        "meeting_snapshots",
    ] {
        assert_eq!(count(&store, table), 0, "{table} retains private words");
    }
    let connection = store.connection().unwrap();
    let hits: u64 = connection.query_row("SELECT COUNT(*) FROM meeting_search_fts WHERE meeting_search_fts MATCH 'privateutterance'", [], |row| row.get(0)).unwrap();
    assert_eq!(hits, 0);
    let kept: String = connection
        .query_row(
            "SELECT content_json FROM meeting_artifact_revisions",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&kept).unwrap(),
        content
    );
    assert_eq!(
        connection
            .query_row("SELECT body FROM meeting_notes", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "Manual note"
    );
    assert_eq!(
        connection
            .query_row("SELECT body FROM meeting_user_notes", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "My context"
    );
    assert!(connection
        .query_row("SELECT done FROM meeting_action_item_states", [], |row| row
            .get::<_, bool>(0))
        .unwrap());
    drop(connection);
    assert_eq!(count(&store, "meeting_transcript_revisions"), 2);
    assert_eq!(count(&store, "meeting_artifact_revisions"), 1);
    assert_eq!(count(&store, "meeting_notes"), 1);
    assert_eq!(count(&store, "meeting_user_notes"), 1);
    assert_eq!(count(&store, "meeting_action_item_states"), 1);
    assert_eq!(count(&store, "meeting_person_links"), 1);
    assert_eq!(
        store.retained_conversation_metrics(session_id).unwrap(),
        Some(before.clone())
    );
    store
        .store_conversation_metrics(session_id, 999, &MeetingAnalytics::default())
        .unwrap();
    assert_eq!(
        store.retained_conversation_metrics(session_id).unwrap(),
        Some(before)
    );
    assert_eq!(
        store.replace_semantic_chunks(
            session_id,
            &inputs.key,
            "test-model",
            NOW,
            &[("privateutterance".to_owned(), vec![1])]
        ),
        Err(StoreError::Conflict)
    );
    assert_eq!(
        store.require_retained_transcript(session_id),
        Err(StoreError::TranscriptDeleted)
    );
    let snapshot = store.session_snapshot(session_id).unwrap();
    assert_eq!(snapshot.title, "Notes to keep");
    assert_eq!(snapshot.transcript_purged_at_utc_ms, Some(NOW));
    assert!(!snapshot
        .allowed_actions
        .contains(&AllowedMeetingAction::Regenerate));
    assert!(snapshot
        .allowed_actions
        .contains(&AllowedMeetingAction::Edit));
    assert!(store
        .purge_transcript_at(session_id, NOW + DAY)
        .unwrap()
        .is_none());
    assert_eq!(
        store.session_snapshot(session_id).unwrap().revision,
        snapshot.revision
    );
}

#[test]
fn failed_audio_cleanup_retries_from_stored_marker_even_if_policy_is_disabled() {
    let (_dir, store) = store();
    let session_id = ended(&store, Some(1), "review_ready");
    let path = store.root.join(id(session_id));
    // A non-directory at the recording path makes remove_dir_all fail deterministically.
    fs::write(&path, b"not a directory").unwrap();
    policy(&store, Some(1), NOW - COOLDOWN_MS);
    assert!(store.purge_transcript_at(session_id, NOW).is_err());
    assert_eq!(
        store.require_retained_transcript(session_id),
        Err(StoreError::TranscriptDeleted)
    );
    let revision = store.session_snapshot(session_id).unwrap().revision;
    policy(&store, None, NOW);
    assert_eq!(
        store.due_transcript_retention_sessions(NOW).unwrap(),
        vec![session_id]
    );
    fs::remove_file(&path).unwrap();
    assert!(store
        .purge_transcript_at(session_id, NOW + DAY)
        .unwrap()
        .is_some());
    assert!(store
        .due_transcript_retention_sessions(NOW + DAY)
        .unwrap()
        .is_empty());
    assert_eq!(
        store.session_snapshot(session_id).unwrap().revision,
        revision
    );
}

#[test]
fn purge_removes_local_cloud_copies_and_queues_notes_only_even_while_sync_is_paused() {
    let (_dir, store) = store();
    let session_id = ended(&store, Some(1), "review_ready");
    let object_id = "retentionobject000001";
    store
        .upsert_cloud_head(&CloudHead {
            object_id: object_id.to_owned(),
            source_session_id: Some(session_id),
            remote_revision_id: Some("retentionrevision001".to_owned()),
            tombstone: false,
            acknowledged_revision_id: Some("retentionrevision001".to_owned()),
            change_sequence: 1,
        })
        .unwrap();
    let old = store
        .enqueue_cloud_outbox(CloudOutboxInput {
            kind: CloudOutboxKind::Object,
            object_id: object_id.to_owned(),
            source_session_id: Some(session_id),
            source_revision: Some(0),
            base_remote_revision_id: Some("retentionrevision001".to_owned()),
            share_content_kind: None,
            remote_revision_id: Some("retentionrevision002".to_owned()),
            idempotency_key: "retentionintent000001".to_owned(),
            next_attempt_utc_ms: 1,
        })
        .unwrap();
    let outbox = store.root.join(&old.payload_relative_dir);
    store
        .stage_cloud_outbox_chunks::<StoreError>(&old.outbox_id, |directory| {
            assert!(
                matches!(
                    store.connection.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                ),
                "retention must not interleave with payload writes"
            );
            fs::write(directory.join("chunk"), b"encrypted transcript")?;
            Ok(vec![CloudOutboxChunk {
                chunk_index: 0,
                size_bytes: 20,
                sha256: "digest".to_owned(),
                accepted: false,
            }])
        })
        .unwrap();
    let inbox = store.cloud_recording_staging_path(object_id).unwrap();
    fs::write(&inbox, b"decrypted recording").unwrap();
    let conflict_path = store
        .root
        .join(format!(".cloud-conflicts/{object_id}.bundle"));
    ensure_private_directory(conflict_path.parent().unwrap()).unwrap();
    fs::write(&conflict_path, b"old transcript bundle").unwrap();
    store.connection().unwrap().execute(
        "INSERT INTO meeting_cloud_conflicts
         (object_id,source_session_id,source_revision,remote_revision_id,remote_sequence,remote_bundle_relative_path,detected_at_utc_ms)
         VALUES (?1,?2,0,'retentionrevision003',1,?3,1)",
        params![object_id, id(session_id), format!(".cloud-conflicts/{object_id}.bundle")],
    ).unwrap();
    store.connection().unwrap().execute(
        "INSERT INTO meeting_cloud_state VALUES (1,'retentionvault000001','retentiondevice0001','https://example.invalid',NULL,NULL,0,1,1)", []
    ).unwrap();
    policy(&store, Some(1), NOW - COOLDOWN_MS);
    assert!(store
        .purge_transcript_at(session_id, NOW)
        .unwrap()
        .is_some());
    assert!(!outbox.exists());
    assert!(!inbox.exists());
    assert!(!conflict_path.exists());
    assert!(store.cloud_conflict(object_id).unwrap().is_none());
    assert_eq!(
        store.cloud_outbox(&old.outbox_id).unwrap().unwrap().state,
        CloudOutboxState::Cancelled
    );
    let queued = store.cloud_outboxes_for_session(session_id).unwrap();
    let replacement: Vec<_> = queued
        .iter()
        .filter(|row| row.state == CloudOutboxState::Pending)
        .collect();
    assert_eq!(replacement.len(), 1);
    assert_eq!(
        replacement[0].source_revision,
        Some(store.session_snapshot(session_id).unwrap().revision)
    );
    assert_eq!(replacement[0].object_id, object_id);
    assert_eq!(
        store.cloud_recording_staging_path(object_id),
        Err(StoreError::TranscriptDeleted)
    );
    let late = store.stage_cloud_outbox_chunks::<StoreError>(&old.outbox_id, |directory| {
        fs::write(directory.join("chunk"), b"late encrypted transcript")?;
        Ok(Vec::new())
    });
    assert_eq!(late, Err(StoreError::Conflict));
    assert!(!outbox.exists());
}
