use super::encode_json;
use super::voice_identity::{
    ExplicitVoiceConsent, MeetingSpeakerIdentifyDisposition, MeetingSpeakerIdentifyRequest,
    MeetingSpeakerIdentifyTarget, VoiceEnrollmentEvidence, VoiceProfileEnrollmentRequest,
    VoiceProfileStatus, VoiceSpeakerSuggestion,
};
use super::workflow_core_tests::{meeting, person, reviewable_meeting, store};
use super::*;
use crate::meeting::diarization::{
    wespeaker_embedding_model_key, SpeakerEmbedding, SPEAKER_EMBEDDING_DIMENSIONS,
};
use crate::meeting::people_types::{PersonId, VoiceProfileMergeResolution};
use crate::meeting::store::voice_identity::VoiceSpeakerIdentityResult;
use crate::meeting::transcript_correction_types::{
    TranscriptCorrection, TranscriptCorrectionContext, TranscriptCorrectionKind,
    TranscriptCorrectionRemoveRequest, TranscriptCorrectionTeachRequest,
    TranscriptCorrectionUpdateRequest,
};
use crate::meeting::types::{
    DeletionCause, MeetingDiarizationGenerationId, MeetingOperationId, MeetingOrigin, SourceTrackId,
};
use crate::secrets::SecretManager;
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

fn insert_speaker(store: &MeetingStore, session_id: MeetingSessionId, speaker_id: SpeakerId) {
    // PANIC: test fixture setup cannot continue without its encrypted store.
    store
        .connection()
        .expect("encrypted store connection")
        .execute(
            "INSERT INTO meeting_speakers (
                speaker_id, session_id, source_kind, display_name, revision, merged_into_speaker_id
             ) VALUES (?1, ?2, 'microphone', 'Speaker', 0, NULL)",
            params![id(speaker_id), id(session_id)],
        )
        .expect("speaker setup");
}

fn people_revision(store: &MeetingStore) -> u64 {
    // PANIC: test setup requires the initialized people-state singleton.
    let revision: i64 = store
        .connection()
        .expect("encrypted store connection")
        .query_row(
            "SELECT revision FROM people_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("people revision");
    // PANIC: the schema's nonnegative revision invariant permits this conversion.
    u64::try_from(revision).expect("nonnegative people revision")
}
fn voice(axis: usize) -> Result<SpeakerEmbedding, StoreError> {
    let mut values = [0.0_f32; SPEAKER_EMBEDDING_DIMENSIONS];
    values[axis] = 1.0;
    SpeakerEmbedding::from_normalized_slice(&values).ok_or(StoreError::VoiceInvariant)
}

fn unit_embedding() -> Result<SpeakerEmbedding, StoreError> {
    voice(0)
}

/// The one recording a session has: its microphone track and the diarization
/// generation over it. Created on first use and shared by every speaker a test
/// enrolls from that meeting, because a session holds one track per source
/// kind and one plan attempt at a time.
fn session_recording(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<(MeetingDiarizationGenerationId, SourceTrackId), StoreError> {
    let existing = connection
        .query_row(
            "SELECT g.generation_id, t.track_id
               FROM meeting_diarization_generations g
               JOIN meeting_source_tracks t ON t.session_id = g.session_id
              WHERE g.session_id = ?1 AND t.source_kind = 'microphone'",
            params![id(session_id)],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((generation_id, track_id)) = existing {
        let generation_id = Uuid::parse_str(&generation_id).map_err(|_| StoreError::Corrupt)?;
        let track_id = Uuid::parse_str(&track_id).map_err(|_| StoreError::Corrupt)?;
        return Ok((
            MeetingDiarizationGenerationId::from_uuid(generation_id),
            SourceTrackId::from_uuid(track_id),
        ));
    }
    let plan_id = Uuid::new_v4();
    let track_id = SourceTrackId::new();
    let transcript_revision_id = Uuid::new_v4();
    let generation_id = MeetingDiarizationGenerationId::new();
    connection.execute(
        "INSERT INTO meeting_run_plans (
            plan_id, session_id, attempt_number, schema_version, consent_id,
            canonical_plan_json, created_at_utc_ms
         ) VALUES (?1, ?2, 1, 1, ?3, '{}', 1)",
        params![
            plan_id.to_string(),
            id(session_id),
            Uuid::new_v4().to_string()
        ],
    )?;
    connection.execute(
        "INSERT INTO meeting_source_tracks (
            track_id, session_id, plan_id, source_kind, required, requested,
            descriptor_json, timestamp_bridge_json, health
         ) VALUES (?1, ?2, ?3, 'microphone', 1, 1, '{}', '{}', '\"healthy\"')",
        params![id(track_id), id(session_id), plan_id.to_string()],
    )?;
    connection.execute(
        "INSERT INTO meeting_track_records (
            track_id, source_sequence, source_epoch, start_offset_ns, duration_ns,
            frame_count, record_offset_bytes, record_bytes, durable_at_utc_ms
         ) VALUES (?1, 0, 0, 0, 4000000000, 64000, 0, 1, 1)",
        params![id(track_id)],
    )?;
    connection.execute(
        "INSERT INTO meeting_transcript_revisions (
            transcript_revision_id, session_id, engine_id, destination_json,
            source_set_json, language, state, created_at_utc_ms
         ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 1)",
        params![transcript_revision_id.to_string(), id(session_id)],
    )?;
    connection.execute(
        "INSERT INTO meeting_diarization_generations (
            generation_id, session_id, transcript_revision_id, input_revision, model_id,
            model_version, state, created_at_utc_ms, completed_at_utc_ms
         ) VALUES (?1, ?2, ?3, 0, 'test', 'test', 'completed', 1, 1)",
        params![
            id(generation_id),
            id(session_id),
            transcript_revision_id.to_string()
        ],
    )?;
    // `meeting` writes a bare `manual` into `origin_kind`, a JSON column the
    // enrollment gate decodes. Local evidence belongs to a session recorded the
    // way `create_meeting` records one.
    connection.execute(
        "UPDATE meeting_sessions SET origin_kind = ?1 WHERE id = ?2",
        params![encode_json(&MeetingOrigin::Manual)?, id(session_id)],
    )?;
    Ok((generation_id, track_id))
}

fn enrollment_evidence(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
) -> Result<VoiceEnrollmentEvidence, StoreError> {
    let connection = store.connection()?;
    let (generation_id, track_id) = session_recording(&connection, session_id)?;
    for (start_offset_ns, end_offset_ns) in
        [(0_i64, 2_000_000_000_i64), (2_000_000_000, 4_000_000_000)]
    {
        connection.execute(
            "INSERT INTO meeting_diarization_evidence_spans (
                generation_id, speaker_id, track_id, start_offset_ns, end_offset_ns, evidence_kind
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'sortformer_exclusive')",
            params![
                id(generation_id),
                id(speaker_id),
                id(track_id),
                start_offset_ns,
                end_offset_ns,
            ],
        )?;
    }
    VoiceEnrollmentEvidence::new(
        session_id,
        generation_id,
        speaker_id,
        track_id,
        0,
        4_000_000_000,
    )
}

fn meeting_revision(store: &MeetingStore, session_id: MeetingSessionId) -> Result<u64, StoreError> {
    let revision: i64 = store.connection()?.query_row(
        "SELECT revision FROM meeting_sessions WHERE id = ?1",
        params![id(session_id)],
        |row| row.get(0),
    )?;
    u64::try_from(revision).map_err(|_| StoreError::VoiceInvariant)
}

/// Label the speaker, then enroll the one approved sample behind it.
fn enroll(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
    person_id: PersonId,
) -> Result<(), StoreError> {
    let labeled = label_existing(
        store,
        session_id,
        speaker_id,
        person_id,
        meeting_revision(store, session_id)?,
    )?;
    let evidence = enrollment_evidence(store, session_id, speaker_id)?;
    let request = enrollment_request(
        person_id,
        evidence,
        labeled
            .receipt
            .new_revision
            .ok_or(StoreError::VoiceInvariant)?,
        store.active_voice_speaker_revision(session_id, speaker_id)?,
        people_revision(store),
    )?;
    store.commit_voice_profile_enrollment(request)?;
    Ok(())
}

fn enrollment_request(
    person_id: PersonId,
    evidence: VoiceEnrollmentEvidence,
    expected_meeting_revision: u64,
    expected_speaker_revision: u64,
    expected_people_revision: u64,
) -> Result<VoiceProfileEnrollmentRequest, StoreError> {
    Ok(VoiceProfileEnrollmentRequest {
        person_id,
        expected_meeting_revision,
        expected_people_revision,
        expected_speaker_revision,
        consent: ExplicitVoiceConsent::granted(1)?,
        evidence,
        model: wespeaker_embedding_model_key(),
        embedding: unit_embedding()?,
        committed_at_utc_ms: 5,
    })
}

