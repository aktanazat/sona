//! Repeating work: a task said to come back "every week" gets its next
//! instance once, when it is done or dropped, or when its due day passes
//! with it still open.

use super::items::{self, day_start_ms, local_today, NewItem};
use super::types::{WorkActor, WorkRepeat, WorkStatus};
use anyhow::Result;
use chrono::{Datelike, Days, Local, Months, NaiveDate, NaiveTime, TimeZone, Timelike, Weekday};
use rusqlite::{params, Connection};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
enum Rule {
    Days(u64),
    Weeks(u64),
    Months(u32),
    Weekdays,
    On { days: Vec<Weekday>, weeks: u64 },
}

const PARTS_OF_DAY: [&str; 4] = ["morning", "afternoon", "evening", "night"];

fn weekday(word: &str) -> Option<Weekday> {
    let word = word.strip_suffix('s').unwrap_or(word);
    [
        ("monday", Weekday::Mon),
        ("tuesday", Weekday::Tue),
        ("wednesday", Weekday::Wed),
        ("thursday", Weekday::Thu),
        ("friday", Weekday::Fri),
        ("saturday", Weekday::Sat),
        ("sunday", Weekday::Sun),
    ]
    .into_iter()
    .find(|(name, _)| *name == word)
    .map(|(_, day)| day)
}

fn unit(word: &str, count: u32) -> Option<Rule> {
    let word = word.strip_suffix('s').unwrap_or(word);
    Some(match word {
        "day" => Rule::Days(u64::from(count)),
        "week" => Rule::Weeks(u64::from(count)),
        "month" => Rule::Months(count),
        "quarter" => Rule::Months(count.checked_mul(3)?),
        "year" => Rule::Months(count.checked_mul(12)?),
        _ => return None,
    })
}

/// The rule in how a repeat was said, as the capture parser writes it:
/// "every week", "every 2 days", "every other friday", "every monday and
/// thursday morning". Anything else repeats nothing by itself.
fn rule(every: &str) -> Option<Rule> {
    let lower = every.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty() && *word != "and" && !PARTS_OF_DAY.contains(word))
        .collect();
    let (&first, rest) = words.split_first()?;
    if first != "every" {
        return None;
    }
    if rest.is_empty() && PARTS_OF_DAY.iter().any(|part| lower.contains(part)) {
        return Some(Rule::Days(1));
    }
    let (count, rest) = match rest.split_first()? {
        (&"other", after) => (2, after),
        (word, after) => match word.parse::<u32>() {
            Ok(count) if count > 0 => (count, after),
            Ok(_) => return None,
            Err(_) => (1, rest),
        },
    };
    match rest {
        [] => None,
        ["weekday"] if count == 1 => Some(Rule::Weekdays),
        [word] if unit(word, count).is_some() => unit(word, count),
        days => {
            let days = days
                .iter()
                .map(|word| weekday(word))
                .collect::<Option<Vec<_>>>()?;
            Some(Rule::On {
                days,
                weeks: u64::from(count),
            })
        }
    }
}

fn plus(day: NaiveDate, days: u64) -> Option<NaiveDate> {
    day.checked_add_days(Days::new(days))
}

/// The next day the rule falls on after `day`.
fn after(rule: &Rule, day: NaiveDate) -> Option<NaiveDate> {
    match rule {
        Rule::Days(count) => plus(day, *count),
        Rule::Weeks(count) => plus(day, count * 7),
        Rule::Months(count) => day.checked_add_months(Months::new(*count)),
        Rule::Weekdays => (1..=3)
            .filter_map(|step| plus(day, step))
            .find(|day| day.weekday().number_from_monday() <= 5),
        Rule::On { days, weeks } => {
            let next = (1..=7)
                .filter_map(|step| plus(day, step))
                .find(|day| days.contains(&day.weekday()))?;
            plus(next, (weeks - 1) * 7)
        }
    }
}

const DEFAULT_TIME: (u32, u32) = (17, 0);

/// When the instance after one due `due_at_ms` is due: the rule's next day
/// after it, at the same local time, moved on until it is not before
/// today. Work with no due day counts from `now_ms`.
fn next_due(every: &str, due_at_ms: Option<i64>, now_ms: i64) -> Result<Option<i64>> {
    let Some(rule) = rule(every) else {
        return Ok(None);
    };
    let today = local_today(now_ms)?;
    let (from, time) = match due_at_ms.and_then(|due| Local.timestamp_millis_opt(due).single()) {
        Some(due) => (due.date_naive(), due.time()),
        None => (
            today,
            NaiveTime::from_hms_opt(DEFAULT_TIME.0, DEFAULT_TIME.1, 0)
                .ok_or_else(|| anyhow::anyhow!("five in the evening is outside the clock"))?,
        ),
    };
    let mut day = from;
    loop {
        day =
            after(&rule, day).ok_or_else(|| anyhow::anyhow!("that day is outside the calendar"))?;
        if day >= today {
            break;
        }
    }
    let at = day
        .and_hms_opt(time.hour(), time.minute(), 0)
        .ok_or_else(|| anyhow::anyhow!("that time is outside the clock"))?;
    Ok(Local
        .from_local_datetime(&at)
        .earliest()
        .map(|at| at.timestamp_millis()))
}

