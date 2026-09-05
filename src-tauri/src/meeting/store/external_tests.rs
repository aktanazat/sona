//! The external plane over a corpus that holds one of every noun.
//!
//! `crate::query::external::tests` owns the half that needs no store: which
//! request a command line names, which flag combinations are refused, and the
//! refusal an agent gets when external access is off. What needs a corpus is
//! everything here — that each verb reads what it claims to, and that the JSON
//! an outside reader receives has the fields the module documents and no
//! others. So the assertions are whole-value comparisons rather than field
//! spot-checks: a projection that quietly grows a field is a wire change, and
//! this is the test that says so.
//!
//! The fixture is built the way the app builds it. A typed note is what puts a
//! meeting in the search index and what mints its receipt; the continuity
//! workflow is what makes its ledger rows reachable corpus-wide; and the one
//! `done` row got there through the same `resolve_loop` the review screen
//! calls, rather than a hand-written state row claiming to be the result.

use super::workflow_core_tests::{event, inputs, person, store};
use super::MeetingStore;
use crate::cli::CliArgs;
use crate::meeting::detection::machine::CalendarEventSummary;
use crate::meeting::loop_types::{
    MeetingLoopId, MeetingLoopKind, MeetingLoopReopenRequest, MeetingLoopResolution,
    MeetingLoopResolveRequest, MeetingLoopRow, MeetingLoopStatus,
};
use crate::meeting::people_types::PersonId;
use crate::meeting::session::{MeetingSessionManager, NoCaptureSources};
use crate::meeting::types::{
    ManualNote, ManualNoteId, MeetingCommandKind, MeetingOperationId, MeetingSessionId,
    OperationActor, OperationResult, ProcessingFailure, ProcessingStatus, SourceKind, SpeakerId,
};
use crate::meeting::workflow_types::WorkflowEventKind;
use crate::query::external::{
    loops_page, meeting_detail, meetings_page, people_page, resolve_loop,
    transcript as transcript_of, AllowedRequest, ExternalConsent, ExternalErrorCode,
    ExternalLoopSide, ExternalLoopStatus, ExternalRequest, ExternalResponse,
    EXTERNAL_ACCESS_SETTING_PATH, EXTERNAL_MUTATIONS_SETTING_PATH,
};
use crate::query::{QueryEventsPage, QueryPageReason, QuerySearchPage, QUERY_SCHEMA_VERSION};
use crate::secrets::{MemorySecretBackend, SecretManager};
use clap::Parser;
use rusqlite::params;
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

const NOW: i64 = 1_700_000_000_000;
const EARLIER: i64 = NOW - 3 * 24 * 60 * 60 * 1_000;

const TITLE: &str = "Pricing review";
const OLDER_TITLE: &str = "Retro";
const SEGMENT: &str = "Dana asked which tier the trial converts into.";
const NOTE: &str = "Dana still owes the tier comparison.";
const SUMMARY: &str = "Pricing stayed open.";
const HEADLINE: &str = "Dana's tier question stayed open.";
const THREAD: &str = "Dana's trial conversion tier";
const COMMITMENT: &str = "Send the tier comparison";
/// The display name the transcript fixture gives this machine's microphone
/// speaker. A ledger row it owns is one the user owes.
const ME: &str = "Speaker 1";

struct Corpus {
    directory: TempDir,
    secrets: Arc<SecretManager>,
    store: Arc<MeetingStore>,
    session_id: MeetingSessionId,
    older_id: MeetingSessionId,
    person_id: PersonId,
    /// The thread Dana still owes: open, and waiting on somebody else.
    open_loop: MeetingLoopId,
    /// The commitment the user made and then ticked off.
    done_commitment: MeetingLoopId,
    /// When `resolve_loop` wrote that tick. The store clocks its own writes,
    /// so the fixture reads the number back rather than claiming one.
    resolved_at_utc_ms: i64,
}

