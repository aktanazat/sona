use super::people::calendar_context_in;
use super::people::{
    all_people_in, derive_calendar_links_in, derive_speaker_link_in, derive_title_links_in,
    recompute_organizations_in,
};
use super::*;
use crate::meeting::detection::machine::{
    CalendarAttendee, CalendarEventSummary, ParticipationStatus,
};
use crate::meeting::people_types::{
    PersonId, PersonLinkConfidence, PersonLinkSource, PersonSplitRequest, PersonSplitTarget,
};
use crate::meeting::workflow_types::{NewWorkflowEvent, WorkflowEventKind, WorkflowId};
use crate::secrets::SecretManager;
use rusqlite::params;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

/// The learning-inputs boundary for tests that are not about the loops: an empty
/// corpus and settings that claim nothing, so a mining pass is a no-op.
pub(crate) fn inputs() -> impl crate::meeting::store::learning::LearningInputs {
    crate::meeting::learning::no_inputs()
}

pub(crate) fn store() -> (TempDir, Arc<MeetingStore>) {
    let directory = TempDir::new().unwrap();
    let secrets = SecretManager::with_backend(Arc::new(crate::secrets::MemorySecretBackend::new()));
    let key = tauri::async_runtime::block_on(secrets.meeting_storage_key()).unwrap();
    let store = MeetingStore::open(directory.path().join("meetings"), key).unwrap();
    (directory, store)
}

pub(crate) fn meeting(store: &MeetingStore, title: &str, at_utc_ms: i64) -> MeetingSessionId {
    let id = MeetingSessionId::new();
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_sessions (
                id, phase, revision, title, origin_kind, preflight_json,
                created_at_utc_ms, started_at_utc_ms, processing_status,
                retention_policy_json
             ) VALUES (?1, 'review_ready', 0, ?2, 'manual', '{}', ?3, ?3, 'pending', 'forever')",
            params![id.uuid().to_string(), title, at_utc_ms],
        )
        .unwrap();
    id
}

/// A meeting whose row survives `review_snapshot`.
///
/// [`meeting`] writes bare strings into `processing_status` and
/// `retention_policy_json`, which are JSON columns: every reader that has
/// needed them so far reads around them, and `review_snapshot` is the one that
/// decodes. A test whose subject reads the review record starts here instead.
pub(crate) fn reviewable_meeting(
    store: &MeetingStore,
    title: &str,
    at_utc_ms: i64,
) -> MeetingSessionId {
    let id = meeting(store, title, at_utc_ms);
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_sessions
                SET processing_status = '{\"kind\":\"succeeded\"}',
                    retention_policy_json = '{\"kind\":\"forever\"}'
              WHERE id = ?1",
            params![id.uuid().to_string()],
        )
        .unwrap();
    id
}

pub(crate) fn person(
    store: &MeetingStore,
    name: &str,
    aliases: &[&str],
    emails: &[&str],
) -> PersonId {
    let id = PersonId::new();
    let aliases = serde_json::to_string(aliases).unwrap();
    let emails = serde_json::to_string(emails).unwrap();
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO persons (
                id, display_name, aliases_json, calendar_emails_json,
                created_at_utc_ms, updated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, 1, 1)",
            params![id.uuid().to_string(), name, aliases, emails],
        )
        .unwrap();
    id
}

pub(crate) fn link(
    store: &MeetingStore,
    meeting_id: MeetingSessionId,
    person_id: PersonId,
    source: &str,
    confidence: &str,
) {
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_person_links (
                meeting_id, person_id, source, confidence, created_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, 1)",
            params![
                meeting_id.uuid().to_string(),
                person_id.uuid().to_string(),
                source,
                confidence
            ],
        )
        .unwrap();
}

fn calendar_link(
    store: &MeetingStore,
    person_id: PersonId,
    email: &str,
    at_utc_ms: i64,
) -> MeetingSessionId {
    let meeting_id = meeting(store, email, at_utc_ms);
    store
        .remember_calendar_facts(
            meeting_id,
            &CalendarEventSummary {
                event_key: format!("{email}-{at_utc_ms}"),
                series_key: String::new(),
                title: email.to_string(),
                attendee_count: 2,
                start_utc_ms: at_utc_ms,
                end_utc_ms: at_utc_ms + 1,
                attendees: vec![CalendarAttendee {
                    name: email.to_string(),
                    email: Some(email.to_string()),
                    status: ParticipationStatus::Accepted,
                    is_self: false,
                }],
                notes: None,
                calendar_name: None,
                url: None,
            },
        )
        .unwrap();
    link(store, meeting_id, person_id, "calendar", "confirmed");
    meeting_id
}

