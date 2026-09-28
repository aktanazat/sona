//! Per-series preferences: the notes template, digest inclusion, and the
//! standing grant that lets a series record itself.
//!
//! One row per calendar series in `meeting_series_preferences`, holding the
//! choices, plus a join onto `meeting_series_consents` for the third. The
//! template id stored is the same stable string an artifact revision carries
//! (`meeting-one-on-one`, and so on), so a preference and an artifact never
//! disagree about what a template is called.
//!
//! Resolution order: the meeting's own notes, its series, its folder, then the
//! app default. This module owns only the series rung.
//!
//! Always-record is deliberately *not* a column here. Permission to record is
//! consent: it lives in `meeting_series_consents`, where the grant carries its
//! policy version and acknowledged sources and where every auto-started
//! attempt revalidates it inside the transaction that writes its own receipt.
//! A duplicate boolean beside the template would be a second answer to a
//! question consent already owns.
//!
//! Every setter takes one decision at a time rather than upserting the whole
//! record. Three controls sit side by side on D28's Upcoming row, and a
//! whole-record write would let the template picker silently restore a digest
//! flag the operator had just cleared in another window.

use super::fence::{write_fenced, Fence, FencedWrite};
use super::{
    grant_series_consent_in, id, live_series_consent_in, parse_uuid, revoke_series_consent_in,
    MeetingStore, StoreError,
};
use crate::meeting::analytics::MeetingNotesTemplate;
use crate::meeting::series_types::{
    MeetingSeriesAlwaysRecordSetRequest, MeetingSeriesDigestSetRequest,
    MeetingSeriesMutationResult, MeetingSeriesPreferences, MeetingSeriesRemoteOptOutSetRequest,
    MeetingSeriesRemoteRoster, MeetingSeriesRemoteRow, MeetingSeriesTemplateSetRequest,
};
use crate::meeting::template_types::MeetingTemplateId;
use crate::meeting::types::{
    GeneratedMeetingArtifacts, MeetingCommandKind, MeetingOperationId, MeetingSessionId,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// How many series a roster offers.
///
/// A settings list is a place to make a decision, not an archive: two dozen
/// covers a working life's recurring meetings, and the number lives here rather
/// than in a caller so one surface cannot ask for a longer list than another.
pub(super) const SERIES_ROSTER_LIMIT: usize = 24;

/// The previous occurrence's link target and generated headline.
pub(crate) struct PreviousSeriesBrief {
    pub session_id: MeetingSessionId,
    pub headline: String,
}
impl MeetingStore {
    /// What one series has decided, by its own key.
    pub(crate) fn series_preferences(
        &self,
        series_key: &str,
    ) -> Result<MeetingSeriesPreferences, StoreError> {
        let connection = self.connection()?;
        series_preferences_in(&connection, Some(series_key.trim()))
    }

    /// The occurrence immediately before a calendar start, when its current
    /// artifact has a headline worth showing.
    pub(crate) fn previous_series_brief(
        &self,
        series_key: &str,
        before_utc_ms: i64,
    ) -> Result<Option<PreviousSeriesBrief>, StoreError> {
        let series_key = series_key.trim();
        if series_key.is_empty() {
            return Ok(None);
        }
        let connection = self.connection()?;
        previous_series_brief_in(&connection, series_key, before_utc_ms)
    }

    /// The fence every series write carries, on its own.
    ///
    /// A surface showing many series holds one counter, not one per row, so it
    /// reads the counter rather than picking it off whichever record happened
    /// to come back first.
    pub(crate) fn series_revision(&self) -> Result<u64, StoreError> {
        let connection = self.connection()?;
        series_revision_in(&connection)
    }

    /// What the series behind one meeting has decided.
    ///
    /// A meeting with no calendar event has no series, and the record says so
    /// with a `None` key rather than an error: "this meeting is not part of a
    /// series" is an answer the review surface has to render.
    pub(crate) fn series_preferences_for_session(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingSeriesPreferences, StoreError> {
        let connection = self.connection()?;
        let series_key = session_series_key_in(&connection, session_id)?;
        series_preferences_in(&connection, series_key.as_deref())
    }

    /// The same record for many series at once, keyed by series key.
    ///
    /// One read for a whole week of calendar rows. Series with no stored row
    /// and no standing grant are present in the map with their defaults, so the
    /// caller never has to decide what an absent key means.
    pub(crate) fn series_preferences_many(
        &self,
        series_keys: &[String],
    ) -> Result<HashMap<String, MeetingSeriesPreferences>, StoreError> {
        let connection = self.connection()?;
        let revision = series_revision_in(&connection)?;
        let mut records = HashMap::with_capacity(series_keys.len());
        for series_key in series_keys {
            let series_key = series_key.trim();
            if series_key.is_empty() || records.contains_key(series_key) {
                continue;
            }
            records.insert(
                series_key.to_string(),
                stored_series_preferences_in(&connection, series_key, revision)?,
            );
        }
        Ok(records)
    }

    /// D14. The roster with the one preference the meeting-intelligence surface
    /// can change attached.
    ///
    /// The list itself is [`series_roster_in`]'s; this only says which of those
    /// series are kept off the operator's server.
    pub(crate) fn series_remote_roster(&self) -> Result<MeetingSeriesRemoteRoster, StoreError> {
        let connection = self.connection()?;
        let revision = series_revision_in(&connection)?;
        let excluded = remote_opt_out_keys_in(&connection)?;
        let rows = series_roster_in(&connection, SERIES_ROSTER_LIMIT)?
            .into_iter()
            .map(|row| MeetingSeriesRemoteRow {
                remote_intelligence_opt_out: excluded.contains(&row.series_key),
                series_key: row.series_key,
                title: row.title,
                last_met_at_utc_ms: row.last_met_at_utc_ms,
                meetings: row.meetings,
            })
            .collect();
        Ok(MeetingSeriesRemoteRoster { rows, revision })
    }

    /// Remember, from the consent panel, whether this series announces itself.
    ///
    /// Written directly rather than through [`Self::write_series_preference`],
    /// exactly as the standing consent the same panel grants is: the panel is
    /// pressing Record, not editing a settings surface, so it holds no series
    /// revision to be fenced on and there is no second control to collide with.
    /// The fenced setters remain the path for anything that shows the decision.
    pub(crate) fn remember_series_announce(
        &self,
        series_key: &str,
        announce_in_chat: bool,
        now_utc_ms: i64,
    ) -> Result<(), StoreError> {
        let series_key = series_key.trim();
        if series_key.is_empty() {
            return Err(StoreError::Invalid);
        }
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO meeting_series_preferences (
                series_key, template_id, digest_included, announce_in_chat, updated_at_utc_ms
             ) VALUES (?1, NULL, 1, ?2, ?3)
             ON CONFLICT(series_key) DO UPDATE SET
                announce_in_chat = excluded.announce_in_chat,
                updated_at_utc_ms = excluded.updated_at_utc_ms",
            params![series_key, announce_in_chat, now_utc_ms],
        )?;
        delete_default_row_in(&connection, series_key)?;
        Ok(())
    }

    /// Remembers, or forgets, one series' template.
    ///
    /// Idempotent on `operation_id` and fenced on `expected_revision`, like
    /// every other receipted mutation: a replayed request returns the receipt it
    /// already wrote, and a stale one is rejected with the current revision so
    /// the caller can re-read and try again.
    pub(crate) fn set_series_template(
        &self,
        request: &MeetingSeriesTemplateSetRequest,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingSeriesMutationResult, StoreError> {
        self.write_series_preference(
            &request.series_key,
            request.operation_id,
            request.expected_revision,
            requested_at_utc_ms,
            MeetingCommandKind::SeriesTemplateSet,
            |connection, series_key, now| {
                if let Some(template_id) = request.custom_template_id {
                    if !super::custom_templates::custom_template_exists_in(connection, template_id)? {
                        return Err(StoreError::NotFound);
                    }
                }
                let template = if request.custom_template_id.is_some() {
                    None
                } else {
                    request.template.map(encode_series_template)
                };
                connection.execute(
                    "INSERT INTO meeting_series_preferences (
                        series_key, template_id, custom_template_id, digest_included, updated_at_utc_ms
                     ) VALUES (?1, ?2, ?3, 1, ?4)
                     ON CONFLICT(series_key) DO UPDATE SET
                        template_id = excluded.template_id,
                        custom_template_id = excluded.custom_template_id,
                        updated_at_utc_ms = excluded.updated_at_utc_ms",
                    params![series_key, template, request.custom_template_id.map(id), now],
                )?;
                delete_default_row_in(connection, series_key)?;
                Ok(())
            },
        )
    }

    /// D28. Keeps this series in the evening digest, or takes it out.
    pub(crate) fn set_series_digest(
        &self,
        request: &MeetingSeriesDigestSetRequest,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingSeriesMutationResult, StoreError> {
        self.write_series_preference(
            &request.series_key,
            request.operation_id,
            request.expected_revision,
            requested_at_utc_ms,
            MeetingCommandKind::SeriesDigestSet,
            |connection, series_key, now| {
                connection.execute(
                    "INSERT INTO meeting_series_preferences (
                        series_key, template_id, digest_included, updated_at_utc_ms
                     ) VALUES (?1, NULL, ?2, ?3)
                     ON CONFLICT(series_key) DO UPDATE SET
                        digest_included = excluded.digest_included,
                        updated_at_utc_ms = excluded.updated_at_utc_ms",
                    params![series_key, request.digest_included, now],
                )?;
                delete_default_row_in(connection, series_key)?;
                Ok(())
            },
        )
    }

    /// D14. Keeps this series' text on this Mac, or hands it back to the global
    /// meeting-intelligence setting.
    ///
    /// One column, one decision, one receipt — the same shape as the template
    /// and digest writes, and for the same reason: the exclusion must survive
    /// another pane saving a template from a stale read.
    pub(crate) fn set_series_remote_opt_out(
        &self,
        request: &MeetingSeriesRemoteOptOutSetRequest,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingSeriesMutationResult, StoreError> {
        self.write_series_preference(
            &request.series_key,
            request.operation_id,
            request.expected_revision,
            requested_at_utc_ms,
            MeetingCommandKind::SeriesRemoteOptOutSet,
            |connection, series_key, now| {
                connection.execute(
                    "INSERT INTO meeting_series_preferences (
                        series_key, template_id, digest_included,
                        remote_intelligence_opt_out, updated_at_utc_ms
                     ) VALUES (?1, NULL, 1, ?2, ?3)
                     ON CONFLICT(series_key) DO UPDATE SET
                        remote_intelligence_opt_out = excluded.remote_intelligence_opt_out,
                        updated_at_utc_ms = excluded.updated_at_utc_ms",
                    params![series_key, request.remote_intelligence_opt_out, now],
                )?;
                delete_default_row_in(connection, series_key)?;
                Ok(())
            },
        )
    }

    /// D28. Grants or revokes the standing consent that lets this series record
    /// itself.
    ///
    /// The write goes to `meeting_series_consents` through the same two
    /// primitives the consent panel uses, so an auto-start authorized from this
    /// toggle is indistinguishable — to the revalidation that gates it — from
    /// one authorized in the panel. Granting with no acknowledged source is
    /// rejected by that primitive, which is the invariant that keeps a grant
    /// from meaning "record something, unspecified".
    pub(crate) fn set_series_always_record(
        &self,
        request: &MeetingSeriesAlwaysRecordSetRequest,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingSeriesMutationResult, StoreError> {
        self.write_series_preference(
            &request.series_key,
            request.operation_id,
            request.expected_revision,
            requested_at_utc_ms,
            MeetingCommandKind::SeriesAlwaysRecordSet,
            |connection, series_key, now| {
                if request.always_record {
                    grant_series_consent_in(
                        connection,
                        series_key,
                        request.policy_version,
                        &request.acknowledged_sources,
                        now,
                    )?;
                } else {
                    revoke_series_consent_in(connection, series_key, now)?;
                }
                Ok(())
            },
        )
    }

    /// The one write path all four setters share.
    ///
    /// The skeleton — idempotency, the fence, the receipt, the revision bump —
    /// is [`write_fenced`]'s. All this adds is which counter fences a series
    /// preference, what a read of one returns, and that the series a write
    /// touched is the one thing its receipt cannot say on its own.
    fn write_series_preference(
        &self,
        series_key: &str,
        operation_id: MeetingOperationId,
        expected_revision: u64,
        requested_at_utc_ms: i64,
        command: MeetingCommandKind,
        write: impl FnOnce(&Connection, &str, i64) -> Result<(), StoreError>,
    ) -> Result<MeetingSeriesMutationResult, StoreError> {
        let series_key = series_key.trim();
        if series_key.is_empty() {
            return Err(StoreError::Invalid);
        }
        let (receipt, preferences) = write_fenced(
            self,
            FencedWrite {
                fence: SERIES_FENCE,
                command,
                effect_ids: vec![series_key.to_string()],
                operation_id,
                expected_revision,
                requested_at_utc_ms,
            },
            |connection| series_preferences_in(connection, Some(series_key)),
            |connection, now| write(connection, series_key, now),
        )?;
        Ok(MeetingSeriesMutationResult {
            receipt,
            preferences,
        })
    }
}

/// The series key behind one meeting, from the calendar facts the session
/// remembered when it started. `None` for a manual recording.
pub(crate) fn session_series_key_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Option<String>, StoreError> {
    let key: Option<String> = connection
        .query_row(
            "SELECT json_extract(event_json, '$.seriesKey')
               FROM meeting_calendar_facts WHERE session_id = ?1",
            params![id(session_id)],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(key.filter(|key| !key.trim().is_empty()))
}

fn previous_series_brief_in(
    connection: &Connection,
    series_key: &str,
    before_utc_ms: i64,
) -> Result<Option<PreviousSeriesBrief>, StoreError> {
    let row: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT f.session_id,
                    (SELECT a.content_json
                       FROM meeting_artifact_revisions a
                      WHERE a.session_id = f.session_id
                        AND a.state = 'current'
                        AND a.content_json IS NOT NULL
                      ORDER BY a.generated_at_utc_ms DESC LIMIT 1)
               FROM meeting_calendar_facts f
               JOIN meeting_sessions m ON m.id = f.session_id
              WHERE json_extract(f.event_json, '$.seriesKey') = ?1
                AND COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) < ?2
                AND m.phase != 'deleting'
              ORDER BY COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) DESC, m.id DESC
              LIMIT 1",
            params![series_key, before_utc_ms],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((session_id, Some(content_json))) = row else {
        return Ok(None);
    };
    let content: GeneratedMeetingArtifacts =
        serde_json::from_str(&content_json).map_err(|_| StoreError::Corrupt)?;
    let Some(headline) = content
        .headline()
        .map(str::trim)
        .filter(|headline| !headline.is_empty())
    else {
        return Ok(None);
    };
    let session_id = Uuid::parse_str(&session_id)
        .map(MeetingSessionId::from_uuid)
        .map_err(|_| StoreError::Corrupt)?;
    Ok(Some(PreviousSeriesBrief {
        session_id,
        headline: headline.to_string(),
    }))
}

/// One series on a roster: what every surface listing series needs, before any
/// one of them attaches what only it cares about.
pub(super) struct SeriesRosterRow {
    pub series_key: String,
    pub title: String,
    pub last_met_at_utc_ms: i64,
    pub meetings: u32,
}

/// The series this Mac has actually met with, newest first.
///
/// There is no series table: a series is known only through the meetings that
/// carried it, so the roster is derived from the calendar facts those meetings
/// recorded. That is also what makes it honest — a series Sona has never sat in
/// cannot be offered a switch it was never part of.
///
/// Every surface that lists series reads this one function, because two
/// settings screens showing a different set of series, a different name for the
/// same series, or a different count, from one corpus is a disagreement no
/// caller can see and none of them could be blamed for. So the three rules that
/// used to differ per surface are settled here: a meeting on its way out
/// (`phase = 'deleting'`) does not count towards a series it is about to leave;
/// the title is the one the most recent occurrence that had a title used, which
/// is the name the operator's calendar shows today rather than whatever the
/// series was first called; and the list is bounded for everyone.
pub(super) fn series_roster_in(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<SeriesRosterRow>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT g.series_key,
                (SELECT trim(json_extract(recent.event_json, '$.title'))
                   FROM meeting_calendar_facts recent
                   JOIN meeting_sessions recent_sessions
                     ON recent_sessions.id = recent.session_id
                  WHERE json_extract(recent.event_json, '$.seriesKey') = g.series_key
                    AND recent_sessions.phase != 'deleting'
                    AND trim(COALESCE(json_extract(recent.event_json, '$.title'), '')) <> ''
                  ORDER BY COALESCE(recent_sessions.started_at_utc_ms,
                                    recent_sessions.created_at_utc_ms) DESC
                  LIMIT 1) AS title,
                g.last_met_at,
                g.meetings
           FROM (SELECT json_extract(facts.event_json, '$.seriesKey') AS series_key,
                        MAX(COALESCE(sessions.started_at_utc_ms,
                                     sessions.created_at_utc_ms)) AS last_met_at,
                        COUNT(*) AS meetings
                   FROM meeting_calendar_facts facts
                   JOIN meeting_sessions sessions ON sessions.id = facts.session_id
                  WHERE sessions.phase != 'deleting'
                    AND json_extract(facts.event_json, '$.seriesKey') IS NOT NULL
                    AND trim(json_extract(facts.event_json, '$.seriesKey')) <> ''
                  GROUP BY series_key) g
          ORDER BY g.last_met_at DESC, g.series_key
          LIMIT ?1",
    )?;
    let mut rows = statement.query(params![
        i64::try_from(limit).map_err(|_| StoreError::Corrupt)?
    ])?;
    let mut roster = Vec::new();
    while let Some(row) = rows.next()? {
        let title: Option<String> = row.get(1)?;
        roster.push(SeriesRosterRow {
            series_key: row.get(0)?,
            title: title.unwrap_or_default(),
            last_met_at_utc_ms: row.get(2)?,
            meetings: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(u32::MAX),
        });
    }
    Ok(roster)
}