fn corpus() -> Corpus {
    let directory = TempDir::new().unwrap();
    let secrets = Arc::new(SecretManager::with_backend(Arc::new(
        MemorySecretBackend::new(),
    )));
    let key = tauri::async_runtime::block_on(secrets.meeting_storage_key()).unwrap();
    let store = MeetingStore::open(directory.path().join("meetings"), key).unwrap();
    let session_id = meeting(&store, TITLE, NOW);
    transcript(&store, session_id, SEGMENT);
    // Before the artifact, deliberately: a note marks the current artifact out
    // of date, and a ledger nobody can read is a ledger with no loops.
    note(&store, session_id);
    artifact(&store, session_id);
    finalize(&store, session_id);
    // A second meeting with nothing in it, so the list has a row whose one
    // line is genuinely absent and a window has something to exclude.
    let older_id = meeting(&store, OLDER_TITLE, EARLIER);
    let person_id = person(&store, "Dana Reyes", &["Dana"], &["dana@example.com"]);

    let open_loop = MeetingLoopId::derive(session_id, MeetingLoopKind::Loop, THREAD);
    let done_commitment =
        MeetingLoopId::derive(session_id, MeetingLoopKind::Commitment, COMMITMENT);
    let resolved = store
        .resolve_loop(
            MeetingLoopResolveRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: done_commitment.clone(),
                expected_revision: 0,
                resolution: MeetingLoopResolution::Done,
            },
            OperationActor::User,
            NOW,
        )
        .unwrap();
    let resolved_at_utc_ms = resolved
        .loops
        .rows
        .iter()
        .find(|row| row.loop_id == done_commitment)
        .and_then(|row| row.resolved_at_utc_ms)
        .expect("the commitment is ticked off");

    Corpus {
        directory,
        secrets,
        store,
        session_id,
        older_id,
        person_id,
        open_loop,
        done_commitment,
        resolved_at_utc_ms,
    }
}

/// One review-ready meeting the app could have written.
///
/// Local to this file rather than borrowed from `workflow_core_tests`: that
/// fixture's sessions carry column values the loops and people reads never
/// decode (`processing_status = 'pending'`, `health = 'healthy'`), and every
/// verb here goes through `list_sessions` or `review_snapshot`, which do.
fn meeting(store: &MeetingStore, title: &str, at_utc_ms: i64) -> MeetingSessionId {
    let session_id = MeetingSessionId::new();
    let plan_id = Uuid::new_v4();
    let connection = store.connection().unwrap();
    connection
        .execute(
            "INSERT INTO meeting_sessions (
                id, phase, revision, title, origin_kind, preflight_json,
                created_at_utc_ms, started_at_utc_ms, processing_status,
                retention_policy_json
             ) VALUES (?1, 'review_ready', 0, ?2, 'manual', '{}', ?3, ?3,
                       '{\"kind\":\"succeeded\"}', '{\"kind\":\"keep_forever\"}')",
            params![session_id.uuid().to_string(), title, at_utc_ms],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meeting_run_plans (
                plan_id, session_id, attempt_number, schema_version, consent_id,
                canonical_plan_json, created_at_utc_ms
             ) VALUES (?1, ?2, 1, 1, ?3, '{}', ?4)",
            params![
                plan_id.to_string(),
                session_id.uuid().to_string(),
                Uuid::new_v4().to_string(),
                at_utc_ms
            ],
        )
        .unwrap();
    for source_kind in [SourceKind::Microphone, SourceKind::SystemAudio] {
        connection
            .execute(
                "INSERT INTO meeting_source_tracks (
                    track_id, session_id, plan_id, source_kind, required, requested,
                    descriptor_json, timestamp_bridge_json, health
                 ) VALUES (?1, ?2, ?3, ?4, 1, 1, '{}', '{}', '\"healthy\"')",
                params![
                    Uuid::new_v4().to_string(),
                    session_id.uuid().to_string(),
                    plan_id.to_string(),
                    source_kind.as_str()
                ],
            )
            .unwrap();
    }
    session_id
}

