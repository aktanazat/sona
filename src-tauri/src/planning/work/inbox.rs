//! The tasks the user is subscribed to, and the inbox of what others did
//! to them: status changes, comments, mentions, assignments and any field
//! an agent changed. The user is the inbox's one reader, so a change the
//! user made notifies no one. Creating a task, being made its owner or
//! being mentioned on it subscribes the user. Sona posts its own notices
//! too: a reminder whose time came, work the user owes that is due today,
//! and an automation run that finished or failed, and a weekly digest of
//! the user's projects. Each of those carries a key, so it is posted once
//! however often it is offered, across restarts.

use super::digest::ProjectDigest;
use super::types::{WorkActivityKind, WorkActor};
use super::{now_ms, run_history, store, Change, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::planning::store::Refusal;
use anyhow::Result;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    Status,
    Comment,
    Mention,
    Assigned,
    /// An agent changed another field; `field` names it.
    Changed,
    Reminder,
    Due,
    /// An automation run ended; `to` is `done` or `failed`.
    Automation,
    /// A digest of the user's projects; `digest` holds it.
    Digest,
}

crate::planning::types::sql_enum!(NotificationKind {
    Status => "status",
    Comment => "comment",
    Mention => "mention",
    Assigned => "assigned",
    Changed => "changed",
    Reminder => "reminder",
    Due => "due",
    Automation => "automation",
    Digest => "digest",
});

/// Notices without a task, kinds for a field change, Sona's own posts
/// and automation runs, and a key that posts each of those once.
pub(crate) const INBOX_NOTICES_MIGRATION: &str = "DROP TRIGGER planning_task_lifecycle_delete;
CREATE TABLE planning_notifications_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT,
    kind TEXT NOT NULL CHECK(kind IN ('status', 'comment', 'mention', 'assigned', 'changed',
        'reminder', 'due', 'automation')),
    actor TEXT NOT NULL CHECK(actor IN ('sona', 'agent')),
    agent TEXT CHECK(agent IS NULL OR length(trim(agent)) > 0),
    activity_id TEXT,
    comment_id TEXT,
    field TEXT,
    run_id TEXT,
    event_key TEXT UNIQUE,
    from_value TEXT,
    to_value TEXT,
    created_at_ms INTEGER NOT NULL,
    read_at_ms INTEGER,
    snoozed_until_ms INTEGER,
    archived_at_ms INTEGER,
    CHECK(task_id IS NOT NULL OR run_id IS NOT NULL)
);
INSERT INTO planning_notifications_next (rowid, id, task_id, kind, actor, activity_id, comment_id,
        from_value, to_value, created_at_ms, read_at_ms, snoozed_until_ms, archived_at_ms)
    SELECT rowid, id, task_id, kind, actor, activity_id, comment_id, from_value, to_value,
        created_at_ms, read_at_ms, snoozed_until_ms, archived_at_ms
    FROM planning_notifications;
DROP TABLE planning_notifications;
ALTER TABLE planning_notifications_next RENAME TO planning_notifications;
CREATE INDEX planning_notifications_created ON planning_notifications(created_at_ms);
CREATE INDEX planning_notifications_task ON planning_notifications(task_id);
CREATE TRIGGER planning_task_lifecycle_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_task_versions WHERE task_id = old.id;
    DELETE FROM planning_task_subscriptions WHERE task_id = old.id;
    DELETE FROM planning_notifications WHERE task_id = old.id;
    DELETE FROM planning_favorites WHERE kind = 'task' AND target_id = old.id;
END;
CREATE TRIGGER planning_notifications_run_delete AFTER DELETE ON planning_loop_runs BEGIN
    DELETE FROM planning_notifications WHERE run_id = old.id;
END;";

