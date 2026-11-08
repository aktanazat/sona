use super::workflow_core_tests::{meeting, store};
use super::*;
use crate::context::{FrontWindow, FrontWindowRead};
use crate::meeting::open_windows::{item_key, record_look, OFFERED_KEEP_MS};
use rusqlite::params;
use std::collections::HashSet;

const SEEN_AT_UTC_MS: i64 = 1_800_000_000_000;

fn recording(store: &MeetingStore) -> MeetingSessionId {
    let session_id = meeting(store, "Planning", SEEN_AT_UTC_MS);
    // PANIC: a fixture; a setup that fails should fail the test that asked.
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_sessions SET phase = 'capturing_recording' WHERE id = ?1",
            params![session_id.uuid().to_string()],
        )
        .unwrap();
    session_id
}

fn keynote() -> FrontWindow {
    FrontWindow {
        app_name: Some("Keynote".to_string()),
        bundle_id: Some("com.apple.iWork.Keynote".to_string()),
        title: Some("Q3-plan.key".to_string()),
        document_path: Some("/Users/someone/Q3-plan.key".to_string()),
        ..FrontWindow::default()
    }
}

fn linear_page() -> FrontWindow {
    FrontWindow {
        app_name: Some("Safari".to_string()),
        bundle_id: Some("com.apple.Safari".to_string()),
        title: Some("LIN-482 Checkout copy".to_string()),
        host: Some("linear.app".to_string()),
        ..FrontWindow::default()
    }
}

fn note(store: &MeetingStore, session_id: MeetingSessionId, window: &FrontWindow, offset_ns: u64) {
    // PANIC: a fixture; a setup that fails should fail the test that asked.
    store
        .note_open_window(
            session_id,
            &item_key(window).unwrap(),
            window,
            offset_ns,
            3_000,
            SEEN_AT_UTC_MS,
        )
        .unwrap();
}

fn labels(store: &MeetingStore, session_id: MeetingSessionId) -> Vec<(String, bool)> {
    store
        .open_windows(session_id)
        .unwrap()
        .into_iter()
        .map(|window| (window.label, window.attached_at_utc_ms.is_some()))
        .collect()
}

#[test]
fn dismissing_the_offer_deletes_what_was_not_attached() {
    let (_directory, store) = store();
    let session_id = recording(&store);
    note(&store, session_id, &keynote(), 1_000);
    note(&store, session_id, &linear_page(), 2_000);
    let keynote_id = store.open_windows(session_id).unwrap()[0].item_id;
    store
        .attach_open_window(session_id, keynote_id, SEEN_AT_UTC_MS)
        .unwrap();

    store.dismiss_open_windows(session_id).unwrap();

    assert_eq!(
        labels(&store, session_id),
        vec![("Q3-plan.key".to_string(), true)]
    );
}

#[test]
fn offered_windows_expire_seven_days_after_they_were_seen() {
    let (_directory, store) = store();
    let session_id = recording(&store);
    note(&store, session_id, &linear_page(), 1_000);
    let window = keynote();
    store
        .attach_open_window_now(
            session_id,
            &item_key(&window).unwrap(),
            &window,
            2_000,
            SEEN_AT_UTC_MS,
        )
        .unwrap();
    let both = vec![
        ("LIN-482 Checkout copy".to_string(), false),
        ("Q3-plan.key".to_string(), true),
    ];

    store
        .purge_stale_open_windows(SEEN_AT_UTC_MS + OFFERED_KEEP_MS - 1)
        .unwrap();
    assert_eq!(labels(&store, session_id), both, "one millisecond early");

    store
        .purge_stale_open_windows(SEEN_AT_UTC_MS + OFFERED_KEEP_MS)
        .unwrap();
    assert_eq!(
        labels(&store, session_id),
        vec![("Q3-plan.key".to_string(), true)]
    );
}

#[test]
fn a_window_that_showed_a_password_field_is_never_listed() {
    let (_directory, store) = store();
    let session_id = recording(&store);
    let mut password_windows = HashSet::new();
    let mut look = |read: FrontWindowRead| {
        record_look(&store, session_id, read, 1_000, &[], &mut password_windows).unwrap();
    };

    look(FrontWindowRead::Window(linear_page()));
    look(FrontWindowRead::PasswordField(linear_page()));
    look(FrontWindowRead::Window(linear_page()));

    assert_eq!(labels(&store, session_id), Vec::new());
}