/// One thing somebody said, on this machine's own microphone.
fn transcript(store: &MeetingStore, session_id: MeetingSessionId, text: &str) {
    let speaker_id = SpeakerId::new();
    let revision_id = Uuid::new_v4();
    let connection = store.connection().unwrap();
    let track_id: String = connection
        .query_row(
            "SELECT track_id FROM meeting_source_tracks
              WHERE session_id = ?1 AND source_kind = 'microphone'",
            params![session_id.uuid().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meeting_speakers (
                speaker_id, session_id, source_kind, display_name, revision
             ) VALUES (?1, ?2, 'microphone', ?3, 0)",
            params![
                speaker_id.uuid().to_string(),
                session_id.uuid().to_string(),
                ME
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meeting_transcript_revisions (
                transcript_revision_id, session_id, engine_id, destination_json,
                source_set_json, language, state, created_at_utc_ms
             ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 1)",
            params![revision_id.to_string(), session_id.uuid().to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meeting_transcript_segments (
                segment_id, transcript_revision_id, track_id, ordinal,
                start_offset_ns, end_offset_ns, speaker_id, base_text
             ) VALUES (?1, ?2, ?3, 0, 0, 1500000000, ?4, ?5)",
            params![
                Uuid::new_v4().to_string(),
                revision_id.to_string(),
                track_id,
                speaker_id.uuid().to_string(),
                text
            ],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
            params![revision_id.to_string(), session_id.uuid().to_string()],
        )
        .unwrap();
}

/// A note the reader typed, through the mutation that rebuilds the search
/// documents and records the receipt.
fn note(store: &MeetingStore, session_id: MeetingSessionId) {
    store
        .create_note(
            MeetingOperationId::new(),
            NOW,
            &ManualNote {
                note_id: ManualNoteId::new(),
                session_id,
                start_offset_ns: None,
                end_offset_ns: None,
                body: NOTE.to_string(),
                revision: 0,
                created_at_utc_ms: NOW,
                updated_at_utc_ms: NOW,
            },
            0,
        )
        .unwrap();
}

/// The current artifact revision: one thread nobody answered, and one thing
/// the user promised.
fn artifact(store: &MeetingStore, session_id: MeetingSessionId) {
    let content = json!({
        "summary": {"text": SUMMARY, "citations": []},
        "outline": [],
        "decisions": [],
        "action_items": [],
        "key_questions": [],
        "risks": [],
        "follow_up_draft": {"text": "", "citations": []},
        "ledger": {
            "headline": HEADLINE,
            "threads": [{
                "topic": THREAD,
                "state": "open",
                "substantive": true,
                "receipt": {
                    "quote": "which tier does the trial convert into",
                    "speaker": "Dana",
                    "t_ms": 12000,
                    "citations": []
                },
                "owner": "Dana Reyes"
            }],
            "open_loops": [],
            "commitments": [{
                "who": ME,
                "what": COMMITMENT,
                "firmness": "firm",
                "receipt": {
                    "quote": "I'll send the tier comparison tonight",
                    "speaker": ME,
                    "t_ms": 20000,
                    "citations": []
                }
            }],
            "stances": [],
            "caveats": [],
            "receipts": {"status": "verified"}
        }
    });
    let artifact_id = Uuid::new_v4();
    let connection = store.connection().unwrap();
    let transcript_revision_id: String = connection
        .query_row(
            "SELECT current_transcript_revision_id FROM meeting_sessions WHERE id = ?1",
            params![session_id.uuid().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meeting_artifact_revisions (
                artifact_id, session_id, transcript_revision_id, input_revision,
                template_id, template_version, generation_key, state,
                content_json, generated_at_utc_ms
             ) VALUES (?1, ?2, ?3, 0, 'test', 1, ?4, 'current', ?5, ?6)",
            params![
                artifact_id.to_string(),
                session_id.uuid().to_string(),
                transcript_revision_id,
                format!("test-{artifact_id}"),
                content.to_string(),
                NOW
            ],
        )
        .unwrap();
}

/// The continuity run. A meeting's ledger rows are only reachable corpus-wide
/// once the workflow that reads them has succeeded for it.
fn finalize(store: &MeetingStore, session_id: MeetingSessionId) {
    store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingFinalized,
                json!({
                    "session_id": session_id.uuid().to_string(),
                    "known_vocabulary": []
                }),
                "external-plane-finalized",
            ),
            &inputs(),
        )
        .unwrap();
}

fn value<T: serde::Serialize>(payload: &T) -> serde_json::Value {
    serde_json::to_value(payload).unwrap()
}

fn headless_manager(corpus: &Corpus) -> Arc<MeetingSessionManager> {
    Arc::new(MeetingSessionManager::with_parts(
        None,
        Some(corpus.directory.path().join("meetings")),
        Arc::clone(&corpus.secrets),
        Arc::new(NoCaptureSources),
    ))
}

fn loop_resolve_request(loop_id: &MeetingLoopId) -> ExternalRequest {
    let args = CliArgs::try_parse_from(["sona", "--loop-resolve", loop_id.as_str()])
        .expect("the loop flag parses");
    ExternalRequest::from_args(&args).expect("the loop flag names an external request")
}

fn external_consent(reads: bool, mutations: bool) -> ExternalConsent {
    let mut settings = crate::settings::get_default_settings();
    settings.external_query_enabled = reads;
    settings.external_mutations_enabled = mutations;
    ExternalConsent::from_settings(&settings)
}

async fn loop_row(meetings: &MeetingSessionManager, loop_id: &MeetingLoopId) -> MeetingLoopRow {
    let session_id = loop_id
        .session_id()
        .expect("the test loop id contains its session id");
    meetings
        .loops_list(session_id)
        .await
        .expect("read the test loop")
        .rows
        .into_iter()
        .find(|row| &row.loop_id == loop_id)
        .expect("the test loop remains in the ledger")
}

#[test]
fn the_meetings_list_is_newest_first_and_says_what_each_one_left() {
    let corpus = corpus();

    let page = meetings_page(&corpus.store, None, None, 25).unwrap();

    assert_eq!(
        value(&page),
        json!({
            "schema_version": QUERY_SCHEMA_VERSION,
            "has_more": false,
            "entries": [
                {
                    "id": corpus.session_id.uuid(),
                    "title": TITLE,
                    "phase": "review_ready",
                    "when_utc_ms": NOW,
                    "recorded_duration_ms": null,
                    "capture_completeness": "not_started",
                    "speakers": [ME],
                    "headline": {"kind": "ledger", "text": HEADLINE},
                    "link": format!("sona://meeting/{}", corpus.session_id.uuid()),
                },
                {
                    "id": corpus.older_id.uuid(),
                    "title": OLDER_TITLE,
                    "phase": "review_ready",
                    "when_utc_ms": EARLIER,
                    "recorded_duration_ms": null,
                    "capture_completeness": "not_started",
                    "speakers": [],
                    // Nothing was said and nothing was written, so the row says
                    // so rather than inventing a line.
                    "headline": {"kind": "none"},
                    "link": format!("sona://meeting/{}", corpus.older_id.uuid()),
                },
            ],
        })
    );
}

#[test]
fn a_bounded_list_says_there_is_more() {
    let corpus = corpus();

    let page = meetings_page(&corpus.store, None, None, 1).unwrap();

    assert_eq!(
        page.entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [corpus.session_id.uuid()]
    );
    assert!(page.has_more);
}

/// `--from`/`--to` is one window: the store's cursor is its upper bound, and
/// the lower bound cuts the tail of a newest-first page.
#[test]
fn a_window_keeps_only_the_meetings_inside_it() {
    let corpus = corpus();

    let recent = meetings_page(&corpus.store, Some(NOW - 1_000), None, 25).unwrap();
    let old = meetings_page(&corpus.store, None, Some(NOW), 25).unwrap();
    let empty = meetings_page(&corpus.store, Some(NOW + 1), None, 25).unwrap();

    assert_eq!(
        recent
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [corpus.session_id.uuid()],
        "a lower bound drops what is older than it"
    );
    assert_eq!(
        old.entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        [corpus.older_id.uuid()],
        "the upper bound is exclusive, so the meeting at that instant is out"
    );
    assert!(empty.entries.is_empty());
    assert!(!empty.has_more, "an empty window has no next page");
}

#[test]
fn one_meeting_comes_back_with_its_summary_and_every_row_it_left() {
    let corpus = corpus();

    let detail = meeting_detail(&corpus.store, corpus.session_id).unwrap();

    assert_eq!(
        value(&detail),
        json!({
            "schema_version": QUERY_SCHEMA_VERSION,
            "id": corpus.session_id.uuid(),
            "title": TITLE,
            "phase": "review_ready",
            "started_at_utc_ms": NOW,
            "processing_status": {"kind": "succeeded"},
            "speakers": [ME],
            "summary": SUMMARY,
            "headline": HEADLINE,
            "notes": [NOTE],
            "loops": [
                {
                    "id": corpus.open_loop.as_str(),
                    "meeting_id": corpus.session_id.uuid(),
                    "meeting_title": TITLE,
                    "kind": "loop",
                    "status": "open",
                    "direction": "waiting_on",
                    "text": THREAD,
                    "owner": "Dana Reyes",
                    "when_utc_ms": NOW,
                    "resolved_at_utc_ms": null,
                    "link": format!("sona://loop/{}", corpus.open_loop.as_str()),
                },
                {
                    "id": corpus.done_commitment.as_str(),
                    "meeting_id": corpus.session_id.uuid(),
                    "meeting_title": TITLE,
                    "kind": "commitment",
                    "status": "done",
                    // The microphone speaker is the user, so a row that speaker
                    // owns is one the user owes.
                    "direction": "mine",
                    "text": COMMITMENT,
                    "owner": ME,
                    "when_utc_ms": NOW,
                    "resolved_at_utc_ms": corpus.resolved_at_utc_ms,
                    "link": format!("sona://loop/{}", corpus.done_commitment.as_str()),
                },
            ],
            "link": format!("sona://meeting/{}", corpus.session_id.uuid()),
        })
    );
}

#[test]
fn an_artifactless_failed_meeting_names_its_processing_failure() {
    let corpus = corpus();
    corpus
        .store
        .set_processing_status(
            corpus.older_id,
            ProcessingStatus::Failed {
                reason: ProcessingFailure::EngineFailure,
                cause: None,
            },
        )
        .unwrap();

    let detail = meeting_detail(&corpus.store, corpus.older_id).unwrap();

    assert_eq!(detail.summary, None, "there is no artifact to summarize");
    assert_eq!(
        value(&detail)["processing_status"],
        json!({"kind": "failed", "reason": "engine_failure", "cause": null}),
        "a failed generation must not look like an unfinished empty meeting"
    );
}

#[test]
fn an_id_the_corpus_does_not_hold_is_not_found() {
    let corpus = corpus();
    let absent = MeetingSessionId::new();

    for error in [
        meeting_detail(&corpus.store, absent).unwrap_err(),
        transcript_of(&corpus.store, absent).unwrap_err(),
    ] {
        assert_eq!(error.error, ExternalErrorCode::NotFound);
        assert_eq!(error.settings_path, None, "this is not a consent problem");
    }
}

#[test]
fn a_transcript_comes_back_speaker_labeled() {
    let corpus = corpus();

    let lines = transcript_of(&corpus.store, corpus.session_id).unwrap();

    assert_eq!(
        value(&lines),
        json!({
            "schema_version": QUERY_SCHEMA_VERSION,
            "meeting_id": corpus.session_id.uuid(),
            "title": TITLE,
            "started_at_utc_ms": NOW,
            "lines": [{
                "speaker": ME,
                "start_ms": 0,
                "end_ms": 1_500,
                "text": SEGMENT,
            }],
            "link": format!("sona://meeting/{}", corpus.session_id.uuid()),
        })
    );
}

#[test]
fn a_meeting_with_nothing_said_in_it_has_no_lines() {
    let corpus = corpus();

    let lines = transcript_of(&corpus.store, corpus.older_id).unwrap();

    assert!(lines.lines.is_empty());
    assert_eq!(lines.started_at_utc_ms, Some(EARLIER));
}

#[test]
fn the_corpus_loop_list_carries_the_meeting_each_row_came_from() {
    let corpus = corpus();

    let page = loops_page(&corpus.store, None, None, None, 25).unwrap();

    assert_eq!(
        page.entries
            .iter()
            .map(|row| (row.id.as_str(), row.meeting_title.as_str(), row.when_utc_ms))
            .collect::<Vec<_>>(),
        [
            (corpus.open_loop.as_str(), TITLE, NOW),
            (corpus.done_commitment.as_str(), TITLE, NOW),
        ]
    );
    assert!(!page.has_more);
}

/// The filters emptied this page, not the corpus: the same store answers a
/// filterless `--loops` with two rows. An agent told `no_rows` here would stop
/// asking, having never seen the rows it was allowed to see.
#[test]
fn a_loop_list_its_filters_emptied_does_not_read_as_an_empty_corpus() {
    let corpus = corpus();

    let filtered = loops_page(
        &corpus.store,
        Some(ExternalLoopStatus::Open),
        Some(ExternalLoopSide::Mine),
        None,
        25,
    )
    .unwrap();
    let unfiltered = loops_page(&corpus.store, None, None, None, 25).unwrap();

    assert!(filtered.entries.is_empty());
    assert_eq!(filtered.reason, Some(QueryPageReason::FilteredOut));
    assert_eq!(
        unfiltered.reason, None,
        "a page carrying its rows has nothing to explain"
    );
}

/// Ledger rows exist and are deliberately not reportable yet: the continuity
/// pass has not matched this meeting against the one before it, so a
/// carried-forward loop would still read as open. A fresh corpus mid-workflow
/// is the common case, and it is not an empty one.
#[test]
fn a_loop_list_waiting_on_continuity_says_so_rather_than_nothing() {
    let (_directory, store) = store();
    let session_id = meeting(&store, TITLE, NOW);
    transcript(&store, session_id, SEGMENT);
    artifact(&store, session_id);

    let page = loops_page(&store, None, None, None, 25).unwrap();

    assert!(page.entries.is_empty());
    assert_eq!(page.reason, Some(QueryPageReason::AwaitingContinuity));
}

/// The one answer that means the question was fully asked, and the only one an
/// agent may stop on.
#[test]
fn a_corpus_with_no_meetings_in_it_answers_the_loop_list_with_no_rows() {
    let (_directory, store) = store();

    let page = loops_page(&store, None, None, None, 25).unwrap();

    assert!(page.entries.is_empty());
    assert_eq!(page.reason, Some(QueryPageReason::NoRows));
}

#[test]
fn a_status_or_a_side_narrows_the_loop_list() {
    let corpus = corpus();

    let cases = [
        (
            Some(ExternalLoopStatus::Open),
            None,
            vec![corpus.open_loop.as_str()],
        ),
        (
            Some(ExternalLoopStatus::Done),
            None,
            vec![corpus.done_commitment.as_str()],
        ),
        (
            None,
            Some(ExternalLoopSide::Mine),
            vec![corpus.done_commitment.as_str()],
        ),
        (
            None,
            Some(ExternalLoopSide::WaitingOn),
            vec![corpus.open_loop.as_str()],
        ),
        (
            Some(ExternalLoopStatus::Open),
            Some(ExternalLoopSide::Mine),
            vec![],
        ),
    ];

    for (status, side, expected) in cases {
        let page = loops_page(&corpus.store, status, side, None, 25).unwrap();
        assert_eq!(
            page.entries
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>(),
            expected,
            "{status:?} / {side:?}"
        );
    }
}

#[test]
fn a_bounded_loop_list_supplies_a_cursor_for_its_next_page() {
    let corpus = corpus();

    let page = loops_page(&corpus.store, None, None, None, 1).unwrap();

    assert_eq!(page.entries.len(), 1);
    assert!(page.has_more);
    assert_eq!(
        value(&page)["next_cursor"],
        json!(corpus.open_loop.as_str()),
        "has_more must name the existing loop id to resume after"
    );
    let cursor = page
        .next_cursor
        .as_deref()
        .expect("a bounded page has a cursor");
    let next = loops_page(&corpus.store, None, None, Some(cursor), 1).unwrap();
    assert_eq!(next.entries.len(), 1);
    assert_eq!(next.entries[0].id.as_str(), corpus.done_commitment.as_str());
    assert!(!next.has_more);
    assert_eq!(next.next_cursor, None);

    let stale = loops_page(&corpus.store, None, None, Some("missing"), 1).unwrap_err();
    assert_eq!(stale.error, ExternalErrorCode::NotFound);
}

#[test]
fn a_person_is_found_by_every_name_she_answers_to() {
    let corpus = corpus();

    for name in ["dana reyes", "Dana", "dana@example.com"] {
        let page = people_page(&corpus.store, name, 25).unwrap();
        assert_eq!(
            value(&page),
            json!({
                "schema_version": QUERY_SCHEMA_VERSION,
                "has_more": false,
                "reason": null,
                "entries": [{
                    "id": corpus.person_id.uuid(),
                    "display_name": "Dana Reyes",
                    "aliases": ["Dana"],
                    "calendar_emails": ["dana@example.com"],
                    "meetings_count": 0,
                    "last_meeting_at_utc_ms": null,
                    "last_meeting_title": null,
                    "last_meeting_headline": null,
                    "link": format!("sona://person/{}", corpus.person_id.uuid()),
                }],
            }),
            "{name:?}"
        );
    }
}

/// A name that matched nobody, against an index that holds somebody: the
/// lookup was answered, and spelling the name differently is the only thing
/// left to try.
#[test]
fn a_name_nobody_answers_to_finds_nobody() {
    let corpus = corpus();

    let page = people_page(&corpus.store, "steven", 25).unwrap();

    assert!(page.entries.is_empty());
    assert!(!page.has_more);
    assert_eq!(page.reason, Some(QueryPageReason::NoRows));
}

/// Diarization is what names people. Until it has run there is no index for a
/// name to miss, which is a fact about the corpus rather than about the name —
/// and the difference is whether asking again could ever work.
#[test]
fn a_corpus_that_has_named_nobody_says_its_people_index_is_empty() {
    let (_directory, store) = store();

    let page = people_page(&store, "dana", 25).unwrap();

    assert!(page.entries.is_empty());
    assert_eq!(page.reason, Some(QueryPageReason::PeopleIndexEmpty));
}

#[test]
fn a_lookup_with_no_name_in_it_is_a_usage_error() {
    let corpus = corpus();

    assert_eq!(
        people_page(&corpus.store, "   ", 25).unwrap_err().error,
        ExternalErrorCode::InvalidRequest
    );
}

/// `--query` and `--events` are the plane's own pages. This surface adds a
/// wrapper and nothing else, so the wrapper has to be invisible on the wire:
/// what an agent parses is exactly what ⌘K and the panel agent already get.
#[test]
fn the_plane_pages_are_passed_through_unchanged() {
    let page = QuerySearchPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries: Vec::new(),
        next_cursor: None,
        reason: None,
    };
    let events = QueryEventsPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries: Vec::new(),
        next_cursor: None,
    };

    assert_eq!(value(&ExternalResponse::Search(page.clone())), value(&page));
    assert_eq!(
        value(&ExternalResponse::Events(events.clone())),
        value(&events)
    );
}

