//! Meeting commitments nobody has put in the planner yet, and the two answers
//! a person gives one on the meeting page: track it in a project, or say it
//! is not work. Every open commitment also waits in the tracker's triage;
//! these answers accept or decline that triage item.
//!
//! Tracking copies the commitment's words into the task title. That copy is
//! the person's own task once they pick a project, so it stays when the
//! meeting is deleted, as a title they typed would.

use super::sources;
use super::store::{self, Refusal};
use super::types::{EvidenceSourceKind, EvidenceTargetKind, PlanningTask, Project};
use super::work::sync::{self, MeetingReader};
use super::work::types::{WorkActivityKind, WorkActor, WorkPatch, WorkStatus};
use super::work::{self, items, Change, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::meeting::loop_types::{
    MeetingLoopId, MeetingLoopKind, MeetingLoopRow, MeetingLoopStatus,
};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::today::{finished_meeting_ids, TodayTally};
use crate::meeting::store::{loops, StoreError};
use crate::meeting::types::MeetingSessionId;
use anyhow::Result as StorageResult;
use rusqlite::{params, Connection};
use serde::Serialize;
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, State};

/// Live planner work cites the loop: a task or milestone that was not
/// canceled, or a project. An accepted plan links a commitment it found to a
/// project or milestone instead of copying it into a task, so those count.
pub(super) const TRACKED: &str = "SELECT EXISTS(
    SELECT 1 FROM planning_evidence_links l
    WHERE l.source_kind = 'loop' AND l.source_id = ?1 AND CASE l.target_kind
        WHEN 'task' THEN EXISTS(
            SELECT 1 FROM planning_tasks t WHERE t.id = l.target_id AND t.status != 'canceled')
        WHEN 'milestone' THEN EXISTS(
            SELECT 1 FROM planning_milestones m WHERE m.id = l.target_id AND m.status != 'canceled')
        ELSE EXISTS(SELECT 1 FROM planning_projects p WHERE p.id = l.target_id)
    END
)";

pub(super) const DISMISSED: &str =
    "SELECT EXISTS(SELECT 1 FROM planning_dismissed_loops WHERE loop_id = ?1)";

