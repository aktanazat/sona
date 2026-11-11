//! The tracker against a recorded meeting, through the functions its
//! commands and hooks run: intake, triage, the two-way commitment sync, bulk
//! changes, archive and duplicates, and the migration that made it.

use super::super::schema;
use super::super::store as planning_store;
use super::items::{self, NewItem};
use super::types::{
    WorkActivityKind, WorkActor, WorkCommentSubject, WorkGroupBy, WorkItem, WorkItemInput,
    WorkOwner, WorkPatch, WorkPlace, WorkQuery, WorkRelationKind, WorkStatus,
};
use super::{
    accept, bulk_update, close_parents, create, keep, list, mark_duplicate, set_archived, update,
    Change,
};
use super::{bin, inbox, live, sync};
use crate::meeting::ledger::{
    LedgerCommitment, LedgerFirmness, LedgerReceipt, LedgerReceiptState, MeetingLedger,
};
use crate::meeting::loop_types::{
    MeetingLoopId, MeetingLoopKind, MeetingLoopReopenRequest, MeetingLoopResolution,
    MeetingLoopResolveRequest, MeetingLoopRow, MeetingLoopStatus,
};
use crate::meeting::store::{loops_tests, workflow_core_tests, MeetingStore, StoreError};
use crate::meeting::types::{
    MeetingOperationId, MeetingSessionId, OperationActor, OperationResult,
};
use crate::planning::types::{EvidenceSourceKind, EvidenceTargetKind};
use chrono::{Duration, Local, NaiveDate, TimeZone};
use rusqlite::{Connection, TransactionBehavior};
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;
use std::collections::HashMap;

const AT: i64 = 1_700_000_000_000;

fn operation() -> String {
    Uuid::new_v4().to_string()
}

fn by_me(operation_id: &str, at_ms: i64) -> Change<'_> {
    Change {
        operation_id,
        actor: WorkActor::Me,
        at_ms,
    }
}

fn status(status: WorkStatus) -> WorkPatch {
    WorkPatch {
        status: Some(status),
        ..WorkPatch::default()
    }
}

fn statuses(items: &[WorkItem]) -> Vec<WorkStatus> {
    items.iter().map(|item| item.status).collect()
}

fn typed(title: &str) -> WorkItemInput {
    WorkItemInput {
        title: title.to_owned(),
        details: None,
        status: None,
        priority: None,
        project_id: None,
        milestone_id: None,
        owner: None,
        due_at_ms: None,
        label_ids: None,
        recurrence: None,
    }
}

/// A recorded meeting in which Dana promised each of `promises`, a second
/// apart, beside an empty tracker, and the clock both of them read.
struct Desk {
    _directory: TempDir,
    meetings: Arc<MeetingStore>,
    history: Connection,
    meeting: MeetingSessionId,
    now: i64,
}

fn desk(promises: &[&str]) -> Desk {
    let (directory, meetings) = workflow_core_tests::store();
    let meeting = workflow_core_tests::meeting(&meetings, "Launch sync", AT);
    hold(&meetings, meeting, promises);
    // PANIC: a fixture that cannot open its database is a broken test.
    let history = Connection::open_in_memory().expect("an in-memory history database");
    // PANIC: as above, for the planner schema.
    schema::apply_all(&history).expect("the planner schema");
    Desk {
        _directory: directory,
        meetings,
        history,
        meeting,
        now: AT,
    }
}

