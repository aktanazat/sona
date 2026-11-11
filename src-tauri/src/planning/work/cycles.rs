//! Cycles: runs of one to eight weeks, with an optional cooldown between
//! them, that make themselves ahead of time, hold the work planned in them,
//! and hand what is unfinished to the next when it starts. A cycle is kept
//! as the local day it starts and the one after it ends, so it keeps its
//! days across time zone and clock changes; its daily counts are frozen
//! once it ends.

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
    name: Option<String>,
}

fn day_text(day: NaiveDate) -> String {
    day.format(DAY).to_string()
}

fn plus(day: NaiveDate, days: u64) -> Result<NaiveDate> {
    day.checked_add_days(Days::new(days))
        .ok_or_else(|| anyhow::anyhow!("that day is outside the calendar"))
}

fn spans(conn: &Connection) -> Result<Vec<Span>> {
    let mut statement = conn
        .prepare("SELECT number, start_day, end_day, name FROM planning_cycles ORDER BY number")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (number, start, end, name) = row?;
        Ok(Span {
            number,
            start: NaiveDate::parse_from_str(&start, DAY)?,
            end: NaiveDate::parse_from_str(&end, DAY)?,
            name,
        })
    })
    .collect()
}

fn under_way(spans: &[Span], today: NaiveDate) -> Option<&Span> {
    spans
        .iter()
        .find(|span| span.start <= today && today < span.end)
}

/// The first cycle that starts on or after `day`.
fn after(spans: &[Span], day: NaiveDate) -> Option<&Span> {
    spans.iter().find(|span| span.start >= day)
}

pub(crate) fn settings(conn: &Connection) -> Result<WorkCycleSettings> {
    Ok(conn.query_row(
        "SELECT weeks, start_weekday, cooldown_weeks, upcoming FROM planning_cycle_settings WHERE id = 1",
        [],
        |row| {
            Ok(WorkCycleSettings {
                weeks: row.get(0)?,
                start_weekday: row.get(1)?,
                cooldown_weeks: row.get(2)?,
                upcoming: row.get(3)?,
            })
        },
    )?)
}

fn length(settings: WorkCycleSettings) -> u64 {
    u64::from(settings.weeks) * 7
}

fn cooldown(settings: WorkCycleSettings) -> u64 {
    u64::from(settings.cooldown_weeks) * 7
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

/// Makes the cycle under way when one is due and the upcoming ones after
/// it, a cooldown apart, then freezes the counts of every cycle that ended
/// and moves its unfinished work into the one under way.
pub(crate) fn advance(conn: &Connection, now_ms: i64) -> Result<Advanced> {
    let today = local_today(now_ms)?;
    let settings = settings(conn)?;
    let (length, cooldown) = (length(settings), cooldown(settings));
    let mut all = spans(conn)?;
    let mut opened = false;
    if under_way(&all, today).is_none() {
        let start = match all.last() {
            Some(last) if last.end <= today => {
                let first = plus(last.end, cooldown)?;
                if first > today {
                    Some(first)
                } else {
                    let behind = u64::try_from((today - first).num_days())?;
                    let step = length + cooldown;
                    let start = plus(first, behind / step * step)?;
                    Some(if plus(start, length)? <= today {
                        plus(start, step)?
                    } else {
                        start
                    })
                }
            }
            Some(_) => None,
            None => Some(aligned(today, settings.start_weekday)?),
        };
        if let Some(start) = start {
            let number = all.last().map_or(1, |last| last.number + 1);
            let end = plus(start, length)?;
            insert(conn, number, start, end, now_ms)?;
            opened = true;
            all.push(Span {
                number,
                start,
                end,
                name: None,
            });
        }
    }
    while all.iter().filter(|span| span.start > today).count() < usize::from(settings.upcoming) {
        let Some(last) = all.last() else { break };
        let (number, start) = (last.number + 1, plus(last.end, cooldown)?);
        let end = plus(start, length)?;
        insert(conn, number, start, end, now_ms)?;
        opened = true;
        all.push(Span {
            number,
            start,
            end,
            name: None,
        });
    }
    for span in all.iter().filter(|span| span.end <= today) {
        freeze(conn, span, now_ms)?;
    }
    let moved = match under_way(&all, today) {
        Some(current) => roll(conn, current.number, |span| span.end <= today, now_ms)?,
        None => Vec::new(),
    };
    Ok(Advanced { moved, opened })
}

/// Moves the unfinished work of every cycle `ended` picks into `into`.
fn roll(
    conn: &Connection,
    into: i64,
    ended: impl Fn(&Span) -> bool,
    now_ms: i64,
) -> Result<Vec<String>> {
    let from = spans(conn)?
        .into_iter()
        .filter(|span| span.number != into && ended(span))
        .map(|span| span.number)
        .collect::<Vec<_>>();
    let mut statement = conn.prepare(
        "SELECT id FROM planning_tasks
         WHERE cycle = ?1 AND archived_at_ms IS NULL
           AND status IN ('backlog', 'not_started', 'in_progress')
         ORDER BY number",
    )?;
    let mut moved = Vec::new();
    for number in from {
        let ids = statement
            .query_map([number], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in ids {
            items::place_in_cycle(conn, &id, Some(number), Some(into), WorkActor::Sona, now_ms)?;
            moved.push(id);
        }
    }
    Ok(moved)
}

/// Keeps the counts of an ended cycle as they stood each day, once.
fn freeze(conn: &Connection, span: &Span, now_ms: i64) -> Result<()> {
    let frozen: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_cycle_days WHERE number = ?1)",
        [span.number],
        |row| row.get(0),
    )?;
    if frozen {
        return Ok(());
    }
    for day in live_days(conn, span, now_ms)? {
        conn.execute(
            "INSERT INTO planning_cycle_days (number, day, scope, started, done)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![span.number, day.day, day.scope, day.started, day.done],
        )?;
    }
    Ok(())
}

