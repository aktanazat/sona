//! Comments on a task, in threads one reply deep as Linear keeps them: a
//! reply to a reply joins the thread's first comment. Deleting a comment
//! blanks it and keeps its place while replies hang under it.

use super::super::store::{self, Refusal};
use super::agents;
use super::items;
use super::types::{WorkActivityKind, WorkActor, WorkComment, WorkCommentAuthor, WorkCommentPage};
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use uuid::Uuid;

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

const COLUMNS: &str = "id, task_id, parent_comment_id, author_kind, author_name, body,
    created_at_ms, edited_at_ms, resolved_at_ms, deleted_at_ms";

const SHOWN: &str = "(c.deleted_at_ms IS NULL OR EXISTS (
    SELECT 1 FROM planning_task_comments r
    WHERE r.parent_comment_id = c.id AND r.deleted_at_ms IS NULL))";

fn read(row: &Row<'_>) -> rusqlite::Result<WorkComment> {
    let deleted = row.get::<_, Option<i64>>(9)?.is_some();
    Ok(WorkComment {
        id: row.get(0)?,
        item_id: row.get(1)?,
        parent_id: row.get(2)?,
        author: WorkCommentAuthor {
            kind: row.get(3)?,
            name: row.get(4)?,
        },
        body: if deleted { None } else { Some(row.get(5)?) },
        created_at_ms: row.get(6)?,
        edited_at_ms: row.get(7)?,
        resolved_at_ms: row.get(8)?,
        deleted,
        replies: Vec::new(),
    })
}

fn comment(conn: &Connection, id: &str) -> Result<WorkComment> {
    let id = store::uuid(id)?;
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM planning_task_comments WHERE id = ?1"),
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
        "SELECT {COLUMNS} FROM planning_task_comments
         WHERE parent_comment_id = ?1 AND deleted_at_ms IS NULL
         ORDER BY created_at_ms, id"
    ))?;
    root.replies = statement
        .query_map([&root.id], read)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(root)
}

/// A task's threads oldest first, from just after `cursor`, a thread's
/// first comment, at most `limit` of them.
pub(crate) fn page(
    conn: &Connection,
    task_id: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<WorkCommentPage> {
    let task_id = items::item(conn, &store::uuid(task_id)?)?.id;
    let after = cursor
        .map(|cursor| {
            let at = comment(conn, cursor)?;
            if at.item_id != task_id || at.parent_id.is_some() {
                return Err(Refusal::Invalid.into());
            }
            Ok::<_, anyhow::Error>((at.created_at_ms, at.id))
        })
        .transpose()?;
    let (after_ms, after_id) = after.unwrap_or((i64::MIN, String::new()));
    let mut statement = conn.prepare(&format!(
        "SELECT c.id FROM planning_task_comments c
         WHERE c.task_id = ?1 AND c.parent_comment_id IS NULL AND {SHOWN}
           AND (c.created_at_ms, c.id) > (?2, ?3)
         ORDER BY c.created_at_ms, c.id LIMIT ?4"
    ))?;
    let fetch = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
    let mut ids: Vec<String> = statement
        .query_map(params![task_id, after_ms, after_id, fetch], |row| {
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
    })
}

/// Every thread on a task, oldest first.
pub(crate) fn all(conn: &Connection, task_id: &str) -> Result<Vec<WorkComment>> {
    Ok(page(conn, task_id, usize::MAX - 1, None)?.threads)
}

/// A new comment, or a reply joining the thread `reply_to` is in, written
/// into the task's history under its author.
pub(crate) fn add(
    conn: &Connection,
    task_id: &str,
    reply_to: Option<&str>,
    author: Author<'_>,
    text: &str,
    now_ms: i64,
) -> Result<WorkComment> {
    let task_id = items::item(conn, &store::uuid(task_id)?)?.id;
    let text = body(text)?;
    let parent_id = reply_to
        .map(|reply_to| {
            let to = comment(conn, reply_to)?;
            if to.item_id != task_id {
                return Err(Refusal::Invalid.into());
            }
            Ok::<_, anyhow::Error>(to.parent_id.unwrap_or(to.id))
        })
        .transpose()?;
    let id = Uuid::new_v4().to_string();
    let name = author.name();
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
    if let Some(name) = &name {
        conn.execute(
            "INSERT INTO planning_activity_agents (activity_id, agent)
             SELECT id, ?3 FROM planning_task_activity
             WHERE task_id = ?1 AND kind = 'comment' AND to_value = ?2",
            params![task_id, id, name],
        )?;
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
            "UPDATE planning_task_comments SET body = ?2, edited_at_ms = ?3 WHERE id = ?1",
            params![found.id, text, now_ms],
        )?;
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
        "UPDATE planning_task_comments SET body = '', deleted_at_ms = ?2 WHERE id = ?1",
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
        "UPDATE planning_task_comments
         SET resolved_at_ms = CASE WHEN ?2 THEN coalesce(resolved_at_ms, ?3) END
         WHERE id = ?1",
        params![root, resolved, now_ms],
    )?;
    thread(conn, &root)
}

#[cfg(test)]
mod tests {
    use super::super::super::schema;
    use super::super::super::store::Refusal;
    use super::super::items::{self, NewItem};
    use super::super::types::{WorkActivityKind, WorkActor, WorkOwner, WorkPatch, WorkStatus};
    use super::{add, delete, page, Author};
    use rusqlite::Connection;

    const AT: i64 = 1_700_000_000_000;
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
        let root = add(&conn, &id, None, ME, "Who books the venue?", AT).expect("root");
        let reply = add(&conn, &id, Some(&root.id), ME, "Dana", AT + 1).expect("reply");

        let nested = add(&conn, &id, Some(&reply.id), ME, "Confirmed", AT + 2).expect("nested");

        assert_eq!(nested.parent_id.as_deref(), Some(root.id.as_str()));
        let threads = page(&conn, &id, 10, None).expect("threads").threads;
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].replies.len(), 2);
    }

    #[test]
    fn deleting_a_thread_root_keeps_its_replies_readable() {
        let conn = tracker();
        let id = task(&conn, "Launch");
        let root = add(&conn, &id, None, ME, "Who books the venue?", AT).expect("root");
        add(&conn, &id, Some(&root.id), ME, "Dana", AT + 1).expect("reply");

        delete(&conn, &root.id, ME, AT + 2).expect("deleted root");

        let threads = page(&conn, &id, 10, None).expect("threads").threads;
        assert_eq!(threads.len(), 1);
        assert!(threads[0].deleted);
        assert_eq!(threads[0].body, None);
        assert_eq!(threads[0].replies[0].body.as_deref(), Some("Dana"));
    }
}
