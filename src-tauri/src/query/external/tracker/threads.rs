//! `sona work` verbs for comments on a task, a project, a milestone, a
//! meeting or a note, and for sub-tasks.

use super::catalog::pick_milestone;
use super::organize::sent_before;
use super::roadmap::ExternalProjectRef;
use super::{
    handle, now_ms, refusal, status_text, task_ref, task_ref_of, Board, ExternalFieldChange,
    ExternalWorkChange, ExternalWorkPlanned, ExternalWorkPreview, ExternalWorkRef,
    ExternalWorkSkip, Priority, TaskRef, Tracker, WorkAction, WorkAnswer, WriteArgs,
};
use crate::meeting::types::MeetingSessionId;
use crate::planning;
use crate::planning::store as plans;
use crate::planning::work::agents;
use crate::planning::work::comments::{self, Author};
use crate::planning::work::reactions;
use crate::planning::work::types::{
    WorkActor, WorkComment, WorkCommentSubject, WorkItemInput, WorkOwner, WorkPatch, WorkReaction,
    WorkReactionSubject,
};
use crate::planning::work::{self};
use crate::planning::{Milestone, Project};
use crate::query::external::{local_time, review, store, ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use crate::scratchpad::personal::ScratchNoteRole;
use chrono::{Local, NaiveDate};
use clap::{ArgGroup, Args, Subcommand};
use rusqlite::Connection;
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
    /// Comment on a task, a project, a milestone, a meeting or a note, or
    /// reply in a thread with --reply-to.
    Add(CommentAddArgs),
    /// Change a comment you wrote.
    Edit(CommentEditArgs),
    /// Delete a comment you wrote. Replies under it stay.
    Delete(CommentDeleteArgs),
    /// Resolve the thread a comment is in, or reopen it with --reopen.
    Resolve(CommentResolveArgs),
}