/// Makes the next instance of a repeating item, once: the link to it is
/// written with it, and an item that already has one makes no other.
pub(crate) fn follow(
    conn: &Connection,
    id: &str,
    actor: WorkActor,
    now_ms: i64,
) -> Result<Option<String>> {
    let (every, series, next): (Option<String>, Option<String>, Option<String>) = conn.query_row(
        "SELECT recurrence, series_id, next_id FROM planning_tasks WHERE id = ?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let (Some(every), None) = (every, next) else {
        return Ok(None);
    };
    let item = items::item(conn, id)?;
    let Some(due_at_ms) = next_due(&every, item.due_at_ms, now_ms)? else {
        return Ok(None);
    };
    let owner = item.owner();
    let spawned = items::insert(
        conn,
        &NewItem {
            title: &item.title,
            details: item.details.as_deref(),
            status: WorkStatus::NotStarted,
            priority: item.priority,
            project_id: item.project_id.as_deref(),
            milestone_id: item.milestone_id.as_deref(),
            owner: &owner,
            due_at_ms: Some(due_at_ms),
            label_ids: &item.label_ids,
            source_loop_id: None,
            recurrence: Some(&every),
        },
        actor,
        now_ms,
    )?;
    let series = series.unwrap_or_else(|| id.to_owned());
    conn.execute(
        "UPDATE planning_tasks SET series_id = ?1 WHERE id IN (?2, ?3)",
        params![series, id, spawned.id],
    )?;
    conn.execute(
        "UPDATE planning_tasks SET next_id = ?2 WHERE id = ?1",
        params![id, spawned.id],
    )?;
    Ok(Some(spawned.id))
}

/// Brings back repeating work left open past its due day. Done and dropped
/// instances bring theirs back as they close.
pub(crate) fn catch_up(conn: &Connection, now_ms: i64) -> Result<Vec<String>> {
    let today_starts = day_start_ms(local_today(now_ms)?, 0)?;
    let mut statement = conn.prepare(
        "SELECT id FROM planning_tasks
         WHERE recurrence IS NOT NULL AND next_id IS NULL AND archived_at_ms IS NULL
           AND status IN ('backlog', 'not_started', 'in_progress') AND due_at_ms < ?1
         ORDER BY number",
    )?;
    let late = statement
        .query_map([today_starts], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut ids = Vec::new();
    for id in late {
        if let Some(next) = follow(conn, &id, WorkActor::Sona, now_ms)? {
            ids.push(id);
            ids.push(next);
        }
    }
    Ok(ids)
}

/// The instance an item handed its repeat on to, when it has.
pub(crate) fn next_of(conn: &Connection, id: &str) -> Result<Option<String>> {
    Ok(conn.query_row(
        "SELECT next_id FROM planning_tasks WHERE id = ?1",
        [id],
        |row| row.get(0),
    )?)
}

/// For every repeating item, its rule and how its series has gone: of the
/// instances closed or past their due day, how many were done by the end
/// of it.
pub(crate) fn repeats(conn: &Connection, now_ms: i64) -> Result<HashMap<String, WorkRepeat>> {
    let mut statement = conn.prepare(
        "SELECT id, recurrence, coalesce(series_id, id), status, due_at_ms, status_changed_at_ms
         FROM planning_tasks
         WHERE recurrence IS NOT NULL AND archived_at_ms IS NULL AND status != 'duplicate'",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, WorkStatus>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut series: HashMap<&str, (u32, u32)> = HashMap::new();
    for (_, _, key, status, due, changed) in &rows {
        let day_ends = due
            .map(|due| local_today(due).and_then(|day| day_start_ms(day, 1)))
            .transpose()?;
        let counted = match status {
            WorkStatus::Done | WorkStatus::Canceled => true,
            _ => day_ends.is_some_and(|ends| ends <= now_ms),
        };
        if !counted {
            continue;
        }
        let tally = series.entry(key).or_default();
        tally.1 += 1;
        if *status == WorkStatus::Done && day_ends.is_none_or(|ends| *changed < ends) {
            tally.0 += 1;
        }
    }
    Ok(rows
        .iter()
        .map(|(id, every, key, ..)| {
            let (kept, of) = series.get(key.as_str()).copied().unwrap_or((0, 0));
            (
                id.clone(),
                WorkRepeat {
                    every: every.clone(),
                    kept,
                    of,
                },
            )
        })
        .collect())
}
