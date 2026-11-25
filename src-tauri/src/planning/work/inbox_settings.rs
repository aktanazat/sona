//! Which kinds of notice reach the inbox, which of them also show a banner
//! on this Mac, and the hours banners keep quiet. A kind the user never set
//! keeps its default: every kind reaches the inbox, and mentions,
//! assignments, questions, comments, reminders and due days show a banner.
//! A notice that arrives during quiet hours waits in the inbox without one.

use super::inbox::{self, Notification, NotificationKind, Shown};
use super::{now_ms, run_history, store, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::planning::store::Refusal;
use anyhow::Result;
use chrono::{Local, TimeZone, Timelike};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// How long after it arrives a notice may still show its banner.
pub(crate) const BANNER_WINDOW_MS: i64 = 10 * 60 * 1000;

/// One row: whether the Dock icon shows how many notices wait. No row means on.
pub(crate) const DOCK_BADGE_MIGRATION: &str = "CREATE TABLE planning_inbox_dock (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    badge INTEGER NOT NULL CHECK(badge IN (0, 1))
);";

const fn banner_by_default(kind: NotificationKind) -> bool {
    matches!(
        kind,
        NotificationKind::Mention
            | NotificationKind::Assigned
            | NotificationKind::Question
            | NotificationKind::Comment
            | NotificationKind::Reminder
            | NotificationKind::Due
    )
}

/// From `start_minute` to `end_minute` past local midnight, across it when
/// the end comes first.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuietHours {
    pub(crate) start_minute: u32,
    pub(crate) end_minute: u32,
}

impl QuietHours {
    pub(crate) fn holds(self, minute: u32) -> bool {
        if self.start_minute < self.end_minute {
            (self.start_minute..self.end_minute).contains(&minute)
        } else {
            minute >= self.start_minute || minute < self.end_minute
        }
    }

    /// `22:00-08:00`.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let (start, end) = value.trim().split_once('-')?;
        let minute = |text: &str| {
            let (hours, minutes) = text.trim().split_once(':')?;
            let hours: u32 = hours.parse().ok()?;
            let minutes: u32 = minutes.parse().ok()?;
            (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
        };
        let hours = Self {
            start_minute: minute(start)?,
            end_minute: minute(end)?,
        };
        (hours.start_minute != hours.end_minute).then_some(hours)
    }

    pub(crate) fn text(self) -> String {
        let clock = |minute: u32| format!("{:02}:{:02}", minute / 60, minute % 60);
        format!("{}-{}", clock(self.start_minute), clock(self.end_minute))
    }
}

/// One kind: whether it reaches the inbox and whether it shows a banner.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindSetting {
    pub(crate) kind: NotificationKind,
    pub(crate) inbox: bool,
    pub(crate) banner: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboxSettings {
    /// Every kind, in [`NotificationKind::ALL`]'s order.
    pub(crate) kinds: Vec<KindSetting>,
    pub(crate) quiet_hours: Option<QuietHours>,
    pub(crate) dock_badge: bool,
}

/// What to change; a field left out keeps its value. `inbox` and `banners`
/// name every kind that should be on, the rest go off; `quiet_hours` null
/// turns quiet hours off.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboxSettingsChange {
    pub(crate) inbox: Option<Vec<NotificationKind>>,
    pub(crate) banners: Option<Vec<NotificationKind>>,
    #[serde(default, deserialize_with = "super::types::nullable")]
    pub(crate) quiet_hours: Option<Option<QuietHours>>,
    pub(crate) dock_badge: Option<bool>,
}

fn stored(conn: &Connection, kind: NotificationKind) -> Result<Option<(bool, bool)>> {
    Ok(conn
        .prepare_cached("SELECT inbox, banner FROM planning_inbox_kinds WHERE kind = ?1")?
        .query_row([kind.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?)
}

fn setting(conn: &Connection, kind: NotificationKind) -> Result<KindSetting> {
    let (inbox, banner) = stored(conn, kind)?.unwrap_or((true, banner_by_default(kind)));
    Ok(KindSetting {
        kind,
        inbox,
        banner,
    })
}

/// Whether a notice of `kind` is posted at all.
pub(crate) fn posts(conn: &Connection, kind: NotificationKind) -> Result<bool> {
    Ok(setting(conn, kind)?.inbox)
}

/// Whether a notice of `kind` shows a banner; one kept out of the inbox
/// shows none.
pub(crate) fn banners(conn: &Connection, kind: NotificationKind) -> Result<bool> {
    let setting = setting(conn, kind)?;
    Ok(setting.inbox && setting.banner)
}

fn dock_badge(conn: &Connection) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT badge FROM planning_inbox_dock WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(true))
}

