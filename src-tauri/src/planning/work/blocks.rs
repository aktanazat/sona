//! Time to do the work: an hour booked in the "Sona" calendar before a task
//! is due, and the mark on due work whose day has no free hour left.

use super::items::{self, day_start_ms, local_midnight_ms, local_today};
use super::store;
use super::types::{WorkActivityKind, WorkActor, WorkTimeBlock};
use super::{now_ms, run_history, Done, Effects};
use crate::integrations::calendar;
use crate::managers::history::HistoryManager;
use crate::meeting::detection::calendar::{platform_calendar, CalendarAccess};
use crate::meeting::session::MeetingSessionManager;
use anyhow::Result;
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

const HOUR_MS: i64 = 60 * 60 * 1000;
const QUARTER_MS: i64 = 15 * 60 * 1000;
const WEEK_MS: i64 = 7 * 24 * HOUR_MS;
/// Long enough that it marks a day, a trip or time off rather than a
/// meeting, so it blocks nothing.
const ALL_DAY_MS: i64 = 20 * HOUR_MS;
const WORK_HOURS: (u32, u32) = (9, 18);

/// The calendar blocks are read from and booked in: the Mac's, or in debug
/// builds a JSON file named by `SONA_DEBUG_WORK_CALENDAR`, so a scratch run
/// never reads or writes a real calendar.
pub(crate) enum WorkCalendar {
    System,
    File(PathBuf),
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileEvent {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: String,
    start_ms: i64,
    end_ms: i64,
}

impl WorkCalendar {
    pub(crate) fn current() -> Self {
        #[cfg(debug_assertions)]
        if let Some(path) = std::env::var_os("SONA_DEBUG_WORK_CALENDAR") {
            return Self::File(path.into());
        }
        Self::System
    }

    fn file_events(path: &PathBuf) -> Result<Vec<FileEvent>, String> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| "The test calendar file is not a list of events.".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(_) => Err("The test calendar file could not be read.".into()),
        }
    }

    fn write_file(path: &PathBuf, events: &[FileEvent]) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(events)
            .map_err(|_| "The test calendar could not be written.")?;
        std::fs::write(path, bytes).map_err(|_| "The test calendar could not be written.".into())
    }

    /// When the calendar is busy between `from` and `to`, or `None` when it
    /// cannot be read. Blocks on the calendar.
    pub(crate) fn busy(&self, from: i64, to: i64) -> Option<Vec<(i64, i64)>> {
        let spans: Vec<(i64, i64)> = match self {
            Self::System => {
                let calendar = platform_calendar();
                if calendar.access() != CalendarAccess::Authorized {
                    return None;
                }
                calendar
                    .events_between(from, to)
                    .into_iter()
                    .map(|event| (event.summary.start_utc_ms, event.summary.end_utc_ms))
                    .collect()
            }
            Self::File(path) => Self::file_events(path)
                .ok()?
                .into_iter()
                .map(|event| (event.start_ms, event.end_ms))
                .collect(),
        };
        Some(
            spans
                .into_iter()
                .filter(|(start, end)| end - start < ALL_DAY_MS && *start < to && *end > from)
                .collect(),
        )
    }

    async fn book(&self, title: &str, start: i64, end: i64, notes: &str) -> Result<String, String> {
        match self {
            Self::System => calendar::create(title, start, end, notes, "").await,
            Self::File(path) => {
                let mut events = Self::file_events(path)?;
                let id = uuid::Uuid::new_v4().to_string();
                events.push(FileEvent {
                    id: Some(id.clone()),
                    title: title.to_owned(),
                    start_ms: start,
                    end_ms: end,
                });
                Self::write_file(path, &events)?;
                Ok(id)
            }
        }
    }

    async fn remove(&self, id: &str) -> Result<(), String> {
        match self {
            Self::System => calendar::undo(id).await,
            Self::File(path) => {
                let mut events = Self::file_events(path)?;
                events.retain(|event| event.id.as_deref() != Some(id));
                Self::write_file(path, &events)
            }
        }
    }
}

fn at_hour(day: NaiveDate, hour: u32) -> Result<i64> {
    let at = day
        .and_hms_opt(hour, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("that hour is outside the clock"))?;
    Local
        .from_local_datetime(&at)
        .earliest()
        .map(|at| at.timestamp_millis())
        .ok_or_else(|| anyhow::anyhow!("that hour does not exist on this day"))
}

fn on_quarter(at_ms: i64) -> i64 {
    (at_ms + QUARTER_MS - 1).div_euclid(QUARTER_MS) * QUARTER_MS
}