fn label_existing(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
    person_id: PersonId,
    expected_meeting_revision: u64,
) -> Result<VoiceSpeakerIdentityResult, StoreError> {
    store.identify_speaker(MeetingSpeakerIdentifyRequest {
        operation_id: MeetingOperationId::new(),
        requested_at_utc_ms: 2,
        session_id,
        expected_meeting_revision,
        expected_people_revision: people_revision(store),
        speaker_id,
        disposition: MeetingSpeakerIdentifyDisposition::Label {
            target: MeetingSpeakerIdentifyTarget::Existing(person_id),
        },
    })
}

fn profile_count(store: &MeetingStore, person_id: PersonId) -> Result<i64, StoreError> {
    Ok(store.connection()?.query_row(
        "SELECT COUNT(*) FROM voice_profiles WHERE person_id = ?1",
        params![id(person_id)],
        |row| row.get(0),
    )?)
}

fn profile_sample_count(store: &MeetingStore, person_id: PersonId) -> Result<i64, StoreError> {
    Ok(store.connection()?.query_row(
        "SELECT COUNT(*) FROM voice_profile_samples WHERE person_id = ?1",
        params![id(person_id)],
        |row| row.get(0),
    )?)
}

fn insert_compatible_profile(
    store: &MeetingStore,
    person_id: PersonId,
    embedding: &SpeakerEmbedding,
) -> Result<(), StoreError> {
    let model = wespeaker_embedding_model_key();
    let centroid: Vec<u8> = embedding
        .as_slice()
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    store.connection()?.execute(
        "INSERT INTO voice_profiles (
            person_id, model_id, model_revision, model_sha256, embedding_dimensions,
            sample_rate_hz, feature_bins, feature_pipeline_revision, normalization,
            centroid, sample_count, profile_revision, consent_version, created_at_utc_ms, updated_at_utc_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, 1, 1, 1, 1)",
        params![
            id(person_id),
            model.model_id,
            model.model_revision,
            model.model_sha256,
            i64::try_from(model.embedding_dimensions).map_err(|_| StoreError::VoiceInvariant)?,
            i64::from(model.sample_rate_hz),
            i64::try_from(model.feature_bins).map_err(|_| StoreError::VoiceInvariant)?,
            model.feature_pipeline_revision,
            model.normalization,
            centroid,
        ],
    )?;
    Ok(())
}

#[test]
fn unresolved_voice_speaker_ids_are_ordered_and_exclude_current_resolutions(
) -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let unresolved = SpeakerId::new();
    let stale = SpeakerId::new();
    let automatic = SpeakerId::new();
    let manual = SpeakerId::new();
    let person_id = person(&store, "Generic Test Person", &[], &[]);
    let mut speakers = vec![unresolved, stale, automatic, manual];
    speakers.sort_by_key(|speaker_id| id(*speaker_id));
    for speaker_id in speakers.into_iter().rev() {
        insert_speaker(&store, session_id, speaker_id);
    }
    insert_compatible_profile(&store, person_id, &unit_embedding()?)?;
    // The store hands out one non-reentrant connection guard, so each fixture
    // write releases it before the query under test asks for it.
    {
        let connection = store.connection()?;
        for (speaker_id, profile_revision) in [(automatic, 1_i64), (stale, 2_i64)] {
            connection.execute(
                "INSERT INTO voice_speaker_matches (
                    speaker_id, session_id, person_id, profile_revision, speaker_revision, matched_at_utc_ms
                 ) VALUES (?1, ?2, ?3, ?4, 0, 1)",
                params![
                    id(speaker_id),
                    id(session_id),
                    id(person_id),
                    profile_revision,
                ],
            )?;
        }
        connection.execute(
            "INSERT INTO meeting_voice_speaker_resolutions (
                operation_id, speaker_id, resolution_kind, resolved_person_id, is_current
             ) VALUES (?1, ?2, 'unknown', NULL, 1)",
            params![id(MeetingOperationId::new()), id(manual)],
        )?;
    }

    let mut expected = vec![unresolved, stale];
    expected.sort_by_key(|speaker_id| id(*speaker_id));
    assert_eq!(
        store.unresolved_active_voice_speaker_ids(session_id, None)?,
        expected
    );
    Ok(())
}

#[test]
fn label_and_correction_return_the_resolved_person_ids() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let labeled_person_id = person(&store, "Ada Lovelace", &[], &[]);
    let corrected_person_id = person(&store, "Grace Hopper", &[], &[]);

    let labeled = store
        .identify_speaker(MeetingSpeakerIdentifyRequest {
            operation_id: MeetingOperationId::new(),
            requested_at_utc_ms: 2,
            session_id,
            expected_meeting_revision: 0,
            expected_people_revision: people_revision(&store),
            speaker_id,
            disposition: MeetingSpeakerIdentifyDisposition::Label {
                target: MeetingSpeakerIdentifyTarget::Existing(labeled_person_id),
            },
        })
        .expect("label speaker");
    assert_eq!(labeled.resolved_person_id, Some(labeled_person_id));

    let corrected = store
        .identify_speaker(MeetingSpeakerIdentifyRequest {
            operation_id: MeetingOperationId::new(),
            requested_at_utc_ms: 3,
            session_id,
            expected_meeting_revision: labeled.receipt.new_revision.expect("label revision"),
            expected_people_revision: people_revision(&store),
            speaker_id,
            disposition: MeetingSpeakerIdentifyDisposition::CorrectTo {
                target: MeetingSpeakerIdentifyTarget::Existing(corrected_person_id),
            },
        })
        .expect("correct speaker");
    assert_eq!(corrected.resolved_person_id, Some(corrected_person_id));
    assert!(store
        .unresolved_active_voice_speaker_ids(session_id, None)
        .expect("list manually resolved speakers")
        .is_empty());
}

#[test]
fn create_identity_replays_the_same_created_person_id() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let request = MeetingSpeakerIdentifyRequest {
        operation_id: MeetingOperationId::new(),
        requested_at_utc_ms: 2,
        session_id,
        expected_meeting_revision: 0,
        expected_people_revision: people_revision(&store),
        speaker_id,
        disposition: MeetingSpeakerIdentifyDisposition::Label {
            target: MeetingSpeakerIdentifyTarget::Create {
                display_name: "Katherine Johnson".to_owned(),
            },
        },
    };

    let first = store
        .identify_speaker(request.clone())
        .expect("create identity");
    let created_person_id = first.resolved_person_id.expect("created person id");
    let correction_person_id = person(&store, "Dorothy Vaughan", &[], &[]);
    let correction = store
        .identify_speaker(MeetingSpeakerIdentifyRequest {
            operation_id: MeetingOperationId::new(),
            requested_at_utc_ms: 3,
            session_id,
            expected_meeting_revision: first.receipt.new_revision.expect("create revision"),
            expected_people_revision: people_revision(&store),
            speaker_id,
            disposition: MeetingSpeakerIdentifyDisposition::CorrectTo {
                target: MeetingSpeakerIdentifyTarget::Existing(correction_person_id),
            },
        })
        .expect("correct created identity");
    assert_eq!(correction.resolved_person_id, Some(correction_person_id));
    let replay = store.identify_speaker(request).expect("replay identity");
    assert_eq!(replay, first);
    let people_with_created_id: i64 = store
        .connection()
        .expect("encrypted store connection")
        .query_row(
            "SELECT COUNT(*) FROM persons WHERE id = ?1",
            params![id(created_person_id)],
            |row| row.get(0),
        )
        .expect("created person lookup");
    assert_eq!(people_with_created_id, 1);
}

