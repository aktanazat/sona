//! `sona work` verbs for files, images and links on a task.

use super::{
    now_ms, refusal, status_text, task_ref, task_ref_of, ExternalWorkRef, TaskRef, Tracker,
    WorkAnswer, WriteArgs,
};
use crate::planning;
use crate::planning::work::attachments::{self, Naming};
use crate::planning::work::comments::Author;
use crate::planning::work::types::{WorkActor, WorkAttachment, WorkAttachmentKind};
use crate::planning::work::{self, AttachSource};
use crate::query::external::{local_time, ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use clap::Args;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use uuid::Uuid;

/// What `attach` names: a link, or a file on this Mac.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttachTarget {
    Link(String),
    File(PathBuf),
}

fn attach_target(value: &str) -> Result<AttachTarget, String> {
    let value = value.trim();
    if value.contains("://") || value.to_ascii_lowercase().starts_with("mailto:") {
        return Ok(AttachTarget::Link(value.to_owned()));
    }
    std::path::absolute(value)
        .map(AttachTarget::File)
        .map_err(|error| error.to_string())
}

fn meta_pair(value: &str) -> Result<(String, String), String> {
    let (key, value) = value
        .split_once('=')
        .ok_or_else(|| "Write it as KEY=VALUE.".to_owned())?;
    let key = key.trim();
    if key.is_empty() {
        return Err("The key is empty.".to_owned());
    }
    Ok((key.to_owned(), value.to_owned()))
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct AttachArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// A file to copy in, or an http(s), mailto or sona link.
    #[arg(value_parser = attach_target, value_name = "PATH|URL")]
    pub target: AttachTarget,
    /// Shown instead of the file's name or the link.
    #[arg(long)]
    pub title: Option<String>,
    /// A second line under the title.
    #[arg(long)]
    pub subtitle: Option<String>,
    /// A key-value fact to keep on it, as KEY=VALUE; repeat for more. On a
    /// file or link already there, these replace its metadata.
    #[arg(long = "meta", value_parser = meta_pair, value_name = "KEY=VALUE")]
    pub meta: Vec<(String, String)>,
    /// The id of a comment on the task to put the file or link on, from
    /// `comments`.
    #[arg(long, value_name = "COMMENT")]
    pub comment: Option<Uuid>,
    /// Check the file or link and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct AttachmentsArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DetachArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// The attachment's id, from `attachments`.
    #[arg(value_name = "ID")]
    pub attachment: Uuid,
    /// Print what would go and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalAttachmentAuthor {
    pub kind: WorkActor,
    /// The agent's name, for an agent.
    pub name: Option<String>,
}

/// A file, image or link on a task. A file's `path` is Sona's own copy.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkAttachment {
    pub id: String,
    pub task_id: String,
    pub kind: WorkAttachmentKind,
    pub title: String,
    pub subtitle: Option<String>,
    pub url: Option<String>,
    pub path: Option<String>,
    pub original_name: Option<String>,
    pub mime: Option<String>,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub created_at_ms: i64,
    pub created_by: ExternalAttachmentAuthor,
    /// Key-value facts kept on it, by key.
    pub metadata: BTreeMap<String, String>,
    /// The comment it is on; absent for one on the task itself.
    pub comment_id: Option<String>,
}

