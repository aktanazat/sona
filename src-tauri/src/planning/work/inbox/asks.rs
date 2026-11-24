//! Whether an agent's comment waits on the user, judged once by the local
//! model and kept. The rule in [`super::rows::asks`] answers until a comment
//! is judged, and for good when no local model is ready.
//!
//! New judgments use only on-device Apple Intelligence when it is selected
//! and available. The inbox does not send comments to configured endpoints.

use crate::managers::history::HistoryManager;
use crate::meeting::processing::{first_json_value, MeetingTextGenerator, ReplyShape};
use crate::meeting::session::MeetingSessionManager;
use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

pub(crate) const ASKS_MIGRATION: &str = "CREATE TABLE planning_comment_asks (
    comment_id TEXT PRIMARY KEY NOT NULL,
    said_at_ms INTEGER NOT NULL,
    asks INTEGER NOT NULL CHECK(asks IN (0, 1)),
    model TEXT NOT NULL,
    judged_at_ms INTEGER NOT NULL
);
CREATE TRIGGER planning_comment_asks_delete AFTER DELETE ON planning_task_comments BEGIN
    DELETE FROM planning_comment_asks WHERE comment_id = old.id;
END;";

pub(crate) const ASK_DELETION_MIGRATION: &str = "CREATE TRIGGER planning_comment_asks_soft_delete
AFTER UPDATE OF deleted_at_ms ON planning_task_comments
WHEN new.deleted_at_ms IS NOT NULL
BEGIN
    DELETE FROM planning_comment_asks WHERE comment_id = new.id;
END;
DELETE FROM planning_comment_asks
WHERE comment_id IN (
    SELECT id FROM planning_task_comments WHERE deleted_at_ms IS NOT NULL
);";

/// Comments judged in one pass, so a first read of a long inbox does not
/// hold the model for minutes; the next read takes the next ones.
const PASS: usize = 24;
/// The most of a comment the model reads. An ask sits in the first or last
/// lines; the middle of a long report is not where it hides.
const READ_BYTES: usize = 1_600;

const SYSTEM: &str = "You read one comment that an AI agent left for its user on a task. \
Answer one thing: is the agent now waiting for the user to do or say something?