#[test]
fn manual_voice_resolutions_remain_resolved_after_the_encrypted_store_reopens() {
    let directory = TempDir::new().expect("temporary meeting store root");
    let secrets = SecretManager::with_backend(Arc::new(crate::secrets::MemorySecretBackend::new()));
    let root = directory.path().join("meetings");
    let store = MeetingStore::open(
        root.clone(),
        tauri::async_runtime::block_on(secrets.meeting_storage_key()).expect("meeting key"),
    )
    .expect("open encrypted store");
    let session_id = meeting(&store, "Voice", 1);
    let labeled_speaker_id = SpeakerId::new();
    let corrected_speaker_id = SpeakerId::new();
    let unknown_speaker_id = SpeakerId::new();
    for speaker_id in [labeled_speaker_id, corrected_speaker_id, unknown_speaker_id] {
        insert_speaker(&store, session_id, speaker_id);
    }
    let labeled_person_id = person(&store, "Ada Lovelace", &[], &[]);
    let corrected_person_id = person(&store, "Grace Hopper", &[], &[]);

    let labeled = store
        .identify_speaker(MeetingSpeakerIdentifyRequest {
            operation_id: MeetingOperationId::new(),
            requested_at_utc_ms: 2,
            session_id,
            expected_meeting_revision: 0,
            expected_people_revision: people_revision(&store),
            speaker_id: labeled_speaker_id,
            disposition: MeetingSpeakerIdentifyDisposition::Label {
                target: MeetingSpeakerIdentifyTarget::Existing(labeled_person_id),
            },
        })
        .expect("label speaker");
    assert_eq!(labeled.resolved_person_id, Some(labeled_person_id));

    let corrected = store
        .identify_speaker(MeetingSpeakerIdentifyRequest {
            operation_id: MeetingOperationId::new(),
            requested_at_utc_ms: 3,
            session_id,
            expected_meeting_revision: labeled.receipt.new_revision.expect("label revision"),
            expected_people_revision: people_revision(&store),
            speaker_id: corrected_speaker_id,
            disposition: MeetingSpeakerIdentifyDisposition::CorrectTo {
                target: MeetingSpeakerIdentifyTarget::Existing(corrected_person_id),
            },
        })
        .expect("correct speaker");
    assert_eq!(corrected.resolved_person_id, Some(corrected_person_id));

    let marked_unknown = store
        .identify_speaker(MeetingSpeakerIdentifyRequest {
            operation_id: MeetingOperationId::new(),
            requested_at_utc_ms: 4,
            session_id,
            expected_meeting_revision: corrected.receipt.new_revision.expect("correction revision"),
            expected_people_revision: people_revision(&store),
            speaker_id: unknown_speaker_id,
            disposition: MeetingSpeakerIdentifyDisposition::MarkUnknown,
        })
        .expect("mark speaker unknown");
    assert_eq!(marked_unknown.resolved_person_id, None);
    drop(store);

    let reopened = MeetingStore::open(
        root,
        tauri::async_runtime::block_on(secrets.meeting_storage_key())
            .expect("reopened meeting key"),
    )
    .expect("reopen encrypted store");
    assert!(reopened
        .unresolved_active_voice_speaker_ids(session_id, None)
        .expect("list after reopen")
        .is_empty());
}
#[test]
fn enrollment_accepts_the_current_manual_resolution_once() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let person_id = person(&store, "Ada Lovelace", &[], &[]);
    let labeled = label_existing(&store, session_id, speaker_id, person_id, 0)?;
    let evidence = enrollment_evidence(&store, session_id, speaker_id)?;
    let meeting_revision = labeled
        .receipt
        .new_revision
        .ok_or(StoreError::VoiceInvariant)?;
    let speaker_revision = store.active_voice_speaker_revision(session_id, speaker_id)?;

    let enrolled = store.commit_voice_profile_enrollment(enrollment_request(
        person_id,
        evidence,
        meeting_revision,
        speaker_revision,
        people_revision(&store),
    )?)?;
    assert!(matches!(
        enrolled,
        VoiceProfileStatus::Enrolled {
            sample_count: 1,
            ..
        }
    ));

    let replayed = store.commit_voice_profile_enrollment(enrollment_request(
        person_id,
        evidence,
        meeting_revision,
        speaker_revision,
        people_revision(&store),
    )?)?;
    assert!(matches!(
        replayed,
        VoiceProfileStatus::Enrolled {
            sample_count: 1,
            ..
        }
    ));
    assert_eq!(profile_count(&store, person_id)?, 1);
    assert_eq!(profile_sample_count(&store, person_id)?, 1);
    Ok(())
}

#[test]
fn enrollment_uses_contiguous_speech_duration_not_coalesced_row_count() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let person_id = person(&store, "Ada Lovelace", &[], &[]);
    let labeled = label_existing(&store, session_id, speaker_id, person_id, 0)?;
    {
        let connection = store.connection()?;
        let (generation_id, track_id) = session_recording(&connection, session_id)?;
        connection.execute(
            "UPDATE meeting_track_records SET duration_ns = 14000000000, frame_count = 224000
             WHERE track_id = ?1",
            params![id(track_id)],
        )?;
        connection.execute(
            "INSERT INTO meeting_diarization_evidence_spans (
                generation_id, speaker_id, track_id, start_offset_ns, end_offset_ns, evidence_kind
             ) VALUES (?1, ?2, ?3, 0, 14000000000, 'sortformer_exclusive')",
            params![id(generation_id), id(speaker_id), id(track_id)],
        )?;
    }
    let evidence = store.local_voice_enrollment_evidence(session_id, speaker_id)?;
    assert_eq!(evidence.start_offset_ns(), 0);
    assert_eq!(evidence.end_offset_ns(), 12_000_000_000);
    let status = store.commit_voice_profile_enrollment(enrollment_request(
        person_id,
        evidence,
        labeled
            .receipt
            .new_revision
            .ok_or(StoreError::VoiceInvariant)?,
        store.active_voice_speaker_revision(session_id, speaker_id)?,
        people_revision(&store),
    )?)?;
    assert!(matches!(
        status,
        VoiceProfileStatus::Enrolled {
            sample_count: 1,
            ..
        }
    ));
    Ok(())
}

#[test]
fn enrollment_does_not_join_short_speech_across_a_gap() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    enrollment_evidence(&store, session_id, speaker_id)?;
    store.connection()?.execute(
        "UPDATE meeting_diarization_evidence_spans SET start_offset_ns = 2500000000
         WHERE speaker_id = ?1 AND start_offset_ns = 2000000000",
        params![id(speaker_id)],
    )?;
    assert_eq!(
        store.local_voice_enrollment_evidence(session_id, speaker_id),
        Err(StoreError::InsufficientEnrollmentEvidence),
    );
    Ok(())
}

/// Removing a person's voice profile is the deletion half of the identity
/// contract, and the samples behind it are retained speech. The mutation
/// deletes `voice_profiles` and `voice_speaker_matches` and never names
/// `voice_profile_samples`, which go with the profile only while the schema
/// cascades them. So this asserts the retained rows and the matcher's own
/// answer, not the status the call returns.
#[test]
fn removing_a_voice_profile_deletes_the_retained_samples_and_stops_matching(
) -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let person_id = person(&store, "Ada Lovelace", &[], &[]);
    enroll(&store, session_id, speaker_id, person_id)?;
    assert_eq!(profile_sample_count(&store, person_id)?, 1);
    assert!(store
        .match_local_voice_profile(&unit_embedding()?, wespeaker_embedding_model_key())?
        .is_some());

    let status = store.remove_voice_profile(person_id, people_revision(&store))?;

    assert_eq!(status, VoiceProfileStatus::Unenrolled);
    assert_eq!(profile_count(&store, person_id)?, 0);
    assert_eq!(profile_sample_count(&store, person_id)?, 0);
    assert!(store
        .match_local_voice_profile(&unit_embedding()?, wespeaker_embedding_model_key())?
        .is_none());
    Ok(())
}

#[test]
fn enrollment_rejects_a_person_without_the_current_manual_resolution() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let resolved_person_id = person(&store, "Ada Lovelace", &[], &[]);
    let injected_person_id = person(&store, "Grace Hopper", &[], &[]);
    let labeled = label_existing(&store, session_id, speaker_id, resolved_person_id, 0)?;
    let evidence = enrollment_evidence(&store, session_id, speaker_id)?;
    let meeting_revision = labeled
        .receipt
        .new_revision
        .ok_or(StoreError::VoiceInvariant)?;
    let speaker_revision = store.active_voice_speaker_revision(session_id, speaker_id)?;

    assert_eq!(
        store
            .commit_voice_profile_enrollment(enrollment_request(
                injected_person_id,
                evidence,
                meeting_revision,
                speaker_revision,
                people_revision(&store),
            )?)
            .err(),
        Some(StoreError::Conflict),
    );
    assert_eq!(profile_count(&store, injected_person_id)?, 0);
    assert_eq!(profile_sample_count(&store, injected_person_id)?, 0);
    Ok(())
}

#[test]
fn enrollment_rejects_stale_meeting_and_speaker_fences_after_correction() -> Result<(), StoreError>
{
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let original_person_id = person(&store, "Ada Lovelace", &[], &[]);
    let corrected_person_id = person(&store, "Grace Hopper", &[], &[]);
    let labeled = label_existing(&store, session_id, speaker_id, original_person_id, 0)?;
    let evidence = enrollment_evidence(&store, session_id, speaker_id)?;
    let stale_meeting_revision = labeled
        .receipt
        .new_revision
        .ok_or(StoreError::VoiceInvariant)?;
    let stale_speaker_revision = store.active_voice_speaker_revision(session_id, speaker_id)?;

    let corrected = store.identify_speaker(MeetingSpeakerIdentifyRequest {
        operation_id: MeetingOperationId::new(),
        requested_at_utc_ms: 3,
        session_id,
        expected_meeting_revision: stale_meeting_revision,
        expected_people_revision: people_revision(&store),
        speaker_id,
        disposition: MeetingSpeakerIdentifyDisposition::CorrectTo {
            target: MeetingSpeakerIdentifyTarget::Existing(corrected_person_id),
        },
    })?;
    assert_eq!(corrected.resolved_person_id, Some(corrected_person_id));

    let current_meeting_revision = corrected
        .receipt
        .new_revision
        .ok_or(StoreError::VoiceInvariant)?;
    let current_speaker_revision = store.active_voice_speaker_revision(session_id, speaker_id)?;
    assert_ne!(current_meeting_revision, stale_meeting_revision);
    assert_ne!(current_speaker_revision, stale_speaker_revision);

    assert_eq!(
        store
            .commit_voice_profile_enrollment(enrollment_request(
                original_person_id,
                evidence,
                stale_meeting_revision,
                current_speaker_revision,
                people_revision(&store),
            )?)
            .err(),
        Some(StoreError::StaleRevision),
    );
    assert_eq!(
        store
            .commit_voice_profile_enrollment(enrollment_request(
                original_person_id,
                evidence,
                current_meeting_revision,
                stale_speaker_revision,
                people_revision(&store),
            )?)
            .err(),
        Some(StoreError::StaleRevision),
    );
    assert_eq!(profile_count(&store, original_person_id)?, 0);
    assert_eq!(profile_sample_count(&store, original_person_id)?, 0);
    Ok(())
}

