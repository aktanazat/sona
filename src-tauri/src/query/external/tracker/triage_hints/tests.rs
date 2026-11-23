use super::super::tests::{board, row};
use super::super::{Board, Fence};
use super::{decided_before, file, Decided, Outcome, SuggestionAction};
use crate::meeting::store::workflow_core_tests::{link, meeting, person, store};
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::MeetingSessionId;
use crate::planning::schema;
use crate::planning::store::{self as planning_store, Refusal};
use crate::planning::work::agents::AgentChange;
use crate::planning::work::items::{self, NewItem};
use crate::planning::work::sync::{self, MeetingReader};
use crate::planning::work::types::{
    WorkActor, WorkFieldSuggestion, WorkItem, WorkOwner, WorkStatus, WorkSuggestionField,
};
use crate::query::external::ExternalErrorCode;
use crate::query::KeptLocal;
use rusqlite::Connection;
use std::collections::HashSet;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

const AT: i64 = 1_700_000_000_000;

/// A tracker beside a meeting database, and the clock both read.
struct Desk {
    _directory: TempDir,
    meetings: Arc<MeetingStore>,
    history: Connection,
    now: i64,
}

impl Desk {
    fn new() -> Self {
        let (directory, meetings) = store();
        // PANIC: a fixture that cannot open its database is a broken test.
        let history = Connection::open_in_memory().expect("an in-memory history database");
        // PANIC: as above, for the planner schema.
        schema::apply_all(&history).expect("the planner schema");
        Self {
            _directory: directory,
            meetings,
            history,
            now: AT,
        }
    }

    fn project(&mut self, title: &str) -> String {
        // PANIC: a fixture project in an empty tracker cannot clash.
        planning_store::create_project(
            &mut self.history,
            title,
            None,
            &Uuid::new_v4().to_string(),
            self.now,
        )
        .expect("a project")
        .id
    }

    /// A task filed a second after the last, raised in `raised_in` when given.
    fn task(
        &mut self,
        title: &str,
        status: WorkStatus,
        project_id: Option<&str>,
        owner: &WorkOwner,
        raised_in: Option<MeetingSessionId>,
    ) -> WorkItem {
        self.now += 1_000;
        let source_loop_id =
            raised_in.map(|meeting| format!("{}:loop:{:016x}", meeting.uuid(), self.now));
        // PANIC: filing a fixture task in its own tracker cannot fail.
        items::insert(
            &self.history,
            &NewItem {
                title,
                details: None,
                status,
                priority: 0,
                project_id,
                milestone_id: None,
                owner,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: source_loop_id.as_deref(),
                recurrence: None,
            },
            WorkActor::Me,
            self.now,
        )
        .expect("a filed task")
    }

    /// What Triage shows for `item`, read the way the task list reads it.
    fn suggestions(&self, item: &WorkItem) -> Vec<WorkFieldSuggestion> {
        // PANIC: reading the fixture's own tracker cannot fail.
        let all = items::items(&self.history, false).expect("the tasks");
        let mut failed = None;
        let read = self.meetings.with_planning_snapshot(|meetings| {
            let mut reader = MeetingReader::new(Some(meetings), None);
            sync::suggestions(&self.history, &mut reader, &all, item).map_err(|error| {
                failed = Some(error);
                StoreError::Unavailable
            })
        });
        // PANIC: as above, for the meeting database beside it.
        read.unwrap_or_else(|error| panic!("the suggestions: {failed:?} {error:?}"))
    }
}

fn reasons<'a>(
    suggestions: &'a [WorkFieldSuggestion],
    fields: &[WorkSuggestionField],
) -> Vec<&'a str> {
    suggestions
        .iter()
        .filter(|suggestion| fields.contains(&suggestion.field))
        .map(|suggestion| suggestion.reason.as_str())
        .collect()
}

