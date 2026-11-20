//! One item per promise. Notes written again give a meeting's commitments
//! new ids, and a promise made again in a later meeting comes in as a
//! commitment of its own; either way the item that came first goes on, and
//! the later meeting becomes one more of its receipts.

use super::batches::{self, MoveKind};
use super::comments;
use super::inbox;
use super::items;
use super::sync::{same_words, settle, MeetingReader};
use super::types::{
    WorkActivityKind, WorkActor, WorkCommentSubject, WorkItem, WorkPatch, WorkStatus,
};
use crate::managers::history::HistoryManager;
use crate::meeting::loop_types::{MeetingLoopDirection, MeetingLoopKind, MeetingLoopRow};
use anyhow::Result;
use chrono::Days;
use rusqlite::{params, Connection, TransactionBehavior};
use std::collections::HashSet;
use tauri::AppHandle;

pub(crate) const CYCLES_ARCHIVE_MIGRATION: &str =
    "ALTER TABLE planning_cycles ADD COLUMN archived_at_ms INTEGER;";

const DAY_MS: i64 = 86_400_000;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct HousekeepingReport {
    pub(crate) closed_tasks: Vec<String>,
    pub(crate) archived_tasks: Vec<String>,
    pub(crate) archived_projects: Vec<String>,
    pub(crate) archived_cycles: Vec<i64>,
}

impl HousekeepingReport {
    pub(crate) fn is_empty(&self) -> bool {
        self.closed_tasks.is_empty()
            && self.archived_tasks.is_empty()
            && self.archived_projects.is_empty()
            && self.archived_cycles.is_empty()
    }
}