fn quiet_hours(conn: &Connection) -> Result<Option<QuietHours>> {
    Ok(conn
        .query_row(
            "SELECT start_minute, end_minute FROM planning_inbox_quiet WHERE id = 1",
            [],
            |row| {
                Ok(QuietHours {
                    start_minute: row.get(0)?,
                    end_minute: row.get(1)?,
                })
            },
        )
        .optional()?)
}

/// Whether banners keep quiet at `now_ms` on this Mac's clock.
pub(crate) fn quiet_at(conn: &Connection, now_ms: i64) -> Result<bool> {
    let Some(hours) = quiet_hours(conn)? else {
        return Ok(false);
    };
    let local = Local
        .timestamp_millis_opt(now_ms)
        .single()
        .ok_or_else(|| anyhow::anyhow!("{now_ms} is not a local time"))?;
    Ok(hours.holds(local.hour() * 60 + local.minute()))
}

pub(crate) fn read(conn: &Connection) -> Result<InboxSettings> {
    Ok(InboxSettings {
        kinds: NotificationKind::ALL
            .into_iter()
            .map(|kind| setting(conn, kind))
            .collect::<Result<_>>()?,
        quiet_hours: quiet_hours(conn)?,
        dock_badge: dock_badge(conn)?,
    })
}

pub(crate) fn save(conn: &Connection, change: &InboxSettingsChange) -> Result<InboxSettings> {
    for kind in NotificationKind::ALL {
        let current = setting(conn, kind)?;
        let on = |list: &Option<Vec<NotificationKind>>, now: bool| {
            list.as_ref().map_or(now, |list| list.contains(&kind))
        };
        conn.execute(
            "INSERT INTO planning_inbox_kinds (kind, inbox, banner) VALUES (?1, ?2, ?3)
             ON CONFLICT(kind) DO UPDATE SET inbox = excluded.inbox, banner = excluded.banner",
            params![
                kind.as_str(),
                on(&change.inbox, current.inbox),
                on(&change.banners, current.banner)
            ],
        )?;
    }
    match change.quiet_hours {
        Some(Some(hours)) => {
            if hours.start_minute >= 24 * 60
                || hours.end_minute >= 24 * 60
                || hours.start_minute == hours.end_minute
            {
                return Err(Refusal::Invalid.into());
            }
            conn.execute(
                "INSERT INTO planning_inbox_quiet (id, start_minute, end_minute) VALUES (1, ?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET start_minute = excluded.start_minute,
                    end_minute = excluded.end_minute",
                params![hours.start_minute, hours.end_minute],
            )?;
        }
        Some(None) => {
            conn.execute("DELETE FROM planning_inbox_quiet", [])?;
        }
        None => {}
    }
    if let Some(on) = change.dock_badge {
        conn.execute(
            "INSERT INTO planning_inbox_dock (id, badge) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET badge = excluded.badge",
            params![on],
        )?;
    }
    read(conn)
}

/// Unread notices still new enough for a banner, whose kind shows one and
/// whose key `inbox:<id>` is not in `shown`. A task's reminder or due day
/// is left out: it has a banner of its own. A reminder on a project,
/// initiative or note is never too old: like a task's, it waits until a
/// banner shows it, past quiet hours or a sleeping Mac.
pub(crate) fn banners_due(
    conn: &Connection,
    shown: &HashSet<String>,
    now_ms: i64,
) -> Result<Vec<(String, Notification)>> {
    let unread = Shown {
        unread_only: true,
        ..Shown::default()
    };
    let mut due = Vec::new();
    for notice in inbox::notifications(conn, unread, now_ms)? {
        let reminder = matches!(
            notice.kind,
            NotificationKind::Reminder | NotificationKind::Due
        );
        let own_banner = reminder && notice.item_id.is_some();
        let too_old = !reminder && notice.created_at_ms + BANNER_WINDOW_MS < now_ms;
        let key = format!("inbox:{}", notice.id);
        if too_old || own_banner || shown.contains(&key) || !banners(conn, notice.kind)? {
            continue;
        }
        due.push((key, notice));
    }
    Ok(due)
}