/// A suggestion whose reason counts tasks from a meeting kept on this Mac
/// never reaches an agent, though the task it is for came from a meeting the
/// agent may read. The same suggestion reaches it once nothing is kept.
#[test]
fn a_reason_counting_a_meeting_kept_on_this_mac_never_reaches_an_agent() {
    let mut desk = Desk::new();
    let dana = person(&desk.meetings, "Dana Reyes", &[], &[]);
    let kept = meeting(&desk.meetings, "Pier walk", AT);
    let today = meeting(&desk.meetings, "Pier sync", AT + 86_400_000);
    link(&desk.meetings, kept, dana, "calendar", "confirmed");
    link(&desk.meetings, today, dana, "calendar", "confirmed");
    let lighting = desk.project("Pier Lighting");
    for title in ["Order the bulbs", "Fix the ramp"] {
        desk.task(
            title,
            WorkStatus::NotStarted,
            Some(&lighting),
            &WorkOwner::Nobody,
            Some(kept),
        );
    }
    let new = desk.task(
        "Call the electrician",
        WorkStatus::Triage,
        None,
        &WorkOwner::Nobody,
        Some(today),
    );

    let suggestions = desk.suggestions(&new);
    let read = |fence: &Fence| -> Vec<&str> {
        suggestions
            .iter()
            .filter(|suggestion| !fence.hides_suggestion(suggestion))
            .map(|suggestion| suggestion.reason.as_str())
            .collect()
    };

    assert_eq!(
        read(&Fence::Open(KeptLocal(HashSet::new()))),
        ["The last 2 tasks from meetings with Dana Reyes went to Pier Lighting"]
    );
    assert_eq!(
        read(&Fence::Open(KeptLocal(HashSet::from([kept])))),
        Vec::<&str>::new()
    );
}

/// One past task is not a pattern: a Triage task titled like a single
/// earlier one gets no project or owner from it, and gets both once a second
/// earlier task agrees.
#[test]
fn one_similar_task_is_too_little_to_suggest_from() {
    let mut desk = Desk::new();
    let priya = WorkOwner::Person {
        person_id: person(&desk.meetings, "Priya Nair", &[], &[])
            .uuid()
            .to_string(),
    };
    let lighting = desk.project("Pier Lighting");
    desk.task(
        "Price LED fixtures for the pier",
        WorkStatus::NotStarted,
        Some(&lighting),
        &priya,
        None,
    );
    let new = desk.task(
        "Install LED fixtures on the pier",
        WorkStatus::Triage,
        None,
        &WorkOwner::Nobody,
        None,
    );
    let decided = [WorkSuggestionField::Project, WorkSuggestionField::Owner];

    assert_eq!(
        reasons(&desk.suggestions(&new), &decided),
        Vec::<&str>::new()
    );

    desk.task(
        "Order LED fixtures for the pier",
        WorkStatus::NotStarted,
        Some(&lighting),
        &priya,
        None,
    );

    assert_eq!(
        reasons(&desk.suggestions(&new), &decided),
        [
            "The last 2 tasks with similar titles went to Pier Lighting",
            "The last 2 tasks with similar titles went to Priya Nair",
        ]
    );
}

/// A decision made under an operation id answers again, word for word but
/// marked replayed, when the same id asks again; the id decides nothing
/// else; and once the meeting its reason counts is kept on this Mac, the
/// answer is gone rather than repeated.
#[test]
fn the_same_operation_id_answers_with_its_first_decision() {
    let mut desk = Desk::new();
    let dana = person(&desk.meetings, "Dana Reyes", &[], &[]);
    let walk = meeting(&desk.meetings, "Pier walk", AT);
    let sync = meeting(&desk.meetings, "Pier sync", AT + 86_400_000);
    link(&desk.meetings, walk, dana, "calendar", "confirmed");
    link(&desk.meetings, sync, dana, "calendar", "confirmed");
    let lighting = desk.project("Pier Lighting");
    for title in ["Order the bulbs", "Fix the ramp"] {
        desk.task(
            title,
            WorkStatus::NotStarted,
            Some(&lighting),
            &WorkOwner::Nobody,
            Some(walk),
        );
    }
    let new = desk.task(
        "Call the electrician",
        WorkStatus::Triage,
        None,
        &WorkOwner::Nobody,
        Some(sync),
    );
    let suggestion = desk
        .suggestions(&new)
        .into_iter()
        .find(|suggestion| suggestion.field == WorkSuggestionField::Project)
        .expect("a project suggestion");
    let id = suggestion.id.clone();
    let decided = Decided {
        item: new.clone(),
        suggestion,
        outcome: Outcome::Changed(AgentChange {
            item: WorkItem {
                project_id: Some(lighting),
                revision: new.revision + 1,
                ..new.clone()
            },
            proposed: Vec::new(),
        }),
    };
    let seen = |kept: HashSet<MeetingSessionId>| Board {
        fence: Fence::Open(KeptLocal(kept)),
        ..board(vec![row(new.clone())], Vec::new())
    };
    let operation_id = Uuid::new_v4();
    let accepted = SuggestionAction::Accepted;
    let answer = |decided: &Decided, board: &Board, replayed| {
        decided
            .answer(board, accepted, operation_id.to_string(), replayed)
            .map(|answer| serde_json::to_value(answer).expect("the answer as JSON"))
    };

    assert!(decided_before(&desk.history, operation_id, accepted, &id)
        .expect("the ledger")
        .is_none());
    file(&desk.history, operation_id, accepted, &decided).expect("the decision filed");
    let again = decided_before(&desk.history, operation_id, accepted, &id)
        .expect("the ledger")
        .expect("the first decision");

    let mut first = answer(&decided, &seen(HashSet::new()), false).expect("the first answer");
    first["replayed"] = true.into();
    first["change"]["replayed"] = true.into();
    assert_eq!(
        answer(&again, &seen(HashSet::new()), true).ok(),
        Some(first)
    );
    assert_eq!(
        decided_before(
            &desk.history,
            operation_id,
            SuggestionAction::Dismissed,
            &id
        )
        .err()
        .and_then(|error| error.downcast_ref::<Refusal>().copied()),
        Some(Refusal::OperationConflict)
    );
    assert_eq!(
        answer(&again, &seen(HashSet::from([walk])), true)
            .err()
            .map(|error| error.error),
        Some(ExternalErrorCode::NotFound)
    );
}

