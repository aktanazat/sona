//! Comments in threads one reply deep as Linear keeps them: a reply to a
//! reply joins the thread's first comment. Deleting a comment blanks it and
//! keeps its place while replies hang under it. A task's comments go into
//! its history; a project's, a milestone's, a meeting's or a note's are
//! kept beside it.

use super::super::store::{self, Refusal};
use super::agents;
use super::items;
use super::types::{
    WorkActivityKind, WorkActor, WorkComment, WorkCommentAuthor, WorkCommentPage,
    WorkCommentSubject,
};
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::HashMap;
use uuid::Uuid;

/// Threads on a project, a milestone, a meeting or a note, kept as a task's
/// are. A project's and a milestone's leave with it.
pub(crate) const SUBJECTS_MIGRATION: &str = "CREATE TABLE planning_comments (
    id TEXT PRIMARY KEY NOT NULL,
    subject_kind TEXT NOT NULL CHECK(subject_kind IN ('project', 'milestone', 'meeting', 'note')),
    subject_id TEXT NOT NULL,
    parent_comment_id TEXT,
    author_kind TEXT NOT NULL CHECK(author_kind IN ('me', 'sona', 'agent')),
    author_name TEXT CHECK(author_name IS NULL OR length(trim(author_name)) > 0),
    body TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    edited_at_ms INTEGER,
    resolved_at_ms INTEGER,
    deleted_at_ms INTEGER,
    CHECK(deleted_at_ms IS NOT NULL OR length(trim(body)) > 0),
    CHECK(parent_comment_id IS NULL OR resolved_at_ms IS NULL),
    CHECK(parent_comment_id IS NULL OR parent_comment_id != id)
);
CREATE INDEX planning_comments_subject
    ON planning_comments(subject_kind, subject_id, created_at_ms, id);
CREATE INDEX planning_comments_thread ON planning_comments(parent_comment_id);
CREATE TRIGGER planning_comments_project_delete AFTER DELETE ON planning_projects BEGIN
    DELETE FROM planning_comments WHERE subject_kind = 'project' AND subject_id = old.id;
END;
CREATE TRIGGER planning_comments_milestone_delete AFTER DELETE ON planning_milestones BEGIN
    DELETE FROM planning_comments WHERE subject_kind = 'milestone' AND subject_id = old.id;
END;";

/// A note's threads leave with it. Kept apart from [`SUBJECTS_MIGRATION`]
/// because notes are not in the planner's own schema.
pub(crate) const NOTE_COMMENTS_MIGRATION: &str =
    "CREATE TRIGGER planning_comments_note_delete AFTER DELETE ON scratch_notes BEGIN
    DELETE FROM planning_comments WHERE subject_kind = 'note' AND subject_id = old.id;
END;";

pub(crate) const PAGE: usize = 50;
pub(crate) const MAX_PAGE: usize = 100;

/// Who writes or changes a comment.
#[derive(Clone, Copy)]
pub(crate) struct Author<'a> {
    pub(crate) actor: WorkActor,
    pub(crate) agent: Option<&'a str>,
}

impl Author<'_> {
    pub(crate) fn name(&self) -> Option<String> {
        match self.actor {
            WorkActor::Agent => Some(agents::agent_name(self.agent)),
            WorkActor::Me | WorkActor::Sona => None,
        }
    }

    fn wrote(&self, comment: &WorkComment) -> bool {
        comment.author.kind == self.actor
            && match (self.name(), comment.author.name.as_deref()) {
                (Some(mine), Some(theirs)) => mine.eq_ignore_ascii_case(theirs),
                (None, None) => true,
                _ => false,
            }
    }
}

const TASK_COLUMNS: &str = "id, 'task', task_id, parent_comment_id, author_kind, author_name,
    body, created_at_ms, edited_at_ms, resolved_at_ms, deleted_at_ms";

const SUBJECT_COLUMNS: &str = "id, subject_kind, subject_id, parent_comment_id, author_kind,
    author_name, body, created_at_ms, edited_at_ms, resolved_at_ms, deleted_at_ms";

fn table(subject: WorkCommentSubject) -> &'static str {
    match subject {
        WorkCommentSubject::Task => "planning_task_comments",
        _ => "planning_comments",
    }
}

