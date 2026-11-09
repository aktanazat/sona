use super::{Fence, ShowArgs, TaskRef, WorkCommand, WorkVerb};
use crate::cli::{CliArgs, Command};
use crate::meeting::types::MeetingSessionId;
use crate::planning::work::types::{WorkDirection, WorkItem, WorkStatus};
use crate::query::KeptLocal;
use clap::Parser;
use std::collections::HashSet;
use uuid::Uuid;

const KEPT_MEETING: &str = "1e1a5f0e-0000-4000-8000-000000000001";
const SHARED_MEETING: &str = "1e1a5f0e-0000-4000-8000-000000000002";

fn shown(argv_task: &str) -> Result<TaskRef, String> {
    match CliArgs::try_parse_from(["sona", "work", "show", argv_task]) {
        Ok(CliArgs {
            command:
                Some(Command::Work(WorkCommand {
                    verb: WorkVerb::Show(ShowArgs { task }),
                    ..
                })),
            ..
        }) => Ok(task),
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

fn task(source_meeting: Option<&str>) -> WorkItem {
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
        source_loop_id: source_meeting.map(|meeting| format!("{meeting}:loop:0123456789abcdef")),
        duplicate_of_id: None,
        snoozed_until_ms: None,
        archived_at_ms: None,
        status_changed_at_ms: 0,
        created_at_ms: 0,
        updated_at_ms: 0,
        revision: 1,
        sort_order: 0.0,
    }
}

/// Work raised in a meeting kept on this Mac never reaches an agent. While
/// the conversations are closed nothing says which meetings those are, so
/// every task from a meeting stays out; typed tasks show either way.
#[test]
fn the_fence_leaves_out_work_from_kept_meetings_and_all_of_it_while_closed() {
    let kept = MeetingSessionId::from_uuid(Uuid::parse_str(KEPT_MEETING).expect("a uuid"));
    let open = Fence::Open(KeptLocal(HashSet::from([kept])));
    let closed = Fence::Closed;
    let typed = task(None);
    let from_kept = task(Some(KEPT_MEETING));
    let from_shared = task(Some(SHARED_MEETING));

    let hidden = |fence: &Fence| {
        [
            ("typed", &typed),
            ("from kept", &from_kept),
            ("from shared", &from_shared),
        ]
        .into_iter()
        .filter(|(_, item)| fence.hides_item(item))
        .map(|(name, _)| name)
        .collect::<Vec<_>>()
    };

    assert_eq!(hidden(&open), ["from kept"]);
    assert_eq!(hidden(&closed), ["from kept", "from shared"]);
}