/// One meeting's open commitments that no live planner work cites and the
/// person has not set aside, with the projects still taking work, the one
/// worked in most recently first.
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct UntrackedActions {
    pub actions: Vec<MeetingLoopRow>,
    pub projects: Vec<Project>,
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_untracked_actions(
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<UntrackedActions, String> {
    let user_name = meetings.user_name();
    super::with_sources(&history_state, &meetings, |history, meetings| {
        read(history, meetings, session_id, user_name.as_deref())
    })
    .await
}

/// Accepts a meeting commitment's triage item into the chosen project,
/// with the owner and due day the meeting suggests, unless planner work
/// already tracks it.
#[tauri::command]
#[specta::specta]
pub async fn planning_track_action(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    loop_id: String,
    project_id: String,
    operation_id: String,
) -> Result<PlanningTask, String> {
    track_action_as(
        &app,
        &history_state,
        &meetings,
        WorkActor::Me,
        &operation_id,
        &loop_id,
        &project_id,
    )
    .await
}

/// [`planning_track_action`] made by `actor`.
pub(crate) async fn track_action_as(
    app: &AppHandle,
    history: &HistoryManager,
    meetings: &MeetingSessionManager,
    actor: WorkActor,
    operation_id: &str,
    loop_id: &str,
    project_id: &str,
) -> Result<PlanningTask, String> {
    let change = Change::now(operation_id, actor);
    let user_name = meetings.user_name();
    let done = work::with_both(history, meetings, |history, meeting_conn| {
        track(
            history,
            meeting_conn,
            user_name.as_deref(),
            loop_id,
            project_id,
            change,
        )
    })
    .await?;
    work::announce(app, meetings, &done.effects).await;
    Ok(done.value)
}

/// Remembers that a meeting commitment is not work, so it stops asking to be
/// tracked: its triage item is declined and the commitment dropped in its
/// meeting.
#[tauri::command]
#[specta::specta]
pub async fn planning_dismiss_action(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    loop_id: String,
    operation_id: String,
) -> Result<(), String> {
    set_aside_as(
        &app,
        &history_state,
        &meetings,
        WorkActor::Me,
        &operation_id,
        &loop_id,
        true,
    )
    .await?;
    Ok(())
}

/// Asks about a commitment again after it was set aside as not work: its
/// declined item goes back to triage and the commitment reopens.
#[tauri::command]
#[specta::specta]
pub async fn planning_restore_action(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    loop_id: String,
    operation_id: String,
) -> Result<(), String> {
    set_aside_as(
        &app,
        &history_state,
        &meetings,
        WorkActor::Me,
        &operation_id,
        &loop_id,
        false,
    )
    .await?;
    Ok(())
}

/// [`planning_dismiss_action`], or [`planning_restore_action`] when
/// `dismissed` is false, made by `actor`. Returns the ids of the tasks it
/// changed.
pub(crate) async fn set_aside_as(
    app: &AppHandle,
    history: &HistoryManager,
    meetings: &MeetingSessionManager,
    actor: WorkActor,
    operation_id: &str,
    loop_id: &str,
    dismissed: bool,
) -> Result<Vec<String>, String> {
    let change = Change::now(operation_id, actor);
    let done = work::with_both(history, meetings, |history, meeting_conn| {
        if dismissed {
            dismiss(history, meeting_conn, loop_id, change)
        } else {
            restore(history, meeting_conn, loop_id, change)
        }
    })
    .await?;
    work::announce(app, meetings, &done.effects).await;
    Ok(done.effects.items)
}

/// Home's "not tracked" count: the commitments across finished meetings that
/// still wait to be tracked, and the newest meeting holding one.
pub(crate) async fn tally(
    history: &HistoryManager,
    meetings: &MeetingSessionManager,
    hidden: &(dyn Fn(MeetingSessionId) -> bool + Sync),
) -> Result<TodayTally, String> {
    let user_name = meetings.user_name();
    super::with_sources(history, meetings, |history, meetings| {
        tally_in(history, meetings, user_name.as_deref(), hidden)
    })
    .await
}

fn tally_in(
    history: &Connection,
    meetings: &Connection,
    user_name: Option<&str>,
    hidden: &dyn Fn(MeetingSessionId) -> bool,
) -> StorageResult<TodayTally> {
    let finished = finished_meeting_ids(meetings)
        .map_err(|_| anyhow::anyhow!("finished meetings could not be read"))?;
    let mut tally = TodayTally::default();
    for session_id in finished.into_iter().filter(|id| !hidden(*id)) {
        let waiting = untracked_in(history, meetings, session_id, user_name)?.len();
        if waiting == 0 {
            continue;
        }
        tally.count = tally.count.saturating_add(u32::try_from(waiting)?);
        tally.meeting_id.get_or_insert(session_id);
    }
    Ok(tally)
}

fn read(
    history: &Connection,
    meetings: &Connection,
    session_id: MeetingSessionId,
    user_name: Option<&str>,
) -> StorageResult<UntrackedActions> {
    Ok(UntrackedActions {
        actions: untracked_in(history, meetings, session_id, user_name)?,
        projects: store::projects_by_activity(history)?,
    })
}

fn untracked_in(
    history: &Connection,
    meetings: &Connection,
    session_id: MeetingSessionId,
    user_name: Option<&str>,
) -> StorageResult<Vec<MeetingLoopRow>> {
    let loops = loops::loops_in(meetings, session_id, user_name).map_err(|error| match error {
        StoreError::NotFound => anyhow::Error::from(Refusal::NotFound),
        _ => anyhow::anyhow!("meeting loops could not be read"),
    })?;
    let mut actions = Vec::new();
    for row in loops.rows {
        if row.kind != MeetingLoopKind::Commitment || row.status != MeetingLoopStatus::Open {
            continue;
        }
        let lineage = lineage(meetings, row.loop_id.as_str())?;
        if any(history, TRACKED, &lineage)?
            || any(history, DISMISSED, &lineage)?
            || sync::tracker_of(history, &lineage)?
                .is_some_and(|item| item.status != WorkStatus::Triage)
        {
            continue;
        }
        if sources::loop_words(history, meetings, row.loop_id.as_str())?.is_some() {
            actions.push(row);
        }
    }
    Ok(actions)
}

#[derive(Serialize)]
struct TrackRequest<'a> {
    loop_id: &'a str,
    project_id: &'a str,
}

fn track(
    history: &mut Connection,
    meetings: &Connection,
    user_name: Option<&str>,
    loop_id: &str,
    project_id: &str,
    change: Change<'_>,
) -> StorageResult<Done<PlanningTask>> {
    let Change {
        operation_id,
        actor,
        at_ms: now_ms,
    } = change;
    let loop_id = sources::canonical_source_id(EvidenceSourceKind::Loop, loop_id)?;
    let project_id = store::uuid(project_id)?;
    let request = TrackRequest {
        loop_id: &loop_id,
        project_id: &project_id,
    };
    store::mutate(
        history,
        operation_id,
        "track_loop",
        &request,
        now_ms,
        |tx| {
            if any(tx, TRACKED, &lineage(meetings, &loop_id)?)? {
                return Err(Refusal::AlreadyTracked.into());
            }
            store::project(tx, &project_id)?;
            let item = match items::by_source_loop(tx, &loop_id)? {
                Some(item) => item,
                None => {
                    let session_id = MeetingLoopId(loop_id.clone())
                        .session_id()
                        .ok_or(Refusal::Invalid)?;
                    let meeting = sync::meeting_rows(meetings, session_id, user_name)?
                        .ok_or(Refusal::SourceUnavailable)?;
                    let row = meeting
                        .rows
                        .iter()
                        .find(|row| row.loop_id.as_str() == loop_id)
                        .ok_or(Refusal::SourceUnavailable)?;
                    sync::take_in_row(tx, meetings, user_name, &meeting, row, now_ms)?
                        .ok_or(Refusal::SourceUnavailable)?
                }
            };
            if item.status != WorkStatus::Triage {
                return Err(Refusal::AlreadyTracked.into());
            }
            let mut reader = MeetingReader::new(Some(meetings), user_name);
            let patch = sync::acceptance(tx, &mut reader, &item, true, Some(&project_id))?;
            let (after, moved) =
                sync::accept(tx, Some(meetings), &item, &patch, actor, now_ms)?;
            store::insert_link(
                tx,
                &store::SourceLinkDraft {
                    target_kind: EvidenceTargetKind::Task,
                    target_id: &after.id,
                    source_kind: EvidenceSourceKind::Loop,
                    source_id: &loop_id,
                    at_ms: None,
                    meeting_note_id: None,
                },
                now_ms,
            )?;
            let mut effects = Effects::default();
            effects.item(&item);
            effects.item(&after);
            effects.loops.extend(moved);
            Ok(Done {
                value: store::task(tx, &after.id)?,
                effects,
            })
        },
    )
}

fn dismiss(
    history: &mut Connection,
    meetings: &Connection,
    loop_id: &str,
    change: Change<'_>,
) -> StorageResult<Done<()>> {
    let Change {
        operation_id,
        actor,
        at_ms: now_ms,
    } = change;
    let loop_id = sources::canonical_source_id(EvidenceSourceKind::Loop, loop_id)?;
    store::mutate(
        history,
        operation_id,
        "dismiss_loop",
        &loop_id,
        now_ms,
        |tx| {
            tx.execute(
                "INSERT INTO planning_dismissed_loops (loop_id, dismissed_at_ms) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
                params![loop_id, now_ms],
            )?;
            let mut effects = Effects::default();
            if let Some(item) = sync::tracking_item(tx, meetings, &loop_id)?
                .filter(|item| item.status == WorkStatus::Triage)
            {
                let (after, moved) =
                    sync::decline(tx, Some(meetings), &item, None, actor, now_ms)?;
                effects.item(&after);
                effects.loops.extend(moved);
            }
            Ok(Done { value: (), effects })
        },
    )
}

fn restore(
    history: &mut Connection,
    meetings: &Connection,
    loop_id: &str,
    change: Change<'_>,
) -> StorageResult<Done<()>> {
    let Change {
        operation_id,
        actor,
        at_ms: now_ms,
    } = change;
    let loop_id = sources::canonical_source_id(EvidenceSourceKind::Loop, loop_id)?;
    store::mutate(
        history,
        operation_id,
        "restore_loop",
        &loop_id,
        now_ms,
        |tx| {
            tx.execute(
                "DELETE FROM planning_dismissed_loops WHERE loop_id = ?1",
                [&loop_id],
            )?;
            let mut effects = Effects::default();
            if let Some(item) = sync::tracking_item(tx, meetings, &loop_id)?
                .filter(|item| item.status == WorkStatus::Canceled)
            {
                let changed = items::apply(
                    tx,
                    &item.id,
                    &WorkPatch {
                        status: Some(WorkStatus::Triage),
                        ..WorkPatch::default()
                    },
                    WorkActivityKind::Status,
                    actor,
                    now_ms,
                )?;
                effects.changed(&changed);
                effects.loops.extend(sync::settle(
                    tx,
                    Some(meetings),
                    &item.id,
                    actor,
                    now_ms,
                )?);
            }
            Ok(Done { value: (), effects })
        },
    )
}

/// A loop and every earlier occurrence carried forward into it, so a
/// commitment tracked or set aside in one meeting stays answered in the next.
pub(super) fn lineage(meetings: &Connection, loop_id: &str) -> StorageResult<Vec<String>> {
    let mut statement = meetings.prepare_cached(
        "WITH RECURSIVE lineage(loop_id) AS (
             SELECT ?1
             UNION
             SELECT s.loop_id FROM meeting_loop_states s
             JOIN lineage l ON s.carried_into_loop_id = l.loop_id
         )
         SELECT loop_id FROM lineage",
    )?;
    let ids = statement.query_map([loop_id], |row| row.get(0))?;
    Ok(ids.collect::<rusqlite::Result<_>>()?)
}