/// What the threads are on when it is not a task.
#[derive(Args, Clone, Debug, Default, Eq, PartialEq)]
pub struct CommentOnArgs {
    /// A project's name or id.
    #[arg(long, value_name = "PROJECT")]
    pub project: Option<String>,
    /// With --project, one of its milestones by name or id.
    #[arg(long, requires = "project", value_name = "MILESTONE")]
    pub milestone: Option<String>,
    /// A meeting's id. A meeting kept on this Mac reads as absent.
    #[arg(long, value_name = "MEETING_ID")]
    pub meeting: Option<Uuid>,
    /// A note's id.
    #[arg(long, value_name = "NOTE_ID")]
    pub note: Option<Uuid>,
    /// An initiative's name or id.
    #[arg(long, value_name = "INITIATIVE")]
    pub initiative: Option<String>,
    /// A project update's or an initiative update's id.
    #[arg(long, value_name = "UPDATE_ID")]
    pub update: Option<Uuid>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
#[command(group(ArgGroup::new("on").required(true).args(["task", "project", "meeting", "note", "initiative", "update"])))]
pub struct CommentAddArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: Option<TaskRef>,
    #[command(flatten)]
    pub on: CommentOnArgs,
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
    /// The caller's id, or one derived from what the comment is on, the
    /// thread, the trimmed text and `day`: a retry that day posts the
    /// comment once.
    fn operation_id(&self, subject_id: &str, day: NaiveDate) -> Result<String, ExternalError> {
        self.write.operation_id(
            "comment",
            &(subject_id, self.reply_to, self.body.trim(), day.to_string()),
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
#[command(group(ArgGroup::new("on").required(true).args(["task", "project", "meeting", "note", "initiative", "update"])))]
pub struct CommentsArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: Option<TaskRef>,
    #[command(flatten)]
    pub on: CommentOnArgs,
    /// Threads to print, at most 100.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub limit: Option<u32>,
    /// The `next_cursor` of the page before.
    #[arg(long, value_name = "COMMENT")]
    pub cursor: Option<Uuid>,
    /// Who is reading, as Sona shows agents; a personal document's threads
    /// show only to an agent it is shared with.
    #[arg(long, value_name = "NAME")]
    pub agent: Option<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SubtaskArgs {
    /// The parent: S-12, 12, or its id.
    #[arg(value_parser = task_ref, value_name = "PARENT")]
    pub parent: TaskRef,
    /// One title per sub-task.
    #[arg(
        required_unless_present = "from_checklist",
        conflicts_with = "from_checklist",
        value_name = "TITLE"
    )]
    pub titles: Vec<String>,
    /// Turn the open `- [ ]` lines of the parent's description into
    /// sub-tasks, and take them out of the description.
    #[arg(long)]
    pub from_checklist: bool,
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

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CopyArgs {
    /// The task to copy: S-12, 12, or its id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// Copy its sub-tasks too, each under its parent's copy.
    #[arg(long)]
    pub with_subtasks: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ReactArgs {
    /// A comment's id, or a task: S-12, 12, or its id.
    pub target: String,
    /// One emoji, such as the thumbs up.
    pub emoji: String,
    /// Take your reaction off instead.
    #[arg(long)]
    pub remove: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkReactions {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: String,
    /// `comment` or `task`.
    pub on: WorkReactionSubject,
    pub id: String,
    /// Every reaction on it after this change, each emoji with who used it.
    pub reactions: Vec<WorkReaction>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalCommentAuthor {
    pub kind: WorkActor,
    /// The agent's name, for an agent.
    pub name: Option<String>,
}

/// The id of what a comment is on, under its kind's key: `task_id`,
/// `project_id`, `milestone_id`, `meeting_id`, `note_id`, `initiative_id`
/// or `update_id`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalCommentOnId {
    TaskId(String),
    ProjectId(String),
    MilestoneId(String),
    MeetingId(String),
    NoteId(String),
    InitiativeId(String),
    UpdateId(String),
}

/// One comment. A thread's first comment carries its replies and whether
/// the thread is resolved.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkComment {
    pub id: String,
    #[serde(flatten)]
    pub on: ExternalCommentOnId,
    /// The thread's first comment, for a reply.
    pub reply_to: Option<String>,
    pub author: ExternalCommentAuthor,
    /// Markdown; none once deleted.
    pub body: Option<String>,
    pub created_at_ms: i64,
    pub edited_at_ms: Option<i64>,
    pub resolved_at_ms: Option<i64>,
    pub deleted: bool,
    /// The `sona://task/<id>?comment=<id>` address the app's Copy link gives,
    /// on a task's comment that is not deleted.
    pub link: Option<String>,
    pub replies: Vec<ExternalWorkComment>,
    /// Emoji reactions, each with who put it there.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<WorkReaction>,
}

pub(super) fn project_comment(comment: WorkComment) -> ExternalWorkComment {
    let id = comment.item_id;
    let link = (matches!(comment.subject, WorkCommentSubject::Task) && !comment.deleted)
        .then(|| format!("sona://task/{id}?comment={}", comment.id));
    ExternalWorkComment {
        id: comment.id,
        on: match comment.subject {
            WorkCommentSubject::Task => ExternalCommentOnId::TaskId(id),
            WorkCommentSubject::Project => ExternalCommentOnId::ProjectId(id),
            WorkCommentSubject::Milestone => ExternalCommentOnId::MilestoneId(id),
            WorkCommentSubject::Meeting => ExternalCommentOnId::MeetingId(id),
            WorkCommentSubject::Note => ExternalCommentOnId::NoteId(id),
            WorkCommentSubject::Initiative => ExternalCommentOnId::InitiativeId(id),
            WorkCommentSubject::Update => ExternalCommentOnId::UpdateId(id),
        },
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
        link,
        replies: comment.replies.into_iter().map(project_comment).collect(),
        reactions: comment.reactions,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalCommentSubjectRef {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalCommentMilestoneRef {
    pub id: String,
    pub name: String,
    pub project: ExternalProjectRef,
}

/// An update and what it is on: a project or an initiative.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalCommentUpdateRef {
    pub id: String,
    pub project: Option<ExternalProjectRef>,
    pub initiative: Option<ExternalProjectRef>,
}

/// What threads are on, under its kind's key: `task`, `project`,
/// `milestone`, `meeting`, `note`, `initiative` or `update`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalCommentOn {
    Task(ExternalWorkRef),
    Project(ExternalProjectRef),
    Milestone(ExternalCommentMilestoneRef),
    Meeting(ExternalCommentSubjectRef),
    Note(ExternalCommentSubjectRef),
    Initiative(ExternalProjectRef),
    Update(ExternalCommentUpdateRef),
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkComments {
    pub schema_version: u32,
    #[serde(flatten)]
    pub on: ExternalCommentOn,
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
    #[serde(flatten)]
    pub on: ExternalCommentOn,
    /// The comment as it is now, or for a dry run as it would be; for
    /// resolve, the thread. Absent on a dry run of add.
    pub comment: Option<ExternalWorkComment>,
    /// True when this answered from an earlier run of the same operation.
    pub replayed: bool,
}

fn missing_comment(id: &Uuid) -> ExternalError {
    ExternalError::new(ExternalErrorCode::NotFound, format!("No comment {id}."))
}

/// What a comment is on, as the caller may see it.
struct Subject {
    kind: WorkCommentSubject,
    id: String,
    on: ExternalCommentOn,
}

impl Subject {
    fn task(row: &super::WorkItemRow) -> Self {
        Self {
            kind: WorkCommentSubject::Task,
            id: row.item.id.clone(),
            on: ExternalCommentOn::Task(task_ref_of(row)),
        }
    }

    fn project(project: &Project) -> Self {
        Self {
            kind: WorkCommentSubject::Project,
            id: project.id.clone(),
            on: ExternalCommentOn::Project(ExternalProjectRef {
                id: project.id.clone(),
                name: project.title.clone(),
            }),
        }
    }

    fn milestone(milestone: &Milestone, project: &Project) -> Self {
        Self {
            kind: WorkCommentSubject::Milestone,
            id: milestone.id.clone(),
            on: ExternalCommentOn::Milestone(ExternalCommentMilestoneRef {
                id: milestone.id.clone(),
                name: milestone.title.clone(),
                project: ExternalProjectRef {
                    id: project.id.clone(),
                    name: project.title.clone(),
                },
            }),
        }
    }

    fn initiative(id: &str, name: &str) -> Self {
        Self {
            kind: WorkCommentSubject::Initiative,
            id: id.to_owned(),
            on: ExternalCommentOn::Initiative(ExternalProjectRef {
                id: id.to_owned(),
                name: name.to_owned(),
            }),
        }
    }

    /// "The task", "The project" and so on, for a refusal.
    fn the(&self) -> &'static str {
        match self.kind {
            WorkCommentSubject::Task => "The task",
            WorkCommentSubject::Project => "The project",
            WorkCommentSubject::Milestone => "The milestone",
            WorkCommentSubject::Meeting => "The meeting",
            WorkCommentSubject::Note => "The note",
            WorkCommentSubject::Initiative => "The initiative",
            WorkCommentSubject::Update => "The update",
        }
    }
}

impl Tracker<'_> {
    fn author<'a>(write: &'a WriteArgs) -> Author<'a> {
        Author {
            actor: WorkActor::Agent,
            agent: write.agent.as_deref(),
        }
    }

    /// What TASK or `on` names, refused when the caller may not see it; a
    /// personal document also needs the agent `agent` to hold `needed` on it.
    async fn subject(
        &self,
        board: &Board,
        task: Option<&TaskRef>,
        on: &CommentOnArgs,
        agent: Option<&str>,
        needed: ScratchNoteRole,
    ) -> Result<Subject, ExternalError> {
        if let Some(task) = task {
            return Ok(Subject::task(board.task(task)?));
        }
        if let Some(project) = &on.project {
            let projects = self.every_project()?;
            let project = Self::pick_project(project, &projects)?;
            return match &on.milestone {
                Some(milestone) => {
                    let milestones = self.milestones_of(project)?;
                    Ok(Subject::milestone(
                        pick_milestone(milestone, &milestones)?,
                        project,
                    ))
                }
                None => Ok(Subject::project(project)),
            };
        }
        if let Some(meeting) = on.meeting {
            return self
                .meeting_subject(MeetingSessionId::from_uuid(meeting))
                .await;
        }
        if let Some(note) = on.note {
            return self
                .note_subject(&note.to_string(), agent, needed)?
                .ok_or_else(|| {
                    ExternalError::new(ExternalErrorCode::NotFound, format!("No note {note}."))
                });
        }
        if let Some(initiative) = &on.initiative {
            let all = self.all_initiatives()?;
            let initiative = Self::pick_initiative(initiative, &all)?;
            return Ok(Subject::initiative(&initiative.id, &initiative.name));
        }
        if let Some(update) = on.update {
            return self.update_subject(&update.to_string())?.ok_or_else(|| {
                ExternalError::new(ExternalErrorCode::NotFound, format!("No update {update}."))
            });
        }
        Err(ExternalError::invalid(
            "Name a task, or pass --project, --meeting, --note, --initiative or --update.",
        ))
    }

    /// A project's or an initiative's update not deleted.
    fn update_subject(&self, id: &str) -> Result<Option<Subject>, ExternalError> {
        let found = planning::history(self.history, |conn| {
            if let Ok(update) = crate::planning::updates::update(conn, id) {
                let project = plans::project(conn, &update.project_id)?;
                return Ok(Some(ExternalCommentUpdateRef {
                    id: update.id,
                    project: Some(ExternalProjectRef {
                        id: project.id,
                        name: project.title,
                    }),
                    initiative: None,
                }));
            }
            let Ok(update) = work::initiatives::update(conn, id) else {
                return Ok(None);
            };
            let initiative = work::initiatives::initiative(conn, &update.initiative_id)?;
            Ok(Some(ExternalCommentUpdateRef {
                id: update.id,
                project: None,
                initiative: Some(ExternalProjectRef {
                    id: initiative.id,
                    name: initiative.name,
                }),
            }))
        })
        .map_err(|message| refusal(self.history, message))?;
        Ok(found.map(|update| Subject {
            kind: WorkCommentSubject::Update,
            id: update.id.clone(),
            on: ExternalCommentOn::Update(update),
        }))
    }

    /// A meeting the caller may see; one kept on this Mac reads as absent.
    async fn meeting_subject(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Subject, ExternalError> {
        let store = store(self.meetings).await?;
        let review = review(&store, session_id)?;
        let id = session_id.uuid().to_string();
        Ok(Subject {
            kind: WorkCommentSubject::Meeting,
            id: id.clone(),
            on: ExternalCommentOn::Meeting(ExternalCommentSubjectRef {
                id,
                title: review.session.title,
            }),
        })
    }

    fn note_subject(
        &self,
        id: &str,
        agent: Option<&str>,
        needed: ScratchNoteRole,
    ) -> Result<Option<Subject>, ExternalError> {
        let Ok(uuid) = Uuid::parse_str(id) else {
            return Ok(None);
        };
        let note = match self.note(&uuid, agent, needed) {
            Ok(note) => note,
            Err(error) if error.error == ExternalErrorCode::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(Subject {
            kind: WorkCommentSubject::Note,
            id: note.id.clone(),
            on: ExternalCommentOn::Note(ExternalCommentSubjectRef {
                id: note.id,
                title: note.title,
            }),
        }))
    }

    /// What `comment` is on, when the caller may see it.
    async fn subject_of(
        &self,
        board: &Board,
        comment: &WorkComment,
        agent: Option<&str>,
        needed: ScratchNoteRole,
    ) -> Result<Option<Subject>, ExternalError> {
        let id = comment.item_id.as_str();
        Ok(match comment.subject {
            WorkCommentSubject::Task => board.row(id).map(Subject::task),
            WorkCommentSubject::Project => {
                planning::history(self.history, |conn| plans::project(conn, id))
                    .ok()
                    .map(|project| Subject::project(&project))
            }
            WorkCommentSubject::Milestone => planning::history(self.history, |conn| {
                let milestone = plans::milestone(conn, id)?;
                let project = plans::project(conn, &milestone.project_id)?;
                Ok((milestone, project))
            })
            .ok()
            .map(|(milestone, project)| Subject::milestone(&milestone, &project)),
            WorkCommentSubject::Meeting => match Uuid::parse_str(id) {
                Ok(uuid) => match self
                    .meeting_subject(MeetingSessionId::from_uuid(uuid))
                    .await
                {
                    Ok(subject) => Some(subject),
                    Err(error) if error.error == ExternalErrorCode::NotFound => None,
                    Err(error) => return Err(error),
                },
                Err(_) => None,
            },
            WorkCommentSubject::Note => self.note_subject(id, agent, needed)?,
            WorkCommentSubject::Initiative => {
                planning::history(self.history, |conn| work::initiatives::initiative(conn, id))
                    .ok()
                    .map(|initiative| Subject::initiative(&initiative.id, &initiative.name))
            }
            WorkCommentSubject::Update => self.update_subject(id)?,
        })
    }

    /// The comment and what it is on, when that is something the caller
    /// may see.
    async fn comment_on(
        &self,
        board: &Board,
        id: &Uuid,
        agent: Option<&str>,
        needed: ScratchNoteRole,
    ) -> Result<(WorkComment, Subject), ExternalError> {
        let found = planning::history(self.history, |conn| comments::thread(conn, &id.to_string()))
            .map_err(|_| missing_comment(id))?;
        let subject = self
            .subject_of(board, &found, agent, needed)
            .await?
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
        Ok((comment, subject))
    }

    fn comment_change(
        action: CommentAction,
        dry_run: bool,
        operation_id: Option<String>,
        on: ExternalCommentOn,
        comment: Option<WorkComment>,
        replayed: bool,
    ) -> WorkAnswer {
        WorkAnswer::Comment(Box::new(ExternalWorkCommentChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run,
            operation_id,
            on,
            comment: comment.map(project_comment),
            replayed,
        }))
    }

    pub(super) async fn comments(&self, args: &CommentsArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let subject = self
            .subject(
                &board,
                args.task.as_ref(),
                &args.on,
                args.agent.as_deref(),
                ScratchNoteRole::View,
            )
            .await?;
        let limit = args
            .limit
            .and_then(|limit| usize::try_from(limit).ok())
            .unwrap_or(comments::PAGE);
        let cursor = args.cursor.map(|cursor| cursor.to_string());
        let page = planning::history(self.history, |conn| {
            comments::page(conn, subject.kind, &subject.id, limit, cursor.as_deref())
        })
        .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::Comments(ExternalWorkComments {
            schema_version: QUERY_SCHEMA_VERSION,
            on: subject.on,
            threads: page.threads.into_iter().map(project_comment).collect(),
            next_cursor: page.next_cursor,
        }))
    }