/// A notice may be a project digest, with no task or run, holding its
/// projects as they stood when it was posted.
pub(crate) const DIGEST_NOTICES_MIGRATION: &str = "DROP TRIGGER planning_task_lifecycle_delete;
DROP TRIGGER planning_notifications_run_delete;
CREATE TABLE planning_notifications_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT,
    kind TEXT NOT NULL CHECK(kind IN ('status', 'comment', 'mention', 'assigned', 'changed',
        'reminder', 'due', 'automation', 'digest')),
    actor TEXT NOT NULL CHECK(actor IN ('sona', 'agent')),
    agent TEXT CHECK(agent IS NULL OR length(trim(agent)) > 0),
    activity_id TEXT,
    comment_id TEXT,
    field TEXT,
    run_id TEXT,
    event_key TEXT UNIQUE,
    from_value TEXT,
    to_value TEXT,
    digest TEXT,
    created_at_ms INTEGER NOT NULL,
    read_at_ms INTEGER,
    snoozed_until_ms INTEGER,
    archived_at_ms INTEGER,
    CHECK((kind = 'digest') = (digest IS NOT NULL)),
    CHECK(task_id IS NOT NULL OR run_id IS NOT NULL OR kind = 'digest')
);
INSERT INTO planning_notifications_next (rowid, id, task_id, kind, actor, agent, activity_id,
        comment_id, field, run_id, event_key, from_value, to_value, created_at_ms, read_at_ms,
        snoozed_until_ms, archived_at_ms)
    SELECT rowid, id, task_id, kind, actor, agent, activity_id, comment_id, field, run_id,
        event_key, from_value, to_value, created_at_ms, read_at_ms, snoozed_until_ms,
        archived_at_ms
    FROM planning_notifications;
DROP TABLE planning_notifications;
ALTER TABLE planning_notifications_next RENAME TO planning_notifications;
CREATE INDEX planning_notifications_created ON planning_notifications(created_at_ms);
CREATE INDEX planning_notifications_task ON planning_notifications(task_id);
CREATE TRIGGER planning_task_lifecycle_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_task_versions WHERE task_id = old.id;
    DELETE FROM planning_task_subscriptions WHERE task_id = old.id;
    DELETE FROM planning_notifications WHERE task_id = old.id;
    DELETE FROM planning_favorites WHERE kind = 'task' AND target_id = old.id;
END;
CREATE TRIGGER planning_notifications_run_delete AFTER DELETE ON planning_loop_runs BEGIN
    DELETE FROM planning_notifications WHERE run_id = old.id;
END;";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscribeReason {
    Subscribed,
    Created,
    Assigned,
    Mentioned,
}

crate::planning::types::sql_enum!(SubscribeReason {
    Subscribed => "subscribed",
    Created => "created",
    Assigned => "assigned",
    Mentioned => "mentioned",
});

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Subscription {
    pub(crate) item_id: String,
    pub(crate) reason: SubscribeReason,
    pub(crate) created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Notification {
    pub(crate) id: String,
    /// None only for an automation run about no task.
    pub(crate) item_id: Option<String>,
    pub(crate) kind: NotificationKind,
    pub(crate) actor: WorkActor,
    /// The agent, when one named itself.
    pub(crate) agent: Option<String>,
    pub(crate) comment_id: Option<String>,
    /// For a field change, the history kind of what changed, or `reminder`.
    pub(crate) field: Option<String>,
    pub(crate) automation: Option<NoticeAutomation>,
    pub(crate) from: Option<String>,
    pub(crate) to: Option<String>,
    pub(crate) created_at_ms: i64,
    pub(crate) read_at_ms: Option<i64>,
    pub(crate) snoozed_until_ms: Option<i64>,
    pub(crate) archived_at_ms: Option<i64>,
    /// For a digest, the projects it covers.
    pub(crate) digest: Option<ProjectDigest>,
}

/// The automation run an automation notice is about.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NoticeAutomation {
    pub(crate) id: String,
    pub(crate) run_id: String,
    pub(crate) name: String,
    pub(crate) error: Option<String>,
}

