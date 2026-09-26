//! D14 at the store boundary: which series keep their text on this Mac, the
//! roster a settings surface offers, and the engine a rewrite records.

use super::workflow_core_tests::{meeting, store};
use super::*;
use crate::meeting::analytics::MeetingNotesTemplate;
use crate::meeting::series_types::{
    MeetingSeriesMutationResult, MeetingSeriesRemoteOptOutSetRequest,
    MeetingSeriesTemplateSetRequest,
};
use crate::meeting::types::{
    MeetingArtifactId, MeetingCommandKind, MeetingOperationId, MeetingReasonCode, MeetingSessionId,
    OperationActor, OperationResult,
};
use rusqlite::params;

/// The calendar facts a finished meeting leaves behind, which is the only place
/// a series key or a series' name is ever written down.
fn calendar_facts(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    series_key: &str,
    title: &str,
) {
    let event_json = serde_json::json!({
        "eventKey": format!("{series_key}#{title}"),
        "seriesKey": series_key,
        "title": title,
    });
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_calendar_facts(session_id, event_key, event_json)
             VALUES (?1, ?2, ?3)",
            params![
                session_id.uuid().to_string(),
                format!("{series_key}#{title}"),
                event_json.to_string()
            ],
        )
        .unwrap();
}

fn exclude(
    store: &MeetingStore,
    series_key: &str,
    opt_out: bool,
    expected_revision: u64,
) -> MeetingSeriesMutationResult {
    store
        .set_series_remote_opt_out(
            &MeetingSeriesRemoteOptOutSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: series_key.to_string(),
                remote_intelligence_opt_out: opt_out,
                expected_revision,
            },
            1_000,
        )
        .unwrap()
}

#[test]
fn a_series_nobody_has_excluded_follows_the_global_setting() {
    let (_directory, store) = store();

    let preferences = store.series_preferences("weekly-sync").unwrap();

    assert!(
        !preferences.remote_intelligence_opt_out,
        "no row means no departure from the setting"
    );
}

#[test]
fn excluding_a_series_is_receipted_and_names_the_series_it_touched() {
    let (_directory, store) = store();

    let result = exclude(&store, "weekly-sync", true, 0);

    assert_eq!(result.receipt.result, OperationResult::Committed);
    assert_eq!(
        result.receipt.command,
        MeetingCommandKind::SeriesRemoteOptOutSet
    );
    assert_eq!(result.receipt.actor, OperationActor::User);
    assert_eq!(result.receipt.effect_ids, vec!["weekly-sync".to_string()]);
    assert_eq!(result.receipt.new_revision, Some(1));
    assert!(result.preferences.remote_intelligence_opt_out);
    assert!(
        store
            .series_preferences("weekly-sync")
            .unwrap()
            .remote_intelligence_opt_out,
        "the exclusion is what the next read sees"
    );
}

/// The failure this guards against is the quiet one: a template picker in
/// another pane saving from a stale read and handing a sensitive series back to
/// the server. Every series write shares one counter for exactly that reason.
#[test]
fn a_stale_exclusion_is_rejected_and_changes_nothing() {
    let (_directory, store) = store();
    exclude(&store, "weekly-sync", true, 0);

    let stale = exclude(&store, "weekly-sync", false, 0);

    assert_eq!(stale.receipt.result, OperationResult::Rejected);
    assert_eq!(
        stale.receipt.reason_codes,
        vec![MeetingReasonCode::StaleRevision]
    );
    assert_eq!(stale.receipt.new_revision, Some(1));
    assert!(
        stale.preferences.remote_intelligence_opt_out,
        "a rejected write leaves the exclusion in place"
    );
}

#[test]
fn a_replayed_exclusion_returns_the_receipt_it_already_wrote() {
    let (_directory, store) = store();
    let request = MeetingSeriesRemoteOptOutSetRequest {
        operation_id: MeetingOperationId::new(),
        series_key: "weekly-sync".to_string(),
        remote_intelligence_opt_out: true,
        expected_revision: 0,
    };

    let first = store.set_series_remote_opt_out(&request, 1_000).unwrap();
    let replay = store.set_series_remote_opt_out(&request, 2_000).unwrap();

    assert_eq!(first.receipt, replay.receipt);
    assert_eq!(
        store.series_revision().unwrap(),
        1,
        "a replay is not a second write"
    );
}