    pub(super) async fn comment_add(
        &self,
        args: &CommentAddArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let agent = args.write.agent.as_deref();
        let subject = self
            .subject(
                &board,
                args.task.as_ref(),
                &args.on,
                agent,
                ScratchNoteRole::Comment,
            )
            .await?;
        if let Some(reply_to) = &args.reply_to {
            let (to, _) = self
                .comment_on(&board, reply_to, agent, ScratchNoteRole::Comment)
                .await?;
            if to.subject != subject.kind || to.item_id != subject.id {
                return Err(ExternalError::invalid(format!(
                    "Comment {reply_to} is on something else."
                )));
            }
        }
        let operation_id = args.operation_id(&subject.id, Local::now().date_naive())?;
        if args.dry_run {
            return Ok(Self::comment_change(
                CommentAction::Added,
                true,
                Some(operation_id),
                subject.on,
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
            subject.kind,
            &subject.id,
            &args.body,
            reply_to.as_deref(),
        )
        .map_err(|message| self.write_refusal(message, &operation_id, subject.the()))?;
        let replayed = comment.created_at_ms < started_ms;
        Ok(Self::comment_change(
            CommentAction::Added,
            false,
            Some(operation_id),
            subject.on,
            Some(comment),
            replayed,
        ))
    }

    pub(super) async fn comment_edit(
        &self,
        args: &CommentEditArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (comment, subject) = self
            .comment_on(
                &board,
                &args.comment,
                args.write.agent.as_deref(),
                ScratchNoteRole::Comment,
            )
            .await?;
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
                subject.on,
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
            subject.on,
            Some(comment),
            false,
        ))
    }

