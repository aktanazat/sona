//! D19: titles from content.
//!
//! `derive_title_from_headline` shipped in wave 1 with no tests, which is the
//! wrong shape for a write that fires by itself after every generation. Three
//! things have to stay true, and each is a different way of being wrong:
//!
//! - A manual recording gets its name from what was said in it.
//! - A title somebody else already supplied — typed, or from a calendar
//!   event — is never overwritten. The failure here is silent and destructive:
//!   the app argues with its user and the user's own words are gone.
//! - "Local notes" survives a meeting with nothing in it, rather than the
//!   recording being renamed to the empty string.

use super::workflow_core_tests::{meeting, store};
use crate::meeting::types::{
    MeetingSessionId, OperationActor, OperationResult, MANUAL_DEFAULT_TITLE,
};
use rusqlite::params;

const NOW: i64 = 1_700_000_000_000;

fn title_of(store: &super::MeetingStore, session_id: MeetingSessionId) -> String {
    store
        .connection()
        .unwrap()
        .query_row(
            "SELECT title FROM meeting_sessions WHERE id = ?1",
            params![session_id.uuid().to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

fn revision_of(store: &super::MeetingStore, session_id: MeetingSessionId) -> u64 {
    let value: i64 = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT revision FROM meeting_sessions WHERE id = ?1",
            params![session_id.uuid().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    u64::try_from(value).unwrap()
}

#[test]
fn a_manual_recording_is_renamed_from_its_headline() {
    let (_directory, store) = store();
    let session_id = meeting(&store, MANUAL_DEFAULT_TITLE, NOW);

    let receipt = store
        .derive_title_from_headline(
            session_id,
            "pricing stayed open and Dana took the comparison. Billing was settled.",
        )
        .unwrap()
        .expect("a headline with a sentence in it renames the recording");

    // The first sentence, sentence-cased, without its full stop. The second
    // sentence is not part of a title.
    assert_eq!(
        title_of(&store, session_id),
        "Pricing stayed open and Dana took the comparison"
    );
    // Nobody asked for this write, so the receipt says so, and it carries the
    // new name as its effect for a reader checking what changed.
    assert_eq!(receipt.actor, OperationActor::System);
    assert_eq!(receipt.result, OperationResult::Committed);
    assert_eq!(
        receipt.effect_ids,
        vec!["Pricing stayed open and Dana took the comparison".to_string()]
    );
    assert_eq!(receipt.new_revision, Some(revision_of(&store, session_id)));
}

#[test]
fn a_title_somebody_already_supplied_is_left_alone() {
    let (_directory, store) = store();
    // The two cases that are not the manual default: a name a person typed,
    // and a name a calendar event supplied. Neither is a placeholder, so the
    // derivation must treat them identically.
    for supplied in ["Q3 pricing", "Weekly sync (Dana / Amir)"] {
        let session_id = meeting(&store, supplied, NOW);
        let revision_before = revision_of(&store, session_id);

        let outcome = store
            .derive_title_from_headline(session_id, "Pricing stayed open.")
            .unwrap();

        assert!(outcome.is_none(), "{supplied} must not be renamed");
        assert_eq!(title_of(&store, session_id), supplied);
        // Not renaming must also not bump the meeting, or every generation
        // would move a revision nothing changed at.
        assert_eq!(revision_of(&store, session_id), revision_before);
    }
}

#[test]
fn local_notes_survives_a_headline_with_no_title_in_it() {
    let (_directory, store) = store();
    // What an empty transcript produces: a headline that is blank, or nothing
    // but the punctuation a sentence would have ended with.
    for headline in ["", "   ", "\n\t ", ".", "  ...  ", "?!"] {
        let session_id = meeting(&store, MANUAL_DEFAULT_TITLE, NOW);

        let outcome = store
            .derive_title_from_headline(session_id, headline)
            .unwrap();

        assert!(
            outcome.is_none(),
            "{headline:?} yields no title, so nothing is written"
        );
        assert_eq!(title_of(&store, session_id), MANUAL_DEFAULT_TITLE);
        assert_eq!(revision_of(&store, session_id), 0);
    }
}

#[test]
fn a_holder_title_is_replaced_by_the_topic() {
    let (_directory, store) = store();
    // Both holder forms `detection::holder_title` writes, and the call form.
    for holder in ["Slack meeting", "Call in Safari", "FaceTime call, 3:15 PM"] {
        let session_id = meeting(&store, holder, NOW);

        store
            .derive_title_from_headline(
                session_id,
                "The meeting addressed the Q3 pricing rollout, with Dana presenting. Billing was settled.",
            )
            .unwrap()
            .unwrap_or_else(|| panic!("{holder} is a placeholder and must be renamed"));

        assert_eq!(title_of(&store, session_id), "Q3 pricing rollout");
    }
}

#[test]
fn a_title_that_only_looks_like_a_holder_is_left_alone() {
    let (_directory, store) = store();
    // "<word> meeting" is how people name meetings too. Only an app's name
    // in front makes it a placeholder.
    for typed in [
        "Board meeting",
        "Team meeting",
        "Call in sick policy review with the whole team",
        "Slack meeting notes",
        "Dana call, follow-up",
    ] {
        let session_id = meeting(&store, typed, NOW);

        let outcome = store
            .derive_title_from_headline(session_id, "Pricing stayed open.")
            .unwrap();

        assert!(outcome.is_none(), "{typed} must not be renamed");
        assert_eq!(title_of(&store, session_id), typed);
    }
}

#[test]
fn a_derived_title_follows_the_notes_until_somebody_renames_it() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Zoom meeting", NOW);
    store
        .derive_title_from_headline(session_id, "The call covered hiring plans.")
        .unwrap()
        .unwrap();
    assert_eq!(title_of(&store, session_id), "Hiring plans");

    // Notes written again read differently, and the title Sona derived is
    // still Sona's to change.
    store
        .derive_title_from_headline(session_id, "The call covered the hiring freeze.")
        .unwrap()
        .unwrap();
    assert_eq!(title_of(&store, session_id), "Hiring freeze");

    // Once a person names it, the derivation is done with this meeting —
    // even when they type the derived words back.
    store
        .set_title(
            crate::meeting::types::MeetingOperationId::new(),
            NOW,
            session_id,
            revision_of(&store, session_id),
            "Hiring freeze".to_string(),
        )
        .unwrap();
    let outcome = store
        .derive_title_from_headline(session_id, "The call covered budgets.")
        .unwrap();
    assert!(outcome.is_none());
    assert_eq!(title_of(&store, session_id), "Hiring freeze");
}

/// Synthetic headlines in the shapes a notes model opens a summary with, and
/// the topic a person would title each meeting by.
const TITLE_FIXTURES: &[(&str, &str)] = &[
    (
        "The meeting addressed the Q3 pricing rollout and the billing migration.",
        "Q3 pricing rollout and the billing migration",
    ),
    (
        "The discussion covered onboarding flow changes, with Priya presenting mockups.",
        "Onboarding flow changes",
    ),
    (
        "The call focused on the Android crash backlog.",
        "Android crash backlog",
    ),
    (
        "The team discussed the launch checklist for the mobile beta.",
        "Launch checklist for the mobile beta",
    ),
    (
        "In this meeting, the team reviewed hiring plans for the design group.",
        "Hiring plans for the design group",
    ),
    (
        "The conversation centered on vendor selection for the data warehouse.",
        "Vendor selection for the data warehouse",
    ),
    (
        "This meeting was about the incident retrospective for the March outage.",
        "Incident retrospective for the March outage",
    ),
    (
        "The meeting primarily addressed budget cuts; travel is frozen until June.",
        "Budget cuts",
    ),
    (
        "Participants discussed the API deprecation timeline, including the v1 sunset date.",
        "API deprecation timeline",
    ),
    (
        "We went over the quarterly roadmap. Two items slipped.",
        "Quarterly roadmap",
    ),
    (
        "The huddle covered a flaky checkout test.",
        "Flaky checkout test",
    ),
    (
        "The session explored how to migrate the search index.",
        "Migrate the search index",
    ),
    (
        "The sync touched on support queue staffing, as well as the holiday rota.",
        "Support queue staffing",
    ),
    (
        "During the call, the group talked about renewing the Acme contract.",
        "Renewing the Acme contract",
    ),
    (
        "The discussion revolved around customer churn in the starter plan.",
        "Customer churn in the starter plan",
    ),
    (
        "They reviewed the security audit findings, which were mostly low severity.",
        "Security audit findings",
    ),
    // Headlines that already open on their subject stay as they are.
    (
        "Pricing stayed open and Dana took the comparison. Billing was settled.",
        "Pricing stayed open and Dana took the comparison",
    ),
    ("Launch moved to the 14th.", "Launch moved to the 14th"),
    (
        "Amir owns the migration plan now.",
        "Amir owns the migration plan now",
    ),
    (
        "No decision on the vendor; Lena wants one more quote.",
        "No decision on the vendor; Lena wants one more quote",
    ),
];

#[test]
fn derived_titles_name_the_topic_on_synthetic_headlines() {
    let started = std::time::Instant::now();
    let mut right = 0;
    for (headline, expected) in TITLE_FIXTURES {
        let title = super::derived_title(headline).unwrap_or_default();
        if title == *expected {
            right += 1;
        } else {
            println!("miss: {headline:?} -> {title:?}, expected {expected:?}");
        }
    }
    let elapsed = started.elapsed();
    println!(
        "title accuracy: {right}/{} exact, {:.1} us per headline",
        TITLE_FIXTURES.len(),
        elapsed.as_secs_f64() * 1e6 / TITLE_FIXTURES.len() as f64
    );
    assert_eq!(right, TITLE_FIXTURES.len());
}