/// Auto-closes untouched waiting tasks that are the user's or no one's as
/// canceled, and archives old done/canceled tasks, completed projects, and
/// ended cycles. A task counts as touched by an edit, a comment or an
/// agent session's activity. Everything one day moves is one batch: one
/// inbox notice whose counts add up across the day's runs, with an undo.
/// Work the user took back from an earlier batch is left alone.
pub(crate) fn run_housekeeping(
    conn: &mut Connection,
    auto_close_stale_days: Option<u32>,
    auto_archive_days: Option<u32>,
    now_ms: i64,
) -> Result<HousekeepingReport> {
    if auto_close_stale_days.is_none() && auto_archive_days.is_none() {
        return Ok(HousekeepingReport::default());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut report = HousekeepingReport::default();
    let day = items::local_today(now_ms)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| (now_ms / DAY_MS).to_string());
    let batch = format!("housekeeping:{day}");
    let before = batches::last_change(&tx)?;

    if let Some(days) = auto_close_stale_days.filter(|d| *d > 0) {
        let cutoff = now_ms - i64::from(days) * DAY_MS;
        let stale: Vec<(String, WorkStatus)> = {
            let mut stmt = tx.prepare(
                "SELECT t.id, t.status FROM planning_tasks t
                 WHERE t.archived_at_ms IS NULL AND t.trashed_at_ms IS NULL
                   AND t.status IN ('backlog', 'not_started')
                   AND (t.owner_is_self = 1 OR t.owner_person_id IS NULL)
                   AND (t.snoozed_until_ms IS NULL OR t.snoozed_until_ms <= ?2)
                   AND MAX(t.updated_at_ms,
                       COALESCE((SELECT MAX(c.created_at_ms) FROM planning_task_comments c
                           WHERE c.task_id = t.id), 0),
                       COALESCE((SELECT MAX(MAX(s.created_at_ms, COALESCE(s.heartbeat_at_ms, 0)))
                           FROM planning_agent_sessions s WHERE s.task_id = t.id), 0),
                       COALESCE((SELECT MAX(a.at_ms) FROM planning_agent_activities a
                           JOIN planning_agent_sessions s ON s.id = a.session_id
                           WHERE s.task_id = t.id), 0)) <= ?1
                   AND NOT EXISTS (SELECT 1 FROM planning_sona_moves m
                       WHERE m.target_id = t.id AND m.kind = 'closed_stale'
                         AND m.undone_at_ms IS NOT NULL)
                 ORDER BY t.number",
            )?;
            let rows = stmt
                .query_map(params![cutoff, now_ms], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let note = format!("Auto-closed after {days} days untouched");
        let patch = WorkPatch {
            status: Some(WorkStatus::Canceled),
            ..WorkPatch::default()
        };
        for (id, status) in stale {
            items::apply(
                &tx,
                &id,
                &patch,
                WorkActivityKind::Status,
                WorkActor::Sona,
                now_ms,
            )?;
            comments::add(
                &tx,
                WorkCommentSubject::Task,
                &id,
                None,
                comments::Author {
                    actor: WorkActor::Sona,
                    agent: None,
                },
                &note,
                now_ms,
            )?;
            inbox::withdraw_sona_notices(&tx, &id, now_ms)?;
            batches::record(&tx, &batch, MoveKind::ClosedStale, &id, Some(status), now_ms)?;
            report.closed_tasks.push(id);
        }
    }

    if let Some(days) = auto_archive_days.filter(|d| *d > 0) {
        let cutoff = now_ms - i64::from(days) * DAY_MS;
        let task_ids: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT t.id FROM planning_tasks t
                 WHERE t.archived_at_ms IS NULL AND t.trashed_at_ms IS NULL
                   AND t.status IN ('done', 'canceled', 'duplicate')
                   AND COALESCE(t.completed_at_ms, t.updated_at_ms) <= ?1
                   AND NOT EXISTS (SELECT 1 FROM planning_sona_moves m
                       WHERE m.target_id = t.id AND m.kind = 'archived_task'
                         AND m.undone_at_ms IS NOT NULL)
                 ORDER BY t.number",
            )?;
            let rows = stmt
                .query_map([cutoff], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            rows
        };
        for id in task_ids {
            items::set_archived(&tx, &id, true, WorkActor::Sona, now_ms)?;
            batches::record(&tx, &batch, MoveKind::ArchivedTask, &id, None, now_ms)?;
            report.archived_tasks.push(id);
        }

        let project_ids: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT p.id FROM planning_projects p
                 WHERE p.archived_at_ms IS NULL AND p.trashed_at_ms IS NULL
                   AND p.status IN ('completed', 'canceled')
                   AND p.updated_at_ms <= ?1
                   AND NOT EXISTS (SELECT 1 FROM planning_sona_moves m
                       WHERE m.target_id = p.id AND m.kind = 'archived_project'
                         AND m.undone_at_ms IS NOT NULL)
                 ORDER BY p.title",
            )?;
            let rows = stmt
                .query_map([cutoff], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            rows
        };
        for id in project_ids {
            tx.execute(
                "UPDATE planning_projects SET archived_at_ms = ?2, updated_at_ms = ?2 WHERE id = ?1",
                params![id, now_ms],
            )?;
            batches::record(&tx, &batch, MoveKind::ArchivedProject, &id, None, now_ms)?;
            report.archived_projects.push(id);
        }

        if let Ok(today) = items::local_today(now_ms) {
            if let Some(cutoff_day) = today.checked_sub_days(Days::new(u64::from(days))) {
                let cutoff_text = cutoff_day.format("%Y-%m-%d").to_string();
                let cycle_numbers: Vec<i64> = {
                    let mut stmt = tx.prepare(
                        "SELECT c.number FROM planning_cycles c
                         WHERE c.archived_at_ms IS NULL AND c.end_day <= ?1
                           AND NOT EXISTS (SELECT 1 FROM planning_sona_moves m
                               WHERE m.target_id = CAST(c.number AS TEXT)
                                 AND m.kind = 'archived_cycle' AND m.undone_at_ms IS NOT NULL)
                         ORDER BY c.number",
                    )?;
                    let rows = stmt
                        .query_map([cutoff_text], |row| row.get(0))?
                        .collect::<rusqlite::Result<Vec<i64>>>()?;
                    rows
                };
                for number in cycle_numbers {
                    tx.execute(
                        "UPDATE planning_cycles SET archived_at_ms = ?2 WHERE number = ?1",
                        params![number, now_ms],
                    )?;
                    let target = number.to_string();
                    batches::record(&tx, &batch, MoveKind::ArchivedCycle, &target, None, now_ms)?;
                    report.archived_cycles.push(number);
                }
            }
        }
    }

    if !report.is_empty() {
        batches::quiet_since(&tx, before)?;
        batches::post(&tx, None, &batch, now_ms)?;
    }
    tx.commit()?;
    Ok(report)
}

pub(crate) fn run_housekeeping_tick(
    app: &AppHandle,
    history: &HistoryManager,
    now_ms: i64,
) -> Result<HousekeepingReport> {
    let settings = crate::settings::get_settings(app);
    if settings.work_auto_close_stale_days.is_none() && settings.work_auto_archive_days.is_none() {
        return Ok(HousekeepingReport::default());
    }
    history.with_connection(|conn| {
        run_housekeeping(
            conn,
            settings.work_auto_close_stale_days,
            settings.work_auto_archive_days,
            now_ms,
        )
    })
}