pub(super) fn any(history: &Connection, query: &str, lineage: &[String]) -> StorageResult<bool> {
    let mut statement = history.prepare_cached(query)?;
    for id in lineage {
        if statement.query_row([id], |row| row.get(0))? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::ledger::{
        LedgerCommitment, LedgerFirmness, LedgerReceipt, LedgerReceiptState, MeetingLedger,
    };
    use crate::meeting::store::workflow_core_tests;
    use crate::meeting::types::{
        ArtifactCitation, CitedArtifactText, GeneratedMeetingArtifacts, TranscriptSegmentId,
    };
    use crate::planning::TaskStatus;
    use uuid::Uuid;

    fn by_agent(operation_id: &str) -> Change<'_> {
        Change {
            operation_id,
            actor: WorkActor::Agent,
            at_ms: AT,
        }
    }

    /// How many entries an agent filed in a task's history, Sona's own left
    /// out; an entry filed by anyone else fails the test.
    fn agent_entries(history: &Connection, task_id: &str) -> usize {
        let actors: Vec<WorkActor> = items::activity(history, task_id)
            .expect("the task's history")
            .into_iter()
            .map(|entry| entry.actor)
            .filter(|actor| *actor != WorkActor::Sona)
            .collect();
        assert!(actors.iter().all(|actor| *actor == WorkActor::Agent), "{actors:?}");
        actors.len()
    }

    const AT: i64 = 1_700_000_000_000;

    fn promise(what: &str, segment_id: TranscriptSegmentId) -> LedgerCommitment {
        LedgerCommitment {
            who: "Dana".to_string(),
            what: what.to_string(),
            firmness: LedgerFirmness::Firm,
            receipt: LedgerReceipt {
                quote: "I will send the outline and book the venue".to_string(),
                speaker: Some("Dana".to_string()),
                t_ms: 0,
                citations: vec![ArtifactCitation {
                    segment_id,
                    start_offset_ns: 0,
                    end_offset_ns: 1,
                }],
            },
        }
    }

    fn texts(untracked: &UntrackedActions) -> Vec<&str> {
        untracked
            .actions
            .iter()
            .map(|row| row.text.as_str())
            .collect()
    }

    #[test]
    fn an_action_is_tracked_once_and_one_set_aside_stops_asking() {
        let (_directory, meetings) = workflow_core_tests::store();
        let session_id = workflow_core_tests::meeting(&meetings, "Launch sync", AT);
        workflow_core_tests::transcript(
            &meetings,
            session_id,
            "I will send the outline and book the venue",
        );
        meetings
            .with_planning_snapshot(|connection| {
                let (transcript_id, segment_id): (String, String) = connection.query_row(
                    "SELECT s.transcript_revision_id, s.segment_id FROM meeting_transcript_segments s
                     JOIN meeting_sessions m ON m.current_transcript_revision_id = s.transcript_revision_id
                     WHERE m.id = ?1",
                    [session_id.uuid().to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let segment_id = TranscriptSegmentId::from_uuid(
                    Uuid::parse_str(&segment_id).expect("saved transcript segment id"),
                );
                let artifacts = GeneratedMeetingArtifacts {
                    summary: CitedArtifactText {
                        text: "Two promises".to_string(),
                        citations: Vec::new(),
                    },
                    summary_trace: Vec::new(),
                    outline: Vec::new(),
                    decisions: Vec::new(),
                    action_items: Vec::new(),
                    key_questions: Vec::new(),
                    risks: Vec::new(),
                    follow_up_draft: CitedArtifactText {
                        text: String::new(),
                        citations: Vec::new(),
                    },
                    ledger: Some(MeetingLedger {
                        headline: "Two promises".to_string(),
                        threads: Vec::new(),
                        open_loops: Vec::new(),
                        commitments: vec![
                            promise("Send the outline", segment_id),
                            promise("Book the venue", segment_id),
                        ],
                        stances: Vec::new(),
                        caveats: Vec::new(),
                        receipts: LedgerReceiptState::Verified,
                    }),
                    ledger_failure: None,
                    plain: false,
                    plain_because: None,
                    left_out: Vec::new(),
                };
                let artifact_id = Uuid::new_v4().to_string();
                connection.execute(
                    "INSERT INTO meeting_artifact_revisions (
                        artifact_id, session_id, transcript_revision_id, input_revision,
                        template_id, template_version, generation_key, state,
                        content_json, generated_at_utc_ms
                    ) VALUES (?1, ?2, ?3, 0, 'test', 1, ?4, 'current', ?5, ?6)",
                    params![
                        artifact_id,
                        session_id.uuid().to_string(),
                        transcript_id,
                        format!("test-{artifact_id}"),
                        serde_json::to_string(&artifacts).expect("encode the ledger"),
                        AT
                    ],
                )?;
                Ok(())
            })
            .expect("save a ledger with two cited promises");

        let mut history = Connection::open_in_memory().expect("history");
        super::super::schema::apply_all(&history).expect("planning migrations");
        let project = store::insert_project(&history, "Launch", None, AT).expect("project");

        meetings
            .with_planning_snapshot(|connection| {
                let untracked = read(&history, connection, session_id, None).expect("first read");
                assert_eq!(texts(&untracked), ["Send the outline", "Book the venue"]);
                assert_eq!(untracked.projects, [project.clone()]);
                let outline = untracked.actions[0].loop_id.as_str();
                let venue = untracked.actions[1].loop_id.as_str();

                let task = track(
                    &mut history,
                    connection,
                    None,
                    outline,
                    &project.id,
                    by_agent(&Uuid::new_v4().to_string()),
                )
                .expect("track the outline")
                .value;
                assert_eq!(task.title, "Send the outline");
                assert_eq!(task.status, TaskStatus::NotStarted);
                assert!(agent_entries(&history, &task.id) > 0);
                let saved = store::records(&history, &project.id).expect("saved records");
                assert!(saved.evidence.iter().any(|link| link.target_id == task.id
                    && link.source_kind == EvidenceSourceKind::Loop
                    && link.source_id == outline));
                assert_eq!(
                    texts(&read(&history, connection, session_id, None).expect("second read")),
                    ["Book the venue"]
                );

                let twice = track(
                    &mut history,
                    connection,
                    None,
                    outline,
                    &project.id,
                    by_agent(&Uuid::new_v4().to_string()),
                )
                .err()
                .expect("a second task for the same promise");
                assert_eq!(
                    twice.downcast_ref::<Refusal>(),
                    Some(&Refusal::AlreadyTracked)
                );
                assert_eq!(
                    store::records(&history, &project.id)
                        .expect("records after refusal")
                        .tasks
                        .len(),
                    1
                );

                dismiss(
                    &mut history,
                    connection,
                    venue,
                    by_agent(&Uuid::new_v4().to_string()),
                )
                .expect("set the venue aside");
                assert!(read(&history, connection, session_id, None)
                    .expect("third read")
                    .actions
                    .is_empty());

                restore(
                    &mut history,
                    connection,
                    venue,
                    by_agent(&Uuid::new_v4().to_string()),
                )
                .expect("ask about the venue again");
                assert_eq!(
                    texts(&read(&history, connection, session_id, None).expect("fourth read")),
                    ["Book the venue"]
                );
                Ok(())
            })
            .expect("read the encrypted meeting snapshot");
    }
}
