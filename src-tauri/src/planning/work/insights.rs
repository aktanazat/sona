//! Insights: how long work takes from being made to done (lead time), and
//! how long it waits in triage until it is accepted or declined, over the
//! last few days, each as a median and a 90th percentile. Both are read
//! back from the tasks a list query keeps, so a project, a label or an
//! owner narrows them the way it narrows the list.

use super::store::Refusal;
use super::types::{WorkItem, WorkQuery, WorkStatus};
use super::{list, now_ms, with_meetings};
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::State;

const DAY_MS: i64 = 86_400_000;
const WEEK_MS: i64 = 7 * DAY_MS;
/// The longest window insights read back over: ten years.
pub(crate) const MAX_DAYS: u32 = 3650;

/// How long a set of spans took: how many, and the median and 90th
/// percentile by nearest rank, none when there are none.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkTiming {
    pub count: u32,
    pub median_ms: Option<i64>,
    pub p90_ms: Option<i64>,
}

impl WorkTiming {
    fn of(mut took: Vec<i64>) -> Self {
        took.sort_unstable();
        let rank = |percent: usize| {
            took.get((took.len() * percent).div_ceil(100).saturating_sub(1))
                .copied()
        };
        Self {
            count: u32::try_from(took.len()).unwrap_or(u32::MAX),
            median_ms: rank(50),
            p90_ms: rank(90),
        }
    }
}

/// One week of the window, by when each span ended.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkInsightsWeek {
    pub start_ms: i64,
    pub end_ms: i64,
    pub lead: WorkTiming,
    pub triage: WorkTiming,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkInsights {
    pub days: u32,
    pub from_ms: i64,
    pub to_ms: i64,
    /// Made to done, for each task done in the window.
    pub lead: WorkTiming,
    /// In triage until accepted or declined, for each such decision in the
    /// window.
    pub triage: WorkTiming,
    pub accepted: u32,
    pub declined: u32,
    /// The window in weeks back from its end, oldest first; the first may
    /// be shorter.
    pub weeks: Vec<WorkInsightsWeek>,
}

/// How long something took, and when it ended.
struct Span {
    end_ms: i64,
    took_ms: i64,
}

