//! The tasks the user is subscribed to, and the inbox of what others did
//! to them: status changes, comments, mentions, assignments and any field
//! an agent changed. The user is the inbox's one reader, so a change the
//! user made notifies no one. Creating a task, being made its owner or
//! being mentioned on it subscribes the user. Sona posts its own notices
//! too: a reminder whose time came, work the user owes that is due today,
//! and an automation run that finished or failed, and a weekly digest of
//! the user's projects. Each of those carries a key, so it is posted once
//! however often it is offered, across restarts. A notice may be about a
//! project, initiative, note or saved view instead of a task (its subject),
//! and a kind the user turned off in the inbox settings is never posted.

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
    /// An agent asked a question in its session on the task and waits for
    /// the answer; `to` holds the question.
    Question,
    /// Something happened to a followed project: `field` is `update` (an
    /// update posted, `to` its health) or `status` (`from` and `to`).
    Project,
    /// A task joined a followed view (`field` `added`), or one in it was
    /// done or canceled (`field` `done` or `canceled`).
    View,
    /// An agent edited a followed note.
    Note,
}

impl NotificationKind {
    pub(crate) const ALL: [Self; 13] = [
        Self::Status,
        Self::Comment,
        Self::Mention,
        Self::Assigned,
        Self::Changed,
        Self::Reminder,
        Self::Due,
        Self::Automation,
        Self::Digest,
        Self::Question,
        Self::Project,
        Self::View,
        Self::Note,
    ];

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// What needs the user: being mentioned or assigned, a question an
    /// agent waits on, a reminder, and work due today.
    pub(crate) const fn priority(self) -> bool {
        matches!(
            self,
            Self::Mention | Self::Assigned | Self::Question | Self::Reminder | Self::Due
        )
    }
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
    Question => "question",
    Project => "project",
    View => "view",
    Note => "note",
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

/// Notices about a project, initiative, note or saved view (their
/// subject), questions agents ask, the followed projects, views and notes,
/// and which kinds reach the inbox or show a banner, and when banners keep
/// quiet. Deleting a subject takes its follows and notices with it.
pub(crate) const INBOX_SUBJECTS_MIGRATION: &str = "DROP TRIGGER planning_task_lifecycle_delete;
DROP TRIGGER planning_notifications_run_delete;
CREATE TABLE planning_notifications_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT,
    kind TEXT NOT NULL CHECK(kind IN ('status', 'comment', 'mention', 'assigned', 'changed',
        'reminder', 'due', 'automation', 'digest', 'question', 'project', 'view', 'note')),
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
    subject_kind TEXT CHECK(subject_kind IN ('project', 'initiative', 'note', 'view')),
    subject_id TEXT,
    created_at_ms INTEGER NOT NULL,
    read_at_ms INTEGER,
    snoozed_until_ms INTEGER,
    archived_at_ms INTEGER,
    CHECK((subject_kind IS NULL) = (subject_id IS NULL)),
    CHECK((kind = 'digest') = (digest IS NOT NULL)),
    CHECK(task_id IS NOT NULL OR run_id IS NOT NULL OR subject_id IS NOT NULL
        OR kind = 'digest')
);
INSERT INTO planning_notifications_next (rowid, id, task_id, kind, actor, agent, activity_id,
        comment_id, field, run_id, event_key, from_value, to_value, digest, created_at_ms,
        read_at_ms, snoozed_until_ms, archived_at_ms)
    SELECT rowid, id, task_id, kind, actor, agent, activity_id, comment_id, field, run_id,
        event_key, from_value, to_value, digest, created_at_ms, read_at_ms, snoozed_until_ms,
        archived_at_ms
    FROM planning_notifications;
DROP TABLE planning_notifications;
ALTER TABLE planning_notifications_next RENAME TO planning_notifications;
CREATE INDEX planning_notifications_created ON planning_notifications(created_at_ms);
CREATE INDEX planning_notifications_task ON planning_notifications(task_id);
CREATE INDEX planning_notifications_subject ON planning_notifications(subject_kind, subject_id);
CREATE TRIGGER planning_task_lifecycle_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_task_versions WHERE task_id = old.id;
    DELETE FROM planning_task_subscriptions WHERE task_id = old.id;
    DELETE FROM planning_notifications WHERE task_id = old.id;
    DELETE FROM planning_favorites WHERE kind = 'task' AND target_id = old.id;
