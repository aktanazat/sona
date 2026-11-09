//! Cycles: runs of one or two weeks that make themselves, hold the work
//! planned in them, and hand what is unfinished to the next when it starts.
//! A cycle is kept as the local day it starts and the one after it ends,
//! so it keeps its days across time zone and clock changes.

use super::items::{self, local_midnight_ms, local_today};
use super::store::{self, Refusal};
use super::types::{
    WorkActor, WorkCycle, WorkCycleDay, WorkCycleProgress, WorkCycleRef, WorkCycleSettings,
    WorkCycleState, WorkCycles, WorkItem, WorkStatus,
};
use super::{blocks, now_ms, recur, run_history, tasks_changed, Done, Effects};
use crate::managers::history::HistoryManager;
use anyhow::Result;
use chrono::{Datelike, Days, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};

const DAY: &str = "%Y-%m-%d";

struct Span {
    number: i64,
    start: NaiveDate,
    end: NaiveDate,
}

fn day_text(day: NaiveDate) -> String {
    day.format(DAY).to_string()
}

fn plus(day: NaiveDate, days: u64) -> Result<NaiveDate> {
    day.checked_add_days(Days::new(days))
        .ok_or_else(|| anyhow::anyhow!("that day is outside the calendar"))
}

fn spans(conn: &Connection) -> Result<Vec<Span>> {
    let mut statement =
        conn.prepare("SELECT number, start_day, end_day FROM planning_cycles ORDER BY number")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    rows.map(|row| {
        let (number, start, end) = row?;
        Ok(Span {
            number,
            start: NaiveDate::parse_from_str(&start, DAY)?,
            end: NaiveDate::parse_from_str(&end, DAY)?,
        })
    })
    .collect()
}

fn under_way(spans: &[Span], today: NaiveDate) -> Option<&Span> {
    spans
        .iter()
        .find(|span| span.start <= today && today < span.end)
}

pub(crate) fn settings(conn: &Connection) -> Result<WorkCycleSettings> {
    Ok(conn.query_row(
        "SELECT weeks, start_weekday FROM planning_cycle_settings WHERE id = 1",
        [],
        |row| {
            Ok(WorkCycleSettings {
                weeks: row.get(0)?,
                start_weekday: row.get(1)?,
            })
        },
    )?)
}

fn length(settings: WorkCycleSettings) -> u64 {
    u64::from(settings.weeks) * 7
}

/// The last day on or before `day` that falls on the weekday cycles start.
fn aligned(day: NaiveDate, weekday: u8) -> Result<NaiveDate> {
    let back = (day.weekday().number_from_monday() + 7 - u32::from(weekday)) % 7;
    day.checked_sub_days(Days::new(u64::from(back)))
        .ok_or_else(|| anyhow::anyhow!("that day is outside the calendar"))
}

fn insert(
    conn: &Connection,
    number: i64,
    start: NaiveDate,
    end: NaiveDate,
    now_ms: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO planning_cycles (number, start_day, end_day, created_at_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![number, day_text(start), day_text(end), now_ms],
    )?;
    Ok(())
}

/// What moving cycles on changed: the items carried into the cycle under
/// way, and whether a cycle was made.
pub(crate) struct Advanced {
    pub(crate) moved: Vec<String>,
    pub(crate) opened: bool,
}