/// What a mark does to notifications.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "mark", content = "until_ms")]
pub(crate) enum Mark {
    Read,
    Unread,
    Archive,
    Unarchive,
    /// Hidden until then; none wakes it.
    Snooze(Option<i64>),
}

/// Which notifications a list shows besides the unread and read ones that
/// are awake and not archived.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Shown {
    pub(crate) unread_only: bool,
    pub(crate) snoozed: bool,
    pub(crate) archived: bool,
}

pub(crate) fn subscribed(conn: &Connection, task_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_task_subscriptions WHERE task_id = ?1)",
        [task_id],
        |row| row.get(0),
    )?)
}

/// Subscribes the user, keeping the first reason. True when it changed.
pub(crate) fn subscribe(
    conn: &Connection,
    task_id: &str,
    reason: SubscribeReason,
    now_ms: i64,
) -> Result<bool> {
    Ok(conn.execute(
        "INSERT INTO planning_task_subscriptions (task_id, reason, created_at_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(task_id) DO NOTHING",
        params![task_id, reason.as_str(), now_ms],
    )? > 0)
}

pub(crate) fn unsubscribe(conn: &Connection, task_id: &str) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM planning_task_subscriptions WHERE task_id = ?1",
        [task_id],
    )? > 0)
}

pub(crate) fn subscriptions(conn: &Connection) -> Result<Vec<Subscription>> {
    let mut statement = conn.prepare(
        "SELECT task_id, reason, created_at_ms FROM planning_task_subscriptions
         ORDER BY created_at_ms DESC, task_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Subscription {
            item_id: row.get(0)?,
            reason: row.get(1)?,
            created_at_ms: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

struct Notice<'a> {
    task_id: Option<&'a str>,
    kind: NotificationKind,
    actor: WorkActor,
    agent: Option<&'a str>,
    activity_id: Option<&'a str>,
    comment_id: Option<&'a str>,
    field: Option<&'a str>,
    run_id: Option<&'a str>,
    key: Option<&'a str>,
    from: Option<&'a str>,
    to: Option<&'a str>,
    digest: Option<&'a str>,
}

impl<'a> Notice<'a> {
    const fn new(task_id: Option<&'a str>, kind: NotificationKind, actor: WorkActor) -> Self {
        Self {
            task_id,
            kind,
            actor,
            agent: None,
            activity_id: None,
            comment_id: None,
            field: None,
            run_id: None,
            key: None,
            from: None,
            to: None,
            digest: None,
        }
    }
}

/// Posts a notice; one with a key already posted is left alone. True when
/// it was posted.
fn notify(conn: &Connection, notice: &Notice<'_>, now_ms: i64) -> Result<bool> {
    Ok(conn.execute(
        "INSERT INTO planning_notifications (id, task_id, kind, actor, agent, activity_id,
            comment_id, field, run_id, event_key, from_value, to_value, digest, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(event_key) DO NOTHING",
        params![
            Uuid::new_v4().to_string(),
            notice.task_id,
            notice.kind.as_str(),
            notice.actor.as_str(),
            notice.agent,
            notice.activity_id,
            notice.comment_id,
            notice.field,
            notice.run_id,
            notice.key,
            notice.from,
            notice.to,
            notice.digest,
            now_ms
        ],
    )? > 0)
}

fn owned_by_user(conn: &Connection, task_id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT owner_is_self FROM planning_tasks WHERE id = ?1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// One history entry as it is written.
pub(crate) struct Entry<'a> {
    pub(crate) task_id: &'a str,
    pub(crate) activity_id: &'a str,
    pub(crate) actor: WorkActor,
    pub(crate) kind: WorkActivityKind,
    pub(crate) from: Option<&'a str>,
    pub(crate) to: Option<&'a str>,
}

/// Subscribes and notifies for one history entry as it is written.
pub(crate) fn on_activity(conn: &Connection, entry: &Entry<'_>, now_ms: i64) -> Result<()> {
    let Entry {
        task_id,
        activity_id,
        actor,
        kind,
        from,
        to,
    } = *entry;
    let notice = |kind: NotificationKind| Notice {
        activity_id: Some(activity_id),
        comment_id: if kind == NotificationKind::Comment {
            to
        } else {
            None
        },
        from,
        to,
        ..Notice::new(Some(task_id), kind, actor)
    };
    match kind {
        WorkActivityKind::Created => {
            let owned = owned_by_user(conn, task_id)?;
            if actor == WorkActor::Me {
                subscribe(conn, task_id, SubscribeReason::Created, now_ms)?;
            } else if owned {
                subscribe(conn, task_id, SubscribeReason::Assigned, now_ms)?;
                if actor == WorkActor::Agent {
                    let assigned = Notice {
                        from: None,
                        to: Some("self"),
                        ..notice(NotificationKind::Assigned)
                    };
                    notify(conn, &assigned, now_ms)?;
                }
            }
        }
        WorkActivityKind::Owner if to == Some("self") => {
            subscribe(conn, task_id, SubscribeReason::Assigned, now_ms)?;
            if actor != WorkActor::Me {
                notify(conn, &notice(NotificationKind::Assigned), now_ms)?;
            }
        }
        WorkActivityKind::Status
        | WorkActivityKind::Accepted
        | WorkActivityKind::Declined
        | WorkActivityKind::Duplicate
            if actor != WorkActor::Me && subscribed(conn, task_id)? =>
        {
            notify(conn, &notice(NotificationKind::Status), now_ms)?;
        }
        WorkActivityKind::Comment if actor != WorkActor::Me && subscribed(conn, task_id)? => {
            notify(conn, &notice(NotificationKind::Comment), now_ms)?;
        }
        WorkActivityKind::Priority
        | WorkActivityKind::Owner
        | WorkActivityKind::Due
        | WorkActivityKind::Project
        | WorkActivityKind::Title
        | WorkActivityKind::Details
        | WorkActivityKind::Labels
        | WorkActivityKind::Archived
        | WorkActivityKind::Restored
        | WorkActivityKind::Snoozed
        | WorkActivityKind::Relation
        | WorkActivityKind::Recurrence
        | WorkActivityKind::Block
        | WorkActivityKind::Parent
        | WorkActivityKind::Attachment
            if actor == WorkActor::Agent && subscribed(conn, task_id)? =>
        {
            let changed = Notice {
                field: Some(kind.as_str()),
                ..notice(NotificationKind::Changed)
            };
            notify(conn, &changed, now_ms)?;
        }
        _ => {}
    }
    Ok(())
}

/// An agent set or cleared a reminder on a task the user follows; `at_ms`
/// is the new time, none when cleared.
pub(crate) fn reminder_changed(
    conn: &Connection,
    task_id: &str,
    agent: Option<&str>,
    at_ms: Option<i64>,
    now_ms: i64,
) -> Result<()> {
    if !subscribed(conn, task_id)? {
        return Ok(());
    }
    let to = at_ms.map(|at| at.to_string());
    let notice = Notice {
        agent,
        field: Some("reminder"),
        to: to.as_deref(),
        ..Notice::new(Some(task_id), NotificationKind::Changed, WorkActor::Agent)
    };
    notify(conn, &notice, now_ms)?;
    Ok(())
}

/// Sona's word that a task's time has come: its reminder, `at_ms` set at
/// `set_at_ms`, or the morning of the day it is due at `at_ms`. Posted once
/// for each; true when this call posted it.
pub(crate) fn remind(
    conn: &Connection,
    task_id: &str,
    kind: Remind,
    at_ms: i64,
    now_ms: i64,
) -> Result<bool> {
    let (kind, key) = match kind {
        Remind::Reminder { set_at_ms } => (
            NotificationKind::Reminder,
            format!("reminder:{task_id}:{at_ms}:{set_at_ms}"),
        ),
        Remind::Due => (NotificationKind::Due, format!("due:{task_id}:{at_ms}")),
    };
    let at = at_ms.to_string();
    let notice = Notice {
        key: Some(&key),
        to: Some(&at),
        ..Notice::new(Some(task_id), kind, WorkActor::Sona)
    };
    notify(conn, &notice, now_ms)
}

/// What brought a task's time: a reminder, or its due day.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Remind {
    Reminder { set_at_ms: i64 },
    Due,
}

/// An automation run that finished or failed. Posted once per run; a run
/// still waiting or skipped posts nothing.
pub(crate) fn automation_ended(conn: &Connection, run_id: &str, now_ms: i64) -> Result<bool> {
    let run: Option<(Option<String>, String)> = conn
        .query_row(
            "SELECT task_id, status FROM planning_loop_runs
             WHERE id = ?1 AND status IN ('done', 'failed')",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((task_id, status)) = run else {
        return Ok(false);
    };
    let key = format!("automation:{run_id}");
    let notice = Notice {
        agent: Some(super::loops::LOOP_AGENT),
        run_id: Some(run_id),
        key: Some(&key),
        to: Some(&status),
        ..Notice::new(task_id.as_deref(), NotificationKind::Automation, WorkActor::Agent)
    };
    notify(conn, &notice, now_ms)
}

/// Posts a project digest from Sona, once per key; none posts every time.
/// The notice when it was posted.
pub(crate) fn digest_posted(
    conn: &Connection,
    digest: &ProjectDigest,
    key: Option<&str>,
    now_ms: i64,
) -> Result<Option<Notification>> {
    let json = serde_json::to_string(digest)?;
    let notice = Notice {
        key,
        digest: Some(&json),
        ..Notice::new(None, NotificationKind::Digest, WorkActor::Sona)
    };
    if !notify(conn, &notice, now_ms)? {
        return Ok(None);
    }
    let id: String = conn.query_row(
        "SELECT id FROM planning_notifications WHERE rowid = ?1",
        [conn.last_insert_rowid()],
        |row| row.get(0),
    )?;
    notification(conn, &id).map(Some)
}

/// Whether a weekly digest arrived on schedule at or after `since_ms`; one
/// asked for by hand does not count.
pub(crate) fn digest_since(conn: &Connection, since_ms: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_notifications
                       WHERE kind = 'digest' AND event_key IS NOT NULL
                         AND created_at_ms >= ?1)",
        [since_ms],
        |row| row.get(0),
    )?)
}