/// The user's 686-item report, rebuilt from invented names: four projects,
/// each with its own lead, and one colleague who sits in every meeting.
/// Accepting Triage with Sona's suggestions must file each meeting's work
/// under its own project or leave it for the user, never under another
/// project. `started` is how many of the projects already hold tasks.
fn misfiled_after_a_busy_month(started: usize) -> (usize, Vec<usize>) {
    const ITEMS: usize = 686;
    let mut desk = Desk::new();
    let everywhere = person(&desk.meetings, "Sam Ortiz", &[], &[]);
    let streams: Vec<_> = [
        ("Pier Lighting", "Dana Reyes"),
        ("Harbor Dredging", "Priya Nair"),
        ("Ferry Timetable", "Marco Bell"),
        ("Market Stalls", "Lena Fischer"),
    ]
    .into_iter()
    .map(|(project, lead)| {
        (
            desk.project(project),
            person(&desk.meetings, lead, &[], &[]),
        )
    })
    .collect();
    let held = |desk: &mut Desk, stream: usize, title: &str| {
        desk.now += 3_600_000;
        let session = meeting(&desk.meetings, title, desk.now);
        link(&desk.meetings, session, everywhere, "calendar", "confirmed");
        link(
            &desk.meetings,
            session,
            streams[stream].1,
            "calendar",
            "confirmed",
        );
        session
    };
    // What the user filed by hand before the month began; the first project
    // is the busiest and the newest.
    for stream in (0..started).rev() {
        let kickoff = held(&mut desk, stream, "Kickoff");
        for task in 0..if stream == 0 { 3 } else { 2 } {
            desk.task(
                &format!("Kickoff s{stream}k{task}a s{stream}k{task}b"),
                WorkStatus::NotStarted,
                Some(&streams[stream].0),
                &WorkOwner::Nobody,
                Some(kickoff),
            );
        }
    }
    let mut filed = vec![0; streams.len()];
    let mut misfiled = 0;
    let mut session = None;
    for index in 0..ITEMS {
        let stream = index % streams.len();
        // Two items a meeting, the streams taking turns.
        if index % (2 * streams.len()) < streams.len() {
            session = None;
        }
        let raised_in = match session {
            Some(sessions) => sessions,
            None => [0, 1, 2, 3].map(|stream| held(&mut desk, stream, "Weekly")),
        };
        session = Some(raised_in);
        let item = desk.task(
            &format!("Follow n{index}a n{index}b n{index}c"),
            WorkStatus::Triage,
            None,
            &WorkOwner::Nobody,
            Some(raised_in[stream]),
        );
        let mut failed = None;
        let accepted = desk.meetings.with_planning_snapshot(|meetings| {
            let mut reader = MeetingReader::new(Some(meetings), None);
            sync::acceptance(&desk.history, &mut reader, &item, true, None)
                .and_then(|patch| {
                    sync::accept(&desk.history, None, &item, &patch, WorkActor::Me, desk.now)
                })
                .map_err(|error| {
                    failed = Some(error);
                    StoreError::Unavailable
                })
        });
        // PANIC: accepting a fixture's own Triage item cannot fail.
        let (accepted, _) =
            accepted.unwrap_or_else(|error| panic!("the acceptance: {failed:?} {error:?}"));
        match accepted.project_id {
            Some(project) if project == streams[stream].0 => filed[stream] += 1,
            Some(_) => misfiled += 1,
            None => {}
        }
    }
    (misfiled, filed)
}

