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

fn invented(display_name: &str, aliases: &[&str]) -> Person {
    Person {
        id: crate::meeting::people_types::PersonId::new(),
        display_name: display_name.to_string(),
        aliases: aliases.iter().map(|alias| (*alias).to_string()).collect(),
        calendar_emails: Vec::new(),
        organization: None,
        summary: None,
        role: None,
        notes: None,
        created_at_utc_ms: 1,
        updated_at_utc_ms: 1,
    }
}

fn names(found: Vec<&Person>) -> Vec<&str> {
    found
        .into_iter()
        .map(|person| person.display_name.as_str())
        .collect()
}

#[test]
fn a_nickname_two_people_could_answer_to_suggests_neither() {
    let people = [
        invented("Robin Castell", &["Robin"]),
        invented("Robin Adeyemi", &[]),
        invented("Kit", &[]),
        invented("Kit Morland", &[]),
    ];
    let lines = ["Thanks Robin, and thanks Kit.".to_string()];
    let listed = ["Robin".to_string(), "Kit".to_string()];
    let vague = Heard {
        lines: &lines,
        listed: &listed,
    };
    assert!(people_heard(&people, &vague, None).is_empty());

    let full = ["Robin Adeyemi".to_string()];
    let exact = Heard {
        lines: &[],
        listed: &full,
    };
    assert_eq!(
        names(people_heard(&people, &exact, None)),
        ["Robin Adeyemi"]
    );
}

#[test]
fn a_contact_who_shares_the_users_first_name_is_found_by_their_full_name() {
    let people = [invented("Jordan Pryce", &["Jordan"])];
    let said = ["Jordan, can you hear us?".to_string()];
    let first_name_only = Heard {
        lines: &said,
        listed: &[],
    };
    assert!(people_heard(&people, &first_name_only, Some("Jordan Vale")).is_empty());

    let listed = ["Jordan Pryce".to_string()];
    let full_name = Heard {
        lines: &[],
        listed: &listed,
    };
    assert_eq!(
        names(people_heard(&people, &full_name, Some("Jordan Vale"))),
        ["Jordan Pryce"]
    );
}

#[test]
fn the_users_own_entry_is_not_suggested_by_its_nickname() {
    let people = [invented("Jordan Vale", &["Jojo"])];
    let lines = ["Thanks Jojo, that covers it.".to_string()];
    let listed = ["Jojo".to_string()];
    let heard = Heard {
        lines: &lines,
        listed: &listed,
    };

    assert!(people_heard(&people, &heard, Some("Jordan Vale")).is_empty());
    assert_eq!(people_heard(&people, &heard, None).len(), 1);
}

#[test]
fn a_contact_whose_alias_is_the_users_whole_name_is_found_by_their_own() {
    let people = [invented("Jordan Pryce", &["Jordan"])];
    let listed = ["Jordan Pryce".to_string()];
    let lines = ["Jordan, are you there?".to_string()];
    let full_name = Heard {
        lines: &lines,
        listed: &listed,
    };
    assert_eq!(
        names(people_heard(&people, &full_name, Some("Jordan"))),
        ["Jordan Pryce"]
    );

    let alias_only = Heard {
        lines: &lines,
        listed: &[],
    };
    assert!(people_heard(&people, &alias_only, Some("Jordan")).is_empty());
}