#[test]
fn enrollment_rejects_evidence_already_owned_by_another_person() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let first_person_id = person(&store, "Ada Lovelace", &[], &[]);
    let second_person_id = person(&store, "Grace Hopper", &[], &[]);
    let labeled = label_existing(&store, session_id, speaker_id, first_person_id, 0)?;
    let evidence = enrollment_evidence(&store, session_id, speaker_id)?;
    let meeting_revision = labeled
        .receipt
        .new_revision
        .ok_or(StoreError::VoiceInvariant)?;
    let speaker_revision = store.active_voice_speaker_revision(session_id, speaker_id)?;
    store.commit_voice_profile_enrollment(enrollment_request(
        first_person_id,
        evidence,
        meeting_revision,
        speaker_revision,
        people_revision(&store),
    )?)?;

    store.connection()?.execute(
        "UPDATE meeting_voice_speaker_resolutions
            SET resolved_person_id = ?1
          WHERE speaker_id = ?2 AND is_current = 1",
        params![id(second_person_id), id(speaker_id)],
    )?;
    assert_eq!(
        store
            .commit_voice_profile_enrollment(enrollment_request(
                second_person_id,
                evidence,
                meeting_revision,
                speaker_revision,
                people_revision(&store),
            )?)
            .err(),
        Some(StoreError::Conflict),
    );
    assert_eq!(profile_count(&store, second_person_id)?, 0);
    assert_eq!(profile_sample_count(&store, second_person_id)?, 0);
    Ok(())
}

#[test]
fn successful_match_rejects_a_profile_set_changed_by_concurrent_enrollment(
) -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let matched_speaker_id = SpeakerId::new();
    let enrolling_speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, matched_speaker_id);
    insert_speaker(&store, session_id, enrolling_speaker_id);
    let matched_person_id = person(&store, "Ada Lovelace", &[], &[]);
    let enrolling_person_id = person(&store, "Grace Hopper", &[], &[]);
    let labeled = label_existing(
        &store,
        session_id,
        enrolling_speaker_id,
        enrolling_person_id,
        0,
    )?;
    let embedding = unit_embedding()?;
    insert_compatible_profile(&store, matched_person_id, &embedding)?;
    let matched = store
        .match_local_voice_profile(&embedding, wespeaker_embedding_model_key())?
        .ok_or(StoreError::VoiceInvariant)?;

    let evidence = enrollment_evidence(&store, session_id, enrolling_speaker_id)?;
    let meeting_revision = labeled
        .receipt
        .new_revision
        .ok_or(StoreError::VoiceInvariant)?;
    let enrolling_speaker_revision =
        store.active_voice_speaker_revision(session_id, enrolling_speaker_id)?;
    store.commit_voice_profile_enrollment(enrollment_request(
        enrolling_person_id,
        evidence,
        meeting_revision,
        enrolling_speaker_revision,
        people_revision(&store),
    )?)?;
    assert!(store
        .match_local_voice_profile(&embedding, wespeaker_embedding_model_key())?
        .is_none());

    assert_eq!(
        store
            .commit_successful_voice_match(session_id, matched_speaker_id, 0, matched, 6)
            .err(),
        Some(StoreError::StaleRevision),
    );
    let match_count: i64 = store.connection()?.query_row(
        "SELECT COUNT(*) FROM voice_speaker_matches WHERE speaker_id = ?1",
        params![id(matched_speaker_id)],
        |row| row.get(0),
    )?;
    assert_eq!(match_count, 0);
    Ok(())
}

#[test]
fn enrollment_rejects_evidence_from_an_imported_meeting() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let person_id = person(&store, "Ada Lovelace", &[], &[]);
    let labeled = label_existing(&store, session_id, speaker_id, person_id, 0)?;
    let evidence = enrollment_evidence(&store, session_id, speaker_id)?;
    let meeting_revision = labeled
        .receipt
        .new_revision
        .ok_or(StoreError::VoiceInvariant)?;
    let speaker_revision = store.active_voice_speaker_revision(session_id, speaker_id)?;
    // Production writes this column through `encode_json`, so the stored value
    // is the quoted JSON form and nothing else can stand in for it.
    store.connection()?.execute(
        "UPDATE meeting_sessions SET origin_kind = ?1 WHERE id = ?2",
        params![encode_json(&MeetingOrigin::Import)?, id(session_id)],
    )?;

    assert_eq!(
        store
            .commit_voice_profile_enrollment(enrollment_request(
                person_id,
                evidence,
                meeting_revision,
                speaker_revision,
                people_revision(&store),
            )?)
            .err(),
        Some(StoreError::LocalEvidenceUnavailable),
    );
    assert_eq!(profile_count(&store, person_id)?, 0);
    assert_eq!(profile_sample_count(&store, person_id)?, 0);
    Ok(())
}

#[test]
fn combining_compatible_profiles_repoints_every_sample_to_the_target() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let source_speaker_id = SpeakerId::new();
    let target_speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, source_speaker_id);
    insert_speaker(&store, session_id, target_speaker_id);
    let source_person_id = person(&store, "Ada Lovelace", &[], &[]);
    let target_person_id = person(&store, "Grace Hopper", &[], &[]);
    enroll(&store, session_id, source_speaker_id, source_person_id)?;
    enroll(&store, session_id, target_speaker_id, target_person_id)?;
    assert_eq!(profile_sample_count(&store, source_person_id)?, 1);
    assert_eq!(profile_sample_count(&store, target_person_id)?, 1);

    store.merge_persons_with_voice_resolution(
        source_person_id,
        target_person_id,
        people_revision(&store),
        Some(VoiceProfileMergeResolution::CombineCompatible),
        7,
    )?;

    assert_eq!(profile_count(&store, source_person_id)?, 0);
    assert_eq!(profile_sample_count(&store, source_person_id)?, 0);
    assert_eq!(profile_count(&store, target_person_id)?, 1);
    assert_eq!(profile_sample_count(&store, target_person_id)?, 2);
    Ok(())
}

#[test]
fn replacing_a_target_with_a_sourceless_profile_keeps_the_target_profile() -> Result<(), StoreError>
{
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let source_person_id = person(&store, "Ada Lovelace", &[], &[]);
    let target_person_id = person(&store, "Grace Hopper", &[], &[]);
    enroll(&store, session_id, speaker_id, target_person_id)?;

    store.merge_persons_with_voice_resolution(
        source_person_id,
        target_person_id,
        people_revision(&store),
        Some(VoiceProfileMergeResolution::ReplaceTargetWithSource),
        7,
    )?;

    assert_eq!(profile_count(&store, target_person_id)?, 1);
    assert_eq!(profile_sample_count(&store, target_person_id)?, 1);
    Ok(())
}

/// The default merge sends `CombineCompatible`. With one enrolled voice
/// between the two people there is nothing to be incompatible with: the
/// merged person keeps that voice, whichever side it was on.
#[test]
fn combining_moves_a_sole_source_profile_to_the_target() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let source_person_id = person(&store, "Ada Lovelace", &[], &[]);
    let target_person_id = person(&store, "Grace Hopper", &[], &[]);
    enroll(&store, session_id, speaker_id, source_person_id)?;

    store.merge_persons_with_voice_resolution(
        source_person_id,
        target_person_id,
        people_revision(&store),
        Some(VoiceProfileMergeResolution::CombineCompatible),
        7,
    )?;

    assert_eq!(profile_count(&store, source_person_id)?, 0);
    assert_eq!(profile_count(&store, target_person_id)?, 1);
    assert_eq!(profile_sample_count(&store, target_person_id)?, 1);
    Ok(())
}

#[test]
fn combining_keeps_a_sole_target_profile() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let source_person_id = person(&store, "Ada Lovelace", &[], &[]);
    let target_person_id = person(&store, "Grace Hopper", &[], &[]);
    enroll(&store, session_id, speaker_id, target_person_id)?;

    store.merge_persons_with_voice_resolution(
        source_person_id,
        target_person_id,
        people_revision(&store),
        Some(VoiceProfileMergeResolution::CombineCompatible),
        7,
    )?;

    assert_eq!(profile_count(&store, target_person_id)?, 1);
    assert_eq!(profile_sample_count(&store, target_person_id)?, 1);
    Ok(())
}

