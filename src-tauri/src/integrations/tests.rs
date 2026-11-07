use super::*;
use crate::meeting::detection::machine::{
    CalendarAttendee, CalendarEventSummary, ParticipationStatus,
};
use crate::meeting::store::workflow_core_tests::{
    current_artifact, reviewable_meeting, seed_connection_grant_time, seed_folder_filing_time,
    store,
};
use crate::meeting::store::CalendarEventSource;
use std::sync::atomic::{AtomicUsize, Ordering};

fn connection() -> Connection {
    Connection {
        id: uuid::Uuid::new_v4().to_string(),
        name: "Team notes".into(),
        kind: ConnectionKind::PublicWebhook,
        target: String::new(),
        enabled: true,
    }
}

#[test]
fn overlapping_rules_and_repeated_requests_have_one_external_effect() {
    let (_directory, store) = store();
    let connection = connection();
    store.save_connection(&connection).unwrap();
    let calls = AtomicUsize::new(0);
    let operation = uuid::Uuid::new_v4().to_string();
    let first = tauri::async_runtime::block_on(deliver(
        &store,
        &connection,
        Some("meeting".into()),
        &operation,
        Some("automatic:meeting:connection"),
        || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        },
    ))
    .unwrap();
    let second = tauri::async_runtime::block_on(deliver(
        &store,
        &connection,
        Some("meeting".into()),
        &uuid::Uuid::new_v4().to_string(),
        Some("automatic:meeting:connection"),
        || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        },
    ))
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(first, second);
    assert_eq!(
        store.connection_receipt(&operation).unwrap().state,
        SendState::Sent
    );
    assert_eq!(store.connection_receipts().unwrap(), vec![first]);
}

#[test]
fn a_failed_delivery_has_a_durable_readable_receipt_and_never_replays_silently() {
    let (_directory, store) = store();
    let connection = connection();
    store.save_connection(&connection).unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let receipt = tauri::async_runtime::block_on(deliver(
        &store,
        &connection,
        None,
        &operation,
        None,
        || async { Err("The destination did not confirm delivery.".into()) },
    ))
    .unwrap();
    assert_eq!(receipt.state, SendState::Failed);
    assert_eq!(
        store.connection_receipt(&operation).unwrap().detail,
        "The destination did not confirm delivery."
    );
    let replay = tauri::async_runtime::block_on(deliver(
        &store,
        &connection,
        None,
        &operation,
        None,
        || async { panic!("a claimed request must not reach the destination twice") },
    ))
    .unwrap();
    assert_eq!(replay, receipt);
}

#[test]
fn automatic_sends_are_opt_in_future_only_and_disconnect_removes_the_grant() {
    let (_directory, store) = store();
    let connection = connection();
    store.save_connection(&connection).unwrap();
    let session = reviewable_meeting(&store, "Planning", 100);
    let mut rule = AutoSendRule {
        id: uuid::Uuid::new_v4().to_string(),
        connection_id: connection.id.clone(),
        scope: SendScope::All,
        enabled: true,
        created_at_utc_ms: 0,
    };
    store.save_connection_rule(&mut rule).unwrap();
    let granted_at = rule.created_at_utc_ms;
    assert!(automatic_destinations(&store, session, 100)
        .unwrap()
        .is_empty());
    assert!(automatic_destinations(&store, session, granted_at)
        .unwrap()
        .is_empty());
    assert!(automatic_destinations(&store, session, granted_at + 1)
        .unwrap()
        .contains(&connection.id));
    rule.enabled = false;
    store.save_connection_rule(&mut rule).unwrap();
    assert!(automatic_destinations(&store, session, granted_at + 1)
        .unwrap()
        .is_empty());
    // Seed a historical disabled grant; re-enabling must not export its disabled period.
    seed_connection_grant_time(&store, &rule.id, 300).unwrap();
    rule.enabled = true;
    store.save_connection_rule(&mut rule).unwrap();
    assert!(automatic_destinations(&store, session, 400)
        .unwrap()
        .is_empty());
    store.disconnect_connection(&connection.id).unwrap();
    assert_eq!(store.connection_rules().unwrap(), vec![]);
}

#[test]
fn credentials_and_targets_fail_closed_before_a_send() {
    let mut connection = connection();
    let mut credential = Credential {
        token: "0123456789abcdef".into(),
        webhook_url: "https://hooks.zapier.com/hooks/catch/1/a/".into(),
    };
    assert!(validate(&connection, &credential).is_ok());
    credential.webhook_url = "http://127.0.0.1".into();
    assert!(validate(&connection, &credential).is_err());
    connection.kind = ConnectionKind::SlackBot;
    connection.target = "C123".into();
    credential.webhook_url.clear();
    assert!(validate(&connection, &credential).is_ok());
    connection.target = "C123&other=secret".into();
    assert!(validate(&connection, &credential).is_err());
    connection.kind = ConnectionKind::NotionDatabase;
    connection.target = "../../users".into();
    assert!(validate(&connection, &credential).is_err());
    assert!(!valid_email("a@example.com\r\nBcc:b@example.com"));
}

