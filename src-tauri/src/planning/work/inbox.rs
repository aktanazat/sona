//! The tasks the user is subscribed to, and the inbox of what others did
//! to them: status changes, comments, mentions and assignments. The user is
//! the inbox's one reader, so a change the user made notifies no one.
//! Creating a task, being made its owner or being mentioned on it
//! subscribes the user.

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
}

crate::planning::types::sql_enum!(NotificationKind {
    Status => "status",
    Comment => "comment",
    Mention => "mention",
    Assigned => "assigned",
});

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
    pub(crate) item_id: String,
    pub(crate) kind: NotificationKind,
    pub(crate) actor: WorkActor,
    /// The agent, when one named itself.
    pub(crate) agent: Option<String>,
    pub(crate) comment_id: Option<String>,
    pub(crate) from: Option<String>,
    pub(crate) to: Option<String>,
    pub(crate) created_at_ms: i64,
    pub(crate) read_at_ms: Option<i64>,
    pub(crate) snoozed_until_ms: Option<i64>,
    pub(crate) archived_at_ms: Option<i64>,
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
    task_id: &'a str,
    kind: NotificationKind,
    actor: WorkActor,
    activity_id: Option<&'a str>,
    comment_id: Option<&'a str>,
    from: Option<&'a str>,
    to: Option<&'a str>,
}

fn notify(conn: &Connection, notice: &Notice<'_>, now_ms: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO planning_notifications (id, task_id, kind, actor, activity_id, comment_id,
            from_value, to_value, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            Uuid::new_v4().to_string(),
            notice.task_id,
            notice.kind.as_str(),
            notice.actor.as_str(),
            notice.activity_id,
            notice.comment_id,
            notice.from,
            notice.to,
            now_ms
        ],
    )?;
    Ok(())
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
        task_id,
        kind,
        actor,
        activity_id: Some(activity_id),
        comment_id: if kind == NotificationKind::Comment {
            to
        } else {
            None
        },
        from,
        to,
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
        _ => {}
    }
    Ok(())
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
                task_id,
                kind: NotificationKind::Mention,
                actor,
                activity_id: activity_id.as_deref(),
                comment_id: Some(comment_id),
                from: None,
                to: Some(comment_id),
            },
            now_ms,
        )?;
    }
    Ok(())
}

const COLUMNS: &str = "x.id, x.task_id, x.kind, x.actor, a.agent, x.comment_id, x.from_value,
    x.to_value, x.created_at_ms, x.read_at_ms, x.snoozed_until_ms, x.archived_at_ms";

fn read(row: &Row<'_>) -> rusqlite::Result<Notification> {
    Ok(Notification {
        id: row.get(0)?,
        item_id: row.get(1)?,
        kind: row.get(2)?,
        actor: row.get(3)?,
        agent: row.get(4)?,
        comment_id: row.get(5)?,
        from: row.get(6)?,
        to: row.get(7)?,
        created_at_ms: row.get(8)?,
        read_at_ms: row.get(9)?,
        snoozed_until_ms: row.get(10)?,
        archived_at_ms: row.get(11)?,
    })
}

/// Notifications, newest first.
pub(crate) fn notifications(
    conn: &Connection,
    shown: Shown,
    now_ms: i64,
) -> Result<Vec<Notification>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM planning_notifications x
         LEFT JOIN planning_activity_agents a ON a.activity_id = x.activity_id
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
        &format!(
            "SELECT {COLUMNS} FROM planning_notifications x
             LEFT JOIN planning_activity_agents a ON a.activity_id = x.activity_id
             WHERE x.id = ?1"
        ),
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