fn words(text: &str) -> HashSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The share of all words in either wording that both use, in percent.
pub(super) fn shared_percent(left: &str, right: &str) -> usize {
    let (left, right) = (words(left), words(right));
    let all = left.union(&right).count();
    if all == 0 {
        return 0;
    }
    left.intersection(&right).count() * 100 / all
}

/// The item still tracking a commitment of `row`'s meeting that the notes,
/// as `rows` now hold them, no longer have, when `row` reads as that
/// commitment reworded: said at the same moment, or sharing at least half
/// of their words. None when nothing matches, or two match equally well.
pub(crate) fn reworded(
    history: &Connection,
    rows: &[MeetingLoopRow],
    row: &MeetingLoopRow,
) -> Result<Option<WorkItem>> {
    let meeting_id = row.session_id.uuid().to_string();
    let prefix = format!("{meeting_id}:{}:", MeetingLoopKind::Commitment.as_str());
    let live: HashSet<&str> = rows.iter().map(|row| row.loop_id.as_str()).collect();
    let mut statement = history.prepare_cached(
        "SELECT t.id, t.source_loop_id, t.title, EXISTS(
             SELECT 1 FROM planning_evidence_links e
             WHERE e.target_kind = 'task' AND e.target_id = COALESCE(t.duplicate_of, t.id)
               AND e.source_kind = 'meeting' AND e.source_id = ?2 AND e.at_ms = ?3)
         FROM planning_tasks t
         WHERE substr(t.source_loop_id, 1, length(?1)) = ?1",
    )?;
    let candidates = statement
        .query_map(
            params![prefix, meeting_id, i64::try_from(row.at_ms).ok()],
            |found| {
                Ok((
                    found.get::<_, String>(0)?,
                    found.get::<_, String>(1)?,
                    found.get::<_, String>(2)?,
                    found.get::<_, bool>(3)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut scored: Vec<((bool, usize), String)> = candidates
        .into_iter()
        .filter(|(_, loop_id, _, _)| !live.contains(loop_id.as_str()))
        .map(|(id, _, title, same_moment)| ((same_moment, shared_percent(&title, &row.text)), id))
        .filter(|((same_moment, shared), _)| *same_moment || *shared >= 50)
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0));
    match scored.as_slice() {
        [] => Ok(None),
        [(best, _), (next, _), ..] if best == next => Ok(None),
        [(_, id), ..] => Ok(Some(items::item(history, id)?)),
    }
}

/// Who made a promise, the way two meetings can agree on it: the user, the
/// person picked for it, else the name the notes read. None when the notes
/// name nobody.
fn promiser(row: &MeetingLoopRow) -> Option<String> {
    if row.direction == MeetingLoopDirection::Mine {
        return Some(String::from("me"));
    }
    if let Some(person) = row.owner_person_id {
        return Some(person.uuid().to_string());
    }
    row.owner_text
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_lowercase)
}