#[test]
fn folder_rules_send_old_notes_only_when_explicitly_filed_after_the_grant() {
    use crate::meeting::folder_types::{
        MeetingFolderCreateRequest, MeetingFolderMembershipRequest,
    };
    use crate::meeting::types::MeetingOperationId;
    let (_directory, store) = store();
    let connection = connection();
    store.save_connection(&connection).unwrap();
    let old = reviewable_meeting(&store, "Already filed", 50);
    let newly_filed = reviewable_meeting(&store, "Older notes filed later", 60);
    let result = store
        .create_meeting_folder(
            &MeetingFolderCreateRequest {
                operation_id: MeetingOperationId::new(),
                name: "Team notes".into(),
                expected_revision: 0,
            },
            100,
        )
        .unwrap();
    let folder_id = result.folders.folders[0].folder_id;
    let first = store
        .add_meeting_to_folder(
            &MeetingFolderMembershipRequest {
                operation_id: MeetingOperationId::new(),
                folder_id,
                session_id: old,
                expected_revision: result.folders.revision,
            },
            150,
        )
        .unwrap();
    let mut rule = AutoSendRule {
        id: uuid::Uuid::new_v4().to_string(),
        connection_id: connection.id.clone(),
        scope: SendScope::Folder(folder_id.uuid().to_string()),
        enabled: true,
        created_at_utc_ms: 0,
    };
    store.save_connection_rule(&mut rule).unwrap();
    let second = store
        .add_meeting_to_folder(
            &MeetingFolderMembershipRequest {
                operation_id: MeetingOperationId::new(),
                folder_id,
                session_id: newly_filed,
                expected_revision: first.folders.revision,
            },
            250,
        )
        .unwrap();
    // Freeze committed times around the stored grant instead of waiting for the wall clock.
    seed_folder_filing_time(&store, folder_id, old, rule.created_at_utc_ms - 1).unwrap();
    seed_folder_filing_time(&store, folder_id, newly_filed, rule.created_at_utc_ms).unwrap();
    assert!(automatic_destinations(&store, old, 50).unwrap().is_empty());
    assert!(automatic_destinations(&store, newly_filed, 60)
        .unwrap()
        .is_empty());
    seed_folder_filing_time(&store, folder_id, newly_filed, rule.created_at_utc_ms + 1).unwrap();
    assert!(automatic_destinations(&store, newly_filed, 60)
        .unwrap()
        .contains(&connection.id));
    store
        .add_meeting_to_folder(
            &MeetingFolderMembershipRequest {
                operation_id: MeetingOperationId::new(),
                folder_id,
                session_id: old,
                expected_revision: second.folders.revision,
            },
            300,
        )
        .unwrap();
    assert!(automatic_destinations(&store, old, 50).unwrap().is_empty());
}

/// Two meetings of one series: one whose start named the event, one that
/// found it under way when its recording was started by hand.
fn chosen_and_found(
    store: &MeetingStore,
) -> Result<(MeetingSessionId, MeetingSessionId), crate::meeting::store::StoreError> {
    let event = CalendarEventSummary {
        event_key: "series-roadmap#100".into(),
        series_key: "series-roadmap".into(),
        title: "Roadmap review".into(),
        attendee_count: 2,
        start_utc_ms: 100,
        end_utc_ms: 200,
        attendees: vec![CalendarAttendee {
            name: "Stephen Wolfram".into(),
            email: Some("stephen@example.com".into()),
            status: ParticipationStatus::Accepted,
            is_self: false,
        }],
        notes: None,
        calendar_name: None,
        location: None,
        url: None,
        place: None,
    };
    let chosen = reviewable_meeting(store, "Roadmap review", 100);
    let found = reviewable_meeting(store, "Roadmap review", 100);
    store.remember_calendar_facts(chosen, &event)?;
    store.remember_calendar_event(found, &event, CalendarEventSource::FoundUnderWay)?;
    Ok((chosen, found))
}

#[test]
fn a_series_rule_never_sends_a_meeting_whose_event_was_found_under_way() {
    let (_directory, store) = store();
    let connection = connection();
    store.save_connection(&connection).unwrap();
    let (chosen, found) = chosen_and_found(&store).unwrap();
    let mut rule = AutoSendRule {
        id: uuid::Uuid::new_v4().to_string(),
        connection_id: connection.id.clone(),
        scope: SendScope::Series("series-roadmap".into()),
        enabled: true,
        created_at_utc_ms: 0,
    };
    store.save_connection_rule(&mut rule).unwrap();
    let started_after_the_grant = rule.created_at_utc_ms + 1;

    assert!(
        automatic_destinations(&store, chosen, started_after_the_grant)
            .unwrap()
            .contains(&connection.id)
    );
    assert!(
        automatic_destinations(&store, found, started_after_the_grant)
            .unwrap()
            .is_empty(),
        "nobody chose an event found under way, so its series rule sends nothing"
    );
}

#[test]
fn an_automatic_send_matches_no_attendee_of_an_event_found_under_way() {
    let (_directory, store) = store();
    let (chosen, found) = chosen_and_found(&store).unwrap();
    let generated = serde_json::json!({
        "summary": {"text": "The roadmap is agreed.", "citations": []},
        "outline": [],
        "decisions": [],
        "action_items": [],
        "key_questions": [],
        "risks": [],
        "follow_up_draft": {"text": "", "citations": []}
    });
    for session_id in [chosen, found] {
        current_artifact(&store, session_id, &generated, 1);
    }

    assert_eq!(
        automatic_notes(&store, chosen).unwrap().attendee_emails,
        ["stephen@example.com"]
    );
    assert!(
        automatic_notes(&store, found)
            .unwrap()
            .attendee_emails
            .is_empty(),
        "an automatic send matches nobody from an event nobody chose"
    );
    assert_eq!(
        notes(&store, found).unwrap().attendee_emails,
        ["stephen@example.com"],
        "a send the person starts still reaches the attendees"
    );
}
