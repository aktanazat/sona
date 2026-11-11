//! `sona work` verbs for comments on a task and for sub-tasks.

use super::{
    handle, now_ms, refusal, status_text, task_ref, task_ref_of, Board, ExternalFieldChange,
    ExternalWorkChange, ExternalWorkPlanned, ExternalWorkPreview, ExternalWorkRef, Priority,
    TaskRef, Tracker, WorkAction, WorkAnswer, WriteArgs,
};
use crate::planning;
use crate::planning::work::agents;
use crate::planning::work::comments::{self, Author};
use crate::planning::work::types::{WorkActor, WorkComment, WorkItemInput, WorkOwner};
use crate::planning::work::{self};
use crate::query::external::{local_time, ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use chrono::{Local, NaiveDate};
use clap::{Args, Subcommand};
use serde::Serialize;
use std::fmt::Write as _;
use uuid::Uuid;

/// A task's parent as a caller names it: a task, or none.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParentArg {
    None,
    Task(TaskRef),
}

pub(super) fn parent_arg(value: &str) -> Result<ParentArg, String> {
    match value.trim().to_lowercase().as_str() {
        "none" | "null" => Ok(ParentArg::None),
        _ => task_ref(value).map(ParentArg::Task),
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CommentCommand {
    #[command(subcommand)]
    pub verb: CommentVerb,
}

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum CommentVerb {
    /// Comment on a task, or reply in a thread with --reply-to.
    Add(CommentAddArgs),
    /// Change a comment you wrote.
    Edit(CommentEditArgs),
    /// Delete a comment you wrote. Replies under it stay.
    Delete(CommentDeleteArgs),
    /// Resolve the thread a comment is in, or reopen it with --reopen.
    Resolve(CommentResolveArgs),
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CommentAddArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// The comment, in Markdown.
    #[arg(long)]
    pub body: String,
    /// A comment's id: the reply joins that comment's thread.
    #[arg(long, value_name = "COMMENT")]
    pub reply_to: Option<Uuid>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

impl CommentAddArgs {
    /// The caller's id, or one derived from the task, the thread, the
    /// trimmed text and `day`: a retry that day posts the comment once.
    fn operation_id(&self, task_id: &str, day: NaiveDate) -> Result<String, ExternalError> {
        self.write.operation_id(
            "comment",
            &(task_id, self.reply_to, self.body.trim(), day.to_string()),
        )
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CommentEditArgs {
    #[arg(value_name = "COMMENT")]
    pub comment: Uuid,
    #[arg(long)]
    pub body: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CommentDeleteArgs {
    #[arg(value_name = "COMMENT")]
    pub comment: Uuid,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CommentResolveArgs {
    #[arg(value_name = "COMMENT")]
    pub comment: Uuid,
    /// Reopen the thread instead.
    #[arg(long)]
    pub reopen: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CommentsArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// Threads to print, at most 100.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub limit: Option<u32>,
    /// The `next_cursor` of the page before.
    #[arg(long, value_name = "COMMENT")]
    pub cursor: Option<Uuid>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SubtaskArgs {
    /// The parent: S-12, 12, or its id.
    #[arg(value_parser = task_ref, value_name = "PARENT")]
    pub parent: TaskRef,
    /// One title per sub-task.
    #[arg(required = true, value_name = "TITLE")]
    pub titles: Vec<String>,
    /// me, nobody, or a person's name or id. Left out: you when the parent is
    /// yours, else the owner its sub-tasks share.
    #[arg(long)]
    pub owner: Option<String>,
    /// Left out: the parent's.
    #[arg(long, value_enum)]
    pub priority: Option<Priority>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalCommentAuthor {
    pub kind: WorkActor,
    /// The agent's name, for an agent.
    pub name: Option<String>,
}

/// One comment. A thread's first comment carries its replies and whether
/// the thread is resolved.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkComment {
    pub id: String,
    pub task_id: String,
    /// The thread's first comment, for a reply.
    pub reply_to: Option<String>,
    pub author: ExternalCommentAuthor,
    /// Markdown; none once deleted.
    pub body: Option<String>,
    pub created_at_ms: i64,
    pub edited_at_ms: Option<i64>,
    pub resolved_at_ms: Option<i64>,
    pub deleted: bool,
    pub replies: Vec<ExternalWorkComment>,
}

pub(super) fn project_comment(comment: WorkComment) -> ExternalWorkComment {
    ExternalWorkComment {
        id: comment.id,
        task_id: comment.item_id,
        reply_to: comment.parent_id,
        author: ExternalCommentAuthor {
            kind: comment.author.kind,
            name: comment.author.name,
        },
        body: comment.body,
        created_at_ms: comment.created_at_ms,
        edited_at_ms: comment.edited_at_ms,
        resolved_at_ms: comment.resolved_at_ms,
        deleted: comment.deleted,
        replies: comment.replies.into_iter().map(project_comment).collect(),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkComments {
    pub schema_version: u32,
    pub task: ExternalWorkRef,
    /// Threads oldest first.
    pub threads: Vec<ExternalWorkComment>,
    /// Pass as --cursor for the next page; none on the last.
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentAction {
    Added,
    Edited,
    Deleted,
    Resolved,
    Reopened,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkCommentChange {
    pub schema_version: u32,
    pub action: CommentAction,
    pub dry_run: bool,
    /// Absent on a dry run that names none.
    pub operation_id: Option<String>,
    pub task: ExternalWorkRef,
    /// The comment as it is now, or for a dry run as it would be; for
    /// resolve, the thread. Absent on a dry run of add.
    pub comment: Option<ExternalWorkComment>,
    /// True when this answered from an earlier run of the same operation.
    pub replayed: bool,
}

fn missing_comment(id: &Uuid) -> ExternalError {
    ExternalError::new(ExternalErrorCode::NotFound, format!("No comment {id}."))
}

impl Tracker<'_> {
    fn author<'a>(write: &'a WriteArgs) -> Author<'a> {
        Author {
            actor: WorkActor::Agent,
            agent: write.agent.as_deref(),
        }
    }

    /// The comment and the task it is on, when that task is one the caller
    /// may see.
    fn comment_on<'b>(
        &self,
        board: &'b Board,
        id: &Uuid,
    ) -> Result<(WorkComment, &'b super::WorkItemRow), ExternalError> {
        let found = planning::history(self.history, |conn| comments::thread(conn, &id.to_string()))
            .map_err(|_| missing_comment(id))?;
        let row = board
            .row(&found.item_id)
            .ok_or_else(|| missing_comment(id))?;
        let comment = if found.id == id.to_string() {
            found
        } else {
            found
                .replies
                .into_iter()
                .find(|reply| reply.id == id.to_string())
                .ok_or_else(|| missing_comment(id))?
        };
        Ok((comment, row))
    }

    fn comment_change(
        action: CommentAction,
        dry_run: bool,
        operation_id: Option<String>,
        row: &super::WorkItemRow,
        comment: Option<WorkComment>,
        replayed: bool,
    ) -> WorkAnswer {
        WorkAnswer::Comment(Box::new(ExternalWorkCommentChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run,
            operation_id,
            task: task_ref_of(row),
            comment: comment.map(project_comment),
            replayed,
        }))
    }

    pub(super) async fn comments(&self, args: &CommentsArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let limit = args
            .limit
            .and_then(|limit| usize::try_from(limit).ok())
            .unwrap_or(comments::PAGE);
        let cursor = args.cursor.map(|cursor| cursor.to_string());
        let page = planning::history(self.history, |conn| {
            comments::page(conn, &row.item.id, limit, cursor.as_deref())
        })
        .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::Comments(ExternalWorkComments {
            schema_version: QUERY_SCHEMA_VERSION,
            task: task_ref_of(row),
            threads: page.threads.into_iter().map(project_comment).collect(),
            next_cursor: page.next_cursor,
        }))
    }

    pub(super) async fn comment_add(
        &self,
        args: &CommentAddArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        if let Some(reply_to) = &args.reply_to {
            let (to, _) = self.comment_on(&board, reply_to)?;
            if to.item_id != row.item.id {
                return Err(ExternalError::invalid(format!(
                    "Comment {reply_to} is on another task."
                )));
            }
        }
        let operation_id = args.operation_id(&row.item.id, Local::now().date_naive())?;
        if args.dry_run {
            return Ok(Self::comment_change(
                CommentAction::Added,
                true,
                Some(operation_id),
                row,
                None,
                false,
            ));
        }
        let started_ms = now_ms();
        let reply_to = args.reply_to.map(|id| id.to_string());
        let comment = work::comment_add_as(
            self.app,
            Self::author(&args.write),
            &operation_id,
            &row.item.id,
            &args.body,
            reply_to.as_deref(),
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The task"))?;
        let replayed = comment.created_at_ms < started_ms;
        Ok(Self::comment_change(
            CommentAction::Added,
            false,
            Some(operation_id),
            row,
            Some(comment),
            replayed,
        ))
    }

    pub(super) async fn comment_edit(
        &self,
        args: &CommentEditArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (comment, row) = self.comment_on(&board, &args.comment)?;
        let operation_id = args.write.operation_id(
            "comment-edit",
            &(&comment.id, comment.edited_at_ms, args.body.trim()),
        )?;
        if args.dry_run {
            let edited = WorkComment {
                body: Some(args.body.trim().to_owned()),
                ..comment
            };
            return Ok(Self::comment_change(
                CommentAction::Edited,
                true,
                Some(operation_id),
                row,
                Some(edited),
                false,
            ));
        }
        let comment = work::comment_edit_as(
            self.app,
            Self::author(&args.write),
            &operation_id,
            &comment.id,
            &args.body,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The comment"))?;
        Ok(Self::comment_change(
            CommentAction::Edited,
            false,
            Some(operation_id),
            row,
            Some(comment),
            false,
        ))
    }

    pub(super) async fn comment_delete(
        &self,
        args: &CommentDeleteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (comment, row) = self.comment_on(&board, &args.comment)?;
        let operation_id = args.write.operation_id("comment-delete", &comment.id)?;
        if args.dry_run {
            return Ok(Self::comment_change(
                CommentAction::Deleted,
                true,
                Some(operation_id),
                row,
                Some(comment),
                false,
            ));
        }
        let comment = work::comment_delete_as(
            self.app,
            Self::author(&args.write),
            &operation_id,
            &comment.id,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The comment"))?;
        Ok(Self::comment_change(
            CommentAction::Deleted,
            false,
            Some(operation_id),
            row,
            Some(comment),
            false,
        ))
    }

    pub(super) async fn comment_resolve(
        &self,
        args: &CommentResolveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let action = if args.reopen {
            CommentAction::Reopened
        } else {
            CommentAction::Resolved
        };
        let board = self.board().await?;
        let (comment, row) = self.comment_on(&board, &args.comment)?;
        let thread = planning::history(self.history, |conn| comments::thread(conn, &comment.id))
            .map_err(|message| refusal(self.history, message))?;
        let operation_id = args.write.operation_id(
            "comment-resolve",
            &(&thread.id, thread.resolved_at_ms, args.reopen),
        )?;
        if args.dry_run {
            let would = WorkComment {
                resolved_at_ms: if args.reopen {
                    None
                } else {
                    thread.resolved_at_ms.or(Some(now_ms()))
                },
                ..thread
            };
            return Ok(Self::comment_change(
                action,
                true,
                Some(operation_id),
                row,
                Some(would),
                false,
            ));
        }
        self.applies(if args.reopen {
            "reopening a thread"
        } else {
            "resolving a thread"
        })?;
        let thread = work::comment_resolve_as(
            self.app,
            WorkActor::Agent,
            &operation_id,
            &thread.id,
            !args.reopen,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The thread"))?;
        Ok(Self::comment_change(
            action,
            false,
            Some(operation_id),
            row,
            Some(thread),
            false,
        ))
    }

    pub(super) async fn subtask(&self, args: &SubtaskArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let parent = board.task(&args.parent)?;
        let owner = args
            .owner
            .as_deref()
            .map(|owner| board.owner(owner))
            .transpose()?;
        let inputs: Vec<WorkItemInput> = args
            .titles
            .iter()
            .map(|title| WorkItemInput {
                title: title.trim().to_owned(),
                details: None,
                status: None,
                priority: args.priority.map(Priority::value),
                project_id: None,
                milestone_id: None,
                owner: owner.clone(),
                due_at_ms: None,
                label_ids: None,
                recurrence: None,
            })
            .collect();
        if inputs.iter().any(|input| input.title.is_empty()) {
            return Err(ExternalError::invalid("A sub-task needs a title."));
        }
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let operation_id = args
            .write
            .operation_id("subtask", &(&parent.item.id, &inputs, today))?;
        if args.dry_run {
            let inherited = owner
                .clone()
                .unwrap_or_else(|| board.inherited_owner(parent));
            let changes = inputs
                .iter()
                .map(|input| ExternalWorkPlanned {
                    id: String::new(),
                    handle: "new".to_owned(),
                    title: input.title.clone(),
                    fields: vec![
                        ExternalFieldChange {
                            field: "parent",
                            from: "none".to_owned(),
                            to: board.task_text(&parent.item.id),
                        },
                        ExternalFieldChange {
                            field: "owner",
                            from: "nobody".to_owned(),
                            to: board.owner_text(&inherited),
                        },
                        ExternalFieldChange {
                            field: "priority",
                            from: Priority::of(0).text().to_owned(),
                            to: Priority::of(input.priority.unwrap_or(parent.item.priority))
                                .text()
                                .to_owned(),
                        },
                        ExternalFieldChange {
                            field: "project",
                            from: "none".to_owned(),
                            to: parent
                                .item
                                .project_id
                                .as_deref()
                                .map_or_else(|| "none".to_owned(), |id| board.project_name(id)),
                        },
                    ],
                    commitment: None,
                    skipped: None,
                })
                .collect();
            return Ok(WorkAnswer::Preview(ExternalWorkPreview {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                action: WorkAction::Created,
                operation_id: Some(operation_id),
                changes,
            }));
        }
        let started_ms = now_ms();
        let items = agents::subtask_create_as(
            self.app,
            &args.write.agent(),
            &operation_id,
            &parent.item.id,
            &inputs,
        )
        .await
        .map_err(|message| {
            self.write_refusal(message, &operation_id, &handle(parent.item.number))
        })?;
        let replayed = items.iter().all(|item| item.created_at_ms < started_ms);
        Ok(WorkAnswer::Change(ExternalWorkChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: WorkAction::Created,
            operation_id: Some(operation_id),
            tasks: items
                .into_iter()
                .map(|item| board.project_item(item))
                .collect(),
            skipped: Vec::new(),
            proposed: Vec::new(),
            replayed,
            evidence_url: None,
        }))
    }
}

impl Board {
    /// The owner a new sub-task of `parent` takes when none is given, as
    /// the tracker decides it, from the sub-tasks the caller can see.
    fn inherited_owner(&self, parent: &super::WorkItemRow) -> WorkOwner {
        if parent.item.owner_is_self {
            return WorkOwner::Myself;
        }
        let mut owners = self
            .rows
            .iter()
            .filter(|row| {
                row.item.parent_id.as_deref() == Some(parent.item.id.as_str())
                    && row.item.archived_at_ms.is_none()
            })
            .map(|row| row.item.owner());
        let Some(first) = owners.next() else {
            return WorkOwner::Nobody;
        };
        if owners.all(|owner| owner == first) {
            first
        } else {
            WorkOwner::Nobody
        }
    }

    /// Whether `id` is `parent_id` or sits above it.
    pub(super) fn above(&self, id: &str, parent_id: &str) -> bool {
        let mut at = Some(parent_id);
        let mut steps = 0;
        while let Some(current) = at {
            if current == id {
                return true;
            }
            steps += 1;
            if steps > self.rows.len() {
                return false;
            }
            at = self
                .row(current)
                .and_then(|row| row.item.parent_id.as_deref());
        }
        false
    }
}

fn comment_text(text: &mut String, comment: &ExternalWorkComment, indent: &str) {
    let who = match comment.author.kind {
        WorkActor::Me => "you",
        WorkActor::Sona => "Sona",
        WorkActor::Agent => comment.author.name.as_deref().unwrap_or("agent"),
    };
    let mut notes = Vec::new();
    if comment.edited_at_ms.is_some() {
        notes.push("edited");
    }
    if comment.resolved_at_ms.is_some() {
        notes.push("resolved");
    }
    let notes = if notes.is_empty() {
        String::new()
    } else {
        format!(" ({})", notes.join(", "))
    };
    let _ = writeln!(
        text,
        "{indent}{}  {who}  {}{notes}  id {}",
        local_time(comment.created_at_ms),
        if comment.deleted { "deleted" } else { "" },
        comment.id
    );
    for line in comment.body.as_deref().unwrap_or_default().lines() {
        let _ = writeln!(text, "{indent}  {line}");
    }
    for reply in &comment.replies {
        comment_text(text, reply, &format!("{indent}    "));
    }
}

pub(super) fn threads_text(text: &mut String, threads: &[ExternalWorkComment]) {
    for thread in threads {
        comment_text(text, thread, "  ");
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::Comments(page) => {
            let _ = writeln!(
                text,
                "{} {} ({})",
                page.task.handle,
                page.task.title,
                status_text(page.task.status)
            );
            if page.threads.is_empty() {
                text.push_str("No comments.\n");
            }
            threads_text(text, &page.threads);
            if let Some(cursor) = &page.next_cursor {
                let _ = writeln!(text, "More: --cursor {cursor}");
            }
        }
        WorkAnswer::Comment(change) => {
            let verb = match (change.action, change.dry_run) {
                (CommentAction::Added, false) => "Commented on",
                (CommentAction::Added, true) => "Would comment on",
                (CommentAction::Edited, false) => "Edited a comment on",
                (CommentAction::Edited, true) => "Would edit a comment on",
                (CommentAction::Deleted, false) => "Deleted a comment on",
                (CommentAction::Deleted, true) => "Would delete a comment on",
                (CommentAction::Resolved, false) => "Resolved a thread on",
                (CommentAction::Resolved, true) => "Would resolve a thread on",
                (CommentAction::Reopened, false) => "Reopened a thread on",
                (CommentAction::Reopened, true) => "Would reopen a thread on",
            };
            let _ = writeln!(
                text,
                "{verb} {} {}{}",
                change.task.handle,
                change.task.title,
                if change.replayed {
                    " (already done)"
                } else {
                    ""
                }
            );
            if let Some(comment) = &change.comment {
                comment_text(text, comment, "  ");
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::{WorkCommand, WorkVerb};
    use super::{CommentAddArgs, CommentCommand, CommentVerb};
    use crate::cli::{CliArgs, Command};
    use chrono::NaiveDate;
    use clap::Parser;

    const TASK: &str = "5b0c6a8e-3d1f-4c2a-9e7b-0a1b2c3d4e5f";

    fn comment(body: &str) -> CommentAddArgs {
        match CliArgs::try_parse_from(["sona", "work", "comment", "add", "S-1", "--body", body]) {
            Ok(CliArgs {
                command:
                    Some(Command::Work(WorkCommand {
                        verb:
                            WorkVerb::Comment(CommentCommand {
                                verb: CommentVerb::Add(args),
                            }),
                        ..
                    })),
                ..
            }) => args,
            other => panic!("`work comment add` parsed as {other:?}"),
        }
    }

    #[test]
    fn a_retried_comment_is_posted_under_the_same_id() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 11).expect("a day");
        let first = comment("Venue is held until Friday.").operation_id(TASK, day);
        let retry = comment("Venue is held until Friday. ").operation_id(TASK, day);
        let other = comment("Venue is held until Monday.").operation_id(TASK, day);
        let first = first.expect("an id");
        assert_eq!(first, retry.expect("an id"));
        assert_ne!(first, other.expect("an id"));
    }
}