END;
CREATE TRIGGER planning_notifications_run_delete AFTER DELETE ON planning_loop_runs BEGIN
    DELETE FROM planning_notifications WHERE run_id = old.id;
END;
CREATE TABLE planning_follows (
    kind TEXT NOT NULL CHECK(kind IN ('project', 'view', 'note')),
    target_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    members TEXT,
    checked_at_ms INTEGER,
    PRIMARY KEY(kind, target_id)
);
CREATE TABLE planning_inbox_kinds (
    kind TEXT PRIMARY KEY NOT NULL,
    inbox INTEGER NOT NULL CHECK(inbox IN (0, 1)),
    banner INTEGER NOT NULL CHECK(banner IN (0, 1))
);
CREATE TABLE planning_inbox_quiet (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    start_minute INTEGER NOT NULL CHECK(start_minute BETWEEN 0 AND 1439),
    end_minute INTEGER NOT NULL CHECK(end_minute BETWEEN 0 AND 1439),
    CHECK(start_minute != end_minute)
);
CREATE TRIGGER planning_notifications_project_delete AFTER DELETE ON planning_projects BEGIN
    DELETE FROM planning_follows WHERE kind = 'project' AND target_id = old.id;
    DELETE FROM planning_notifications WHERE subject_kind = 'project' AND subject_id = old.id;
END;
CREATE TRIGGER planning_notifications_initiative_delete AFTER DELETE ON planning_initiatives
BEGIN
    DELETE FROM planning_notifications WHERE subject_kind = 'initiative'
        AND subject_id = old.id;
END;
CREATE TRIGGER planning_notifications_view_delete AFTER DELETE ON planning_views BEGIN
    DELETE FROM planning_follows WHERE kind = 'view' AND target_id = old.id;
    DELETE FROM planning_notifications WHERE subject_kind = 'view' AND subject_id = old.id;
END;";

/// A note deleted for good takes its follow, reminder and notices with it.
pub(crate) const NOTE_NOTICES_MIGRATION: &str =
    "CREATE TRIGGER planning_notifications_note_delete AFTER DELETE ON scratch_notes BEGIN
    DELETE FROM planning_follows WHERE kind = 'note' AND target_id = old.id;
    DELETE FROM planning_subject_reminders WHERE subject_kind = 'note' AND subject_id = old.id;
    DELETE FROM planning_notifications WHERE subject_kind = 'note' AND subject_id = old.id;
END;";

/// What a notice is about when it is not a task.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    Project,
    Initiative,
    Note,
    View,
}

crate::planning::types::sql_enum!(SubjectKind {
    Project => "project",
    Initiative => "initiative",
    Note => "note",
    View => "view",
});

/// The project, initiative, note or view a notice is about, with its name
/// as it is now; none once it is gone.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoticeSubject {
    pub(crate) kind: SubjectKind,
    pub(crate) id: String,
    pub(crate) name: Option<String>,
}

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
    /// None for an automation run about no task, a digest, and a notice
    /// about a subject.
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
    /// The project, initiative, note or view it is about, when not a task.
    pub(crate) subject: Option<NoticeSubject>,
    /// Whether it needs the user; see [`NotificationKind::priority`].
    pub(crate) priority: bool,
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
    /// Only the ones that need the user.
    pub(crate) priority_only: bool,
}

/// Narrows a list: any of these kinds, about a task in this project or the
/// project itself, about a task of any of these priorities (0 none to 4
/// low). Empty keeps every one.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboxFilter {
    #[serde(default)]
    pub(crate) kinds: Vec<NotificationKind>,
    pub(crate) project_id: Option<String>,
    #[serde(default)]
    pub(crate) task_priorities: Vec<u8>,
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

/// One notice as it is posted. Other work modules post their own through
/// [`notify`].
pub(super) struct Notice<'a> {
    pub(super) task_id: Option<&'a str>,
    pub(super) kind: NotificationKind,
    pub(super) actor: WorkActor,
    pub(super) agent: Option<&'a str>,
    pub(super) activity_id: Option<&'a str>,
    pub(super) comment_id: Option<&'a str>,
    pub(super) field: Option<&'a str>,
    pub(super) run_id: Option<&'a str>,
    pub(super) key: Option<&'a str>,
    pub(super) from: Option<&'a str>,
    pub(super) to: Option<&'a str>,
    pub(super) digest: Option<&'a str>,
    pub(super) subject: Option<(SubjectKind, &'a str)>,
}

