//! How far a project and its milestones have come, week by week, when it
//! would finish at its recent pace, and whether its updates have gone quiet.
//! Done work counts fully and started work a quarter, as Linear counts it.

use super::store;
use super::types::{
    Milestone, MilestoneProgress, PlanningTask, Project, ProjectPrediction, ProjectProgress,
    ProjectStatus, ProjectUpdate, ProjectWeek, TaskStatus,
};
use super::updates;
use super::work::cycles;
use super::work::items::{local_midnight_ms, local_today};
use super::work::types::WorkStatus;
use anyhow::Result;
use chrono::{Datelike, Days};
use rusqlite::Connection;

const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const MAX_WEEKS: usize = 52;

/// One task's statuses over time, oldest first; it counts from the first.
#[derive(Clone, Debug)]
pub(crate) struct Timeline {
    pub(crate) statuses: Vec<(i64, WorkStatus)>,
}

impl Timeline {
    fn at(&self, at_ms: i64) -> Option<WorkStatus> {
        self.statuses
            .iter()
            .take_while(|(at, _)| *at <= at_ms)
            .last()
            .map(|(_, status)| *status)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Counts {
    pub(crate) scope: u32,
    pub(crate) started: u32,
    pub(crate) done: u32,
}

impl Counts {
    fn add(&mut self, status: WorkStatus) {
        if matches!(
            status,
            WorkStatus::Canceled | WorkStatus::Duplicate | WorkStatus::Triage
        ) {
            return;
        }
        self.scope += 1;
        match status {
            WorkStatus::InProgress => self.started += 1,
            WorkStatus::Done => self.done += 1,
            _ => {}
        }
    }

    pub(crate) fn percent(self) -> u8 {
        if self.scope == 0 {
            return 0;
        }
        u8::try_from(u64::from(self.done) * 100 / u64::from(self.scope)).unwrap_or(100)
    }

    /// Work left in quarters of a task: a started task has three left.
    fn quarters_left(self) -> i64 {
        4 * i64::from(self.scope) - 4 * i64::from(self.done) - i64::from(self.started)
    }
}

fn quarters_done(week: &ProjectWeek) -> i64 {
    4 * i64::from(week.done) + i64::from(week.started)
}

fn counts_at(timelines: &[Timeline], at_ms: i64) -> Counts {
    let mut counts = Counts::default();
    for status in timelines.iter().filter_map(|timeline| timeline.at(at_ms)) {
        counts.add(status);
    }
    counts
}

/// The project's counts at the end of each week that starts at one of
/// `week_starts`, the last one through `now_ms`, and the finish its done
/// work per week points to: later weeks weigh more, and the dates either
/// side are 40% sooner and later.
pub(crate) fn history(timelines: &[Timeline], week_starts: &[i64], now_ms: i64) -> ProjectProgress {
    let weeks = week_starts
        .iter()
        .map(|start| {
            let counts = counts_at(timelines, (start + WEEK_MS).min(now_ms));
            ProjectWeek {
                week_start_ms: *start,
                scope: counts.scope,
                started: counts.started,
                done: counts.done,
            }
        })
        .collect::<Vec<_>>();
    let now = counts_at(timelines, now_ms);
    ProjectProgress {
        scope: now.scope,
        started: now.started,
        done: now.done,
        percent: now.percent(),
        prediction: predict(&weeks, now, now_ms),
        weeks,
    }
}

fn predict(weeks: &[ProjectWeek], now: Counts, now_ms: i64) -> Option<ProjectPrediction> {
    let finished = weeks
        .iter()
        .filter(|week| week.week_start_ms + WEEK_MS <= now_ms)
        .collect::<Vec<_>>();
    let remaining = now.quarters_left();
    if finished.is_empty() || remaining <= 0 {
        return None;
    }
    let mut before = 0_i64;
    let (mut weighted, mut weights) = (0_i64, 0_i64);
    for (index, week) in finished.iter().enumerate() {
        let weight = i64::try_from(index + 1).ok()?;
        let done = quarters_done(week);
        weighted += (done - before).max(0) * weight;
        weights += weight;
        before = done;
    }
    if weighted <= 0 {
        return None;
    }
    let after = |percent: i64| {
        let numerator =
            i128::from(remaining) * i128::from(weights) * i128::from(WEEK_MS) * i128::from(percent);
        let denominator = i128::from(weighted) * 100;
        let span = (numerator + denominator / 2) / denominator;
        i64::try_from(span).map(|span| now_ms.saturating_add(span))
    };
    Some(ProjectPrediction {
        expected_at_ms: after(100).ok()?,
        optimistic_at_ms: after(60).ok()?,
        pessimistic_at_ms: after(140).ok()?,
    })
}

/// Each milestone's counts from the project's tasks, and which one is the
/// first, in order, still open.
pub(crate) fn milestones(
    milestones: &[Milestone],
    tasks: &[PlanningTask],
) -> Vec<MilestoneProgress> {
    let mut seen_open = false;
    milestones
        .iter()
        .map(|milestone| {
            let mut counts = Counts::default();
            for task in tasks
                .iter()
                .filter(|task| task.milestone_id.as_deref() == Some(milestone.id.as_str()))
            {
                counts.add(match task.status {
                    TaskStatus::NotStarted => WorkStatus::NotStarted,
                    TaskStatus::InProgress => WorkStatus::InProgress,
                    TaskStatus::Done => WorkStatus::Done,
                    TaskStatus::Canceled => WorkStatus::Canceled,
                });
            }
            let open = !matches!(milestone.status, TaskStatus::Done | TaskStatus::Canceled)
                && (counts.scope == 0 || counts.done < counts.scope);
            let next_open = open && !seen_open;
            seen_open |= open;
            MilestoneProgress {
                milestone_id: milestone.id.clone(),
                scope: counts.scope,
                started: counts.started,
                done: counts.done,
                percent: counts.percent(),
                next_open,
            }
        })
        .collect()
}

pub(crate) fn timelines(conn: &Connection, project_id: &str) -> Result<Vec<Timeline>> {
    let mut tasks = conn.prepare_cached(
        "SELECT id, created_at_ms, status FROM planning_tasks
         WHERE project_id = ?1 AND archived_at_ms IS NULL ORDER BY number",
    )?;
    let rows = tasks
        .query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, WorkStatus>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut events = conn.prepare_cached(
        "SELECT at_ms, kind, from_value, to_value FROM planning_task_activity
         WHERE task_id = ?1 AND kind IN ('status', 'accepted', 'declined', 'duplicate')
         ORDER BY at_ms, rowid",
    )?;
    rows.into_iter()
        .map(|(id, created_at_ms, status_now)| {
            let mut first = status_now;
            let mut moves = Vec::new();
            for row in events.query_map([&id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })? {
                let (at_ms, kind, from, to) = row?;
                let next = if kind == "duplicate" {
                    Some(WorkStatus::Duplicate)
                } else {
                    to.as_deref().and_then(cycles::status)
                };
                let Some(next) = next else { continue };
                if moves.is_empty() {
                    if let Some(before) = from.as_deref().and_then(cycles::status) {
                        first = before;
                    }
                }
                moves.push((at_ms, next));
            }
            let mut statuses = vec![(created_at_ms, first)];
            statuses.extend(moves);
            Ok(Timeline { statuses })
        })
        .collect()
}

/// Local Monday midnights from the week `since_ms` falls in through the one
/// `now_ms` falls in, the last `MAX_WEEKS` of them.
fn week_starts(since_ms: i64, now_ms: i64) -> Result<Vec<i64>> {
    let monday = |at_ms: i64| -> Result<chrono::NaiveDate> {
        let day = local_today(at_ms)?;
        day.checked_sub_days(Days::new(u64::from(day.weekday().num_days_from_monday())))
            .ok_or_else(|| anyhow::anyhow!("that day is outside the calendar"))
    };
    let (mut week, last) = (monday(since_ms.min(now_ms))?, monday(now_ms)?);
    let mut starts = Vec::new();
    while week <= last {
        starts.push(local_midnight_ms(week)?);
        week = week
            .checked_add_days(Days::new(7))
            .ok_or_else(|| anyhow::anyhow!("that day is outside the calendar"))?;
    }
    let skip = starts.len().saturating_sub(MAX_WEEKS);
    Ok(starts.split_off(skip))
}

/// What a project page shows beyond its saved rows: the latest update,
/// whether one is overdue, and progress over time.
pub(crate) struct Outlook {
    pub(crate) latest_update: Option<ProjectUpdate>,
    pub(crate) update_missing: bool,
    pub(crate) progress: ProjectProgress,
}

pub(crate) fn outlook(conn: &Connection, project: &Project, now_ms: i64) -> Result<Outlook> {
    let timelines = timelines(conn, &project.id)?;
    let since = timelines
        .iter()
        .filter_map(|timeline| timeline.statuses.first().map(|(at, _)| *at))
        .min()
        .unwrap_or(project.created_at_ms)
        .min(project.created_at_ms);
    let progress = history(&timelines, &week_starts(since, now_ms)?, now_ms);
    let latest_update = updates::latest(conn, &project.id)?;
    let quiet_ms = i64::from(cycles::settings(conn)?.weeks) * WEEK_MS + 3 * DAY_MS;
    let last_ms = latest_update
        .as_ref()
        .map_or(project.created_at_ms, |update| update.created_at_ms);
    let update_missing = project.status == ProjectStatus::Active
        && project.archived_at_ms.is_none()
        && project.trashed_at_ms.is_none()
        && now_ms - last_ms > quiet_ms;
    Ok(Outlook {
        latest_update,
        update_missing,
        progress,
    })
}

/// The project's progress now, in percent, from its tasks as they stand.
pub(crate) fn percent_now(conn: &Connection, project_id: &str) -> Result<u8> {
    let mut statement = conn.prepare_cached(
        "SELECT status FROM planning_tasks WHERE project_id = ?1 AND archived_at_ms IS NULL",
    )?;
    let mut counts = Counts::default();
    for status in statement.query_map([project_id], |row| row.get::<_, WorkStatus>(0))? {
        counts.add(status?);
    }
    Ok(counts.percent())
}

/// The project's milestones in order with their progress, read fresh.
pub(crate) fn milestone_progress(
    conn: &Connection,
    project_id: &str,
) -> Result<(Vec<Milestone>, Vec<MilestoneProgress>)> {
    let records = store::records(conn, project_id)?;
    let progress = milestones(&records.milestones, &records.tasks);
    Ok((records.milestones, progress))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(statuses: &[(i64, WorkStatus)]) -> Timeline {
        Timeline {
            statuses: statuses.to_vec(),
        }
    }

    #[test]
    fn percent_counts_only_done_work() {
        let counts = Counts {
            scope: 10,
            started: 4,
            done: 5,
        };
        assert_eq!(counts.percent(), 50);
    }

    #[test]
    fn prediction_weighs_recent_weeks_and_spreads_forty_percent() {
        let week = WEEK_MS;
        let timelines = vec![
            task(&[(0, WorkStatus::NotStarted), (week / 2, WorkStatus::Done)]),
            task(&[(0, WorkStatus::NotStarted), (week + 1, WorkStatus::Done)]),
            task(&[(0, WorkStatus::NotStarted), (week + 2, WorkStatus::Done)]),
            task(&[(0, WorkStatus::NotStarted)]),
            task(&[(0, WorkStatus::NotStarted)]),
        ];
        let progress = history(&timelines, &[0, week], 2 * week);
        assert_eq!(
            progress
                .weeks
                .iter()
                .map(|w| (w.scope, w.done))
                .collect::<Vec<_>>(),
            vec![(5, 1), (5, 3)]
        );
        let prediction = progress.prediction.expect("two full weeks predict");
        assert_eq!(prediction.expected_at_ms, 2 * week + week * 6 / 5);
        assert_eq!(prediction.optimistic_at_ms, 2 * week + week * 18 / 25);
        assert_eq!(prediction.pessimistic_at_ms, 2 * week + week * 42 / 25);
    }
}