/// The first free hour on a weekday's working hours from `from` until
/// `until`, on the quarter hour.
pub(crate) fn free_hour(busy: &[(i64, i64)], from: i64, until: i64) -> Result<Option<(i64, i64)>> {
    let mut busy = busy.to_vec();
    busy.sort_unstable();
    let mut day = local_today(from)?;
    let last = local_today(until)?;
    while day <= last {
        if day.weekday().number_from_monday() <= 5 {
            let close = at_hour(day, WORK_HOURS.1)?.min(until);
            let mut start = on_quarter(at_hour(day, WORK_HOURS.0)?.max(from));
            for &(taken_from, taken_to) in &busy {
                if taken_to <= start {
                    continue;
                }
                if taken_from >= start + HOUR_MS {
                    break;
                }
                start = on_quarter(taken_to);
            }
            if start + HOUR_MS <= close {
                return Ok(Some((start, start + HOUR_MS)));
            }
        }
        day = day
            .succ_opt()
            .ok_or_else(|| anyhow::anyhow!("that day is outside the calendar"))?;
    }
    Ok(None)
}

/// Where the risk check stops looking: the end of the day after tomorrow.
pub(crate) fn risk_horizon(now_ms: i64) -> Result<i64> {
    day_start_ms(local_today(now_ms)?, 3)
}

/// Marks my open work due on a weekday within two days, with no time booked
/// for it, when its due day has no free hour left. A calendar that cannot be
/// read marks nothing. Returns the items whose mark changed.
pub(crate) fn assess(
    conn: &Connection,
    busy: Option<&[(i64, i64)]>,
    now_ms: i64,
) -> Result<Vec<String>> {
    let today_starts = day_start_ms(local_today(now_ms)?, 0)?;
    let mut at_risk = BTreeSet::new();
    if let Some(busy) = busy {
        let mut statement = conn.prepare(
            "SELECT t.id, t.due_at_ms FROM planning_tasks t
             WHERE t.owner_is_self = 1 AND t.archived_at_ms IS NULL
               AND t.status IN ('backlog', 'not_started', 'in_progress')
               AND t.due_at_ms >= ?1 AND t.due_at_ms < ?2
               AND NOT EXISTS (SELECT 1 FROM planning_time_blocks b
                   WHERE b.task_id = t.id AND b.ends_at_ms > ?1)",
        )?;
        let due = statement
            .query_map(params![today_starts, risk_horizon(now_ms)?], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, due_at_ms) in due {
            let day = local_today(due_at_ms)?;
            if day.weekday().number_from_monday() > 5 {
                continue;
            }
            let from = local_midnight_ms(day)?.max(now_ms);
            if free_hour(busy, from, at_hour(day, WORK_HOURS.1)?)?.is_none() {
                at_risk.insert(id);
            }
        }
    }
    let marked = conn
        .prepare("SELECT task_id FROM planning_task_risks")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    if marked == at_risk {
        return Ok(Vec::new());
    }
    conn.execute("DELETE FROM planning_task_risks", [])?;
    for id in &at_risk {
        conn.execute(
            "INSERT INTO planning_task_risks (task_id, checked_at_ms) VALUES (?1, ?2)",
            params![id, now_ms],
        )?;
    }
    Ok(marked.symmetric_difference(&at_risk).cloned().collect())
}

pub(crate) fn risks(conn: &Connection) -> Result<BTreeSet<String>> {
    conn.prepare("SELECT task_id FROM planning_task_risks")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()
        .map_err(Into::into)
}