#[test]
fn an_external_loop_resolve_refuses_each_missing_consent_before_mutating() {
    let corpus = corpus();
    tauri::async_runtime::block_on(async {
        let meetings = headless_manager(&corpus);

        let before = loop_row(&meetings, &corpus.open_loop).await;

        for (consent, settings_path) in [
            (external_consent(false, true), EXTERNAL_ACCESS_SETTING_PATH),
            (
                external_consent(true, false),
                EXTERNAL_MUTATIONS_SETTING_PATH,
            ),
        ] {
            let refusal = AllowedRequest::new(consent, loop_resolve_request(&corpus.open_loop))
                .expect_err("a missing grant stops the request before dispatch");

            assert_eq!(refusal.error, ExternalErrorCode::ConsentRequired);
            assert_eq!(refusal.settings_path, Some(settings_path));
            assert_eq!(loop_row(&meetings, &corpus.open_loop).await, before);
        }
    });
}

#[test]
fn an_external_loop_resolve_replays_the_receipt_and_closes_a_reopened_loop() {
    let corpus = corpus();
    tauri::async_runtime::block_on(async {
        let meetings = headless_manager(&corpus);
        let already_closed = loop_row(&meetings, &corpus.done_commitment).await;
        let persisted_operation_id = MeetingOperationId::from_uuid(
            Uuid::parse_str(
                already_closed
                    .resolving_operation_id
                    .as_deref()
                    .expect("the closed fixture loop records its receipt id"),
            )
            .expect("the fixture receipt id is a UUID"),
        );
        let persisted_receipt = corpus
            .store
            .operation_receipt(persisted_operation_id)
            .expect("read the fixture receipt")
            .expect("the closed fixture loop points to a receipt");
        AllowedRequest::new(
            external_consent(true, true),
            loop_resolve_request(&corpus.done_commitment),
        )
        .expect("both grants allow a replay request");
        let replayed_done = resolve_loop(&meetings, &corpus.done_commitment)
            .await
            .expect("the external request replays a non-external close");
        assert_eq!(replayed_done.receipt, persisted_receipt);
        assert_eq!(
            loop_row(&meetings, &corpus.done_commitment).await,
            already_closed
        );

        let initial = loop_row(&meetings, &corpus.open_loop).await;
        assert_eq!(initial.status, MeetingLoopStatus::Open);

        AllowedRequest::new(
            external_consent(true, true),
            loop_resolve_request(&corpus.open_loop),
        )
        .expect("both grants allow the parsed CLI request");
        let first = resolve_loop(&meetings, &corpus.open_loop)
            .await
            .expect("the external request resolves the open loop");
        let wire = value(&ExternalResponse::LoopResolve(first.clone()));
        assert_eq!(wire["schema_version"], json!(QUERY_SCHEMA_VERSION));
        assert_eq!(wire["receipt"]["command"], "loop_resolve");
        assert_eq!(wire["receipt"]["result"], "committed");
        assert_eq!(first.receipt.command, MeetingCommandKind::LoopResolve);
        assert_eq!(first.receipt.result, OperationResult::Committed);
        assert_eq!(first.receipt.expected_revision, initial.revision);
        assert_eq!(first.receipt.new_revision, Some(initial.revision + 1));

        let closed = loop_row(&meetings, &corpus.open_loop).await;
        let first_operation_id = first.receipt.operation_id.uuid().to_string();
        assert_eq!(closed.status, MeetingLoopStatus::Done);
        assert_eq!(closed.revision, initial.revision + 1);
        assert_eq!(
            closed.resolving_operation_id.as_deref(),
            Some(first_operation_id.as_str())
        );

        AllowedRequest::new(
            external_consent(true, true),
            loop_resolve_request(&corpus.open_loop),
        )
        .expect("the replay is still an allowed CLI request");
        let replay = resolve_loop(&meetings, &corpus.open_loop)
            .await
            .expect("the closed loop replays its receipt");
        assert_eq!(replay.receipt, first.receipt);
        assert_eq!(loop_row(&meetings, &corpus.open_loop).await, closed);

        let reopened = meetings
            .loop_reopen(MeetingLoopReopenRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: corpus.open_loop.clone(),
                expected_revision: closed.revision,
            })
            .await
            .expect("the real manager reopens the closed loop");
        assert_eq!(reopened.receipt.result, OperationResult::Committed);
        let reopened_row = loop_row(&meetings, &corpus.open_loop).await;
        assert_eq!(reopened_row.status, MeetingLoopStatus::Open);
        assert_eq!(reopened_row.revision, closed.revision + 1);

        AllowedRequest::new(
            external_consent(true, true),
            loop_resolve_request(&corpus.open_loop),
        )
        .expect("the reopened loop is an allowed CLI request");
        let second = resolve_loop(&meetings, &corpus.open_loop)
            .await
            .expect("the reopened loop resolves again");
        assert_eq!(second.receipt.result, OperationResult::Committed);
        assert_eq!(second.receipt.expected_revision, reopened_row.revision);
        assert_eq!(second.receipt.new_revision, Some(reopened_row.revision + 1));
        assert_ne!(second.receipt.operation_id, first.receipt.operation_id);

        let closed_again = loop_row(&meetings, &corpus.open_loop).await;
        assert_eq!(closed_again.status, MeetingLoopStatus::Done);
        assert_eq!(closed_again.revision, reopened_row.revision + 1);
    });
}

