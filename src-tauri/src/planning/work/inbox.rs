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
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

pub(crate) mod asks;
pub(crate) mod rows;

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
    /// What Sona closed or archived by itself; `to` holds the summary.
    Housekeeping,
    /// A followed task's last open blocker finished; `to` is the status it finished in.
    Unblocked,
    /// A meeting's notes changed decisions earlier meetings made; the
    /// subject is the meeting, `from` its title and `to` how many changed.
    Decision,
}

impl NotificationKind {
    pub(crate) const ALL: [Self; 16] = [
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
        Self::Housekeeping,
        Self::Unblocked,
        Self::Decision,
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
    Housekeeping => "housekeeping",
    Unblocked => "unblocked",
    Decision => "decision",
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

/// A notice may be Sona's own housekeeping, with or without a task. Runs
/// after `INBOX_SUBJECTS_MIGRATION` and `NOTE_NOTICES_MIGRATION`; the note
/// trigger, on a table only the history database has, is dropped here and
/// made again by running `NOTE_NOTICES_MIGRATION` once more.
pub(crate) const HOUSEKEEPING_NOTICES_MIGRATION: &str = "DROP TRIGGER IF EXISTS planning_task_lifecycle_delete;
DROP TRIGGER IF EXISTS planning_notifications_run_delete;
DROP TRIGGER IF EXISTS planning_notifications_project_delete;
DROP TRIGGER IF EXISTS planning_notifications_initiative_delete;
DROP TRIGGER IF EXISTS planning_notifications_view_delete;
DROP TRIGGER IF EXISTS planning_notifications_note_delete;
CREATE TABLE planning_notifications_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT,
    kind TEXT NOT NULL CHECK(kind IN ('status', 'comment', 'mention', 'assigned', 'changed',
        'reminder', 'due', 'automation', 'digest', 'question', 'project', 'view', 'note',
        'housekeeping')),
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
        OR kind IN ('digest', 'housekeeping'))
);
INSERT INTO planning_notifications_next (rowid, id, task_id, kind, actor, agent, activity_id,
        comment_id, field, run_id, event_key, from_value, to_value, digest, subject_kind,
        subject_id, created_at_ms, read_at_ms, snoozed_until_ms, archived_at_ms)
    SELECT rowid, id, task_id, kind, actor, agent, activity_id, comment_id, field, run_id,
        event_key, from_value, to_value, digest, subject_kind, subject_id, created_at_ms,
        read_at_ms, snoozed_until_ms, archived_at_ms
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

/// Adds the unblocked kind to the notices table; every other row and trigger is kept as it was.
pub(crate) const UNBLOCKED_NOTICES_MIGRATION: &str =
    "DROP TRIGGER IF EXISTS planning_task_lifecycle_delete;
DROP TRIGGER IF EXISTS planning_notifications_run_delete;
DROP TRIGGER IF EXISTS planning_notifications_project_delete;
DROP TRIGGER IF EXISTS planning_notifications_initiative_delete;
DROP TRIGGER IF EXISTS planning_notifications_view_delete;
DROP TRIGGER IF EXISTS planning_notifications_note_delete;
CREATE TABLE planning_notifications_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT,
    kind TEXT NOT NULL CHECK(kind IN ('status', 'comment', 'mention', 'assigned', 'changed',
        'reminder', 'due', 'automation', 'digest', 'question', 'project', 'view', 'note',
        'housekeeping', 'unblocked')),
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
        OR kind IN ('digest', 'housekeeping'))
);
INSERT INTO planning_notifications_next (rowid, id, task_id, kind, actor, agent, activity_id,
        comment_id, field, run_id, event_key, from_value, to_value, digest, subject_kind,
        subject_id, created_at_ms, read_at_ms, snoozed_until_ms, archived_at_ms)
    SELECT rowid, id, task_id, kind, actor, agent, activity_id, comment_id, field, run_id,
        event_key, from_value, to_value, digest, subject_kind, subject_id, created_at_ms,
        read_at_ms, snoozed_until_ms, archived_at_ms
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

