use super::*;
use crate::meeting::store::people::derive_title_links_in;
use crate::meeting::store::workflow_core_tests::{
    event, inputs, meeting, person, store, transcript_segments,
};
use crate::meeting::store::MeetingStore;
use crate::meeting::workflow_types::{WorkflowEventKind, WorkflowId};
use rusqlite::params;
use std::collections::BTreeSet;
use std::time::Instant;
use uuid::Uuid;

/// Invented people. Two share a first name, three have first names that are
/// also words, one goes by a second name.
const PEOPLE: &[(&str, &[&str])] = &[
    ("Dana Whitfield", &[]),
    ("Priya Raman", &[]),
    ("Sam Okafor", &[]),
    ("Sam Lindqvist", &[]),
    ("Will Harper", &[]),
    ("Grace Lindholm", &[]),
    ("Mark Ellison", &[]),
    ("Tomas Varga", &["Tom Varga"]),
    ("Mei Tanaka", &[]),
];

struct Fixture {
    title: &'static str,
    lines: &'static [&'static str],
    /// Names the call listed for the far side.
    roster: &'static [&'static str],
    expected: &'static [&'static str],
}

/// Invented meetings; no line comes from a real transcript.
const MEETINGS: &[Fixture] = &[
    Fixture {
        title: "Slack meeting",
        lines: &[
            "Thanks Dana, can you send the pricing sheet?",
            "Sure, I will.",
        ],
        roster: &[],
        expected: &["Dana Whitfield"],
    },
    Fixture {
        title: "Slack meeting",
        lines: &["Priya Raman is joining a few minutes late."],
        roster: &[],
        expected: &["Priya Raman"],
    },
    Fixture {
        title: "Call in Safari",
        lines: &["Will that work for everyone?", "I think so."],
        roster: &[],
        expected: &[],
    },
    Fixture {
        title: "Zoom meeting",
        lines: &["Will, can you take the migration?"],
        roster: &[],
        expected: &["Will Harper"],
    },
    Fixture {
        title: "Local notes",
        lines: &["Sam mentioned the outage on Tuesday."],
        roster: &[],
        expected: &[],
    },
    Fixture {
        title: "Local notes",
        lines: &["Sam Okafor mentioned the outage on Tuesday."],
        roster: &[],
        expected: &["Sam Okafor"],
    },
    Fixture {
        title: "Slack meeting",
        lines: &["Let's ship it this week."],
        roster: &["Mei Tanaka"],
        expected: &["Mei Tanaka"],
    },
    Fixture {
        title: "Roadmap with Grace",
        lines: &["We need a longer grace period on invoices."],
        roster: &[],
        expected: &["Grace Lindholm"],
    },
    Fixture {
        title: "Slack meeting",
        lines: &["Grace under pressure is the theme of the offsite."],
        roster: &[],
        expected: &[],
    },
    Fixture {
        title: "Zoom meeting",
        lines: &["I talked to Tom Varga yesterday about the contract."],
        roster: &[],
        expected: &["Tomas Varga"],
    },
    Fixture {
        title: "Local notes",
        lines: &["the dana point office is closed on friday"],
        roster: &[],
        expected: &[],
    },
    Fixture {
        title: "Slack meeting",
        lines: &["Mei's draft is ready and Priya reviewed it."],
        roster: &[],
        expected: &["Mei Tanaka", "Priya Raman"],
    },
    Fixture {
        title: "Call in Chrome",
        lines: &["Mark the ticket as done. Will do."],
        roster: &[],
        expected: &[],
    },
    Fixture {
        title: "Call in Chrome",
        lines: &["I asked Mark about the budget and he agreed."],
        roster: &[],
        expected: &["Mark Ellison"],
    },
    Fixture {
        title: "Slack meeting",
        lines: &["Hope that helps."],
        roster: &["sam", "Dana Whitfield"],
        expected: &["Dana Whitfield"],
    },
    Fixture {
        title: "FaceTime call, 3:15 PM",
        lines: &["Nothing about anybody in particular."],
        roster: &[],
        expected: &[],
    },
];

fn roster(store: &MeetingStore, meeting_id: MeetingSessionId, names: &[&str]) {
    let connection = store.connection().unwrap();
    let meeting = meeting_id.uuid().to_string();
    let plan_id: String = connection
        .query_row(
            "SELECT plan_id FROM meeting_run_plans WHERE session_id = ?1",
            [&meeting],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meeting_call_name_runs (session_id, plan_id, target_json, state, detail)
             VALUES (?1, ?2, '{}', 'reading', '')",
            params![meeting, plan_id],
        )
        .unwrap();
    for name in names {
        connection
            .execute(
                "INSERT INTO meeting_call_participants
                    (session_id, participant_id, display_name, is_local)
                 VALUES (?1, ?2, ?3, 0)",
                params![meeting, Uuid::new_v4().to_string(), name],
            )
            .unwrap();
    }
}

fn linked(connection: &Connection, meeting_id: MeetingSessionId) -> BTreeSet<String> {
    let mut statement = connection
        .prepare(
            "SELECT p.display_name FROM meeting_person_links l JOIN persons p ON p.id = l.person_id
              WHERE l.meeting_id = ?1",
        )
        .unwrap();
    let names = statement
        .query_map([meeting_id.uuid().to_string()], |row| row.get(0))
        .unwrap()
        .collect::<Result<BTreeSet<String>, _>>()
        .unwrap();
    names
}