pub(super) fn project_attachment(attachment: WorkAttachment) -> ExternalWorkAttachment {
    ExternalWorkAttachment {
        id: attachment.id,
        task_id: attachment.item_id,
        kind: attachment.kind,
        title: attachment.title,
        subtitle: attachment.subtitle,
        url: attachment.url,
        path: attachment.path,
        original_name: attachment.original_name,
        mime: attachment.mime,
        size: attachment.size,
        sha256: attachment.sha256,
        created_at_ms: attachment.created_at_ms,
        created_by: ExternalAttachmentAuthor {
            kind: attachment.created_by.kind,
            name: attachment.created_by.name,
        },
        metadata: attachment.metadata,
        comment_id: attachment.comment_id,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkAttachments {
    pub schema_version: u32,
    pub task: ExternalWorkRef,
    /// Oldest first.
    pub attachments: Vec<ExternalWorkAttachment>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentAction {
    Attached,
    Detached,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkAttachmentChange {
    pub schema_version: u32,
    pub action: AttachmentAction,
    pub dry_run: bool,
    /// Absent on a dry run that names none.
    pub operation_id: Option<String>,
    pub task: ExternalWorkRef,
    /// The attachment as it is now, or as it was for a detach. Absent on a
    /// dry run of attach.
    pub attachment: Option<ExternalWorkAttachment>,
    /// What a dry run of attach would add: the file's name or the link.
    pub would_attach: Option<String>,
    /// True when this answered from an earlier run, or the task already
    /// held the same file or link.
    pub replayed: bool,
}

fn missing(id: &Uuid, task: &ExternalWorkRef) -> ExternalError {
    ExternalError::new(
        ExternalErrorCode::NotFound,
        format!("No attachment {id} on {}.", task.handle),
    )
}

impl Tracker<'_> {
    fn attacher(write: &WriteArgs) -> Author<'_> {
        Author {
            actor: WorkActor::Agent,
            agent: write.agent.as_deref(),
        }
    }

    fn attachments_on(&self, task_id: &str) -> Result<Vec<WorkAttachment>, ExternalError> {
        work::attachments_of(self.app, task_id).map_err(|message| refusal(self.history, message))
    }

    pub(super) async fn attachments(
        &self,
        args: &AttachmentsArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let found = self.attachments_on(&row.item.id)?;
        Ok(WorkAnswer::Attachments(ExternalWorkAttachments {
            schema_version: QUERY_SCHEMA_VERSION,
            task: task_ref_of(row),
            attachments: found.into_iter().map(project_attachment).collect(),
        }))
    }

    pub(super) async fn attach(&self, args: &AttachArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let task = task_ref_of(row);
        let metadata: BTreeMap<String, String> = args.meta.iter().cloned().collect();
        let comment_id = args.comment.map(|id| id.to_string());
        let naming = Naming {
            title: args.title.as_deref(),
            subtitle: args.subtitle.as_deref(),
            metadata: Some(&metadata),
            comment_id: comment_id.as_deref(),
        };
        if args.dry_run {
            let would = match &args.target {
                AttachTarget::Link(url) => attachments::link(url)
                    .map_err(|error| refusal(self.history, planning::failure(error)))?,
                AttachTarget::File(path) => {
                    attachments::check_file(path)
                        .map_err(|error| refusal(self.history, planning::failure(error)))?;
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                }
            };
            return Ok(WorkAnswer::Attachment(Box::new(
                ExternalWorkAttachmentChange {
                    schema_version: QUERY_SCHEMA_VERSION,
                    action: AttachmentAction::Attached,
                    dry_run: true,
                    operation_id: args.write.operation_id.map(|id| id.to_string()),
                    task,
                    attachment: None,
                    would_attach: Some(would),
                    replayed: false,
                },
            )));
        }
        let operation_id = args.write.fresh_operation_id();
        let started_ms = now_ms();
        let author = Self::attacher(&args.write);
        let attached = match &args.target {
            AttachTarget::Link(url) => {
                work::attach_link_as(self.app, author, &operation_id, &row.item.id, url, naming)
            }
            AttachTarget::File(path) => work::attach_file_as(
                self.app,
                author,
                &operation_id,
                &row.item.id,
                AttachSource::Path(path),
                naming,
            ),
        }
        .map_err(|message| self.write_refusal(message, &operation_id, "The task"))?;
        let replayed = attached.created_at_ms < started_ms;
        Ok(WorkAnswer::Attachment(Box::new(
            ExternalWorkAttachmentChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action: AttachmentAction::Attached,
                dry_run: false,
                operation_id: Some(operation_id),
                task,
                attachment: Some(project_attachment(attached)),
                would_attach: None,
                replayed,
            },
        )))
    }

    pub(super) async fn detach(&self, args: &DetachArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let task = task_ref_of(row);
        let id = args.attachment.to_string();
        let found = self
            .attachments_on(&row.item.id)?
            .into_iter()
            .find(|attachment| attachment.id == id);
        let operation_id = args.write.operation_id("detach", &(&row.item.id, &id))?;
        if args.dry_run {
            let found = found.ok_or_else(|| missing(&args.attachment, &task))?;
            return Ok(WorkAnswer::Attachment(Box::new(
                ExternalWorkAttachmentChange {
                    schema_version: QUERY_SCHEMA_VERSION,
                    action: AttachmentAction::Detached,
                    dry_run: true,
                    operation_id: Some(operation_id),
                    task,
                    attachment: Some(project_attachment(found)),
                    would_attach: None,
                    replayed: false,
                },
            )));
        }
        let was_there = found.is_some();
        let removed = work::detach_as(
            self.app,
            Self::attacher(&args.write),
            &operation_id,
            &id,
            Some(&row.item.id),
        )
        .map_err(|message| {
            if was_there {
                self.write_refusal(message, &operation_id, "The attachment")
            } else {
                missing(&args.attachment, &task)
            }
        })?;
        Ok(WorkAnswer::Attachment(Box::new(
            ExternalWorkAttachmentChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action: AttachmentAction::Detached,
                dry_run: false,
                operation_id: Some(operation_id),
                task,
                attachment: Some(project_attachment(removed)),
                would_attach: None,
                replayed: !was_there,
            },
        )))
    }
}