/// The series kept off the operator's server, as a set: a roster attaches this
/// column to two dozen rows at once, and one read beats a lookup per row.
fn remote_opt_out_keys_in(connection: &Connection) -> Result<HashSet<String>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT series_key FROM meeting_series_preferences
          WHERE remote_intelligence_opt_out = 1",
    )?;
    let keys = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(keys)
}

/// True unless this series has been taken out of the evening digest. A series
/// with no preference row is in it, which is what makes the digest's own query
/// a plain `LEFT JOIN`.
pub(super) fn series_digest_included_in(
    connection: &Connection,
    series_key: &str,
) -> Result<bool, StoreError> {
    let included: Option<bool> = connection
        .query_row(
            "SELECT digest_included FROM meeting_series_preferences WHERE series_key = ?1",
            params![series_key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(included.unwrap_or(true))
}

fn series_template_in(
    connection: &Connection,
    series_key: &str,
) -> Result<(Option<MeetingNotesTemplate>, Option<MeetingTemplateId>), StoreError> {
    let stored: Option<(Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT template_id, custom_template_id FROM meeting_series_preferences WHERE series_key = ?1",
            params![series_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((template, custom)) = stored else {
        return Ok((None, None));
    };
    if let Some(custom) = custom {
        let template_id = MeetingTemplateId::from_uuid(parse_uuid(&custom)?);
        let exists = super::custom_templates::custom_template_exists_in(connection, template_id)?;
        return Ok((None, exists.then_some(template_id)));
    }
    Ok((
        template
            .as_deref()
            .map(decode_series_template)
            .transpose()?,
        None,
    ))
}

/// Clearing a deleted template must preserve the series' other choices.
pub(super) fn forget_custom_template_in(
    connection: &Connection,
    template_id: MeetingTemplateId,
) -> Result<(), StoreError> {
    let mut statement = connection.prepare(
        "SELECT series_key FROM meeting_series_preferences WHERE custom_template_id = ?1",
    )?;
    let keys = statement
        .query_map(params![id(template_id)], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if keys.is_empty() {
        return Ok(());
    }
    connection.execute(
        "UPDATE meeting_series_preferences SET template_id = NULL, custom_template_id = NULL
         WHERE custom_template_id = ?1",
        params![id(template_id)],
    )?;
    for key in keys {
        delete_default_row_in(connection, &key)?;
    }
    bump_series_revision_in(connection)?;
    Ok(())
}

/// D14. False unless this series has been kept off the operator's server. A
/// series with no preference row follows the global setting, which is what
/// makes "no row" mean "no departure from it".
pub(super) fn series_remote_opt_out_in(
    connection: &Connection,
    series_key: &str,
) -> Result<bool, StoreError> {
    let opted_out: Option<bool> = connection
        .query_row(
            "SELECT remote_intelligence_opt_out FROM meeting_series_preferences
              WHERE series_key = ?1",
            params![series_key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(opted_out.unwrap_or(false))
}

/// Whether this series announces itself in the meeting's chat. False for a
/// series that has never been asked to, which is every series until somebody
/// ticks the box.
pub(super) fn series_announce_in_chat_in(
    connection: &Connection,
    series_key: &str,
) -> Result<bool, StoreError> {
    let announce: Option<bool> = connection
        .query_row(
            "SELECT announce_in_chat FROM meeting_series_preferences WHERE series_key = ?1",
            params![series_key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(announce.unwrap_or(false))
}

fn series_preferences_in(
    connection: &Connection,
    series_key: Option<&str>,
) -> Result<MeetingSeriesPreferences, StoreError> {
    let revision = series_revision_in(connection)?;
    let Some(series_key) = series_key.filter(|key| !key.is_empty()) else {
        return Ok(MeetingSeriesPreferences {
            series_key: None,
            template: None,
            custom_template_id: None,
            digest_included: true,
            always_record: false,
            remote_intelligence_opt_out: false,
            announce_in_chat: false,
            revision,
        });
    };
    stored_series_preferences_in(connection, series_key, revision)
}

/// The record for a key that is known to be a real series, with the revision
/// already read. Split out so a bulk read pays for the counter once.
fn stored_series_preferences_in(
    connection: &Connection,
    series_key: &str,
    revision: u64,
) -> Result<MeetingSeriesPreferences, StoreError> {
    let (template, custom_template_id) = series_template_in(connection, series_key)?;
    Ok(MeetingSeriesPreferences {
        series_key: Some(series_key.to_string()),
        template,
        custom_template_id,
        digest_included: series_digest_included_in(connection, series_key)?,
        always_record: live_series_consent_in(connection, series_key)?.is_some(),
        remote_intelligence_opt_out: series_remote_opt_out_in(connection, series_key)?,
        announce_in_chat: series_announce_in_chat_in(connection, series_key)?,
        revision,
    })
}

/// Drops a preference row that no longer holds a preference.
///
/// Without this, clearing every choice would leave a row that says exactly what
/// its absence says, and "has a row" would stop being usable as "has decided
/// something". Every column a decision can land in has to appear here: a row
/// kept alive only by an exclusion must survive a template being cleared.
fn delete_default_row_in(connection: &Connection, series_key: &str) -> Result<(), StoreError> {
    connection.execute(
        "DELETE FROM meeting_series_preferences
          WHERE series_key = ?1 AND template_id IS NULL AND custom_template_id IS NULL AND digest_included = 1
            AND remote_intelligence_opt_out = 0 AND announce_in_chat = 0",
        params![series_key],
    )?;
    Ok(())
}

fn series_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    let revision: i64 = connection.query_row(
        "SELECT revision FROM meeting_series_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(revision).map_err(|_| StoreError::Corrupt)
}

fn bump_series_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    connection.execute(
        "UPDATE meeting_series_state SET revision = revision + 1 WHERE singleton = 1",
        [],
    )?;
    series_revision_in(connection)
}

/// The counter every series-preference write is fenced on. One per surface
/// showing many series, not one per row: see [`MeetingStore::series_revision`].
const SERIES_FENCE: Fence = Fence {
    read: series_revision_in,
    bump: bump_series_revision_in,
};

/// The stored form is the artifact template id, not the serde name: an
/// artifact revision already persists these strings, and one spelling per
/// template is what keeps a preference and the artifact it produced legible as
/// the same choice.
fn encode_series_template(template: MeetingNotesTemplate) -> &'static str {
    template.artifact_template_id()
}

/// A stored id this build does not know is corruption, not a silent fallback:
/// answering "General" for a template a newer build wrote would show the user a
/// choice they never made.
fn decode_series_template(stored: &str) -> Result<MeetingNotesTemplate, StoreError> {
    MeetingNotesTemplate::from_artifact_template_id(stored).ok_or(StoreError::Corrupt)
}
