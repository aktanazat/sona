use super::{pick_person, Board, Fence, ShowArgs, TaskRef, WorkPersonChoice, WorkVerb};
use crate::cli::{CliArgs, Command};
use crate::meeting::processing::MeetingLivePromise;
use crate::meeting::types::MeetingSessionId;
use crate::planning::schema;
use crate::planning::work::capture::{attach, WorkCaptureSource};
use crate::planning::work::items::{self, NewItem};
use crate::planning::work::live;
use crate::planning::work::types::{
    WorkActor, WorkDirection, WorkItem, WorkItemRow, WorkOwner, WorkPatch, WorkSource, WorkStatus,
};
use crate::query::KeptLocal;
use clap::Parser;
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

const KEPT_MEETING: &str = "1e1a5f0e-0000-4000-8000-000000000001";
const SHARED_MEETING: &str = "1e1a5f0e-0000-4000-8000-000000000002";

fn shown(argv_task: &str) -> Result<TaskRef, String> {
    match CliArgs::try_parse_from(["sona", "work", "show", argv_task]) {
        Ok(CliArgs {
            command: Some(Command::Work(work)),
            ..
        }) => match work.verb {
            WorkVerb::Show(ShowArgs { task }) => Ok(task),
            other => panic!("`work show` parsed as {other:?}"),
        },
        Ok(other) => panic!("`work show` parsed as {other:?}"),
        Err(error) => Err(error.to_string()),
    }
}

/// Agents copy a task's name from the CLI (`S-12`), the app (`#12`) or JSON
/// (its id); every form names the same task, and a name that is no task is
/// refused rather than read as some other one.
#[test]
fn a_task_is_named_by_handle_number_or_id() {
    let id = "5b0c6a8e-3d1f-4c2a-9e7b-0a1b2c3d4e5f";
    let accepted = [
        ("S-12", TaskRef::Number(12)),
        ("s-12", TaskRef::Number(12)),
        ("#12", TaskRef::Number(12)),
        ("12", TaskRef::Number(12)),
        (id, TaskRef::Id(id.to_owned())),
        (
            "5B0C6A8E-3D1F-4C2A-9E7B-0A1B2C3D4E5F",
            TaskRef::Id(id.to_owned()),
        ),
    ];
    let wrong: Vec<(&str, Result<TaskRef, String>, TaskRef)> = accepted
        .into_iter()
        .map(|(name, expected)| (name, shown(name), expected))
        .filter(|(_, got, expected)| got.as_ref() != Ok(expected))
        .collect();
    assert_eq!(wrong, Vec::new(), "these names did not name their task");

    let not_refused: Vec<(&str, Result<TaskRef, String>)> = ["0", "S-0", "S-twelve", "twelve", ""]
        .into_iter()
        .map(|name| (name, shown(name)))
        .filter(|(_, got)| {
            !got.as_ref()
                .is_err_and(|error| error.contains("expected a task: S-12, 12, or a task id"))
        })
        .collect();
    assert_eq!(
        not_refused,
        Vec::new(),
        "these name no task but were not refused as such"
    );
}

/// A saved view shows its due windows as --due takes them, so an agent can
/// copy one into a list; a window of no days is refused, not read as none.
#[test]
fn a_due_window_reads_back_as_the_flag_it_came_from() {
    let read: Vec<(&str, Result<String, String>)> = [
        ("overdue", "overdue"),
        (" Today", "today"),
        ("THIS-WEEK", "this-week"),
        ("next:14", "next:14"),
        ("none", "none"),
    ]
    .into_iter()
    .map(|(typed, flag)| (flag, super::due_window(typed).map(super::due_flag)))
    .filter(|(flag, read)| read.as_deref() != Ok(*flag))
    .collect();
    assert_eq!(read, Vec::new(), "these windows did not read back");

    let not_refused: Vec<&str> = ["next:0", "next:", "next:-3", "next:two", "soon", ""]
        .into_iter()
        .filter(|typed| super::due_window(typed).is_ok())
        .collect();
    assert_eq!(not_refused, Vec::<&str>::new(), "these name no window");
}

fn task() -> WorkItem {
    WorkItem {
        id: Uuid::new_v4().to_string(),
        number: 1,
        title: "Send the launch deck".to_owned(),
        details: None,
        status: WorkStatus::NotStarted,
        priority: 0,
        project_id: None,
        milestone_id: None,
        owner_person_id: None,
        owner_is_self: true,
        direction: WorkDirection::Mine,
        due_at_ms: None,
        label_ids: Vec::new(),
        source_loop_id: None,
        duplicate_of_id: None,
        snoozed_until_ms: None,
        archived_at_ms: None,
        status_changed_at_ms: 0,
        created_at_ms: 0,
        updated_at_ms: 0,
        revision: 1,
        sort_order: 0.0,
        cycle: None,
    }
}

fn session(meeting: &str) -> MeetingSessionId {
    MeetingSessionId::from_uuid(Uuid::parse_str(meeting).expect("a uuid"))
}

fn loop_in(meeting: &str) -> String {
    format!("{meeting}:loop:0123456789abcdef")
}

/// A Triage task the way the tracker files one, tracking `source_loop_id`.
fn filed(history: &Connection, title: &str, source_loop_id: Option<&str>) -> String {
    items::insert(
        history,
        &NewItem {
            title,
            details: None,
            status: WorkStatus::Triage,
            priority: 0,
            project_id: None,
            milestone_id: None,
            owner: &WorkOwner::Nobody,
            due_at_ms: None,
            label_ids: &[],
            source_loop_id,
            recurrence: None,
        },
        WorkActor::Me,
        0,
    )
    .expect("a filed task")
    .id
}