#[test]
fn marking_unknown_cleans_up_a_profile_from_a_superseded_model() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(&store, session_id, speaker_id);
    let person_id = person(&store, "Ada Lovelace", &[], &[]);
    enroll(&store, session_id, speaker_id, person_id)?;
    // The running model moves on. The stored row keeps its own model columns,
    // and cleanup still has to be able to retire it.
    store.connection()?.execute(
        "UPDATE voice_profiles SET model_revision = 'superseded' WHERE person_id = ?1",
        params![id(person_id)],
    )?;

    store.identify_speaker(MeetingSpeakerIdentifyRequest {
        operation_id: MeetingOperationId::new(),
        requested_at_utc_ms: 8,
        session_id,
        expected_meeting_revision: meeting_revision(&store, session_id)?,
        expected_people_revision: people_revision(&store),
        speaker_id,
        disposition: MeetingSpeakerIdentifyDisposition::MarkUnknown,
    })?;

    assert_eq!(profile_sample_count(&store, person_id)?, 0);
    assert_eq!(profile_count(&store, person_id)?, 0);
    Ok(())
}

#[test]
fn matching_refuses_a_profile_from_another_model_revision() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let person_id = person(&store, "Ada Lovelace", &[], &[]);
    let embedding = unit_embedding()?;
    insert_compatible_profile(&store, person_id, &embedding)?;
    let model = wespeaker_embedding_model_key();
    assert!(store
        .match_local_voice_profile(&embedding, model)?
        .is_some());

    // The stored row now belongs to a superseded model. Compatibility lives
    // entirely on this side of the boundary: the matcher no longer takes a key.
    store.connection()?.execute(
        "UPDATE voice_profiles SET model_revision = 'superseded' WHERE person_id = ?1",
        params![id(person_id)],
    )?;

    assert!(store
        .match_local_voice_profile(&embedding, model)?
        .is_none());
    Ok(())
}

/// Automatic matching is what "remember this voice" pays off as: a speaker
/// still under the label the store wrote takes the matched person's name and
/// leaves the to-label queue, while a name somebody typed survives the match.
#[test]
fn successful_match_names_a_generated_speaker_and_keeps_a_typed_name() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let person_id = person(&store, "Ada Lovelace", &[], &[]);
    let embedding = unit_embedding()?;
    insert_compatible_profile(&store, person_id, &embedding)?;
    for (label, expected) in [
        ("Speaker 2", "Ada Lovelace"),
        ("Unknown speaker", "Ada Lovelace"),
        ("Bob", "Bob"),
        ("Speaker", "Speaker"),
        ("Speaker 2b", "Speaker 2b"),
    ] {
        let speaker_id = SpeakerId::new();
        store.connection()?.execute(
            "INSERT INTO meeting_speakers (
                speaker_id, session_id, source_kind, display_name, revision, merged_into_speaker_id
             ) VALUES (?1, ?2, 'microphone', ?3, 0, NULL)",
            params![id(speaker_id), id(session_id), label],
        )?;
        let matched = store
            .match_local_voice_profile(&embedding, wespeaker_embedding_model_key())?
            .ok_or(StoreError::VoiceInvariant)?;
        store.commit_successful_voice_match(session_id, speaker_id, 0, matched, 6)?;
        let name: String = store.connection()?.query_row(
            "SELECT display_name FROM meeting_speakers WHERE speaker_id = ?1",
            params![id(speaker_id)],
            |row| row.get(0),
        )?;
        assert_eq!(name, expected, "{label:?}");
        assert!(
            !store
                .unresolved_active_voice_speaker_ids(session_id, None)?
                .contains(&speaker_id),
            "{label:?} stayed in the to-label queue"
        );
    }
    let links: i64 = store.connection()?.query_row(
        "SELECT COUNT(*) FROM meeting_person_links WHERE meeting_id = ?1 AND person_id = ?2",
        params![id(session_id), id(person_id)],
        |row| row.get(0),
    )?;
    assert_eq!(links, 1);
    Ok(())
}

fn learned_phrase_source(
    store: &MeetingStore,
    person_id: PersonId,
    text: &str,
) -> Result<(MeetingSessionId, TranscriptSegmentId, SpeakerId), StoreError> {
    let session_id = super::workflow_core_tests::reviewable_meeting(store, "Learned phrase", 1);
    let speaker_id = SpeakerId::new();
    insert_speaker(store, session_id, speaker_id);
    let (generation_id, track_id) = session_recording(&*store.connection()?, session_id)?;
    let segment_id = TranscriptSegmentId::new();
    {
        let connection = store.connection()?;
        let revision_id: String = connection.query_row(
            "SELECT transcript_revision_id FROM meeting_diarization_generations
             WHERE generation_id = ?1",
            params![id(generation_id)],
            |row| row.get(0),
        )?;
        connection.execute(
            "INSERT INTO meeting_transcript_segments
             (segment_id, transcript_revision_id, track_id, ordinal, start_offset_ns,
              end_offset_ns, speaker_id, base_text)
             VALUES (?1, ?2, ?3, 0, 0, 4000000000, ?4, ?5)",
            params![
                id(segment_id),
                revision_id,
                id(track_id),
                id(speaker_id),
                text
            ],
        )?;
        connection.execute(
            "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
            params![revision_id, id(session_id)],
        )?;
    }
    label_existing(store, session_id, speaker_id, person_id, 0)?;
    Ok((session_id, segment_id, speaker_id))
}

fn teach_learned_phrase(
    store: &MeetingStore,
    person_id: PersonId,
    session_id: MeetingSessionId,
    segment_id: TranscriptSegmentId,
    replacement: &str,
) -> Result<TranscriptCorrection, StoreError> {
    let before = store.transcript_correction_review(session_id)?;
    let learned = store.teach_transcript_correction(TranscriptCorrectionTeachRequest {
        person_id,
        session_id,
        segment_id,
        expected_meeting_revision: before.meeting_revision,
        expected_correction_revision: before.correction_revision,
        original_phrase: "sowna".to_string(),
        replacement_phrase: replacement.to_string(),
        kind: TranscriptCorrectionKind::Phrase,
        context: TranscriptCorrectionContext::Person,
    })?;
    learned
        .corrections
        .rules
        .into_iter()
        .find(|rule| rule.source_segment_id == segment_id)
        .ok_or(StoreError::Corrupt)
}