/// (links that should exist and do, links that should not exist and do).
fn score(found: &BTreeSet<String>, expected: &[&str]) -> (usize, usize) {
    let right = found
        .iter()
        .filter(|name| expected.contains(&name.as_str()))
        .count();
    (right, found.len() - right)
}

#[test]
fn meetings_suggest_the_people_they_name_on_synthetic_fixtures() {
    let (_directory, store) = store();
    for (name, aliases) in PEOPLE {
        person(&store, name, aliases, &[]);
    }
    let meetings = MEETINGS
        .iter()
        .enumerate()
        .map(|(index, fixture)| {
            let meeting_id = meeting(&store, fixture.title, i64::try_from(index).unwrap() + 1);
            transcript_segments(&store, meeting_id, fixture.lines);
            if !fixture.roster.is_empty() {
                roster(&store, meeting_id, fixture.roster);
            }
            meeting_id
        })
        .collect::<Vec<_>>();
    let connection = store.connection().unwrap();
    let wanted = MEETINGS
        .iter()
        .map(|fixture| fixture.expected.len())
        .sum::<usize>();

    // What finalizing did before: the title alone.
    let (mut title_right, mut title_wrong) = (0, 0);
    for (fixture, meeting_id) in MEETINGS.iter().zip(&meetings) {
        derive_title_links_in(&connection, *meeting_id, fixture.title, 10).unwrap();
        let (right, wrong) = score(&linked(&connection, *meeting_id), fixture.expected);
        title_right += right;
        title_wrong += wrong;
    }

    let started = Instant::now();
    for meeting_id in &meetings {
        derive_heard_links_in(&connection, *meeting_id, None, 10).unwrap();
    }
    let per_meeting_us = started.elapsed().as_micros() / meetings.len() as u128;
    let (mut right, mut wrong) = (0, 0);
    for (fixture, meeting_id) in MEETINGS.iter().zip(&meetings) {
        let found = linked(&connection, *meeting_id);
        let expected = fixture
            .expected
            .iter()
            .map(|name| (*name).to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(found, expected, "{:?} {:?}", fixture.title, fixture.lines);
        let (meeting_right, meeting_wrong) = score(&found, fixture.expected);
        right += meeting_right;
        wrong += meeting_wrong;
    }
    println!(
        "people links: title only {title_right}/{wanted} found, {title_wrong} wrong; \
         with transcript and roster {right}/{wanted} found, {wrong} wrong; \
         {per_meeting_us} us per meeting"
    );
    let unconfirmed: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM meeting_person_links WHERE confidence != 'suggested'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unconfirmed, 0, "a name being said must stay a suggestion");
}

/// Issue #45's finish line: a huddle with no calendar event, a person who is
/// addressed by name, one suggestion and a receipt that counts the meeting.
#[test]
fn finalizing_suggests_a_person_and_the_receipt_counts_the_meeting() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Slack meeting", 1);
    person(&store, "Dana Whitfield", &["Dee"], &[]);
    transcript_segments(&store, meeting_id, &["Thanks Dana, that covers it."]);

    let dispatch = store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingFinalized,
                serde_json::json!({
                    "session_id": meeting_id.uuid().to_string(),
                    "known_vocabulary": []
                }),
                "heard-finalized",
            ),
            &inputs(),
        )
        .unwrap();

    let receipt = dispatch
        .receipts
        .iter()
        .find(|receipt| receipt.workflow_id == WorkflowId::PersonLinking)
        .unwrap();
    assert_eq!(receipt.outcome_counts.meetings, 1);
    assert_eq!(receipt.outcome_counts.suggestions, 1);
    assert_eq!(receipt.outcome_counts.changes, 1);
    let connection = store.connection().unwrap();
    let (source, confidence): (String, String) = connection
        .query_row(
            "SELECT source, confidence FROM meeting_person_links WHERE meeting_id = ?1",
            [meeting_id.uuid().to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (source.as_str(), confidence.as_str()),
        ("transcript", "suggested")
    );
}

#[test]
fn a_meeting_that_names_nobody_is_examined_and_finds_nothing() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Slack meeting", 1);
    person(&store, "Dana Whitfield", &[], &[]);
    transcript_segments(&store, meeting_id, &["Let's ship it this week."]);

    let dispatch = store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingFinalized,
                serde_json::json!({
                    "session_id": meeting_id.uuid().to_string(),
                    "known_vocabulary": []
                }),
                "heard-nobody",
            ),
            &inputs(),
        )
        .unwrap();

    let receipt = dispatch
        .receipts
        .iter()
        .find(|receipt| receipt.workflow_id == WorkflowId::PersonLinking)
        .unwrap();
    assert_eq!(
        (
            receipt.outcome_counts.meetings,
            receipt.outcome_counts.suggestions
        ),
        (1, 0)
    );
}

#[test]
fn the_users_own_name_and_named_voice_suggest_nobody() {
    let people = [Person {
        id: crate::meeting::people_types::PersonId::new(),
        display_name: "Jordan Vale".to_string(),
        aliases: Vec::new(),
        calendar_emails: Vec::new(),
        organization: None,
        summary: None,
        role: None,
        notes: None,
        created_at_utc_ms: 1,
        updated_at_utc_ms: 1,
    }];
    let lines = ["Thanks Jordan, that covers it.".to_string()];
    let heard = Heard {
        lines: &lines,
        listed: &[],
    };

    assert!(people_heard(&people, &heard, Some("Jordan Vale")).is_empty());
    assert_eq!(people_heard(&people, &heard, None).len(), 1);
}
