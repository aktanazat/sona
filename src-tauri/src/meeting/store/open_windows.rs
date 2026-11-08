//! The windows in front while a meeting recorded. A row is offered until
//! the person attaches it as a link; an offered row lives at most
//! `OFFERED_KEEP_MS` and goes when the offer is dismissed.

use super::{from_i64, id, parse_uuid, session_row, to_i64, MeetingStore, StoreError};
use crate::context::FrontWindow;
use crate::meeting::open_windows::{
    window_label, MeetingOpenWindow, OpenWindowId, OFFERED_KEEP_MS, OFFERED_LIMIT,
};
use crate::meeting::types::{MeetingPhase, MeetingSessionId};
use rusqlite::{params, Connection, OptionalExtension, Row};
use uuid::Uuid;

const COLUMNS: &str = "item_id, app_name, bundle_id, title, document_path, url, host, \
    first_offset_ns, last_offset_ns, frontmost_ns, attached_at_utc_ms";

impl MeetingStore {
    /// One more look at a window while the meeting records: a window seen
    /// before gets the time added, a new one is offered while the meeting
    /// offers fewer than `OFFERED_LIMIT`.
    pub(crate) fn note_open_window(
        &self,
        session_id: MeetingSessionId,
        key: &str,
        window: &FrontWindow,
        offset_ns: u64,
        frontmost_ns: u64,
        now_utc_ms: i64,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        require_recording(&transaction, session_id)?;
        let seen = transaction.execute(
            "UPDATE meeting_open_windows
                SET last_offset_ns = MAX(last_offset_ns, ?3),
                    frontmost_ns = frontmost_ns + ?4
              WHERE session_id = ?1 AND item_key = ?2",
            params![
                id(session_id),
                key,
                to_i64(offset_ns)?,
                to_i64(frontmost_ns)?
            ],
        )?;
        if seen == 0 && offered_count(&transaction, session_id)? < OFFERED_LIMIT {
            insert_window(
                &transaction,
                session_id,
                key,
                window,
                offset_ns,
                frontmost_ns,
                now_utc_ms,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Keeps the window in front as a link at once, for the attach-window
    /// shortcut, whether or not it was offered.
    pub(crate) fn attach_open_window_now(
        &self,
        session_id: MeetingSessionId,
        key: &str,
        window: &FrontWindow,
        offset_ns: u64,
        now_utc_ms: i64,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        require_recording(&transaction, session_id)?;
        insert_window(
            &transaction,
            session_id,
            key,
            window,
            offset_ns,
            0,
            now_utc_ms,
        )?;
        transaction.execute(
            "UPDATE meeting_open_windows
                SET last_offset_ns = MAX(last_offset_ns, ?3),
                    attached_at_utc_ms = COALESCE(attached_at_utc_ms, ?4)
              WHERE session_id = ?1 AND item_key = ?2",
            params![id(session_id), key, to_i64(offset_ns)?, now_utc_ms],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// The meeting's windows, offered and attached, in the order they were
    /// first in front.
    pub(crate) fn open_windows(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<MeetingOpenWindow>, StoreError> {
        let connection = self.connection()?;
        session_row(&connection, session_id)?;
        let mut statement = connection.prepare(&format!(
            "SELECT {COLUMNS} FROM meeting_open_windows WHERE session_id = ?1
              ORDER BY first_offset_ns, created_at_utc_ms, item_id"
        ))?;
        let rows = statement
            .query_map(params![id(session_id)], StoredWindow::read)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(StoredWindow::into_window).collect()
    }

    pub(crate) fn open_window(
        &self,
        session_id: MeetingSessionId,
        item_id: OpenWindowId,
    ) -> Result<MeetingOpenWindow, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM meeting_open_windows
                      WHERE session_id = ?1 AND item_id = ?2"
                ),
                params![id(session_id), id(item_id.uuid())],
                StoredWindow::read,
            )
            .optional()?
            .ok_or(StoreError::NotFound)?
            .into_window()
    }

    /// Keeps an offered window as one of the meeting's links.
    pub(crate) fn attach_open_window(
        &self,
        session_id: MeetingSessionId,
        item_id: OpenWindowId,
        now_utc_ms: i64,
    ) -> Result<(), StoreError> {
        let changed = self.connection()?.execute(
            "UPDATE meeting_open_windows
                SET attached_at_utc_ms = COALESCE(attached_at_utc_ms, ?3)
              WHERE session_id = ?1 AND item_id = ?2",
            params![id(session_id), id(item_id.uuid()), now_utc_ms],
        )?;
        found(changed)
    }

    pub(crate) fn remove_open_window(
        &self,
        session_id: MeetingSessionId,
        item_id: OpenWindowId,
    ) -> Result<(), StoreError> {
        let changed = self.connection()?.execute(
            "DELETE FROM meeting_open_windows WHERE session_id = ?1 AND item_id = ?2",
            params![id(session_id), id(item_id.uuid())],
        )?;
        found(changed)
    }

    /// Forgets an offered window by what it showed: one that had a password
    /// field in focus, or a file that became one of the meeting's documents.
    pub(crate) fn forget_open_window(
        &self,
        session_id: MeetingSessionId,
        key: &str,
    ) -> Result<(), StoreError> {
        self.connection()?.execute(
            "DELETE FROM meeting_open_windows
              WHERE session_id = ?1 AND item_key = ?2 AND attached_at_utc_ms IS NULL",
            params![id(session_id), key],
        )?;
        Ok(())
    }

    /// Deletes what the meeting offered and the person did not attach.
    pub(crate) fn dismiss_open_windows(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<usize, StoreError> {
        Ok(self.connection()?.execute(
            "DELETE FROM meeting_open_windows
              WHERE session_id = ?1 AND attached_at_utc_ms IS NULL",
            params![id(session_id)],
        )?)
    }

    /// Deletes every window offered and not attached, in every meeting.
    pub(crate) fn forget_offered_open_windows(&self) -> Result<usize, StoreError> {
        Ok(self.connection()?.execute(
            "DELETE FROM meeting_open_windows WHERE attached_at_utc_ms IS NULL",
            [],
        )?)
    }

    /// Deletes the offered windows first seen `OFFERED_KEEP_MS` or longer
    /// before `now_utc_ms`.
    pub(crate) fn purge_stale_open_windows(&self, now_utc_ms: i64) -> Result<usize, StoreError> {
        Ok(self.connection()?.execute(
            "DELETE FROM meeting_open_windows
              WHERE attached_at_utc_ms IS NULL AND created_at_utc_ms <= ?1",
            params![now_utc_ms.saturating_sub(OFFERED_KEEP_MS)],
        )?)
    }
}

fn require_recording(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<(), StoreError> {
    if session_row(connection, session_id)?.phase == MeetingPhase::CapturingRecording {
        Ok(())
    } else {
        Err(StoreError::Conflict)
    }
}

fn offered_count(connection: &Connection, session_id: MeetingSessionId) -> Result<u32, StoreError> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM meeting_open_windows
          WHERE session_id = ?1 AND attached_at_utc_ms IS NULL",
        params![id(session_id)],
        |row| row.get(0),
    )?;
    u32::try_from(count).map_err(|_| StoreError::Corrupt)
}

/// Adds the window unless the meeting already has it.
fn insert_window(
    connection: &Connection,
    session_id: MeetingSessionId,
    key: &str,
    window: &FrontWindow,
    offset_ns: u64,
    frontmost_ns: u64,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    let offset_ns = to_i64(offset_ns)?;
    connection.execute(
        "INSERT INTO meeting_open_windows (
             item_id, session_id, item_key, app_name, bundle_id, title, document_path, url,
             host, first_offset_ns, last_offset_ns, frontmost_ns, created_at_utc_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11, ?12)
         ON CONFLICT (session_id, item_key) DO NOTHING",
        params![
            id(Uuid::new_v4()),
            id(session_id),
            key,
            window.app_name,
            window.bundle_id,
            window.title,
            window.document_path,
            window.url,
            window.host,
            offset_ns,
            to_i64(frontmost_ns)?,
            now_utc_ms,
        ],
    )?;
    Ok(())
}

fn found(changed: usize) -> Result<(), StoreError> {
    if changed == 0 {
        Err(StoreError::NotFound)
    } else {
        Ok(())
    }
}

struct StoredWindow {
    item_id: String,
    app_name: Option<String>,
    bundle_id: Option<String>,
    title: Option<String>,
    document_path: Option<String>,
    url: Option<String>,
    host: Option<String>,
    first_offset_ns: i64,
    last_offset_ns: i64,
    frontmost_ns: i64,
    attached_at_utc_ms: Option<i64>,
}

impl StoredWindow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            item_id: row.get(0)?,
            app_name: row.get(1)?,
            bundle_id: row.get(2)?,
            title: row.get(3)?,
            document_path: row.get(4)?,
            url: row.get(5)?,
            host: row.get(6)?,
            first_offset_ns: row.get(7)?,
            last_offset_ns: row.get(8)?,
            frontmost_ns: row.get(9)?,
            attached_at_utc_ms: row.get(10)?,
        })
    }

    fn into_window(self) -> Result<MeetingOpenWindow, StoreError> {
        let label = window_label(
            self.document_path.as_deref(),
            self.title.as_deref(),
            self.host.as_deref(),
        )
        .unwrap_or_default();
        Ok(MeetingOpenWindow {
            item_id: OpenWindowId::from_uuid(parse_uuid(&self.item_id)?),
            label,
            app_name: self.app_name,
            bundle_id: self.bundle_id,
            title: self.title,
            document_path: self.document_path,
            url: self.url,
            host: self.host,
            first_offset_ns: from_i64(self.first_offset_ns)?,
            last_offset_ns: from_i64(self.last_offset_ns)?,
            frontmost_ns: from_i64(self.frontmost_ns)?,
            attached_at_utc_ms: self.attached_at_utc_ms,
        })
    }
}
