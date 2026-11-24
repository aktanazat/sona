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

#[test]
fn a_contact_whose_alias_is_the_users_full_name_is_found_by_their_own_name() {
    let people = [
        invented("Zelia Morse", &[]),
        invented("Tamsin Krill", &["Zelia Morse"]),
    ];
    let said = ["Can you ask Tamsin Krill about the contract?".to_string()];
    let spoken = Heard {
        lines: &said,
        listed: &[],
    };
    assert_eq!(
        names(people_heard(&people, &spoken, Some("Zelia Morse"))),
        ["Tamsin Krill"]
    );

    let listed = ["Tamsin Krill".to_string()];
    let roster = Heard {
        lines: &[],
        listed: &listed,
    };
    assert_eq!(
        names(people_heard(&people, &roster, Some("Zelia Morse"))),
        ["Tamsin Krill"]
    );
}

#[test]
fn a_stranger_whose_surname_is_a_contacts_first_name_is_not_that_contact() {
    let people = [invented("Dana Whitfield", &[])];
    for line in [
        "I asked Marcus Dana about the budget.",
        "Thanks Marcus Dana, send it over.",
        "That was Marcus Dana's idea.",
        "On Monday Marcus Dana joins us.",
    ] {
        let lines = [line.to_string()];
        let heard = Heard {
            lines: &lines,
            listed: &[],
        };
        assert!(people_heard(&people, &heard, None).is_empty(), "{line}");
    }

    let lines = ["Thanks Dana, send it over.".to_string()];
    let heard = Heard {
        lines: &lines,
        listed: &[],
    };
    assert_eq!(
        names(people_heard(&people, &heard, None)),
        ["Dana Whitfield"]
    );
}

#[test]
fn a_shared_multiword_name_or_a_name_with_a_suffix_suggests_nobody() {
    let people = [
        invented("Tess Nolan", &["Nolan Family"]),
        invented("Ben Ostrow", &["Nolan Family"]),
    ];
    let listed = ["Nolan Family".to_string()];
    let lines = ["Thanks Nolan Family, that covers it.".to_string()];
    let heard = Heard {
        lines: &lines,
        listed: &listed,
    };
    assert!(people_heard(&people, &heard, None).is_empty());

    let people = [invented("Tess Nolan", &[])];
    let suffixed = ["Thanks Tess Nolan Junior, that covers it.".to_string()];
    let heard = Heard {
        lines: &suffixed,
        listed: &[],
    };
    assert!(people_heard(&people, &heard, None).is_empty());
}