/// With every project already holding a few tasks, a month of Triage lands
/// on its own project: none of the 686 items is filed under another one.
#[test]
fn a_colleague_in_every_meeting_does_not_pull_every_task_into_one_project() {
    let (misfiled, filed) = misfiled_after_a_busy_month(4);
    println!("686 accepted: {misfiled} under another project, {filed:?} under their own");
    assert_eq!((misfiled, filed), (0, vec![172, 172, 171, 171]));
}

/// With only the first project started, the other three projects' work
/// stays unfiled for the user to place instead of joining the first.
#[test]
fn work_for_projects_with_no_tasks_yet_is_left_for_the_user() {
    let (misfiled, filed) = misfiled_after_a_busy_month(1);
    println!("686 accepted: {misfiled} under another project, {filed:?} under their own");
    assert_eq!((misfiled, filed), (0, vec![172, 0, 0, 0]));
}

/// The specialist an independent review raised: in three of the four
/// meetings with people on them, every task of theirs under one project,
/// and two tasks elsewhere that share the new item's words. Their meetings
/// still say where the item goes, and adding unrelated meetings changes
/// nothing.
#[test]
fn a_specialist_in_most_meetings_still_suggests_their_project() {
    let mut desk = Desk::new();
    let ines = person(&desk.meetings, "Ines Moreau", &[], &[]);
    let optics = desk.project("Optics");
    let power = desk.project("Power");
    for day in 0..2 {
        let held = meeting(&desk.meetings, "Bench review", AT + day * 86_400_000);
        link(&desk.meetings, held, ines, "calendar", "confirmed");
        for title in ["Align the lens mount", "Polish the mirror"] {
            desk.task(
                &format!("{title} {day}"),
                WorkStatus::NotStarted,
                Some(&optics),
                &WorkOwner::Nobody,
                Some(held),
            );
        }
    }
    for title in [
        "Calibrate the bench supply",
        "Calibrate the bench supply rail",
    ] {
        desk.task(
            title,
            WorkStatus::NotStarted,
            Some(&power),
            &WorkOwner::Nobody,
            None,
        );
    }
    let other = person(&desk.meetings, "Tom Hale", &[], &[]);
    let aside = meeting(&desk.meetings, "Budget", AT + 3 * 86_400_000);
    link(&desk.meetings, aside, other, "calendar", "confirmed");
    let today = meeting(&desk.meetings, "Bench review", AT + 4 * 86_400_000);
    link(&desk.meetings, today, ines, "calendar", "confirmed");
    let new = desk.task(
        "Calibrate the bench supply meter",
        WorkStatus::Triage,
        None,
        &WorkOwner::Nobody,
        Some(today),
    );
    let project = [WorkSuggestionField::Project];
    let expected = ["The last 4 tasks from meetings with Ines Moreau went to Optics"];

    assert_eq!(reasons(&desk.suggestions(&new), &project), expected);

    for day in 5..7 {
        let more = meeting(&desk.meetings, "Budget", AT + day * 86_400_000);
        link(&desk.meetings, more, other, "calendar", "confirmed");
    }

    assert_eq!(reasons(&desk.suggestions(&new), &project), expected);
}