/// The settings as they are. The app's `work_inbox_settings`.
pub(crate) fn work_inbox_settings(app: AppHandle) -> Result<InboxSettings, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| read(conn))
}

/// Changes the settings. The app's `work_inbox_settings_save`.
pub(crate) fn save_as(
    app: &AppHandle,
    operation_id: &str,
    change: &InboxSettingsChange,
) -> Result<InboxSettings, String> {
    let now = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_inbox_settings_save",
            change,
            now,
            |tx| {
                Ok(Done {
                    value: save(tx, change)?,
                    effects: Effects::default(),
                })
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::schema;
    use super::super::inbox::{self, NotificationKind, Remind, Shown, SubjectKind};
    use super::super::items::{self, NewItem};
    use super::super::types::{WorkActor, WorkOwner, WorkStatus};
    use super::{banners_due, save, InboxSettingsChange, QuietHours};
    use rusqlite::Connection;
    use std::collections::HashSet;

    const AT: i64 = 1_700_000_000_000;

    fn planner() -> (Connection, String) {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        let task = items::insert(
            &conn,
            &NewItem {
                title: "Call the venue",
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner: &WorkOwner::Myself,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Me,
            AT,
        )
        .expect("a task")
        .id;
        (conn, task)
    }

    fn kinds(conn: &Connection, shown: Shown) -> Vec<NotificationKind> {
        inbox::notifications(conn, shown, AT + 1)
            .expect("the inbox")
            .into_iter()
            .map(|notice| notice.kind)
            .collect()
    }

    #[test]
    fn a_kind_turned_off_is_never_posted_and_priority_keeps_what_needs_the_user() {
        let (conn, task) = planner();
        inbox::remind(&conn, &task, Remind::Due, AT, AT).expect("due");
        inbox::asked(&conn, &task, "q1", Some("codex"), "Ship it?", AT).expect("a question");
        inbox::remind(&conn, &task, Remind::Reminder { set_at_ms: 1 }, AT, AT).expect("reminder");
        let mut every = kinds(&conn, Shown::default());
        every.sort_by_key(|kind| kind.as_str());
        assert_eq!(
            every,
            [
                NotificationKind::Due,
                NotificationKind::Question,
                NotificationKind::Reminder
            ]
        );

        save(
            &conn,
            &InboxSettingsChange {
                inbox: Some(vec![NotificationKind::Due, NotificationKind::Question]),
                ..InboxSettingsChange::default()
            },
        )
        .expect("settings");
        assert!(
            !inbox::remind(&conn, &task, Remind::Reminder { set_at_ms: 2 }, AT, AT)
                .expect("a muted reminder")
        );

        let priority = kinds(
            &conn,
            Shown {
                priority_only: true,
                ..Shown::default()
            },
        );
        assert_eq!(priority.len(), 3);
        assert!(priority.iter().all(|kind| kind.priority()));
    }

    #[test]
    fn quiet_hours_across_midnight_hold_late_and_early_but_not_midday() {
        let night = QuietHours::parse("22:00-08:00").expect("quiet hours");
        assert!(night.holds(23 * 60));
        assert!(night.holds(7 * 60 + 59));
        assert!(!night.holds(8 * 60));
        assert!(!night.holds(12 * 60));
        let lunch = QuietHours::parse("12:00-13:30").expect("quiet hours");
        assert!(lunch.holds(12 * 60 + 15));
        assert!(!lunch.holds(13 * 60 + 30));
        assert_eq!(QuietHours::parse("09:00-09:00"), None);
        assert_eq!(QuietHours::parse("25:00-08:00"), None);
    }

    #[test]
    fn a_project_reminder_held_past_the_banner_window_still_shows_one() {
        let (conn, task) = planner();
        let project = crate::planning::store::insert_project(&conn, "Harbor launch", None, AT)
            .expect("a project")
            .id;
        inbox::asked(&conn, &task, "q1", Some("codex"), "Ship it?", AT).expect("a question");
        inbox::remind_subject(&conn, SubjectKind::Project, &project, AT, 1, AT).expect("posted");

        let after_quiet_hours = AT + 30 * 60 * 1000;
        let due: Vec<_> = banners_due(&conn, &HashSet::new(), after_quiet_hours)
            .expect("banners")
            .into_iter()
            .map(|(_, notice)| notice.kind)
            .collect();
        assert_eq!(due, [NotificationKind::Reminder]);
    }
}