/// Lead and triage time for `items` over the `days` before `now_ms`.
pub(crate) fn measure(
    conn: &Connection,
    items: &[WorkItem],
    days: u32,
    now_ms: i64,
) -> anyhow::Result<WorkInsights> {
    if !(1..=MAX_DAYS).contains(&days) {
        return Err(Refusal::Invalid.into());
    }
    let from_ms = now_ms - i64::from(days) * DAY_MS;
    let within = |at_ms: i64| (from_ms..=now_ms).contains(&at_ms);
    let lead: Vec<Span> = items
        .iter()
        .filter(|item| item.status == WorkStatus::Done)
        .filter_map(|item| {
            let end_ms = item.completed_at_ms.filter(|at_ms| within(*at_ms))?;
            Some(Span {
                end_ms,
                took_ms: (end_ms - item.created_at_ms).max(0),
            })
        })
        .collect();
    let mut triage = Vec::new();
    let (mut accepted, mut declined) = (0, 0);
    let mut history = conn.prepare(
        "SELECT at_ms, kind, from_value, to_value FROM planning_task_activity
         WHERE task_id = ?1 AND kind IN ('created', 'status', 'accepted', 'declined', 'duplicate')
         ORDER BY at_ms, rowid",
    )?;
    for item in items {
        let rows = history.query_map([&item.id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut entered = None;
        for (index, row) in rows.enumerate() {
            let (at_ms, kind, from, to) = row?;
            if index == 0 && kind != "created" && from.as_deref() == Some("triage") {
                entered = Some(item.created_at_ms);
            }
            match kind.as_str() {
                "accepted" | "declined" => {
                    let Some(start_ms) = entered.take() else {
                        continue;
                    };
                    if !within(at_ms) {
                        continue;
                    }
                    triage.push(Span {
                        end_ms: at_ms,
                        took_ms: (at_ms - start_ms).max(0),
                    });
                    if kind == "accepted" {
                        accepted += 1;
                    } else {
                        declined += 1;
                    }
                }
                _ => {
                    entered = (to.as_deref() == Some("triage")).then(|| entered.unwrap_or(at_ms));
                }
            }
        }
    }
    let count = days.div_ceil(7);
    let week_of = |span: &Span| {
        let back = u32::try_from((now_ms - span.end_ms) / WEEK_MS).unwrap_or(u32::MAX);
        count - 1 - back.min(count - 1)
    };
    let weeks = (0..count)
        .map(|week| {
            let end_ms = now_ms - i64::from(count - 1 - week) * WEEK_MS;
            let took = |spans: &[Span]| {
                WorkTiming::of(
                    spans
                        .iter()
                        .filter(|span| week_of(span) == week)
                        .map(|span| span.took_ms)
                        .collect(),
                )
            };
            WorkInsightsWeek {
                start_ms: (end_ms - WEEK_MS).max(from_ms),
                end_ms,
                lead: took(&lead),
                triage: took(&triage),
            }
        })
        .collect();
    Ok(WorkInsights {
        days,
        from_ms,
        to_ms: now_ms,
        lead: WorkTiming::of(lead.iter().map(|span| span.took_ms).collect()),
        triage: WorkTiming::of(triage.iter().map(|span| span.took_ms).collect()),
        accepted,
        declined,
        weeks,
    })
}

/// Lead and triage time over the last `days`, for the tasks `query` keeps.
#[tauri::command]
#[specta::specta]
pub async fn work_insights(
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    query: WorkQuery,
    days: u32,
) -> Result<WorkInsights, String> {
    let user_name = meetings.user_name();
    let now = now_ms();
    with_meetings(&history_state, &meetings, |history, meeting_conn| {
        let items: Vec<WorkItem> = list(history, meeting_conn, user_name.as_deref(), &query, now)?
            .items
            .into_iter()
            .map(|row| row.item)
            .collect();
        measure(history, &items, days, now)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::super::super::schema;
    use super::super::items::{self, NewItem};
    use super::super::types::{WorkActivityKind, WorkActor, WorkOwner, WorkPatch, WorkStatus};
    use super::{measure, WorkTiming, DAY_MS};
    use rusqlite::Connection;

    const NOW: i64 = 1_800_000_000_000;
    const HOUR_MS: i64 = 3_600_000;

    /// Lead time runs from made to done; triage time from each entry into
    /// triage to the accept or decline that ends it, counted only when that
    /// falls in the window and never for a plain move out of triage.
    #[test]
    fn insights_time_work_from_made_to_done_and_each_stay_in_triage_to_its_decision() {
        // PANIC: a fixture that cannot open its database is a broken test.
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        // PANIC: as above, for the planner schema.
        schema::apply_all(&conn).expect("the planner schema");
        let task = |title: &str, status: WorkStatus, days_ago: i64| {
            // PANIC: a fixture that cannot add its task is a broken test.
            items::insert(
                &conn,
                &NewItem {
                    title,
                    details: None,
                    status,
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
                NOW - days_ago * DAY_MS,
            )
            .expect("a task")
            .id
        };
        let change = |id: &str, to: WorkStatus, kind: WorkActivityKind, at_ms: i64| {
            let patch = WorkPatch {
                status: Some(to),
                ..WorkPatch::default()
            };
            // PANIC: each change is one the tracker takes.
            items::apply(&conn, id, &patch, kind, WorkActor::Me, at_ms).expect("a change");
        };
        let accepted = task("Accepted, then done", WorkStatus::Triage, 10);
        change(&accepted, WorkStatus::NotStarted, WorkActivityKind::Accepted, NOW - 9 * DAY_MS);
        change(&accepted, WorkStatus::Done, WorkActivityKind::Status, NOW - 6 * DAY_MS);
        let declined = task("Declined", WorkStatus::Triage, 5);
        change(&declined, WorkStatus::Canceled, WorkActivityKind::Declined, NOW - 5 * DAY_MS + 2 * HOUR_MS);
        let twice = task("In triage twice", WorkStatus::Triage, 50);
        change(&twice, WorkStatus::NotStarted, WorkActivityKind::Accepted, NOW - 45 * DAY_MS);
        change(&twice, WorkStatus::Triage, WorkActivityKind::Status, NOW - 4 * DAY_MS);
        change(&twice, WorkStatus::NotStarted, WorkActivityKind::Accepted, NOW - 3 * DAY_MS);
        let moved = task("Moved out of triage", WorkStatus::Triage, 8);
        change(&moved, WorkStatus::Backlog, WorkActivityKind::Status, NOW - 7 * DAY_MS);
        change(&moved, WorkStatus::Done, WorkActivityKind::Status, NOW - DAY_MS);
        let typed = task("Typed in", WorkStatus::NotStarted, 20);
        change(&typed, WorkStatus::Done, WorkActivityKind::Status, NOW - 2 * DAY_MS);
        // PANIC: the fixture's tasks read back.
        let all = items::items(&conn, true).expect("the tasks");

        // PANIC: thirty days is a window insights read.
        let read = measure(&conn, &all, 30, NOW).expect("insights");

        assert_eq!(
            (read.lead, read.triage, read.accepted, read.declined),
            (
                WorkTiming {
                    count: 3,
                    median_ms: Some(7 * DAY_MS),
                    p90_ms: Some(18 * DAY_MS),
                },
                WorkTiming {
                    count: 3,
                    median_ms: Some(DAY_MS),
                    p90_ms: Some(DAY_MS),
                },
                2,
                1,
            )
        );
    }
}