    pub(super) async fn comment_delete(
        &self,
        args: &CommentDeleteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (comment, subject) = self
            .comment_on(
                &board,
                &args.comment,
                args.write.agent.as_deref(),
                ScratchNoteRole::Comment,
            )
            .await?;
        let operation_id = args.write.operation_id("comment-delete", &comment.id)?;
        if args.dry_run {
            return Ok(Self::comment_change(
                CommentAction::Deleted,
                true,
                Some(operation_id),
                subject.on,
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
            subject.on,
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
        let (comment, subject) = self
            .comment_on(
                &board,
                &args.comment,
                args.write.agent.as_deref(),
                ScratchNoteRole::Comment,
            )
            .await?;
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
                subject.on,
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
            subject.on,
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
        let (titles, kept) = if args.from_checklist {
            let (titles, kept) = checklist(parent.item.details.as_deref().unwrap_or_default());
            if titles.is_empty() {
                return Err(ExternalError::invalid(format!(
                    "{}'s description has no open checklist lines (- [ ] ...).",
                    handle(parent.item.number)
                )));
            }
            (titles, Some(kept))
        } else {
            (args.titles.clone(), None)
        };
        let mut inputs: Vec<WorkItemInput> = titles
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
        let stripped = kept.map(|kept| WorkPatch {
            details: Some(Some(kept).filter(|kept| !kept.is_empty())),
            ..WorkPatch::default()
        });
        if inputs.iter().any(|input| input.title.is_empty()) {
            return Err(ExternalError::invalid("A sub-task needs a title."));
        }
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let operation_id = args
            .write
            .operation_id("subtask", &(&parent.item.id, &inputs, &today))?;
        let mut skipped = Vec::new();
        let mut asked = None;
        if args.from_checklist {
            (inputs, skipped, asked) = planning::history(self.history, |conn| {
                board.checklist_request(conn, &operation_id, parent, inputs)
            })
            .map_err(|message| {
                self.write_refusal(message, &operation_id, &handle(parent.item.number))
            })?;
        }
        if inputs.is_empty() {
            return Ok(WorkAnswer::Change(ExternalWorkChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action: WorkAction::Created,
                operation_id: None,
                tasks: Vec::new(),
                skipped,
                proposed: Vec::new(),
                replayed: false,
                evidence_url: None,
            }));
        }
        if args.dry_run {
            let inherited = owner
                .clone()
                .unwrap_or_else(|| board.inherited_owner(parent));
            let mut changes: Vec<ExternalWorkPlanned> = inputs
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
            if let Some(stripped) = &stripped {
                changes.push(board.planned(parent, stripped, None));
            }
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
        if let Some(asked) = asked {
            self.file_sent(&operation_id, SUBTASK_CHECKLIST_ASKED, &asked, &inputs)?;
        }
        let mut proposed = Vec::new();
        if let Some(stripped) = &stripped {
            let checklist_operation = Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("{operation_id}/checklist").as_bytes(),
            )
            .to_string();
            let change = agents::update_as(
                self.app,
                &args.write.agent(),
                &checklist_operation,
                agents::ItemChange {
                    id: &parent.item.id,
                    expected_revision: parent.item.revision,
                    patch: stripped,
                    link: None,
                },
            )
            .await
            .map_err(|message| {
                self.write_refusal(message, &checklist_operation, &handle(parent.item.number))
            })?;
            proposed = change
                .proposed
                .iter()
                .map(|proposal| board.proposal(proposal))
                .collect();
        }
        let replayed = items.iter().all(|item| item.created_at_ms < started_ms);
        Ok(WorkAnswer::Change(ExternalWorkChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: WorkAction::Created,
            operation_id: Some(operation_id),
            tasks: items
                .into_iter()
                .map(|item| board.project_item(item))
                .collect(),
            skipped,
            proposed,
            replayed,
            evidence_url: None,
        }))
    }

    pub(super) async fn copy(&self, args: &CopyArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let original = board.task(&args.task)?;
        let source = handle(original.item.number);
        let operation_id = args.write.operation_id(
            "copy",
            &(
                &original.item.id,
                original.item.revision,
                args.with_subtasks,
            ),
        )?;
        let hidden = planning::history(self.history, |history| board.fence.hidden_tasks(history))
            .map_err(|message| refusal(self.history, message))?;
        if args.dry_run {
            let copies = self.rehearse(|conn| {
                crate::planning::work::items::copy(
                    conn,
                    &original.item.id,
                    args.with_subtasks,
                    &hidden,
                    WorkActor::Agent,
                    now_ms(),
                )
            })?;
            let changes = copies
                .iter()
                .map(|copy| {
                    let field = if copy.parent_id == original.item.parent_id {
                        ExternalFieldChange {
                            field: "copy_of",
                            from: "none".to_owned(),
                            to: source.clone(),
                        }
                    } else {
                        ExternalFieldChange {
                            field: "parent",
                            from: "none".to_owned(),
                            to: copies
                                .iter()
                                .find(|parent| Some(&parent.id) == copy.parent_id.as_ref())
                                .map_or_else(|| "none".to_owned(), |parent| parent.title.clone()),
                        }
                    };
                    ExternalWorkPlanned {
                        id: String::new(),
                        handle: "new".to_owned(),
                        title: copy.title.clone(),
                        fields: vec![field],
                        commitment: None,
                        skipped: None,
                    }
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
        self.applies("copying a task")?;
        let started_ms = now_ms();
        let items = agents::copy_as(
            self.app,
            &args.write.agent(),
            &operation_id,
            &original.item.id,
            args.with_subtasks,
            &hidden,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, &source))?;
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

    pub(super) async fn react(&self, args: &ReactArgs) -> Result<WorkAnswer, ExternalError> {
        let target = args.target.trim();
        let comment = match Uuid::parse_str(target) {
            Ok(id) => planning::history(self.history, |conn| {
                let id = id.to_string();
                let found: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM planning_task_comments
                                   WHERE id = ?1 AND deleted_at_ms IS NULL)
                         OR EXISTS(SELECT 1 FROM planning_comments
                                   WHERE id = ?1 AND deleted_at_ms IS NULL
                                     AND subject_kind != 'meeting')",
                    [&id],
                    |row| row.get(0),
                )?;
                Ok(found.then_some(id))
            })
            .map_err(|message| refusal(self.history, message))?,
            Err(_) => None,
        };
        let (on, id) = match comment {
            Some(id) => (WorkReactionSubject::Comment, id),
            None => {
                let board = self.board().await?;
                let task = task_ref(target).map_err(ExternalError::invalid)?;
                (
                    WorkReactionSubject::Task,
                    board.task(&task)?.item.id.clone(),
                )
            }
        };
        if reactions::emoji(&args.emoji).is_err() {
            return Err(ExternalError::invalid(
                "Name one emoji, such as a thumbs up; words and letters are not reactions.",
            ));
        }
        let add = !args.remove;
        let operation_id = args
            .write
            .operation_id("react", &(on, &id, args.emoji.trim(), add))?;
        let author = Self::author(&args.write);
        let reactions = if args.dry_run {
            self.rehearse(|conn| {
                reactions::react(conn, author, on, &id, &args.emoji, add, now_ms())
            })?
        } else {
            reactions::react_as(self.app, author, &operation_id, on, &id, &args.emoji, add)
                .map_err(|message| self.write_refusal(message, &operation_id, "It"))?
        };
        Ok(WorkAnswer::Reactions(ExternalWorkReactions {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id,
            on,
            id,
            reactions,
        }))
    }
}

/// The open checklist lines of `details` (`- [ ] title`, also with `*` or
/// `+`) as titles, and the details without them.
fn checklist(details: &str) -> (Vec<String>, String) {
    let mut titles = Vec::new();
    let mut kept = Vec::new();
    for line in details.lines() {
        let open = line
            .trim_start()
            .strip_prefix(['-', '*', '+'])
            .and_then(|rest| rest.strip_prefix(" [ ]"))
            .map(str::trim)
            .filter(|title| !title.is_empty());
        match open {
            Some(title) => titles.push(title.to_owned()),
            None => kept.push(line),
        }
    }
    (titles, kept.join("\n").trim().to_owned())
}

const SUBTASK_CHECKLIST_ASKED: &str = "cli_subtask_from_checklist";

impl Board {
    /// The sub-tasks a checklist run under `operation_id` sends, a skip for
    /// each line `parent` already has as a sub-task, and the checklist as
    /// asked, to file once written. A retry sends what the first run sent,
    /// whatever sub-tasks that run made since.
    fn checklist_request(
        &self,
        conn: &Connection,
        operation_id: &str,
        parent: &super::WorkItemRow,
        inputs: Vec<WorkItemInput>,
    ) -> anyhow::Result<(Vec<WorkItemInput>, Vec<ExternalWorkSkip>, Option<String>)> {
        let asked = serde_json::to_string(&(&parent.item.id, &inputs))?;
        if let Some(sent) = sent_before(conn, operation_id, SUBTASK_CHECKLIST_ASKED, &asked)? {
            return Ok((sent, Vec::new(), None));
        }
        let (unmade, skipped) = self.unmade_subtasks(parent, inputs);
        Ok((unmade, skipped, Some(asked)))
    }

    /// `inputs` split into the sub-tasks `parent` lacks and a skip for each
    /// it already has by that title, so a checklist made into sub-tasks
    /// once is not made again while its lines wait to be taken out.
    fn unmade_subtasks(
        &self,
        parent: &super::WorkItemRow,
        inputs: Vec<WorkItemInput>,
    ) -> (Vec<WorkItemInput>, Vec<ExternalWorkSkip>) {
        let mut skipped = Vec::new();
        let unmade = inputs
            .into_iter()
            .filter(|input| {
                let made = self.rows.iter().find(|row| {
                    row.item.parent_id.as_deref() == Some(parent.item.id.as_str())
                        && row.item.title.trim().eq_ignore_ascii_case(&input.title)
                });
                if let Some(row) = made {
                    skipped.push(ExternalWorkSkip {
                        id: row.item.id.clone(),
                        handle: handle(row.item.number),
                        reason: "already a sub-task".to_owned(),
                    });
                }
                made.is_none()
            })
            .collect();
        (unmade, skipped)
    }

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
        "{indent}{}  {who}  {}{notes}  id {}{}",
        local_time(comment.created_at_ms),
        if comment.deleted { "deleted" } else { "" },
        comment.id,
        comment
            .link
            .as_deref()
            .map(|link| format!("  {link}"))
            .unwrap_or_default()
    );
    for line in comment.body.as_deref().unwrap_or_default().lines() {
        let _ = writeln!(text, "{indent}  {line}");
    }
    if !comment.reactions.is_empty() {
        let _ = writeln!(text, "{indent}  {}", reactions_text(&comment.reactions));
    }
    for reply in &comment.replies {
        comment_text(text, reply, &format!("{indent}    "));
    }
}

/// Each emoji with how many used it and who: `+1 x2 (you, claude)`.
fn reactions_text(reactions: &[WorkReaction]) -> String {
    reactions
        .iter()
        .map(|reaction| {
            let who: Vec<&str> = reaction
                .by
                .iter()
                .map(|author| match author.kind {
                    WorkActor::Me => "you",
                    WorkActor::Sona => "Sona",
                    WorkActor::Agent => author.name.as_deref().unwrap_or("agent"),
                })
                .collect();
            format!("{} x{} ({})", reaction.emoji, who.len(), who.join(", "))
        })
        .collect::<Vec<_>>()
        .join("  ")
}

pub(super) fn threads_text(text: &mut String, threads: &[ExternalWorkComment]) {
    for thread in threads {
        comment_text(text, thread, "  ");
    }
}

/// What threads are on, as a line names it.
fn on_text(on: &ExternalCommentOn) -> String {
    match on {
        ExternalCommentOn::Task(task) => format!("{} {}", task.handle, task.title),
        ExternalCommentOn::Project(project) => format!("project {}", project.name),
        ExternalCommentOn::Milestone(milestone) => {
            format!("milestone {} in {}", milestone.name, milestone.project.name)
        }
        ExternalCommentOn::Meeting(meeting) => format!("meeting {}", meeting.title),
        ExternalCommentOn::Note(note) => format!("note {}", note.title),
        ExternalCommentOn::Initiative(initiative) => format!("initiative {}", initiative.name),
        ExternalCommentOn::Update(update) => match (&update.project, &update.initiative) {
            (Some(project), _) => format!("an update on project {}", project.name),
            (None, Some(initiative)) => format!("an update on initiative {}", initiative.name),
            (None, None) => format!("update {}", update.id),
        },
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::Comments(page) => {
            let _ = match &page.on {
                ExternalCommentOn::Task(task) => writeln!(
                    text,
                    "{} {} ({})",
                    task.handle,
                    task.title,
                    status_text(task.status)
                ),
                on => writeln!(text, "Comments on {}", on_text(on)),
            };
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
                "{verb} {}{}",
                on_text(&change.on),
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
        WorkAnswer::Reactions(answer) => {
            let dry = if answer.dry_run { " (dry run)" } else { "" };
            if answer.reactions.is_empty() {
                let _ = writeln!(text, "No reactions{dry}");
            } else {
                let _ = writeln!(text, "{}{dry}", reactions_text(&answer.reactions));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::WorkVerb;
    use super::{CommentAddArgs, CommentCommand, CommentVerb};
    use crate::cli::{CliArgs, Command};
    use crate::planning::work::types::{WorkItem, WorkItemInput};
    use chrono::NaiveDate;
    use clap::Parser;

    fn input(title: &str) -> WorkItemInput {
        WorkItemInput {
            title: title.to_owned(),
            details: None,
            status: None,
            priority: None,
            project_id: None,
            milestone_id: None,
            owner: None,
            due_at_ms: None,
            label_ids: None,
            recurrence: None,
        }
    }

    /// A checklist made into sub-tasks keeps its lines in the parent while
    /// their removal waits for the user, so the next run must not make
    /// those sub-tasks again.
    #[test]
    fn a_checklist_line_already_made_into_a_sub_task_is_skipped() {
        use super::super::tests::{board, row, task};
        let parent = task();
        let made = WorkItem {
            id: uuid::Uuid::new_v4().to_string(),
            number: 2,
            title: "Book the venue".to_owned(),
            parent_id: Some(parent.id.clone()),
            ..task()
        };
        let elsewhere = WorkItem {
            id: uuid::Uuid::new_v4().to_string(),
            number: 3,
            title: "Send invites".to_owned(),
            ..task()
        };
        let board = board(
            vec![row(parent.clone()), row(made.clone()), row(elsewhere)],
            Vec::new(),
        );
        let (unmade, skipped) = board.unmade_subtasks(
            &row(parent),
            vec![input("book the venue"), input("Send invites")],
        );
        assert_eq!(
            unmade
                .iter()
                .map(|input| input.title.as_str())
                .collect::<Vec<_>>(),
            ["Send invites"]
        );
        assert_eq!(
            skipped
                .iter()
                .map(|skip| skip.id.as_str())
                .collect::<Vec<_>>(),
            [made.id.as_str()]
        );
    }

    /// The retry finds the sub-task its first run made; it must still send
    /// what that run sent, or the store refuses it as another change.
    #[test]
    fn a_checklist_retried_after_a_skip_sends_what_its_first_run_sent() {
        use super::super::organize::file_sent;
        use super::super::tests::{board, row, task};
        use super::SUBTASK_CHECKLIST_ASKED;
        use crate::planning::schema;
        use rusqlite::Connection;
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let parent = task();
        let made = WorkItem {
            id: uuid::Uuid::new_v4().to_string(),
            number: 2,
            title: "Book the venue".to_owned(),
            parent_id: Some(parent.id.clone()),
            ..task()
        };
        let asked = || vec![input("book the venue"), input("Send invites")];
        let operation_id = uuid::Uuid::new_v4().to_string();
        let before = board(vec![row(parent.clone()), row(made.clone())], Vec::new());
        let (sent, _, filed) = before
            .checklist_request(&conn, &operation_id, &row(parent.clone()), asked())
            .expect("the first run");
        let filed = filed.expect("the checklist as asked");
        file_sent(&conn, &operation_id, SUBTASK_CHECKLIST_ASKED, &filed, &sent).expect("filed");
        let invites = WorkItem {
            id: uuid::Uuid::new_v4().to_string(),
            number: 3,
            title: "Send invites".to_owned(),
            parent_id: Some(parent.id.clone()),
            ..task()
        };
        let after = board(
            vec![row(parent.clone()), row(made), row(invites)],
            Vec::new(),
        );
        let (retry, skipped, filed) = after
            .checklist_request(&conn, &operation_id, &row(parent.clone()), asked())
            .expect("the retry");
        assert_eq!(retry, sent);
        assert!(skipped.is_empty());
        assert!(filed.is_none());
        assert!(after
            .checklist_request(
                &conn,
                &operation_id,
                &row(parent),
                vec![input("Send invites")]
            )
            .is_err());
    }

    const TASK: &str = "5b0c6a8e-3d1f-4c2a-9e7b-0a1b2c3d4e5f";

    fn comment(body: &str) -> CommentAddArgs {
        match CliArgs::try_parse_from(["sona", "work", "comment", "add", "S-1", "--body", body]) {
            Ok(CliArgs {
                command: Some(Command::Work(work)),
                ..
            }) => match work.verb {
                WorkVerb::Comment(CommentCommand {
                    verb: CommentVerb::Add(args),
                }) => args,
                other => panic!("`work comment add` parsed as {other:?}"),
            },
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