fn frozen_days(conn: &Connection, number: i64) -> Result<Vec<WorkCycleDay>> {
    let mut statement = conn.prepare_cached(
        "SELECT day, scope, started, done FROM planning_cycle_days WHERE number = ?1 ORDER BY day",
    )?;
    let days = statement
        .query_map([number], |row| {
            Ok(WorkCycleDay {
                day: row.get(0)?,
                scope: row.get(1)?,
                started: row.get(2)?,
                done: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(days)
}

/// Moves a cycle's unfinished work on by hand: into the cycle under way
/// when the cycle has ended, else into the next one. Returns what moved.
pub(crate) fn rollover(conn: &Connection, number: i64, now_ms: i64) -> Result<Vec<String>> {
    let today = local_today(now_ms)?;
    let all = spans(conn)?;
    let span = all
        .iter()
        .find(|span| span.number == number)
        .ok_or(Refusal::NotFound)?;
    let into = match under_way(&all, today) {
        Some(current) if span.end <= current.start => current.number,
        _ => after(&all, span.end).ok_or(Refusal::Invalid)?.number,
    };
    roll(conn, into, |candidate| candidate.number == number, now_ms)
}

/// What a cycle is renamed or moved to. An empty name clears it.
#[derive(Clone, Debug, Default)]
pub(crate) struct CycleEdit {
    pub(crate) name: Option<String>,
    pub(crate) start_day: Option<NaiveDate>,
    pub(crate) end_day: Option<NaiveDate>,
}

/// Renames a cycle, or moves the days of one not yet over. Days never move
/// into the past, a cycle under way keeps its start, and no cycle runs
/// into its neighbours.
pub(crate) fn save_cycle(
    conn: &Connection,
    number: i64,
    edit: &CycleEdit,
    now_ms: i64,
) -> Result<Vec<String>> {
    let today = local_today(now_ms)?;
    let all = spans(conn)?;
    let index = all
        .iter()
        .position(|span| span.number == number)
        .ok_or(Refusal::NotFound)?;
    let span = &all[index];
    if let Some(name) = &edit.name {
        let name = name.trim();
        conn.execute(
            "UPDATE planning_cycles SET name = ?2 WHERE number = ?1",
            params![number, (!name.is_empty()).then_some(name)],
        )?;
    }
    if edit.start_day.is_none() && edit.end_day.is_none() {
        return Ok(Vec::new());
    }
    let start = edit.start_day.unwrap_or(span.start);
    let end = edit.end_day.unwrap_or(span.end);
    let moves_start = start != span.start;
    let previous_end = index.checked_sub(1).map(|before| all[before].end);
    let next_start = all.get(index + 1).map(|next| next.start);
    if span.end <= today
        || end <= start
        || end <= today
        || (moves_start && (span.start <= today || start < today))
        || previous_end.is_some_and(|previous| start < previous)
        || next_start.is_some_and(|next| end > next)
    {
        return Err(Refusal::Invalid.into());
    }
    conn.execute(
        "UPDATE planning_cycles SET start_day = ?2, end_day = ?3 WHERE number = ?1",
        params![number, day_text(start), day_text(end)],
    )?;
    Ok(advance(conn, now_ms)?.moved)
}

/// Starts the next cycle at today's midnight. A cycle under way ends now
/// and its unfinished work moves in; with none, the cooldown ends.
pub(crate) fn start_today(conn: &Connection, number: i64, now_ms: i64) -> Result<Vec<String>> {
    let today = local_today(now_ms)?;
    let all = spans(conn)?;
    let next = all
        .iter()
        .find(|span| span.start > today)
        .ok_or(Refusal::NotFound)?;
    if next.number != number {
        return Err(Refusal::Invalid.into());
    }
    if let Some(current) = under_way(&all, today) {
        if current.start == today {
            return Err(Refusal::Invalid.into());
        }
        conn.execute(
            "UPDATE planning_cycles SET end_day = ?2 WHERE number = ?1",
            params![current.number, day_text(today)],
        )?;
    }
    conn.execute(
        "UPDATE planning_cycles SET start_day = ?2 WHERE number = ?1",
        params![number, day_text(today)],
    )?;
    Ok(advance(conn, now_ms)?.moved)
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
        WorkCycleRef::Next => current.and_then(|current| after(&all, current.end).map(|span| span.number)),
        WorkCycleRef::None => return Ok(CycleWant::NoCycle),
        WorkCycleRef::Number { number } => Some(number),
    };
    Ok(number.map_or(CycleWant::Nothing, CycleWant::In))
}

fn success(counts: Counts) -> Option<u8> {
    let (scope, started, done) = counts;
    (scope > 0).then(|| {
        let points = u64::from(done) * 100 + u64::from(started) * 25;
        u8::try_from(points / u64::from(scope)).unwrap_or(100)
    })
}

fn shown(span: &Span, today: NaiveDate, counts: Counts, at_close: Option<Counts>) -> Result<WorkCycle> {
    let state = if today < span.start {
        WorkCycleState::Upcoming
    } else if today < span.end {
        WorkCycleState::Current
    } else {
        WorkCycleState::Past
    };
    let (scope, started, done) = counts;
    Ok(WorkCycle {
        number: span.number,
        name: span.name.clone(),
        start_day: day_text(span.start),
        end_day: day_text(span.end),
        starts_at_ms: local_midnight_ms(span.start)?,
        ends_at_ms: local_midnight_ms(span.end)?,
        state,
        scope,
        started,
        done,
        success: success(at_close.unwrap_or(counts)),
    })
}

/// Scope, started and done.
type Counts = (u32, u32, u32);

/// Each ended cycle's counts on its last frozen day.
fn closing_counts(conn: &Connection) -> Result<HashMap<i64, Counts>> {
    let mut statement = conn.prepare(
        "SELECT d.number, d.scope, d.started, d.done FROM planning_cycle_days d
         WHERE d.day = (SELECT max(day) FROM planning_cycle_days WHERE number = d.number)",
    )?;
    let counts = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(counts)
}

/// Every cycle, oldest first, each with its scope and done as they stand.
pub(crate) fn all(conn: &Connection, now_ms: i64) -> Result<Vec<WorkCycle>> {
    let today = local_today(now_ms)?;
    let mut statement = conn.prepare(
        "SELECT cycle, sum(status NOT IN ('canceled', 'duplicate', 'triage')),
            sum(status = 'in_progress'), sum(status = 'done')
         FROM planning_tasks WHERE cycle IS NOT NULL AND archived_at_ms IS NULL GROUP BY cycle",
    )?;
    let counts = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))
        })?
        .collect::<rusqlite::Result<HashMap<i64, Counts>>>()?;
    let closing = closing_counts(conn)?;
    spans(conn)?
        .iter()
        .map(|span| {
            shown(
                span,
                today,
                counts.get(&span.number).copied().unwrap_or((0, 0, 0)),
                closing.get(&span.number).copied(),
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

/// Saves how cycles run. When the length, start day or cooldown changes,
/// the cycle under way ends on the next start day after today and the
/// cycles after it are made again; work planned in them stays planned under
/// the same numbers. Work planned in a cycle no longer made goes back to no
/// cycle.
pub(crate) fn save_settings(
    conn: &Connection,
    settings: WorkCycleSettings,
    now_ms: i64,
) -> Result<Vec<String>> {
    if !(1..=8).contains(&settings.weeks)
        || !(1..=7).contains(&settings.start_weekday)
        || settings.cooldown_weeks > 4
        || !(1..=15).contains(&settings.upcoming)
    {
        return Err(Refusal::Invalid.into());
    }
    let before = self::settings(conn)?;
    if before == settings {
        return Ok(Vec::new());
    }
    conn.execute(
        "UPDATE planning_cycle_settings
            SET weeks = ?1, start_weekday = ?2, cooldown_weeks = ?3, upcoming = ?4 WHERE id = 1",
        params![
            settings.weeks,
            settings.start_weekday,
            settings.cooldown_weeks,
            settings.upcoming
        ],
    )?;
    let today = local_today(now_ms)?;
    let all = spans(conn)?;
    let reshaped = (before.weeks, before.start_weekday, before.cooldown_weeks)
        != (settings.weeks, settings.start_weekday, settings.cooldown_weeks);
    match under_way(&all, today) {
        Some(current) if reshaped => {
            let end = plus(aligned(today, settings.start_weekday)?, 7)?;
            conn.execute(
                "UPDATE planning_cycles SET end_day = ?2 WHERE number = ?1",
                params![current.number, day_text(end)],
            )?;
            conn.execute(
                "DELETE FROM planning_cycles WHERE number > ?1",
                [current.number],
            )?;
        }
        Some(current) => {
            let keep = current.number + i64::from(settings.upcoming);
            conn.execute("DELETE FROM planning_cycles WHERE number > ?1", [keep])?;
        }
        None if reshaped => {
            conn.execute(
                "DELETE FROM planning_cycles WHERE start_day > ?1",
                [day_text(today)],
            )?;
        }
        None => {
            if let Some(first) = all.iter().find(|span| span.start > today) {
                let keep = first.number + i64::from(settings.upcoming) - 1;
                conn.execute("DELETE FROM planning_cycles WHERE number > ?1", [keep])?;
            }
        }
    }
    let mut moved = advance(conn, now_ms)?.moved;
    moved.extend(unplan_removed(conn, now_ms)?);
    Ok(moved)
}

/// Takes every task planned in a cycle that no longer exists out of it.
fn unplan_removed(conn: &Connection, now_ms: i64) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT id, cycle FROM planning_tasks
         WHERE cycle IS NOT NULL AND cycle NOT IN (SELECT number FROM planning_cycles)
         ORDER BY number",
    )?;
    let planned = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, cycle) in &planned {
        items::place_in_cycle(conn, id, Some(*cycle), None, WorkActor::Sona, now_ms)?;
    }
    Ok(planned.into_iter().map(|(id, _)| id).collect())
}