/// Adds the decision kind and notices about a meeting; every other row and
/// trigger is kept as it was. The note trigger is made again by running
/// `NOTE_NOTICES_MIGRATION` once more.
pub(crate) const DECISION_NOTICES_MIGRATION: &str =
    "DROP TRIGGER IF EXISTS planning_task_lifecycle_delete;
DROP TRIGGER IF EXISTS planning_notifications_run_delete;
DROP TRIGGER IF EXISTS planning_notifications_project_delete;
DROP TRIGGER IF EXISTS planning_notifications_initiative_delete;
DROP TRIGGER IF EXISTS planning_notifications_view_delete;
DROP TRIGGER IF EXISTS planning_notifications_note_delete;
CREATE TABLE planning_notifications_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT,
    kind TEXT NOT NULL CHECK(kind IN ('status', 'comment', 'mention', 'assigned', 'changed',
        'reminder', 'due', 'automation', 'digest', 'question', 'project', 'view', 'note',
        'housekeeping', 'unblocked', 'decision')),
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
    subject_kind TEXT CHECK(subject_kind IN ('project', 'initiative', 'note', 'view', 'meeting')),
    subject_id TEXT,
    created_at_ms INTEGER NOT NULL,
    read_at_ms INTEGER,
    snoozed_until_ms INTEGER,
    archived_at_ms INTEGER,
    CHECK((subject_kind IS NULL) = (subject_id IS NULL)),
    CHECK((kind = 'digest') = (digest IS NOT NULL)),
    CHECK(task_id IS NOT NULL OR run_id IS NOT NULL OR subject_id IS NOT NULL
        OR kind IN ('digest', 'housekeeping'))
);
INSERT INTO planning_notifications_next (rowid, id, task_id, kind, actor, agent, activity_id,
        comment_id, field, run_id, event_key, from_value, to_value, digest, subject_kind,
        subject_id, created_at_ms, read_at_ms, snoozed_until_ms, archived_at_ms)
    SELECT rowid, id, task_id, kind, actor, agent, activity_id, comment_id, field, run_id,
        event_key, from_value, to_value, digest, subject_kind, subject_id, created_at_ms,
        read_at_ms, snoozed_until_ms, archived_at_ms
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

/// What a notice is about when it is not a task.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    Project,
    Initiative,
    Note,
    View,
    Meeting,
}

crate::planning::types::sql_enum!(SubjectKind {
    Project => "project",
    Initiative => "initiative",
    Note => "note",
    View => "view",
    Meeting => "meeting",
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
    /// The start of the comment a comment or mention notice is about.
    pub(crate) excerpt: Option<String>,
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
    /// Earlier notices this one stands for: the same automation failing
    /// the same way, with nothing else from it in between.
    pub(crate) earlier: Vec<String>,
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
    /// One row per task, its notices folded together; see [`rows::tasks`].
    pub(crate) by_task: bool,
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
    /// Text to find in a task's title or number, a notice's kind, its subject's
    /// name or its comment. Case and surrounding spaces do not matter.
    pub(crate) search: Option<String>,
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
    let posted = conn.execute(
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
    )? > 0;
    if let Some(task_id) = notice.task_id.filter(|_| posted) {
        // A notice about a task wakes the ones about it that were snoozed to a time.
        conn.execute(
            "UPDATE planning_notifications SET snoozed_until_ms = NULL
             WHERE task_id = ?1 AND snoozed_until_ms > ?2 AND archived_at_ms IS NULL",
            params![task_id, now_ms],
        )?;
    }
    Ok(posted)
}

/// Where the notices stand, for [`forget_since`].
pub(crate) fn last_posted(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT coalesce(max(rowid), 0) FROM planning_notifications",
        [],
        |row| row.get(0),
    )?)
}

