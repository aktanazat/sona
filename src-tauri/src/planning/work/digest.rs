//! The weekly digest of the user's projects, posted to the inbox on the day
//! Settings names: for each planned, started or paused project its status,
//! the latest update, the tasks done in the week and what puts it at risk.
//! Work drawn from a meeting kept on this Mac is left out.

use super::inbox::{self, Notification};
use super::items;
use super::types::{WorkActor, WorkItem, WorkStatus};
use super::{run_history, store, Change, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::meeting::store::MeetingStore;
use crate::planning::types::{
    DateResolution, Project, ProjectHealth, ProjectStatus, ProjectUpdateAuthor, TaskStatus,
};
use crate::planning::{catalog, progress, EvidenceSourceKind};
use crate::query::KeptLocal;
use anyhow::Result;
use chrono::{Datelike, Days, Local, Months, NaiveDate, TimeZone, Weekday};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tauri::AppHandle;

pub(crate) const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// The hour of the digest's day it arrives.
const DIGEST_HOUR: u32 = 9;
/// How much of the latest update a digest quotes.
const UPDATE_CHARS: usize = 280;

/// The user's projects over one week.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProjectDigest {
    pub from_ms: i64,
    pub to_ms: i64,
    /// Those at risk first, then those with the most done.
    pub projects: Vec<DigestProject>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DigestProject {
    pub id: String,
    pub title: String,
    pub status: ProjectStatus,
    /// The user's own name for the status, when one is set.
    pub status_name: Option<String>,
    pub percent: u8,
    pub target_at_ms: Option<i64>,
    pub latest_update: Option<DigestUpdate>,
    /// Tasks finished in the week.
    pub done: Vec<DigestTask>,
    pub risks: Vec<DigestRisk>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DigestUpdate {
    pub health: ProjectHealth,
    /// The start of its text, cut at a word.
    pub excerpt: Option<String>,
    pub author: ProjectUpdateAuthor,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DigestTask {
    pub id: String,
    pub number: i64,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DigestRisk {
    /// The latest update says at risk or off track.
    Health { health: ProjectHealth },
    /// The target passed with work left.
    PastTarget { target_at_ms: i64 },
    /// At its recent pace it finishes after the target.
    LateFinish {
        expected_at_ms: i64,
        target_at_ms: i64,
    },
    /// Open work whose due day has ended.
    Overdue { tasks: Vec<DigestTask> },
    /// A milestone whose due day ended before it was done.
    LateMilestone { title: String, due_at_ms: i64 },
    /// A started project that has gone too long without an update.
    NoUpdate { last_at_ms: Option<i64> },
}

/// The tasks a digest leaves out: those drawn from a meeting kept on this
/// Mac, and every one drawn from a meeting while the meetings cannot be
/// read.
pub(crate) fn hidden(conn: &Connection, kept: Option<&KeptLocal>) -> Result<HashSet<String>> {
    super::drawing_on(conn, |kind, id| match kept {
        Some(kept) => crate::query::external::evidence_kept_local(kind, id, kept),
        None => matches!(kind, EvidenceSourceKind::Meeting | EvidenceSourceKind::Loop),
    })
}

/// The digest of [`from_ms`, `to_ms`), leaving out `hidden` tasks.
pub(crate) fn build(
    conn: &Connection,
    hidden: &HashSet<String>,
    from_ms: i64,
    to_ms: i64,
) -> Result<ProjectDigest> {
    let names: HashMap<String, String> = catalog::status_names(conn)?
        .into_iter()
        .map(|name| (name.id, name.name))
        .collect();
    let tasks = items::items(conn, false)?;
    let (today_ms, _) = items::today_bounds(to_ms)?;
    let mut projects = Vec::new();
    for summary in crate::planning::store::projects(conn, false)? {
        let project = summary.project;
        if !matches!(
            project.status,
            ProjectStatus::Planned | ProjectStatus::Active | ProjectStatus::Paused
        ) {
            continue;
        }
        let own: Vec<&WorkItem> = tasks
            .iter()
            .filter(|task| {
                task.project_id.as_deref() == Some(project.id.as_str())
                    && !hidden.contains(&task.id)
            })
            .collect();
        let done = own
            .iter()
            .filter(|task| {
                task.status == WorkStatus::Done
                    && task
                        .completed_at_ms
                        .is_some_and(|at| (from_ms..to_ms).contains(&at))
            })
            .map(|task| digest_task(task))
            .collect();
        let overdue: Vec<DigestTask> = own
            .iter()
            .filter(|task| {
                task.status.is_open() && task.due_at_ms.is_some_and(|due| due < today_ms)
            })
            .map(|task| digest_task(task))
            .collect();
        let outlook = progress::outlook(conn, &project, to_ms)?;
        let mut risks = Vec::new();
        let latest = outlook.latest_update.as_ref();
        if let Some(update) = latest.filter(|update| update.health != ProjectHealth::OnTrack) {
            risks.push(DigestRisk::Health {
                health: update.health,
            });
        }
        if let Some(target_at_ms) = project.target_at_ms {
            let ends_ms = target_end_ms(&project, target_at_ms)?;
            let expected = outlook
                .progress
                .prediction
                .as_ref()
                .map(|prediction| prediction.expected_at_ms);
            if ends_ms <= today_ms && outlook.progress.percent < 100 {
                risks.push(DigestRisk::PastTarget { target_at_ms });
            } else if let Some(expected_at_ms) = expected.filter(|at| *at >= ends_ms) {
                risks.push(DigestRisk::LateFinish {
                    expected_at_ms,
                    target_at_ms,
                });
            }
        }
        if !overdue.is_empty() {
            risks.push(DigestRisk::Overdue { tasks: overdue });
        }
        let (milestones, _) = progress::milestone_progress(conn, &project.id)?;
        for milestone in milestones {
            let finished = matches!(milestone.status, TaskStatus::Done | TaskStatus::Canceled);
            if let Some(due_at_ms) = milestone.due_at_ms.filter(|due| *due < today_ms) {
                if !finished {
                    risks.push(DigestRisk::LateMilestone {
                        title: milestone.title,
                        due_at_ms,
                    });
                }
            }
        }
        if outlook.update_missing {
            risks.push(DigestRisk::NoUpdate {
                last_at_ms: latest.map(|update| update.created_at_ms),
            });
        }
        projects.push(DigestProject {
            status_name: project
                .status_name_id
                .as_ref()
                .and_then(|id| names.get(id).cloned()),
            percent: outlook.progress.percent,
            target_at_ms: project.target_at_ms,
            latest_update: outlook.latest_update.map(|update| DigestUpdate {
                health: update.health,
                excerpt: update.body.as_deref().and_then(excerpt),
                author: update.author,
                created_at_ms: update.created_at_ms,
            }),
            done,
            risks,
            id: project.id,
            title: project.title,
            status: project.status,
        });
    }
    projects.sort_by(|a, b| {
        a.risks
            .is_empty()
            .cmp(&b.risks.is_empty())
            .then(b.done.len().cmp(&a.done.len()))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    Ok(ProjectDigest {
        from_ms,
        to_ms,
        projects,
    })
}

fn digest_task(task: &WorkItem) -> DigestTask {
    DigestTask {
        id: task.id.clone(),
        number: task.number,
        title: task.title.clone(),
    }
}

/// The first line-joined words of `body`, cut at a word near
/// [`UPDATE_CHARS`]; none when it holds no text.
fn excerpt(body: &str) -> Option<String> {
    let text = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= UPDATE_CHARS {
        return Some(text);
    }
    let cut: String = text.chars().take(UPDATE_CHARS).collect();
    let words = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    Some(format!("{}…", words.trim_end_matches([',', ';', ':', '.'])))
}

/// Where the day, month, quarter, half year or year the target names ends.
fn target_end_ms(project: &Project, target_at_ms: i64) -> Result<i64> {
    let day = items::local_today(target_at_ms)?;
    let months = |start: u32, length: u32| -> Result<NaiveDate> {
        NaiveDate::from_ymd_opt(day.year(), start, 1)
            .and_then(|first| first.checked_add_months(Months::new(length)))
            .ok_or_else(|| anyhow::anyhow!("the target is outside the calendar"))
    };
    let month = day.month0();
    let end = match project.target_resolution.unwrap_or(DateResolution::Day) {
        DateResolution::Day => day
            .checked_add_days(Days::new(1))
            .ok_or_else(|| anyhow::anyhow!("the target is outside the calendar"))?,
        DateResolution::Month => months(month + 1, 1)?,
        DateResolution::Quarter => months(month / 3 * 3 + 1, 3)?,
        DateResolution::HalfYear => months(month / 6 * 6 + 1, 6)?,
        DateResolution::Year => months(1, 12)?,
    };
    items::day_start_ms(end, 0)
}

/// Nine in the morning of the latest `day` at or before `now_ms` on this
/// Mac's clock: when that week's digest is due, with its date.
fn due(day: Weekday, now_ms: i64) -> Result<(NaiveDate, i64)> {
    let today = items::local_today(now_ms)?;
    let back = (7 + today.weekday().num_days_from_monday() - day.num_days_from_monday()) % 7;
    let date = today
        .checked_sub_days(Days::new(u64::from(back)))
        .ok_or_else(|| anyhow::anyhow!("that day is outside the calendar"))?;
    let at = morning(date)?;
    if at <= now_ms {
        return Ok((date, at));
    }
    let date = date
        .checked_sub_days(Days::new(7))
        .ok_or_else(|| anyhow::anyhow!("that day is outside the calendar"))?;
    Ok((date, morning(date)?))
}

fn morning(date: NaiveDate) -> Result<i64> {
    let at = date
        .and_hms_opt(DIGEST_HOUR, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("the morning is outside the calendar"))?;
    Local
        .from_local_datetime(&at)
        .earliest()
        .map(|at| at.timestamp_millis())
        .ok_or_else(|| anyhow::anyhow!("the morning does not exist on this clock"))
}

/// Posts this week's digest once its day has come, when Settings asks for
/// one and none has been posted since: once per week whatever the day was
/// before, and late when the Mac was asleep. A week with no project to
/// tell of posts nothing. True when it posted.
pub(crate) fn post_weekly(
    app: &AppHandle,
    history: &HistoryManager,
    meetings: Option<&MeetingStore>,
    now_ms: i64,
) -> Result<bool> {
    let settings = crate::settings::get_settings(app);
    if !settings.work_digest_enabled {
        return Ok(false);
    }
    let (date, due_ms) = due(settings.work_digest_day.weekday(), now_ms)?;
    weekly(history, meetings, date, due_ms, now_ms)
}

fn weekly(
    history: &HistoryManager,
    meetings: Option<&MeetingStore>,
    date: NaiveDate,
    due_ms: i64,
    now_ms: i64,
) -> Result<bool> {
    history.with_connection(|conn| {
        if inbox::digest_since(conn, due_ms)? {
            return Ok(false);
        }
        let kept = meetings.and_then(|store| KeptLocal::read(store).ok());
        let hidden = hidden(conn, kept.as_ref())?;
        let digest = build(conn, &hidden, due_ms - WEEK_MS, due_ms)?;
        if digest.projects.is_empty() {
            return Ok(false);
        }
        let key = format!("digest:{date}");
        Ok(inbox::digest_posted(conn, &digest, Some(&key), now_ms)?.is_some())
    })
}

/// Posts `digest` now, for an agent or the user asking for it. A retry
/// under the same operation id answers the first post.
pub(crate) fn post_as(
    app: &AppHandle,
    operation_id: &str,
    digest: &ProjectDigest,
) -> Result<Option<Notification>, String> {
    let change = Change::now(operation_id, WorkActor::Sona);
    run_history(app, |history| {
        store::mutate(
            history,
            change.operation_id,
            "work_digest_post",
            &(),
            change.at_ms,
            |tx| {
                Ok(Done {
                    value: inbox::digest_posted(tx, digest, None, change.at_ms)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}
