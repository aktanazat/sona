use super::transcript_retention::{
    part_cutoff, part_retention_policy_in, RetainedPart, SWEEP_LIMIT,
};
use super::*;

impl MeetingStore {
    pub(crate) fn audio_retention_policy(
        &self,
    ) -> Result<MeetingPartRetentionSnapshot, StoreError> {
        let connection = self.connection()?;
        part_retention_policy_in(&connection, RetainedPart::Audio)
    }

    pub(crate) fn set_audio_retention_policy(
        &self,
        operation_id: MeetingOperationId,
        now_utc_ms: i64,
        expected_revision: u64,
        policy: &MeetingRetentionPolicy,
    ) -> Result<OperationReceipt, StoreError> {
        self.set_part_retention_policy(
            RetainedPart::Audio,
            operation_id,
            now_utc_ms,
            expected_revision,
            policy,
        )
    }

    /// Finished meetings with recorded audio past the policy, after any purge
    /// a crash left half done. A meeting still capturing, processing or
    /// waiting for recovery keeps its audio, as does one whose words were
    /// never written and one this Mac has held for less than the policy,
    /// such as an old recording imported today.
    pub(crate) fn due_audio_retention_sessions(
        &self,
        now_utc_ms: i64,
    ) -> Result<Vec<MeetingSessionId>, StoreError> {
        let connection = self.connection()?;
        let policy = part_retention_policy_in(&connection, RetainedPart::Audio)?;
        let mut statement = connection.prepare(
            "SELECT id FROM meeting_sessions
             WHERE phase = 'review_ready'
               AND (audio_purge_pending = 1
                    OR (audio_purged_at_utc_ms IS NULL AND transcript_purged_at_utc_ms IS NULL
                        AND current_transcript_revision_id IS NOT NULL
                        AND ended_at_utc_ms <= ?1 AND created_at_utc_ms <= ?1
                        AND EXISTS (SELECT 1 FROM meeting_source_tracks t
                                    JOIN meeting_track_records r ON r.track_id = t.track_id
                                    WHERE t.session_id = meeting_sessions.id)))
             ORDER BY audio_purge_pending DESC, ended_at_utc_ms, id LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![part_cutoff(&policy, now_utc_ms), SWEEP_LIMIT],
            |row| {
                let value: String = row.get(0)?;
                parse_uuid(&value)
                    .map(MeetingSessionId::from_uuid)
                    .map_err(to_sql_error)
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Deletes one finished meeting's recorded audio and keeps everything
    /// else. The marker commits first and is irreversible; the files go after
    /// it, and a crash between the two leaves the purge pending for the next
    /// sweep. Returns whether this call purged anything.
    pub(crate) fn purge_audio_at(
        &self,
        session_id: MeetingSessionId,
        now_utc_ms: i64,
    ) -> Result<bool, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = session_row(&transaction, session_id)?;
        if current.phase != MeetingPhase::ReviewReady {
            return Ok(false);
        }
        let (ended_at, created_at, transcribed, pending, recorded): (
            Option<i64>,
            i64,
            bool,
            bool,
            bool,
        ) = transaction.query_row(
            "SELECT ended_at_utc_ms, created_at_utc_ms,
                    current_transcript_revision_id IS NOT NULL, audio_purge_pending,
                    EXISTS (SELECT 1 FROM meeting_source_tracks t
                            JOIN meeting_track_records r ON r.track_id = t.track_id
                            WHERE t.session_id = ?1)
             FROM meeting_sessions WHERE id = ?1",
            params![id(session_id)],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        if !pending {
            let policy = part_retention_policy_in(&transaction, RetainedPart::Audio)?;
            let due = recorded
                && transcribed
                && current.transcript_purged_at_utc_ms.is_none()
                && current.audio_purged_at_utc_ms.is_none()
                && ended_at
                    .zip(part_cutoff(&policy, now_utc_ms))
                    .is_some_and(|(ended, cutoff)| ended <= cutoff && created_at <= cutoff);
            if !due {
                return Ok(false);
            }
            let next = current.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
            transaction.execute(
                "UPDATE meeting_sessions SET audio_purged_at_utc_ms = ?1,
                    audio_purge_pending = 1, revision = ?2 WHERE id = ?3",
                params![now_utc_ms, to_i64(next)?, id(session_id)],
            )?;
            append_event(
                &transaction,
                session_id,
                next,
                current.phase,
                current.phase,
                "audio_purged",
                None,
            )?;
        }
        let staged = {
            let mut statement = transaction.prepare(
                "SELECT object_id FROM meeting_cloud_heads WHERE source_session_id = ?1",
            )?;
            let rows =
                statement.query_map(params![id(session_id)], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        transaction.commit()?;
        drop(connection);
        let session = validated_relative(&self.root, &id(session_id))?;
        ignore_missing(fs::remove_dir_all(session.join("tracks")))?;
        for object_id in staged {
            let path = validated_relative(&self.root, &format!(".cloud-inbox/{object_id}.wav"))?;
            ignore_missing(fs::remove_file(path))?;
        }
        self.connection()?.execute(
            "UPDATE meeting_sessions SET audio_purge_pending = 0 WHERE id = ?1",
            params![id(session_id)],
        )?;
        Ok(true)
    }
}

/// Refuses a meeting's audio once a retention policy deleted it. A deleted
/// transcript took the recording with it, and says so.
pub(super) fn require_retained_audio_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<(), StoreError> {
    let session = session_row(connection, session_id)?;
    if session.transcript_purged_at_utc_ms.is_some() {
        return Err(StoreError::TranscriptDeleted);
    }
    if session.audio_purged_at_utc_ms.is_some() {
        return Err(StoreError::NotFound);
    }
    Ok(())
}

/// How many meetings still holding a transcript or recorded audio will be
/// past a retention cutoff at `at_utc_ms`: the meetings the sweeps will have
/// taken something from by then, each counted once.
pub(super) fn meetings_purging_by_in(
    connection: &Connection,
    at_utc_ms: i64,
) -> Result<u64, StoreError> {
    let transcript = part_retention_policy_in(connection, RetainedPart::Transcript)?;
    let audio = part_retention_policy_in(connection, RetainedPart::Audio)?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM meeting_sessions
         WHERE transcript_purged_at_utc_ms IS NULL
           AND ((phase IN ('review_ready', 'recovery_required') AND ended_at_utc_ms <= ?1)
                OR (phase = 'review_ready' AND audio_purged_at_utc_ms IS NULL
                    AND current_transcript_revision_id IS NOT NULL
                    AND ended_at_utc_ms <= ?2 AND created_at_utc_ms <= ?2
                    AND EXISTS (SELECT 1 FROM meeting_source_tracks t
                                JOIN meeting_track_records r ON r.track_id = t.track_id
                                WHERE t.session_id = meeting_sessions.id)))",
        params![
            part_cutoff(&transcript, at_utc_ms),
            part_cutoff(&audio, at_utc_ms)
        ],
        |row| row.get(0),
    )?;
    from_i64(count)
}

fn ignore_missing(result: std::io::Result<()>) -> Result<(), StoreError> {
    match result {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other.map_err(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use super::super::transcript_corrections::audio_retained_in;
    use super::super::workflow_core_tests::{reviewable_meeting, store, transcript_segments};
    use super::*;

    const DAY: i64 = 86_400_000;
    const NOW: i64 = 200 * DAY;
    const THIRTY_DAYS: &str = r#"{"kind":"delete_after_days","days":30}"#;
    const FOREVER: &str = r#"{"kind":"forever"}"#;

    fn ended(store: &MeetingStore, at: Option<i64>, phase: &str) -> MeetingSessionId {
        let session_id = reviewable_meeting(store, "Weekly sync", 1);
        transcript_segments(store, session_id, &["ship the draft on friday"]);
        let connection = store.connection().unwrap();
        connection
            .execute(
                "INSERT INTO meeting_track_records (
                    track_id, source_sequence, source_epoch, start_offset_ns, duration_ns,
                    frame_count, record_offset_bytes, record_bytes, durable_at_utc_ms
                 ) SELECT track_id, 0, 0, 0, 1000000000, 1, 0, 1, 1
                   FROM meeting_source_tracks WHERE session_id = ?1",
                params![id(session_id)],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE meeting_sessions SET ended_at_utc_ms = ?1, phase = ?2 WHERE id = ?3",
                params![at, phase, id(session_id)],
            )
            .unwrap();
        session_id
    }

    fn audio_policy(store: &MeetingStore, days: Option<u32>, now: i64) {
        let current = store.audio_retention_policy().unwrap();
        let policy = days.map_or(MeetingRetentionPolicy::Forever, |days| {
            MeetingRetentionPolicy::DeleteAfterDays { days }
        });
        let receipt = store
            .set_audio_retention_policy(MeetingOperationId::new(), now, current.revision, &policy)
            .unwrap();
        assert_eq!(receipt.result, OperationResult::Committed);
    }

    /// A store migrated up to the step before audio retention, holding one
    /// finished meeting under `session_policy`.
    fn store_before_audio_retention(
        session_policy: &str,
    ) -> (Migrations<'static>, Connection, String) {
        let migrations = Migrations::new(MIGRATIONS.to_vec());
        let mut probe = Connection::open_in_memory().unwrap();
        let audio_version = (1..=MIGRATIONS.len())
            .find(|&version| {
                migrations.to_version(&mut probe, version).unwrap();
                probe
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master
                     WHERE name = 'meeting_audio_retention_policy')",
                        [],
                        |row| row.get::<_, bool>(0),
                    )
                    .unwrap()
            })
            .unwrap();
        let mut connection = Connection::open_in_memory().unwrap();
        migrations
            .to_version(&mut connection, audio_version - 1)
            .unwrap();
        let session_id = id(MeetingSessionId::new());
        connection
            .execute(
                "INSERT INTO meeting_sessions
                (id,phase,revision,title,origin_kind,preflight_json,created_at_utc_ms,
                 started_at_utc_ms,ended_at_utc_ms,processing_status,retention_policy_json,
                 delete_after_utc_ms)
             VALUES (?1,'review_ready',0,'Weekly sync',?2,'{}',?3,?3,?3,
                '{\"kind\":\"succeeded\"}',?4,?5)",
                params![
                    session_id,
                    encode_json(&MeetingOrigin::Manual).unwrap(),
                    DAY,
                    session_policy,
                    31 * DAY
                ],
            )
            .unwrap();
        (migrations, connection, session_id)
    }

    fn session_retention(connection: &Connection, session_id: &str) -> (String, Option<i64>) {
        connection
        .query_row(
            "SELECT retention_policy_json, delete_after_utc_ms FROM meeting_sessions WHERE id = ?1",
            params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    }

    fn meeting_policy(connection: &Connection) -> (String, i64) {
        connection
            .query_row(
                "SELECT policy_json, revision FROM meeting_retention_policy WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    #[test]
    fn an_untouched_thirty_day_store_keeps_its_meetings_and_deletes_audio_after_ninety_days() {
        let (migrations, mut connection, session_id) = store_before_audio_retention(THIRTY_DAYS);
        assert_eq!(meeting_policy(&connection), (THIRTY_DAYS.to_owned(), 0));
        let before = utc_now_ms() / 1_000 * 1_000;
        migrations.to_latest(&mut connection).unwrap();
        let after = utc_now_ms();

        assert_eq!(meeting_policy(&connection), (FOREVER.to_owned(), 0));
        assert_eq!(
            session_retention(&connection, &session_id),
            (FOREVER.to_owned(), None)
        );
        let audio = part_retention_policy_in(&connection, RetainedPart::Audio).unwrap();
        assert_eq!(
            audio.policy,
            MeetingRetentionPolicy::DeleteAfterDays { days: 90 }
        );
        assert_eq!(audio.revision, 0);
        assert!((before..=after).contains(&audio.changed_at_utc_ms));
        assert_eq!(
            audio.deletion_begins_at_utc_ms,
            Some(audio.changed_at_utc_ms + 7 * DAY)
        );
    }

    #[test]
    fn a_store_whose_policy_was_chosen_keeps_it_and_all_its_audio() {
        let (migrations, mut connection, session_id) = store_before_audio_retention(THIRTY_DAYS);
        connection
            .execute(
                "UPDATE meeting_retention_policy SET revision = 2 WHERE singleton = 1",
                [],
            )
            .unwrap();
        migrations.to_latest(&mut connection).unwrap();

        assert_eq!(meeting_policy(&connection), (THIRTY_DAYS.to_owned(), 2));
        assert_eq!(
            session_retention(&connection, &session_id),
            (THIRTY_DAYS.to_owned(), Some(31 * DAY))
        );
        let audio = part_retention_policy_in(&connection, RetainedPart::Audio).unwrap();
        assert_eq!(audio.policy, MeetingRetentionPolicy::Forever);
        assert_eq!(audio.deletion_begins_at_utc_ms, None);
    }

    #[test]
    fn deleting_audio_waits_seven_days_after_the_policy_starts_deleting() {
        let (_dir, store) = store();
        let session_id = ended(&store, Some(1), "review_ready");
        let seeded = store.audio_retention_policy().unwrap();
        let begins = seeded.changed_at_utc_ms + 7 * DAY;
        assert_eq!(seeded.deletion_begins_at_utc_ms, Some(begins));
        assert!(store
            .due_audio_retention_sessions(begins - 1)
            .unwrap()
            .is_empty());
        assert!(!store.purge_audio_at(session_id, begins - 1).unwrap());
        assert_eq!(
            store.due_audio_retention_sessions(begins).unwrap(),
            vec![session_id]
        );

        audio_policy(&store, None, NOW);
        audio_policy(&store, Some(30), NOW);
        assert!(store
            .due_audio_retention_sessions(NOW + 7 * DAY - 1)
            .unwrap()
            .is_empty());
        assert!(!store.purge_audio_at(session_id, NOW + 7 * DAY - 1).unwrap());
        assert_eq!(
            store
                .session_snapshot(session_id)
                .unwrap()
                .audio_purged_at_utc_ms,
            None
        );
        assert_eq!(
            store.due_audio_retention_sessions(NOW + 7 * DAY).unwrap(),
            vec![session_id]
        );
    }

    #[test]
    fn unfinished_meetings_keep_their_audio() {
        let (_dir, store) = store();
        let finished = ended(&store, Some(1), "review_ready");
        let unfinished: Vec<_> = [
            "preflight",
            "capturing_recording",
            "stopping",
            "processing",
            "recovery_required",
            "deleting",
        ]
        .into_iter()
        .map(|phase| (phase, ended(&store, Some(1), phase)))
        .collect();
        audio_policy(&store, Some(30), NOW - 7 * DAY);

        assert_eq!(
            store.due_audio_retention_sessions(NOW).unwrap(),
            vec![finished]
        );
        let purged: Vec<_> = unfinished
            .iter()
            .map(|&(phase, session_id)| (phase, store.purge_audio_at(session_id, NOW).unwrap()))
            .collect();
        let kept: Vec<_> = unfinished
            .iter()
            .map(|&(phase, _)| (phase, false))
            .collect();
        assert_eq!(purged, kept);
    }

    #[test]
    fn a_meeting_without_recorded_audio_is_left_alone() {
        let (_dir, store) = store();
        let imported = reviewable_meeting(&store, "Imported notes", 1);
        transcript_segments(&store, imported, &["typed before sona"]);
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE meeting_sessions SET ended_at_utc_ms = 1, phase = 'review_ready'
                 WHERE id = ?1",
                params![id(imported)],
            )
            .unwrap();
        audio_policy(&store, Some(30), NOW - 7 * DAY);

        assert!(store.due_audio_retention_sessions(NOW).unwrap().is_empty());
        assert_eq!(
            meetings_purging_by_in(&store.connection().unwrap(), NOW).unwrap(),
            0
        );
        assert!(!store.purge_audio_at(imported, NOW).unwrap());
        assert_eq!(
            store
                .session_snapshot(imported)
                .unwrap()
                .audio_purged_at_utc_ms,
            None
        );
    }

    #[test]
    fn the_sweep_removes_audio_keeps_the_words_and_marks_lines_not_kept() {
        let (_dir, store) = store();
        let session_id = ended(&store, Some(NOW - 30 * DAY), "review_ready");
        let (track_id, segment_id): (String, String) = store
            .connection()
            .unwrap()
            .query_row(
                "SELECT track_id, segment_id FROM meeting_transcript_segments",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let segment_id = TranscriptSegmentId::from_uuid(parse_uuid(&segment_id).unwrap());
        let folder = store.root.join(id(session_id));
        fs::create_dir_all(folder.join("tracks")).unwrap();
        fs::write(
            folder.join("tracks").join(format!("{track_id}.records")),
            b"encrypted audio",
        )
        .unwrap();
        fs::create_dir_all(folder.join("snapshots")).unwrap();
        fs::write(folder.join("snapshots/preview.jpg"), b"captured screen").unwrap();
        assert!(audio_retained_in(&store.connection().unwrap(), session_id, segment_id).unwrap());
        audio_policy(&store, Some(30), NOW - 7 * DAY);

        let due = store.due_audio_retention_sessions(NOW).unwrap();
        assert_eq!(due, vec![session_id]);
        assert_eq!(
            meetings_purging_by_in(&store.connection().unwrap(), NOW).unwrap(),
            1
        );
        assert!(store.purge_audio_at(session_id, NOW).unwrap());

        assert!(!folder.join("tracks").exists());
        assert!(folder.join("snapshots/preview.jpg").exists());
        assert_eq!(
            meetings_purging_by_in(&store.connection().unwrap(), NOW).unwrap(),
            0
        );
        let snapshot = store.session_snapshot(session_id).unwrap();
        assert_eq!(snapshot.phase, MeetingPhase::ReviewReady);
        assert_eq!(snapshot.audio_purged_at_utc_ms, Some(NOW));
        assert!(store.require_retained_transcript(session_id).is_ok());
        let words: String = store
            .connection()
            .unwrap()
            .query_row(
                "SELECT base_text FROM meeting_transcript_segments WHERE segment_id = ?1",
                params![id(segment_id)],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(words, "ship the draft on friday");
        assert!(!audio_retained_in(&store.connection().unwrap(), session_id, segment_id).unwrap());
        assert!(matches!(
            store.conversation_audio_clip(session_id, Some(segment_id), None, None),
            Err(StoreError::NotFound)
        ));
        assert!(store
            .due_audio_retention_sessions(NOW + DAY)
            .unwrap()
            .is_empty());
        assert!(!store.purge_audio_at(session_id, NOW + DAY).unwrap());
    }

    #[test]
    fn a_purge_interrupted_after_its_marker_finishes_on_the_next_sweep() {
        let (_dir, store) = store();
        let session_id = ended(&store, Some(1), "review_ready");
        let tracks = store.root.join(id(session_id)).join("tracks");
        fs::create_dir_all(store.root.join(id(session_id))).unwrap();
        fs::write(&tracks, b"not a directory").unwrap();
        audio_policy(&store, Some(30), NOW - 7 * DAY);

        assert!(store.purge_audio_at(session_id, NOW).is_err());
        assert_eq!(
            store
                .session_snapshot(session_id)
                .unwrap()
                .audio_purged_at_utc_ms,
            Some(NOW)
        );
        audio_policy(&store, None, NOW);
        assert_eq!(
            store.due_audio_retention_sessions(NOW).unwrap(),
            vec![session_id]
        );
        fs::remove_file(&tracks).unwrap();
        fs::create_dir_all(&tracks).unwrap();
        assert!(store.purge_audio_at(session_id, NOW + DAY).unwrap());
        assert!(!tracks.exists());
        assert_eq!(
            store
                .session_snapshot(session_id)
                .unwrap()
                .audio_purged_at_utc_ms,
            Some(NOW)
        );
        assert!(store
            .due_audio_retention_sessions(NOW + DAY)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn audio_whose_words_were_never_written_is_kept() {
        let (_dir, store) = store();
        let session_id = ended(&store, Some(1), "review_ready");
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE meeting_sessions SET current_transcript_revision_id = NULL WHERE id = ?1",
                params![id(session_id)],
            )
            .unwrap();
        audio_policy(&store, Some(30), NOW - 7 * DAY);

        assert!(store.due_audio_retention_sessions(NOW).unwrap().is_empty());
        assert_eq!(
            meetings_purging_by_in(&store.connection().unwrap(), NOW).unwrap(),
            0
        );
        assert!(!store.purge_audio_at(session_id, NOW).unwrap());
    }

    #[test]
    fn an_old_recording_keeps_its_audio_for_the_policy_after_it_is_imported() {
        let (_dir, store) = store();
        let session_id = ended(&store, Some(1), "review_ready");
        let imported_at = NOW - DAY;
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE meeting_sessions SET created_at_utc_ms = ?1 WHERE id = ?2",
                params![imported_at, id(session_id)],
            )
            .unwrap();
        audio_policy(&store, Some(30), NOW - 7 * DAY);

        assert!(store.due_audio_retention_sessions(NOW).unwrap().is_empty());
        assert_eq!(
            meetings_purging_by_in(&store.connection().unwrap(), NOW).unwrap(),
            0
        );
        assert!(!store.purge_audio_at(session_id, NOW).unwrap());
        assert_eq!(
            store
                .due_audio_retention_sessions(imported_at + 30 * DAY)
                .unwrap(),
            vec![session_id]
        );
    }
}