/// Whether `body` mentions the user: `@me`, or `@` and their first name or
/// full name.
pub(crate) fn mentions_user(body: &str, user_name: Option<&str>) -> bool {
    let lower = body.to_lowercase();
    let mut names = vec!["me".to_owned()];
    if let Some(name) = user_name.map(str::trim).filter(|name| !name.is_empty()) {
        names.push(name.to_lowercase());
        if let Some(first) = name.split_whitespace().next() {
            names.push(first.to_lowercase());
        }
    }
    lower.match_indices('@').any(|(at, _)| {
        let rest = &lower[at + 1..];
        names.iter().any(|name| {
            rest.starts_with(name.as_str())
                && rest[name.len()..]
                    .chars()
                    .next()
                    .is_none_or(|next| !next.is_alphanumeric())
        })
    })
}

/// A comment someone else wrote that mentions the user: they are
/// subscribed, and its notice reads as a mention.
pub(crate) fn mentioned(
    conn: &Connection,
    task_id: &str,
    comment_id: &str,
    actor: WorkActor,
    now_ms: i64,
) -> Result<()> {
    if actor == WorkActor::Me {
        return Ok(());
    }
    subscribe(conn, task_id, SubscribeReason::Mentioned, now_ms)?;
    let upgraded = conn.execute(
        "UPDATE planning_notifications SET kind = 'mention'
         WHERE comment_id = ?1 AND kind = 'comment'",
        [comment_id],
    )?;
    if upgraded == 0 {
        let activity_id: Option<String> = conn
            .query_row(
                "SELECT id FROM planning_task_activity
                 WHERE task_id = ?1 AND kind = 'comment' AND to_value = ?2",
                params![task_id, comment_id],
                |row| row.get(0),
            )
            .optional()?;
        notify(
            conn,
            &Notice {
                activity_id: activity_id.as_deref(),
                comment_id: Some(comment_id),
                to: Some(comment_id),
                ..Notice::new(Some(task_id), NotificationKind::Mention, actor)
            },
            now_ms,
        )?;
    }
    Ok(())
}

