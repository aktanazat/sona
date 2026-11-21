//! Emoji reactions on comments and tasks. Each person, Sona or agent puts
//! an emoji on a thing once; reacting again with it changes nothing.

use super::comments::{self, Author};
use super::items;
use super::types::{
    WorkActor, WorkCommentAuthor, WorkCommentSubject, WorkReaction, WorkReactionSubject,
};
use super::{run_history, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::planning::store::{self, Refusal};
use anyhow::Result;
use rusqlite::{params, Connection};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

/// Reactions leave with the task or the comment they are on.
pub(crate) const REACTIONS_MIGRATION: &str = "CREATE TABLE planning_reactions (
    id TEXT PRIMARY KEY NOT NULL,
    subject_kind TEXT NOT NULL CHECK(subject_kind IN ('comment', 'task')),
    subject_id TEXT NOT NULL,
    emoji TEXT NOT NULL CHECK(length(emoji) BETWEEN 1 AND 16),
    author_kind TEXT NOT NULL CHECK(author_kind IN ('me', 'sona', 'agent')),
    author_name TEXT CHECK(author_name IS NULL OR length(trim(author_name)) > 0),
    created_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX planning_reactions_once ON planning_reactions(subject_kind, subject_id,
    emoji, author_kind, lower(coalesce(author_name, '')));
CREATE INDEX planning_reactions_subject
    ON planning_reactions(subject_kind, subject_id, created_at_ms);
CREATE TRIGGER planning_reactions_task_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_reactions WHERE subject_kind = 'task' AND subject_id = old.id;
END;
CREATE TRIGGER planning_reactions_task_comment_delete AFTER DELETE ON planning_task_comments
BEGIN
    DELETE FROM planning_reactions WHERE subject_kind = 'comment' AND subject_id = old.id;
END;
CREATE TRIGGER planning_reactions_comment_delete AFTER DELETE ON planning_comments BEGIN
    DELETE FROM planning_reactions WHERE subject_kind = 'comment' AND subject_id = old.id;
END;";

/// The emoji in `value`: one short run of symbols with at least one
/// outside ASCII, so a word or a stray letter is refused.
pub(crate) fn emoji(value: &str) -> Result<&str> {
    let value = value.trim();
    let fits = (1..=16).contains(&value.chars().count())
        && !value.is_ascii()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c.is_ascii_alphabetic());
    if !fits {
        return Err(Refusal::Invalid.into());
    }
    Ok(value)
}

/// `id` as `subject` keeps it, refused when it is gone. A deleted comment
/// takes no reactions.
fn subject_id(conn: &Connection, subject: WorkReactionSubject, id: &str) -> Result<String> {
    match subject {
        WorkReactionSubject::Task => Ok(items::item(conn, &store::uuid(id)?)?.id),
        WorkReactionSubject::Comment => Ok(comments::live(conn, id)?.id),
    }
}

/// The reactions on `id`, each emoji in the order it was first used.
pub(crate) fn of(
    conn: &Connection,
    subject: WorkReactionSubject,
    id: &str,
) -> Result<Vec<WorkReaction>> {
    let mut statement = conn.prepare_cached(
        "SELECT emoji, author_kind, author_name FROM planning_reactions
         WHERE subject_kind = ?1 AND subject_id = ?2
         ORDER BY created_at_ms, id",
    )?;
    let rows = statement.query_map(params![subject.as_str(), id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            WorkCommentAuthor {
                kind: row.get(1)?,
                name: row.get(2)?,
            },
        ))
    })?;
    let mut reactions: Vec<WorkReaction> = Vec::new();
    for row in rows {
        let (emoji, author) = row?;
        match reactions
            .iter_mut()
            .find(|reaction| reaction.emoji == emoji)
        {
            Some(reaction) => reaction.by.push(author),
            None => reactions.push(WorkReaction {
                emoji,
                by: vec![author],
            }),
        }
    }
    Ok(reactions)
}