/// What the matching is meant to do, written out the long way so it does not
/// share code with `people_heard`: the meeting gave exactly `cue`, and a
/// person is named by it only through a name that is theirs alone. Said
/// aloud, a cue also ends on its last word, so a stranger whose surname is a
/// contact's only-theirs first name still reads as that contact.
fn should_suggest(
    people: &[Person],
    person: &Person,
    cue: &str,
    spoken: bool,
    user: Option<&str>,
) -> bool {
    let same = |left: &str, right: &str| left.to_lowercase() == right.to_lowercase();
    let words = |name: &str| {
        name.split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    let user = user.map(|user| words(user).join(" "));
    let is_user = user.as_deref().is_some_and(|user| {
        same(&words(&person.display_name).join(" "), user)
            || (user.contains(' ') && person.aliases.iter().any(|alias| same(alias, user)))
    });
    if is_user {
        return false;
    }
    let mut own = vec![person.display_name.clone()];
    own.extend(person.aliases.iter().cloned());
    own.extend(words(&person.display_name).first().cloned());
    own.iter().any(|name| {
        if name.contains(' ') {
            return same(name, cue);
        }
        let ends_on = spoken && cue.split(' ').last().is_some_and(|last| same(name, last));
        if !same(name, cue) && !ends_on {
            return false;
        }
        let word = name.to_lowercase();
        let users = user.as_deref().is_some_and(|user| {
            user.split(' ').any(|part| part == word) || user.replace(' ', "") == word
        });
        let could_mean = people
            .iter()
            .filter(|other| {
                std::iter::once(&other.display_name)
                    .chain(&other.aliases)
                    .any(|name| words(name).contains(&word))
            })
            .count();
        !users && could_mean == 1
    })
}

#[test]
fn every_mix_of_user_name_nicknames_and_shared_names_matches_as_meant() {
    const USERS: &[Option<&str>] = &[
        None,
        Some("Jordan"),
        Some("Jordan Vale"),
        Some("jordan vale"),
        Some("Mary Ann"),
    ];
    const FIRST: &[&str] = &[
        "Jordan Vale",
        "Jordan Pryce",
        "Dana Whitfield",
        "Jordan",
        "Vale Morrow",
        "Maryann Smith",
    ];
    const FIRST_ALIASES: &[&[&str]] = &[
        &[],
        &["Jojo"],
        &["Jordan"],
        &["Jordan Vale"],
        &["Dee"],
        &["Jojo", "Jordan"],
        &["Vale"],
        &["Jordan Vale", "Jojo"],
    ];
    const SECOND: &[Option<&str>] = &[
        None,
        Some("Jordan Pryce"),
        Some("Dana Reed"),
        Some("Priya Dana"),
        Some("Joanna Reed"),
        Some("Jordan Vale"),
    ];
    const SECOND_ALIASES: &[&[&str]] = &[&[], &["Jojo"], &["Dee"], &["Jordan"], &["Dana"]];
    const CUES: &[&str] = &[
        "Jordan Vale",
        "Jordan Pryce",
        "Dana Whitfield",
        "Dana Reed",
        "Priya Dana",
        "Joanna Reed",
        "Vale Morrow",
        "Dana Okafor",
        "Marcus Dana",
        "Jordan",
        "Dana",
        "Jojo",
        "Dee",
        "Vale",
        "Priya",
        "Maryann",
    ];

    let mut checked = 0;
    let mut wrong = Vec::new();
    for user in USERS {
        for first in FIRST {
            for first_aliases in FIRST_ALIASES {
                for second in SECOND {
                    for second_aliases in SECOND_ALIASES {
                        let mut people = vec![invented(first, first_aliases)];
                        match second {
                            Some(second) => people.push(invented(second, second_aliases)),
                            None if !second_aliases.is_empty() => continue,
                            None => {}
                        }
                        for cue in CUES {
                            let said = [
                                vec![format!("I asked {cue} about it.")],
                                vec![format!("Thanks {cue}, send it over.")],
                                vec![format!("That was {cue}'s idea.")],
                                vec![format!("On Monday {cue} joins us.")],
                                vec![format!("We need {cue} ASAP.")],
                                vec![format!("So {cue} I think we ship.")],
                                Vec::new(),
                            ];
                            for (index, lines) in said.iter().enumerate() {
                                let listed = if lines.is_empty() {
                                    vec![(*cue).to_string()]
                                } else {
                                    Vec::new()
                                };
                                let heard = Heard {
                                    lines,
                                    listed: &listed,
                                };
                                let found = people_heard(&people, &heard, *user)
                                    .into_iter()
                                    .map(|person| person.id)
                                    .collect::<Vec<_>>();
                                for person in &people {
                                    checked += 1;
                                    let meant = should_suggest(
                                        &people,
                                        person,
                                        cue,
                                        !lines.is_empty(),
                                        *user,
                                    );
                                    if found.contains(&person.id) != meant {
                                        wrong.push(format!(
                                            "user {user:?}, people {:?}, cue {cue:?} (form {index}): {} {}",
                                            people
                                                .iter()
                                                .map(|person| (&person.display_name, &person.aliases))
                                                .collect::<Vec<_>>(),
                                            person.display_name,
                                            if meant { "missed" } else { "wrongly suggested" },
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    println!(
        "name combinations: {} of {checked} decisions wrong",
        wrong.len()
    );
    for case in wrong.iter().take(12) {
        println!("  {case}");
    }
    assert!(wrong.is_empty());
}