fn artifact(store: &MeetingStore, meeting_id: MeetingSessionId, headline: &str) {
    let revision_id = Uuid::new_v4();
    let artifact_id = Uuid::new_v4();
    let content = serde_json::json!({
        "summary": {"text": headline, "citations": []},
        "outline": [],
        "decisions": [],
        "action_items": [],
        "key_questions": [],
        "risks": [],
        "follow_up_draft": {"text": "", "citations": []},
        "ledger": {
            "headline": headline,
            "threads": [{
                "topic": "Ship the integration",
                "state": "open",
                "substantive": true,
                "receipt": {"quote": "", "speaker": null, "t_ms": 0, "citations": []},
                "owner": "Alice Doe"
            }],
            "open_loops": [],
            "commitments": [{
                "who": "Alice Doe",
                "what": "Send the draft",
                "firmness": "firm",
                "receipt": {"quote": "", "speaker": null, "t_ms": 0, "citations": []}
            }],
            "stances": [],
            "caveats": [],
            "receipts": {"status": "verified"}
        }
    });
    let connection = store.connection().unwrap();
    connection
        .execute(
            "INSERT INTO meeting_transcript_revisions (
                transcript_revision_id, session_id, engine_id, destination_json,
                source_set_json, language, state, created_at_utc_ms
             ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 1)",
            params![revision_id.to_string(), meeting_id.uuid().to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meeting_artifact_revisions (
                artifact_id, session_id, transcript_revision_id, input_revision,
                template_id, template_version, generation_key, state,
                content_json, generated_at_utc_ms
             ) VALUES (?1, ?2, ?3, 0, 'test', 1, ?4, 'current', ?5, 1)",
            params![
                artifact_id.to_string(),
                meeting_id.uuid().to_string(),
                revision_id.to_string(),
                format!("test-{artifact_id}"),
                content.to_string()
            ],
        )
        .unwrap();
}

/// The current artifact revision of a meeting, from content the test wrote,
/// generated against the meeting's transcript revision when it has one.
/// [`artifact`] is one fixed ledger; this is for a test whose subject is the
/// content itself.
pub(crate) fn current_artifact(
    store: &MeetingStore,
    meeting_id: MeetingSessionId,
    content: &serde_json::Value,
    generated_at_utc_ms: i64,
) {
    // PANIC: a fixture that cannot reach the store is a broken test.
    let connection = store.connection().unwrap();
    // PANIC: the meeting row was written by [`meeting`] before this is called.
    let transcript_revision_id: Option<String> = connection
        .query_row(
            "SELECT current_transcript_revision_id FROM meeting_sessions WHERE id = ?1",
            params![meeting_id.uuid().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    // PANIC: a fixture that cannot write its rows is a broken test.
    let transcript_revision_id = transcript_revision_id.unwrap_or_else(|| {
        let revision_id = Uuid::new_v4().to_string();
        // PANIC: as above; the revision row is the fixture's own.
        connection
            .execute(
                "INSERT INTO meeting_transcript_revisions (
                    transcript_revision_id, session_id, engine_id, destination_json,
                    source_set_json, language, state, created_at_utc_ms
                 ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 1)",
                params![revision_id, meeting_id.uuid().to_string()],
            )
            .unwrap();
        revision_id
    });
    let artifact_id = Uuid::new_v4();
    // PANIC: as above; the artifact row is the fixture's own.
    connection
        .execute(
            "INSERT INTO meeting_artifact_revisions (
                artifact_id, session_id, transcript_revision_id, input_revision,
                template_id, template_version, generation_key, state,
                content_json, generated_at_utc_ms
             ) VALUES (?1, ?2, ?3, 0, 'test', 1, ?4, 'current', ?5, ?6)",
            params![
                artifact_id.to_string(),
                meeting_id.uuid().to_string(),
                transcript_revision_id,
                format!("test-{artifact_id}"),
                content.to_string(),
                generated_at_utc_ms
            ],
        )
        .unwrap();
}
/// The committed receipts for one command in the shared encrypted test store.
pub(crate) fn committed_receipt_count(
    store: &MeetingStore,
    command: crate::meeting::types::MeetingCommandKind,
) -> usize {
    let connection = store.connection().unwrap();
    let mut statement = connection
        .prepare("SELECT receipt_json FROM meeting_operation_receipts")
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| {
            serde_json::from_str::<crate::meeting::types::OperationReceipt>(&row.unwrap()).unwrap()
        })
        .filter(|receipt| {
            receipt.command == command
                && receipt.result == crate::meeting::types::OperationResult::Committed
        })
        .count()
}
pub(crate) fn transcript(store: &MeetingStore, meeting_id: MeetingSessionId, text: &str) {
    transcript_segments(store, meeting_id, &[text]);
}