/// The oldest unfinished item that `row` promises again: older than
/// `item`, raised in another meeting by the same person, in the same words.
/// Work typed by hand has no promiser to compare, so nothing repeats it.
pub(crate) fn repeat_of(
    history: &Connection,
    meetings: &Connection,
    user_name: Option<&str>,
    row: &MeetingLoopRow,
    item: &WorkItem,
) -> Result<Option<WorkItem>> {
    let Some(promised_by) = promiser(row) else {
        return Ok(None);
    };
    let words = same_words(&row.text);
    let mut statement = history.prepare_cached(
        "SELECT id, title, source_loop_id FROM planning_tasks
         WHERE archived_at_ms IS NULL AND status NOT IN ('done', 'canceled', 'duplicate')
           AND number < ?1 AND instr(source_loop_id, ':') = 37
           AND substr(source_loop_id, 1, 36) != ?2
         ORDER BY number",
    )?;
    let candidates = statement
        .query_map(
            params![item.number, row.session_id.uuid().to_string()],
            |found| {
                Ok((
                    found.get::<_, String>(0)?,
                    found.get::<_, String>(1)?,
                    found.get::<_, String>(2)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut reader = MeetingReader::new(Some(meetings), user_name);
    for (id, title, loop_id) in candidates {
        if same_words(&title) != words {
            continue;
        }
        let Some(source) = reader.source(&loop_id)? else {
            continue;
        };
        if promiser(source.row).as_ref() == Some(&promised_by) {
            return Ok(Some(items::item(history, &id)?));
        }
    }
    Ok(None)
}

/// Marks `item` a duplicate of `original` for Sona, moving its receipts
/// and its commitment along. Returns the item they now all follow.
pub(crate) fn fold(
    history: &Connection,
    meetings: &Connection,
    item: &WorkItem,
    original: &WorkItem,
    now_ms: i64,
) -> Result<WorkItem> {
    let (root, _) =
        items::mark_duplicate(history, &item.id, &original.id, WorkActor::Sona, now_ms)?;
    settle(history, Some(meetings), &root, WorkActor::Sona, now_ms)?;
    items::item(history, &root)
}

/// Folds triage items that repeat earlier work into it, for meetings taken
/// in before repeats were folded as they came. Work the person already
/// accepted stays as they left it. Returns every item changed.
pub(crate) fn fold_repeats(
    history: &Connection,
    meetings: &Connection,
    user_name: Option<&str>,
    now_ms: i64,
) -> Result<Vec<WorkItem>> {
    let mut reader = MeetingReader::new(Some(meetings), user_name);
    let mut changed = Vec::new();
    for item in items::items(history, false)? {
        if item.status != WorkStatus::Triage {
            continue;
        }
        let Some(loop_id) = item.source_loop_id.as_deref() else {
            continue;
        };
        let Some((rows, row)) = reader
            .source(loop_id)?
            .map(|source| (source.meeting.rows.clone(), source.row.clone()))
        else {
            continue;
        };
        let original = match reworded(history, &rows, &row)? {
            Some(original) => Some(original),
            None => repeat_of(history, meetings, user_name, &row, &item)?,
        };
        if let Some(original) = original {
            let root = fold(history, meetings, &item, &original, now_ms)?;
            changed.push(items::item(history, &item.id)?);
            changed.push(root);
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::schema;
    use crate::planning::work::items::NewItem;
    use crate::planning::work::types::WorkOwner;

    fn open_db() -> Connection {
        let conn = Connection::open_in_memory().expect("memory db");
        schema::apply_all(&conn).expect("schema");
        conn
    }

    fn make_task(conn: &Connection, title: &str, status: WorkStatus, at_ms: i64) -> WorkItem {
        items::insert(
            conn,
            &NewItem {
                title,
                details: None,
                status,
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
            at_ms,
        )
        .expect("task")
    }

    #[test]
    fn housekeeping_auto_closes_stale_and_archives_completed_and_posts_inbox_notice() {
        let mut conn = open_db();
        let now = 1_780_000_000_000i64;
        let old = now - 40 * DAY_MS;
        let recent = now - 5 * DAY_MS;

        let stale = make_task(&conn, "Forgotten task", WorkStatus::NotStarted, old);
        let fresh = make_task(&conn, "Active task", WorkStatus::NotStarted, recent);
        let finished = make_task(&conn, "Old done task", WorkStatus::Done, old);

        conn.execute(
            "INSERT INTO planning_projects (id, title, status, revision, created_at_ms, updated_at_ms)
             VALUES ('proj-old', 'Done project', 'completed', 1, ?1, ?1)",
            [old],
        )
        .expect("project");
        conn.execute(
            "INSERT INTO planning_cycles (number, start_day, end_day, created_at_ms)
             VALUES (1, '2020-01-01', '2020-01-15', ?1)",
            [old],
        )
        .expect("cycle");

        let report = run_housekeeping(&mut conn, Some(30), Some(30), now).expect("housekeeping");
        assert_eq!(report.closed_tasks, vec![stale.id.clone()]);
        assert_eq!(report.archived_tasks, vec![finished.id.clone()]);
        assert_eq!(report.archived_projects, vec!["proj-old".to_owned()]);
        assert_eq!(report.archived_cycles, vec![1]);

        let stale_after = items::item(&conn, &stale.id).expect("stale after");
        assert_eq!(stale_after.status, WorkStatus::Canceled);
        let fresh_after = items::item(&conn, &fresh.id).expect("fresh after");
        assert_eq!(fresh_after.status, WorkStatus::NotStarted);
        let finished_after = items::item(&conn, &finished.id).expect("finished after");
        assert!(finished_after.archived_at_ms.is_some());

        let notices = inbox::notifications(&conn, inbox::Shown::default(), now).expect("inbox");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].kind, inbox::NotificationKind::Housekeeping);
        assert!(notices[0]
            .to
            .as_deref()
            .unwrap_or("")
            .contains("closed 1 stale task"));

        // Running again on the same day does nothing and does not duplicate the notice.
        let second = run_housekeeping(&mut conn, Some(30), Some(30), now + 60_000).expect("second");
        assert!(second.is_empty());
        assert_eq!(
            inbox::notifications(&conn, inbox::Shown::default(), now + 60_000)
                .expect("inbox")
                .len(),
            1
        );
    }

    #[test]
    fn a_later_run_the_same_day_adds_its_closes_to_the_day_s_one_notice() {
        let mut conn = open_db();
        let now = 1_780_000_000_000i64;
        let old = now - 40 * DAY_MS;
        make_task(&conn, "Forgotten task", WorkStatus::NotStarted, old);
        let later = make_task(&conn, "Old idea", WorkStatus::Backlog, now - 20 * DAY_MS);
        run_housekeeping(&mut conn, Some(30), None, now).expect("first run");
        let first = inbox::notifications(&conn, inbox::Shown::default(), now).expect("inbox");
        inbox::mark(&conn, &first[0].id, inbox::Mark::Read, now).expect("read");

        let next = now + 60_000;
        conn.execute(
            "UPDATE planning_tasks SET updated_at_ms = ?2 WHERE id = ?1",
            params![later.id, old],
        )
        .expect("the idea goes stale");
        let second = run_housekeeping(&mut conn, Some(30), None, next).expect("second run");
        let notices = inbox::notifications(&conn, inbox::Shown::default(), next).expect("inbox");

        assert_eq!(second.closed_tasks, vec![later.id]);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].to.as_deref(), Some("closed 2 stale tasks"));
        assert!(notices[0].read_at_ms.is_none());
    }

    #[test]
    fn auto_close_leaves_started_work_others_work_and_tasks_with_recent_comments() {
        let mut conn = open_db();
        let now = 1_780_000_000_000i64;
        let old = now - 40 * DAY_MS;
        make_task(&conn, "Half done", WorkStatus::InProgress, old);
        let theirs = make_task(&conn, "Dana's part", WorkStatus::NotStarted, old);
        conn.execute(
            "UPDATE planning_tasks SET owner_person_id = 'dana' WHERE id = ?1",
            [&theirs.id],
        )
        .expect("owned by Dana");
        let discussed = make_task(&conn, "Still argued about", WorkStatus::NotStarted, old);
        comments::add(
            &conn,
            WorkCommentSubject::Task,
            &discussed.id,
            None,
            comments::Author {
                actor: WorkActor::Me,
                agent: None,
            },
            "Picking this up next week",
            now - DAY_MS,
        )
        .expect("a recent comment");
        let stale = make_task(&conn, "Forgotten task", WorkStatus::NotStarted, old);

        let report = run_housekeeping(&mut conn, Some(30), None, now).expect("housekeeping");

        assert_eq!(report.closed_tasks, vec![stale.id]);
    }

    #[test]
    fn undoing_a_day_of_housekeeping_puts_work_back_and_keeps_it_back() {
        let mut conn = open_db();
        let now = 1_780_000_000_000i64;
        let old = now - 40 * DAY_MS;
        let stale = make_task(&conn, "Forgotten task", WorkStatus::Backlog, old);
        let finished = make_task(&conn, "Old done task", WorkStatus::Done, old);
        run_housekeeping(&mut conn, Some(30), Some(30), now).expect("housekeeping");
        let notice = inbox::notifications(&conn, inbox::Shown::default(), now).expect("inbox")[0]
            .id
            .clone();

        let undone = batches::undo(
            &conn,
            None,
            super::super::Change {
                operation_id: "undo",
                actor: WorkActor::Me,
                at_ms: now + 1,
            },
            &notice,
            &mut super::super::Effects::default(),
        )
        .expect("undone");
        let again = run_housekeeping(&mut conn, Some(30), Some(30), now + 60_000).expect("rerun");

        assert_eq!(undone.restored, 2);
        assert_eq!(items::item(&conn, &stale.id).expect("stale").status, WorkStatus::Backlog);
        assert!(items::item(&conn, &finished.id)
            .expect("finished")
            .archived_at_ms
            .is_none());
        assert!(again.is_empty());
    }

    #[test]
    fn what_housekeeping_closes_sets_off_no_automation() {
        use crate::planning::work::loops::{self, LoopConditions, LoopInput, LoopTrigger};
        let mut conn = open_db();
        let now = 1_780_000_000_000i64;
        make_task(&conn, "Forgotten task", WorkStatus::NotStarted, now - 40 * DAY_MS);
        loops::save(
            &conn,
            None,
            &LoopInput {
                name: "Closed watch".to_owned(),
                prompt: "Say why it closed.".to_owned(),
                trigger: LoopTrigger::StatusChanged { to: None },
                conditions: LoopConditions::default(),
                tools: Vec::new(),
                reasoning: None,
            },
            "you",
            now - 1,
        )
        .expect("an automation");

        run_housekeeping(&mut conn, Some(30), None, now).expect("housekeeping");
        let queued = loops::enqueue(&mut conn, now + 1).expect("triggers read");

        assert_eq!(queued, 0);
    }
}