const COLUMNS: &str = "x.id, x.task_id, x.kind, x.actor, coalesce(a.agent, x.agent), x.comment_id,
    x.field, x.run_id, r.loop_id, l.name, r.error, x.from_value, x.to_value, x.created_at_ms,
    x.read_at_ms, x.snoozed_until_ms, x.archived_at_ms, x.digest";

const FROM: &str = "planning_notifications x
    LEFT JOIN planning_activity_agents a ON a.activity_id = x.activity_id
    LEFT JOIN planning_loop_runs r ON r.id = x.run_id
    LEFT JOIN planning_loops l ON l.id = r.loop_id";

fn read(row: &Row<'_>) -> rusqlite::Result<Notification> {
    let run_id: Option<String> = row.get(7)?;
    let loop_id: Option<String> = row.get(8)?;
    let automation = match (run_id, loop_id) {
        (Some(run_id), Some(id)) => Some(NoticeAutomation {
            id,
            run_id,
            name: row.get(9)?,
            error: row.get(10)?,
        }),
        _ => None,
    };
    let digest = row
        .get::<_, Option<String>>(17)?
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(17, rusqlite::types::Type::Text, Box::new(error))
        })?;
    Ok(Notification {
        id: row.get(0)?,
        item_id: row.get(1)?,
        kind: row.get(2)?,
        actor: row.get(3)?,
        agent: row.get(4)?,
        comment_id: row.get(5)?,
        field: row.get(6)?,
        automation,
        from: row.get(11)?,
        to: row.get(12)?,
        created_at_ms: row.get(13)?,
        read_at_ms: row.get(14)?,
        snoozed_until_ms: row.get(15)?,
        archived_at_ms: row.get(16)?,
        digest,
    })
}