/// Several things one speaker said, a second apart, on a microphone track a
/// review snapshot can read back: `health` is a JSON column, so it holds a
/// JSON string.
pub(crate) fn transcript_segments(
    store: &MeetingStore,
    meeting_id: MeetingSessionId,
    texts: &[&str],
) {
    let plan_id = Uuid::new_v4();
    let track_id = Uuid::new_v4();
    let speaker_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();
    let connection = store.connection().unwrap();
    connection
        .execute(
            "INSERT INTO meeting_run_plans (
                plan_id, session_id, attempt_number, schema_version, consent_id,
                canonical_plan_json, created_at_utc_ms
             ) VALUES (?1, ?2, 1, 1, ?3, '{}', 1)",
            params![
                plan_id.to_string(),
                meeting_id.uuid().to_string(),
                Uuid::new_v4().to_string()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meeting_source_tracks (
                track_id, session_id, plan_id, source_kind, required, requested,
                descriptor_json, timestamp_bridge_json, health
             ) VALUES (?1, ?2, ?3, 'microphone', 1, 1, '{}', '{}', '\"healthy\"')",
            params![
                track_id.to_string(),
                meeting_id.uuid().to_string(),
                plan_id.to_string()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meeting_speakers (
                speaker_id, session_id, source_kind, display_name, revision
             ) VALUES (?1, ?2, 'microphone', 'Speaker 1', 0)",
            params![speaker_id.to_string(), meeting_id.uuid().to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meeting_transcript_revisions (
                transcript_revision_id, session_id, engine_id, destination_json,
                source_set_json, language, state, created_at_utc_ms
             ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 1)",
            params![revision_id.to_string(), meeting_id.uuid().to_string()],
        )
        .unwrap();
    // PANIC: a fixture that cannot write its rows is a broken test.
    for (ordinal, text) in (0u64..).zip(texts) {
        // PANIC: as above; each segment row is the fixture's own.
        connection
            .execute(
                "INSERT INTO meeting_transcript_segments (
                    segment_id, transcript_revision_id, track_id, ordinal,
                    start_offset_ns, end_offset_ns, speaker_id, base_text
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    Uuid::new_v4().to_string(),
                    revision_id.to_string(),
                    track_id.to_string(),
                    ordinal,
                    ordinal * 1_000_000_000,
                    ordinal * 1_000_000_000 + 1,
                    speaker_id.to_string(),
                    text
                ],
            )
            .unwrap();
    }
    connection
        .execute(
            "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
            params![revision_id.to_string(), meeting_id.uuid().to_string()],
        )
        .unwrap();
}

pub(crate) fn event(
    kind: WorkflowEventKind,
    payload: serde_json::Value,
    dedupe_key: &str,
) -> NewWorkflowEvent {
    NewWorkflowEvent {
        kind,
        payload,
        occurred_at_utc_ms: 10,
        source: "test",
        dedupe_key: dedupe_key.to_string(),
    }
}

#[test]
fn link_derivation_respects_evidence_strength() {
    let (_directory, store) = store();
    let calendar_meeting = meeting(&store, "Calendar", 1);
    let speaker_meeting = meeting(&store, "Speaker", 2);
    let title_meeting = meeting(&store, "1:1 with Charlie", 3);
    let calendar_person = person(&store, "Alice Doe", &[], &["alice@example.com"]);
    let speaker_person = person(&store, "Bob Smith", &[], &[]);
    let title_person = person(&store, "Charlie Brown", &[], &[]);
    let attendee = CalendarAttendee {
        name: "Alice Doe".to_string(),
        email: Some("ALICE@example.com".to_string()),
        status: ParticipationStatus::Accepted,
        is_self: false,
    };
    let connection = store.connection().unwrap();
    derive_calendar_links_in(&connection, calendar_meeting, &[attendee], 10).unwrap();
    derive_speaker_link_in(&connection, speaker_meeting, "Bob Smith", 10).unwrap();
    derive_title_links_in(&connection, title_meeting, "1:1 with Charlie", 10).unwrap();
    for (meeting_id, person_id, source, confidence) in [
        (calendar_meeting, calendar_person, "calendar", "confirmed"),
        (speaker_meeting, speaker_person, "speaker", "confirmed"),
        (title_meeting, title_person, "title", "suggested"),
    ] {
        let row: (String, String) = connection
            .query_row(
                "SELECT source, confidence FROM meeting_person_links
                  WHERE meeting_id = ?1 AND person_id = ?2",
                params![meeting_id.uuid().to_string(), person_id.uuid().to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (source.to_string(), confidence.to_string()));
    }
    assert_eq!(
        derive_speaker_link_in(&connection, speaker_meeting, "Bob", 10).unwrap(),
        0
    );
}

#[test]
fn people_organization_derivation_strips_public_mail_and_reads_registrable_labels() {
    let (_directory, store) = store();
    let cases = [
        ("person@acme.com", Some("Acme")),
        ("person@eu.acme.co.uk", Some("Acme")),
        ("person@gmail.com", None),
        ("person@googlemail.com", None),
        ("person@outlook.com", None),
        ("person@hotmail.com", None),
        ("person@live.com", None),
        ("person@icloud.com", None),
        ("person@me.com", None),
        ("person@yahoo.co.uk", None),
        ("person@proton.me", None),
        ("person@pm.me", None),
        ("person@aol.com", None),
        ("person@fastmail.com", None),
        ("person@gmx.de", None),
        ("person@yandex.ru", None),
        ("person@zoho.com", None),
    ];
    let mut people = Vec::new();
    for (index, (email, expected)) in cases.into_iter().enumerate() {
        let person_id = person(&store, email, &[], &[email]);
        calendar_link(&store, person_id, email, i64::try_from(index).unwrap() + 1);
        people.push((person_id, expected));
    }
    let no_email = person(&store, "No calendar address", &[], &[]);
    people.push((no_email, None));

    let connection = store.connection().unwrap();
    assert_eq!(recompute_organizations_in(&connection).unwrap(), 2);
    drop(connection);
    for (person_id, expected) in &people {
        assert_eq!(
            store
                .person_detail(*person_id)
                .unwrap()
                .detail
                .person
                .organization
                .as_deref(),
            *expected
        );
    }
    let organizations = store
        .organizations_for_person_ids(
            &people
                .iter()
                .map(|(person_id, _)| *person_id)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(organizations.len(), 2);
    assert!(organizations.values().all(|value| value == "Acme"));
}

#[test]
fn people_organization_recompute_prefers_frequency_then_newest_and_is_idempotent() {
    let (_directory, store) = store();
    let person_id = person(
        &store,
        "Dana Reyes",
        &[],
        &["dana@acme.com", "dana@beta.io"],
    );
    calendar_link(&store, person_id, "dana@acme.com", 10);
    calendar_link(&store, person_id, "dana@beta.io", 20);

    let connection = store.connection().unwrap();
    assert_eq!(recompute_organizations_in(&connection).unwrap(), 1);
    drop(connection);
    assert_eq!(
        store
            .person_detail(person_id)
            .unwrap()
            .detail
            .person
            .organization
            .as_deref(),
        Some("Beta")
    );

    calendar_link(&store, person_id, "dana@acme.com", 30);
    let connection = store.connection().unwrap();
    assert_eq!(recompute_organizations_in(&connection).unwrap(), 1);
    assert_eq!(recompute_organizations_in(&connection).unwrap(), 0);
    drop(connection);
    assert_eq!(
        store
            .person_detail(person_id)
            .unwrap()
            .detail
            .person
            .organization
            .as_deref(),
        Some("Acme")
    );
}

/* An organization page is the union of its people's pages, so what it has to
 * get right is the union: everybody who carries the label, every meeting once
 * however many of them were in it, and nobody from anywhere else. */
#[test]
fn organization_detail_unions_its_people_and_counts_a_shared_meeting_once() {
    let (_directory, store) = store();
    let alice = person(&store, "Alice Doe", &[], &["alice@acme.com"]);
    let dana = person(&store, "Dana Reyes", &[], &["dana@acme.com"]);
    let outsider = person(&store, "Bob Stone", &[], &["bob@beta.io"]);
    let shared = calendar_link(&store, alice, "alice@acme.com", 10);
    link(&store, shared, dana, "calendar", "confirmed");
    let later = calendar_link(&store, dana, "dana@acme.com", 20);
    calendar_link(&store, outsider, "bob@beta.io", 30);
    artifact(&store, shared, "Pricing is still open.");
    let connection = store.connection().unwrap();
    recompute_organizations_in(&connection).unwrap();
    drop(connection);

    let detail = store.organization_detail("acme").unwrap().detail;

    assert_eq!(detail.name, "Acme");
    assert_eq!(
        detail
            .people
            .iter()
            .map(|entry| entry.person.display_name.as_str())
            .collect::<Vec<_>>(),
        ["Alice Doe", "Dana Reyes"],
        "everybody carrying the label, and nobody from Beta"
    );
    assert_eq!(
        detail
            .recent_meetings
            .iter()
            .map(|meeting| meeting.id)
            .collect::<Vec<_>>(),
        [later, shared],
        "newest first, and the meeting both of them were in appears once"
    );
    assert_eq!(
        detail
            .open_loops
            .iter()
            .map(|open_loop| open_loop.text.as_str())
            .collect::<Vec<_>>(),
        ["Ship the integration"],
        "what is open with anybody here"
    );
}

/* The label a person's header shows and the slug a `sona://organization/…`
 * link carries are the same lookup key, because both are slugified on the way
 * in. A label nobody carries is a missing page rather than an empty one. */
#[test]
fn organization_detail_answers_to_the_label_and_refuses_an_unknown_one() {
    let (_directory, store) = store();
    let dana = person(&store, "Dana Reyes", &[], &["dana@acme.com"]);
    calendar_link(&store, dana, "dana@acme.com", 10);
    let connection = store.connection().unwrap();
    recompute_organizations_in(&connection).unwrap();
    drop(connection);

    for key in ["Acme", "acme", " ACME "] {
        assert_eq!(store.organization_detail(key).unwrap().detail.name, "Acme");
    }
    assert!(matches!(
        store.organization_detail("beta"),
        Err(StoreError::NotFound)
    ));
}

/* A relationship paragraph is a projection, not identity: it lands on the row
 * with its engine and its clock, and it does not move the fence a rename is
 * halfway through using. */
#[test]
fn a_person_summary_is_stored_with_its_engine_and_leaves_the_fence_alone() {
    let (_directory, store) = store();
    let person_id = person(&store, "Dana Reyes", &[], &[]);
    let before = store.people_list().unwrap().revision;

    store
        .set_person_summary(
            person_id,
            crate::meeting::people_types::PersonSummary {
                text: "Dana runs pricing.".to_string(),
                generated_at_utc_ms: 4_218,
                model_id: "apple-intelligence".to_string(),
            },
        )
        .unwrap();

    let summary = store
        .person_detail(person_id)
        .unwrap()
        .detail
        .person
        .summary
        .expect("the paragraph is on the row");
    assert_eq!(summary.text, "Dana runs pricing.");
    assert_eq!(summary.generated_at_utc_ms, 4_218);
    assert_eq!(summary.model_id, "apple-intelligence");
    assert_eq!(store.people_list().unwrap().revision, before);
    assert!(matches!(
        store.set_person_summary(
            PersonId::new(),
            crate::meeting::people_types::PersonSummary {
                text: "Nobody.".to_string(),
                generated_at_utc_ms: 1,
                model_id: "apple-intelligence".to_string(),
            },
        ),
        Err(StoreError::NotFound)
    ));
}

/* The people confirmed in one meeting: what the artifact pass reads to know
 * whose paragraph the meeting it just finished changed. A guess is not a
 * person to write a summary under. */
#[test]
fn person_ids_for_meeting_keeps_confirmed_links_only() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Review", 1);
    let confirmed = person(&store, "Dana Reyes", &[], &[]);
    let suggested = person(&store, "Amir Khan", &[], &[]);
    link(&store, meeting_id, confirmed, "calendar", "confirmed");
    link(&store, meeting_id, suggested, "title", "suggested");

    assert_eq!(
        store.person_ids_for_meeting(meeting_id).unwrap(),
        vec![confirmed]
    );
}

#[test]
fn meeting_deletion_cascades_links_without_deleting_people() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Cascade", 1);
    let person_id = person(&store, "Alice Doe", &[], &[]);
    link(&store, meeting_id, person_id, "manual", "confirmed");
    let connection = store.connection().unwrap();
    connection
        .execute(
            "DELETE FROM meeting_sessions WHERE id = ?1",
            [meeting_id.uuid().to_string()],
        )
        .unwrap();
    let links: i64 = connection
        .query_row("SELECT COUNT(*) FROM meeting_person_links", [], |row| {
            row.get(0)
        })
        .unwrap();
    let people: i64 = connection
        .query_row("SELECT COUNT(*) FROM persons", [], |row| row.get(0))
        .unwrap();
    assert_eq!((links, people), (0, 1));
}

#[test]
fn merge_repoints_links_and_unions_identity() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Merge", 1);
    let source = person(&store, "Alice Jones", &["AJ"], &["aj@example.com"]);
    let target = person(&store, "Alice Doe", &["Al"], &["alice@example.com"]);
    link(&store, meeting_id, source, "speaker", "confirmed");
    store
        .merge_persons_with_voice_resolution(source, target, 0, None, 10)
        .unwrap();
    let detail = store.person_detail(target).unwrap().detail;
    assert!(detail.person.aliases.contains(&"Alice Jones".to_string()));
    assert!(detail.person.aliases.contains(&"AJ".to_string()));
    assert!(detail
        .person
        .calendar_emails
        .contains(&"aj@example.com".to_string()));
    assert_eq!(detail.links.len(), 1);
    assert_eq!(detail.links[0].source, PersonLinkSource::Speaker);
    assert_eq!(detail.links[0].confidence, PersonLinkConfidence::Confirmed);
    assert!(matches!(
        store.person_detail(source),
        Err(StoreError::NotFound)
    ));
}

