//! The tracker against a recorded meeting, through the functions its
//! commands and hooks run: intake, triage, the two-way commitment sync, bulk
//! changes, archive and duplicates, and the migration that made it.

use super::super::schema;
use super::super::store as planning_store;
use super::items::{self, NewItem};
use super::sync;
use super::types::{WorkActivityKind, WorkItem, WorkItemInput, WorkOwner, WorkPatch, WorkStatus};
use super::{accept, bulk_update, create, mark_duplicate, set_archived, update};
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
use crate::planning::types::EvidenceSourceKind;
use rusqlite::{Connection, TransactionBehavior};
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

const AT: i64 = 1_700_000_000_000;

fn operation() -> String {
    Uuid::new_v4().to_string()
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
    let said = promises
        .iter()
        .map(|what| format!("I will {}", what.to_lowercase()))
        .collect::<Vec<_>>();
    workflow_core_tests::transcript_segments(
        &meetings,
        meeting,
        &said.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let commitments = (0_u64..)
        .zip(&said)
        .zip(promises)
        .map(|((second, quote), what)| LedgerCommitment {
            who: "Dana".to_owned(),
            what: (*what).to_owned(),
            firmness: LedgerFirmness::Firm,
            receipt: LedgerReceipt {
                quote: quote.clone(),
                speaker: Some("Dana".to_owned()),
                t_ms: second * 1_000,
                citations: Vec::new(),
            },
        })
        .collect();
    loops_tests::file_ledger(
        &meetings,
        meeting,
        MeetingLedger {
            headline: "Dana took the launch errands.".to_owned(),
            threads: Vec::new(),
            open_loops: Vec::new(),
            commitments,
            stances: Vec::new(),
            caveats: Vec::new(),
            receipts: LedgerReceiptState::Verified,
        },
    );
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
                    meetings,
                    None,
                    &operation(),
                    &[triaged.id],
                    false,
                    at,
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
    let conn = Connection::open_in_memory().expect("history");
    for migration in [
        schema::MIGRATION,
        schema::QUIET_REVIEW_MIGRATION,
        schema::MEETING_NOTE_LINK_MIGRATION,
        schema::DISMISSED_LOOP_MIGRATION,
    ] {
        conn.execute_batch(migration)
            .expect("an earlier planner migration");
    }
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

    conn.execute_batch(schema::WORK_MIGRATION)
        .expect("the tracker migration");

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
        },
        super::types::WorkActor::Me,
        AT,
    )
    .expect("an item outside any project");
    assert_eq!((loose.number, loose.project_id), (4, None));
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
                meetings,
                &operation(),
                &item.id,
                item.revision,
                &status(WorkStatus::Done),
                at,
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
        &operation(),
        &typed("Order the banners"),
        at,
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
            meetings,
            &operation(),
            &[tracked.id.clone(), banners.id.clone(), missing],
            &finish,
            at,
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
                meetings,
                &operation(),
                &[tracked.id.clone(), banners.id.clone()],
                &finish,
                at,
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
        &operation(),
        std::slice::from_ref(&item.id),
        true,
        at,
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
        &operation(),
        std::slice::from_ref(&item.id),
        false,
        at,
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
        &operation(),
        &typed("Send the launch outline"),
        at,
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
                meetings,
                &operation(),
                &repeat.id,
                &original.id,
                at,
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
            meetings,
            &operation(),
            &original.id,
            original.revision,
            &status(WorkStatus::Done),
            at,
        )
    })
    .expect("the original done");
    assert_eq!(desk.page_status(&loop_id), MeetingLoopStatus::Done);
}