/// Two people whose work goes to different projects: the one with more
/// tasks behind them decides, as before, and a weaker match on the title
/// does not win in their place.
#[test]
fn of_two_attendees_who_disagree_the_one_with_more_behind_them_decides() {
    let mut desk = Desk::new();
    let ines = person(&desk.meetings, "Ines Moreau", &[], &[]);
    let tom = person(&desk.meetings, "Tom Hale", &[], &[]);
    let optics = desk.project("Optics");
    let power = desk.project("Power");
    let frames = desk.project("Frames");
    let bench = meeting(&desk.meetings, "Bench review", AT);
    link(&desk.meetings, bench, ines, "calendar", "confirmed");
    for title in [
        "Align the lens mount",
        "Polish the mirror",
        "Coat the prism",
    ] {
        desk.task(
            title,
            WorkStatus::NotStarted,
            Some(&optics),
            &WorkOwner::Nobody,
            Some(bench),
        );
    }
    let supply = meeting(&desk.meetings, "Supply review", AT + 86_400_000);
    link(&desk.meetings, supply, tom, "calendar", "confirmed");
    for title in ["Order the fuses", "Test the inverter"] {
        desk.task(
            title,
            WorkStatus::NotStarted,
            Some(&power),
            &WorkOwner::Nobody,
            Some(supply),
        );
    }
    for title in ["Weld the corner bracket", "Weld the corner bracket again"] {
        desk.task(
            title,
            WorkStatus::NotStarted,
            Some(&frames),
            &WorkOwner::Nobody,
            None,
        );
    }
    let today = meeting(&desk.meetings, "Joint review", AT + 2 * 86_400_000);
    link(&desk.meetings, today, ines, "calendar", "confirmed");
    link(&desk.meetings, today, tom, "calendar", "confirmed");
    let new = desk.task(
        "Weld the corner bracket cover",
        WorkStatus::Triage,
        None,
        &WorkOwner::Nobody,
        Some(today),
    );

    assert_eq!(
        reasons(&desk.suggestions(&new), &[WorkSuggestionField::Project]),
        ["The last 3 tasks from meetings with Ines Moreau went to Optics"]
    );
}

/// What decided that a person's meetings speak for a project is counted
/// over tasks outside the five the reason names. A meeting kept on this Mac
/// behind any of them keeps the suggestion from an agent.
#[test]
fn a_kept_meeting_that_was_only_weighed_still_hides_the_suggestion() {
    let mut desk = Desk::new();
    let dana = person(&desk.meetings, "Dana Reyes", &[], &[]);
    let tom = person(&desk.meetings, "Tom Hale", &[], &[]);
    let lighting = desk.project("Pier Lighting");
    let dredging = desk.project("Harbor Dredging");
    // Older than the five the reason will name, and elsewhere.
    let kept = meeting(&desk.meetings, "Channel walk", AT);
    link(&desk.meetings, kept, tom, "calendar", "confirmed");
    desk.task(
        "Sound the channel",
        WorkStatus::NotStarted,
        Some(&dredging),
        &WorkOwner::Nobody,
        Some(kept),
    );
    let walk = meeting(&desk.meetings, "Pier walk", AT + 86_400_000);
    link(&desk.meetings, walk, dana, "calendar", "confirmed");
    for title in ["Order the bulbs", "Fix the ramp"] {
        desk.task(
            title,
            WorkStatus::NotStarted,
            Some(&lighting),
            &WorkOwner::Nobody,
            Some(walk),
        );
    }
    let today = meeting(&desk.meetings, "Pier sync", AT + 2 * 86_400_000);
    link(&desk.meetings, today, dana, "calendar", "confirmed");
    let new = desk.task(
        "Call the electrician",
        WorkStatus::Triage,
        None,
        &WorkOwner::Nobody,
        Some(today),
    );

    let suggestions = desk.suggestions(&new);
    let read = |fence: &Fence| -> Vec<&str> {
        suggestions
            .iter()
            .filter(|suggestion| !fence.hides_suggestion(suggestion))
            .map(|suggestion| suggestion.reason.as_str())
            .collect()
    };

    assert_eq!(
        read(&Fence::Open(KeptLocal(HashSet::new()))),
        ["The last 2 tasks from meetings with Dana Reyes went to Pier Lighting"]
    );
    assert_eq!(
        read(&Fence::Open(KeptLocal(HashSet::from([kept])))),
        Vec::<&str>::new()
    );
}