/// Takes back every notice posted since `mark`: work brought in from
/// another tracker is old news, not a run of assignments and comments.
pub(crate) fn forget_since(conn: &Connection, mark: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM planning_notifications WHERE rowid > ?1",
        [mark],
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

/// Whether this entry is the other end of a link the inbox was already
/// told about: a link is history on both tasks and news once, on the task
/// it was made from. The code that writes a link names its other end, so
/// nothing is read from when or how the two entries were written.
fn echoes_a_link(conn: &Connection, entry: &Entry<'_>) -> Result<bool> {
    let Some(made) = entry.mirror_of else {
        return Ok(false);
    };
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM planning_notifications WHERE activity_id = ?1)",
        [made],
        |row| row.get(0),
    )?)
}

/// One history entry as it is written.
pub(crate) struct Entry<'a> {
    pub(crate) task_id: &'a str,
    pub(crate) activity_id: &'a str,
    pub(crate) actor: WorkActor,
    pub(crate) kind: WorkActivityKind,
    pub(crate) from: Option<&'a str>,
    pub(crate) to: Option<&'a str>,
    /// The entry for the other end of the link this entry mirrors.
    pub(crate) mirror_of: Option<&'a str>,
}

/// Posts an unblocked notice for each followed task that `finished` was the
/// last open blocker of. Only a blocker that finished counts. The notice
/// names the blocker's title in `from` and shares its status entry, which
/// names the agent that finished it.
fn unblocked(conn: &Connection, finished: &str, entry: &Entry<'_>, now_ms: i64) -> Result<()> {
    if !matches!(entry.to, Some("done" | "canceled")) {
        return Ok(());
    }
    let mut statement = conn.prepare(
        "SELECT b.id FROM planning_task_relations r
            JOIN planning_tasks b ON b.id = r.to_id
         WHERE r.from_id = ?1 AND r.kind = 'blocks'
           AND b.status NOT IN ('done', 'canceled', 'duplicate') AND b.archived_at_ms IS NULL
           AND EXISTS (SELECT 1 FROM planning_task_subscriptions s WHERE s.task_id = b.id)
           AND NOT EXISTS (SELECT 1 FROM planning_task_relations o
                JOIN planning_tasks ob ON ob.id = o.from_id
             WHERE o.to_id = b.id AND o.kind = 'blocks' AND o.from_id <> ?1
               AND ob.status NOT IN ('done', 'canceled', 'duplicate') AND ob.archived_at_ms IS NULL)",
    )?;
    let blocked = statement
        .query_map([finished], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if blocked.is_empty() {
        return Ok(());
    }
    let title: String = conn.query_row(
        "SELECT title FROM planning_tasks WHERE id = ?1",
        [finished],
        |row| row.get(0),
    )?;
    for id in blocked {
        let notice = Notice {
            activity_id: Some(entry.activity_id),
            from: Some(&title),
            to: entry.to,
            ..Notice::new(Some(&id), NotificationKind::Unblocked, entry.actor)
        };
        notify(conn, &notice, now_ms)?;
    }
    Ok(())
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
        mirror_of: _,
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
    if matches!(kind, WorkActivityKind::Status) && actor != WorkActor::Me {
        unblocked(conn, task_id, entry, now_ms)?;
    }
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
        WorkActivityKind::Relation if echoes_a_link(conn, entry)? => {}
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
        ..Notice::new(
            task_id.as_deref(),
            NotificationKind::Automation,
            WorkActor::Agent,
        )
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

/// Takes back the status and comment notices Sona posted on `task_id` at
/// `at_ms`, for a batch whose one summary notice stands for them.
pub(crate) fn withdraw_sona_notices(conn: &Connection, task_id: &str, at_ms: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM planning_notifications
         WHERE task_id = ?1 AND actor = 'sona' AND created_at_ms = ?2 AND kind IN ('status', 'comment')",
        params![task_id, at_ms],
    )?;
    Ok(())
}

/// Posts the housekeeping notice for a batch of Sona's moves, or brings
/// the one posted for `key` up to date and unread again.
pub(crate) fn batch_posted(
    conn: &Connection,
    task_id: Option<&str>,
    key: &str,
    summary: &str,
    now_ms: i64,
) -> Result<()> {
    let notice = Notice {
        key: Some(key),
        to: Some(summary),
        ..Notice::new(task_id, NotificationKind::Housekeeping, WorkActor::Sona)
    };
    if !notify(conn, &notice, now_ms)? {
        conn.execute(
            "UPDATE planning_notifications
             SET to_value = ?2, created_at_ms = ?3, read_at_ms = NULL, snoozed_until_ms = NULL,
                 archived_at_ms = NULL
             WHERE event_key = ?1",
            params![key, summary, now_ms],
        )?;
    }
    Ok(())
}

/// Posts, once per meeting, that its notes changed `changed` decisions
/// earlier meetings made. True when it was posted.
pub(crate) fn decisions_changed(
    conn: &Connection,
    meeting_id: &str,
    title: &str,
    changed: usize,
    now_ms: i64,
) -> Result<bool> {
    let key = format!("decision:{meeting_id}");
    let changed = changed.to_string();
    let notice = Notice {
        key: Some(&key),
        from: Some(title),
        to: Some(&changed),
        ..Notice::about(
            NotificationKind::Decision,
            SubjectKind::Meeting,
            meeting_id,
            WorkActor::Sona,
        )
    };
    notify(conn, &notice, now_ms)
}

/// What a decision notice says happened, from the count it holds.
pub(crate) fn changed_decisions(count: Option<&str>) -> String {
    match count {
        None | Some("1") => "changed a decision an earlier meeting made".to_owned(),
        Some(count) => format!("changed {count} decisions earlier meetings made"),
    }
}

/// The batch a housekeeping notice stands for.
pub(crate) fn batch_key(conn: &Connection, notice_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT event_key FROM planning_notifications WHERE id = ?1 AND kind = 'housekeeping'",
            [notice_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

/// Reads and archives a batch notice once the user undid what it reports.
pub(crate) fn batch_handled(conn: &Connection, key: &str, now_ms: i64) -> Result<()> {
    conn.execute(
        "UPDATE planning_notifications
         SET read_at_ms = coalesce(read_at_ms, ?2), archived_at_ms = ?2
         WHERE event_key = ?1",
        params![key, now_ms],
    )?;
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

/// Whether `body` names Sona as `@Sona`, not a longer word that starts so.
pub(crate) fn mentions_sona(body: &str) -> bool {
    let lower = body.to_lowercase();
    lower.match_indices("@sona").any(|(at, _)| {
        lower[at + "@sona".len()..]
            .chars()
            .next()
            .is_none_or(|next| !next.is_alphanumeric())
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
        WHEN 'meeting' THEN x.from_value
    END,
    (SELECT body FROM planning_task_comments c WHERE c.id = x.comment_id)";

const FROM: &str = "planning_notifications x
    LEFT JOIN planning_activity_agents a ON a.activity_id = x.activity_id
    LEFT JOIN planning_loop_runs r ON r.id = x.run_id
    LEFT JOIN planning_loops l ON l.id = r.loop_id";

const EXCERPT_CHARS: usize = 160;

/// The start of a comment's body, flattened to one line.
fn excerpt(body: &str) -> Option<String> {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    Some(match flat.char_indices().nth(EXCERPT_CHARS) {
        Some((cut, _)) => format!("{}…", flat[..cut].trim_end()),
        None => flat,
    })
}

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
            rusqlite::Error::FromSqlConversionFailure(
                17,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
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
        excerpt: row
            .get::<_, Option<String>>(21)?
            .and_then(|body| excerpt(&body)),
        priority: kind.priority(),
        earlier: Vec::new(),
    })
}

/// Whether `notice` repeats the failure `leader` already stands for.
fn repeats(leader: &Notification, notice: &Notification, now_ms: i64) -> bool {
    let awake = |notice: &Notification| notice.snoozed_until_ms.is_none_or(|until| until <= now_ms);
    let error = |notice: &Notification| notice.automation.as_ref().map(|run| run.error.clone());
    notice.to == leader.to
        && notice.item_id == leader.item_id
        && error(notice) == error(leader)
        && notice.archived_at_ms.is_some() == leader.archived_at_ms.is_some()
        && if awake(leader) {
            awake(notice)
        } else {
            notice.snoozed_until_ms == leader.snoozed_until_ms
        }
}

/// Folds each run of one automation failing the same way into its newest
/// notice, unread while any of them is. `notices` come newest first.
fn fold(notices: Vec<Notification>, now_ms: i64) -> Vec<Notification> {
    let mut shown: Vec<Notification> = Vec::with_capacity(notices.len());
    let mut failing: HashMap<String, usize> = HashMap::new();
    for notice in notices {
        let Some(loop_id) = notice.automation.as_ref().map(|run| run.id.clone()) else {
            shown.push(notice);
            continue;
        };
        if let Some(&at) = failing.get(&loop_id) {
            if repeats(&shown[at], &notice, now_ms) {
                let leader = &mut shown[at];
                if notice.read_at_ms.is_none() {
                    leader.read_at_ms = None;
                }
                leader.earlier.push(notice.id);
                continue;
            }
        }
        if notice.to.as_deref() == Some("failed") {
            failing.insert(loop_id, shown.len());
        } else {
            failing.remove(&loop_id);
        }
        shown.push(notice);
    }
    shown
}

/// Notifications, newest first, repeated failures folded.
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
    let search = filter
        .search
        .as_deref()
        .map(|text| text.trim().to_lowercase())
        .filter(|text| !text.is_empty());
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM {FROM}
            LEFT JOIN planning_tasks t ON t.id = x.task_id
         WHERE (?1 OR x.snoozed_until_ms IS NULL OR x.snoozed_until_ms <= ?3)
           AND (?2 OR x.archived_at_ms IS NULL)
           AND (?4 IS NULL OR x.kind IN (SELECT value FROM json_each(?4)))
           AND (?5 IS NULL OR t.project_id = ?5
                OR (x.subject_kind = 'project' AND x.subject_id = ?5))
           AND (?6 IS NULL OR t.priority IN (SELECT value FROM json_each(?6)))
           AND (?7 IS NULL OR x.kind IN (SELECT value FROM json_each(?7)))
           AND (?8 IS NULL OR instr(lower(coalesce(t.title, '')), ?8) > 0
                OR instr('#' || t.number, ?8) > 0
                OR instr(x.kind, ?8) > 0
                OR x.subject_id IN (SELECT id FROM planning_projects WHERE instr(lower(title), ?8) > 0
                    UNION ALL SELECT id FROM planning_initiatives WHERE instr(lower(name), ?8) > 0
                    UNION ALL SELECT id FROM planning_views WHERE instr(lower(name), ?8) > 0)
                OR EXISTS (SELECT 1 FROM planning_task_comments c
                    WHERE c.id = x.comment_id AND instr(lower(c.body), ?8) > 0))
         ORDER BY x.created_at_ms DESC, x.rowid DESC"
    ))?;
    let rows = statement.query_map(
        params![
            shown.snoozed,
            shown.archived,
            now_ms,
            json_list(&filter.kinds)?,
            filter.project_id,
            json_list(&filter.task_priorities)?,
            json_list(&priority)?,
            search
        ],
        read,
    )?;
    // Unread is decided after folding: a folded run is unread while any of its notices is.
    let mut folded = fold(rows.collect::<rusqlite::Result<Vec<_>>>()?, now_ms);
    if shown.unread_only {
        folded.retain(|notice| notice.read_at_ms.is_none());
    }
    Ok(folded)
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

pub(super) fn marked(
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
            let mut wanted = super::unique(ids)?;
            let folded = notifications(
                tx,
                Shown {
                    snoozed: true,
                    ..Shown::default()
                },
                change.at_ms,
            )?;
            for notice in folded {
                if wanted.contains(&notice.id) {
                    wanted.extend(notice.earlier);
                }
            }
            let value = wanted
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

/// Marks the notices about a task read: opening the task reads its news.
fn read_task(conn: &Connection, task_id: &str, now_ms: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE planning_notifications SET read_at_ms = ?2
         WHERE task_id = ?1 AND read_at_ms IS NULL",
        params![task_id, now_ms],
    )?)
}

fn task_marked(history: &mut Connection, change: Change<'_>, task_id: &str) -> Result<Done<usize>> {
    store::mutate(
        history,
        change.operation_id,
        "work_inbox_mark_task",
        &task_id,
        change.at_ms,
        |tx| {
            Ok(Done {
                value: read_task(tx, task_id, change.at_ms)?,
                effects: Effects::default(),
            })
        },
    )
}

/// Reads every notice about a task, as the task is opened. Returns how many were unread.
pub(crate) fn mark_task_read(
    app: &AppHandle,
    operation_id: &str,
    task_id: &str,
) -> Result<usize, String> {
    let change = Change::now(operation_id, WorkActor::Me);
    run_history(app, |history| task_marked(history, change, task_id))
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
    /// Unread rows, not unread notices: a burst counts once.
    unread: usize,
    /// Unread rows that need the user.
    needs: usize,
    notifications: Vec<Notification>,
    rows: Vec<rows::InboxRow>,
}

fn reading<T>(app: &AppHandle, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| work(conn))
}

/// The rows a list shows. They are built over read notices too, then
/// narrowed: an unread-only view must not split a burst at the notices
/// already read.
pub(crate) fn rows_of(
    conn: &Connection,
    shown: Shown,
    filter: &InboxFilter,
    now: i64,
) -> Result<Vec<rows::InboxRow>> {
    let context = rows::context(conn)?;
    let mut all = filtered(
        conn,
        Shown {
            unread_only: false,
            ..shown
        },
        filter,
        now,
    )?;
    name_notes(conn, &mut all)?;
    Ok(match (shown.unread_only, shown.by_task) {
        (true, by_task) => rows::waiting(all, &context, by_task),
        (false, true) => rows::tasks(all, &context),
        (false, false) => rows::rows(all, &context),
    })
}

pub(crate) fn work_inbox(
    app: AppHandle,
    shown: Shown,
    filter: InboxFilter,
) -> Result<Inbox, String> {
    asks::catch_up(&app);
    reading(&app, |conn| {
        let now = now_ms();
        let context = rows::context(conn)?;
        let waiting = rows::waiting(notifications(conn, Shown::default(), now)?, &context, false);
        let rows = rows_of(conn, shown, &filter, now)?;
        let mut shown = filtered(conn, shown, &filter, now)?;
        name_notes(conn, &mut shown)?;
        Ok(Inbox {
            unread: waiting.len(),
            needs: waiting.iter().filter(|row| row.needs.is_some()).count(),
            rows,
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

#[cfg(test)]
mod tests {
    use super::{
        changed_decisions, decisions_changed, mentions_sona, notifications, NotificationKind,
        Shown, SubjectKind,
    };

    #[test]
    fn sona_is_mentioned_only_as_a_whole_name() {
        assert!(mentions_sona("ask @Sona about it"));
        assert!(mentions_sona("@sona"));
        assert!(!mentions_sona("@Sonata is not Sona"));
        assert!(!mentions_sona("no mention here"));
    }

    #[test]
    fn a_meeting_that_changed_decisions_gets_one_notice_naming_it() {
        let conn = rusqlite::Connection::open_in_memory().expect("an in-memory history database");
        crate::planning::schema::apply_all(&conn).expect("the planner schema");
        let meeting = uuid::Uuid::new_v4().to_string();

        assert!(decisions_changed(&conn, &meeting, "Launch sync", 2, 10).expect("posted"));
        assert!(!decisions_changed(&conn, &meeting, "Launch sync", 3, 20).expect("posted once"));

        let notices = notifications(&conn, Shown::default(), 30).expect("the inbox");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].kind, NotificationKind::Decision);
        let subject = notices[0].subject.as_ref().expect("the meeting");
        assert_eq!(
            (subject.kind, subject.id.as_str(), subject.name.as_deref()),
            (SubjectKind::Meeting, meeting.as_str(), Some("Launch sync"))
        );
        assert_eq!(
            changed_decisions(notices[0].to.as_deref()),
            "changed 2 decisions earlier meetings made"
        );
    }
}