#[test]
fn merged_learned_phrase_keeps_its_source_and_refreshes_both_search_indexes(
) -> Result<(), StoreError> {
    let (_directory, store) = store();
    let source_person = person(&store, "Source", &[], &[]);
    let target_person = person(&store, "Target", &[], &[]);
    let (source_session, source_segment, _) =
        learned_phrase_source(&store, source_person, "ship the sowna memory view")?;
    let (target_session, _, _) =
        learned_phrase_source(&store, target_person, "keep the sowna transcript")?;
    let rule = teach_learned_phrase(
        &store,
        source_person,
        source_session,
        source_segment,
        "ZephyrReviewProof",
    )?;
    assert!(store
        .query_meetings_lexical("sowna", None, 10)?
        .iter()
        .any(|row| row.session_id == target_session));
    let old_inputs = store
        .semantic_index_inputs(target_session)?
        .ok_or(StoreError::Corrupt)?;
    let embedding = [1.0_f32, 0.0, 0.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    store.replace_semantic_chunks(
        target_session,
        &old_inputs.key,
        "learned-phrase-model",
        1,
        &[(old_inputs.transcript.join("\n"), embedding)],
    )?;
    let vectors = store.query_semantic_chunk_vectors("learned-phrase-model", None)?;
    assert_eq!(vectors.len(), 1);
    let old_chunk = vectors[0].chunk_id;
    store.merge_persons_with_voice_resolution(
        source_person,
        target_person,
        people_revision(&store),
        None,
        10,
    )?;
    let mut expected_rule = rule;
    expected_rule.person_id = target_person;
    assert_eq!(
        store.transcript_corrections(target_person)?.rules,
        [expected_rule.clone()]
    );
    let new_inputs = store
        .semantic_index_inputs(target_session)?
        .ok_or(StoreError::Corrupt)?;
    assert_ne!(new_inputs.key, old_inputs.key);
    assert_eq!(
        new_inputs.transcript,
        ["keep the ZephyrReviewProof transcript"]
    );
    assert!(store
        .query_semantic_chunk_vectors("learned-phrase-model", None)?
        .is_empty());
    assert!(store.query_meetings_by_chunk(&[old_chunk])?.is_empty());
    for session_id in [source_session, target_session] {
        assert!(store
            .query_meetings_lexical("ZephyrReviewProof", None, 10)?
            .iter()
            .any(|row| row.session_id == session_id));
        assert!(!store
            .query_meetings_lexical("sowna", None, 10)?
            .iter()
            .any(|row| row.session_id == session_id));
    }
    let current_rules = store.transcript_corrections(target_person)?;
    store.update_transcript_correction(TranscriptCorrectionUpdateRequest {
        correction_id: expected_rule.correction_id,
        person_id: target_person,
        expected_correction_revision: current_rules.revision,
        original_phrase: "sowna".to_string(),
        replacement_phrase: "PolarisReviewProof".to_string(),
        kind: expected_rule.kind,
        context: expected_rule.context.clone(),
        enabled: expected_rule.enabled,
    })?;
    assert!(store
        .query_meetings_lexical("ZephyrReviewProof", None, 10)?
        .is_empty());
    for session_id in [source_session, target_session] {
        assert!(store
            .query_meetings_lexical("PolarisReviewProof", None, 10)?
            .iter()
            .any(|row| row.session_id == session_id));
        assert!(store.transcript_correction_review(session_id)?.segments[0]
            .raw_text
            .contains("sowna"));
    }
    let current_rules = store.transcript_corrections(target_person)?;
    store.remove_transcript_correction(TranscriptCorrectionRemoveRequest {
        correction_id: expected_rule.correction_id,
        person_id: target_person,
        expected_correction_revision: current_rules.revision,
    })?;
    assert!(store
        .query_meetings_lexical("PolarisReviewProof", None, 10)?
        .is_empty());
    let raw_hits = store.query_meetings_lexical("sowna", None, 10)?;
    for session_id in [source_session, target_session] {
        assert!(raw_hits.iter().any(|row| row.session_id == session_id));
    }
    Ok(())
}

#[test]
fn removing_a_correction_source_republishes_dependent_words_without_changing_raw_speech(
) -> Result<(), StoreError> {
    let (_directory, store) = store();
    let person_id = person(&store, "Ada", &[], &[]);
    let (source_session, source_segment, _) =
        learned_phrase_source(&store, person_id, "teach sowna")?;
    let (dependent_session, dependent_segment, _) =
        learned_phrase_source(&store, person_id, "remember sowna")?;
    teach_learned_phrase(&store, person_id, source_session, source_segment, "Sona")?;
    let before_turn = store
        .transcript_correction_review(dependent_session)?
        .segments
        .into_iter()
        .find(|segment| segment.segment_id == dependent_segment)
        .ok_or(StoreError::Corrupt)?;
    assert_eq!(before_turn.raw_text, "remember sowna");
    assert_eq!(before_turn.corrected_text, "remember Sona");

    let before = store.transcript_correction_source_snapshot(source_session)?;
    store.edit_segment(SegmentEdit {
        mutation: StoreMutation {
            operation_id: MeetingOperationId::new(),
            actor: OperationActor::User,
            requested_at_utc_ms: 11,
            session_id: source_session,
            expected_revision: meeting_revision(&store, source_session)?,
            command: MeetingCommandKind::SegmentEdit,
        },
        segment_id: source_segment,
        replacement_text: "teach sowna".to_owned(),
        removed: true,
    })?;
    let changed = store.transcript_correction_projection_changes(before)?;
    assert!(changed
        .iter()
        .any(|session| session.session_id == dependent_session));

    let after_turn = store
        .transcript_correction_review(dependent_session)?
        .segments
        .into_iter()
        .find(|segment| segment.segment_id == dependent_segment)
        .ok_or(StoreError::Corrupt)?;
    assert_eq!(after_turn.raw_text, "remember sowna");
    assert_eq!(after_turn.corrected_text, "remember sowna");
    Ok(())
}

#[test]
fn conflicting_learned_phrases_reject_a_person_merge_without_changing_either_person(
) -> Result<(), StoreError> {
    let (_directory, store) = store();
    let source_person = person(&store, "Source", &[], &[]);
    let target_person = person(&store, "Target", &[], &[]);
    let (source_session, source_segment, _) =
        learned_phrase_source(&store, source_person, "ship sowna")?;
    let (target_session, target_segment, _) =
        learned_phrase_source(&store, target_person, "keep sowna")?;
    teach_learned_phrase(
        &store,
        source_person,
        source_session,
        source_segment,
        "Sona",
    )?;
    teach_learned_phrase(
        &store,
        target_person,
        target_session,
        target_segment,
        "Sonar",
    )?;
    let source_before = store.transcript_correction_review(source_session)?;
    let target_before = store.transcript_correction_review(target_session)?;
    let people_before = people_revision(&store);
    assert!(matches!(
        store.merge_persons_with_voice_resolution(
            source_person,
            target_person,
            people_before,
            None,
            10,
        ),
        Err(StoreError::TranscriptCorrectionConflict)
    ));
    assert_eq!(people_revision(&store), people_before);
    assert_eq!(
        store.transcript_correction_review(source_session)?,
        source_before
    );
    assert_eq!(
        store.transcript_correction_review(target_session)?,
        target_before
    );
    let remaining_people: i64 = store.connection()?.query_row(
        "SELECT COUNT(*) FROM persons WHERE id IN (?1, ?2)",
        params![id(source_person), id(target_person)],
        |row| row.get(0),
    )?;
    assert_eq!(remaining_people, 2);
    Ok(())
}

#[test]
fn merging_identical_phrases_keeps_supported_evidence_instead_of_an_inactive_copy(
) -> Result<(), StoreError> {
    let (_directory, store) = store();
    let source_person = person(&store, "Source", &[], &[]);
    let target_person = person(&store, "Target", &[], &[]);
    let (target_session, target_segment, target_speaker) =
        learned_phrase_source(&store, target_person, "old sowna evidence")?;
    let target_rule = teach_learned_phrase(
        &store,
        target_person,
        target_session,
        target_segment,
        "Sona",
    )?;
    store.identify_speaker(MeetingSpeakerIdentifyRequest {
        operation_id: MeetingOperationId::new(),
        requested_at_utc_ms: 5,
        session_id: target_session,
        expected_meeting_revision: meeting_revision(&store, target_session)?,
        expected_people_revision: people_revision(&store),
        speaker_id: target_speaker,
        disposition: MeetingSpeakerIdentifyDisposition::MarkUnknown,
    })?;
    assert!(!store.transcript_corrections(target_person)?.rules[0].active);
    let (source_session, source_segment, _) =
        learned_phrase_source(&store, source_person, "new sowna evidence")?;
    let source_rule = teach_learned_phrase(
        &store,
        source_person,
        source_session,
        source_segment,
        "Sona",
    )?;
    store.merge_persons_with_voice_resolution(
        source_person,
        target_person,
        people_revision(&store),
        None,
        10,
    )?;
    let mut expected_rule = source_rule;
    expected_rule.person_id = target_person;
    let merged = store.transcript_corrections(target_person)?;
    assert_eq!(merged.rules, [expected_rule]);
    assert!(merged
        .rules
        .iter()
        .all(|rule| rule.correction_id != target_rule.correction_id));
    let (future_session, _, _) =
        learned_phrase_source(&store, target_person, "future sowna contribution")?;
    let future = store.transcript_correction_review(future_session)?;
    assert_eq!(future.segments[0].raw_text, "future sowna contribution");
    assert_eq!(
        future.segments[0].corrected_text,
        "future Sona contribution"
    );
    Ok(())
}

fn keep_voices(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    voices: &[(SpeakerId, usize)],
) -> Result<(), StoreError> {
    let voices = voices
        .iter()
        .map(|(speaker_id, axis)| Ok((*speaker_id, voice(*axis)?)))
        .collect::<Result<Vec<_>, StoreError>>()?;
    store.store_speaker_voiceprints(session_id, wespeaker_embedding_model_key(), &voices, 1)
}

/// The axis of the one voice kept for a speaker, if any.
fn kept_voice(store: &MeetingStore, speaker_id: SpeakerId) -> Result<Option<usize>, StoreError> {
    let blob: Option<Vec<u8>> = store
        .connection()?
        .query_row(
            "SELECT embedding FROM meeting_speaker_voiceprints WHERE speaker_id = ?1",
            params![id(speaker_id)],
            |row| row.get(0),
        )
        .optional()?;
    Ok(blob.and_then(|blob| {
        blob.chunks_exact(4)
            .position(|value| value == 1.0_f32.to_le_bytes())
    }))
}

#[test]
fn a_voice_named_in_one_meeting_is_suggested_in_the_next_and_applied_only_on_confirm(
) -> Result<(), StoreError> {
    let (_directory, store) = store();
    let ada = person(&store, "Ada Lovelace", &[], &[]);
    let first = meeting(&store, "First", 1);
    let named = SpeakerId::new();
    insert_speaker(&store, first, named);
    keep_voices(&store, first, &[(named, 0)])?;
    label_existing(&store, first, named, ada, meeting_revision(&store, first)?)?;

    let second = meeting(&store, "Second", 2);
    let same_voice = SpeakerId::new();
    let other_voice = SpeakerId::new();
    insert_speaker(&store, second, same_voice);
    insert_speaker(&store, second, other_voice);
    keep_voices(&store, second, &[(same_voice, 0), (other_voice, 1)])?;

    assert_eq!(
        store.voice_speaker_suggestions(second, None)?,
        [VoiceSpeakerSuggestion {
            speaker_id: same_voice,
            person_id: Some(ada),
            display_name: "Ada Lovelace".to_owned(),
        }]
    );
    let second_revision = meeting_revision(&store, second)?;
    assert!(store
        .unresolved_active_voice_speaker_ids(second, None)?
        .contains(&same_voice));
    let (display_name, links): (String, i64) = store.connection()?.query_row(
        "SELECT speaker.display_name,
                (SELECT COUNT(*) FROM meeting_person_links WHERE meeting_id = speaker.session_id)
           FROM meeting_speakers speaker WHERE speaker.speaker_id = ?1",
        params![id(same_voice)],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((display_name.as_str(), links), ("Speaker", 0));

    label_existing(&store, second, same_voice, ada, second_revision)?;
    assert!(store.voice_speaker_suggestions(second, None)?.is_empty());
    Ok(())
}

#[test]
fn a_kept_voice_is_forgotten_with_its_speaker_its_person_or_its_meeting() -> Result<(), StoreError>
{
    let (_directory, store) = store();
    let ada = person(&store, "Ada Lovelace", &[], &[]);
    let grace = person(&store, "Grace Hopper", &[], &[]);
    let session_id = meeting(&store, "Voice", 1);
    let unknown = SpeakerId::new();
    let corrected = SpeakerId::new();
    let named = SpeakerId::new();
    let kept = SpeakerId::new();
    for speaker_id in [unknown, corrected, named, kept] {
        insert_speaker(&store, session_id, speaker_id);
    }
    keep_voices(
        &store,
        session_id,
        &[(unknown, 0), (corrected, 1), (named, 2), (kept, 3)],
    )?;
    for (speaker_id, person_id) in [(corrected, ada), (named, ada), (kept, grace)] {
        label_existing(
            &store,
            session_id,
            speaker_id,
            person_id,
            meeting_revision(&store, session_id)?,
        )?;
    }
    for (speaker_id, disposition) in [
        (unknown, MeetingSpeakerIdentifyDisposition::MarkUnknown),
        (
            corrected,
            MeetingSpeakerIdentifyDisposition::CorrectTo {
                target: MeetingSpeakerIdentifyTarget::Existing(grace),
            },
        ),
    ] {
        store.identify_speaker(MeetingSpeakerIdentifyRequest {
            operation_id: MeetingOperationId::new(),
            requested_at_utc_ms: 3,
            session_id,
            expected_meeting_revision: meeting_revision(&store, session_id)?,
            expected_people_revision: people_revision(&store),
            speaker_id,
            disposition,
        })?;
    }
    assert_eq!(kept_voice(&store, unknown)?, None);
    assert_eq!(kept_voice(&store, corrected)?, None);
    assert_eq!(kept_voice(&store, named)?, Some(2));

    store.remove_voice_profile(ada, people_revision(&store))?;
    assert_eq!(kept_voice(&store, named)?, None);
    assert_eq!(kept_voice(&store, kept)?, Some(3));

    let (_, job_id) = store.reserve_deletion(
        MeetingOperationId::new(),
        4,
        session_id,
        meeting_revision(&store, session_id)?,
        DeletionCause::User,
    )?;
    store.finish_deletion(job_id)?;
    assert_eq!(kept_voice(&store, kept)?, None);
    Ok(())
}

#[test]
fn merging_speakers_keeps_the_target_voice_or_takes_the_source_one() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Voice", 1);
    let source = SpeakerId::new();
    let silent_target = SpeakerId::new();
    let voiced_source = SpeakerId::new();
    let voiced_target = SpeakerId::new();
    for speaker_id in [source, silent_target, voiced_source, voiced_target] {
        insert_speaker(&store, session_id, speaker_id);
    }
    keep_voices(
        &store,
        session_id,
        &[(source, 0), (voiced_source, 1), (voiced_target, 2)],
    )?;
    for (from, into) in [(source, silent_target), (voiced_source, voiced_target)] {
        store.merge_speaker(
            MeetingOperationId::new(),
            2,
            session_id,
            meeting_revision(&store, session_id)?,
            from,
            into,
        )?;
    }
    assert_eq!(kept_voice(&store, silent_target)?, Some(0));
    assert_eq!(kept_voice(&store, voiced_target)?, Some(2));
    assert_eq!(kept_voice(&store, source)?, None);
    assert_eq!(kept_voice(&store, voiced_source)?, None);

    keep_voices(&store, session_id, &[(source, 5)])?;
    assert_eq!(kept_voice(&store, source)?, None);
    Ok(())
}

/// Lines diarization gave to `Speaker N`, `N` being `cluster + 1`, over one
/// system-audio track, published the way processing publishes them.
fn diarized_transcript(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    lines: &[(u32, &str)],
) -> Result<(), StoreError> {
    let transcript_revision_id = TranscriptRevisionId::new();
    let segments = {
        let connection = store.connection()?;
        let plan_id = Uuid::new_v4();
        let track_id = SourceTrackId::new();
        let source_speaker_id = SpeakerId::new();
        connection.execute(
            "INSERT INTO meeting_run_plans (
                plan_id, session_id, attempt_number, schema_version, consent_id,
                canonical_plan_json, created_at_utc_ms
             ) VALUES (?1, ?2, 1, 1, ?3, '{}', 1)",
            params![
                plan_id.to_string(),
                id(session_id),
                Uuid::new_v4().to_string()
            ],
        )?;
        connection.execute(
            "INSERT INTO meeting_source_tracks (
                track_id, session_id, plan_id, source_kind, required, requested,
                descriptor_json, timestamp_bridge_json, health
             ) VALUES (?1, ?2, ?3, 'system_audio', 1, 1, '{}', '{}', '\"healthy\"')",
            params![id(track_id), id(session_id), plan_id.to_string()],
        )?;
        connection.execute(
            "INSERT INTO meeting_speakers (
                speaker_id, session_id, source_kind, display_name, revision
             ) VALUES (?1, ?2, 'system_audio', 'Unknown speaker', 0)",
            params![id(source_speaker_id), id(session_id)],
        )?;
        connection.execute(
            "INSERT INTO meeting_transcript_revisions (
                transcript_revision_id, session_id, engine_id, destination_json,
                source_set_json, language, state, created_at_utc_ms
             ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 1)",
            params![id(transcript_revision_id), id(session_id)],
        )?;
        let mut segments = Vec::new();
        for (ordinal, (cluster, text)) in (0_i64..).zip(lines) {
            let segment_id = TranscriptSegmentId::new();
            connection.execute(
                "INSERT INTO meeting_transcript_segments (
                    segment_id, transcript_revision_id, track_id, ordinal,
                    start_offset_ns, end_offset_ns, speaker_id, base_text
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id(segment_id),
                    id(transcript_revision_id),
                    id(track_id),
                    ordinal,
                    ordinal * 1_000_000_000,
                    ordinal * 1_000_000_000 + 1,
                    id(source_speaker_id),
                    text
                ],
            )?;
            segments.push((segment_id, *cluster));
        }
        connection.execute(
            "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
            params![id(transcript_revision_id), id(session_id)],
        )?;
        segments
    };
    let generation_id = store.begin_diarization_generation(
        session_id,
        transcript_revision_id,
        0,
        "test",
        "test",
    )?;
    let assignments = segments
        .into_iter()
        .map(|(segment_id, cluster)| {
            Ok(DiarizationAssignmentInput {
                segment_id,
                speaker_id: store.diarization_speaker(session_id, cluster)?,
                assignment: SpeakerAssignmentKind::SystemSpeaker,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    store.write_diarization_assignments(generation_id, &assignments)?;
    store.publish_diarization_generation(session_id, generation_id)?;
    Ok(())
}

fn rename(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
    name: &str,
    user_name: Option<&str>,
) -> Result<(), StoreError> {
    store.rename_speaker(SpeakerRename {
        operation_id: MeetingOperationId::new(),
        requested_at_utc_ms: 2,
        session_id,
        expected_revision: meeting_revision(store, session_id)?,
        speaker_id,
        display_name: name.to_owned(),
        user_name,
    })?;
    Ok(())
}

fn people_named(store: &MeetingStore, name: &str) -> Result<Vec<PersonId>, StoreError> {
    let connection = store.connection()?;
    let mut statement = connection.prepare("SELECT id FROM persons WHERE display_name = ?1")?;
    let rows = statement.query_map(params![name], |row| row.get::<_, String>(0))?;
    rows.map(|row| {
        let person_id = Uuid::parse_str(&row?).map_err(|_| StoreError::Corrupt)?;
        Ok(PersonId::from_uuid(person_id))
    })
    .collect()
}

/// Naming a speaker in the transcript names every line it said and teaches
/// Sona the voice: the next meeting offers that person for the same voice and
/// names nobody until the user confirms.
#[test]
fn naming_a_speaker_remembers_the_voice_for_a_suggestion_in_the_next_meeting(
) -> Result<(), StoreError> {
    let (_directory, store) = store();
    let first = reviewable_meeting(&store, "First", 1);
    diarized_transcript(
        &store,
        first,
        &[
            (0, "Morning."),
            (1, "Hi, it's Maya."),
            (0, "Shall we start?"),
            (1, "Yes."),
        ],
    )?;
    let speaker_one = store.diarization_speaker(first, 0)?;
    let speaker_two = store.diarization_speaker(first, 1)?;
    keep_voices(&store, first, &[(speaker_one, 1), (speaker_two, 0)])?;

    rename(&store, first, speaker_two, "Maya", None)?;

    let review = store.review_snapshot(first)?;
    let lines = review
        .transcript
        .iter()
        .map(|segment| {
            let speaker = review
                .speakers
                .iter()
                .find(|speaker| speaker.speaker_id == segment.assigned_speaker_id);
            (
                speaker.map(|speaker| speaker.display_name.as_str()),
                segment.base.text.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        [
            (Some("Speaker 1"), "Morning."),
            (Some("Maya"), "Hi, it's Maya."),
            (Some("Speaker 1"), "Shall we start?"),
            (Some("Maya"), "Yes."),
        ]
    );
    let maya = people_named(&store, "Maya")?;
    assert_eq!(maya.len(), 1);
    assert_eq!(kept_voice(&store, speaker_two)?, Some(0));
    assert!(!store
        .unresolved_active_voice_speaker_ids(first, None)?
        .contains(&speaker_two));

    let second = meeting(&store, "Second", 3);
    let same_voice = store.diarization_speaker(second, 0)?;
    let other_voice = store.diarization_speaker(second, 1)?;
    keep_voices(&store, second, &[(same_voice, 0), (other_voice, 1)])?;
    assert_eq!(
        store.voice_speaker_suggestions(second, None)?,
        [VoiceSpeakerSuggestion {
            speaker_id: same_voice,
            person_id: Some(maya[0]),
            display_name: "Maya".to_owned(),
        }]
    );
    let (display_name, links): (String, i64) = store.connection()?.query_row(
        "SELECT speaker.display_name,
                (SELECT COUNT(*) FROM meeting_person_links WHERE meeting_id = speaker.session_id)
           FROM meeting_speakers speaker WHERE speaker.speaker_id = ?1",
        params![id(same_voice)],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((display_name.as_str(), links), ("Speaker 1", 0));
    assert!(store
        .unresolved_active_voice_speaker_ids(second, None)?
        .contains(&same_voice));
    Ok(())
}

/// Renaming back to the label Sona wrote, as the chat rename's undo does,
/// takes the voice off the person it was given to.
#[test]
fn renaming_a_speaker_back_to_its_label_takes_the_voice_off_the_person() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let first = meeting(&store, "First", 1);
    let speaker = store.diarization_speaker(first, 1)?;
    keep_voices(&store, first, &[(speaker, 0)])?;

    rename(&store, first, speaker, "Maya", None)?;
    rename(&store, first, speaker, "Speaker 2", None)?;

    assert!(store
        .unresolved_active_voice_speaker_ids(first, None)?
        .contains(&speaker));
    let second = meeting(&store, "Second", 3);
    let same_voice = store.diarization_speaker(second, 0)?;
    keep_voices(&store, second, &[(same_voice, 0)])?;
    assert!(store.voice_speaker_suggestions(second, None)?.is_empty());
    Ok(())
}

/// The microphone speaker is the user, who is never a person, whatever name
/// they give it.
#[test]
fn naming_the_users_own_microphone_voice_creates_no_person() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let first = meeting(&store, "First", 1);
    let local = SpeakerId::new();
    insert_speaker(&store, first, local);

    rename(&store, first, local, "Aktan", None)?;

    assert!(people_named(&store, "Aktan")?.is_empty());
    assert!(store
        .unresolved_active_voice_speaker_ids(first, None)?
        .contains(&local));
    Ok(())
}

/// A bare first name goes to the one person already known by it instead of
/// creating a second one.
#[test]
fn a_first_name_names_the_one_person_known_by_it() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let maya = person(&store, "Maya Chen", &[], &[]);
    let first = meeting(&store, "First", 1);
    let speaker = store.diarization_speaker(first, 1)?;
    keep_voices(&store, first, &[(speaker, 0)])?;

    rename(&store, first, speaker, "Maya", None)?;

    assert!(people_named(&store, "Maya")?.is_empty());
    let second = meeting(&store, "Second", 3);
    let same_voice = store.diarization_speaker(second, 0)?;
    keep_voices(&store, second, &[(same_voice, 0)])?;
    assert_eq!(
        store.voice_speaker_suggestions(second, None)?,
        [VoiceSpeakerSuggestion {
            speaker_id: same_voice,
            person_id: Some(maya),
            display_name: "Maya Chen".to_owned(),
        }]
    );
    Ok(())
}

/// The user's name from About me, in full or by first name, is the user's
/// voice on any lane: naming a speaker after them creates no person, takes
/// the voice off whoever it was given to, and answers who is speaking, so
/// the speaker no longer waits for a name.
#[test]
fn naming_a_speaker_after_the_user_creates_no_person() -> Result<(), StoreError> {
    let (_directory, store) = store();
    person(&store, "Maya Chen", &[], &[]);
    let first = meeting(&store, "First", 1);
    let speaker = store.diarization_speaker(first, 1)?;
    keep_voices(&store, first, &[(speaker, 0)])?;
    let user = Some("Aktan Azat");

    rename(&store, first, speaker, "Maya", user)?;
    rename(&store, first, speaker, "Aktan", user)?;
    assert!(!store
        .unresolved_active_voice_speaker_ids(first, user)?
        .contains(&speaker));
    rename(&store, first, speaker, "Aktan Azat", user)?;
    let people: i64 = store
        .connection()?
        .query_row("SELECT COUNT(*) FROM persons", [], |row| row.get(0))?;
    assert_eq!(people, 1);

    rename(&store, first, speaker, "Maya", user)?;
    assert!(!store
        .unresolved_active_voice_speaker_ids(first, user)?
        .contains(&speaker));
    Ok(())
}

/// A voice named after the user in one meeting is suggested as theirs in the
/// next, under their full About me name, and names nobody until confirmed.
#[test]
fn a_voice_named_after_the_user_is_suggested_as_theirs() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let user = Some("Aktan Azat");
    let first = meeting(&store, "First", 1);
    let mine = store.diarization_speaker(first, 1)?;
    keep_voices(&store, first, &[(mine, 0)])?;
    rename(&store, first, mine, "aktan", user)?;

    let second = meeting(&store, "Second", 3);
    let same_voice = store.diarization_speaker(second, 0)?;
    let other_voice = store.diarization_speaker(second, 1)?;
    keep_voices(&store, second, &[(same_voice, 0), (other_voice, 1)])?;
    let suggested = [VoiceSpeakerSuggestion {
        speaker_id: same_voice,
        person_id: None,
        display_name: "Aktan Azat".to_owned(),
    }];
    assert_eq!(store.voice_speaker_suggestions(second, user)?, suggested);
    assert!(store.voice_speaker_suggestions(second, None)?.is_empty());
    assert!(people_named(&store, "Aktan Azat")?.is_empty());

    rename(&store, second, same_voice, "Aktan Azat", user)?;
    assert!(store.voice_speaker_suggestions(second, user)?.is_empty());
    Ok(())
}

/// A name is the user's by Unicode case, which SQLite's `lower` does not
/// fold: "Émile" names "Émile Zola" all the same.
#[test]
fn a_voice_named_after_the_user_in_any_script_is_suggested_as_theirs() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let user = Some("Émile Zola");
    let first = meeting(&store, "First", 1);
    let mine = store.diarization_speaker(first, 1)?;
    keep_voices(&store, first, &[(mine, 0)])?;
    rename(&store, first, mine, "Émile", user)?;

    let second = meeting(&store, "Second", 3);
    let same_voice = store.diarization_speaker(second, 0)?;
    keep_voices(&store, second, &[(same_voice, 0)])?;
    assert_eq!(
        store.voice_speaker_suggestions(second, user)?,
        [VoiceSpeakerSuggestion {
            speaker_id: same_voice,
            person_id: None,
            display_name: "Émile Zola".to_owned(),
        }]
    );
    Ok(())
}

/// A person who carries the user's name holds the user's voice: it joins the
/// voices named after the user instead of standing beside them as a second
/// match for the same voice, which would leave neither suggested.
#[test]
fn a_person_named_after_the_user_counts_as_the_users_voice() -> Result<(), StoreError> {
    let (_directory, store) = store();
    let user = Some("Aktan Azat");
    let namesake = person(&store, "Aktan Azat", &[], &[]);
    let first = meeting(&store, "First", 1);
    let labelled = SpeakerId::new();
    insert_speaker(&store, first, labelled);
    keep_voices(&store, first, &[(labelled, 0)])?;
    label_existing(
        &store,
        first,
        labelled,
        namesake,
        meeting_revision(&store, first)?,
    )?;

    let second = meeting(&store, "Second", 2);
    let renamed = store.diarization_speaker(second, 1)?;
    keep_voices(&store, second, &[(renamed, 0)])?;
    rename(&store, second, renamed, "Aktan", user)?;

    let third = meeting(&store, "Third", 3);
    let same_voice = store.diarization_speaker(third, 0)?;
    keep_voices(&store, third, &[(same_voice, 0)])?;
    assert_eq!(
        store.voice_speaker_suggestions(third, user)?,
        [VoiceSpeakerSuggestion {
            speaker_id: same_voice,
            person_id: None,
            display_name: "Aktan Azat".to_owned(),
        }]
    );
    Ok(())
}