/// What the live card files at hang-up: a task tracking no commitment, with
/// the moment and words it was heard in.
fn heard_live(history: &Connection, meeting: &str, what: &str) -> String {
    let promise = MeetingLivePromise {
        id: Uuid::new_v4().to_string(),
        what: what.to_owned(),
        owner: WorkOwner::Nobody,
        owner_name: None,
        owner_set: false,
        due_at_ms: None,
        due_words: None,
        quote: format!("I will {what}"),
        start_offset_ns: 0,
        end_offset_ns: 1_000_000_000,
    };
    // PANIC: a fixture that cannot open its database is a broken test.
    let meetings = Connection::open_in_memory().expect("a meeting database");
    live::file_promises(history, &meetings, session(meeting), &[promise], 0)
        .expect("the heard promise filed")
        .remove(0)
        .id
}

/// Work raised in a meeting kept on this Mac never reaches an agent, however
/// it was filed and whether or not the meeting still keeps its words. While
/// the conversations are closed nothing says which meetings those are, so
/// every task from a meeting stays out; typed tasks show either way.
#[test]
fn the_fence_leaves_out_work_any_kept_meeting_raised_and_all_of_it_while_closed() {
    let history = Connection::open_in_memory().expect("a history database");
    schema::apply_all(&history).expect("the planner schema");
    let mut names = HashMap::new();
    let mut name = |label: &'static str, id: String| names.insert(id, label);
    name("typed", filed(&history, "Book the venue", None));
    name(
        "tracked, kept",
        filed(&history, "Send the deck", Some(&loop_in(KEPT_MEETING))),
    );
    name(
        "tracked, shared",
        filed(&history, "Send the outline", Some(&loop_in(SHARED_MEETING))),
    );
    name(
        "heard live, kept",
        heard_live(&history, KEPT_MEETING, "send the budget"),
    );
    name(
        "heard live, shared",
        heard_live(&history, SHARED_MEETING, "print the badges"),
    );
    let taken = filed(&history, "Call the caterer", None);
    attach(
        &history,
        &taken,
        &WorkCaptureSource::Loop {
            loop_id: loop_in(KEPT_MEETING),
        },
        0,
    )
    .expect("the loop line kept as evidence");
    name("taken from a loop line, kept", taken);

    let hidden = |fence: &Fence| {
        let mut hidden: Vec<&str> = fence
            .hidden_tasks(&history)
            .expect("the fence read")
            .iter()
            .map(|id| names[id])
            .collect();
        hidden.sort_unstable();
        hidden
    };
    let open = Fence::Open(KeptLocal(HashSet::from([session(KEPT_MEETING)])));

    assert_eq!(
        hidden(&open),
        [
            "heard live, kept",
            "taken from a loop line, kept",
            "tracked, kept"
        ]
    );
    assert_eq!(
        hidden(&Fence::Closed),
        [
            "heard live, kept",
            "heard live, shared",
            "taken from a loop line, kept",
            "tracked, kept",
            "tracked, shared"
        ]
    );
}

/// `work update --cycle` moves a task to another cycle; naming the cycle it
/// is already in leaves it alone.
#[test]
fn moving_a_task_to_another_cycle_is_a_change() {
    let row = WorkItemRow {
        item: WorkItem {
            cycle: Some(4),
            ..task()
        },
        project_title: None,
        owner_name: None,
        doer: None,
        source_meeting_id: None,
        source_meeting_title: None,
        source_quote: None,
        source_speaker: None,
        source_at_ms: None,
        blocked_by_open_count: 0,
        raised_in: 0,
        source: WorkSource::Dictation,
        follow_up_at_ms: None,
        at_risk: false,
        time_block: None,
        repeat: None,
    };
    let board = Board {
        rows: vec![row.clone()],
        labels: Vec::new(),
        projects: Vec::new(),
        people: Vec::new(),
        suggestions: BTreeMap::new(),
        focus: Vec::new(),
        repeats: HashMap::new(),
        fence: Fence::Closed,
        cycles: Vec::new(),
    };
    let changes = |cycle| {
        let patch = WorkPatch {
            cycle: Some(cycle),
            ..WorkPatch::default()
        };
        board
            .diff(&row, &patch)
            .into_iter()
            .map(|change| (change.field, change.from, change.to))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        changes(Some(5)),
        [("cycle", "4".to_owned(), "5".to_owned())]
    );
    assert_eq!(
        changes(None),
        [("cycle", "4".to_owned(), "none".to_owned())]
    );
    assert_eq!(changes(Some(4)), []);
}

fn choice(id: &str, name: &str, aliases: &[&str]) -> WorkPersonChoice {
    WorkPersonChoice {
        id: id.to_owned(),
        name: name.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
    }
}

/// A transcript keeps hearing "Danny" for Daniel. The alias saved on the
/// person finds them wherever an owner is named; a part of a name that two
/// people share still refuses to guess.
#[test]
fn a_person_is_found_by_an_alias_and_a_shared_part_of_a_name_is_refused() {
    let people = [
        choice("p1", "Daniel Okafor", &["Danny", "Dan O"]),
        choice("p2", "Dana Reyes", &[]),
    ];
    let found = |value: &str| pick_person(value, &people).map(|person| person.id.as_str());

    assert_eq!(found("danny").ok(), Some("p1"));
    assert_eq!(found("Dan O").ok(), Some("p1"));
    assert_eq!(found("Reyes").ok(), Some("p2"));
    assert_eq!(found("p2").ok(), Some("p2"));
    assert!(found("Dan").is_err(), "both Daniel and Dana contain it");
    assert!(found("Priya").is_err());
}