pub(crate) fn booked(
    conn: &Connection,
) -> Result<std::collections::HashMap<String, WorkTimeBlock>> {
    conn.prepare("SELECT task_id, starts_at_ms, ends_at_ms FROM planning_time_blocks")?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                WorkTimeBlock {
                    starts_at_ms: row.get(1)?,
                    ends_at_ms: row.get(2)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()
        .map_err(Into::into)
}

fn block_of(conn: &Connection, id: &str) -> Result<Option<(String, WorkTimeBlock)>> {
    Ok(conn
        .query_row(
            "SELECT event_id, starts_at_ms, ends_at_ms FROM planning_time_blocks WHERE task_id = ?1",
            [id],
            |row| {
                Ok((
                    row.get(0)?,
                    WorkTimeBlock {
                        starts_at_ms: row.get(1)?,
                        ends_at_ms: row.get(2)?,
                    },
                ))
            },
        )
        .optional()?)
}

/// Writes a booked hour down, replacing any block the task held before.
fn keep_block(
    conn: &Connection,
    id: &str,
    event_id: &str,
    block: WorkTimeBlock,
    now_ms: i64,
) -> Result<()> {
    let before = block_of(conn, id)?;
    conn.execute(
        "INSERT INTO planning_time_blocks (task_id, event_id, starts_at_ms, ends_at_ms, booked_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(task_id) DO UPDATE SET event_id = excluded.event_id,
            starts_at_ms = excluded.starts_at_ms, ends_at_ms = excluded.ends_at_ms,
            booked_at_ms = excluded.booked_at_ms",
        params![id, event_id, block.starts_at_ms, block.ends_at_ms, now_ms],
    )?;
    conn.execute("DELETE FROM planning_task_risks WHERE task_id = ?1", [id])?;
    items::record(
        conn,
        id,
        now_ms,
        WorkActor::Me,
        WorkActivityKind::Block,
        before
            .map(|(_, block)| block.starts_at_ms.to_string())
            .as_deref(),
        Some(&block.starts_at_ms.to_string()),
    )?;
    Ok(())
}

async fn calendar_actions_allowed(app: &AppHandle) -> Result<(), String> {
    let store = app
        .state::<Arc<MeetingSessionManager>>()
        .store()
        .await
        .map_err(|_| super::super::conversations_closed())?;
    let preferences = tauri::async_runtime::spawn_blocking(move || store.connection_preferences())
        .await
        .map_err(|_| "Could not read connection permissions.")?
        .map_err(|_| "Could not read connection permissions.")?;
    if preferences.calendar_actions_enabled {
        Ok(())
    } else {
        Err("Enable calendar actions in Settings > Connections first.".into())
    }
}

/// Books the next free working hour before the task is due, or in the
/// coming week when it has no due day ahead, in the "Sona" calendar. A task
/// that already has an hour ahead keeps it.
#[tauri::command]
#[specta::specta]
pub async fn work_time_block(
    app: AppHandle,
    operation_id: String,
    id: String,
) -> Result<WorkTimeBlock, String> {
    let now = now_ms();
    let calendar = WorkCalendar::current();
    let history = Arc::clone(&app.state::<Arc<HistoryManager>>());
    let (item, held) = super::super::history(&history, |conn| {
        let id = store::uuid(&id)?;
        Ok((items::item(conn, &id)?, block_of(conn, &id)?))
    })?;
    if let Some((_, block)) = held.filter(|(_, block)| block.ends_at_ms > now) {
        return Ok(block);
    }
    if matches!(calendar, WorkCalendar::System) {
        calendar_actions_allowed(&app).await?;
    }
    let until = item
        .due_at_ms
        .filter(|due| *due >= now + HOUR_MS)
        .unwrap_or(now + WEEK_MS);
    let (calendar, busy) = tauri::async_runtime::spawn_blocking(move || {
        let busy = calendar.busy(now, until);
        (calendar, busy)
    })
    .await
    .map_err(|_| "The calendar request was interrupted.")?;
    let busy = busy.ok_or("Allow Sona to read Calendar in System Settings.")?;
    let (starts_at_ms, ends_at_ms) = free_hour(&busy, now, until)
        .map_err(|_| "No free hour before it is due.")?
        .ok_or("No free hour before it is due.")?;
    let block = WorkTimeBlock {
        starts_at_ms,
        ends_at_ms,
    };
    let event_id = calendar
        .book(
            &item.title,
            starts_at_ms,
            ends_at_ms,
            &format!("Sona task #{}", item.number),
        )
        .await?;
    let recorded = run_history(&app, |conn| {
        store::mutate(
            conn,
            &operation_id,
            "work_time_block",
            &item.id,
            now,
            |tx| {
                keep_block(tx, &item.id, &event_id, block, now)?;
                let mut effects = Effects::default();
                effects.item(&item);
                Ok(Done {
                    value: block,
                    effects,
                })
            },
        )
    });
    if recorded.is_err() {
        let _ = calendar.remove(&event_id).await;
    }
    recorded
}

/// Takes a task's booked hour off the calendar.
#[tauri::command]
#[specta::specta]
pub async fn work_time_block_remove(
    app: AppHandle,
    operation_id: String,
    id: String,
) -> Result<(), String> {
    let now = now_ms();
    let history = Arc::clone(&app.state::<Arc<HistoryManager>>());
    let id = super::super::history(&history, |_| store::uuid(&id))?;
    let held = super::super::history(&history, |conn| block_of(conn, &id))?;
    let Some((event_id, block)) = held else {
        return Ok(());
    };
    WorkCalendar::current().remove(&event_id).await?;
    run_history(&app, |conn| {
        store::mutate(
            conn,
            &operation_id,
            "work_time_block_remove",
            &id,
            now,
            |tx| {
                tx.execute("DELETE FROM planning_time_blocks WHERE task_id = ?1", [&id])?;
                items::record(
                    tx,
                    &id,
                    now,
                    WorkActor::Me,
                    WorkActivityKind::Block,
                    Some(&block.starts_at_ms.to_string()),
                    None,
                )?;
                let mut effects = Effects::default();
                effects.items.push(id.clone());
                Ok(Done { value: (), effects })
            },
        )
    })
}