/// Notifications, newest first.
pub(crate) fn notifications(
    conn: &Connection,
    shown: Shown,
    now_ms: i64,
) -> Result<Vec<Notification>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM {FROM}
         WHERE (?1 = 0 OR x.read_at_ms IS NULL)
           AND (?2 OR x.snoozed_until_ms IS NULL OR x.snoozed_until_ms <= ?4)
           AND (?3 OR x.archived_at_ms IS NULL)
         ORDER BY x.created_at_ms DESC, x.rowid DESC"
    ))?;
    let rows = statement.query_map(
        params![shown.unread_only, shown.snoozed, shown.archived, now_ms],
        read,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn notification(conn: &Connection, id: &str) -> Result<Notification> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM {FROM} WHERE x.id = ?1"),
        [id],
        read,
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

pub(crate) fn mark(conn: &Connection, id: &str, mark: Mark, now_ms: i64) -> Result<Notification> {
    let id = store::uuid(id)?;
    notification(conn, &id)?;
    let sql = match mark {
        Mark::Read => "UPDATE planning_notifications SET read_at_ms = coalesce(read_at_ms, ?2) WHERE id = ?1",
        Mark::Unread => "UPDATE planning_notifications SET read_at_ms = NULL WHERE id = ?1 AND ?2 IS NOT NULL",
        Mark::Archive => {
            "UPDATE planning_notifications SET archived_at_ms = coalesce(archived_at_ms, ?2) WHERE id = ?1"
        }
        Mark::Unarchive => {
            "UPDATE planning_notifications SET archived_at_ms = NULL WHERE id = ?1 AND ?2 IS NOT NULL"
        }
        Mark::Snooze(_) => "UPDATE planning_notifications SET snoozed_until_ms = ?2 WHERE id = ?1",
    };
    let value = match mark {
        Mark::Snooze(until_ms) => until_ms,
        _ => Some(now_ms),
    };
    conn.execute(sql, params![id, value])?;
    notification(conn, &id)
}

fn marked(
    history: &mut Connection,
    change: Change<'_>,
    ids: &[String],
    how: Mark,
) -> Result<Done<Vec<Notification>>> {
    store::mutate(
        history,
        change.operation_id,
        "work_inbox_mark",
        &(ids, how),
        change.at_ms,
        |tx| {
            let value = super::unique(ids)?
                .iter()
                .map(|id| mark(tx, id, how, change.at_ms))
                .collect::<Result<Vec<_>>>()?;
            Ok(Done {
                value,
                effects: Effects::default(),
            })
        },
    )
}

/// Marks notifications read or unread, archives or brings them back, or
/// snoozes them: every one, or none.
pub(crate) fn mark_as(
    app: &AppHandle,
    operation_id: &str,
    ids: &[String],
    how: Mark,
) -> Result<Vec<Notification>, String> {
    let change = Change::now(operation_id, WorkActor::Me);
    run_history(app, |history| marked(history, change, ids, how))
}

fn subscribing(
    history: &mut Connection,
    change: Change<'_>,
    ids: &[String],
    on: bool,
) -> Result<Done<Vec<String>>> {
    store::mutate(
        history,
        change.operation_id,
        "work_subscribe",
        &(ids, on),
        change.at_ms,
        |tx| {
            let mut value = Vec::new();
            for id in super::unique(ids)? {
                super::items::item(tx, &id)?;
                let changed = if on {
                    subscribe(tx, &id, SubscribeReason::Subscribed, change.at_ms)?
                } else {
                    unsubscribe(tx, &id)?
                };
                if changed {
                    value.push(id);
                }
            }
            Ok(Done {
                value,
                effects: Effects::default(),
            })
        },
    )
}

/// Subscribes the user to tasks, or unsubscribes them. Returns the tasks
/// that changed.
pub(crate) fn subscribe_as(
    app: &AppHandle,
    operation_id: &str,
    ids: &[String],
    on: bool,
) -> Result<Vec<String>, String> {
    let change = Change::now(operation_id, WorkActor::Me);
    run_history(app, |history| subscribing(history, change, ids, on))
}

/// The inbox as the app shows it: the notifications asked for and how many
/// are unread.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Inbox {
    unread: usize,
    notifications: Vec<Notification>,
}