#[test]
fn context_degrades_to_counts_and_adds_ledger_facts_when_present() {
    let (_directory, store) = store();
    let person_id = person(&store, "Alice Doe", &[], &[]);
    let old = meeting(&store, "Old", 1);
    let recent = meeting(&store, "Recent", 2);
    link(&store, old, person_id, "manual", "confirmed");
    link(&store, recent, person_id, "manual", "confirmed");
    artifact(&store, recent, "Latest headline");
    let row = store.person_context(&[person_id]).unwrap().rows.remove(0);
    assert_eq!(row.meetings_count, 2);
    assert_eq!(
        row.last.unwrap().headline.as_deref(),
        Some("Latest headline")
    );
    assert_eq!(row.open_loops[0].text, "Ship the integration");
    assert_eq!(row.commitments[0].text, "Send the draft");
}

#[test]
fn inbox_omits_loops_after_their_meeting_is_deleted() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Inbox", 1);
    artifact(&store, meeting_id, "Open work");
    store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingFinalized,
                serde_json::json!({
                    "session_id": meeting_id.uuid().to_string(),
                    "known_vocabulary": []
                }),
                "inbox-finalized",
            ),
            &inputs(),
        )
        .unwrap();
    assert_eq!(store.open_loops_inbox(5).unwrap().entries.len(), 1);
    let workflow_revision = store.workflows_list().unwrap().revision;
    let disabled = store
        .set_workflow_enabled(WorkflowId::Continuity, false, workflow_revision)
        .unwrap();
    assert!(store.open_loops_inbox(5).unwrap().entries.is_empty());
    store
        .set_workflow_enabled(WorkflowId::Continuity, true, disabled.revision)
        .unwrap();
    assert_eq!(store.open_loops_inbox(5).unwrap().entries.len(), 1);
    store
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM meeting_sessions WHERE id = ?1",
            [meeting_id.uuid().to_string()],
        )
        .unwrap();
    assert!(store.open_loops_inbox(5).unwrap().entries.is_empty());
}