pub(crate) fn status(text: &str) -> Option<WorkStatus> {
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

/// A cycle's counts as each of its days ended, through today, read back
/// from every task's history.
fn live_days(conn: &Connection, span: &Span, now_ms: i64) -> Result<Vec<WorkCycleDay>> {
    let today = local_today(now_ms)?;
    let stories = stories(conn, span.number)?;
    let mut days = Vec::new();
    let mut day = span.start;
    while day < span.end && day <= today {
        let after = plus(day, 1)?;
        let at_ms = local_midnight_ms(after)?.min(now_ms);
        let (mut scope, mut started, mut done) = (0, 0, 0);
        for (cycle, status) in stories.iter().filter_map(|story| story.at(at_ms)) {
            if cycle != Some(span.number)
                || matches!(
                    status,
                    WorkStatus::Canceled | WorkStatus::Duplicate | WorkStatus::Triage
                )
            {
                continue;
            }
            scope += 1;
            match status {
                WorkStatus::InProgress => started += 1,
                WorkStatus::Done => done += 1,
                _ => {}
            }
        }
        days.push(WorkCycleDay {
            day: day_text(day),
            scope,
            started,
            done,
        });
        day = after;
    }
    Ok(days)
}

/// A cycle's counts day by day: frozen once it ended, else read back from
/// every task's history.
pub(crate) fn progress(conn: &Connection, number: i64, now_ms: i64) -> Result<WorkCycleProgress> {
    let today = local_today(now_ms)?;
    let all = spans(conn)?;
    let span = all
        .iter()
        .find(|span| span.number == number)
        .ok_or(Refusal::NotFound)?;
    let frozen = frozen_days(conn, number)?;
    let at_close = frozen
        .last()
        .map(|day| (day.scope, day.started, day.done));
    let days = if frozen.is_empty() {
        live_days(conn, span, now_ms)?
    } else {
        frozen
    };
    let carried_over = conn.query_row(
        "SELECT count(DISTINCT task_id) FROM planning_task_activity
         WHERE kind = 'cycle' AND actor = 'sona' AND to_value = ?1 AND from_value IS NOT NULL",
        [number.to_string()],
        |row| row.get(0),
    )?;
    let counts = all_counts(conn, number)?;
    Ok(WorkCycleProgress {
        cycle: shown(span, today, counts, at_close)?,
        days,
        carried_over,
    })
}

fn all_counts(conn: &Connection, number: i64) -> Result<Counts> {
    Ok(conn
        .query_row(
            "SELECT sum(status NOT IN ('canceled', 'duplicate', 'triage')),
                sum(status = 'in_progress'), sum(status = 'done')
             FROM planning_tasks WHERE cycle = ?1 AND archived_at_ms IS NULL",
            [number],
            |row| {
                Ok((
                    row.get::<_, Option<u32>>(0)?.unwrap_or(0),
                    row.get::<_, Option<u32>>(1)?.unwrap_or(0),
                    row.get::<_, Option<u32>>(2)?.unwrap_or(0),
                ))
            },
        )
        .optional()?
        .unwrap_or((0, 0, 0)))
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

fn cycle_change(
    app: &AppHandle,
    operation_id: &str,
    command: &str,
    request: &impl serde::Serialize,
    change: impl FnOnce(&Connection, i64) -> Result<Vec<String>>,
) -> Result<WorkCycles, String> {
    let now = now_ms();
    run_history(app, |conn| {
        store::mutate(conn, operation_id, command, request, now, |tx| {
            let moved = change(tx, now)?;
            Ok(Done {
                value: listing(tx, now)?,
                effects: Effects {
                    items: moved,
                    ..Effects::default()
                },
            })
        })
    })
}

pub(crate) fn cycle_save(
    app: &AppHandle,
    operation_id: &str,
    number: i64,
    edit: &CycleEdit,
) -> Result<WorkCycles, String> {
    let request = (
        number,
        &edit.name,
        edit.start_day.map(day_text),
        edit.end_day.map(day_text),
    );
    cycle_change(app, operation_id, "work_cycle_save", &request, |tx, now| {
        save_cycle(tx, number, edit, now)
    })
}

pub(crate) fn cycle_rollover(
    app: &AppHandle,
    operation_id: &str,
    number: i64,
) -> Result<WorkCycles, String> {
    cycle_change(app, operation_id, "work_cycle_rollover", &number, |tx, now| {
        rollover(tx, number, now)
    })
}

pub(crate) fn cycle_start_today(
    app: &AppHandle,
    operation_id: &str,
    number: i64,
) -> Result<WorkCycles, String> {
    cycle_change(
        app,
        operation_id,
        "work_cycle_start_today",
        &number,
        |tx, now| start_today(tx, number, now),
    )
}

fn day_arg(day: Option<String>) -> Result<Option<NaiveDate>, String> {
    day.map(|day| {
        NaiveDate::parse_from_str(day.trim(), DAY)
            .map_err(|_| format!("{day:?} is not a day like 2026-05-04."))
    })
    .transpose()
}

/// Renames a cycle, or moves the days of one not yet over.
#[tauri::command(async)]
#[specta::specta]
pub fn work_cycle_save(
    app: AppHandle,
    operation_id: String,
    number: i64,
    name: Option<String>,
    start_day: Option<String>,
    end_day: Option<String>,
) -> Result<WorkCycles, String> {
    let edit = CycleEdit {
        name,
        start_day: day_arg(start_day)?,
        end_day: day_arg(end_day)?,
    };
    cycle_save(&app, &operation_id, number, &edit)
}

/// Moves a cycle's unfinished work into the cycle under way, or the next.
#[tauri::command(async)]
#[specta::specta]
pub fn work_cycle_rollover(
    app: AppHandle,
    operation_id: String,
    number: i64,
) -> Result<WorkCycles, String> {
    cycle_rollover(&app, &operation_id, number)
}

/// Starts the next cycle today, ending the one under way.
#[tauri::command(async)]
#[specta::specta]
pub fn work_cycle_start_today(
    app: AppHandle,
    operation_id: String,
    number: i64,
) -> Result<WorkCycles, String> {
    cycle_start_today(&app, &operation_id, number)
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
    let (changed, purged) = history.with_connection(|conn| {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut changed = advance(&tx, now_ms)?;
        changed.moved.extend(recur::catch_up(&tx, now_ms)?);
        changed
            .moved
            .extend(blocks::assess(&tx, busy.as_deref(), now_ms)?);
        let mut purged = crate::planning::catalog::purge_bin(&tx, now_ms)?;
        purged.extend(super::bin::purge(&tx, now_ms)?);
        tx.commit()?;
        Ok((changed, purged))
    })?;
    if !purged.is_empty() {
        crate::planning::changed_everywhere(app);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::super::super::schema;
    use super::super::items::{self, NewItem};
    use super::super::types::{
        WorkActivityKind, WorkActor, WorkCycleSettings, WorkOwner, WorkStatus,
    };
    use super::{advance, day_text, insert, local_midnight_ms, plus, save_settings, spans};
    use chrono::{Days, NaiveDate};
    use rusqlite::Connection;

    const HOUR_MS: i64 = 60 * 60 * 1000;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 11).expect("a day")
    }

    fn noon() -> i64 {
        local_midnight_ms(today()).expect("midnight") + 12 * HOUR_MS
    }

    fn tracker() -> Connection {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        conn
    }

    fn every(weeks: u8, cooldown_weeks: u8, upcoming: u8) -> WorkCycleSettings {
        WorkCycleSettings {
            weeks,
            start_weekday: 1,
            cooldown_weeks,
            upcoming,
        }
    }

    #[test]
    fn a_save_without_cooldown_and_upcoming_is_refused() {
        let partial = serde_json::from_str::<WorkCycleSettings>(r#"{"weeks":2,"startWeekday":1}"#);
        assert!(partial.is_err(), "{partial:?}");
    }

    #[test]
    fn work_in_a_cycle_no_longer_made_goes_back_to_no_cycle() {
        let conn = tracker();
        let now = noon();
        save_settings(&conn, every(2, 0, 3), now).expect("three cycles ahead");
        let last = spans(&conn).expect("cycles").last().expect("a cycle").number;
        let task = items::insert(
            &conn,
            &NewItem {
                title: "Ship the site",
                details: None,
                status: WorkStatus::NotStarted,
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
            now,
        )
        .expect("a task")
        .id;
        items::place_in_cycle(&conn, &task, None, Some(last), WorkActor::Me, now)
            .expect("planned in the last cycle");

        let moved = save_settings(&conn, every(2, 0, 1), now + 1).expect("one cycle ahead");

        assert!(spans(&conn)
            .expect("cycles")
            .iter()
            .all(|span| span.number != last));
        assert_eq!(moved, vec![task.clone()]);
        assert_eq!(items::item(&conn, &task).expect("the task").cycle, None);
        let entry = items::activity(&conn, &task)
            .expect("history")
            .into_iter()
            .rfind(|entry| entry.kind == WorkActivityKind::Cycle)
            .expect("a cycle entry");
        assert_eq!(
            (entry.actor, entry.from, entry.to),
            (WorkActor::Sona, Some(last.to_string()), None)
        );
    }

    #[test]
    fn a_cycle_made_inside_a_cooldown_starts_after_it() {
        let conn = tracker();
        conn.execute(
            "UPDATE planning_cycle_settings SET weeks = 1, cooldown_weeks = 1, upcoming = 1",
            [],
        )
        .expect("weekly cycles with a week off");
        let ended = today().checked_sub_days(Days::new(15)).expect("a day");
        let started = ended.checked_sub_days(Days::new(7)).expect("a day");
        insert(&conn, 1, started, ended, noon()).expect("a cycle long over");

        advance(&conn, noon()).expect("cycles moved on");

        let made = spans(&conn).expect("cycles");
        let next = made.iter().find(|span| span.number == 2).expect("cycle 2");
        assert_eq!(
            day_text(next.start),
            day_text(plus(today(), 6).expect("a day"))
        );
        assert!(made.iter().all(|span| span.number == 1 || span.start > today()));
    }
}
