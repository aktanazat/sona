//! `sona work updates`, `feeds`, `feed-save` and `feed-delete`: one feed of
//! the updates posted on projects and the weekly project digests, newest
//! first, narrowed by project, initiative, label, health or a start day, and
//! saved narrowings under a name.

use super::lifecycle::agents_digest;
use super::roadmap::{
    external_update, project_ref, ExternalProjectRef, ExternalProjectUpdate, Health,
};
use super::{iso_day, pick, refusal, Board, Tracker, WorkAnswer, WriteArgs};
use crate::planning;
use crate::planning::work::digest::ProjectDigest;
use crate::planning::work::feed::{self, Feed, FeedEntry, FeedFilter, FeedSince};
use crate::query::external::ExternalError;
use crate::query::QUERY_SCHEMA_VERSION;
use chrono::{Local, NaiveDate, TimeZone};
use clap::Args;
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Args, Clone, Debug, Default, Eq, PartialEq)]
pub struct FeedFilterArgs {
    /// Only updates on this project, by name or id; repeat for more.
    #[arg(long = "project", value_name = "PROJECT")]
    pub projects: Vec<String>,
    /// Only updates on the projects of this initiative; repeat for more.
    #[arg(long = "initiative", value_name = "INITIATIVE")]
    pub initiatives: Vec<String>,
    /// Only updates on projects with this label; repeat for more.
    #[arg(long = "label", value_name = "LABEL")]
    pub labels: Vec<String>,
    /// Only updates of this health; repeat for more. Leaves digests out.
    #[arg(long = "health", value_enum, value_name = "HEALTH")]
    pub healths: Vec<Health>,
    /// Only since this day, YYYY-MM-DD, or over the last N days, Nd, counted
    /// back from each time a saved feed is read.
    #[arg(long, value_name = "DAY")]
    pub since: Option<String>,
}