#[test]
fn calendar_context_matches_known_people_by_exact_email() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Prior", 1);
    let person_id = person(&store, "Alice Doe", &[], &["alice@example.com"]);
    link(&store, meeting_id, person_id, "calendar", "confirmed");
    let rows = calendar_context_in(
        &store.connection().unwrap(),
        &[
            CalendarAttendee {
                name: "Alice Doe".to_string(),
                email: Some("ALICE@example.com".to_string()),
                status: ParticipationStatus::Accepted,
                is_self: false,
            },
            CalendarAttendee {
                name: "Current User".to_string(),
                email: Some("self@example.com".to_string()),
                status: ParticipationStatus::Accepted,
                is_self: true,
            },
        ],
    )
    .unwrap()
    .rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].person_id, person_id);
    assert_eq!(rows[0].meetings_count, 1);
}

#[test]
fn person_split_moves_only_selected_evidence_without_data_loss() {
    let (_directory, store) = store();
    let source_id = person(
        &store,
        "Combined Person",
        &["Alice Alias"],
        &["alice@example.com"],
    );
    let first = meeting(&store, "First", 10);
    let second = meeting(&store, "Second", 20);
    link(&store, first, source_id, "manual", "confirmed");
    link(&store, second, source_id, "manual", "confirmed");
    artifact(&store, first, "First headline");
    let document = store
        .ingest_document(
            MeetingOperationId::new(),
            "Plan".to_string(),
            "plan.md".to_string(),
            "text/markdown".to_string(),
            "Private notes".to_string(),
            30,
        )
        .unwrap()
        .document
        .unwrap();
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO document_person_links(document_id, person_id, created_at_utc_ms)
             VALUES (?1, ?2, 30)",
            params![
                document.summary.id.uuid().to_string(),
                source_id.uuid().to_string()
            ],
        )
        .unwrap();

    let revision = store.people_list().unwrap().revision;
    let result = store
        .split_person(
            PersonSplitRequest {
                source_person_id: source_id,
                target: PersonSplitTarget::Create {
                    display_name: "Alice Doe".to_string(),
                },
                meeting_ids: vec![first, second],
                aliases: vec!["Alice Alias".to_string()],
                calendar_emails: vec!["alice@example.com".to_string()],
                document_ids: vec![document.summary.id],
                expected_revision: revision,
            },
            40,
        )
        .unwrap();
    let target = result.person.unwrap();

    let source = store.person_detail(source_id).unwrap().detail;
    let target_detail = store.person_detail(target.id).unwrap().detail;
    assert!(source.person.aliases.is_empty());
    assert!(source.person.calendar_emails.is_empty());
    assert!(source.links.is_empty());
    assert!(source.documents.is_empty());
    assert_eq!(target_detail.person.aliases, vec!["Alice Alias"]);
    assert_eq!(
        target_detail.person.calendar_emails,
        vec!["alice@example.com"]
    );
    assert_eq!(target_detail.links.len(), 2);
    assert_eq!(target_detail.documents.len(), 1);

    let list_entry = store
        .people_list()
        .unwrap()
        .entries
        .into_iter()
        .find(|entry| entry.person.id == target.id)
        .unwrap();
    assert_eq!(list_entry.confirmed_count, 2);
    assert_eq!(list_entry.suggested_count, 0);
    assert_eq!(list_entry.evidence_sources, vec![PersonLinkSource::Manual]);
    assert_eq!(list_entry.last_meeting.unwrap().session_id, second);

    let context = store.meeting_people_context(second).unwrap();
    assert_eq!(context.rows.len(), 1);
    assert_eq!(context.rows[0].person_id, target.id);
    assert_eq!(context.rows[0].prior_meetings, 1);
    assert_eq!(
        context.rows[0].last_prior_meeting.as_ref().unwrap().id,
        first
    );
    assert_eq!(
        context.rows[0].top_open_loop.as_ref().unwrap().text,
        "Ship the integration"
    );
}