/// A row that exists only to hold an exclusion must survive the other controls
/// being cleared. The sweep that drops preference rows with nothing in them is
/// the thing most likely to forget a new column, and forgetting it here would
/// silently re-enable the server for that series.
#[test]
fn clearing_a_template_does_not_drop_a_series_exclusion() {
    let (_directory, store) = store();
    exclude(&store, "weekly-sync", true, 0);
    store
        .set_series_template(
            &MeetingSeriesTemplateSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: "weekly-sync".to_string(),
                template: Some(MeetingNotesTemplate::Standup),
                custom_template_id: None,
                expected_revision: 1,
            },
            1_000,
        )
        .unwrap();

    let cleared = store
        .set_series_template(
            &MeetingSeriesTemplateSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: "weekly-sync".to_string(),
                template: None,
                custom_template_id: None,
                expected_revision: 2,
            },
            1_000,
        )
        .unwrap();

    assert_eq!(cleared.preferences.template, None);
    assert!(
        cleared.preferences.remote_intelligence_opt_out,
        "the exclusion outlives the template it sat beside"
    );
}

/// Taking the exclusion off is a decision too, and once nothing is left the row
/// goes — otherwise "has a row" would stop meaning "has decided something".
#[test]
fn a_series_handed_back_to_the_setting_keeps_no_row() {
    let (_directory, store) = store();
    exclude(&store, "weekly-sync", true, 0);

    let restored = exclude(&store, "weekly-sync", false, 1);

    assert!(!restored.preferences.remote_intelligence_opt_out);
    let rows: i64 = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM meeting_series_preferences WHERE series_key = ?1",
            params!["weekly-sync"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0, "nothing left to remember");
}

/// The roster is what makes the per-series switch reachable at all: it is
/// derived from the meetings a series actually had, newest first, and it carries
/// the name the latest occurrence used.
#[test]
fn the_roster_lists_met_series_newest_first_with_their_latest_name() {
    let (_directory, store) = store();
    let older = meeting(&store, "Weekly sync", 1_000);
    let newer = meeting(&store, "Weekly sync", 5_000);
    let other = meeting(&store, "Board", 3_000);
    calendar_facts(&store, older, "weekly-sync", "Weekly sync");
    calendar_facts(&store, newer, "weekly-sync", "Weekly sync (new room)");
    calendar_facts(&store, other, "board", "Board");
    let manual = meeting(&store, "Local notes", 9_000);

    exclude(&store, "board", true, 0);
    let roster = store.series_remote_roster().unwrap();

    assert_eq!(roster.revision, 1, "the fence every switch writes with");
    let keys: Vec<&str> = roster
        .rows
        .iter()
        .map(|row| row.series_key.as_str())
        .collect();
    assert_eq!(keys, vec!["weekly-sync", "board"]);
    let weekly = &roster.rows[0];
    assert_eq!(
        weekly.title, "Weekly sync (new room)",
        "the name the operator would recognize is the most recent one"
    );
    assert_eq!(weekly.meetings, 2);
    assert_eq!(weekly.last_met_at_utc_ms, 5_000);
    assert!(!weekly.remote_intelligence_opt_out);
    assert!(roster.rows[1].remote_intelligence_opt_out);
    assert!(
        !roster
            .rows
            .iter()
            .any(|row| row.series_key.contains(&manual.uuid().to_string())),
        "a meeting with no calendar event belongs to no series"
    );
}

/// The audit question D14 creates: these notes exist, where were they written?
/// The receipt answers it for the operation that ran, which is the only answer
/// that cannot be contradicted by a setting changing afterwards.
#[test]
fn the_regeneration_receipt_names_the_engine_that_wrote_the_notes() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Weekly sync", 1_000);
    let artifact_id = MeetingArtifactId::new();

    let receipt = store
        .record_artifact_regeneration(
            MeetingOperationId::new(),
            1_000,
            session_id,
            0,
            artifact_id,
            "sona-relay",
        )
        .unwrap();

    assert_eq!(receipt.result, OperationResult::Committed);
    assert_eq!(receipt.command, MeetingCommandKind::ArtifactsRegenerate);
    assert_eq!(
        receipt.effect_ids,
        vec![artifact_id.uuid().to_string(), "sona-relay".to_string()],
        "the artifact and the engine that produced it"
    );
}

/// The same receipt for the on-device engine, so a reader can tell the two
/// apart rather than inferring locality from the absence of a note.
#[test]
fn a_locally_written_rewrite_names_the_on_device_engine_too() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Weekly sync", 1_000);
    let artifact_id = MeetingArtifactId::new();

    let receipt = store
        .record_artifact_regeneration(
            MeetingOperationId::new(),
            1_000,
            session_id,
            0,
            artifact_id,
            "apple-intelligence",
        )
        .unwrap();

    assert_eq!(
        receipt.effect_ids.last().map(String::as_str),
        Some("apple-intelligence")
    );
}