/// Makes the cycle under way and the one after it when they do not exist
/// yet, then moves unfinished work out of every cycle that has ended into
/// the one under way.
pub(crate) fn advance(conn: &Connection, now_ms: i64) -> Result<Advanced> {
    let today = local_today(now_ms)?;
    let settings = settings(conn)?;
    let length = length(settings);
    let mut all = spans(conn)?;
    let mut opened = false;
    let (number, end) = if let Some(span) = under_way(&all, today) {
        (span.number, span.end)
    } else {
        let start = match all.last() {
            Some(last) if last.end <= today => {
                let behind = u64::try_from((today - last.end).num_days())?;
                plus(last.end, behind / length * length)?
            }
            Some(_) => {
                return Ok(Advanced {
                    moved: Vec::new(),
                    opened,
                })
            }
            None => aligned(today, settings.start_weekday)?,
        };
        let number = all.last().map_or(1, |last| last.number + 1);
        let end = plus(start, length)?;
        insert(conn, number, start, end, now_ms)?;
        opened = true;
        all.push(Span { number, start, end });
        (number, end)
    };
    if !all.iter().any(|span| span.start == end) {
        let next = all.iter().map(|span| span.number).max().unwrap_or(number) + 1;
        insert(conn, next, end, plus(end, length)?, now_ms)?;
        opened = true;
    }
    let mut statement = conn.prepare(
        "SELECT t.id, t.cycle FROM planning_tasks t JOIN planning_cycles c ON c.number = t.cycle
         WHERE c.end_day <= ?1 AND t.archived_at_ms IS NULL
           AND t.status IN ('backlog', 'not_started', 'in_progress')
         ORDER BY t.number",
    )?;
    let unfinished = statement
        .query_map([day_text(today)], |row| {
            Ok((row.get::<_, String>(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<Vec<(String, i64)>>>()?;
    for (id, from) in &unfinished {
        items::place_in_cycle(conn, id, Some(*from), Some(number), WorkActor::Sona, now_ms)?;
    }
    Ok(Advanced {
        moved: unfinished.into_iter().map(|(id, _)| id).collect(),
        opened,
    })
}

/// The cycle under way, when one has been made.
pub(crate) fn current(conn: &Connection, now_ms: i64) -> Result<Option<i64>> {
    Ok(under_way(&spans(conn)?, local_today(now_ms)?).map(|span| span.number))
}

/// The cycle open work lands in by itself: the one under way once it is
/// started, else the one not yet over that holds its due day.
pub(crate) fn fitting(
    conn: &Connection,
    status: WorkStatus,
    due_at_ms: Option<i64>,
    now_ms: i64,
) -> Result<Option<i64>> {
    if status == WorkStatus::InProgress {
        return current(conn, now_ms);
    }
    let Some(due) = due_at_ms else {
        return Ok(None);
    };
    let (day, today) = (local_today(due)?, local_today(now_ms)?);
    Ok(spans(conn)?
        .iter()
        .find(|span| span.start <= day && day < span.end && today < span.end)
        .map(|span| span.number))
}

/// A cycle a change may plan work in: one that exists and has not ended.
pub(crate) fn open(conn: &Connection, number: i64, now_ms: i64) -> Result<i64> {
    let today = local_today(now_ms)?;
    match spans(conn)?.iter().find(|span| span.number == number) {
        Some(span) if today < span.end => Ok(number),
        Some(_) => Err(Refusal::Invalid.into()),
        None => Err(Refusal::NotFound.into()),
    }
}

/// What a query's cycle names, read against the cycles as they are now.
#[derive(Clone, Copy)]
pub(crate) enum CycleWant {
    In(i64),
    NoCycle,
    Nothing,
}

impl CycleWant {
    pub(crate) fn holds(self, item: &WorkItem) -> bool {
        match self {
            Self::In(number) => item.cycle == Some(number),
            Self::NoCycle => item.cycle.is_none(),
            Self::Nothing => false,
        }
    }
}

pub(crate) fn want(conn: &Connection, cycle: WorkCycleRef, now_ms: i64) -> Result<CycleWant> {
    let all = spans(conn)?;
    let current = under_way(&all, local_today(now_ms)?);
    let number = match cycle {
        WorkCycleRef::Current => current.map(|span| span.number),
        WorkCycleRef::Next => current.and_then(|current| {
            all.iter()
                .find(|span| span.start == current.end)
                .map(|span| span.number)
        }),
        WorkCycleRef::None => return Ok(CycleWant::NoCycle),
        WorkCycleRef::Number { number } => Some(number),
    };
    Ok(number.map_or(CycleWant::Nothing, CycleWant::In))
}

fn shown(span: &Span, today: NaiveDate, (scope, done): (u32, u32)) -> Result<WorkCycle> {
    let state = if today < span.start {
        WorkCycleState::Upcoming
    } else if today < span.end {
        WorkCycleState::Current
    } else {
        WorkCycleState::Past
    };
    Ok(WorkCycle {
        number: span.number,
        start_day: day_text(span.start),
        end_day: day_text(span.end),
        starts_at_ms: local_midnight_ms(span.start)?,
        ends_at_ms: local_midnight_ms(span.end)?,
        state,
        scope,
        done,
    })
}

/// Every cycle, oldest first, each with its scope and done as they stand.
pub(crate) fn all(conn: &Connection, now_ms: i64) -> Result<Vec<WorkCycle>> {
    let today = local_today(now_ms)?;
    let mut statement = conn.prepare(
        "SELECT cycle, sum(status NOT IN ('canceled', 'duplicate', 'triage')), sum(status = 'done')
         FROM planning_tasks WHERE cycle IS NOT NULL AND archived_at_ms IS NULL GROUP BY cycle",
    )?;
    let counts = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, (row.get(1)?, row.get(2)?)))
        })?
        .collect::<rusqlite::Result<HashMap<i64, (u32, u32)>>>()?;
    spans(conn)?
        .iter()
        .map(|span| {
            shown(
                span,
                today,
                counts.get(&span.number).copied().unwrap_or((0, 0)),
            )
        })
        .collect()
}

pub(crate) fn listing(conn: &Connection, now_ms: i64) -> Result<WorkCycles> {
    Ok(WorkCycles {
        settings: settings(conn)?,
        cycles: all(conn, now_ms)?,
    })
}

/// Saves how cycles run. The cycle under way ends on the next start day
/// after today, and the one after it is made again at the new length; work
/// planned in it stays planned there.
pub(crate) fn save_settings(
    conn: &Connection,
    settings: WorkCycleSettings,
    now_ms: i64,
) -> Result<Vec<String>> {
    if !(1..=2).contains(&settings.weeks) || !(1..=7).contains(&settings.start_weekday) {
        return Err(Refusal::Invalid.into());
    }
    if self::settings(conn)? == settings {
        return Ok(Vec::new());
    }
    conn.execute(
        "UPDATE planning_cycle_settings SET weeks = ?1, start_weekday = ?2 WHERE id = 1",
        params![settings.weeks, settings.start_weekday],
    )?;
    let today = local_today(now_ms)?;
    if let Some(current) = under_way(&spans(conn)?, today) {
        let end = plus(aligned(today, settings.start_weekday)?, 7)?;
        conn.execute(
            "UPDATE planning_cycles SET end_day = ?2 WHERE number = ?1",
            params![current.number, day_text(end)],
        )?;
        conn.execute(
            "DELETE FROM planning_cycles WHERE number > ?1",
            [current.number],
        )?;
        insert(
            conn,
            current.number + 1,
            end,
            plus(end, length(settings))?,
            now_ms,
        )?;
    }
    Ok(advance(conn, now_ms)?.moved)
}

fn status(text: &str) -> Option<WorkStatus> {
    [
        WorkStatus::Triage,
        WorkStatus::Backlog,
        WorkStatus::NotStarted,
        WorkStatus::InProgress,
        WorkStatus::Done,
        WorkStatus::Canceled,
        WorkStatus::Duplicate,
    ]
    .into_iter()
    .find(|status| status.as_str() == text)
}

/// One task's moves in and out of cycles and between statuses, oldest
/// first, to read where it stood at any moment.
struct Story {
    created_at_ms: i64,
    cycle_first: Option<i64>,
    status_first: WorkStatus,
    cycles: Vec<(i64, Option<i64>)>,
    statuses: Vec<(i64, WorkStatus)>,
}

impl Story {
    fn at(&self, at_ms: i64) -> Option<(Option<i64>, WorkStatus)> {
        if self.created_at_ms > at_ms {
            return None;
        }
        let cycle = self
            .cycles
            .iter()
            .take_while(|(at, _)| *at <= at_ms)
            .last()
            .map_or(self.cycle_first, |(_, cycle)| *cycle);
        let status = self
            .statuses
            .iter()
            .take_while(|(at, _)| *at <= at_ms)
            .last()
            .map_or(self.status_first, |(_, status)| *status);
        Some((cycle, status))
    }
}

fn stories(conn: &Connection, number: i64) -> Result<Vec<Story>> {
    let mut members = conn.prepare(
        "SELECT id, created_at_ms, cycle, status FROM planning_tasks
         WHERE cycle = ?1 OR id IN (SELECT task_id FROM planning_task_activity
            WHERE kind = 'cycle' AND (from_value = ?2 OR to_value = ?2))",
    )?;
    let tasks = members
        .query_map(params![number, number.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, WorkStatus>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut events = conn.prepare(
        "SELECT at_ms, kind, from_value, to_value FROM planning_task_activity
         WHERE task_id = ?1 AND kind IN ('created', 'status', 'accepted', 'declined', 'duplicate', 'cycle')
         ORDER BY at_ms, rowid",
    )?;
    tasks
        .into_iter()
        .map(|(id, created_at_ms, cycle_now, status_now)| {
            let mut story = Story {
                created_at_ms,
                cycle_first: cycle_now,
                status_first: status_now,
                cycles: Vec::new(),
                statuses: Vec::new(),
            };
            let rows = events.query_map([&id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?;
            for row in rows {
                let (at_ms, kind, from, to) = row?;
                match kind.as_str() {
                    "cycle" => {
                        if story.cycles.is_empty() {
                            story.cycle_first = from.and_then(|from| from.parse().ok());
                        }
                        story
                            .cycles
                            .push((at_ms, to.and_then(|to| to.parse().ok())));
                    }
                    "duplicate" => story.statuses.push((at_ms, WorkStatus::Duplicate)),
                    _ => {
                        let Some(next) = to.as_deref().and_then(status) else {
                            continue;
                        };
                        if story.statuses.is_empty() {
                            if let Some(before) = from.as_deref().and_then(status) {
                                story.status_first = before;
                            }
                        }
                        story.statuses.push((at_ms, next));
                    }
                }
            }
            Ok(story)
        })
        .collect()
}

/// A cycle's scope and done as each of its days ended, through today, read
/// back from every task's history.
pub(crate) fn progress(conn: &Connection, number: i64, now_ms: i64) -> Result<WorkCycleProgress> {
    let today = local_today(now_ms)?;
    let all = spans(conn)?;
    let span = all
        .iter()
        .find(|span| span.number == number)
        .ok_or(Refusal::NotFound)?;
    let stories = stories(conn, number)?;
    let mut days = Vec::new();
    let mut day = span.start;
    while day < span.end && day <= today {
        let after = plus(day, 1)?;
        let at_ms = local_midnight_ms(after)?.min(now_ms);
        let (mut scope, mut done) = (0, 0);
        for (cycle, status) in stories.iter().filter_map(|story| story.at(at_ms)) {
            if cycle != Some(number)
                || matches!(
                    status,
                    WorkStatus::Canceled | WorkStatus::Duplicate | WorkStatus::Triage
                )
            {
                continue;
            }
            scope += 1;
            if status == WorkStatus::Done {
                done += 1;
            }
        }
        days.push(WorkCycleDay {
            day: day_text(day),
            scope,
            done,
        });
        day = after;
    }
    let carried_over = conn.query_row(
        "SELECT count(DISTINCT task_id) FROM planning_task_activity
         WHERE kind = 'cycle' AND actor = 'sona' AND to_value = ?1 AND from_value IS NOT NULL",
        [number.to_string()],
        |row| row.get(0),
    )?;
    let counts = all_counts(conn, number)?;
    Ok(WorkCycleProgress {
        cycle: shown(span, today, counts)?,
        days,
        carried_over,
    })
}

fn all_counts(conn: &Connection, number: i64) -> Result<(u32, u32)> {
    Ok(conn
        .query_row(
            "SELECT sum(status NOT IN ('canceled', 'duplicate', 'triage')), sum(status = 'done')
             FROM planning_tasks WHERE cycle = ?1 AND archived_at_ms IS NULL",
            [number],
            |row| {
                Ok((
                    row.get::<_, Option<u32>>(0)?.unwrap_or(0),
                    row.get::<_, Option<u32>>(1)?.unwrap_or(0),
                ))
            },
        )
        .optional()?
        .unwrap_or((0, 0)))
}

/// How cycles run, and every cycle with its scope and done.
#[tauri::command(async)]
#[specta::specta]
pub fn work_cycles(app: AppHandle) -> Result<WorkCycles, String> {
    let now = now_ms();
    super::super::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        listing(conn, now)
    })
}

/// One cycle's scope and done day by day.
#[tauri::command(async)]
#[specta::specta]
pub fn work_cycle(app: AppHandle, number: i64) -> Result<WorkCycleProgress, String> {
    let now = now_ms();
    super::super::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        progress(conn, number, now)
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_cycle_settings_save(
    app: AppHandle,
    operation_id: String,
    settings: WorkCycleSettings,
) -> Result<WorkCycles, String> {
    let now = now_ms();
    run_history(&app, |conn| {
        store::mutate(
            conn,
            &operation_id,
            "work_cycle_settings_save",
            &settings,
            now,
            |tx| {
                let moved = save_settings(tx, settings, now)?;
                Ok(Done {
                    value: listing(tx, now)?,
                    effects: Effects {
                        items: moved,
                        ..Effects::default()
                    },
                })
            },
        )
    })
}

const TICK: Duration = Duration::from_secs(60);
/// History opens a moment after launch, and a locked history fails the
/// first ticks; the cycle under way should not wait a minute for that.
/// Each failure in a row doubles the wait, back up to a minute.
const RETRY: Duration = Duration::from_secs(2);

/// Every minute: cycles start and hand on their unfinished work, repeating
/// work whose day passed comes back, and due work with no free hour left
/// is marked. Its own thread, because the calendar read blocks.
pub(crate) fn start(app: AppHandle) {
    std::thread::spawn(move || {
        let mut retry = RETRY;
        loop {
            let wait = match tick(&app, now_ms()) {
                Ok(Advanced { moved, opened }) => {
                    if opened || !moved.is_empty() {
                        tasks_changed(&app, moved);
                    }
                    retry = RETRY;
                    TICK
                }
                Err(error) => {
                    log::warn!("Cycles and repeats could not move on: {error:#}");
                    let wait = retry;
                    retry = (retry * 2).min(TICK);
                    wait
                }
            };
            std::thread::sleep(wait);
        }
    });
}

fn tick(app: &AppHandle, now_ms: i64) -> Result<Advanced> {
    let busy = blocks::WorkCalendar::current().busy(now_ms, blocks::risk_horizon(now_ms)?);
    let history = app.state::<Arc<HistoryManager>>();
    history.with_connection(|conn| {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut changed = advance(&tx, now_ms)?;
        changed.moved.extend(recur::catch_up(&tx, now_ms)?);
        changed
            .moved
            .extend(blocks::assess(&tx, busy.as_deref(), now_ms)?);
        tx.commit()?;
        Ok(changed)
    })
}