/// The band on a meeting describes what came before it. The oldest of three
/// meetings has no earlier meeting to cite, and the loop raised in a later
/// one is not "still open" from its point of view.
#[test]
fn previously_together_reads_only_meetings_before_this_one() {
    let (_directory, store) = store();
    let person_id = person(&store, "Alice Doe", &[], &[]);
    let oldest = meeting(&store, "Oldest", 10);
    let middle = meeting(&store, "Middle", 20);
    let newest = meeting(&store, "Newest", 30);
    for meeting_id in [oldest, middle, newest] {
        link(&store, meeting_id, person_id, "manual", "confirmed");
    }
    artifact(&store, newest, "Newest headline");

    let context = store.meeting_people_context(oldest).unwrap();
    assert_eq!(context.rows.len(), 1);
    assert_eq!(context.rows[0].prior_meetings, 0);
    assert!(context.rows[0].last_prior_meeting.is_none());
    assert!(context.rows[0].top_open_loop.is_none());

    let context = store.meeting_people_context(newest).unwrap();
    assert_eq!(context.rows[0].prior_meetings, 2);
    assert_eq!(
        context.rows[0].last_prior_meeting.as_ref().unwrap().id,
        middle
    );
    assert!(context.rows[0].top_open_loop.is_none());
}

/// Renaming your own voice must not mint a contact out of you.
///
/// Two places state the invariant. `loop_types::MeetingLoopDirection` says
/// "Sona models no `Person` for its own user … a `PersonId` is always somebody
/// else", and `store::loops::microphone_speaker_labels_in` says the microphone
/// track is the whole answer to which voice is the user's. So the one speaker
/// row a rename must never derive a person from is the microphone one — and
/// `Local speaker`, the display name the store writes for that row itself,
/// clears the two-word bar the derivation uses to reject a bare first name.
///
/// The consequence is not cosmetic. A person for the user gets a `PersonId`,
/// and `loop_direction` treats an explicit `owner_person_id` as settling the
/// question *because a person is always somebody else* — so assigning your own
/// commitment to yourself flips it from `Mine` to `WaitingOn`.
#[test]
fn speaker_derivation_refuses_the_users_own_microphone_voice() {
    let (_directory, store) = store();
    // Every session first: `meeting` takes its own connection, and holding one
    // across that call deadlocks the pool.
    let placeholder = meeting(&store, "Local notes", 1);
    let renamed = meeting(&store, "Local notes", 2);
    let remote = meeting(&store, "Pricing review", 3);
    let connection = store.connection().unwrap();

    // The store's own placeholder, and the real name a person renames it to.
    for (session, label) in [(placeholder, "Local speaker"), (renamed, "Aktan Azat")] {
        speaker(&connection, session, SourceKind::Microphone, label);
        assert_eq!(
            derive_speaker_link_in(&connection, session, label, 10).unwrap(),
            0,
            "{label:?} minted a person out of the user"
        );
    }
    assert_eq!(all_people_in(&connection).unwrap().len(), 0);

    // The other side of the conversation still lands, which is what the
    // derivation is for.
    speaker(
        &connection,
        remote,
        SourceKind::SystemAudio,
        "Stephen Kowalski",
    );
    assert!(derive_speaker_link_in(&connection, remote, "Stephen Kowalski", 10).unwrap() > 0);
    // And the bar the guard was originally written for still holds.
    assert_eq!(
        derive_speaker_link_in(&connection, remote, "Stephen", 10).unwrap(),
        0
    );
    assert_eq!(
        all_people_in(&connection)
            .unwrap()
            .into_iter()
            .map(|person| person.display_name)
            .collect::<Vec<_>>(),
        vec!["Stephen Kowalski".to_string()]
    );
}

fn speaker(
    connection: &Connection,
    session_id: MeetingSessionId,
    source_kind: SourceKind,
    display_name: &str,
) {
    // PANIC: each caller creates `session_id` through `meeting` before opening this fixture connection.
    connection
        .execute(
            "INSERT INTO meeting_speakers (
                speaker_id, session_id, source_kind, display_name, revision, merged_into_speaker_id
             ) VALUES (?1, ?2, ?3, ?4, 0, NULL)",
            params![
                Uuid::new_v4().to_string(),
                session_id.uuid().to_string(),
                source_kind.as_str(),
                display_name,
            ],
        )
        .unwrap();
}