Answer {\"asks\": true} when the comment asks the user a question, asks the user to choose, \
confirm, approve, sign or provide something, or says the agent is stopped, holding, blocked or \
unsure until the user responds.

Answer {\"asks\": false} when the comment only reports what the agent did or found, asks a \
question and then answers it itself, repeats a question someone else asked, makes an offer the \
user can ignore, or waits on someone who is not the user.

Examples:
Comment: Do you want this filed under travel or under events?
{\"asks\": true}
Comment: Filed it under travel.
{\"asks\": false}
Comment: I have not sent the email; it goes out once you approve the draft.
{\"asks\": true}
Comment: Was the form submitted? Yes, at 9am.
{\"asks\": false}

Reply with the JSON object only.";

pub(crate) struct Comment {
    pub(crate) id: String,
    pub(crate) body: String,
    said_at_ms: i64,
}

/// Verdicts for live comments whose words have not changed.
pub(crate) fn judged(conn: &Connection) -> Result<HashMap<String, bool>> {
    let mut statement = conn.prepare(
        "SELECT a.comment_id, a.asks FROM planning_comment_asks a
         JOIN planning_task_comments c ON c.id = a.comment_id
         WHERE c.deleted_at_ms IS NULL
           AND a.said_at_ms = coalesce(c.edited_at_ms, c.created_at_ms)",
    )?;
    let verdicts = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(verdicts)
}

/// Comments behind unread inbox notices that have no verdict for their
/// current words, newest first.
pub(crate) fn unjudged(conn: &Connection, limit: usize) -> Result<Vec<Comment>> {
    let mut statement = conn.prepare(
        "SELECT c.id, c.body, coalesce(c.edited_at_ms, c.created_at_ms) FROM planning_task_comments c
         WHERE c.deleted_at_ms IS NULL
           AND c.id IN (SELECT comment_id FROM planning_notifications
                        WHERE kind = 'comment' AND read_at_ms IS NULL AND archived_at_ms IS NULL)
           AND NOT EXISTS (SELECT 1 FROM planning_comment_asks a
                           WHERE a.comment_id = c.id
                             AND a.said_at_ms = coalesce(c.edited_at_ms, c.created_at_ms))
         ORDER BY c.created_at_ms DESC, c.id
         LIMIT ?1",
    )?;
    let comments = statement
        .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
            Ok(Comment {
                id: row.get(0)?,
                body: row.get(1)?,
                said_at_ms: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(comments)
}

/// Keeps a verdict, unless the comment was edited or removed meanwhile.
pub(crate) fn keep(
    conn: &Connection,
    comment: &Comment,
    asks: bool,
    model: &str,
    now_ms: i64,
) -> Result<bool> {
    let kept = conn.execute(
        "INSERT OR REPLACE INTO planning_comment_asks (comment_id, said_at_ms, asks, model, judged_at_ms)
         SELECT c.id, ?2, ?3, ?4, ?5 FROM planning_task_comments c
         WHERE c.id = ?1 AND c.deleted_at_ms IS NULL
           AND coalesce(c.edited_at_ms, c.created_at_ms) = ?2",
        params![comment.id, comment.said_at_ms, asks, model, now_ms],
    )?;
    Ok(kept == 1)
}

fn read(body: &str) -> String {
    if body.len() <= READ_BYTES {
        return body.to_owned();
    }
    let half = READ_BYTES / 2;
    let head = (0..=half).rev().find(|at| body.is_char_boundary(*at));
    let tail = (body.len() - half..body.len()).find(|at| body.is_char_boundary(*at));
    match (head, tail) {
        (Some(head), Some(tail)) => format!("{}\n[...]\n{}", &body[..head], &body[tail..]),
        _ => body.to_owned(),
    }
}

/// The local model's verdict on one comment, or `None` when it gave none
/// that can be read.
pub(crate) fn judge(generator: &dyn MeetingTextGenerator, body: &str) -> Option<bool> {
    #[derive(Deserialize)]
    struct Verdict {
        asks: bool,
    }
    let reply = generator
        .generate(
            SYSTEM,
            &format!("Comment: {}", read(body)),
            64,
            ReplyShape::Json,
        )
        .ok()?;
    let start = reply.find('{')?;
    first_json_value::<Verdict>(&reply[start..])
        .ok()
        .map(|verdict| verdict.asks)
}

static JUDGING: AtomicBool = AtomicBool::new(false);

/// Judges the waiting comments off the caller's thread, one pass at a time.
/// Stops when the model stops answering and whenever
/// anything records, so the model is free for the recording.
pub(crate) fn catch_up(app: &AppHandle) {
    let Some(meetings) = app.try_state::<Arc<MeetingSessionManager>>() else {
        return;
    };
    let Some(history) = app.try_state::<Arc<HistoryManager>>() else {
        return;
    };
    if JUDGING.swap(true, Ordering::AcqRel) {
        return;
    }
    let processing = meetings.processing().clone();
    let history = history.inner().clone();
    let app = app.clone();
    std::thread::spawn(move || {
        let pass = || -> Option<()> {
            let waiting = crate::planning::history(&history, |conn| unjudged(conn, PASS)).ok()?;
            if waiting.is_empty() || crate::utils::capture_running(&app) {
                return None;
            }
            let generator = processing.inbox_apple_text_generator()?;
            let model = generator.model_version();
            let mut missed = 0;
            for comment in waiting {
                if crate::utils::capture_running(&app) {
                    return None;
                }
                // A comment the model cannot answer keeps the rule; three in
                // a row means the model is not answering at all.
                let Some(asks) = judge(&generator, &comment.body) else {
                    missed += 1;
                    if missed == 3 {
                        return None;
                    }
                    continue;
                };
                missed = 0;
                crate::planning::history(&history, |conn| {
                    keep(conn, &comment, asks, &model, super::now_ms())
                })
                .ok()?;
            }
            Some(())
        };
        pass();
        JUDGING.store(false, Ordering::Release);
    });
}

#[cfg(test)]
mod tests;
