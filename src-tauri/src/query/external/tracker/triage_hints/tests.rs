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