impl<'a> Notice<'a> {
    pub(super) const fn new(
        task_id: Option<&'a str>,
        kind: NotificationKind,
        actor: WorkActor,
    ) -> Self {
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
            subject: None,
        }
    }

    /// A notice about a project, initiative, note or view.
    pub(super) const fn about(
        kind: NotificationKind,
        subject: SubjectKind,
        id: &'a str,
        actor: WorkActor,
    ) -> Self {
        Self {
            subject: Some((subject, id)),
            ..Self::new(None, kind, actor)
        }
    }
}

/// Posts a notice unless its kind is turned off in the inbox settings; one
/// with a key already posted is left alone. True when it was posted.
pub(super) fn notify(conn: &Connection, notice: &Notice<'_>, now_ms: i64) -> Result<bool> {
    if !super::inbox_settings::posts(conn, notice.kind)? {
        return Ok(false);
    }
    let (subject_kind, subject_id) = notice
        .subject
        .map_or((None, None), |(kind, id)| (Some(kind.as_str()), Some(id)));
    Ok(conn.execute(
        "INSERT INTO planning_notifications (id, task_id, kind, actor, agent, activity_id,
            comment_id, field, run_id, event_key, from_value, to_value, digest, subject_kind,
            subject_id, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
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
            subject_kind,
            subject_id,
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

/// Sona's word that a reminder on a project, initiative or note came: `at_ms`
/// set at `set_at_ms`. Posted once for each; true when this call posted it.
pub(crate) fn remind_subject(
    conn: &Connection,
    subject: SubjectKind,
    id: &str,
    at_ms: i64,
    set_at_ms: i64,
    now_ms: i64,
) -> Result<bool> {
    let key = format!("reminder:{}:{id}:{at_ms}:{set_at_ms}", subject.as_str());
    let at = at_ms.to_string();
    let notice = Notice {
        key: Some(&key),
        to: Some(&at),
        ..Notice::about(NotificationKind::Reminder, subject, id, WorkActor::Sona)
    };
    notify(conn, &notice, now_ms)
}

/// Sona's word that an active project's update is due: a reminder about the
/// project, `field` `update`, `to` the time it came due. Each cycle is due
/// on the local day `weeks` after the last update (or the project's start)
/// at `update_hour`, and posts once; a cycle missed while the Mac slept
/// posts when it wakes. True when this call posted one.
pub(crate) fn post_update_due(conn: &Connection, now_ms: i64) -> Result<bool> {
    let mut posted = false;
    let zone = jiff::tz::TimeZone::system();
    for summary in crate::planning::store::projects(conn, false)? {
        let project = summary.project;
        if project.status != crate::planning::ProjectStatus::Active
            || project.archived_at_ms.is_some()
            || project.trashed_at_ms.is_some()
        {
            continue;
        }
        let Some(weeks) = project.update_every_weeks else {
            continue;
        };
        let last_ms = crate::planning::updates::latest(conn, &project.id)?
            .map_or(project.created_at_ms, |update| update.created_at_ms);
        let Some(due_ms) = last_cycle_due(&zone, last_ms, weeks, project.update_hour, now_ms)
        else {
            continue;
        };
        let key = format!("update_due:{}:{due_ms}", project.id);
        let at = due_ms.to_string();
        let notice = Notice {
            field: Some("update"),
            key: Some(&key),
            to: Some(&at),
            ..Notice::about(
                NotificationKind::Reminder,
                SubjectKind::Project,
                &project.id,
                WorkActor::Sona,
            )
        };
        posted |= notify(conn, &notice, now_ms)?;
    }
    Ok(posted)
}

/// When the latest update cycle at or before `now_ms` came due in `zone`,
/// if one has. An hour a clock change skips comes due just past the gap.
fn last_cycle_due(
    zone: &jiff::tz::TimeZone,
    last_ms: i64,
    weeks: u8,
    hour: u8,
    now_ms: i64,
) -> Option<i64> {
    let span = i64::from(weeks) * super::digest::WEEK_MS;
    let time = jiff::civil::Time::new(i8::try_from(hour).ok()?, 0, 0, 0).ok()?;
    let due = |cycle: i64| {
        let day = jiff::Timestamp::from_millisecond(last_ms + cycle * span)
            .ok()?
            .to_zoned(zone.clone())
            .date();
        let at = day.to_datetime(time).to_zoned(zone.clone()).ok()?;
        Some(at.timestamp().as_millisecond())
    };
    let mut cycle = (now_ms - last_ms).div_euclid(span).max(0) + 1;
    while cycle >= 1 {
        match due(cycle) {
            Some(due_ms) if due_ms <= now_ms => return Some(due_ms),
            _ => cycle -= 1,
        }
    }
    None
}

/// An agent asked the user a question in its session on a task; posted once
/// per question, `activity_id` naming it.
pub(crate) fn asked(
    conn: &Connection,
    task_id: &str,
    activity_id: &str,
    agent: Option<&str>,
    question: &str,
    now_ms: i64,
) -> Result<bool> {
    let key = format!("question:{activity_id}");
    let notice = Notice {
        agent,
        key: Some(&key),
        to: Some(question),
        ..Notice::new(Some(task_id), NotificationKind::Question, WorkActor::Agent)
    };
    notify(conn, &notice, now_ms)
}

/// The user answered the questions waiting on a task: their notices are
/// read.
pub(crate) fn answered(conn: &Connection, task_id: &str, now_ms: i64) -> Result<()> {
    conn.execute(
        "UPDATE planning_notifications SET read_at_ms = coalesce(read_at_ms, ?2)
         WHERE task_id = ?1 AND kind = 'question'",
        params![task_id, now_ms],
    )?;
    Ok(())
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
    let upgraded = if super::inbox_settings::posts(conn, NotificationKind::Mention)? {
        conn.execute(
            "UPDATE planning_notifications SET kind = 'mention'
             WHERE comment_id = ?1 AND kind = 'comment'",
            [comment_id],
        )?
    } else {
        0
    };
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
    x.read_at_ms, x.snoozed_until_ms, x.archived_at_ms, x.digest, x.subject_kind, x.subject_id,
    CASE x.subject_kind
        WHEN 'project' THEN (SELECT title FROM planning_projects WHERE id = x.subject_id)
        WHEN 'initiative' THEN (SELECT name FROM planning_initiatives WHERE id = x.subject_id)
        WHEN 'view' THEN (SELECT name FROM planning_views WHERE id = x.subject_id)
    END";

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
    let subject = match (row.get::<_, Option<SubjectKind>>(18)?, row.get(19)?) {
        (Some(kind), Some(id)) => Some(NoticeSubject {
            kind,
            id,
            name: row.get(20)?,
        }),
        _ => None,
    };
    let kind: NotificationKind = row.get(2)?;
    Ok(Notification {
        id: row.get(0)?,
        item_id: row.get(1)?,
        kind,
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
        subject,
        priority: kind.priority(),
    })
}

/// Notifications, newest first.
pub(crate) fn notifications(
    conn: &Connection,
    shown: Shown,
    now_ms: i64,
) -> Result<Vec<Notification>> {
    filtered(conn, shown, &InboxFilter::default(), now_ms)
}

fn json_list<T: Serialize>(values: &[T]) -> Result<Option<String>> {
    Ok(if values.is_empty() {
        None
    } else {
        Some(serde_json::to_string(values)?)
    })
}

/// Notifications `filter` keeps, newest first.
pub(crate) fn filtered(
    conn: &Connection,
    shown: Shown,
    filter: &InboxFilter,
    now_ms: i64,
) -> Result<Vec<Notification>> {
    let priority: Vec<NotificationKind> = if shown.priority_only {
        NotificationKind::ALL
            .into_iter()
            .filter(|kind| kind.priority())
            .collect()
    } else {
        Vec::new()
    };
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM {FROM}
            LEFT JOIN planning_tasks t ON t.id = x.task_id
         WHERE (?1 = 0 OR x.read_at_ms IS NULL)
           AND (?2 OR x.snoozed_until_ms IS NULL OR x.snoozed_until_ms <= ?4)
           AND (?3 OR x.archived_at_ms IS NULL)
           AND (?5 IS NULL OR x.kind IN (SELECT value FROM json_each(?5)))
           AND (?6 IS NULL OR t.project_id = ?6
                OR (x.subject_kind = 'project' AND x.subject_id = ?6))
           AND (?7 IS NULL OR t.priority IN (SELECT value FROM json_each(?7)))
           AND (?8 IS NULL OR x.kind IN (SELECT value FROM json_each(?8)))
         ORDER BY x.created_at_ms DESC, x.rowid DESC"
    ))?;
    let rows = statement.query_map(
        params![
            shown.unread_only,
            shown.snoozed,
            shown.archived,
            now_ms,
            json_list(&filter.kinds)?,
            filter.project_id,
            json_list(&filter.task_priorities)?,
            json_list(&priority)?
        ],
        read,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Fills in the titles of the notes notices are about. Notes live beside
/// the planner, so this runs only against the whole history database.
pub(crate) fn name_notes(conn: &Connection, notices: &mut [Notification]) -> Result<()> {
    let mut notes: Vec<&mut NoticeSubject> = notices
        .iter_mut()
        .filter_map(|notice| notice.subject.as_mut())
        .filter(|subject| subject.kind == SubjectKind::Note)
        .collect();
    if notes.is_empty() {
        return Ok(());
    }
    let mut statement = conn.prepare_cached("SELECT title FROM scratch_notes WHERE id = ?1")?;
    for subject in &mut notes {
        subject.name = statement
            .query_row([&subject.id], |row| row.get(0))
            .optional()?;
    }
    Ok(())
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

pub(crate) fn work_inbox(
    app: AppHandle,
    shown: Shown,
    filter: InboxFilter,
) -> Result<Inbox, String> {
    reading(&app, |conn| {
        let now = now_ms();
        let unread = Shown {
            unread_only: true,
            ..Shown::default()
        };
        let mut shown = filtered(conn, shown, &filter, now)?;
        name_notes(conn, &mut shown)?;
        Ok(Inbox {
            unread: notifications(conn, unread, now)?.len(),
            notifications: shown,
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

#[cfg(test)]
mod update_due_tests {
    use super::super::super::schema;
    use super::super::super::store::insert_project;
    use super::{last_cycle_due, post_update_due};
    use chrono::{Local, TimeZone};
    use rusqlite::Connection;

    fn local(day: u32, hour: u32, minute: u32) -> i64 {
        Local
            .with_ymd_and_hms(2026, 3, day, hour, minute, 0)
            .earliest()
            .expect("a local time")
            .timestamp_millis()
    }

    #[test]
    fn an_update_reminder_comes_at_its_hour_once_for_each_missed_cycle() {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        let project = insert_project(&conn, "Harbor", None, local(2, 17, 0))
            .expect("a project")
            .id;
        conn.execute(
            "UPDATE planning_projects SET status = 'active', update_every_weeks = 1,
                update_hour = 9 WHERE id = ?1",
            [&project],
        )
        .expect("a weekly reminder at 09:00");

        assert!(!post_update_due(&conn, local(9, 8, 59)).expect("before the hour"));
        assert!(post_update_due(&conn, local(9, 9, 0)).expect("at the hour"));
        assert!(!post_update_due(&conn, local(9, 18, 0)).expect("same cycle"));
        assert!(post_update_due(&conn, local(16, 9, 0)).expect("next missed cycle"));
        let notices: Vec<(String, String, Option<String>)> = conn
            .prepare(
                "SELECT kind, subject_kind, field FROM planning_notifications
                 WHERE subject_id = ?1 ORDER BY created_at_ms",
            )
            .expect("a query")
            .query_map([&project], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("notices")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        let reminder = ("reminder".to_owned(), "project".to_owned(), Some("update".to_owned()));
        assert_eq!(notices, vec![reminder.clone(), reminder]);
    }

    #[test]
    fn an_update_reminder_set_for_an_hour_the_clocks_skip_comes_just_after() {
        let zone = jiff::tz::TimeZone::get("America/New_York").expect("a time zone");
        let at = |day: i8, hour: i8| {
            jiff::civil::date(2026, 3, day)
                .at(hour, 0, 0, 0)
                .to_zoned(zone.clone())
                .expect("a time")
                .timestamp()
                .as_millisecond()
        };
        let last_ms = at(1, 12);

        assert_eq!(last_cycle_due(&zone, last_ms, 1, 2, at(8, 1)), None);
        assert_eq!(last_cycle_due(&zone, last_ms, 1, 2, at(8, 3)), Some(at(8, 3)));
    }
}
