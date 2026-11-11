//! `sona work digest`: the weekly project digest over the last seven days,
//! posted to the inbox now, or with --dry-run only shown. Sona posts one on
//! its own each week on the day Settings names. Work from meetings kept on
//! this Mac stays out.

use super::{day_text, now_ms, refusal, Tracker, WorkAnswer, WriteArgs};
use crate::planning;
use crate::planning::ProjectHealth;
use crate::planning::work::digest::{self, DigestRisk, DigestTask, ProjectDigest};
use crate::query::external::ExternalError;
use crate::query::QUERY_SCHEMA_VERSION;
use clap::Args;
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DigestArgs {
    /// Show the digest and post nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkDigest {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub replayed: bool,
    /// The inbox notification it became; none on a dry run or with no
    /// project to tell of.
    pub notification_id: Option<String>,
    pub digest: ProjectDigest,
    /// False while the conversations can't be opened: then all work from
    /// meetings is left out, not only work from meetings kept on this Mac.
    pub conversations_available: bool,
}

impl Tracker<'_> {
    pub(super) async fn digest(&self, args: &DigestArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let to_ms = now_ms();
        let built = planning::history(self.history, |conn| {
            let hidden = board.fence.hidden_tasks(conn)?;
            digest::build(conn, &hidden, to_ms - digest::WEEK_MS, to_ms)
        })
        .map_err(|message| refusal(self.history, message))?;
        let mut answer = ExternalWorkDigest {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id: None,
            replayed: false,
            notification_id: None,
            digest: built,
            conversations_available: board.fence.is_open(),
        };
        if args.dry_run || answer.digest.projects.is_empty() {
            return Ok(WorkAnswer::Digest(Box::new(answer)));
        }
        let operation_id = args.write.fresh_operation_id();
        self.applies("posting the project digest")?;
        answer.replayed = self.ran(&operation_id)?;
        let posted = digest::post_as(self.app, &operation_id, &answer.digest)
            .map_err(|message| self.write_refusal(message, &operation_id, "The digest"))?;
        if let Some(notification) = posted {
            answer.notification_id = Some(notification.id);
            if let Some(stored) = notification.digest {
                answer.digest = stored;
            }
        }
        answer.operation_id = Some(operation_id);
        Ok(WorkAnswer::Digest(Box::new(answer)))
    }
}

fn health(health: ProjectHealth) -> &'static str {
    match health {
        ProjectHealth::OnTrack => "on track",
        ProjectHealth::AtRisk => "at risk",
        ProjectHealth::OffTrack => "off track",
    }
}

fn handles(tasks: &[DigestTask]) -> String {
    tasks
        .iter()
        .map(|task| format!("S-{} {}", task.number, task.title))
        .collect::<Vec<_>>()
        .join("; ")
}

fn risk(risk: &DigestRisk) -> String {
    match risk {
        DigestRisk::Health { health: said } => format!("latest update says {}", health(*said)),
        DigestRisk::PastTarget { target_at_ms } => {
            format!("target {} has passed", day_text(Some(*target_at_ms)))
        }
        DigestRisk::LateFinish {
            expected_at_ms,
            target_at_ms,
        } => format!(
            "at this pace it finishes {}, after its target {}",
            day_text(Some(*expected_at_ms)),
            day_text(Some(*target_at_ms))
        ),
        DigestRisk::Overdue { tasks } => format!("{} overdue: {}", tasks.len(), handles(tasks)),
        DigestRisk::LateMilestone { title, due_at_ms } => format!(
            "milestone {title} was due {}",
            day_text(Some(*due_at_ms))
        ),
        DigestRisk::NoUpdate { last_at_ms } => match last_at_ms {
            Some(at) => format!("no update since {}", day_text(Some(*at))),
            None => "no update yet".to_owned(),
        },
    }
}

/// The digest as lines, one block per project.
pub(super) fn digest_text(digest: &ProjectDigest, text: &mut String) {
    let _ = writeln!(
        text,
        "Projects, {} to {}",
        day_text(Some(digest.from_ms)),
        day_text(Some(digest.to_ms))
    );
    if digest.projects.is_empty() {
        let _ = writeln!(text, "No planned, active or paused projects.");
    }
    for project in &digest.projects {
        let status = project
            .status_name
            .clone()
            .unwrap_or_else(|| format!("{:?}", project.status).to_lowercase());
        let _ = writeln!(
            text,
            "\n{}  {status}, {}% done",
            project.title, project.percent
        );
        match &project.latest_update {
            Some(update) => {
                let _ = writeln!(
                    text,
                    "  Latest update {}: {}{}",
                    day_text(Some(update.created_at_ms)),
                    health(update.health),
                    update
                        .excerpt
                        .as_deref()
                        .map(|excerpt| format!(". {excerpt}"))
                        .unwrap_or_default()
                );
            }
            None => {
                let _ = writeln!(text, "  No update yet.");
            }
        }
        if project.done.is_empty() {
            let _ = writeln!(text, "  Nothing done this week.");
        } else {
            let _ = writeln!(
                text,
                "  Done this week ({}): {}",
                project.done.len(),
                handles(&project.done)
            );
        }
        for at_risk in &project.risks {
            let _ = writeln!(text, "  At risk: {}", risk(at_risk));
        }
    }
}

pub(super) fn text(answer: &ExternalWorkDigest, text: &mut String) {
    if answer.dry_run {
        text.push_str("Dry run. ");
    }
    let posted = match (&answer.notification_id, answer.digest.projects.is_empty()) {
        (_, true) => "Nothing to post.",
        (Some(_), false) if answer.replayed => "Already posted to the inbox.",
        (Some(_), false) => "Posted to the inbox.",
        (None, false) => "Would post to the inbox.",
    };
    let _ = writeln!(text, "{posted}");
    digest_text(&answer.digest, text);
}