fn columns(subject: WorkCommentSubject) -> &'static str {
    match subject {
        WorkCommentSubject::Task => TASK_COLUMNS,
        _ => SUBJECT_COLUMNS,
    }
}

/// Picks the comments on `?1` among `c`, a row of [`table`].
fn on(subject: WorkCommentSubject) -> &'static str {
    match subject {
        WorkCommentSubject::Task => "c.task_id = ?1",
        WorkCommentSubject::Project => "c.subject_kind = 'project' AND c.subject_id = ?1",
        WorkCommentSubject::Milestone => "c.subject_kind = 'milestone' AND c.subject_id = ?1",
        WorkCommentSubject::Meeting => "c.subject_kind = 'meeting' AND c.subject_id = ?1",
        WorkCommentSubject::Note => "c.subject_kind = 'note' AND c.subject_id = ?1",
    }
}

/// Whether `c` shows: live, or deleted with live replies under it.
fn shown(subject: WorkCommentSubject) -> String {
    format!(
        "(c.deleted_at_ms IS NULL OR EXISTS (
            SELECT 1 FROM {} r
            WHERE r.parent_comment_id = c.id AND r.deleted_at_ms IS NULL))",
        table(subject)
    )
}

/// `id` as `subject` keeps it, refused when there is no such subject. A
/// meeting is the caller's to check: meetings are kept in their own store.
fn subject_id(conn: &Connection, subject: WorkCommentSubject, id: &str) -> Result<String> {
    let id = store::uuid(id)?;
    match subject {
        WorkCommentSubject::Task => Ok(items::item(conn, &id)?.id),
        WorkCommentSubject::Project => Ok(store::project(conn, &id)?.id),
        WorkCommentSubject::Milestone => Ok(store::milestone(conn, &id)?.id),
        WorkCommentSubject::Note => crate::scratchpad::note_with_connection(conn, &id)?
            .map(|note| note.id)
            .ok_or_else(|| Refusal::NotFound.into()),
        WorkCommentSubject::Meeting => Ok(id),
    }
}

fn read(row: &Row<'_>) -> rusqlite::Result<WorkComment> {
    let deleted = row.get::<_, Option<i64>>(10)?.is_some();
    Ok(WorkComment {
        id: row.get(0)?,
        subject: row.get(1)?,
        item_id: row.get(2)?,
        parent_id: row.get(3)?,
        author: WorkCommentAuthor {
            kind: row.get(4)?,
            name: row.get(5)?,
        },
        body: if deleted { None } else { Some(row.get(6)?) },
        created_at_ms: row.get(7)?,
        edited_at_ms: row.get(8)?,
        resolved_at_ms: row.get(9)?,
        deleted,
        replies: Vec::new(),
    })
}

fn comment(conn: &Connection, id: &str) -> Result<WorkComment> {
    let id = store::uuid(id)?;
    conn.query_row(
        &format!(
            "SELECT {TASK_COLUMNS} FROM planning_task_comments WHERE id = ?1
             UNION ALL SELECT {SUBJECT_COLUMNS} FROM planning_comments WHERE id = ?1"
        ),
        [&id],
        read,
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

fn live(conn: &Connection, id: &str) -> Result<WorkComment> {
    let found = comment(conn, id)?;
    if found.deleted {
        return Err(Refusal::NotFound.into());
    }
    Ok(found)
}

fn body(value: &str) -> Result<&str> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Refusal::Invalid.into());
    }
    Ok(value)
}

/// A thread's first comment with its live replies, oldest first.
pub(crate) fn thread(conn: &Connection, id: &str) -> Result<WorkComment> {
    let found = comment(conn, id)?;
    let mut root = match &found.parent_id {
        Some(parent_id) => comment(conn, parent_id)?,
        None => found,
    };
    let mut statement = conn.prepare(&format!(
        "SELECT {} FROM {}
         WHERE parent_comment_id = ?1 AND deleted_at_ms IS NULL
         ORDER BY created_at_ms, id",
        columns(root.subject),
        table(root.subject)
    ))?;
    root.replies = statement
        .query_map([&root.id], read)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(root)
}