/// Dana says each of `promises` in `meeting`, a second apart, and the
/// meeting's notes record them.
fn hold(meetings: &MeetingStore, meeting: MeetingSessionId, promises: &[&str]) {
    let said = promises
        .iter()
        .map(|what| format!("I will {}", what.to_lowercase()))
        .collect::<Vec<_>>();
    workflow_core_tests::transcript_segments(
        meetings,
        meeting,
        &said.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    loops_tests::file_ledger(meetings, meeting, notes(promises));
}

/// Notes in which Dana promised each of `promises`, a second apart.
fn notes(promises: &[&str]) -> MeetingLedger {
    let commitments = (0_u64..)
        .zip(promises)
        .map(|(second, what)| LedgerCommitment {
            who: "Dana".to_owned(),
            what: (*what).to_owned(),
            firmness: LedgerFirmness::Firm,
            receipt: LedgerReceipt {
                quote: format!("I will {}", what.to_lowercase()),
                speaker: Some("Dana".to_owned()),
                t_ms: second * 1_000,
                citations: Vec::new(),
            },
        })
        .collect();
    MeetingLedger {
        headline: "Dana took the launch errands.".to_owned(),
        threads: Vec::new(),
        open_loops: Vec::new(),
        commitments,
        stances: Vec::new(),
        caveats: Vec::new(),
        receipts: LedgerReceiptState::Verified,
    }
}

impl Desk {
    /// A second after the last thing that happened at the desk.
    fn tick(&mut self) -> i64 {
        self.now += 1_000;
        self.now
    }

    /// Runs `work` the way a tracker command does: inside the meeting
    /// database's transaction, which a failure on either side rolls back.
    fn both<T>(
        &mut self,
        work: impl FnOnce(&mut Connection, &Connection) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let history = &mut self.history;
        let mut failed = None;
        let result = self.meetings.with_planning_snapshot(|meetings| {
            work(history, meetings).map_err(|error| {
                failed = Some(error);
                StoreError::Unavailable
            })
        });
        result.map_err(|error| {
            failed
                .take()
                .unwrap_or_else(|| anyhow::anyhow!("the meeting database: {error:?}"))
        })
    }

    /// What the processing hook does once the meeting's notes are written.
    fn take_in(&mut self) -> Vec<WorkItem> {
        let meeting = self.meeting;
        let at = self.tick();
        // PANIC: intake from the fixture's own meeting cannot fail.
        self.both(|history, meetings| {
            let tx = history.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let taken = sync::take_in_meeting(&tx, meetings, meeting, None, at)?;
            tx.commit()?;
            Ok(taken)
        })
        .expect("the meeting's commitments in triage")
    }

    /// What `MeetingSessionManager::follow_tracker` does after a loop moved
    /// on its meeting's page. Returns the items that moved.
    fn follow(&mut self, loop_id: &MeetingLoopId) -> Vec<WorkItem> {
        let at = self.tick();
        // PANIC: following the fixture's own commitment cannot fail.
        self.both(|history, meetings| {
            let tx = history.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let followed = sync::follow_loop_in(&tx, meetings, loop_id, at)?;
            tx.commit()?;
            Ok(followed.items)
        })
        .expect("the tracker following its commitment")
    }

    /// Closes the commitment on its meeting's page, or reopens it with
    /// `None`, then lets the tracker follow.
    fn on_meeting_page(
        &mut self,
        loop_id: &MeetingLoopId,
        resolution: Option<MeetingLoopResolution>,
    ) -> Vec<WorkItem> {
        let revision = self.page_row(loop_id).revision;
        let at = self.tick();
        let operation_id = MeetingOperationId::new();
        let loop_id_owned = loop_id.clone();
        // PANIC: the request carries the revision just read.
        let moved = match resolution {
            Some(resolution) => self.meetings.resolve_loop(
                MeetingLoopResolveRequest {
                    operation_id,
                    loop_id: loop_id_owned,
                    expected_revision: revision,
                    resolution,
                },
                OperationActor::User,
                at,
            ),
            None => self.meetings.reopen_loop(
                MeetingLoopReopenRequest {
                    operation_id,
                    loop_id: loop_id_owned,
                    expected_revision: revision,
                },
                at,
            ),
        }
        .expect("the commitment moved on its page");
        assert_eq!(moved.receipt.result, OperationResult::Committed);
        self.follow(loop_id)
    }

    /// The meeting's commitments as its page shows them.
    fn commitments(&self) -> Vec<MeetingLoopRow> {
        // PANIC: the fixture's meeting always reads.
        let read = self
            .meetings
            .meeting_loops(self.meeting)
            .expect("the meeting's loops");
        read.rows
            .into_iter()
            .filter(|row| row.kind == MeetingLoopKind::Commitment)
            .collect()
    }

    fn page_row(&self, loop_id: &MeetingLoopId) -> MeetingLoopRow {
        // PANIC: every loop a test asks after is on the fixture's page.
        self.commitments()
            .into_iter()
            .find(|row| row.loop_id == *loop_id)
            .expect("the commitment on its meeting's page")
    }

    fn page_status(&self, loop_id: &MeetingLoopId) -> MeetingLoopStatus {
        self.page_row(loop_id).status
    }

    fn item(&self, id: &str) -> WorkItem {
        // PANIC: every id a test asks after was made by the test.
        items::item(&self.history, id).expect("a saved item")
    }

    /// Takes the meeting in and accepts its first commitment.
    fn accepted(&mut self) -> (WorkItem, MeetingLoopId) {
        let triaged = self.take_in().remove(0);
        // PANIC: intake only makes items that track a commitment.
        let loop_id = MeetingLoopId(
            triaged
                .source_loop_id
                .clone()
                .expect("a tracked commitment"),
        );
        let at = self.tick();
        // PANIC: a fresh triage item accepts.
        let item = self
            .both(|history, meetings| {
                accept(
                    history,
                    Some(meetings),
                    None,
                    by_me(&operation(), at),
                    &[triaged.id],
                    false,
                )
            })
            .expect("the triage item accepted")
            .value
            .remove(0);
        (item, loop_id)
    }

    fn evidence(&self, id: &str) -> Vec<(EvidenceSourceKind, String)> {
        // PANIC: every id a test asks after was made by the test.
        planning_store::task_evidence(&self.history, id)
            .expect("the item's evidence")
            .into_iter()
            .map(|link| (link.source_kind, link.source_id))
            .collect()
    }
}

#[test]
fn the_tracker_migration_keeps_every_task_and_the_commitment_it_tracks() {
    let mut conn = Connection::open_in_memory().expect("history");
    conn.pragma_update(None, "foreign_keys", false)
        .expect("the migration runner turns foreign keys off");
    let all = crate::managers::history::MIGRATIONS.to_vec();
    let tracker = all
        .iter()
        .position(|step| *step == rusqlite_migration::M::up(schema::WORK_MIGRATION))
        .expect("the tracker migration");
    let migrations = rusqlite_migration::Migrations::new(all);
    migrations
        .to_version(&mut conn, tracker)
        .expect("every migration before the tracker");
    conn.execute_batch(
        "INSERT INTO planning_projects (id, title, description, status, revision, created_at_ms, updated_at_ms)
            VALUES ('p', 'Launch', NULL, 'active', 1, 1, 1);
         INSERT INTO planning_milestones (id, project_id, title, due_at_ms, status, revision, created_at_ms, updated_at_ms)
            VALUES ('m', 'p', 'Beta', 500, 'not_started', 1, 1, 1);
         INSERT INTO planning_tasks (id, project_id, milestone_id, title, details, due_at_ms,
            owner_person_id, owner_is_self, status, revision, created_at_ms, updated_at_ms) VALUES
            ('live', 'p', 'm', 'Send the outline', 'Two pages', 900, NULL, 1, 'in_progress', 3, 20, 25),
            ('done', 'p', NULL, 'Book the venue', NULL, NULL, 'dana', 0, 'done', 2, 10, 30),
            ('old', 'p', NULL, 'Send the outline', NULL, NULL, NULL, 0, 'canceled', 1, 5, 6);
         INSERT INTO planning_evidence_links (id, target_kind, target_id, source_kind, source_id,
            at_ms, revision, created_at_ms) VALUES
            ('e1', 'task', 'live', 'loop', 'outline', NULL, 1, 21),
            ('e2', 'task', 'old', 'loop', 'outline', NULL, 1, 5),
            ('e3', 'task', 'done', 'loop', 'venue', NULL, 1, 11),
            ('e4', 'task', 'done', 'meeting', 'launch-sync', 0, 1, 11);",
    )
    .expect("tasks the planner saved");

    migrations
        .to_latest(&mut conn)
        .expect("the tracker migration and every one after it");

    let migrated = items::items(&conn, true).expect("the migrated items");
    let summary = migrated
        .iter()
        .map(|item| {
            (
                item.number,
                item.id.as_str(),
                item.status,
                item.source_loop_id.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            (1, "old", WorkStatus::Canceled, None),
            (2, "done", WorkStatus::Done, Some("venue")),
            (3, "live", WorkStatus::InProgress, Some("outline")),
        ]
    );
    let live = &migrated[2];
    assert_eq!(
        (
            live.title.as_str(),
            live.details.as_deref(),
            live.project_id.as_deref(),
            live.milestone_id.as_deref(),
            live.due_at_ms,
            live.owner_is_self,
            live.revision,
            live.created_at_ms,
            live.updated_at_ms,
        ),
        (
            "Send the outline",
            Some("Two pages"),
            Some("p"),
            Some("m"),
            Some(900),
            true,
            3,
            20,
            25
        )
    );
    assert_eq!(migrated[1].owner_person_id.as_deref(), Some("dana"));
    assert_eq!(
        planning_store::task_evidence(&conn, "done")
            .expect("evidence")
            .len(),
        2
    );

    let loose = items::insert(
        &conn,
        &NewItem {
            title: "Order the banners",
            details: None,
            status: WorkStatus::Triage,
            priority: 0,
            project_id: None,
            milestone_id: None,
            owner: &WorkOwner::Nobody,
            due_at_ms: None,
            label_ids: &[],
            source_loop_id: None,
            recurrence: None,
        },
        super::types::WorkActor::Me,
        AT,
    )
    .expect("an item outside any project");
    assert_eq!((loose.number, loose.project_id), (4, None));
}

#[test]
fn the_repeat_migration_keeps_every_history_entry_and_waiting_change() {
    let conn = Connection::open_in_memory().expect("history");
    for migration in [
        schema::MIGRATION,
        schema::QUIET_REVIEW_MIGRATION,
        schema::MEETING_NOTE_LINK_MIGRATION,
        schema::DISMISSED_LOOP_MIGRATION,
        schema::WORK_MIGRATION,
        schema::PROJECT_DELETE_KEEPS_TASKS_MIGRATION,
        schema::DAY_PLAN_MIGRATION,
        schema::FOLLOW_MIGRATION,
        schema::AGENTS_MIGRATION,
        schema::CAPTURE_MIGRATION,
    ] {
        conn.execute_batch(migration)
            .expect("an earlier planner migration");
    }
    conn.execute_batch(&format!(
        "INSERT INTO planning_tasks (id, number, title, owner_is_self, status, priority,
            status_changed_at_ms, sort_order, revision, created_at_ms, updated_at_ms)
            VALUES ('plants', 1, 'Water the plants', 0, 'not_started', 0, {at}, {at}, 1, {at}, {at});
         INSERT INTO planning_task_activity (id, task_id, at_ms, actor, kind, from_value, to_value) VALUES
            ('kept', '{id}', {kept}, 'me', 'kept', NULL, NULL),
            ('snoozed', '{id}', {snoozed}, 'agent', 'snoozed', NULL, '{snoozed}');
         INSERT INTO planning_activity_agents (activity_id, agent) VALUES ('snoozed', 'Claude Code');
         INSERT INTO planning_task_proposals (id, task_id, agent, field, from_value, patch_json,
            state, created_at_ms, decided_at_ms) VALUES
            ('waiting', '{id}', 'Claude Code', 'priority', 'none', '{{\"priority\":1}}', 'open', {kept}, NULL);",
        id = "plants",
        at = AT,
        kept = AT + 1,
        snoozed = AT + 2,
    ))
    .expect("history and a waiting change");
    let history = |conn: &Connection| {
        conn.prepare(
            "SELECT activity.id, activity.kind, activity.actor, agents.agent
             FROM planning_task_activity activity
             LEFT JOIN planning_activity_agents agents ON agents.activity_id = activity.id
             WHERE activity.task_id = 'plants'
             ORDER BY activity.at_ms, activity.id",
        )
        .expect("the task's history")
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .expect("the task's history")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("the task's history")
    };
    let waiting = |conn: &Connection| {
        super::agents::item_proposals(conn, "plants")
            .expect("waiting changes")
            .into_iter()
            .map(|proposal| (proposal.id, proposal.agent))
            .collect::<Vec<_>>()
    };
    let before = (history(&conn), waiting(&conn));

    conn.execute_batch(schema::REPEAT_MIGRATION)
        .expect("the repeat migration");

    assert_eq!((history(&conn), waiting(&conn)), before);
    conn.execute_batch(&format!(
        "INSERT INTO planning_task_activity (id, task_id, at_ms, actor, kind, from_value, to_value)
            VALUES ('repeat', '{id}', {at}, 'me', 'recurrence', NULL, 'every monday');
         INSERT INTO planning_task_proposals (id, task_id, agent, field, from_value, patch_json,
            state, created_at_ms, decided_at_ms) VALUES
            ('stop', '{id}', 'Claude Code', 'recurrence', 'every monday', '{{\"recurrence\":null}}', 'open', {at}, NULL);
         DELETE FROM planning_tasks WHERE id = '{id}';",
        id = "plants",
        at = AT + 3,
    ))
    .expect("a repeat in the history and a waiting change to it");
    assert_eq!(waiting(&conn), Vec::new());
}

#[test]
fn a_meeting_reaches_triage_once_per_open_commitment() {
    let mut desk = desk(&["Send the outline", "Book the venue", "Print the badges"]);
    let badges = desk
        .commitments()
        .into_iter()
        .find(|row| row.text == "Print the badges")
        .expect("the badges commitment")
        .loop_id;
    desk.on_meeting_page(&badges, Some(MeetingLoopResolution::Done));

    let first = desk.take_in();
    let titles = first
        .iter()
        .map(|item| item.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(titles, ["Send the outline", "Book the venue"]);
    assert_eq!(statuses(&first), [WorkStatus::Triage, WorkStatus::Triage]);
    for item in &first {
        assert_eq!(
            desk.evidence(&item.id),
            [(EvidenceSourceKind::Meeting, desk.meeting.uuid().to_string())]
        );
    }

    assert!(desk.take_in().is_empty());
    assert_eq!(
        items::items(&desk.history, true)
            .expect("the tracker")
            .len(),
        2
    );
}

#[test]
fn a_commitment_and_its_item_move_together_without_echo() {
    let mut desk = desk(&["Send the outline"]);
    let (item, loop_id) = desk.accepted();
    assert_eq!(item.status, WorkStatus::NotStarted);
    assert_eq!(desk.page_status(&loop_id), MeetingLoopStatus::Open);

    let at = desk.tick();
    let done = desk
        .both(|history, meetings| {
            update(
                history,
                Some(meetings),
                by_me(&operation(), at),
                &item.id,
                item.revision,
                &status(WorkStatus::Done),
            )
        })
        .expect("done in the tracker")
        .value;
    assert_eq!(desk.page_status(&loop_id), MeetingLoopStatus::Done);
    assert!(desk.follow(&loop_id).is_empty());
    assert_eq!(desk.item(&item.id), done);

    let reopened = desk.on_meeting_page(&loop_id, None);
    assert_eq!(statuses(&reopened), [WorkStatus::NotStarted]);
    assert_eq!(desk.page_status(&loop_id), MeetingLoopStatus::Open);
    assert!(desk.follow(&loop_id).is_empty());

    let dropped = desk.on_meeting_page(&loop_id, Some(MeetingLoopResolution::Dropped));
    assert_eq!(statuses(&dropped), [WorkStatus::Canceled]);
    assert_eq!(desk.page_status(&loop_id), MeetingLoopStatus::Dropped);
    assert!(desk.follow(&loop_id).is_empty());
}

#[test]
fn a_bulk_change_lands_on_every_item_or_on_none() {
    let mut desk = desk(&["Send the outline"]);
    let (tracked, loop_id) = desk.accepted();
    let at = desk.tick();
    let banners = create(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &typed("Order the banners"),
    )
    .expect("a typed item")
    .value;
    let finish = WorkPatch {
        status: Some(WorkStatus::Done),
        priority: Some(2),
        ..WorkPatch::default()
    };

    let missing = Uuid::new_v4().to_string();
    let at = desk.tick();
    let refused = desk.both(|history, meetings| {
        bulk_update(
            history,
            Some(meetings),
            by_me(&operation(), at),
            &[tracked.id.clone(), banners.id.clone(), missing],
            &finish,
        )
    });
    assert!(refused.is_err());
    assert_eq!(desk.item(&tracked.id), tracked);
    assert_eq!(desk.item(&banners.id), banners);
    assert_eq!(desk.page_status(&loop_id), MeetingLoopStatus::Open);

    let at = desk.tick();
    let landed = desk
        .both(|history, meetings| {
            bulk_update(
                history,
                Some(meetings),
                by_me(&operation(), at),
                &[tracked.id.clone(), banners.id.clone()],
                &finish,
            )
        })
        .expect("the bulk change")
        .value;
    assert_eq!(statuses(&landed), [WorkStatus::Done, WorkStatus::Done]);
    assert!(landed.iter().all(|item| item.priority == 2));
    assert_eq!(desk.page_status(&loop_id), MeetingLoopStatus::Done);
}

#[test]
fn archiving_hides_an_item_and_restoring_brings_it_back_as_it_was() {
    let mut desk = desk(&["Send the outline"]);
    let (item, loop_id) = desk.accepted();

    let at = desk.tick();
    let archived = set_archived(
        &mut desk.history,
        by_me(&operation(), at),
        std::slice::from_ref(&item.id),
        true,
    )
    .expect("archived")
    .value
    .remove(0);
    assert!(archived.archived_at_ms.is_some());
    assert!(items::items(&desk.history, false)
        .expect("the open list")
        .is_empty());
    assert_eq!(desk.page_status(&loop_id), MeetingLoopStatus::Open);
    assert!(desk.take_in().is_empty());

    let at = desk.tick();
    let restored = set_archived(
        &mut desk.history,
        by_me(&operation(), at),
        std::slice::from_ref(&item.id),
        false,
    )
    .expect("restored")
    .value
    .remove(0);
    assert_eq!(
        items::items(&desk.history, false).expect("the open list"),
        [restored.clone()]
    );
    assert_eq!(
        WorkItem {
            revision: item.revision,
            updated_at_ms: item.updated_at_ms,
            ..restored
        },
        item
    );
    let kinds = items::activity(&desk.history, &item.id)
        .expect("the item's history")
        .into_iter()
        .map(|entry| entry.kind)
        .collect::<Vec<_>>();
    assert!(kinds.ends_with(&[WorkActivityKind::Archived, WorkActivityKind::Restored]));
}

#[test]
fn a_duplicate_hands_its_meeting_to_the_original_and_follows_it() {
    let mut desk = desk(&["Send the outline"]);
    let at = desk.tick();
    let original = create(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &typed("Send the launch outline"),
    )
    .expect("a typed item")
    .value;
    let repeat = desk.take_in().remove(0);
    let loop_id = MeetingLoopId(repeat.source_loop_id.clone().expect("a tracked commitment"));
    let heard = desk.evidence(&repeat.id);
    assert_eq!(heard.len(), 1);

    let at = desk.tick();
    let marked = desk
        .both(|history, meetings| {
            mark_duplicate(
                history,
                Some(meetings),
                by_me(&operation(), at),
                &repeat.id,
                &original.id,
            )
        })
        .expect("marked a duplicate")
        .value;
    assert_eq!(marked.status, WorkStatus::Duplicate);
    assert_eq!(
        marked.duplicate_of_id.as_deref(),
        Some(original.id.as_str())
    );
    assert_eq!(desk.evidence(&original.id), heard);
    assert!(desk.evidence(&repeat.id).is_empty());
    assert_eq!(desk.page_status(&loop_id), MeetingLoopStatus::Open);

    let original = desk.item(&original.id);
    let at = desk.tick();
    desk.both(|history, meetings| {
        update(
            history,
            Some(meetings),
            by_me(&operation(), at),
            &original.id,
            original.revision,
            &status(WorkStatus::Done),
        )
    })
    .expect("the original done");
    assert_eq!(desk.page_status(&loop_id), MeetingLoopStatus::Done);
}

#[test]
fn an_agent_change_is_filed_under_the_agent_on_both_sides() {
    let mut desk = desk(&["Send the outline"]);
    let (item, loop_id) = desk.accepted();

    let at = desk.tick();
    desk.both(|history, meetings| {
        update(
            history,
            Some(meetings),
            Change {
                operation_id: &operation(),
                actor: WorkActor::Agent,
                at_ms: at,
            },
            &item.id,
            item.revision,
            &status(WorkStatus::Done),
        )
    })
    .expect("done by an agent");

    let filed = items::activity(&desk.history, &item.id)
        .expect("the item's history")
        .into_iter()
        .filter(|entry| entry.at_ms == at)
        .map(|entry| entry.actor)
        .collect::<Vec<_>>();
    assert_eq!(filed, [WorkActor::Agent]);
    let row = desk.page_row(&loop_id);
    assert_eq!(row.status, MeetingLoopStatus::Done);
    assert_eq!(row.resolved_by, Some(OperationActor::External));
}

#[test]
fn work_an_agent_keeps_is_filed_under_the_agent() {
    let mut desk = desk(&[]);
    let at = desk.tick();
    let item = create(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &typed("Renew the launch domain"),
    )
    .expect("an item typed in")
    .value;

    let kept_at = desk.tick();
    keep(
        &mut desk.history,
        Change {
            operation_id: &operation(),
            actor: WorkActor::Agent,
            at_ms: kept_at,
        },
        &[item.id.clone()],
    )
    .expect("kept by an agent");

    let filed = items::activity(&desk.history, &item.id)
        .expect("the item's history")
        .into_iter()
        .filter(|entry| entry.at_ms == kept_at)
        .map(|entry| (entry.kind, entry.actor))
        .collect::<Vec<_>>();
    assert_eq!(filed, [(WorkActivityKind::Kept, WorkActor::Agent)]);
}

#[test]
fn the_list_search_reads_a_task_s_comments() {
    let mut desk = desk(&[]);
    let at = desk.tick();
    let made = ["Repaint the railings", "Order the paint"].map(|title| {
        create(&mut desk.history, None, by_me(&operation(), at), &typed(title))
            .expect("an item typed in")
            .value
    });
    super::comments::add(
        &desk.history,
        WorkCommentSubject::Task,
        &made[0].id,
        None,
        super::comments::Author {
            actor: WorkActor::Me,
            agent: None,
        },
        "Hold the marine deposit until the paint arrives",
        at,
    )
    .expect("a comment");

    let query = WorkQuery {
        search: Some("marine deposit".to_owned()),
        ..WorkQuery::default()
    };
    let found = list(&desk.history, None, None, &query, at)
        .expect("the searched list")
        .items
        .into_iter()
        .map(|row| row.item.id)
        .collect::<Vec<_>>();
    assert_eq!(found, [made[0].id.clone()]);
}

#[test]
fn a_change_made_while_the_conversations_are_closed_reaches_its_commitment_when_they_open() {
    let mut desk = desk(&["Send the outline"]);
    let (item, loop_id) = desk.accepted();

    let at = desk.tick();
    let closed = list(&desk.history, None, None, &WorkQuery::default(), at)
        .expect("the tracker while the conversations are closed");
    assert!(!closed.conversations_available);
    let listed = closed
        .items
        .iter()
        .map(|row| (row.item.id.as_str(), row.item.source_loop_id.as_deref()))
        .collect::<Vec<_>>();
    assert_eq!(listed, [(item.id.as_str(), Some(loop_id.as_str()))]);

    let done = update(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &item.id,
        item.revision,
        &status(WorkStatus::Done),
    )
    .expect("done while the conversations are closed")
    .value;
    assert_eq!(done.status, WorkStatus::Done);
    assert_eq!(desk.page_status(&loop_id), MeetingLoopStatus::Open);

    let at = desk.tick();
    let caught_up = desk
        .both(|history, meetings| {
            let tx = history.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let followed = sync::reconcile(&tx, meetings, at)?;
            tx.commit()?;
            Ok(followed)
        })
        .expect("the catch-up as the conversations open");
    assert_eq!(caught_up.loops, [loop_id.clone()]);
    assert!(caught_up.items.is_empty());
    let row = desk.page_row(&loop_id);
    assert_eq!(row.status, MeetingLoopStatus::Done);
    assert_eq!(row.resolved_by, Some(OperationActor::User));
    assert_eq!(desk.item(&item.id), done);
}

#[test]
fn deleting_a_project_keeps_its_items() {
    let mut desk = desk(&["Send the outline"]);
    let (tracked, _) = desk.accepted();
    let at = desk.tick();
    // PANIC: a project and milestone on the fixture's own database.
    let project =
        planning_store::insert_project(&desk.history, "Launch", None, at).expect("a project");
    let milestone = planning_store::insert_milestone(&desk.history, &project.id, "Beta", None, at)
        .expect("a milestone");
    let planned = update(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &tracked.id,
        tracked.revision,
        &WorkPatch {
            project_id: Some(Some(project.id.clone())),
            milestone_id: Some(Some(milestone.id.clone())),
            ..WorkPatch::default()
        },
    )
    .expect("the commitment planned into the project")
    .value;
    let booked = create(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &WorkItemInput {
            project_id: Some(project.id.clone()),
            ..typed("Book the venue")
        },
    )
    .expect("an item typed into the project")
    .value;

    desk.history
        .execute("DELETE FROM planning_projects WHERE id = ?1", [&project.id])
        .expect("the project deleted");

    let kept = [&planned, &booked].map(|item| {
        items::item(&desk.history, &item.id).ok().map(|kept| {
            (
                kept.project_id,
                kept.milestone_id,
                kept.revision - item.revision,
            )
        })
    });
    assert_eq!(kept, [Some((None, None, 1)), Some((None, None, 1))]);
    assert!(planning_store::milestone(&desk.history, &milestone.id).is_err());
}

#[test]
fn a_due_window_counts_days_from_the_day_the_list_is_read() {
    let mut desk = desk(&["Send the outline"]);
    // PANIC: noon on a November Wednesday exists on every clock.
    let wednesday = NaiveDate::from_ymd_opt(2026, 11, 4).expect("a calendar day");
    let noon = |days: i64| {
        let noon = (wednesday + Duration::days(days))
            .and_hms_opt(12, 0, 0)
            .expect("noon");
        Local
            .from_local_datetime(&noon)
            .single()
            .expect("one noon on this clock")
            .timestamp_millis()
    };
    let now = noon(0);
    let made = [
        ("Tuesday, open", Some(-1)),
        ("Tuesday, done", Some(-1)),
        ("Wednesday", Some(0)),
        ("Sunday", Some(4)),
        ("next Monday", Some(5)),
        ("in 14 days", Some(14)),
        ("in 15 days", Some(15)),
        ("no date", None),
    ]
    .map(|(title, days)| {
        create(
            &mut desk.history,
            None,
            by_me(&operation(), now),
            &WorkItemInput {
                due_at_ms: days.map(noon),
                ..typed(title)
            },
        )
        .expect("a dated item")
        .value
    });
    update(
        &mut desk.history,
        None,
        by_me(&operation(), now),
        &made[1].id,
        made[1].revision,
        &status(WorkStatus::Done),
    )
    .expect("Tuesday's item done");

    let listed = |due: &str| {
        // PANIC: each window is written the way the Mac app sends it.
        let query = serde_json::from_str::<WorkQuery>(&format!(r#"{{"due":{due}}}"#))
            .expect("a due filter");
        list(&desk.history, None, None, &query, now)
            .expect("the filtered list")
            .items
            .into_iter()
            .map(|row| row.item.title)
            .collect::<Vec<_>>()
    };
    assert_eq!(listed(r#"[{"kind":"overdue"}]"#), ["Tuesday, open"]);
    assert_eq!(listed(r#"[{"kind":"today"}]"#), ["Wednesday"]);
    assert_eq!(listed(r#"[{"kind":"this_week"}]"#), ["Wednesday", "Sunday"]);
    assert_eq!(
        listed(r#"[{"kind":"next_days","days":14}]"#),
        ["Wednesday", "Sunday", "next Monday", "in 14 days"]
    );
    assert_eq!(listed(r#"[{"kind":"no_date"}]"#), ["no date"]);
    assert_eq!(
        listed(r#"[{"kind":"overdue"},{"kind":"today"}]"#),
        ["Tuesday, open", "Wednesday"]
    );
}

#[test]
fn work_waiting_on_a_merged_person_lists_under_either_of_their_ids() {
    let mut desk = desk(&["Send the outline"]);
    let old = workflow_core_tests::person(&desk.meetings, "D. Ruiz", &[], &[]);
    let dana = workflow_core_tests::person(&desk.meetings, "Dana Ruiz", &[], &[]);
    let at = desk.tick();
    let waiting = desk
        .both(|history, meetings| {
            create(
                history,
                Some(meetings),
                by_me(&operation(), at),
                &WorkItemInput {
                    owner: Some(WorkOwner::Person {
                        person_id: old.uuid().to_string(),
                    }),
                    ..typed("Review the venue contract")
                },
            )
        })
        .expect("an item waiting on a person")
        .value;
    let revision = desk
        .both(|_, meetings| {
            Ok(meetings.query_row(
                "SELECT revision FROM people_state WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .expect("the people revision");
    let at = desk.tick();
    desk.meetings
        .merge_persons_with_voice_resolution(
            old,
            dana,
            u64::try_from(revision).expect("a stored revision"),
            None,
            at,
        )
        .expect("the two people merged");

    let listed = [dana, old].map(|person| {
        let query = WorkQuery {
            owner_person_ids: Some(vec![person.uuid().to_string()]),
            ..WorkQuery::default()
        };
        desk.both(|history, meetings| list(history, Some(meetings), None, &query, at))
            .expect("what waits on the person")
            .items
            .into_iter()
            .map(|row| row.item.id)
            .collect::<Vec<_>>()
    });
    assert_eq!(listed, [[waiting.id.clone()], [waiting.id]]);
}

#[test]
fn a_promise_made_again_in_a_later_meeting_joins_the_item_from_the_first() {
    let mut desk = desk(&["Send the outline"]);
    let first = desk.take_in().remove(0);
    let launch_sync = desk.meeting;
    let launch_review =
        workflow_core_tests::meeting(&desk.meetings, "Launch review", AT + 86_400_000);
    hold(&desk.meetings, launch_review, &["Send the outline"]);
    desk.meeting = launch_review;

    let taken = desk.take_in();
    assert_eq!(
        taken
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        [first.id.as_str()]
    );
    let tracker = items::items(&desk.history, true).expect("the tracker");
    let open = tracker
        .iter()
        .filter(|item| item.status != WorkStatus::Duplicate)
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(open, [first.id.as_str()]);
    let mut heard = desk
        .evidence(&first.id)
        .into_iter()
        .map(|(_, meeting)| meeting)
        .collect::<Vec<_>>();
    heard.sort();
    let mut both = vec![
        launch_sync.uuid().to_string(),
        launch_review.uuid().to_string(),
    ];
    both.sort();
    assert_eq!(heard, both);
    let at = desk.tick();
    let listed =
        list(&desk.history, None, None, &WorkQuery::default(), at).expect("the tracker list");
    let row = listed
        .items
        .iter()
        .find(|row| row.item.id == first.id)
        .expect("the first item listed");
    assert_eq!(row.raised_in, 2);
}

#[test]
fn notes_written_again_in_other_words_move_the_item_instead_of_adding_one() {
    let mut desk = desk(&["Send the outline"]);
    let item = desk.take_in().remove(0);
    let meeting = desk.meeting.uuid().to_string();
    desk.both(|_, meetings| {
        meetings.execute(
            "UPDATE meeting_artifact_revisions SET state = 'out_of_date' WHERE session_id = ?1",
            [&meeting],
        )?;
        Ok(())
    })
    .expect("an edit putting the notes out of date");
    loops_tests::file_ledger(
        &desk.meetings,
        desk.meeting,
        notes(&["Send the launch outline"]),
    );
    let reworded = desk.commitments().remove(0).loop_id;

    let taken = desk.take_in();
    assert_eq!(
        taken
            .iter()
            .map(|item| (item.id.as_str(), item.source_loop_id.as_deref()))
            .collect::<Vec<_>>(),
        [(item.id.as_str(), Some(reworded.as_str()))]
    );
    assert_eq!(
        items::items(&desk.history, true)
            .expect("the tracker")
            .len(),
        1
    );
}

#[test]
fn a_promise_heard_live_becomes_the_item_its_commitment_tracks() {
    let mut desk = desk(&["Send the pricing deck"]);
    let meeting = desk.meeting;
    let heard = crate::meeting::processing::MeetingLivePromise {
        id: operation(),
        what: "Send Dana the pricing deck".to_owned(),
        owner: WorkOwner::Nobody,
        owner_name: None,
        owner_set: false,
        due_at_ms: None,
        due_words: None,
        quote: "I will send the pricing deck".to_owned(),
        start_offset_ns: 0,
        end_offset_ns: 1_000_000_000,
    };
    let at = desk.tick();
    // PANIC: filing into the fixture's own meeting cannot fail.
    let filed = desk
        .both(|history, meetings| {
            super::live::file_promises(history, meetings, meeting, &[heard], at)
        })
        .expect("the heard promise filed")
        .remove(0);
    let tracked = desk.commitments().remove(0).loop_id;

    let taken = desk.take_in();
    assert_eq!(
        taken
            .iter()
            .map(|item| (item.id.as_str(), item.source_loop_id.as_deref()))
            .collect::<Vec<_>>(),
        [(filed.id.as_str(), Some(tracked.as_str()))]
    );
    assert_eq!(
        items::items(&desk.history, true)
            .expect("the tracker")
            .len(),
        1
    );
}

fn on_the_clock(day: u32, hour: u32) -> i64 {
    Local
        .with_ymd_and_hms(2026, 11, day, hour, 0, 0)
        .single()
        .expect("a November 2026 hour on this Mac's clock")
        .timestamp_millis()
}

#[test]
fn a_weekly_task_whose_day_passed_comes_back_once_a_week_later() {
    // PANIC: a fixture that cannot open its database is a broken test.
    let conn = Connection::open_in_memory().expect("an in-memory history database");
    // PANIC: as above, for the planner schema.
    schema::apply_all(&conn).expect("the planner schema");
    // PANIC: a fixture that cannot add its task is a broken test.
    let task = items::insert(
        &conn,
        &NewItem {
            title: "Send the weekly update",
            details: None,
            status: WorkStatus::NotStarted,
            priority: 0,
            project_id: None,
            milestone_id: None,
            owner: &WorkOwner::Myself,
            due_at_ms: Some(on_the_clock(6, 17)),
            label_ids: &[],
            source_loop_id: None,
            recurrence: Some("every friday"),
        },
        WorkActor::Me,
        on_the_clock(2, 9),
    )
    .expect("a task");

    super::recur::catch_up(&conn, on_the_clock(8, 9)).expect("the late repeats");
    super::recur::catch_up(&conn, on_the_clock(8, 10)).expect("the late repeats again");
    items::apply(
        &conn,
        &task.id,
        &status(WorkStatus::Done),
        WorkActivityKind::Status,
        WorkActor::Me,
        on_the_clock(8, 11),
    )
    .expect("finished late");

    let instances = items::items(&conn, false)
        .expect("the tracker")
        .into_iter()
        .filter(|item| item.title == "Send the weekly update")
        .map(|item| item.due_at_ms)
        .collect::<Vec<_>>();
    assert_eq!(
        instances,
        vec![Some(on_the_clock(6, 17)), Some(on_the_clock(13, 17))]
    );
}

/// Evidence keeps the words it quoted only while its source still has them:
/// a deleted dictation leaves its task a link that says it is gone, and
/// nothing of what was said.
#[test]
fn a_deleted_dictation_takes_its_words_out_of_the_tasks_evidence() {
    let mut history = Connection::open_in_memory().expect("a history database");
    rusqlite_migration::Migrations::new(crate::managers::history::MIGRATIONS.to_vec())
        .to_latest(&mut history)
        .expect("the history schema");
    history
        .execute(
            "INSERT INTO transcription_history (file_name, timestamp, saved, title, transcription_text)
             VALUES ('sona-1.wav', 1, 0, 'Recording', 'Send the deck to Dana')",
            [],
        )
        .expect("a dictation");
    let recording = history.last_insert_rowid();
    let task = items::insert(
        &history,
        &NewItem {
            title: "Send the deck",
            details: None,
            status: WorkStatus::Triage,
            priority: 0,
            project_id: None,
            milestone_id: None,
            owner: &WorkOwner::Nobody,
            due_at_ms: None,
            label_ids: &[],
            source_loop_id: None,
            recurrence: None,
        },
        WorkActor::Me,
        AT,
    )
    .expect("a task");
    super::capture::attach(
        &history,
        &task.id,
        &super::capture::WorkCaptureSource::Dictation {
            history_id: recording,
            start_ms: 0,
            end_ms: 1_000,
            quote: "Send the deck to Dana".to_owned(),
        },
        AT,
    )
    .expect("the dictation kept as evidence");
    let evidence = |history: &Connection| {
        super::detail(history, None, None, std::path::Path::new("/"), &task.id)
            .expect("the task")
            .evidence
            .into_iter()
            .map(|link| (link.available, link.caption))
            .collect::<Vec<_>>()
    };
    let before = evidence(&history);

    history
        .execute(
            "DELETE FROM transcription_history WHERE id = ?1",
            [recording],
        )
        .expect("the dictation deleted");

    assert_eq!(before, [(true, Some("Send the deck to Dana".to_owned()))]);
    assert_eq!(evidence(&history), [(false, None)]);
}

#[test]
fn the_follow_up_names_who_made_a_promise_still_in_triage() {
    let (directory, meetings) = workflow_core_tests::store();
    let meeting = workflow_core_tests::meeting(&meetings, "Launch sync", AT);
    workflow_core_tests::transcript_segments(&meetings, meeting, &["I will send the deck"]);
    // PANIC: a fixture that cannot identify its known local owner is broken.
    let renamed = meetings
        .with_planning_snapshot(|connection| {
            Ok(connection.execute(
                "UPDATE meeting_speakers SET display_name = 'Local speaker'
                 WHERE session_id = ?1 AND source_kind = 'microphone'",
                [meeting.uuid().to_string()],
            )?)
        })
        .expect("the fixture's known local speaker");
    assert_eq!(renamed, 1);
    let mut ledger = notes(&["Send the deck"]);
    ledger.commitments[0].who = "Local speaker".to_owned();
    ledger.commitments[0].receipt.speaker = Some("Local speaker".to_owned());
    loops_tests::file_ledger(&meetings, meeting, ledger);
    // PANIC: a fixture that cannot open its database is a broken test.
    let history = Connection::open_in_memory().expect("an in-memory history database");
    // PANIC: as above, for the planner schema.
    schema::apply_all(&history).expect("the planner schema");
    let mut desk = Desk {
        _directory: directory,
        meetings,
        history,
        meeting,
        now: AT,
    };
    let taken = desk.take_in();
    let now = desk.now;
    let meeting_id = meeting.uuid().to_string();
    let follow_up = desk
        .both(|history, meetings| {
            let listed = list(
                history,
                Some(meetings),
                Some("Dana"),
                &WorkQuery {
                    source_meeting_id: Some(meeting_id.clone()),
                    include_snoozed: true,
                    ..WorkQuery::default()
                },
                now,
            )?;
            Ok(live::follow_up(
                meeting_id,
                Some("Launch sync"),
                Some("Dana"),
                listed,
            ))
        })
        .expect("the follow-up");

    assert_eq!(statuses(&taken), [WorkStatus::Triage]);
    let lines = follow_up
        .lines
        .iter()
        .map(|line| line.owner_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(lines, ["Dana"]);
}

#[test]
fn a_task_linked_to_a_whole_meeting_is_listed_under_that_meeting() {
    let (directory, meetings) = workflow_core_tests::store();
    let meeting = workflow_core_tests::meeting(&meetings, "Slipway inspection", AT);
    // PANIC: a fixture that cannot open its database is a broken test.
    let history = Connection::open_in_memory().expect("an in-memory history database");
    // PANIC: as above, for the planner schema.
    schema::apply_all(&history).expect("the planner schema");
    let mut desk = Desk {
        _directory: directory,
        meetings,
        history,
        meeting,
        now: AT,
    };
    let meeting_id = meeting.uuid().to_string();
    let listed = desk
        .both(|history, meetings| {
            let task = items::insert(
                history,
                &NewItem {
                    title: "Book a follow-up on Slipway inspection",
                    details: None,
                    status: WorkStatus::Triage,
                    priority: 0,
                    project_id: None,
                    milestone_id: None,
                    owner: &WorkOwner::Nobody,
                    due_at_ms: None,
                    label_ids: &[],
                    source_loop_id: None,
                    recurrence: None,
                },
                WorkActor::Agent,
                AT,
            )?;
            planning_store::insert_link(
                history,
                &planning_store::SourceLinkDraft {
                    target_kind: EvidenceTargetKind::Task,
                    target_id: &task.id,
                    source_kind: EvidenceSourceKind::Meeting,
                    source_id: &meeting_id,
                    at_ms: None,
                    meeting_note_id: None,
                },
                AT,
            )?;
            Ok(list(history, Some(meetings), None, &WorkQuery::default(), AT)?)
        })
        .expect("the listed tasks");

    let sources = listed
        .items
        .iter()
        .map(|row| {
            (
                row.source_meeting_id.as_deref(),
                row.source_meeting_title.as_deref(),
                row.source_quote.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sources,
        [(Some(meeting_id.as_str()), Some("Slipway inspection"), None)]
    );
}

#[test]
fn unidentified_ownership_saved_keeps_manual_owners_after_new_notes() {
    fn unknown_notes(desk: &mut Desk) {
        let meeting = desk.meeting.uuid().to_string();
        desk.both(|_, meetings| {
            meetings.execute(
                "UPDATE meeting_artifact_revisions SET state = 'out_of_date' WHERE session_id = ?1",
                [&meeting],
            )?;
            Ok(())
        })
        .expect("the previous notes made stale");
        let mut ledger = notes(&["Send the pricing deck"]);
        ledger.commitments[0].who = "I".to_owned();
        ledger.commitments[0].receipt.speaker = None;
        loops_tests::file_ledger(&desk.meetings, desk.meeting, ledger);
    }

    let mut desk = desk(&["Send the pricing deck"]);
    unknown_notes(&mut desk);
    assert_eq!(
        desk.commitments()[0].direction,
        crate::meeting::loop_types::MeetingLoopDirection::Unattributed,
    );
    let (item, _) = desk.accepted();
    let at = desk.tick();
    let chosen = desk
        .both(|history, meetings| {
            update(
                history,
                Some(meetings),
                by_me(&operation(), at),
                &item.id,
                item.revision,
                &WorkPatch {
                    owner: Some(WorkOwner::Myself),
                    ..WorkPatch::default()
                },
            )
        })
        .expect("the user's explicit owner edit")
        .value;
    unknown_notes(&mut desk);
    let retaken = desk.take_in();
    let reread = desk.item(&chosen.id);
    assert_eq!(reread.owner(), WorkOwner::Myself);
    assert_eq!(reread, chosen);
    assert!(retaken.is_empty());
    let at = desk.tick();
    let typed = create(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &WorkItemInput {
            owner: Some(WorkOwner::Myself),
            ..typed("A manually owned task")
        },
    )
    .expect("a typed task without a speaker quote")
    .value;
    assert_eq!(typed.owner(), WorkOwner::Myself);
}

#[test]
fn restoring_from_the_bin_brings_back_whether_the_task_was_archived() {
    let mut desk = desk(&[]);
    let at = desk.tick();
    let open = create(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &typed("Book the venue"),
    )
    .expect("an open task")
    .value;
    let shelved = create(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &typed("Order the cake"),
    )
    .expect("a task to archive")
    .value;
    let at = desk.tick();
    set_archived(
        &mut desk.history,
        by_me(&operation(), at),
        std::slice::from_ref(&shelved.id),
        true,
    )
    .expect("archived");

    let at = desk.tick();
    for id in [&open.id, &shelved.id] {
        bin::trash(&desk.history, id, WorkActor::Me, at).expect("deleted");
    }
    assert!(items::items(&desk.history, true)
        .expect("every list")
        .is_empty());
    assert_eq!(bin::binned(&desk.history, at).expect("the bin").len(), 2);

    let at = desk.tick();
    let restored = [&open.id, &shelved.id].map(|id| {
        bin::untrash(&desk.history, id, WorkActor::Me, at)
            .expect("restored")
            .after
            .archived_at_ms
            .is_some()
    });
    assert_eq!(restored, [false, true]);

    let at = desk.tick();
    bin::trash(&desk.history, &open.id, WorkActor::Me, at).expect("deleted again");
    let later = at + crate::planning::catalog::BIN_MS;
    assert!(bin::untrash(&desk.history, &open.id, WorkActor::Me, later).is_err());
    assert_eq!(
        bin::purge(&desk.history, later).expect("the purge"),
        [open.id.clone()]
    );
}

#[test]
fn the_inbox_hears_what_others_change_on_the_users_tasks_and_not_the_user() {
    fn by_agent(operation_id: &str, at_ms: i64) -> Change<'_> {
        Change {
            operation_id,
            actor: WorkActor::Agent,
            at_ms,
        }
    }
    let mut desk = desk(&[]);
    let at = desk.tick();
    let mine = create(
        &mut desk.history,
        None,
        by_agent(&operation(), at),
        &WorkItemInput {
            owner: Some(WorkOwner::Myself),
            ..typed("Book the venue")
        },
    )
    .expect("a task an agent gave the user")
    .value;
    assert!(inbox::subscribed(&desk.history, &mine.id).expect("subscribed"));

    let at = desk.tick();
    let started = update(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &mine.id,
        mine.revision,
        &status(WorkStatus::InProgress),
    )
    .expect("the user starts it")
    .value;
    let at = desk.tick();
    update(
        &mut desk.history,
        None,
        by_agent(&operation(), at),
        &mine.id,
        started.revision,
        &status(WorkStatus::Done),
    )
    .expect("an agent finishes it");

    let now = desk.tick();
    let heard = inbox::notifications(&desk.history, inbox::Shown::default(), now)
        .expect("the inbox")
        .into_iter()
        .map(|notice| (notice.kind, notice.to))
        .collect::<Vec<_>>();
    assert_eq!(
        heard,
        [
            (inbox::NotificationKind::Status, Some("done".to_owned())),
            (inbox::NotificationKind::Assigned, Some("self".to_owned())),
        ]
    );
}

#[test]
fn the_inbox_hears_each_field_an_agent_changes_on_a_followed_task_and_not_the_users() {
    let mut desk = desk(&[]);
    let at = desk.tick();
    let mine = create(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &typed("Book the venue"),
    )
    .expect("a task the user files")
    .value;
    let at = desk.tick();
    let dated = update(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &mine.id,
        mine.revision,
        &WorkPatch {
            due_at_ms: Some(Some(at + 86_400_000)),
            ..WorkPatch::default()
        },
    )
    .expect("the user dates it")
    .value;
    let at = desk.tick();
    update(
        &mut desk.history,
        None,
        Change {
            operation_id: &operation(),
            actor: WorkActor::Agent,
            at_ms: at,
        },
        &mine.id,
        dated.revision,
        &WorkPatch {
            priority: Some(1),
            ..WorkPatch::default()
        },
    )
    .expect("an agent raises it");

    let now = desk.tick();
    let heard = inbox::notifications(&desk.history, inbox::Shown::default(), now)
        .expect("the inbox")
        .into_iter()
        .map(|notice| (notice.kind, notice.field))
        .collect::<Vec<_>>();
    assert_eq!(
        heard,
        [(inbox::NotificationKind::Changed, Some("priority".to_owned()))]
    );
}

#[test]
fn writing_a_task_handle_in_the_details_links_that_task_once() {
    let mut desk = desk(&[]);
    let made = |desk: &mut Desk, title: &str| {
        let at = desk.tick();
        create(&mut desk.history, None, by_me(&operation(), at), &typed(title))
            .expect("a typed item")
            .value
    };
    let launch = made(&mut desk, "Ship the launch");
    let outline = made(&mut desk, "Send the outline");

    let mut current = outline;
    for _ in 0..2 {
        let at = desk.tick();
        current = update(
            &mut desk.history,
            None,
            by_me(&operation(), at),
            &current.id,
            current.revision,
            &WorkPatch {
                details: Some(Some(format!(
                    "Needs S-{} first; not S-999 or S-{} itself.",
                    launch.number, current.number
                ))),
                ..WorkPatch::default()
            },
        )
        .expect("details saved")
        .value;
        desk.history
            .execute("UPDATE planning_tasks SET details = 'cleared' WHERE id = ?1", [&current.id])
            .expect("details cleared");
    }

    let linked = items::relations(&desk.history, &current.id)
        .expect("the outline's relations")
        .into_iter()
        .map(|relation| (relation.from_id, relation.to_id, relation.kind))
        .collect::<Vec<_>>();
    assert_eq!(
        linked,
        [(current.id.clone(), launch.id.clone(), WorkRelationKind::Related)]
    );
}

#[test]
fn a_parent_closes_once_its_last_open_sub_task_does() {
    let mut desk = desk(&[]);
    let made = |desk: &mut Desk, title: &str, parent: Option<&str>| {
        let at = desk.tick();
        let item = create(&mut desk.history, None, by_me(&operation(), at), &typed(title))
            .expect("a typed item")
            .value;
        let Some(parent) = parent else { return item };
        let at = desk.tick();
        update(
            &mut desk.history,
            None,
            by_me(&operation(), at),
            &item.id,
            item.revision,
            &WorkPatch {
                parent_id: Some(Some(parent.to_owned())),
                ..WorkPatch::default()
            },
        )
        .expect("made a sub-task")
        .value
    };
    let launch = made(&mut desk, "Ship the launch", None);
    let outline = made(&mut desk, "Send the outline", Some(&launch.id));
    let deck = made(&mut desk, "Draw the deck", Some(&launch.id));

    let finish = |desk: &mut Desk, item: &WorkItem, to: WorkStatus| {
        let at = desk.tick();
        let mut done = update(
            &mut desk.history,
            None,
            by_me(&operation(), at),
            &item.id,
            item.revision,
            &status(to),
        )
        .expect("sub-task closed");
        close_parents(&mut desk.history, None, &mut done.effects).expect("parents closed");
        desk.item(&launch.id).status
    };
    assert_eq!(finish(&mut desk, &outline, WorkStatus::Done), launch.status);
    assert_eq!(finish(&mut desk, &deck, WorkStatus::Canceled), WorkStatus::Done);
    let closed_by = items::activity(&desk.history, &launch.id)
        .expect("the launch's history")
        .last()
        .map(|entry| entry.actor);
    assert_eq!(closed_by, Some(WorkActor::Sona));
}

#[test]
fn a_view_saved_before_sub_tasks_swimlanes_and_hidden_columns_reads_with_their_defaults() {
    let desk = desk(&[]);
    let saved_before = r#"{"layout":"board","groupBy":"project","orderBy":"priority","showEmptyGroups":false,"properties":["id","priority"]}"#;
    desk.history
        .execute(
            "INSERT INTO planning_views (id, name, query_json, display_json, revision, created_at_ms, updated_at_ms)
             VALUES ('old', 'Old view', '{}', ?1, 1, 0, 0)",
            [saved_before],
        )
        .expect("a view in the old shape");

    let view = items::views(&desk.history)
        .expect("the saved views")
        .remove(0);
    assert_eq!(view.display.group_by, WorkGroupBy::Project);
    assert!(view.display.show_sub_tasks);
    assert_eq!(view.display.sub_group_by, WorkGroupBy::None);
    assert!(view.display.hidden_columns.is_empty());
}

#[test]
fn moving_tasks_puts_them_at_either_end_or_beside_another_in_the_manual_order() {
    let mut desk = desk(&[]);
    let mut ids = HashMap::new();
    for title in ["A", "B", "C", "D"] {
        let at = desk.tick();
        let made = create(&mut desk.history, None, by_me(&operation(), at), &typed(title))
            .expect("a typed task")
            .value;
        ids.insert(title, made.id);
    }
    let id = |title: &str| ids[title].clone();
    let order = |history: &Connection| -> String {
        history
            .prepare("SELECT title FROM planning_tasks ORDER BY sort_order, number")
            .expect("the order query")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("the order")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("the titles")
            .concat()
    };
    let place = |history: &Connection, moving: &[&str], place: WorkPlace| {
        let moving: Vec<String> = moving.iter().map(|title| id(title)).collect();
        items::place_manually(history, &moving, &place).expect("the move")
    };

    place(&desk.history, &["C"], WorkPlace::Top);
    assert_eq!(order(&desk.history), "CABD");
    place(&desk.history, &["D", "A"], WorkPlace::Bottom);
    assert_eq!(order(&desk.history), "CBAD");
    place(&desk.history, &["D"], WorkPlace::Before(id("B")));
    assert_eq!(order(&desk.history), "CDBA");
    let moved = place(&desk.history, &["C"], WorkPlace::After(id("A")));
    assert_eq!(order(&desk.history), "DBAC");
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].after.revision, moved[0].before.revision + 1);

    desk.history
        .execute("UPDATE planning_tasks SET sort_order = 5.0", [])
        .expect("every task tied");
    let renumbered = place(&desk.history, &["A"], WorkPlace::Before(id("C")));
    assert_eq!(order(&desk.history), "BACD");
    let sorts: Vec<f64> = renumbered.iter().map(|changed| changed.after.sort_order).collect();
    assert!(sorts.windows(2).all(|pair| pair[0] < pair[1]), "{sorts:?}");

    assert!(items::place_manually(&desk.history, &[id("B")], &WorkPlace::Before(id("B"))).is_err());
}

/// A task keeps the moment it was finished while it stays done, and loses
/// it once it opens again.
#[test]
fn a_task_keeps_when_it_was_done_until_it_opens_again() {
    // PANIC: a fixture that cannot open its database is a broken test.
    let conn = Connection::open_in_memory().expect("an in-memory history database");
    // PANIC: as above, for the planner schema.
    schema::apply_all(&conn).expect("the planner schema");
    // PANIC: a fixture that cannot add its task is a broken test.
    let task = items::insert(
        &conn,
        &NewItem {
            title: "Send the deck",
            details: None,
            status: WorkStatus::NotStarted,
            priority: 0,
            project_id: None,
            milestone_id: None,
            owner: &WorkOwner::Myself,
            due_at_ms: None,
            label_ids: &[],
            source_loop_id: None,
            recurrence: None,
        },
        WorkActor::Me,
        on_the_clock(2, 9),
    )
    .expect("a task");
    let change = |patch: &WorkPatch, kind: WorkActivityKind, hour: u32| {
        // PANIC: each change is one the tracker takes.
        items::apply(&conn, &task.id, patch, kind, WorkActor::Me, on_the_clock(3, hour))
            .expect("a change");
        items::item(&conn, &task.id).expect("the task").completed_at_ms
    };
    let estimate = WorkPatch {
        estimate: Some(Some(3)),
        ..WorkPatch::default()
    };

    assert_eq!(
        [
            change(&status(WorkStatus::Done), WorkActivityKind::Status, 10),
            change(&estimate, WorkActivityKind::Estimate, 11),
            change(&status(WorkStatus::InProgress), WorkActivityKind::Status, 12),
        ],
        [Some(on_the_clock(3, 10)), Some(on_the_clock(3, 10)), None]
    );
}

/// Work finished before tasks kept when they were done takes the last time
/// its history shows it done, and its last status change when it has none.
#[test]
fn done_work_from_before_takes_when_it_was_done_from_its_history() {
    // PANIC: a fixture that cannot open its database is a broken test.
    let mut history = Connection::open_in_memory().expect("a history database");
    let all = crate::managers::history::MIGRATIONS.to_vec();
    let columns = all
        .iter()
        .position(|step| *step == rusqlite_migration::M::up(schema::TASK_COLUMNS_MIGRATION))
        .expect("the task columns migration");
    let migrations = rusqlite_migration::Migrations::new(all);
    // PANIC: as above, for the schema before the task columns.
    migrations.to_version(&mut history, columns).expect("the schema before");
    // PANIC: a fixture that cannot add its rows is a broken test.
    history
        .execute_batch(
            "INSERT INTO planning_tasks (id, number, title, owner_is_self, status, priority,
                 status_changed_at_ms, sort_order, revision, created_at_ms, updated_at_ms)
             VALUES ('logged', 1, 'Logged', 1, 'done', 0, 9000, 1, 1, 1000, 9000),
                    ('unlogged', 2, 'Unlogged', 1, 'done', 0, 7000, 2, 1, 1000, 7000),
                    ('open', 3, 'Open', 1, 'in_progress', 0, 8000, 3, 1, 1000, 8000);
             INSERT INTO planning_task_activity (id, task_id, at_ms, actor, kind, from_value, to_value)
             VALUES ('a1', 'logged', 3000, 'me', 'status', 'not_started', 'done'),
                    ('a2', 'logged', 4000, 'me', 'status', 'done', 'in_progress'),
                    ('a3', 'logged', 5000, 'me', 'status', 'in_progress', 'done'),
                    ('a4', 'logged', 6000, 'me', 'title', 'Old', 'done'),
                    ('a5', 'open', 2000, 'me', 'status', 'not_started', 'done');",
        )
        .expect("tasks from before");
    // PANIC: as above, for the migration under test.
    migrations.to_latest(&mut history).expect("the task columns");

    let mut read = history
        .prepare("SELECT id, completed_at_ms FROM planning_tasks ORDER BY number")
        .expect("a read");
    let done: Vec<(String, Option<i64>)> = read
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("the tasks")
        .collect::<rusqlite::Result<_>>()
        .expect("each task");
    assert_eq!(
        done,
        [
            ("logged".to_owned(), Some(5000)),
            ("unlogged".to_owned(), Some(7000)),
            ("open".to_owned(), None),
        ]
    );
}

/// The weekly digest tells a project's work done that week, leaves out work
/// drawn from meetings it cannot read, and posts once per week's key.
#[test]
fn the_weekly_digest_leaves_out_meeting_work_and_posts_once_a_week() {
    let mut desk = desk(&["Send the outline"]);
    let (tracked, _) = desk.accepted();
    let at = desk.tick();
    // PANIC: a project on the fixture's own database.
    let project =
        planning_store::insert_project(&desk.history, "Launch", None, at).expect("a project");
    let booked = create(
        &mut desk.history,
        None,
        by_me(&operation(), at),
        &WorkItemInput {
            project_id: Some(project.id.clone()),
            ..typed("Book the venue")
        },
    )
    .expect("a task typed into the project")
    .value;
    for task in [&tracked, &booked] {
        let at = desk.tick();
        update(
            &mut desk.history,
            None,
            by_me(&operation(), at),
            &task.id,
            task.revision,
            &WorkPatch {
                project_id: Some(Some(project.id.clone())),
                ..status(WorkStatus::Done)
            },
        )
        .expect("the task done in the project");
    }
    let now = desk.tick();

    let hidden = super::digest::hidden(&desk.history, None).expect("the hidden tasks");
    let digest = super::digest::build(&desk.history, &hidden, now - super::digest::WEEK_MS, now)
        .expect("the digest");
    let done: Vec<(&str, Vec<&str>)> = digest
        .projects
        .iter()
        .map(|told| {
            let ids = told.done.iter().map(|task| task.id.as_str()).collect();
            (told.title.as_str(), ids)
        })
        .collect();
    assert_eq!(done, [("Launch", vec![booked.id.as_str()])]);

    let posted = [now, now + 1].map(|at| {
        inbox::digest_posted(&desk.history, &digest, Some("digest:2023-11-13"), at)
            .expect("a post")
            .is_some()
    });
    let heard = inbox::notifications(&desk.history, inbox::Shown::default(), now + 1)
        .expect("the inbox");
    assert_eq!(posted, [true, false]);
    assert_eq!(
        heard
            .iter()
            .filter_map(|notice| notice.digest.as_ref())
            .collect::<Vec<_>>(),
        [&digest]
    );
}