/// What the matching is meant to do, written out the long way so it does not
/// share code with `people_heard`: the meeting gave exactly `cue`, and a
/// person is named by it only through a name that is theirs alone.
fn should_suggest(people: &[Person], person: &Person, cue: &str, user: Option<&str>) -> bool {
    let same = |left: &str, right: &str| left.to_lowercase() == right.to_lowercase();
    let words = |name: &str| {
        name.split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    let user = user.map(|user| words(user).join(" "));
    let is_user = user
        .as_deref()
        .is_some_and(|user| same(&words(&person.display_name).join(" "), user));
    if is_user {
        return false;
    }
    let mut own = vec![person.display_name.clone()];
    own.extend(person.aliases.iter().cloned());
    own.extend(words(&person.display_name).first().cloned());
    own.iter().any(|name| {
        if name.contains(' ') {
            let holders = people
                .iter()
                .filter(|other| {
                    std::iter::once(&other.display_name)
                        .chain(&other.aliases)
                        .any(|held| held.contains(' ') && same(held, cue))
                })
                .count();
            return same(name, cue)
                && holders == 1
                && !user.as_deref().is_some_and(|user| same(user, cue));
        }
        if !same(name, cue) {
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
                                    let meant = should_suggest(&people, person, cue, *user);
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

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Invented recent meetings and whether each names "Dee", the alias the
/// person is about to be saved under.
const SAID_BEFORE_THE_SAVE: &[(&str, &[&str], bool)] = &[
    ("Slack meeting", &["Thanks Dee, send it over."], true),
    ("Call in Safari", &["I asked Dee about the renewal."], true),
    ("Dee sync", &["Let's start."], true),
    ("Zoom meeting", &["Dee, can you share your screen?"], true),
    ("Slack meeting", &["Let's ship it this week."], false),
    ("Zoom meeting", &["The deed is signed."], false),
    ("Slack meeting", &["Priya will send the notes."], false),
    ("Call in Safari", &["Indeed, that works."], false),
];

fn person_saved(
    store: &MeetingStore,
    person_id: PersonId,
    at_utc_ms: i64,
    dedupe_key: &str,
) -> crate::meeting::workflow_types::WorkflowOutcomeCounts {
    let mut saved = event(
        WorkflowEventKind::PersonSaved,
        serde_json::json!({ "person_id": person_id.uuid().to_string() }),
        dedupe_key,
    );
    saved.occurred_at_utc_ms = at_utc_ms;
    let dispatch = store
        .record_and_run_workflow_event(saved, &inputs())
        .unwrap();
    dispatch
        .receipts
        .iter()
        .find(|receipt| receipt.workflow_id == WorkflowId::PersonLinking)
        .unwrap()
        .outcome_counts
        .clone()
}

fn suggested_for(store: &MeetingStore, person_id: PersonId) -> BTreeSet<String> {
    let connection = store.connection().unwrap();
    let mut statement = connection
        .prepare(
            "SELECT meeting_id FROM meeting_person_links
              WHERE person_id = ?1 AND confidence = 'suggested'",
        )
        .unwrap();
    let meetings = statement
        .query_map([person_id.uuid().to_string()], |row| row.get(0))
        .unwrap()
        .collect::<Result<BTreeSet<String>, _>>()
        .unwrap();
    meetings
}

#[test]
fn saving_an_alias_suggests_the_recent_meetings_that_used_it() {
    let (_directory, store) = store();
    let now = 200 * DAY_MS;
    let dana = person(&store, "Dana Whitfield", &[], &[]);
    person(&store, "Priya Raman", &[], &[]);
    let meetings = SAID_BEFORE_THE_SAVE
        .iter()
        .enumerate()
        .map(|(index, (title, lines, _))| {
            let at = now - i64::try_from(index + 1).unwrap() * DAY_MS;
            let meeting_id = meeting(&store, title, at);
            transcript_segments(&store, meeting_id, lines);
            meeting_id
        })
        .collect::<Vec<_>>();
    let expected = SAID_BEFORE_THE_SAVE
        .iter()
        .zip(&meetings)
        .filter(|((_, _, named), _)| *named)
        .map(|(_, meeting_id)| meeting_id.uuid().to_string())
        .collect::<BTreeSet<_>>();

    // What a save did before: nothing, so each meeting only has what its own
    // finalizing found, which ran before the alias existed.
    {
        let connection = store.connection().unwrap();
        for ((title, _, _), meeting_id) in SAID_BEFORE_THE_SAVE.iter().zip(&meetings) {
            derive_title_links_in(&connection, *meeting_id, title, 10).unwrap();
            derive_heard_links_in(&connection, *meeting_id, None, 10).unwrap();
        }
    }
    let before = suggested_for(&store, dana);

    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE persons SET aliases_json = '[\"Dee\"]' WHERE id = ?1",
            [dana.uuid().to_string()],
        )
        .unwrap();
    let started = Instant::now();
    let counts = person_saved(&store, dana, now, "saved-dee");
    let elapsed = started.elapsed();
    let after = suggested_for(&store, dana);

    println!(
        "person save: before {}/{} found, {} wrong; after {}/{} found, {} wrong; {} us for {} meetings",
        before.intersection(&expected).count(),
        expected.len(),
        before.difference(&expected).count(),
        after.intersection(&expected).count(),
        expected.len(),
        after.difference(&expected).count(),
        elapsed.as_micros(),
        meetings.len(),
    );
    assert!(before.is_empty());
    assert_eq!(after, expected);
    assert_eq!(
        (counts.meetings, counts.changes, counts.suggestions),
        (8, 4, 4)
    );
}

#[test]
fn a_save_reads_only_recent_finished_meetings_and_only_for_that_person() {
    let (_directory, store) = store();
    let now = 200 * DAY_MS;
    let dana = person(&store, "Dana Whitfield", &[], &[]);
    let priya = person(&store, "Priya Raman", &[], &[]);
    let old = meeting(&store, "Slack meeting", now - 120 * DAY_MS);
    transcript_segments(&store, old, &["Thanks Dana, send it over."]);
    let recording = meeting(&store, "Slack meeting", now - DAY_MS);
    transcript_segments(&store, recording, &["Thanks Dana, send it over."]);
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_sessions SET phase = 'processing' WHERE id = ?1",
            [recording.uuid().to_string()],
        )
        .unwrap();
    let recent = meeting(&store, "Slack meeting", now - 2 * DAY_MS);
    transcript_segments(&store, recent, &["Dana and Priya Raman both joined."]);

    let counts = person_saved(&store, dana, now, "saved-dana");

    assert_eq!(
        (counts.meetings, counts.changes, counts.suggestions),
        (1, 1, 1)
    );
    assert_eq!(
        suggested_for(&store, dana),
        BTreeSet::from([recent.uuid().to_string()])
    );
    assert!(suggested_for(&store, priya).is_empty());
}

#[test]
fn a_save_with_no_recent_meetings_says_none_were_read() {
    let (_directory, store) = store();
    let dana = person(&store, "Dana Whitfield", &[], &[]);

    let counts = person_saved(&store, dana, 200 * DAY_MS, "saved-nothing");

    assert_eq!(
        (counts.meetings, counts.changes, counts.suggestions),
        (0, 0, 0)
    );
}

#[test]
fn a_save_never_confirms_and_leaves_a_confirmed_link_alone() {
    let (_directory, store) = store();
    let now = 200 * DAY_MS;
    let dana = person(&store, "Dana Whitfield", &[], &[]);
    let meeting_id = meeting(&store, "Slack meeting", now - DAY_MS);
    transcript_segments(&store, meeting_id, &["Thanks Dana, send it over."]);
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_person_links
                (meeting_id, person_id, source, confidence, created_at_utc_ms)
             VALUES (?1, ?2, 'manual', 'confirmed', 1)",
            params![meeting_id.uuid().to_string(), dana.uuid().to_string()],
        )
        .unwrap();

    let counts = person_saved(&store, dana, now, "saved-confirmed");

    assert_eq!(
        (counts.meetings, counts.changes, counts.suggestions),
        (1, 0, 0)
    );
    let (source, confidence): (String, String) = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT source, confidence FROM meeting_person_links WHERE meeting_id = ?1",
            [meeting_id.uuid().to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (source.as_str(), confidence.as_str()),
        ("manual", "confirmed")
    );
}

#[test]
fn only_a_save_that_changes_the_names_reads_meetings_again() {
    use crate::meeting::workflow_engine::{person_names, person_saved_event};

    // A role or an address is not a name; which name is the display name is.
    let mut edited = invented("Dee Whitfield", &["Dana"]);
    let names = person_names(&edited);
    edited.role = Some("Buyer".to_string());
    edited.calendar_emails = vec!["dee@example.invalid".to_string()];
    assert_eq!(person_names(&edited), names);
    assert_ne!(person_names(&invented("Dana", &["Dee Whitfield"])), names);

    let (_directory, store) = store();
    let now = 200 * DAY_MS;
    let dana = person(&store, "Dana Whitfield", &[], &[]);
    let meeting_id = meeting(&store, "Slack meeting", now - DAY_MS);
    transcript_segments(&store, meeting_id, &["Thanks Dee, send it over."]);
    let mut saved = invented("Dana Whitfield", &[]);
    saved.id = dana;

    let first = store
        .record_and_run_workflow_event(person_saved_event(&saved, now), &inputs())
        .unwrap();
    saved.role = Some("Buyer".to_string());
    let same_names = store
        .record_and_run_workflow_event(person_saved_event(&saved, now + 1), &inputs())
        .unwrap();
    saved.aliases = vec!["dee".to_string()];
    let lowercase = person_saved_event(&saved, now + 2).dedupe_key;
    saved.aliases = vec!["Dee".to_string()];
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE persons SET aliases_json = '[\"Dee\"]' WHERE id = ?1",
            [dana.uuid().to_string()],
        )
        .unwrap();
    let new_alias = person_saved_event(&saved, now + 2);
    assert_eq!(new_alias.dedupe_key, lowercase);
    let renamed = store
        .record_and_run_workflow_event(new_alias, &inputs())
        .unwrap();

    assert!(first.inserted);
    // The engine runs an event only when it is new.
    assert!(!same_names.inserted);
    assert!(renamed.inserted);
    assert_eq!(
        suggested_for(&store, dana),
        BTreeSet::from([meeting_id.uuid().to_string()])
    );
}
