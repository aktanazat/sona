//! What a reader takes from another tracker's export, before any of it is
//! written: tasks, projects with their milestones, initiatives, labels,
//! cycles, documents and views, each under the id the other tracker gave it.
//! Readers fill it; [`super::write`] decides what lands.

use super::super::initiatives::InitiativeStatus;
use super::super::types::WorkStatus;
use crate::planning::types::ProjectStatus;
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use specta::Type;

/// The tracker an import came from.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
    Linear,
    Csv,
    Jira,
    Github,
    Asana,
    Shortcut,
    Trello,
}

impl ImportSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Csv => "csv",
            Self::Jira => "jira",
            Self::Github => "github",
            Self::Asana => "asana",
            Self::Shortcut => "shortcut",
            Self::Trello => "trello",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "linear" => Self::Linear,
            "csv" => Self::Csv,
            "jira" => Self::Jira,
            "github" => Self::Github,
            "asana" => Self::Asana,
            "shortcut" => Self::Shortcut,
            "trello" => Self::Trello,
            _ => return None,
        })
    }

    /// The name the tracker goes by.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::Csv => "CSV",
            Self::Jira => "Jira",
            Self::Github => "GitHub",
            Self::Asana => "Asana",
            Self::Shortcut => "Shortcut",
            Self::Trello => "Trello",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SourceLabel {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) group: Option<String>,
    /// 0 to 360, from the tracker's color when it has one.
    pub(crate) hue: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceInitiative {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    /// None when the file only named it.
    pub(crate) status: Option<InitiativeStatus>,
    pub(crate) target_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceMilestone {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) due_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceProject {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    /// None when the file only named it.
    pub(crate) status: Option<ProjectStatus>,
    pub(crate) priority: u8,
    pub(crate) start_at_ms: Option<i64>,
    pub(crate) target_at_ms: Option<i64>,
    pub(crate) initiative: Option<String>,
    pub(crate) milestones: Vec<SourceMilestone>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceCycle {
    pub(crate) id: String,
    pub(crate) start: NaiveDate,
    pub(crate) end: NaiveDate,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceComment {
    pub(crate) id: String,
    pub(crate) author: Option<String>,
    pub(crate) body: String,
    pub(crate) created_at_ms: Option<i64>,
    pub(crate) parent: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceTask {
    pub(crate) id: String,
    /// What the tracker showed it as, such as `ENG-12`.
    pub(crate) reference: Option<String>,
    /// The number in that reference.
    pub(crate) number: Option<i64>,
    pub(crate) title: String,
    pub(crate) details: Option<String>,
    pub(crate) status: WorkStatus,
    pub(crate) priority: u8,
    pub(crate) estimate: Option<u8>,
    pub(crate) due_at_ms: Option<i64>,
    pub(crate) created_at_ms: Option<i64>,
    pub(crate) updated_at_ms: Option<i64>,
    pub(crate) completed_at_ms: Option<i64>,
    /// When the tracker archived it, which Sona reads as put away.
    pub(crate) archived_at_ms: Option<i64>,
    pub(crate) labels: Vec<String>,
    pub(crate) project: Option<String>,
    pub(crate) milestone: Option<String>,
    pub(crate) cycle: Option<String>,
    pub(crate) parent: Option<String>,
    pub(crate) assignee: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) comments: Vec<SourceComment>,
}

impl SourceTask {
    pub(crate) fn new(id: String, title: String) -> Self {
        Self {
            id,
            reference: None,
            number: None,
            title,
            details: None,
            status: WorkStatus::NotStarted,
            priority: 0,
            estimate: None,
            due_at_ms: None,
            created_at_ms: None,
            updated_at_ms: None,
            completed_at_ms: None,
            archived_at_ms: None,
            labels: Vec::new(),
            project: None,
            milestone: None,
            cycle: None,
            parent: None,
            assignee: None,
            url: None,
            comments: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceDocument {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) project: Option<String>,
    pub(crate) created_at_ms: Option<i64>,
}

/// A saved view and the filters it keeps, already in Sona's terms, or why
/// Sona cannot keep them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceView {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) filter: Result<ViewFilter, String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ViewFilter {
    pub(crate) statuses: Vec<WorkStatus>,
    pub(crate) projects: Vec<String>,
    pub(crate) labels: Vec<String>,
    pub(crate) priorities: Vec<u8>,
}

/// Everything one import read.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Batch {
    pub(crate) labels: Vec<SourceLabel>,
    pub(crate) initiatives: Vec<SourceInitiative>,
    pub(crate) projects: Vec<SourceProject>,
    pub(crate) cycles: Vec<SourceCycle>,
    pub(crate) tasks: Vec<SourceTask>,
    pub(crate) documents: Vec<SourceDocument>,
    pub(crate) views: Vec<SourceView>,
    /// Names the person importing goes by there, when the export says.
    pub(crate) me: Vec<String>,
}

impl Batch {
    /// Folds another file's reading into this one; a record read twice is
    /// kept once, the later reading winning, except that a project or an
    /// initiative another file only named keeps what it was read with.
    pub(crate) fn merge(&mut self, other: Self) {
        fn fold<T>(
            into: &mut Vec<T>,
            from: Vec<T>,
            id: impl Fn(&T) -> &str,
            join: impl Fn(&mut T, T),
        ) {
            for item in from {
                match into.iter().position(|kept| id(kept) == id(&item)) {
                    Some(place) => join(&mut into[place], item),
                    None => into.push(item),
                }
            }
        }
        fold(
            &mut self.labels,
            other.labels,
            |label| &label.id,
            |kept, item| *kept = item,
        );
        fold(
            &mut self.initiatives,
            other.initiatives,
            |initiative| &initiative.id,
            |kept, item| {
                kept.description = item.description.or(kept.description.take());
                kept.status = item.status.or(kept.status);
                kept.target_at_ms = item.target_at_ms.or(kept.target_at_ms);
            },
        );
        fold(
            &mut self.projects,
            other.projects,
            |project| &project.id,
            |kept, item| {
                kept.description = item.description.or(kept.description.take());
                kept.status = item.status.or(kept.status);
                kept.start_at_ms = item.start_at_ms.or(kept.start_at_ms);
                kept.target_at_ms = item.target_at_ms.or(kept.target_at_ms);
                kept.initiative = item.initiative.or(kept.initiative.take());
                if item.priority != 0 {
                    kept.priority = item.priority;
                }
                for milestone in item.milestones {
                    match kept.milestones.iter_mut().find(|m| m.id == milestone.id) {
                        Some(found) => *found = milestone,
                        None => kept.milestones.push(milestone),
                    }
                }
            },
        );
        fold(
            &mut self.cycles,
            other.cycles,
            |cycle| &cycle.id,
            |kept, item| *kept = item,
        );
        fold(
            &mut self.tasks,
            other.tasks,
            |task| &task.id,
            |kept, item| *kept = item,
        );
        fold(
            &mut self.documents,
            other.documents,
            |document| &document.id,
            |kept, item| *kept = item,
        );
        fold(
            &mut self.views,
            other.views,
            |view| &view.id,
            |kept, item| *kept = item,
        );
        for name in other.me {
            if !self.me.contains(&name) {
                self.me.push(name);
            }
        }
    }
}

/// `value` trimmed, or none when blank.
pub(crate) fn present(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// A moment as trackers write one: RFC 3339, a day, a day and a time, or
/// the JavaScript `Date` text Linear's exports carry ("Tue Mar 05 2024
/// 14:22:31 GMT+0000 (Coordinated Universal Time)"). A day or a time with
/// no zone is read as UTC.
pub(crate) fn moment(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(at) = DateTime::parse_from_rfc3339(value) {
        return Some(at.timestamp_millis());
    }
    for format in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%m/%d/%Y %H:%M",
    ] {
        if let Ok(at) = NaiveDateTime::parse_from_str(value, format) {
            return Some(Utc.from_utc_datetime(&at).timestamp_millis());
        }
    }
    if let Some(day) = plain_day(value) {
        return day
            .and_hms_opt(0, 0, 0)
            .map(|at| Utc.from_utc_datetime(&at).timestamp_millis());
    }
    let javascript = value.split(" (").next().unwrap_or(value);
    [
        "%a %b %d %Y %H:%M:%S GMT%z",
        "%d %b %Y %H:%M:%S %z",
        "%a, %d %b %Y %H:%M:%S %z",
    ]
    .iter()
    .find_map(|format| DateTime::parse_from_str(javascript, format).ok())
    .map(|at| at.timestamp_millis())
}

/// A calendar day: `2024-03-05`, `03/05/2024`, or the UTC day of a moment.
pub(crate) fn day(value: &str) -> Option<NaiveDate> {
    if let Some(day) = plain_day(value) {
        return Some(day);
    }
    let at = Utc.timestamp_millis_opt(moment(value)?).single()?;
    Some(at.date_naive())
}

fn plain_day(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    ["%Y-%m-%d", "%m/%d/%Y", "%d %b %Y", "%b %d, %Y"]
        .iter()
        .find_map(|format| NaiveDate::parse_from_str(value, format).ok())
}

/// A due day as the local midnight Sona keeps due days at.
pub(crate) fn due(value: &str) -> Option<i64> {
    super::super::items::local_midnight_ms(day(value)?).ok()
}

/// A status as words trackers use for one. Unknown words are none.
pub(crate) fn status_named(value: &str) -> Option<WorkStatus> {
    let words = value.trim().to_lowercase().replace(['_', '-'], " ");
    Some(match words.as_str() {
        "triage" | "inbox" | "new" | "untriaged" => WorkStatus::Triage,
        "backlog" | "icebox" | "someday" | "later" => WorkStatus::Backlog,
        "todo"
        | "to do"
        | "unstarted"
        | "open"
        | "not started"
        | "ready"
        | "planned"
        | "selected for development"
        | "ready for development" => WorkStatus::NotStarted,
        "in progress" | "started" | "doing" | "in review" | "review" | "in development"
        | "code review" | "qa" | "testing" => WorkStatus::InProgress,
        "done" | "completed" | "complete" | "closed" | "resolved" | "shipped" | "released"
        | "finished" => WorkStatus::Done,
        "canceled" | "cancelled" | "won't do" | "wont do" | "won't fix" | "duplicate"
        | "not planned" | "rejected" | "abandoned" => WorkStatus::Canceled,
        _ => return None,
    })
}

/// A priority as Linear numbers it, from its number or its words.
pub(crate) fn priority_named(value: &str) -> u8 {
    match value.trim().to_lowercase().as_str() {
        "1" | "urgent" | "highest" | "critical" | "blocker" | "p0" => 1,
        "2" | "high" | "p1" => 2,
        "3" | "medium" | "normal" | "p2" => 3,
        "4" | "low" | "lowest" | "minor" | "trivial" | "p3" | "p4" => 4,
        _ => 0,
    }
}

/// Points as Sona keeps them, 0 to 64, rounded; none past that.
pub(crate) fn estimate(value: &str) -> Option<u8> {
    let points: f64 = value.trim().parse().ok()?;
    if !(0.0..=64.0).contains(&points) {
        return None;
    }
    let rounded = points.round();
    (0..=64).find(|step| f64::from(*step) == rounded)
}

/// The hue of a `#rrggbb` color.
pub(crate) fn hue(color: &str) -> Option<f64> {
    let hex = color.trim().strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let channel = |at: usize| {
        u8::from_str_radix(hex.get(at..at + 2)?, 16)
            .ok()
            .map(|value| f64::from(value) / 255.0)
    };
    let (red, green, blue) = (channel(0)?, channel(2)?, channel(4)?);
    let high = red.max(green).max(blue);
    let low = red.min(green).min(blue);
    let spread = high - low;
    if spread == 0.0 {
        return Some(0.0);
    }
    let sixths = if high == red {
        ((green - blue) / spread).rem_euclid(6.0)
    } else if high == green {
        (blue - red) / spread + 2.0
    } else {
        (red - green) / spread + 4.0
    };
    Some((sixths * 60.0).clamp(0.0, 360.0))
}

/// The number in an identifier such as `ENG-12` or `#12`.
pub(crate) fn number_in(identifier: &str) -> Option<i64> {
    let digits = identifier
        .trim()
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()?;
    digits.parse().ok().filter(|number| *number > 0)
}

/// A list a cell holds, split on commas and trimmed.
pub(crate) fn listed(value: &str) -> Vec<String> {
    value.split(',').filter_map(present).collect()
}
