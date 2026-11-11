//! `sona work timeline`: projects on a time axis from their start to their
//! target day, with the milestones and cycles that fall in the days shown.

use super::catalog::{ExternalWorkMilestone, ExternalWorkPersonRef};
use super::{day_text, midnight, project_cycle, refusal, ExternalWorkCycle, Tracker, WorkAnswer};
use crate::planning::work::cycles;
use crate::planning::ProjectStatus;
use crate::query::external::ExternalError;
use crate::query::QUERY_SCHEMA_VERSION;
use chrono::{Datelike, Local, Months, NaiveDate};
use clap::Args;
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TimelineArgs {
    /// First day shown: YYYY-MM-DD, today or tomorrow. The first of this
    /// month when left out.
    #[arg(long, value_parser = super::day)]
    pub from: Option<NaiveDate>,
    /// Last day shown. Three months from --from when left out.
    #[arg(long, value_parser = super::day)]
    pub to: Option<NaiveDate>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkTimeline {
    pub schema_version: u32,
    pub from: String,
    pub to: String,
    pub today: String,
    /// Projects whose days or milestones fall between `from` and `to`,
    /// earliest first.
    pub projects: Vec<ExternalWorkTimelineProject>,
    /// Projects with neither a start nor a target day, which have no place
    /// on the axis.
    pub undated: Vec<ExternalWorkPersonRef>,
    /// Cycles with a day between `from` and `to`, earliest first.
    pub cycles: Vec<ExternalWorkCycle>,
    /// Dependencies between the projects shown, each a line from the
    /// blocker's end to the blocked project's start; broken when the
    /// blocker ends after that start.
    pub dependencies: Vec<super::dependencies::ExternalProjectDependency>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkTimelineProject {
    pub id: String,
    pub name: String,
    pub status: ProjectStatus,
    pub start: Option<String>,
    pub start_at_ms: Option<i64>,
    pub target: Option<String>,
    pub target_at_ms: Option<i64>,
    /// Milestones with a target day between `from` and `to`, earliest first.
    pub milestones: Vec<ExternalWorkMilestone>,
}

impl Tracker<'_> {
    pub(super) async fn timeline(&self, args: &TimelineArgs) -> Result<WorkAnswer, ExternalError> {
        let today = Local::now().date_naive();
        let from = args.from.unwrap_or_else(|| today.with_day(1).unwrap_or(today));
        let to = match args.to {
            Some(to) => to,
            None => from
                .checked_add_months(Months::new(3))
                .and_then(|end| end.pred_opt())
                .ok_or_else(|| ExternalError::invalid("--from is past the calendar."))?,
        };
        if to < from {
            return Err(ExternalError::invalid("--to is before --from."));
        }
        let first_ms = midnight(from)?;
        let end_ms = midnight(
            to.succ_opt()
                .ok_or_else(|| ExternalError::invalid("--to is past the calendar."))?,
        )?;
        let board = self.board().await?;
        let mut projects = self.every_project()?;
        projects.retain(|project| {
            project.trashed_at_ms.is_none() && project.archived_at_ms.is_none()
        });
        projects.sort_by_key(|project| {
            (
                project.start_at_ms.or(project.target_at_ms),
                project.title.to_lowercase(),
            )
        });
        let mut shown = Vec::new();
        let mut undated = Vec::new();
        for project in &projects {
            let mut milestones = self.milestones_of(project)?;
            milestones.retain(|milestone| {
                milestone
                    .due_at_ms
                    .is_some_and(|due| (first_ms..end_ms).contains(&due))
            });
            milestones.sort_by_key(|milestone| milestone.due_at_ms);
            let first = project.start_at_ms.or(project.target_at_ms);
            let last = project.target_at_ms.or(project.start_at_ms);
            let spans = first.zip(last).is_some_and(|(first, last)| first < end_ms && last >= first_ms);
            if !spans && milestones.is_empty() {
                if first.is_none() {
                    undated.push(ExternalWorkPersonRef {
                        id: project.id.clone(),
                        name: project.title.clone(),
                    });
                }
                continue;
            }
            shown.push(ExternalWorkTimelineProject {
                id: project.id.clone(),
                name: project.title.clone(),
                status: project.status,
                start: project.start_at_ms.map(|ms| day_text(Some(ms))),
                start_at_ms: project.start_at_ms,
                target: project.target_at_ms.map(|ms| day_text(Some(ms))),
                target_at_ms: project.target_at_ms,
                milestones: milestones
                    .iter()
                    .map(|milestone| Self::milestone_row(milestone, &board))
                    .collect(),
            });
        }
        let (first_day, last_day) = (
            from.format("%Y-%m-%d").to_string(),
            to.format("%Y-%m-%d").to_string(),
        );
        let cycles = cycles::work_cycles(self.app.clone())
            .map_err(|message| refusal(self.history, message))?
            .cycles
            .iter()
            .filter(|cycle| cycle.start_day <= last_day && cycle.end_day > first_day)
            .map(project_cycle)
            .collect();
        let dependencies = self
            .all_dependencies()?
            .iter()
            .filter(|dependency| {
                let shows = |id: &String| shown.iter().any(|project| &project.id == id);
                shows(&dependency.blocker_id) && shows(&dependency.blocked_id)
            })
            .map(|dependency| super::dependencies::external(dependency, &projects))
            .collect();
        Ok(WorkAnswer::Timeline(ExternalWorkTimeline {
            schema_version: QUERY_SCHEMA_VERSION,
            from: first_day,
            to: last_day,
            today: today.format("%Y-%m-%d").to_string(),
            projects: shown,
            undated,
            cycles,
            dependencies,
        }))
    }
}

pub(super) fn text(timeline: &ExternalWorkTimeline, text: &mut String) {
    let _ = writeln!(
        text,
        "{} to {} · today {}",
        timeline.from, timeline.to, timeline.today
    );
    if timeline.projects.is_empty() {
        text.push_str("No project falls in these days.\n");
    }
    for project in &timeline.projects {
        let _ = writeln!(
            text,
            "{}  · {:?} · {} → {}",
            project.name,
            project.status,
            project.start.as_deref().unwrap_or("no start"),
            project.target.as_deref().unwrap_or("no target")
        );
        for milestone in &project.milestones {
            let _ = writeln!(
                text,
                "  ◆ {} · {}",
                milestone.name,
                milestone.target.as_deref().unwrap_or("no target")
            );
        }
    }
    if !timeline.undated.is_empty() {
        let names: Vec<&str> = timeline
            .undated
            .iter()
            .map(|project| project.name.as_str())
            .collect();
        let _ = writeln!(text, "No days set: {}", names.join(", "));
    }
    for dependency in &timeline.dependencies {
        let _ = writeln!(text, "⤷ {}", super::dependencies::line(dependency));
    }
    for cycle in &timeline.cycles {
        let _ = writeln!(
            text,
            "↻ {} · {} → {}",
            cycle
                .name
                .clone()
                .unwrap_or_else(|| format!("Cycle {}", cycle.number)),
            cycle.first_day,
            cycle.last_day
        );
    }
}
