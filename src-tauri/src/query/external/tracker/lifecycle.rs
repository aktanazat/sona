//! `sona work` verbs for the bin and the archive, a description's earlier
//! versions, favorites, subscriptions and the inbox.

use super::{
    handle, iso_day, now_ms, refusal, task_ref, Board, ExternalWorkRef, TaskRef, TasksArgs,
    Tracker, WorkAnswer, WriteArgs,
};
use crate::planning;
use crate::planning::work::agents;
use crate::planning::work::bin::{self, Binned};
use crate::planning::work::favorites::{self, FavoriteKind};
use crate::planning::work::inbox::{
    self, Mark, Notification, NotificationKind, Shown, SubscribeReason,
};
use crate::planning::work::types::{WorkActor, WorkItem, WorkPatch};
use crate::planning::work::versions::{self, DescriptionVersion};
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use chrono::{Local, NaiveDate, TimeZone};
use clap::Args;
use serde::Serialize;
use std::collections::HashSet;
use std::fmt::Write as _;
use uuid::Uuid;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct VersionsArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct VersionArgs {
    /// The version's id, from `description-versions`.
    #[arg(value_name = "VERSION")]
    pub version: Uuid,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct VersionRestoreArgs {
    /// The version's id, from `description-versions`.
    #[arg(value_name = "VERSION")]
    pub version: Uuid,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FavoriteArgs {
    /// A task: S-12, 12, or its id.
    #[arg(value_parser = task_ref, value_name = "TASK", required_unless_present_any = ["project", "view"], conflicts_with_all = ["project", "view"])]
    pub task: Option<TaskRef>,
    /// A project, by name or id.
    #[arg(long, value_name = "PROJECT", conflicts_with = "view")]
    pub project: Option<String>,
    /// A saved view, by name or id.
    #[arg(long, value_name = "VIEW")]
    pub view: Option<String>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct InboxArgs {
    /// Only unread notifications.
    #[arg(long)]
    pub unread: bool,
    /// Also snoozed ones that have not woken yet.
    #[arg(long)]
    pub snoozed: bool,
    /// Also archived ones.
    #[arg(long)]
    pub archived: bool,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct InboxMarkArgs {
    /// Notification ids, from `inbox`.
    #[arg(value_name = "NOTIFICATION", required_unless_present = "all")]
    pub notifications: Vec<Uuid>,
    /// read, unread, archive, unarchive, or snooze.
    #[arg(long, value_parser = ["read", "unread", "archive", "unarchive", "snooze"])]
    pub mark: String,
    /// For snooze: YYYY-MM-DD, tomorrow, or none to wake it now.
    #[arg(long, value_name = "DAY", required_if_eq("mark", "snooze"))]
    pub until: Option<String>,
    /// Every unread notification, for --mark read.
    #[arg(long, conflicts_with = "notifications")]
    pub all: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleAction {
    Deleted,
    RestoredFromBin,
    DescriptionRestored,
    Favorited,
    Unfavorited,
    Subscribed,
    Unsubscribed,
    InboxMarked,
}

impl LifecycleAction {
    const fn text(self, dry_run: bool) -> &'static str {
        match (self, dry_run) {
            (Self::Deleted, false) => "Moved to the bin",
            (Self::Deleted, true) => "Would move to the bin",
            (Self::RestoredFromBin, false) => "Restored from the bin",
            (Self::RestoredFromBin, true) => "Would restore from the bin",
            (Self::DescriptionRestored, false) => "Restored the description",
            (Self::DescriptionRestored, true) => "Would restore the description",
            (Self::Favorited, false) => "Added to favorites",
            (Self::Favorited, true) => "Would add to favorites",
            (Self::Unfavorited, false) => "Removed from favorites",
            (Self::Unfavorited, true) => "Would remove from favorites",
            (Self::Subscribed, false) => "Subscribed",
            (Self::Subscribed, true) => "Would subscribe",
            (Self::Unsubscribed, false) => "Unsubscribed",
            (Self::Unsubscribed, true) => "Would unsubscribe",
            (Self::InboxMarked, false) => "Marked",
            (Self::InboxMarked, true) => "Would mark",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalLifecycleSkip {
    pub id: String,
    pub reason: String,
}

/// What a bin, favorite, subscription or inbox write did, or would do.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalLifecycleChange {
    pub schema_version: u32,
    pub action: LifecycleAction,
    pub dry_run: bool,
    /// None when nothing needed to change.
    pub operation_id: Option<String>,
    /// True when the operation id had already run: this is its answer again.
    pub replayed: bool,
    pub tasks: Vec<ExternalWorkRef>,
    pub skipped: Vec<ExternalLifecycleSkip>,
    pub favorite: Option<ExternalFavorite>,
    pub notifications: Vec<ExternalNotification>,
    /// For a description restore under suggest: it waits for the user.
    pub proposed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalArchivedTask {
    pub task: ExternalWorkRef,
    pub archived_at_ms: Option<i64>,
    /// Set for a deleted task, in the bin.
    pub deleted_at_ms: Option<i64>,
    /// When a deleted task is purged for good.
    pub purge_at_ms: Option<i64>,
    pub days_left: Option<i64>,
}

/// The archive: archived tasks and deleted ones still in the bin.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkArchive {
    pub schema_version: u32,
    pub archived: Vec<ExternalArchivedTask>,
    pub deleted: Vec<ExternalArchivedTask>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalDescriptionVersion {
    pub id: String,
    pub body: Option<String>,
    /// you, Sona, an agent's name, or before for the text before any edit.
    pub by: String,
    pub created_at_ms: i64,
    /// True for the description the task has now.
    pub current: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkVersions {
    pub schema_version: u32,
    pub task: ExternalWorkRef,
    pub versions: Vec<ExternalDescriptionVersion>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalFavorite {
    pub kind: FavoriteKind,
    pub id: String,
    pub name: String,
    /// `S-<number>`, for a task.
    pub handle: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkFavorites {
    pub schema_version: u32,
    pub favorites: Vec<ExternalFavorite>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSubscription {
    pub task: ExternalWorkRef,
    pub reason: SubscribeReason,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkSubscriptions {
    pub schema_version: u32,
    pub subscriptions: Vec<ExternalSubscription>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNotification {
    pub id: String,
    pub kind: NotificationKind,
    pub task: ExternalWorkRef,
    /// Sona or the agent's name.
    pub by: String,
    pub comment_id: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub created_at_ms: i64,
    pub read: bool,
    pub snoozed_until_ms: Option<i64>,
    pub archived: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkInbox {
    pub schema_version: u32,
    /// Unread notifications that are awake and not archived.
    pub unread: usize,
    pub notifications: Vec<ExternalNotification>,
}

pub(super) fn item_ref(item: &WorkItem) -> ExternalWorkRef {
    ExternalWorkRef {
        id: item.id.clone(),
        handle: handle(item.number),
        title: item.title.clone(),
        status: item.status,
    }
}

fn author(actor: Option<WorkActor>, agent: Option<&str>) -> String {
    match actor {
        None => "original".to_owned(),
        Some(WorkActor::Me) => "you".to_owned(),
        Some(WorkActor::Sona) => "Sona".to_owned(),
        Some(WorkActor::Agent) => agent.unwrap_or("an agent").to_owned(),
    }
}

fn snooze_until(value: &str) -> Result<Option<i64>, ExternalError> {
    let day = match value.trim().to_lowercase().as_str() {
        "none" | "now" => return Ok(None),
        "tomorrow" => Local::now().date_naive().succ_opt(),
        other => NaiveDate::parse_from_str(other, "%Y-%m-%d").ok(),
    };
    day.and_then(|day| day.and_hms_opt(8, 0, 0))
        .and_then(|at| Local.from_local_datetime(&at).earliest())
        .map(|at| Some(at.timestamp_millis()))
        .ok_or_else(|| ExternalError::invalid("--until takes YYYY-MM-DD, tomorrow, or none."))
}

fn found(board: &Board, binned: &[Binned], task: &TaskRef) -> Option<WorkItem> {
    let matches = |item: &WorkItem| match task {
        TaskRef::Number(number) => item.number == *number,
        TaskRef::Id(id) => item.id == *id,
    };
    binned
        .iter()
        .map(|binned| &binned.item)
        .find(|item| matches(item))
        .or_else(|| {
            board
                .rows
                .iter()
                .map(|row| &row.item)
                .find(|item| matches(item))
        })
        .cloned()
}

fn no_task(task: &TaskRef) -> ExternalError {
    ExternalError::new(ExternalErrorCode::NotFound, format!("No task {task}."))
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    let day = |at_ms: i64| iso_day(at_ms).unwrap_or_default();
    match answer {
        WorkAnswer::Lifecycle(change) => {
            let verb = if change.operation_id.is_none() && !change.dry_run {
                "Unchanged"
            } else {
                change.action.text(change.dry_run)
            };
            let what: Vec<String> = change
                .tasks
                .iter()
                .map(|task| format!("{} {}", task.handle, task.title))
                .chain(change.favorite.iter().map(|favorite| favorite.name.clone()))
                .chain(
                    change
                        .notifications
                        .iter()
                        .map(|notice| format!("{} on {}", notice.id, notice.task.handle)),
                )
                .collect();
            if !what.is_empty() {
                let _ = writeln!(text, "{verb}: {}.", what.join(", "));
            }
            if change.proposed {
                text.push_str("It waits for the user to accept in Sona.\n");
            }
            if change.replayed {
                text.push_str("This operation already ran; this is its answer again.\n");
            }
            for skip in &change.skipped {
                let _ = writeln!(text, "Skipped {}: {}.", skip.id, skip.reason);
            }
        }
        WorkAnswer::Archive(archive) => {
            if archive.archived.is_empty() && archive.deleted.is_empty() {
                text.push_str("Nothing archived or deleted.\n");
            }
            for task in &archive.archived {
                let _ = writeln!(
                    text,
                    "{} {}  archived {}",
                    task.task.handle,
                    task.task.title,
                    task.archived_at_ms.map(day).unwrap_or_default()
                );
            }
            for task in &archive.deleted {
                let _ = writeln!(
                    text,
                    "{} {}  deleted {}, purged in {} days",
                    task.task.handle,
                    task.task.title,
                    task.deleted_at_ms.map(day).unwrap_or_default(),
                    task.days_left.unwrap_or_default()
                );
            }
        }
        WorkAnswer::Versions(versions) => {
            let _ = writeln!(text, "{} {}", versions.task.handle, versions.task.title);
            if versions.versions.is_empty() {
                text.push_str("No earlier descriptions.\n");
            }
            for version in &versions.versions {
                let _ = writeln!(
                    text,
                    "{}  {} {}{}\n  {}",
                    version.id,
                    day(version.created_at_ms),
                    if version.by == "original" {
                        "original".to_owned()
                    } else {
                        format!("by {}", version.by)
                    },
                    if version.current { " (current)" } else { "" },
                    version
                        .body
                        .as_deref()
                        .unwrap_or("(empty)")
                        .replace('\n', "\n  ")
                );
            }
        }
        WorkAnswer::Favorites(favorites) => {
            if favorites.favorites.is_empty() {
                text.push_str("No favorites yet. Add one with: sona work favorite S-12\n");
            }
            for favorite in &favorites.favorites {
                let _ = writeln!(
                    text,
                    "{:?}  {}{}",
                    favorite.kind,
                    favorite
                        .handle
                        .as_deref()
                        .map(|handle| format!("{handle} "))
                        .unwrap_or_default(),
                    favorite.name
                );
            }
        }
        WorkAnswer::Subscriptions(subscriptions) => {
            if subscriptions.subscriptions.is_empty() {
                text.push_str("Not subscribed to any task.\n");
            }
            for subscription in &subscriptions.subscriptions {
                let _ = writeln!(
                    text,
                    "{} {}  ({:?})",
                    subscription.task.handle, subscription.task.title, subscription.reason
                );
            }
        }
        WorkAnswer::Inbox(inbox) => {
            let _ = writeln!(text, "{} unread.", inbox.unread);
            for notice in &inbox.notifications {
                let _ = writeln!(
                    text,
                    "{} {}  {:?} by {} on {} {}{}",
                    if notice.read { " " } else { "*" },
                    notice.id,
                    notice.kind,
                    notice.by,
                    notice.task.handle,
                    notice.task.title,
                    if notice.archived { " (archived)" } else { "" }
                );
            }
        }
        _ => {}
    }
}

impl Tracker<'_> {
    /// Deleted tasks the caller may see, still within their 30 days.
    fn binned(&self, board: &Board) -> Result<Vec<Binned>, ExternalError> {
        planning::history(self.history, |conn| {
            let hidden = board.fence.hidden_tasks(conn)?;
            let mut binned = bin::binned(conn, now_ms())?;
            binned.retain(|binned| !hidden.contains(&binned.item.id));
            Ok(binned)
        })
        .map_err(|message| refusal(self.history, message))
    }

    fn change(&self, action: LifecycleAction, dry_run: bool) -> ExternalLifecycleChange {
        ExternalLifecycleChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run,
            operation_id: None,
            replayed: false,
            tasks: Vec::new(),
            skipped: Vec::new(),
            favorite: None,
            notifications: Vec::new(),
            proposed: false,
        }
    }

    pub(super) async fn delete(&self, args: &TasksArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let binned = self.binned(&board)?;
        let mut answer = self.change(LifecycleAction::Deleted, args.dry_run);
        let mut named: Vec<WorkItem> = Vec::new();
        for task in &args.tasks {
            let item = found(&board, &binned, task).ok_or_else(|| no_task(task))?;
            if !named.iter().any(|seen| seen.id == item.id) {
                named.push(item);
            }
        }
        let in_bin: HashSet<&str> = binned
            .iter()
            .map(|binned| binned.item.id.as_str())
            .collect();
        let request: Vec<(&str, i64)> = named
            .iter()
            .map(|item| (item.id.as_str(), item.revision))
            .collect();
        let operation_id = args.write.operation_id("delete", &request)?;
        let replayed = self.ran(&operation_id)?;
        for item in &named {
            if in_bin.contains(item.id.as_str()) && !replayed {
                answer.skipped.push(ExternalLifecycleSkip {
                    id: handle(item.number),
                    reason: "already in the bin".to_owned(),
                });
            } else {
                answer.tasks.push(item_ref(item));
            }
        }
        if answer.tasks.is_empty() {
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        answer.operation_id = Some(operation_id.clone());
        if args.dry_run {
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        self.applies("deleting")?;
        let ids: Vec<String> = named.iter().map(|item| item.id.clone()).collect();
        let started_ms = now_ms();
        bin::set_trashed_as(self.app, WorkActor::Agent, &operation_id, &ids, true)
            .map_err(|message| self.write_refusal(message, &operation_id, "One of these tasks"))?;
        self.credit(&args.write, &ids, started_ms)?;
        answer.replayed = replayed;
        Ok(WorkAnswer::Lifecycle(Box::new(answer)))
    }

    /// `work restore`: tasks in the bin come out of it, archived ones come
    /// back from the archive. One call does one or the other.
    pub(super) async fn restore(&self, args: &TasksArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let binned = self.binned(&board)?;
        let mut from_bin = Vec::new();
        let mut elsewhere = false;
        for task in &args.tasks {
            match found(&board, &binned, task) {
                Some(item) if binned.iter().any(|binned| binned.item.id == item.id) => {
                    if !from_bin.iter().any(|seen: &WorkItem| seen.id == item.id) {
                        from_bin.push(item);
                    }
                }
                Some(_) => elsewhere = true,
                None => return Err(no_task(task)),
            }
        }
        if from_bin.is_empty() {
            return self.archive(args, false).await;
        }
        if elsewhere {
            return Err(ExternalError::invalid(
                "Some of these tasks are in the bin and some are not. Restore them in two calls.",
            ));
        }
        let request: Vec<(&str, i64)> = from_bin
            .iter()
            .map(|item| (item.id.as_str(), item.revision))
            .collect();
        let operation_id = args.write.operation_id("undelete", &request)?;
        let mut answer = self.change(LifecycleAction::RestoredFromBin, args.dry_run);
        answer.tasks = from_bin.iter().map(item_ref).collect();
        answer.operation_id = Some(operation_id.clone());
        if args.dry_run {
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        self.applies("restoring from the bin")?;
        answer.replayed = self.ran(&operation_id)?;
        let ids: Vec<String> = from_bin.iter().map(|item| item.id.clone()).collect();
        let started_ms = now_ms();
        let items = bin::set_trashed_as(self.app, WorkActor::Agent, &operation_id, &ids, false)
            .map_err(|message| self.write_refusal(message, &operation_id, "One of these tasks"))?;
        self.credit(&args.write, &ids, started_ms)?;
        answer.tasks = items.iter().map(item_ref).collect();
        Ok(WorkAnswer::Lifecycle(Box::new(answer)))
    }

    pub(super) async fn archive_list(
        &self,
        deleted_only: bool,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let now = now_ms();
        let deleted = self
            .binned(&board)?
            .into_iter()
            .map(|binned| ExternalArchivedTask {
                task: item_ref(&binned.item),
                archived_at_ms: binned.item.archived_at_ms,
                deleted_at_ms: Some(binned.trashed_at_ms),
                purge_at_ms: Some(binned.purge_at_ms),
                days_left: Some(((binned.purge_at_ms - now).max(0) + DAY_MS - 1) / DAY_MS),
            })
            .collect();
        let archived = if deleted_only {
            Vec::new()
        } else {
            let visible: HashSet<&str> =
                board.rows.iter().map(|row| row.item.id.as_str()).collect();
            planning::history(self.history, |conn| bin::archived(conn))
                .map_err(|message| refusal(self.history, message))?
                .into_iter()
                .filter(|item| visible.contains(item.id.as_str()))
                .map(|item| ExternalArchivedTask {
                    task: item_ref(&item),
                    archived_at_ms: item.archived_at_ms,
                    deleted_at_ms: None,
                    purge_at_ms: None,
                    days_left: None,
                })
                .collect()
        };
        Ok(WorkAnswer::Archive(ExternalWorkArchive {
            schema_version: QUERY_SCHEMA_VERSION,
            archived,
            deleted,
        }))
    }

    pub(super) async fn description_versions(
        &self,
        args: &VersionsArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let found = planning::history(self.history, |conn| versions::list(conn, &row.item.id))
            .map_err(|message| refusal(self.history, message))?;
        Ok(versions_answer(&row.item, found))
    }

    fn version_of(
        &self,
        board: &Board,
        id: &Uuid,
    ) -> Result<(DescriptionVersion, WorkItem), ExternalError> {
        let missing = || {
            ExternalError::new(
                ExternalErrorCode::NotFound,
                format!("No description version {id}."),
            )
            .hint("List them with: sona work description-versions S-12")
        };
        let version = planning::history(self.history, |conn| {
            versions::version(conn, &id.to_string())
        })
        .map_err(|_| missing())?;
        let item = board
            .row(&version.item_id)
            .ok_or_else(missing)?
            .item
            .clone();
        Ok((version, item))
    }

    pub(super) async fn description_version(
        &self,
        args: &VersionArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (version, item) = self.version_of(&board, &args.version)?;
        Ok(versions_answer(&item, vec![version]))
    }

    pub(super) async fn description_restore(
        &self,
        args: &VersionRestoreArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (version, item) = self.version_of(&board, &args.version)?;
        let mut answer = self.change(LifecycleAction::DescriptionRestored, args.dry_run);
        answer.tasks.push(item_ref(&item));
        if version.body == item.details {
            answer.skipped.push(ExternalLifecycleSkip {
                id: handle(item.number),
                reason: "the description is already this version".to_owned(),
            });
            answer.tasks.clear();
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        let operation_id = args
            .write
            .operation_id("description-restore", &(&version.id, item.revision))?;
        answer.operation_id = Some(operation_id.clone());
        if args.dry_run {
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        answer.replayed = self.ran(&operation_id)?;
        let patch = WorkPatch {
            details: Some(version.body.clone()),
            ..WorkPatch::default()
        };
        let change = agents::update_as(
            self.app,
            &args.write.agent(),
            &operation_id,
            agents::ItemChange {
                id: &item.id,
                expected_revision: item.revision,
                patch: &patch,
                link: None,
            },
        )
        .await
        .map_err(|message| self.write_refusal(message, &operation_id, &handle(item.number)))?;
        answer.proposed = !change.proposed.is_empty();
        answer.tasks = vec![item_ref(&change.item)];
        Ok(WorkAnswer::Lifecycle(Box::new(answer)))
    }

    fn favorite_target(
        &self,
        board: &Board,
        args: &FavoriteArgs,
    ) -> Result<ExternalFavorite, ExternalError> {
        if let Some(task) = &args.task {
            let row = board.task(task)?;
            return Ok(ExternalFavorite {
                kind: FavoriteKind::Task,
                id: row.item.id.clone(),
                name: row.item.title.clone(),
                handle: Some(handle(row.item.number)),
            });
        }
        if let Some(project) = &args.project {
            let project = board
                .project(project)?
                .ok_or_else(|| ExternalError::invalid("Name a project."))?;
            return Ok(ExternalFavorite {
                kind: FavoriteKind::Project,
                id: project.id.clone(),
                name: project.title.clone(),
                handle: None,
            });
        }
        let view = self.view_named(args.view.as_deref().unwrap_or_default())?;
        Ok(ExternalFavorite {
            kind: FavoriteKind::View,
            id: view.id,
            name: view.name,
            handle: None,
        })
    }

    pub(super) async fn favorite(
        &self,
        args: &FavoriteArgs,
        on: bool,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let target = self.favorite_target(&board, args)?;
        let action = if on {
            LifecycleAction::Favorited
        } else {
            LifecycleAction::Unfavorited
        };
        let mut answer = self.change(action, args.dry_run);
        let operation_id = args.write.fresh_operation_id();
        let replayed = self.ran(&operation_id)?;
        let already = planning::history(self.history, |conn| {
            Ok(favorites::favorites(conn)?
                .iter()
                .any(|favorite| favorite.kind == target.kind && favorite.target_id == target.id))
        })
        .map_err(|message| refusal(self.history, message))?;
        answer.favorite = Some(target.clone());
        if already == on && !replayed {
            answer.skipped.push(ExternalLifecycleSkip {
                id: target.handle.clone().unwrap_or_else(|| target.name.clone()),
                reason: if on {
                    "already a favorite"
                } else {
                    "not a favorite"
                }
                .to_owned(),
            });
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        answer.operation_id = Some(operation_id.clone());
        if args.dry_run {
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        self.applies("changing favorites")?;
        favorites::set_as(self.app, &operation_id, target.kind, &target.id, on)
            .map_err(|message| self.write_refusal(message, &operation_id, "The favorite"))?;
        answer.replayed = replayed;
        Ok(WorkAnswer::Lifecycle(Box::new(answer)))
    }

    pub(super) async fn favorites(&self) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let views = self.all_views()?;
        let saved = planning::history(self.history, |conn| favorites::favorites(conn))
            .map_err(|message| refusal(self.history, message))?;
        let favorites = saved
            .into_iter()
            .filter_map(|favorite| match favorite.kind {
                FavoriteKind::Task => board.row(&favorite.target_id).map(|row| ExternalFavorite {
                    kind: favorite.kind,
                    id: row.item.id.clone(),
                    name: row.item.title.clone(),
                    handle: Some(handle(row.item.number)),
                }),
                FavoriteKind::Project => board
                    .projects
                    .iter()
                    .find(|project| project.id == favorite.target_id)
                    .map(|project| ExternalFavorite {
                        kind: favorite.kind,
                        id: project.id.clone(),
                        name: project.title.clone(),
                        handle: None,
                    }),
                FavoriteKind::View => {
                    views
                        .iter()
                        .find(|view| view.id == favorite.target_id)
                        .map(|view| ExternalFavorite {
                            kind: favorite.kind,
                            id: view.id.clone(),
                            name: view.name.clone(),
                            handle: None,
                        })
                }
            })
            .collect();
        Ok(WorkAnswer::Favorites(ExternalWorkFavorites {
            schema_version: QUERY_SCHEMA_VERSION,
            favorites,
        }))
    }

    pub(super) async fn subscribe(
        &self,
        args: &TasksArgs,
        on: bool,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let rows = board.tasks(&args.tasks)?;
        let action = if on {
            LifecycleAction::Subscribed
        } else {
            LifecycleAction::Unsubscribed
        };
        let mut answer = self.change(action, args.dry_run);
        let operation_id = args.write.fresh_operation_id();
        let replayed = self.ran(&operation_id)?;
        let subscribed: HashSet<String> =
            planning::history(self.history, |conn| inbox::subscriptions(conn))
                .map_err(|message| refusal(self.history, message))?
                .into_iter()
                .map(|subscription| subscription.item_id)
                .collect();
        let mut ids = Vec::new();
        for row in rows {
            if subscribed.contains(&row.item.id) == on && !replayed {
                answer.skipped.push(ExternalLifecycleSkip {
                    id: handle(row.item.number),
                    reason: if on {
                        "already subscribed"
                    } else {
                        "not subscribed"
                    }
                    .to_owned(),
                });
            } else {
                ids.push(row.item.id.clone());
                answer.tasks.push(item_ref(&row.item));
            }
        }
        if ids.is_empty() {
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        answer.operation_id = Some(operation_id.clone());
        if args.dry_run {
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        self.applies("changing subscriptions")?;
        inbox::subscribe_as(self.app, &operation_id, &ids, on)
            .map_err(|message| self.write_refusal(message, &operation_id, "One of these tasks"))?;
        answer.replayed = replayed;
        Ok(WorkAnswer::Lifecycle(Box::new(answer)))
    }

    pub(super) async fn subscriptions(&self) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let subscriptions = planning::history(self.history, |conn| inbox::subscriptions(conn))
            .map_err(|message| refusal(self.history, message))?
            .into_iter()
            .filter_map(|subscription| {
                board
                    .row(&subscription.item_id)
                    .map(|row| ExternalSubscription {
                        task: item_ref(&row.item),
                        reason: subscription.reason,
                    })
            })
            .collect();
        Ok(WorkAnswer::Subscriptions(ExternalWorkSubscriptions {
            schema_version: QUERY_SCHEMA_VERSION,
            subscriptions,
        }))
    }

    fn notice(board: &Board, notification: Notification) -> Option<ExternalNotification> {
        let row = board.row(&notification.item_id)?;
        Some(ExternalNotification {
            task: item_ref(&row.item),
            by: author(Some(notification.actor), notification.agent.as_deref()),
            id: notification.id,
            kind: notification.kind,
            comment_id: notification.comment_id,
            from: notification.from,
            to: notification.to,
            created_at_ms: notification.created_at_ms,
            read: notification.read_at_ms.is_some(),
            snoozed_until_ms: notification.snoozed_until_ms,
            archived: notification.archived_at_ms.is_some(),
        })
    }

    fn notices(
        &self,
        board: &Board,
        shown: Shown,
    ) -> Result<Vec<ExternalNotification>, ExternalError> {
        planning::history(self.history, |conn| {
            inbox::notifications(conn, shown, now_ms())
        })
        .map_err(|message| refusal(self.history, message))
        .map(|found| {
            found
                .into_iter()
                .filter_map(|notification| Self::notice(board, notification))
                .collect()
        })
    }

    pub(super) async fn inbox(&self, args: &InboxArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let unread = self
            .notices(
                &board,
                Shown {
                    unread_only: true,
                    ..Shown::default()
                },
            )?
            .len();
        let notifications = self.notices(
            &board,
            Shown {
                unread_only: args.unread,
                snoozed: args.snoozed,
                archived: args.archived,
            },
        )?;
        Ok(WorkAnswer::Inbox(ExternalWorkInbox {
            schema_version: QUERY_SCHEMA_VERSION,
            unread,
            notifications,
        }))
    }

    pub(super) async fn inbox_mark(
        &self,
        args: &InboxMarkArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let how = match args.mark.as_str() {
            "read" => Mark::Read,
            "unread" => Mark::Unread,
            "archive" => Mark::Archive,
            "unarchive" => Mark::Unarchive,
            _ => Mark::Snooze(snooze_until(args.until.as_deref().unwrap_or("none"))?),
        };
        if args.all && how != Mark::Read {
            return Err(ExternalError::invalid("--all goes with --mark read."));
        }
        let every = self.notices(
            &board,
            Shown {
                unread_only: false,
                snoozed: true,
                archived: true,
            },
        )?;
        let picked: Vec<ExternalNotification> = if args.all {
            every
                .into_iter()
                .filter(|notice| {
                    !notice.read
                        && !notice.archived
                        && notice
                            .snoozed_until_ms
                            .is_none_or(|until| until <= now_ms())
                })
                .collect()
        } else {
            let mut picked = Vec::new();
            for id in &args.notifications {
                let id = id.to_string();
                let notice = every.iter().find(|notice| notice.id == id).ok_or_else(|| {
                    ExternalError::new(
                        ExternalErrorCode::NotFound,
                        format!("No notification {id}."),
                    )
                    .hint("List them with: sona work inbox --archived --snoozed")
                })?;
                if !picked
                    .iter()
                    .any(|seen: &ExternalNotification| seen.id == id)
                {
                    picked.push(notice.clone());
                }
            }
            picked
        };
        let mut answer = self.change(LifecycleAction::InboxMarked, args.dry_run);
        answer.notifications = picked;
        if answer.notifications.is_empty() {
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        let ids: Vec<String> = answer
            .notifications
            .iter()
            .map(|notice| notice.id.clone())
            .collect();
        let operation_id = args.write.fresh_operation_id();
        answer.operation_id = Some(operation_id.clone());
        if args.dry_run {
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        self.applies("changing the inbox")?;
        answer.replayed = self.ran(&operation_id)?;
        let marked = inbox::mark_as(self.app, &operation_id, &ids, how)
            .map_err(|message| self.write_refusal(message, &operation_id, "The notification"))?;
        answer.notifications = marked
            .into_iter()
            .filter_map(|notification| Self::notice(&board, notification))
            .collect();
        Ok(WorkAnswer::Lifecycle(Box::new(answer)))
    }
}

fn versions_answer(item: &WorkItem, found: Vec<DescriptionVersion>) -> WorkAnswer {
    let newest = found.first().map(|version| version.id.clone());
    WorkAnswer::Versions(ExternalWorkVersions {
        schema_version: QUERY_SCHEMA_VERSION,
        task: item_ref(item),
        versions: found
            .into_iter()
            .map(|version| ExternalDescriptionVersion {
                current: newest.as_deref() == Some(version.id.as_str())
                    && version.body == item.details,
                by: author(version.actor, version.agent.as_deref()),
                id: version.id,
                body: version.body,
                created_at_ms: version.created_at_ms,
            })
            .collect(),
    })
}