fn size_text(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{} KB", bytes.div_ceil(KB))
    } else {
        format!("{bytes} B")
    }
}

fn attachment_text(text: &mut String, attachment: &ExternalWorkAttachment) {
    let kind = match attachment.kind {
        WorkAttachmentKind::File => "file",
        WorkAttachmentKind::Image => "image",
        WorkAttachmentKind::Link => "link",
    };
    let who = match attachment.created_by.kind {
        WorkActor::Me => "you",
        WorkActor::Sona => "Sona",
        WorkActor::Agent => attachment.created_by.name.as_deref().unwrap_or("agent"),
    };
    let size = attachment
        .size
        .map(|size| format!(", {}", size_text(size)))
        .unwrap_or_default();
    let _ = writeln!(
        text,
        "  {}  ({kind}{size})  {who}  {}  id {}",
        attachment.title,
        local_time(attachment.created_at_ms),
        attachment.id
    );
    if let Some(subtitle) = &attachment.subtitle {
        let _ = writeln!(text, "    {subtitle}");
    }
    if let Some(at) = attachment.url.as_deref().or(attachment.path.as_deref()) {
        let _ = writeln!(text, "    {at}");
    }
    if let Some(comment) = &attachment.comment_id {
        let _ = writeln!(text, "    on comment {comment}");
    }
    for (key, value) in &attachment.metadata {
        let _ = writeln!(text, "    {key}: {value}");
    }
}

pub(super) fn attachments_text(text: &mut String, found: &[ExternalWorkAttachment]) {
    for attachment in found {
        attachment_text(text, attachment);
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::Attachments(list) => {
            let _ = writeln!(
                text,
                "{} {} ({})",
                list.task.handle,
                list.task.title,
                status_text(list.task.status)
            );
            if list.attachments.is_empty() {
                text.push_str("No attachments.\n");
            }
            attachments_text(text, &list.attachments);
        }
        WorkAnswer::Attachment(change) => {
            let verb = match (change.action, change.dry_run, change.replayed) {
                (AttachmentAction::Attached, true, _) => "Would attach to",
                (AttachmentAction::Attached, false, false) => "Attached to",
                (AttachmentAction::Attached, false, true) => "Already attached to",
                (AttachmentAction::Detached, true, _) => "Would remove from",
                (AttachmentAction::Detached, false, false) => "Removed from",
                (AttachmentAction::Detached, false, true) => "Already removed from",
            };
            let _ = writeln!(text, "{verb} {} {}", change.task.handle, change.task.title);
            if let Some(would) = &change.would_attach {
                let _ = writeln!(text, "  {would}");
            }
            if let Some(attachment) = &change.attachment {
                attachment_text(text, attachment);
            }
        }
        _ => {}
    }
}
