//! `sona work` verbs for the bin and the archive, a description's earlier
//! versions, favorites, subscriptions and the inbox.

use super::{
    handle, iso_day, now_ms, pick, refusal, task_ref, Board, ExternalWorkRef, TaskRef, TasksArgs,
    Tracker, WorkAnswer, WriteArgs,
};
use crate::planning;
use crate::planning::work::agents;
use crate::planning::work::bin::{self, Binned};
use crate::planning::work::favorites::{
    self, FavoriteEntry, FavoriteKind, FavoriteMove, Favorites, FolderSave,
};
use crate::planning::work::inbox::{
    self, Mark, Notification, NotificationKind, Shown, SubscribeReason,
};
use crate::planning::work::digest::{DigestRisk, ProjectDigest};
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

/// One favorite: a task, or a project, saved view or person by flag.
#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FavoriteTarget {
    /// A task: S-12, 12, or its id.
    #[arg(value_parser = task_ref, value_name = "TASK", required_unless_present_any = ["project", "view", "person"], conflicts_with_all = ["project", "view", "person"])]
    pub task: Option<TaskRef>,
    /// A project, by name or id.
    #[arg(long, value_name = "PROJECT", conflicts_with_all = ["view", "person"])]
    pub project: Option<String>,
    /// A saved view, by name or id.
    #[arg(long, value_name = "VIEW", conflicts_with = "person")]
    pub view: Option<String>,
    /// A person: a name, another name they go by, or their id.
    #[arg(long, value_name = "PERSON")]
    pub person: Option<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FavoriteArgs {
    #[command(flatten)]
    pub target: FavoriteTarget,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FavoriteFolderSaveArgs {
    /// The folder to rename or move, by name or id. Without it a new folder
    /// is made.
    #[arg(long, value_name = "FOLDER", required_unless_present = "name")]
    pub folder: Option<String>,
    /// The folder's name.
    #[arg(long, value_name = "NAME", required_unless_present = "position")]
    pub name: Option<String>,
    /// Its place among the folders and loose favorites at the top, from 1.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub position: Option<u64>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FavoriteFolderDeleteArgs {
    /// The folder, by name or id.
    #[arg(value_name = "FOLDER")]
    pub folder: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FavoriteMoveArgs {
    #[command(flatten)]
    pub target: FavoriteTarget,
    /// Into this folder, by name or id.
    #[arg(long, value_name = "FOLDER", conflicts_with = "top")]
    pub into: Option<String>,
    /// Out of its folder, to the top.
    #[arg(long)]
    pub top: bool,
    /// Its place there, from 1. Last without it when it changes folder;
    /// otherwise it must be given.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..), required_unless_present_any = ["into", "top"])]
    pub position: Option<u64>,
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
    /// Every notification of a kind: with --mark read, every unread one;
    /// with --mark archive, every read one, clearing them out.
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
    FavoriteMoved,
    FolderSaved,
    FolderDeleted,
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
            (Self::FavoriteMoved, false) => "Moved",
            (Self::FavoriteMoved, true) => "Would move",
            (Self::FolderSaved, false) => "Saved",
            (Self::FolderSaved, true) => "Would save",
            (Self::FolderDeleted, false) => "Deleted",
            (Self::FolderDeleted, true) => "Would delete",
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
    /// The folder made, renamed or deleted, or the one a favorite moved
    /// into.
    pub folder: Option<ExternalFolderRef>,
    /// Where the favorite or folder now sits in its folder or at the top,
    /// from 1.
    pub position: Option<usize>,
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

/// A favorites folder a write names. No id yet on a dry run that makes one.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalFolderRef {
    pub id: Option<String>,
    pub name: String,
}

/// A folder in the favorites, with its favorites in order.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalFavoriteFolder {
    /// Always `folder`.
    pub kind: &'static str,
    pub id: String,
    pub name: String,
    pub favorites: Vec<ExternalFavorite>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ExternalFavoriteEntry {
    Favorite(ExternalFavorite),
    Folder(ExternalFavoriteFolder),
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkFavorites {
    pub schema_version: u32,
    /// The sidebar's order: loose favorites and folders, each folder with
    /// its own.
    pub favorites: Vec<ExternalFavoriteEntry>,
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
    /// None only for an automation run about no task.
    pub task: Option<ExternalWorkRef>,
    /// Sona or the agent's name.
    pub by: String,
    pub comment_id: Option<String>,
    /// For `changed`: what changed, as the task history names it (due,
    /// priority, details, owner, labels, project, parent, relation,
    /// attachment) or `reminder`.
    pub field: Option<String>,
    /// For `automation`: which automation ran, and its error when it failed.
    pub automation: Option<ExternalNoticeAutomation>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub created_at_ms: i64,
    pub read: bool,
    pub snoozed_until_ms: Option<i64>,
    pub archived: bool,
    /// For `digest`: each project's status, latest update, tasks done and
    /// risks over its week, as posted.
    pub digest: Option<ProjectDigest>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNoticeAutomation {
    pub id: String,
    pub run_id: String,
    pub name: String,
    pub error: Option<String>,
}

impl ExternalNotification {
    /// The task it is about, or the automation for a run about no task.
    fn about(&self) -> String {
        match (&self.task, &self.automation) {
            (Some(task), _) => format!("{} {}", task.handle, task.title),
            (None, Some(automation)) => format!("automation {}", automation.name),
            (None, None) if self.digest.is_some() => "weekly project digest".to_owned(),
            (None, None) => String::new(),
        }
    }
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
                        .folder
                        .iter()
                        .filter(|_| change.favorite.is_none())
                        .map(|folder| format!("folder {}", folder.name)),
                )
                .chain(
                    change
                        .notifications
                        .iter()
                        .map(|notice| format!("{} on {}", notice.id, notice.about())),
                )
                .collect();
            if !what.is_empty() {
                let _ = writeln!(text, "{verb}: {}.", what.join(", "));
            }
            if change.action == LifecycleAction::FavoriteMoved {
                if let Some(position) = change.position {
                    let place = change
                        .folder
                        .as_ref()
                        .map_or_else(|| "at the top".to_owned(), |folder| format!("in {}", folder.name));
                    let _ = writeln!(text, "Place {position} {place}.");
                }
            }
            if change.action == LifecycleAction::FolderDeleted && change.operation_id.is_some() {
                text.push_str("Its favorites stay, at the top where it was.\n");
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
            let line = |text: &mut String, indent: &str, favorite: &ExternalFavorite| {
                let _ = writeln!(
                    text,
                    "{indent}{:?}  {}{}",
                    favorite.kind,
                    favorite
                        .handle
                        .as_deref()
                        .map(|handle| format!("{handle} "))
                        .unwrap_or_default(),
                    favorite.name
                );
            };
            for entry in &favorites.favorites {
                match entry {
                    ExternalFavoriteEntry::Favorite(favorite) => line(text, "", favorite),
                    ExternalFavoriteEntry::Folder(folder) => {
                        let _ = writeln!(text, "Folder  {}", folder.name);
                        if folder.favorites.is_empty() {
                            text.push_str("  (empty)\n");
                        }
                        for favorite in &folder.favorites {
                            line(text, "  ", favorite);
                        }
                    }
                }
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
                let what = match (&notice.field, &notice.automation) {
                    (Some(field), _) => format!("{:?} {field}", notice.kind),
                    (None, Some(_)) => format!(
                        "{:?} {}",
                        notice.kind,
                        notice.to.as_deref().unwrap_or_default()
                    ),
                    (None, None) => format!("{:?}", notice.kind),
                };
                let _ = writeln!(
                    text,
                    "{} {}  {what} by {} on {}{}",
                    if notice.read { " " } else { "*" },
                    notice.id,
                    notice.by,
                    notice.about(),
                    if notice.archived { " (archived)" } else { "" }
                );
            }
            let newest = inbox
                .notifications
                .iter()
                .find_map(|notice| notice.digest.as_ref());
            if let Some(digest) = newest {
                text.push('\n');
                super::digest::digest_text(digest, text);
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
            folder: None,
            position: None,
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
        target: &FavoriteTarget,
    ) -> Result<ExternalFavorite, ExternalError> {
        if let Some(task) = &target.task {
            let row = board.task(task)?;
            return Ok(ExternalFavorite {
                kind: FavoriteKind::Task,
                id: row.item.id.clone(),
                name: row.item.title.clone(),
                handle: Some(handle(row.item.number)),
            });
        }
        if let Some(project) = &target.project {
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
        if let Some(person) = &target.person {
            let person = board.person(person)?;
            return Ok(ExternalFavorite {
                kind: FavoriteKind::Person,
                id: person.id.clone(),
                name: person.name.clone(),
                handle: None,
            });
        }
        let view = self.view_named(target.view.as_deref().unwrap_or_default())?;
        Ok(ExternalFavorite {
            kind: FavoriteKind::View,
            id: view.id,
            name: view.name,
            handle: None,
        })
    }

    fn saved_favorites(&self) -> Result<Favorites, ExternalError> {
        planning::history(self.history, |conn| favorites::favorites(conn))
            .map_err(|message| refusal(self.history, message))
    }

    fn favorite_folder(
        saved: &Favorites,
        value: &str,
    ) -> Result<ExternalFolderRef, ExternalError> {
        pick("favorites folder", value, &saved.folders, |folder| &folder.id, |folder| &folder.name)
            .map(|folder| ExternalFolderRef {
                id: Some(folder.id.clone()),
                name: folder.name.clone(),
            })
            .map_err(|refusal| refusal.hint("List them with: sona work favorites"))
    }

    pub(super) async fn favorite(
        &self,
        args: &FavoriteArgs,
        on: bool,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let target = self.favorite_target(&board, &args.target)?;
        let action = if on {
            LifecycleAction::Favorited
        } else {
            LifecycleAction::Unfavorited
        };
        let mut answer = self.change(action, args.dry_run);
        let operation_id = args.write.fresh_operation_id();
        let replayed = self.ran(&operation_id)?;
        let already = self.saved_favorites()?.contains(target.kind, &target.id);
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
            .await
            .map_err(|message| self.write_refusal(message, &operation_id, "The favorite"))?;
        answer.replayed = replayed;
        Ok(WorkAnswer::Lifecycle(Box::new(answer)))
    }

    pub(super) async fn favorite_folder_save(
        &self,
        args: &FavoriteFolderSaveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let saved = self.saved_favorites()?;
        let existing = args
            .folder
            .as_deref()
            .map(|folder| Self::favorite_folder(&saved, folder))
            .transpose()?;
        let name = args.name.as_deref().map(str::trim);
        if name.is_some_and(str::is_empty) {
            return Err(ExternalError::invalid("Give the folder a name."));
        }
        let index = args.position.map(|position| usize::try_from(position - 1).unwrap_or(usize::MAX));
        let mut answer = self.change(LifecycleAction::FolderSaved, args.dry_run);
        let operation_id = args.write.fresh_operation_id();
        let replayed = self.ran(&operation_id)?;
        answer.folder = Some(ExternalFolderRef {
            id: existing.as_ref().and_then(|folder| folder.id.clone()),
            name: name
                .map(str::to_owned)
                .or_else(|| existing.as_ref().map(|folder| folder.name.clone()))
                .unwrap_or_default(),
        });
        answer.operation_id = Some(operation_id.clone());
        if args.dry_run {
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        self.applies("changing favorites")?;
        let folder = favorites::save_folder_as(
            self.app,
            &operation_id,
            &FolderSave {
                id: existing.and_then(|folder| folder.id),
                name: name.map(str::to_owned),
                index,
            },
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The folder"))?;
        answer.position = self.saved_favorites()?.top().iter().position(|entry| {
            matches!(entry, FavoriteEntry::Folder { id } if *id == folder.id)
        }).map(|at| at + 1);
        answer.folder = Some(ExternalFolderRef {
            id: Some(folder.id),
            name: folder.name,
        });
        answer.replayed = replayed;
        Ok(WorkAnswer::Lifecycle(Box::new(answer)))
    }

    pub(super) async fn favorite_folder_delete(
        &self,
        args: &FavoriteFolderDeleteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let folder = Self::favorite_folder(&self.saved_favorites()?, &args.folder)?;
        let mut answer = self.change(LifecycleAction::FolderDeleted, args.dry_run);
        let operation_id = args.write.fresh_operation_id();
        let replayed = self.ran(&operation_id)?;
        answer.folder = Some(folder.clone());
        answer.operation_id = Some(operation_id.clone());
        if args.dry_run {
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        self.applies("changing favorites")?;
        favorites::delete_folder_as(self.app, &operation_id, folder.id.as_deref().unwrap_or_default())
            .map_err(|message| self.write_refusal(message, &operation_id, "The folder"))?;
        answer.replayed = replayed;
        Ok(WorkAnswer::Lifecycle(Box::new(answer)))
    }

    pub(super) async fn favorite_move(
        &self,
        args: &FavoriteMoveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let target = self.favorite_target(&board, &args.target)?;
        let saved = self.saved_favorites()?;
        let Some(current) = saved
            .favorites
            .iter()
            .find(|favorite| favorite.kind == target.kind && favorite.target_id == target.id)
        else {
            return Err(ExternalError::invalid(format!(
                "{} is not a favorite. Add it with: sona work favorite",
                target.name
            )));
        };
        let into = match (&args.into, args.top) {
            (Some(folder), _) => Some(Self::favorite_folder(&saved, folder)?),
            (None, true) => None,
            (None, false) => current
                .folder_id
                .as_deref()
                .map(|folder| Self::favorite_folder(&saved, folder))
                .transpose()?,
        };
        let into_id = into.as_ref().and_then(|folder| folder.id.clone());
        let entry = FavoriteEntry::Favorite {
            kind: target.kind,
            id: target.id.clone(),
        };
        let mut others: Vec<FavoriteEntry> = match &into_id {
            Some(folder) => saved
                .inside(folder)
                .map(|favorite| FavoriteEntry::Favorite {
                    kind: favorite.kind,
                    id: favorite.target_id.clone(),
                })
                .collect(),
            None => saved.top(),
        };
        let before = others.iter().position(|kept| *kept == entry);
        others.retain(|kept| *kept != entry);
        let index = args
            .position
            .map(|position| usize::try_from(position - 1).unwrap_or(usize::MAX));
        let at = index.unwrap_or(others.len()).min(others.len());
        let mut answer = self.change(LifecycleAction::FavoriteMoved, args.dry_run);
        let operation_id = args.write.fresh_operation_id();
        let replayed = self.ran(&operation_id)?;
        answer.favorite = Some(target.clone());
        answer.folder = into;
        answer.position = Some(at + 1);
        if before == Some(at) && !replayed {
            answer.skipped.push(ExternalLifecycleSkip {
                id: target.handle.clone().unwrap_or_else(|| target.name.clone()),
                reason: "already there".to_owned(),
            });
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        answer.operation_id = Some(operation_id.clone());
        if args.dry_run {
            return Ok(WorkAnswer::Lifecycle(Box::new(answer)));
        }
        self.applies("changing favorites")?;
        favorites::move_as(
            self.app,
            &operation_id,
            &FavoriteMove {
                entry,
                into: into_id,
                index: Some(at),
            },
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The favorite"))?;
        answer.replayed = replayed;
        Ok(WorkAnswer::Lifecycle(Box::new(answer)))
    }

    pub(super) async fn favorites(&self) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let views = self.all_views()?;
        let saved = self.saved_favorites()?;
        let named = |favorite: &favorites::Favorite| match favorite.kind {
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
            FavoriteKind::View => views
                .iter()
                .find(|view| view.id == favorite.target_id)
                .map(|view| ExternalFavorite {
                    kind: favorite.kind,
                    id: view.id.clone(),
                    name: view.name.clone(),
                    handle: None,
                }),
            FavoriteKind::Person => board
                .people
                .iter()
                .find(|person| person.id == favorite.target_id)
                .map(|person| ExternalFavorite {
                    kind: favorite.kind,
                    id: person.id.clone(),
                    name: person.name.clone(),
                    handle: None,
                }),
        };
        let favorites = saved
            .top()
            .into_iter()
            .filter_map(|entry| match entry {
                FavoriteEntry::Favorite { kind, id } => saved
                    .favorites
                    .iter()
                    .find(|favorite| favorite.kind == kind && favorite.target_id == id)
                    .and_then(named)
                    .map(ExternalFavoriteEntry::Favorite),
                FavoriteEntry::Folder { id } => {
                    let folder = saved.folders.iter().find(|folder| folder.id == id)?;
                    Some(ExternalFavoriteEntry::Folder(ExternalFavoriteFolder {
                        kind: "folder",
                        id: folder.id.clone(),
                        name: folder.name.clone(),
                        favorites: saved.inside(&folder.id).filter_map(named).collect(),
                    }))
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
        let task = match &notification.item_id {
            Some(id) => Some(item_ref(&board.row(id)?.item)),
            None => None,
        };
        Some(ExternalNotification {
            task,
            by: author(Some(notification.actor), notification.agent.as_deref()),
            id: notification.id,
            kind: notification.kind,
            comment_id: notification.comment_id,
            field: notification.field,
            automation: notification
                .automation
                .map(|automation| ExternalNoticeAutomation {
                    id: automation.id,
                    run_id: automation.run_id,
                    name: automation.name,
                    error: automation.error,
                }),
            from: notification.from,
            to: notification.to,
            created_at_ms: notification.created_at_ms,
            read: notification.read_at_ms.is_some(),
            snoozed_until_ms: notification.snoozed_until_ms,
            archived: notification.archived_at_ms.is_some(),
            digest: notification.digest.map(|mut digest| {
                for project in &mut digest.projects {
                    project.done.retain(|task| board.row(&task.id).is_some());
                    for risk in &mut project.risks {
                        if let DigestRisk::Overdue { tasks } = risk {
                            tasks.retain(|task| board.row(&task.id).is_some());
                        }
                    }
                    project.risks.retain(|risk| {
                        !matches!(risk, DigestRisk::Overdue { tasks } if tasks.is_empty())
                    });
                }
                digest
            }),
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
        let wanted_read = match how {
            Mark::Read => false,
            Mark::Archive => true,
            _ if args.all => {
                return Err(ExternalError::invalid(
                    "--all goes with --mark read or --mark archive.",
                ))
            }
            _ => false,
        };
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
                    notice.read == wanted_read
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