/// Puts `author`'s `emoji` on `id`, or with `on` false takes it off, and
/// hands back every reaction on it.
pub(crate) fn react(
    conn: &Connection,
    author: Author<'_>,
    subject: WorkReactionSubject,
    id: &str,
    value: &str,
    on: bool,
    now_ms: i64,
) -> Result<Vec<WorkReaction>> {
    let id = subject_id(conn, subject, id)?;
    let value = emoji(value)?;
    let name = author.name();
    if on {
        conn.execute(
            "INSERT INTO planning_reactions
                (id, subject_kind, subject_id, emoji, author_kind, author_name, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT DO NOTHING",
            params![
                Uuid::new_v4().to_string(),
                subject.as_str(),
                id,
                value,
                author.actor.as_str(),
                name,
                now_ms
            ],
        )?;
    } else {
        conn.execute(
            "DELETE FROM planning_reactions
             WHERE subject_kind = ?1 AND subject_id = ?2 AND emoji = ?3 AND author_kind = ?4
               AND lower(coalesce(author_name, '')) = lower(coalesce(?5, ''))",
            params![subject.as_str(), id, value, author.actor.as_str(), name],
        )?;
    }
    of(conn, subject, &id)
}

/// The task a reaction on `id` belongs to, for telling the windows.
fn task_of(conn: &Connection, subject: WorkReactionSubject, id: &str) -> Result<Option<String>> {
    Ok(match subject {
        WorkReactionSubject::Task => Some(items::item(conn, &store::uuid(id)?)?.id),
        WorkReactionSubject::Comment => {
            let comment = comments::live(conn, id)?;
            (comment.subject == WorkCommentSubject::Task).then_some(comment.item_id)
        }
    })
}

/// [`of`], read for the app.
pub(crate) fn listed(
    app: &AppHandle,
    subject: WorkReactionSubject,
    id: &str,
) -> std::result::Result<Vec<WorkReaction>, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        of(conn, subject, &subject_id(conn, subject, id)?)
    })
}

/// [`react`] under `operation_id`, telling the windows showing its task.
pub(crate) fn react_as(
    app: &AppHandle,
    author: Author<'_>,
    operation_id: &str,
    subject: WorkReactionSubject,
    id: &str,
    emoji: &str,
    on: bool,
) -> std::result::Result<Vec<WorkReaction>, String> {
    let now = super::now_ms();
    let agent = matches!(author.actor, WorkActor::Agent).then(|| author.name());
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_react",
            &(subject, id, emoji, on, author.actor, agent),
            now,
            |tx| {
                let value = react(tx, author, subject, id, emoji, on, now)?;
                let mut effects = Effects::default();
                effects.items.extend(task_of(tx, subject, id)?);
                Ok(Done { value, effects })
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::schema;
    use super::super::comments::{self, Author};
    use super::super::items::{self, NewItem};
    use super::super::types::{
        WorkActor, WorkCommentSubject, WorkOwner, WorkReactionSubject, WorkStatus,
    };
    use super::{of, react};
    use crate::planning::store::Refusal;
    use rusqlite::Connection;

    const AT: i64 = 1_700_000_000_000;
    const THUMBS_UP: &str = "\u{1F44D}";
    const ME: Author<'static> = Author {
        actor: WorkActor::Me,
        agent: None,
    };
    const CLAUDE: Author<'static> = Author {
        actor: WorkActor::Agent,
        agent: Some("claude"),
    };

    fn task(conn: &Connection) -> String {
        items::insert(
            conn,
            &NewItem {
                title: "Book the venue",
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

    #[test]
    fn each_author_reacts_with_an_emoji_once_and_takes_only_their_own_off() {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        let id = task(&conn);
        let comment = comments::add(
            &conn,
            WorkCommentSubject::Task,
            &id,
            None,
            CLAUDE,
            "Booked for Friday",
            AT,
        )
        .expect("an agent's comment");
        let on = WorkReactionSubject::Comment;

        react(&conn, ME, on, &comment.id, THUMBS_UP, true, AT + 1).expect("mine");
        react(&conn, ME, on, &comment.id, THUMBS_UP, true, AT + 2).expect("mine again");
        react(&conn, CLAUDE, on, &comment.id, THUMBS_UP, true, AT + 3).expect("claude's");
        let left = react(&conn, CLAUDE, on, &comment.id, THUMBS_UP, false, AT + 4).expect("off");

        assert_eq!(left.len(), 1);
        assert_eq!(left[0].by.len(), 1);
        assert_eq!(left[0].by[0].kind, WorkActor::Me);
        let thread = comments::thread(&conn, &comment.id).expect("the thread");
        assert_eq!(thread.reactions, left);
        let word = react(
            &conn,
            ME,
            WorkReactionSubject::Task,
            &id,
            "ok",
            true,
            AT + 5,
        );
        assert_eq!(
            word.expect_err("a word is no emoji")
                .downcast_ref::<Refusal>(),
            Some(&Refusal::Invalid)
        );
        conn.execute("DELETE FROM planning_tasks WHERE id = ?1", [&id])
            .expect("task deleted");
        assert!(of(&conn, on, &comment.id).expect("reactions").is_empty());
    }
}