impl FeedFilterArgs {
    const fn narrows(&self) -> bool {
        !self.projects.is_empty()
            || !self.initiatives.is_empty()
            || !self.labels.is_empty()
            || !self.healths.is_empty()
            || self.since.is_some()
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct UpdatesArgs {
    #[command(flatten)]
    pub filter: FeedFilterArgs,
    /// A saved feed, by name or id, in place of the filters.
    #[arg(long, value_name = "FEED", conflicts_with_all = ["projects", "initiatives", "labels", "healths", "since"])]
    pub feed: Option<String>,
    /// At most this many, newest first.
    #[arg(long, default_value_t = 50, value_name = "N")]
    pub limit: usize,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FeedSaveArgs {
    /// The feed's name. A feed with that name changes.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// The feed to rename or refill, by name or id.
    #[arg(long, value_name = "FEED")]
    pub feed: Option<String>,
    #[command(flatten)]
    pub filter: FeedFilterArgs,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FeedDeleteArgs {
    /// The feed, by name or id.
    #[arg(value_name = "FEED")]
    pub feed: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

/// One line of the feed, as an agent reads it.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExternalFeedEntry {
    Update {
        project: ExternalProjectRef,
        update: ExternalProjectUpdate,
    },
    Digest {
        notice_id: String,
        created_at_ms: i64,
        digest: ProjectDigest,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkUpdates {
    pub schema_version: u32,
    /// The saved feed read, when one was named.
    pub feed: Option<String>,
    pub entries: Vec<ExternalFeedEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalFeed {
    pub id: String,
    pub name: String,
    pub project_ids: Vec<String>,
    pub initiative_ids: Vec<String>,
    pub label_ids: Vec<String>,
    pub healths: Vec<Health>,
    /// A fixed start, when the feed has one.
    pub since_ms: Option<i64>,
    /// How many days back from each read the feed reaches, when it does.
    pub since_days: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkFeeds {
    pub schema_version: u32,
    pub feeds: Vec<ExternalFeed>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedAction {
    Saved,
    Deleted,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalFeedChange {
    pub schema_version: u32,
    pub action: FeedAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub feed: ExternalFeed,
}

fn external_feed(feed: Feed) -> ExternalFeed {
    ExternalFeed {
        id: feed.id,
        name: feed.name,
        project_ids: feed.filter.project_ids,
        initiative_ids: feed.filter.initiative_ids,
        label_ids: feed.filter.label_ids,
        healths: feed.filter.healths.into_iter().map(Health::of).collect(),
        since_ms: match feed.filter.since {
            Some(FeedSince::At(at_ms)) => Some(at_ms),
            _ => None,
        },
        since_days: match feed.filter.since {
            Some(FeedSince::LastDays(days)) => Some(days),
            _ => None,
        },
    }
}

/// The start of a day, or the last `N` days for `Nd`.
fn since(value: &str) -> Result<FeedSince, ExternalError> {
    let value = value.trim();
    if let Some(Ok(days)) = value.strip_suffix('d').map(str::parse::<u32>) {
        return Ok(FeedSince::LastDays(days));
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .and_then(|at| Local.from_local_datetime(&at).earliest())
        .map(|at| FeedSince::At(at.timestamp_millis()))
        .ok_or_else(|| ExternalError::invalid("--since takes YYYY-MM-DD or 7d."))
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    let day = |at_ms: i64| iso_day(at_ms).unwrap_or_default();
    match answer {
        WorkAnswer::Updates(updates) => {
            if updates.entries.is_empty() {
                text.push_str("No updates.\n");
            }
            for entry in &updates.entries {
                match entry {
                    ExternalFeedEntry::Update { project, update } => {
                        let _ = writeln!(
                            text,
                            "{}  {}  {}  by {}",
                            day(update.created_at_ms),
                            project.name,
                            update.health.text(),
                            update.author.name.as_deref().unwrap_or("you")
                        );
                        if let Some(body) = &update.body {
                            for line in body.lines().filter(|line| !line.trim().is_empty()) {
                                let _ = writeln!(text, "    {line}");
                            }
                        }
                    }
                    ExternalFeedEntry::Digest {
                        created_at_ms,
                        digest,
                        ..
                    } => {
                        let _ = writeln!(
                            text,
                            "{}  Weekly project digest: {} projects",
                            day(*created_at_ms),
                            digest.projects.len()
                        );
                    }
                }
            }
        }
        WorkAnswer::Feeds(feeds) => {
            if feeds.feeds.is_empty() {
                text.push_str("No saved feeds.\n");
            }
            for feed in &feeds.feeds {
                let _ = writeln!(text, "{}  ({})", feed.name, feed.id);
            }
        }
        WorkAnswer::Feed(change) => {
            let verb = match (change.action, change.dry_run) {
                (FeedAction::Saved, false) => "Saved",
                (FeedAction::Saved, true) => "Would save",
                (FeedAction::Deleted, false) => "Deleted",
                (FeedAction::Deleted, true) => "Would delete",
            };
            let _ = writeln!(text, "{verb} feed {}.", change.feed.name);
        }
        _ => {}
    }
}

impl Tracker<'_> {
    fn saved_feeds(&self) -> Result<Vec<Feed>, ExternalError> {
        planning::history(self.history, |conn| feed::feeds(conn))
            .map_err(|message| refusal(self.history, message))
    }

    fn feed_named(&self, value: &str) -> Result<Feed, ExternalError> {
        let feeds = self.saved_feeds()?;
        pick("feed", value, &feeds, |feed| &feed.id, |feed| &feed.name)
            .cloned()
            .map_err(|refusal| refusal.hint("List them with: sona work feeds"))
    }

    /// The filter the flags name, every name read as an id.
    fn feed_filter(
        &self,
        board: &Board,
        args: &FeedFilterArgs,
    ) -> Result<FeedFilter, ExternalError> {
        let mut filter = FeedFilter {
            healths: args.healths.iter().map(|health| health.health()).collect(),
            since: args.since.as_deref().map(since).transpose()?,
            ..FeedFilter::default()
        };
        for project in &args.projects {
            let project = board
                .project(project)?
                .ok_or_else(|| ExternalError::invalid("Name a project."))?;
            filter.project_ids.push(project.id.clone());
        }
        if !args.initiatives.is_empty() {
            let all = self.all_initiatives()?;
            for initiative in &args.initiatives {
                filter
                    .initiative_ids
                    .push(Self::pick_initiative(initiative, &all)?.id.clone());
            }
        }
        if !args.labels.is_empty() {
            let labels = self.all_labels()?;
            for label in &args.labels {
                let label = pick(
                    "label",
                    label,
                    &labels,
                    |label| &label.id,
                    |label| &label.name,
                )?;
                filter.label_ids.push(label.id.clone());
            }
        }
        Ok(filter)
    }

    pub(super) async fn updates(&self, args: &UpdatesArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (feed, filter) = match &args.feed {
            Some(name) => {
                let feed = self.feed_named(name)?;
                (Some(feed.name), feed.filter)
            }
            None => (None, self.feed_filter(&board, &args.filter)?),
        };
        let quotes_shown = self.quotes_shown().await?;
        let projects = self.every_project()?;
        let found = planning::history(self.history, |conn| {
            feed::entries(conn, &filter, args.limit, super::now_ms())
        })
        .map_err(|message| refusal(self.history, message))?;
        let entries = found
            .into_iter()
            .filter_map(|entry| match entry {
                FeedEntry::Update { update, .. } => {
                    let project = projects
                        .iter()
                        .find(|project| project.id == update.project_id)?;
                    Some(ExternalFeedEntry::Update {
                        project: project_ref(project),
                        update: external_update(update, &quotes_shown),
                    })
                }
                FeedEntry::Digest {
                    notice_id,
                    created_at_ms,
                    digest,
                } => Some(ExternalFeedEntry::Digest {
                    notice_id,
                    created_at_ms,
                    digest: agents_digest(&board, digest),
                }),
            })
            .collect();
        Ok(WorkAnswer::Updates(ExternalWorkUpdates {
            schema_version: QUERY_SCHEMA_VERSION,
            feed,
            entries,
        }))
    }

    pub(super) fn feeds(&self) -> Result<WorkAnswer, ExternalError> {
        Ok(WorkAnswer::Feeds(ExternalWorkFeeds {
            schema_version: QUERY_SCHEMA_VERSION,
            feeds: self.saved_feeds()?.into_iter().map(external_feed).collect(),
        }))
    }

    pub(super) async fn feed_save(&self, args: &FeedSaveArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let existing = args
            .feed
            .as_deref()
            .map(|feed| self.feed_named(feed))
            .transpose()?;
        let filter = if args.filter.narrows() || existing.is_none() {
            self.feed_filter(&board, &args.filter)?
        } else {
            existing
                .as_ref()
                .map(|feed| feed.filter.clone())
                .unwrap_or_default()
        };
        let id = existing.as_ref().map(|feed| feed.id.as_str());
        let operation_id = args
            .write
            .operation_id("feed-save", &(id, &args.name, &filter))?;
        let (feed, dry_run) = if args.dry_run {
            let feed = self
                .rehearse(|conn| feed::save_feed(conn, id, &args.name, &filter, super::now_ms()))?;
            (feed, true)
        } else {
            self.applies("saving a feed")?;
            let feed = feed::feed_save_as(self.app, &operation_id, id, &args.name, &filter)
                .map_err(|message| self.write_refusal(message, &operation_id, "The feed"))?;
            (feed, false)
        };
        Ok(WorkAnswer::Feed(ExternalFeedChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: FeedAction::Saved,
            dry_run,
            operation_id: Some(operation_id),
            feed: external_feed(feed),
        }))
    }

    pub(super) fn feed_delete(&self, args: &FeedDeleteArgs) -> Result<WorkAnswer, ExternalError> {
        let feed = self.feed_named(&args.feed)?;
        let operation_id = args.write.operation_id("feed-delete", &feed.id)?;
        if !args.dry_run {
            self.applies("deleting a feed")?;
            feed::feed_delete_as(self.app, &operation_id, &feed.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The feed"))?;
        }
        Ok(WorkAnswer::Feed(ExternalFeedChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: FeedAction::Deleted,
            dry_run: args.dry_run,
            operation_id: Some(operation_id),
            feed: external_feed(feed),
        }))
    }
}
