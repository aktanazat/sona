use super::super::workflow_core_tests::{person, reviewable_meeting, store, transcript_segments};
use super::*;

const DAY: i64 = 86_400_000;
const NOW: i64 = 400 * DAY;

fn ended(store: &MeetingStore, ended_at: i64, phase: &str) -> Result<MeetingSessionId, StoreError> {
    let session_id = reviewable_meeting(store, "Inventory", 1);
    transcript_segments(store, session_id, &["held"]);
    store.connection()?.execute(
        "UPDATE meeting_sessions SET ended_at_utc_ms = ?1, phase = ?2 WHERE id = ?3",
        params![ended_at, phase, id(session_id)],
    )?;
    Ok(session_id)
}

/// Audio on the microphone track `transcript_segments` made and, given a
/// second length, on a system track beside it: one second per record.
fn recorded(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    track_seconds: &[i64],
) -> Result<(), StoreError> {
    let connection = store.connection()?;
    let (microphone, plan_id): (String, String) = connection.query_row(
        "SELECT track_id, plan_id FROM meeting_source_tracks WHERE session_id = ?1",
        params![id(session_id)],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let system = Uuid::new_v4().to_string();
    connection.execute(
        "INSERT INTO meeting_source_tracks (
            track_id, session_id, plan_id, source_kind, required, requested,
            descriptor_json, timestamp_bridge_json, health
         ) VALUES (?1, ?2, ?3, 'system_audio', 1, 1, '{}', '{}', '\"healthy\"')",
        params![system, id(session_id), plan_id],
    )?;
    for (track_id, seconds) in [microphone, system].iter().zip(track_seconds) {
        for start in 0..*seconds {
            connection.execute(
                "INSERT INTO meeting_track_records (
                    track_id, source_sequence, source_epoch, start_offset_ns, duration_ns,
                    frame_count, record_offset_bytes, record_bytes, durable_at_utc_ms
                 ) VALUES (?1, ?2, 0, ?3, 1000000000, 1, 0, 1, 1)",
                params![track_id, start, start * 1_000_000_000],
            )?;
        }
    }
    Ok(())
}

fn file(path: PathBuf, bytes: usize) -> Result<(), StoreError> {
    fs::create_dir_all(path.parent().ok_or(StoreError::Io)?)?;
    fs::write(path, vec![0_u8; bytes])?;
    Ok(())
}

#[test]
fn counts_what_the_store_holds_and_measures_track_files() {
    let (_dir, store) = store();
    let empty = store.holdings(NOW).unwrap();
    assert_eq!(empty.recordings.duration_ms, 0);
    assert_eq!(empty.transcripts, 0);
    assert_eq!(empty.purging_within_week, 0);

    let recent = ended(&store, NOW - DAY, "review_ready").unwrap();
    recorded(&store, recent, &[3, 2]).unwrap();
    let old = ended(&store, NOW - 25 * DAY, "recovery_required").unwrap();
    recorded(&store, old, &[4]).unwrap();
    let purged = ended(&store, NOW - 90 * DAY, "review_ready").unwrap();
    recorded(&store, purged, &[60]).unwrap();
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_sessions SET transcript_purged_at_utc_ms = 1 WHERE id = ?1",
            params![id(purged)],
        )
        .unwrap();
    let deleting = ended(&store, NOW - 90 * DAY, "deleting").unwrap();
    recorded(&store, deleting, &[60]).unwrap();

    let current = store.transcript_retention_policy().unwrap();
    store
        .set_transcript_retention_policy(
            MeetingOperationId::new(),
            NOW - 7 * DAY,
            current.revision,
            &MeetingRetentionPolicy::DeleteAfterDays { days: 30 },
        )
        .unwrap();

    let portrayed = person(&store, "Ada", &[], &[]);
    let connection = store.connection().unwrap();
    connection
        .execute(
            "INSERT INTO person_avatars (person_id, png, updated_at_utc_ms, original)
             VALUES (?1, zeroblob(10), 1, zeroblob(20))",
            params![id(portrayed)],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO person_contact_cards (person_id, card_json, updated_at_utc_ms)
             VALUES (?1, '{}', 1)",
            params![id(portrayed)],
        )
        .unwrap();
    drop(connection);
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_cloud_outbox (
                outbox_id, kind, object_id, idempotency_key, state, attempt_count,
                next_attempt_utc_ms, payload_relative_dir, created_at_utc_ms, updated_at_utc_ms
             ) VALUES ('o', 'object', 'x', 'k', 'completed', 0, 0, '.cloud-outbox/o', 1, 1234)",
            [],
        )
        .unwrap();
    let speaker_id = SpeakerId::new();
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_speakers (
                speaker_id, session_id, source_kind, display_name, revision, merged_into_speaker_id
             ) VALUES (?1, ?2, 'system_audio', 'Speaker 1', 0, NULL)",
            params![id(speaker_id), id(recent)],
        )
        .unwrap();
    let mut voice = [0.0_f32; crate::meeting::diarization::SPEAKER_EMBEDDING_DIMENSIONS];
    voice[0] = 1.0;
    store
        .store_speaker_voiceprints(
            recent,
            crate::meeting::diarization::wespeaker_embedding_model_key(),
            &[(
                speaker_id,
                crate::meeting::diarization::SpeakerEmbedding::from_normalized_slice(&voice)
                    .unwrap(),
            )],
            1,
        )
        .unwrap();
    store
        .save_user_voiceprint(super::super::voice_identity::UserVoiceprintRequest {
            consent: super::super::voice_identity::ExplicitVoiceConsent::granted(1).unwrap(),
            model: crate::meeting::diarization::wespeaker_embedding_model_key(),
            embedding: crate::meeting::diarization::SpeakerEmbedding::from_normalized_slice(&voice)
                .unwrap(),
            voiceprint: super::super::voice_identity::UserVoiceprint {
                dictation_count: 12,
                speech_ms: 18_000,
            },
            created_at_utc_ms: 1,
        })
        .unwrap();

    file(
        store.root.join(id(recent)).join("tracks").join("a.records"),
        100,
    )
    .unwrap();
    file(
        store.root.join(id(recent)).join("tracks").join("a.index"),
        11,
    )
    .unwrap();
    file(
        store.root.join(id(old)).join("tracks").join("b.records"),
        40,
    )
    .unwrap();
    file(store.root.join(id(old)).join("notes.json"), 7).unwrap();
    file(
        store.root.join(".cloud-outbox").join("tracks").join("c"),
        500,
    )
    .unwrap();

    assert_eq!(
        store.holdings(NOW).unwrap(),
        MeetingHoldings {
            recordings: RecordingHoldings {
                meetings: 2,
                duration_ms: 7_000,
                bytes: 151,
            },
            transcripts: 2,
            voiceprints: VoiceprintHoldings {
                people: 1,
                sample_ms: 18_000,
                speakers: 1,
            },
            portraits: PortraitHoldings {
                count: 1,
                bytes: 30,
            },
            contact_cards: 1,
            purging_within_week: 1,
            last_cloud_upload_at_utc_ms: Some(1234),
        }
    );
}