/// A meeting kept local can rule a person out, and what is suggested in
/// their place then rests on it: an agent is not shown that either.
#[test]
fn a_kept_meeting_that_ruled_a_person_out_hides_what_is_suggested_instead() {
    let mut desk = Desk::new();
    let dana = person(&desk.meetings, "Dana Reyes", &[], &[]);
    let optics = desk.project("Optics");
    let power = desk.project("Power");
    for title in ["Replace breaker panel", "Label breaker panel"] {
        desk.task(
            title,
            WorkStatus::NotStarted,
            Some(&power),
            &WorkOwner::Nobody,
            None,
        );
    }
    let kept = meeting(&desk.meetings, "Supply review", AT);
    link(&desk.meetings, kept, dana, "calendar", "confirmed");
    for title in [
        "Order the fuses",
        "Meter the feed",
        "Book the outage",
        "Tag the cables",
    ] {
        desk.task(
            title,
            WorkStatus::NotStarted,
            Some(&power),
            &WorkOwner::Nobody,
            Some(kept),
        );
    }
    let bench = meeting(&desk.meetings, "Bench review", AT + 86_400_000);
    link(&desk.meetings, bench, dana, "calendar", "confirmed");
    for title in [
        "Align the mirror",
        "Clean the lens",
        "Mount the prism",
        "Shim the rail",
        "Log the focus",
    ] {
        desk.task(
            title,
            WorkStatus::NotStarted,
            Some(&optics),
            &WorkOwner::Nobody,
            Some(bench),
        );
    }
    let today = meeting(&desk.meetings, "Bench sync", AT + 2 * 86_400_000);
    link(&desk.meetings, today, dana, "calendar", "confirmed");
    let new = desk.task(
        "Inspect breaker panel",
        WorkStatus::Triage,
        None,
        &WorkOwner::Nobody,
        Some(today),
    );

    let suggestions = desk.suggestions(&new);
    let read = |fence: &Fence| -> Vec<&str> {
        suggestions
            .iter()
            .filter(|suggestion| suggestion.field == WorkSuggestionField::Project)
            .filter(|suggestion| !fence.hides_suggestion(suggestion))
            .map(|suggestion| suggestion.reason.as_str())
            .collect()
    };

    assert_eq!(
        read(&Fence::Open(KeptLocal(HashSet::new()))),
        ["The last 2 tasks with similar titles went to Power"]
    );
    assert_eq!(
        read(&Fence::Open(KeptLocal(HashSet::from([kept])))),
        Vec::<&str>::new()
    );
}

/// A month less tidy than the one above, from a fixed seed: meetings in no
/// order, one to three items each, two colleagues who drop in on most
/// meetings, leads who sometimes sit in on each other's, and half the titles
/// sharing a word with another project's. Every project starts with three
/// tasks. Counts what accepting with suggestions files where.
fn misfiled_after_a_messy_month() -> (usize, usize, usize) {
    const MEETINGS: usize = 240;
    let mut seed: u64 = 0x5eed_0f_a_f1c7;
    let mut roll = move |sides: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        // PANIC: a remainder below `sides` fits wherever `sides` came from.
        usize::try_from((seed >> 33) % sides).expect("a small roll")
    };
    let mut desk = Desk::new();
    let floaters = [
        (person(&desk.meetings, "Sam Ortiz", &[], &[]), 8),
        (person(&desk.meetings, "Noor Aziz", &[], &[]), 6),
    ];
    let streams: Vec<_> = [
        ("Pier Lighting", "Dana Reyes", "lamp"),
        ("Harbor Dredging", "Priya Nair", "silt"),
        ("Ferry Timetable", "Marco Bell", "ferry"),
        ("Market Stalls", "Lena Fischer", "stall"),
    ]
    .into_iter()
    .map(|(project, lead, word)| {
        (
            desk.project(project),
            person(&desk.meetings, lead, &[], &[]),
            word,
        )
    })
    .collect();
    for (stream, (project, lead, word)) in streams.iter().enumerate() {
        desk.now += 3_600_000;
        let kickoff = meeting(&desk.meetings, "Kickoff", desk.now);
        link(&desk.meetings, kickoff, *lead, "calendar", "confirmed");
        for task in 0..3 {
            desk.task(
                &format!("Plan the {word} work k{stream}{task}"),
                WorkStatus::NotStarted,
                Some(project),
                &WorkOwner::Nobody,
                Some(kickoff),
            );
        }
    }
    let (mut own, mut other, mut unfiled) = (0, 0, 0);
    let mut number = 0;
    for _ in 0..MEETINGS {
        let stream = roll(4);
        desk.now += 3_600_000;
        let held = meeting(&desk.meetings, "Weekly", desk.now);
        link(
            &desk.meetings,
            held,
            streams[stream].1,
            "calendar",
            "confirmed",
        );
        for (floater, of_ten) in floaters {
            if roll(10) < of_ten {
                link(&desk.meetings, held, floater, "calendar", "confirmed");
            }
        }
        if roll(100) < 15 {
            let guest = (stream + 1 + roll(3)) % 4;
            link(
                &desk.meetings,
                held,
                streams[guest].1,
                "calendar",
                "confirmed",
            );
        }
        for _ in 0..=roll(3) {
            number += 1;
            let word = if roll(2) == 0 {
                streams[stream].2
            } else {
                streams[roll(4)].2
            };
            let item = desk.task(
                &format!("Chase the {word} quote m{number}a m{number}b"),
                WorkStatus::Triage,
                None,
                &WorkOwner::Nobody,
                Some(held),
            );
            let mut failed = None;
            let accepted = desk.meetings.with_planning_snapshot(|meetings| {
                let mut reader = MeetingReader::new(Some(meetings), None);
                sync::acceptance(&desk.history, &mut reader, &item, true, None)
                    .and_then(|patch| {
                        sync::accept(&desk.history, None, &item, &patch, WorkActor::Me, desk.now)
                    })
                    .map_err(|error| {
                        failed = Some(error);
                        StoreError::Unavailable
                    })
            });
            // PANIC: accepting a fixture's own Triage item cannot fail.
            let (accepted, _) =
                accepted.unwrap_or_else(|error| panic!("the acceptance: {failed:?} {error:?}"));
            match accepted.project_id {
                Some(project) if project == streams[stream].0 => own += 1,
                Some(_) => other += 1,
                None => unfiled += 1,
            }
        }
    }
    (own, other, unfiled)
}