/// The threads on `subject_id` oldest first, from just after `cursor`, a
/// thread's first comment, at most `limit` of them.
pub(crate) fn page(
    conn: &Connection,
    subject: WorkCommentSubject,
    subject_id: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<WorkCommentPage> {
    let subject_id = self::subject_id(conn, subject, subject_id)?;
    let after = cursor
        .map(|cursor| {
            let at = comment(conn, cursor)?;
            if at.subject != subject || at.item_id != subject_id || at.parent_id.is_some() {
                return Err(Refusal::Invalid.into());
            }
            Ok::<_, anyhow::Error>((at.created_at_ms, at.id))
        })
        .transpose()?;
    let (after_ms, after_id) = after.unwrap_or((i64::MIN, String::new()));
    let mut statement = conn.prepare(&format!(
        "SELECT c.id FROM {} c
         WHERE {} AND c.parent_comment_id IS NULL AND {}
           AND (c.created_at_ms, c.id) > (?2, ?3)
         ORDER BY c.created_at_ms, c.id LIMIT ?4",
        table(subject),
        on(subject),
        shown(subject)
    ))?;
    let fetch = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
    let mut ids: Vec<String> = statement
        .query_map(params![subject_id, after_ms, after_id, fetch], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    let next_cursor = if ids.len() > limit {
        ids.truncate(limit);
        ids.last().cloned()
    } else {
        None
    };
    let threads = ids
        .iter()
        .map(|id| thread(conn, id))
        .collect::<Result<_>>()?;
    Ok(WorkCommentPage {
        threads,
        next_cursor,
        kept_local: false,
    })
}

/// `page` for a meeting kept on this Mac. Agents cannot read such a
/// meeting, so their comments on it, and the threads they began, are left
/// out.
pub(crate) fn kept_local(mut page: WorkCommentPage) -> WorkCommentPage {
    let by_agent = |comment: &WorkComment| comment.author.kind == WorkActor::Agent;
    page.threads.retain(|thread| !by_agent(thread));
    for thread in &mut page.threads {
        thread.replies.retain(|reply| !by_agent(reply));
    }
    page.kept_local = true;
    page
}

/// Every thread on `subject_id`, oldest first.
pub(crate) fn all(
    conn: &Connection,
    subject: WorkCommentSubject,
    subject_id: &str,
) -> Result<Vec<WorkComment>> {
    Ok(page(conn, subject, subject_id, usize::MAX - 1, None)?.threads)
}

/// The words of every comment not deleted, by task, oldest first: what a
/// search reads besides a task's own fields.
pub(crate) fn bodies(conn: &Connection) -> Result<HashMap<String, Vec<String>>> {
    let mut statement = conn.prepare(
        "SELECT task_id, body FROM planning_task_comments
         WHERE deleted_at_ms IS NULL ORDER BY created_at_ms, id",
    )?;
    let mut rows = statement.query([])?;
    let mut bodies: HashMap<String, Vec<String>> = HashMap::new();
    while let Some(row) = rows.next()? {
        bodies.entry(row.get(0)?).or_default().push(row.get(1)?);
    }
    Ok(bodies)
}

/// A new comment, or a reply joining the thread `reply_to` is in. On a task
/// it is written into the task's history under its author.
pub(crate) fn add(
    conn: &Connection,
    subject: WorkCommentSubject,
    subject_id: &str,
    reply_to: Option<&str>,
    author: Author<'_>,
    text: &str,
    now_ms: i64,
) -> Result<WorkComment> {
    let subject_id = self::subject_id(conn, subject, subject_id)?;
    let text = body(text)?;
    let parent_id = reply_to
        .map(|reply_to| {
            let to = comment(conn, reply_to)?;
            if to.subject != subject || to.item_id != subject_id {
                return Err(Refusal::Invalid.into());
            }
            Ok::<_, anyhow::Error>(to.parent_id.unwrap_or(to.id))
        })
        .transpose()?;
    let id = Uuid::new_v4().to_string();
    let name = author.name();
    if subject != WorkCommentSubject::Task {
        conn.execute(
            "INSERT INTO planning_comments (id, subject_kind, subject_id, parent_comment_id,
                author_kind, author_name, body, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                subject.as_str(),
                subject_id,
                parent_id,
                author.actor.as_str(),
                name,
                text,
                now_ms
            ],
        )?;
        return comment(conn, &id);
    }
    let task_id = subject_id;
    conn.execute(
        "INSERT INTO planning_task_comments
            (id, task_id, parent_comment_id, author_kind, author_name, body, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            task_id,
            parent_id,
            author.actor.as_str(),
            name,
            text,
            now_ms
        ],
    )?;
    items::record(
        conn,
        &task_id,
        now_ms,
        author.actor,
        WorkActivityKind::Comment,
        None,
        Some(&id),
    )?;
    items::relate_mentions(conn, &task_id, None, Some(text), author.actor, now_ms)?;
    if let Some(name) = &name {
        agents::credit(conn, &task_id, now_ms, name, agents::via())?;
    }
    comment(conn, &id)
}

/// A comment's new text, from whoever wrote it.
pub(crate) fn edit(
    conn: &Connection,
    id: &str,
    author: Author<'_>,
    text: &str,
    now_ms: i64,
) -> Result<WorkComment> {
    let found = live(conn, id)?;
    if !author.wrote(&found) {
        return Err(Refusal::NotAuthor.into());
    }
    let text = body(text)?;
    if found.body.as_deref() != Some(text) {
        conn.execute(
            &format!(
                "UPDATE {} SET body = ?2, edited_at_ms = ?3 WHERE id = ?1",
                table(found.subject)
            ),
            params![found.id, text, now_ms],
        )?;
        if found.subject == WorkCommentSubject::Task {
            items::relate_mentions(
                conn,
                &found.item_id,
                found.body.as_deref(),
                Some(text),
                author.actor,
                now_ms,
            )?;
            if let Some(name) = author.name() {
                agents::credit(conn, &found.item_id, now_ms, &name, agents::via())?;
            }
        }
    }
    comment(conn, &found.id)
}

/// Blanks a comment. The user may delete any comment; an agent or Sona
/// only its own.
pub(crate) fn delete(
    conn: &Connection,
    id: &str,
    author: Author<'_>,
    now_ms: i64,
) -> Result<WorkComment> {
    let found = comment(conn, id)?;
    if found.deleted {
        return Ok(found);
    }
    if author.actor != WorkActor::Me && !author.wrote(&found) {
        return Err(Refusal::NotAuthor.into());
    }
    conn.execute(
        &format!(
            "UPDATE {} SET body = '', deleted_at_ms = ?2 WHERE id = ?1",
            table(found.subject)
        ),
        params![found.id, now_ms],
    )?;
    comment(conn, &found.id)
}

/// Resolves or reopens the thread `id` is in.
pub(crate) fn resolve(
    conn: &Connection,
    id: &str,
    resolved: bool,
    now_ms: i64,
) -> Result<WorkComment> {
    let found = live(conn, id)?;
    let root = found.parent_id.unwrap_or(found.id);
    conn.execute(
        &format!(
            "UPDATE {}
             SET resolved_at_ms = CASE WHEN ?2 THEN coalesce(resolved_at_ms, ?3) END
             WHERE id = ?1",
            table(found.subject)
        ),
        params![root, resolved, now_ms],
    )?;
    thread(conn, &root)
}

#[cfg(test)]
mod tests {
    use super::super::super::schema;
    use super::super::super::store::{insert_project, Refusal};
    use super::super::items::{self, NewItem};
    use super::super::types::{
        WorkActivityKind, WorkActor, WorkCommentSubject, WorkOwner, WorkPatch, WorkStatus,
    };
    use super::{add, comment, delete, kept_local, page, Author};
    use rusqlite::Connection;

    const AT: i64 = 1_700_000_000_000;
    const TASK: WorkCommentSubject = WorkCommentSubject::Task;
    const ME: Author<'static> = Author {
        actor: WorkActor::Me,
        agent: None,
    };

    fn tracker() -> Connection {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        conn
    }

    fn task(conn: &Connection, title: &str) -> String {
        items::insert(
            conn,
            &NewItem {
                title,
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Me,
            AT,
        )
        .expect("a task")
        .id
    }

    fn put_under(conn: &Connection, id: &str, parent_id: &str) -> anyhow::Result<()> {
        items::apply(
            conn,
            id,
            &WorkPatch {
                parent_id: Some(Some(parent_id.to_owned())),
                ..WorkPatch::default()
            },
            WorkActivityKind::Status,
            WorkActor::Me,
            AT,
        )
        .map(|_| ())
    }

    #[test]
    fn a_task_cannot_go_under_its_own_sub_task() {
        let conn = tracker();
        let top = task(&conn, "Launch");
        let middle = task(&conn, "Venue");
        let bottom = task(&conn, "Deposit");
        put_under(&conn, &middle, &top).expect("venue under launch");
        put_under(&conn, &bottom, &middle).expect("deposit under venue");

        let refused = put_under(&conn, &top, &bottom).expect_err("a loop");

        assert_eq!(
            refused.downcast_ref::<Refusal>(),
            Some(&Refusal::ParentLoop)
        );
        assert_eq!(items::item(&conn, &top).expect("launch").parent_id, None);
    }

    #[test]
    fn a_reply_to_a_reply_joins_the_thread_root() {
        let conn = tracker();
        let id = task(&conn, "Launch");
        let root = add(&conn, TASK, &id, None, ME, "Who books the venue?", AT).expect("root");
        let reply = add(&conn, TASK, &id, Some(&root.id), ME, "Dana", AT + 1).expect("reply");

        let nested =
            add(&conn, TASK, &id, Some(&reply.id), ME, "Confirmed", AT + 2).expect("nested");

        assert_eq!(nested.parent_id.as_deref(), Some(root.id.as_str()));
        let threads = page(&conn, TASK, &id, 10, None).expect("threads").threads;
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].replies.len(), 2);
    }

    #[test]
    fn deleting_a_thread_root_keeps_its_replies_readable() {
        let conn = tracker();
        let id = task(&conn, "Launch");
        let root = add(&conn, TASK, &id, None, ME, "Who books the venue?", AT).expect("root");
        add(&conn, TASK, &id, Some(&root.id), ME, "Dana", AT + 1).expect("reply");

        delete(&conn, &root.id, ME, AT + 2).expect("deleted root");

        let threads = page(&conn, TASK, &id, 10, None).expect("threads").threads;
        assert_eq!(threads.len(), 1);
        assert!(threads[0].deleted);
        assert_eq!(threads[0].body, None);
        assert_eq!(threads[0].replies[0].body.as_deref(), Some("Dana"));
    }

    #[test]
    fn a_projects_threads_stay_apart_from_its_tasks_and_leave_with_it() {
        let conn = tracker();
        let task_id = task(&conn, "Launch");
        let project_id = insert_project(&conn, "Harbor", None, AT).expect("a project").id;
        let project = WorkCommentSubject::Project;
        let on_task = add(&conn, TASK, &task_id, None, ME, "On the task", AT).expect("task");
        let on_project =
            add(&conn, project, &project_id, None, ME, "On the project", AT + 1).expect("project");

        let crossed = add(&conn, project, &project_id, Some(&on_task.id), ME, "x", AT + 2);

        assert_eq!(
            crossed.expect_err("a reply across subjects").downcast_ref::<Refusal>(),
            Some(&Refusal::Invalid)
        );
        let threads = page(&conn, project, &project_id, 10, None).expect("threads").threads;
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].body.as_deref(), Some("On the project"));
        assert_eq!(page(&conn, TASK, &task_id, 10, None).expect("threads").threads.len(), 1);
        conn.execute("DELETE FROM planning_projects WHERE id = ?1", [&project_id])
            .expect("project deleted");
        assert!(comment(&conn, &on_project.id).is_err());
    }

    #[test]
    fn a_meeting_kept_here_shows_no_agent_comments() {
        let conn = tracker();
        let meeting = WorkCommentSubject::Meeting;
        let id = "a6f47cca-0000-4000-8000-000000000001";
        let claude = Author {
            actor: WorkActor::Agent,
            agent: Some("claude"),
        };
        let mine = add(&conn, meeting, id, None, ME, "Pricing slipped", AT).expect("mine");
        add(&conn, meeting, id, Some(&mine.id), claude, "Read it", AT + 1).expect("reply");
        add(&conn, meeting, id, None, claude, "Summary", AT + 2).expect("agent thread");

        let kept = kept_local(page(&conn, meeting, id, 10, None).expect("threads"));

        assert!(kept.kept_local);
        assert_eq!(kept.threads.len(), 1);
        assert_eq!(kept.threads[0].id, mine.id);
        assert!(kept.threads[0].replies.is_empty());
    }
}