/// One occurrence of the weekly series both meetings sit in. A carry only
/// looks at the previous session of the same series, so the series key is what
/// makes one meeting the other's predecessor.
fn series_event(start_utc_ms: i64) -> CalendarEventSummary {
    CalendarEventSummary {
        event_key: format!("weekly-pricing#{start_utc_ms}"),
        series_key: "weekly-pricing".to_string(),
        title: TITLE.to_string(),
        attendee_count: 2,
        start_utc_ms,
        end_utc_ms: start_utc_ms + 30 * 60 * 1_000,
        attendees: Vec::new(),
        notes: None,
        calendar_name: None,
        url: None,
    }
}

/// A loop the ledger pass carried forward is refused, not replayed.
///
/// `resolving_operation_id` names whichever operation closed the row, and a
/// carry writes its own, so a carried row points at a `loop_carry` receipt.
/// Replaying that answered `result: committed` and exit 0 for a subject the
/// carry had left live in the next occurrence.
#[test]
fn an_external_loop_resolve_refuses_a_loop_that_was_carried_forward() {
    let corpus = corpus();
    let next_week = NOW + 7 * 24 * 60 * 60 * 1_000;
    tauri::async_runtime::block_on(async {
        let meetings = headless_manager(&corpus);
        let successor_session = meeting(&corpus.store, TITLE, next_week);
        transcript(&corpus.store, successor_session, SEGMENT);
        artifact(&corpus.store, successor_session);
        corpus
            .store
            .remember_calendar_facts(corpus.session_id, &series_event(NOW))
            .unwrap();
        corpus
            .store
            .remember_calendar_facts(successor_session, &series_event(next_week))
            .unwrap();

        let receipts = corpus
            .store
            .carry_loops_forward(successor_session)
            .expect("the ledger pass runs over the next occurrence");
        assert_eq!(
            receipts.len(),
            1,
            "the open thread is the only loop with anywhere to go"
        );

        let successor = MeetingLoopId::derive(successor_session, MeetingLoopKind::Loop, THREAD);
        let carried = loop_row(&meetings, &corpus.open_loop).await;
        assert_eq!(carried.status, MeetingLoopStatus::Carried);
        assert_eq!(carried.carried_into_loop_id.as_ref(), Some(&successor));
        let stamped = MeetingOperationId::from_uuid(
            Uuid::parse_str(
                carried
                    .resolving_operation_id
                    .as_deref()
                    .expect("the carry stamped its operation id"),
            )
            .expect("the carry operation id is a UUID"),
        );
        assert_eq!(
            corpus
                .store
                .operation_receipt(stamped)
                .expect("read the carry receipt")
                .expect("the carried row points at a receipt")
                .command,
            MeetingCommandKind::LoopCarry,
            "the row points at the carry, not at a resolution"
        );

        let refusal = resolve_loop(&meetings, &corpus.open_loop)
            .await
            .expect_err("a carry receipt is not a resolution to replay");

        assert_eq!(refusal.error, ExternalErrorCode::InvalidRequest);
        assert_eq!(refusal.settings_path, None, "this is not a consent problem");
        assert!(
            refusal.message.contains(successor.as_str()),
            "the refusal names the loop that is still live: {}",
            refusal.message
        );
        assert_eq!(loop_row(&meetings, &corpus.open_loop).await, carried);
    });
}
