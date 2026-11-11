//! `sona work insights`: lead time (made to done) and triage time (until
//! accepted or declined) over the last few days, as medians and 90th
//! percentiles, for the tasks a scope keeps. Work from meetings kept on
//! this Mac stays out, as it does from every list.

use super::{day_text, now_ms, refusal, Scope, Tracker, WorkAnswer};
use crate::planning;
use crate::planning::work::insights::{self, WorkInsights, WorkTiming};
use crate::planning::work::types::{WorkItem, WorkQuery};
use crate::query::external::ExternalError;
use crate::query::QUERY_SCHEMA_VERSION;
use clap::Args;
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct InsightsArgs {
    /// How many days back to read, ending now: 1 to 3650.
    #[arg(
        long,
        default_value_t = 30,
        value_parser = clap::value_parser!(u32).range(1..=i64::from(insights::MAX_DAYS))
    )]
    pub days: u32,
    #[command(flatten)]
    pub scope: Scope,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkInsights {
    pub schema_version: u32,
    pub days: u32,
    pub from: String,
    pub to: String,
    pub from_ms: i64,
    pub to_ms: i64,
    /// Made to done, for each task done in the window.
    pub lead_time: ExternalWorkTiming,
    /// In triage until accepted or declined, for each decision in the window.
    pub triage_time: ExternalWorkTiming,
    pub accepted: u32,
    pub declined: u32,
    /// The window in weeks back from `to`, oldest first; the first may be
    /// shorter.
    pub weeks: Vec<ExternalWorkInsightsWeek>,
    /// False while the conversations can't be opened: then all work from
    /// meetings is left out, not only work from meetings kept on this Mac.
    pub conversations_available: bool,
}

/// How many, and the median and 90th percentile by nearest rank in
/// milliseconds, null when there are none.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkTiming {
    pub count: u32,
    pub median_ms: Option<i64>,
    pub p90_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkInsightsWeek {
    pub from_ms: i64,
    pub to_ms: i64,
    pub lead_time: ExternalWorkTiming,
    pub triage_time: ExternalWorkTiming,
}

fn timing(timing: &WorkTiming) -> ExternalWorkTiming {
    ExternalWorkTiming {
        count: timing.count,
        median_ms: timing.median_ms,
        p90_ms: timing.p90_ms,
    }
}

impl Tracker<'_> {
    pub(super) async fn insights(&self, args: &InsightsArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let query = args.scope.narrow(
            &board,
            WorkQuery {
                include_archived: true,
                include_snoozed: true,
                ..WorkQuery::default()
            },
        )?;
        let items: Vec<WorkItem> = self
            .listed(&board.fence, query)
            .await?
            .items
            .into_iter()
            .map(|row| row.item)
            .collect();
        let now = now_ms();
        let read: WorkInsights = planning::history(self.history, |conn| {
            insights::measure(conn, &items, args.days, now)
        })
        .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::Insights(ExternalWorkInsights {
            schema_version: QUERY_SCHEMA_VERSION,
            days: read.days,
            from: day_text(Some(read.from_ms)),
            to: day_text(Some(read.to_ms)),
            from_ms: read.from_ms,
            to_ms: read.to_ms,
            lead_time: timing(&read.lead),
            triage_time: timing(&read.triage),
            accepted: read.accepted,
            declined: read.declined,
            weeks: read
                .weeks
                .iter()
                .map(|week| ExternalWorkInsightsWeek {
                    from_ms: week.start_ms,
                    to_ms: week.end_ms,
                    lead_time: timing(&week.lead),
                    triage_time: timing(&week.triage),
                })
                .collect(),
            conversations_available: board.fence.is_open(),
        }))
    }
}

/// A span as a person reads it: "3d 4h", "5h 12m", "8m".
fn took(ms: i64) -> String {
    let minutes = ms / 60_000;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (days, hours) {
        (0, 0) if minutes == 0 => "under 1m".to_owned(),
        (0, 0) => format!("{minutes}m"),
        (0, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

fn spread(timing: &ExternalWorkTiming, what: &str) -> String {
    match (timing.median_ms, timing.p90_ms) {
        (Some(median), Some(p90)) => format!(
            "median {}, p90 {}, {} {what}",
            took(median),
            took(p90),
            timing.count
        ),
        _ => format!("none {what}"),
    }
}

pub(super) fn text(insights: &ExternalWorkInsights, text: &mut String) {
    let _ = writeln!(
        text,
        "Last {} days, {} to {}",
        insights.days, insights.from, insights.to
    );
    let _ = writeln!(
        text,
        "Lead time, made to done: {}",
        spread(&insights.lead_time, "done")
    );
    let _ = write!(
        text,
        "Triage time, until accepted or declined: {}",
        spread(&insights.triage_time, "decided")
    );
    if insights.triage_time.count > 0 {
        let _ = write!(
            text,
            " ({} accepted, {} declined)",
            insights.accepted, insights.declined
        );
    }
    text.push('\n');
    for week in &insights.weeks {
        if week.lead_time.count == 0 && week.triage_time.count == 0 {
            continue;
        }
        let _ = writeln!(
            text,
            "  week to {}: lead {}; triage {}",
            day_text(Some(week.to_ms)),
            spread(&week.lead_time, "done"),
            spread(&week.triage_time, "decided")
        );
    }
}