fn reading<T>(app: &AppHandle, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| work(conn))
}

pub(crate) fn work_inbox(app: AppHandle, shown: Shown) -> Result<Inbox, String> {
    reading(&app, |conn| {
        let now = now_ms();
        let unread = Shown {
            unread_only: true,
            ..Shown::default()
        };
        Ok(Inbox {
            unread: notifications(conn, unread, now)?.len(),
            notifications: notifications(conn, shown, now)?,
        })
    })
}

pub(crate) fn work_inbox_mark(
    app: AppHandle,
    operation_id: String,
    ids: Vec<String>,
    mark: &str,
    until_ms: Option<i64>,
) -> Result<Vec<Notification>, String> {
    let how = match mark {
        "read" => Mark::Read,
        "unread" => Mark::Unread,
        "archive" => Mark::Archive,
        "unarchive" => Mark::Unarchive,
        "snooze" => Mark::Snooze(until_ms),
        other => return Err(format!("Unknown inbox mark `{other}`.")),
    };
    mark_as(&app, &operation_id, &ids, how)
}

pub(crate) fn work_subscribe(
    app: AppHandle,
    operation_id: String,
    ids: Vec<String>,
    on: bool,
) -> Result<Vec<String>, String> {
    subscribe_as(&app, &operation_id, &ids, on)
}

pub(crate) fn work_subscriptions(app: AppHandle) -> Result<Vec<Subscription>, String> {
    reading(&app, subscriptions)
}