/// In the messy month fewer than one item in sixteen is filed under the
/// wrong project and nine times as many under the right one. Two people with
/// as much behind them are told apart by name, so the counts hold from run
/// to run.
#[test]
fn a_messy_month_files_few_items_under_the_wrong_project() {
    let (own, other, unfiled) = misfiled_after_a_messy_month();
    println!(
        "messy month: {own} under their own project, {other} under another, {unfiled} unfiled"
    );
    assert_eq!((own, other, unfiled), (277, 30, 171));
}

/// A regular guest who sat in on all of a specialist's meetings, and on
/// others' too, skips one: the specialist's meetings still say where its
/// work goes.
#[test]
fn a_regular_guest_who_skips_a_meeting_takes_no_suggestion_with_them() {
    let mut desk = Desk::new();
    let dana = person(&desk.meetings, "Dana Reyes", &[], &[]);
    let sam = person(&desk.meetings, "Sam Ortiz", &[], &[]);
    let tom = person(&desk.meetings, "Tom Hale", &[], &[]);
    let lighting = desk.project("Pier Lighting");
    let dredging = desk.project("Harbor Dredging");
    let walk = meeting(&desk.meetings, "Pier walk", AT);
    link(&desk.meetings, walk, dana, "calendar", "confirmed");
    link(&desk.meetings, walk, sam, "calendar", "confirmed");
    for title in ["Order the bulbs", "Fix the ramp"] {
        desk.task(
            title,
            WorkStatus::NotStarted,
            Some(&lighting),
            &WorkOwner::Nobody,
            Some(walk),
        );
    }
    let channel = meeting(&desk.meetings, "Channel walk", AT + 86_400_000);
    link(&desk.meetings, channel, tom, "calendar", "confirmed");
    link(&desk.meetings, channel, sam, "calendar", "confirmed");
    for title in ["Sound the channel", "Hire the barge"] {
        desk.task(
            title,
            WorkStatus::NotStarted,
            Some(&dredging),
            &WorkOwner::Nobody,
            Some(channel),
        );
    }
    let today = meeting(&desk.meetings, "Pier sync", AT + 2 * 86_400_000);
    link(&desk.meetings, today, dana, "calendar", "confirmed");
    let new = desk.task(
        "Call the electrician",
        WorkStatus::Triage,
        None,
        &WorkOwner::Nobody,
        Some(today),
    );

    assert_eq!(
        reasons(&desk.suggestions(&new), &[WorkSuggestionField::Project]),
        ["The last 2 tasks from meetings with Dana Reyes went to Pier Lighting"]
    );
}
