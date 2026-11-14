//! The end of a day at the store boundary: the five lines Home shows, and the
//! maybe-filings for recordings nobody named.
//!
//! A stray is a finished recording with no calendar event and a title Sona
//! made up itself. Each stray is reviewed once, in `filing_reviews`; what the
//! review found waits in `filing_suggestions` until the user files it or
//! turns it down. Nothing about a stray changes until he does. The same table
//! holds the maybes a recording gets from its start, its kind, its place and
//! the meeting it may be; a kind or a place describes a recording but files
//! it nowhere.

use super::loops::loops_in;
use super::people::{
    bump_people_revision_in, derive_calendar_links_in, exact_phrase_mentioned,
    recompute_organizations_in, upsert_link_in,
};
use super::voice_identity::VoiceSpeakerSuggestion;
use super::{
    decode_json, effective_segments_for_session, encode_json, id, parse_uuid, MeetingStore,
    StoreError, READABLE_ARTIFACT_CONTENT,
};
use crate::meeting::detection::calendar::CalendarOccurrence;
use crate::meeting::detection::machine::CalendarEventSummary;
use crate::meeting::evening::{
    EveningLines, FilingReason, FilingSuggestion, FilingTarget, UnfiledRecording,
};
use crate::meeting::loop_types::{MeetingLoopDirection, MeetingLoopRow, MeetingLoopStatus};
use crate::meeting::people_types::{PersonId, PersonLinkConfidence, PersonLinkSource};
use crate::meeting::types::{
    GeneratedMeetingArtifacts, MeetingSessionId, TranscriptSegmentId, MANUAL_DEFAULT_TITLE,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashSet;
use uuid::Uuid;

/// A recording Sona titled itself and no calendar event explains. `?1` is
/// the default title.
const STRAY: &str = "m.phase = 'review_ready'
    AND NOT EXISTS (SELECT 1 FROM meeting_calendar_facts f WHERE f.session_id = m.id)
    AND (m.title = ?1
         OR (EXISTS (SELECT 1 FROM meeting_session_events e
                      WHERE e.session_id = m.id AND e.event_kind = 'title_derived')
             AND NOT EXISTS (SELECT 1 FROM meeting_session_events e
                              WHERE e.session_id = m.id AND e.event_kind = 'title_set')))";

/// A stray nobody has put anywhere yet: no event, person or project filing
/// accepted, no confirmed person.
const NOT_FILED: &str = "NOT EXISTS (SELECT 1 FROM filing_suggestions s
                    WHERE s.session_id = m.id AND s.state = 'accepted'
                      AND s.target_kind IN ('event', 'person', 'project'))
    AND NOT EXISTS (SELECT 1 FROM meeting_person_links l
                     WHERE l.meeting_id = m.id AND l.confidence = 'confirmed')";

/// The evening digest's own rule: a series the user left out of the digest
/// stays out of the evening too.
const IN_DIGEST: &str = "COALESCE((
      SELECT p.digest_included
        FROM meeting_calendar_facts f
        JOIN meeting_series_preferences p
          ON p.series_key = json_extract(f.event_json, '$.seriesKey')
       WHERE f.session_id = m.id
    ), 1) = 1";

/// Calendar events this far either side of a recording still count as near it.
pub(crate) const FILING_EVENT_SLACK_MS: i64 = 15 * 60 * 1000;
const MAX_PROJECT_SUGGESTIONS: usize = 2;
/// A project title said once is a mention; twice is a topic.
const MIN_PROJECT_MENTIONS: u32 = 2;
const MIN_PROJECT_TITLE_CHARS: usize = 4;
const MIN_ATTENDEE_NAME_CHARS: usize = 3;
const ALL_DAY_MS: i64 = 12 * 60 * 60 * 1000;

/// One meeting or recording that started inside the day.
pub(crate) struct EveningMeeting {
    pub session_id: MeetingSessionId,
    pub title: String,
    pub started_at_utc_ms: i64,
    pub ended_at_utc_ms: Option<i64>,
    pub people: Vec<String>,
    pub summary: Option<String>,
    pub decisions: Vec<String>,
    pub promises: Vec<EveningLoop>,
    pub waiting_on: Vec<EveningLoop>,
    pub not_filed: bool,
}

pub(crate) struct EveningLoop {
    pub text: String,
    pub who: Option<String>,
    pub due: Option<String>,
}

/// The lines stored for one day, with the sources they were written from.
pub(crate) struct StoredEveningLines {
    pub lines: EveningLines,
    pub meeting_ids: Vec<MeetingSessionId>,
    pub dictation_ids: Vec<i64>,
}

/// A stray waiting for its one review.
pub(crate) struct StrayCandidate {
    pub session_id: MeetingSessionId,
    pub started_at_utc_ms: i64,
    pub ended_at_utc_ms: i64,
}

/// An active project the review may match against a stray's words.
pub(crate) struct FilingProject {
    pub project_id: String,
    pub title: String,
    pub linked_meetings: HashSet<String>,
}

/// What one review read from outside the meeting store.
pub(crate) struct StrayEvidence<'a> {
    pub events: &'a [CalendarOccurrence],
    pub voices: &'a [VoiceSpeakerSuggestion],
    pub projects: &'a [FilingProject],
}

/// One pending filing read back for its decision.
pub(crate) struct PendingFiling {
    pub session_id: MeetingSessionId,
    pub target: FilingTarget,
}

struct Spoken {
    segment_id: TranscriptSegmentId,
    text: String,
}

impl MeetingStore {
    pub(crate) fn evening_meetings(
        &self,
        day_start_utc_ms: i64,
        day_end_utc_ms: i64,
    ) -> Result<Vec<EveningMeeting>, StoreError> {
        let user_name = self.user_name();
        let mut connection = self.reader()?;
        let transaction = connection.transaction()?;
        let mut statement = transaction.prepare(&format!(
            "SELECT m.id, m.title, m.started_at_utc_ms, m.ended_at_utc_ms,
                    ({READABLE_ARTIFACT_CONTENT}),
                    CASE WHEN {STRAY} AND {NOT_FILED} THEN 1 ELSE 0 END
               FROM meeting_sessions m
              WHERE m.phase != 'deleting'
                AND m.started_at_utc_ms IS NOT NULL
                AND m.started_at_utc_ms >= ?2
                AND m.started_at_utc_ms < ?3
                AND {IN_DIGEST}
              ORDER BY m.started_at_utc_ms, m.id"
        ))?;
        let rows = statement
            .query_map(
                params![MANUAL_DEFAULT_TITLE, day_start_utc_ms, day_end_utc_ms],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let meetings = rows
            .into_iter()
            .map(|(session, title, started, ended, content, not_filed)| {
                let session_id = MeetingSessionId::from_uuid(parse_uuid(&session)?);
                let artifacts = content
                    .as_deref()
                    .map(decode_json::<GeneratedMeetingArtifacts>)
                    .transpose()?;
                evening_meeting(
                    &transaction,
                    session_id,
                    title,
                    (started, ended),
                    artifacts,
                    not_filed,
                    user_name.as_deref(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        transaction.commit()?;
        Ok(meetings)
    }

    pub(crate) fn evening_lines(
        &self,
        local_day: &str,
    ) -> Result<Option<StoredEveningLines>, StoreError> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT lines_json, meeting_ids_json, dictation_ids_json, model_id,
                        written_at_utc_ms
                   FROM evening_lines WHERE local_day = ?1",
                [local_day],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((lines, meetings, dictations, model_id, written_at_utc_ms)) = row else {
            return Ok(None);
        };
        let meeting_ids: Vec<MeetingSessionId> = decode_json(&meetings)?;
        for meeting in &meeting_ids {
            let present = connection
                .query_row(
                    "SELECT 1 FROM meeting_sessions WHERE id = ?1 AND phase != 'deleting'",
                    [id(*meeting)],
                    |_| Ok(()),
                )
                .optional()?;
            if present.is_none() {
                connection.execute(
                    "DELETE FROM evening_lines WHERE local_day = ?1",
                    [local_day],
                )?;
                return Ok(None);
            }
        }
        Ok(Some(StoredEveningLines {
            lines: EveningLines {
                lines: decode_json(&lines)?,
                written_at_utc_ms,
                model_id,
            },
            meeting_ids,
            dictation_ids: decode_json(&dictations)?,
        }))
    }

    pub(crate) fn forget_evening_lines(&self, local_day: &str) -> Result<(), StoreError> {
        self.connection()?.execute(
            "DELETE FROM evening_lines WHERE local_day = ?1",
            [local_day],
        )?;
        Ok(())
    }

    pub(crate) fn save_evening_lines(
        &self,
        local_day: &str,
        stored: &StoredEveningLines,
    ) -> Result<(), StoreError> {
        self.connection()?.execute(
            "INSERT INTO evening_lines(
                 local_day, lines_json, meeting_ids_json, dictation_ids_json, model_id,
                 written_at_utc_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(local_day) DO UPDATE SET
                 lines_json = excluded.lines_json,
                 meeting_ids_json = excluded.meeting_ids_json,
                 dictation_ids_json = excluded.dictation_ids_json,
                 model_id = excluded.model_id,
                 written_at_utc_ms = excluded.written_at_utc_ms",
            params![
                local_day,
                encode_json(&stored.lines.lines)?,
                encode_json(&stored.meeting_ids)?,
                encode_json(&stored.dictation_ids)?,
                stored.lines.model_id,
                stored.lines.written_at_utc_ms,
            ],
        )?;
        Ok(())
    }

    /// Strays that started at or after `since_utc_ms` and were never reviewed.
    pub(crate) fn stray_candidates(
        &self,
        since_utc_ms: i64,
    ) -> Result<Vec<StrayCandidate>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT m.id, m.started_at_utc_ms, COALESCE(m.ended_at_utc_ms, m.started_at_utc_ms)
               FROM meeting_sessions m
              WHERE {STRAY}
                AND m.started_at_utc_ms >= ?2
                AND NOT EXISTS (SELECT 1 FROM filing_reviews r WHERE r.session_id = m.id)
              ORDER BY m.started_at_utc_ms, m.id"
        ))?;
        let rows = statement.query_map(params![MANUAL_DEFAULT_TITLE, since_utc_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (session, started, ended) = row?;
            Ok(StrayCandidate {
                session_id: MeetingSessionId::from_uuid(parse_uuid(&session)?),
                started_at_utc_ms: started,
                ended_at_utc_ms: ended.max(started),
            })
        })
        .collect()
    }

    /// Reviews one stray once: whatever the evidence supports waits as a
    /// pending suggestion, and the stray is never reviewed again.
    pub(crate) fn record_stray_review(
        &self,
        stray: &StrayCandidate,
        evidence: &StrayEvidence<'_>,
        now_utc_ms: i64,
    ) -> Result<usize, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reviewed = transaction
            .query_row(
                "SELECT 1 FROM filing_reviews WHERE session_id = ?1",
                [id(stray.session_id)],
                |_| Ok(()),
            )
            .optional()?;
        if reviewed.is_some() {
            return Ok(0);
        }
        let spoken = effective_segments_for_session(&transaction, stray.session_id)?
            .into_iter()
            .filter(|segment| !segment.removed)
            .map(|segment| Spoken {
                segment_id: segment.base.segment_id,
                text: segment.replacement_text.unwrap_or(segment.base.text),
            })
            .collect::<Vec<_>>();
        let filed_events = filed_event_keys_in(&transaction)?;
        let linked_people = confirmed_people_in(&transaction, stray.session_id)?;
        let suggestions =
            stray_suggestions(stray, evidence, &spoken, &filed_events, &linked_people);
        for (target, reasons) in &suggestions {
            insert_suggestion_in(&transaction, stray.session_id, target, reasons, now_utc_ms)?;
        }
        transaction.execute(
            "INSERT INTO filing_reviews(session_id, reviewed_at_utc_ms) VALUES (?1, ?2)",
            params![id(stray.session_id), now_utc_ms],
        )?;
        transaction.commit()?;
        Ok(suggestions.len())
    }

    /// Strays with a suggestion still waiting for the user, oldest first.
    pub(crate) fn unfiled_recordings(&self) -> Result<Vec<UnfiledRecording>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT m.id, m.title, m.started_at_utc_ms, m.ended_at_utc_ms,
                    s.id, s.target_json, s.reasons_json
               FROM filing_suggestions s
               JOIN meeting_sessions m ON m.id = s.session_id
              WHERE s.state = 'pending' AND s.target_kind IN ('event', 'person', 'project')
                AND {STRAY}
              ORDER BY m.started_at_utc_ms, m.id, s.target_kind, s.created_at_utc_ms, s.id"
        ))?;
        let rows = statement.query_map([MANUAL_DEFAULT_TITLE], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        let mut recordings: Vec<UnfiledRecording> = Vec::new();
        for row in rows {
            let (session, title, started, ended, suggestion, target, reasons) = row?;
            let session_id = MeetingSessionId::from_uuid(parse_uuid(&session)?);
            let suggestion = FilingSuggestion {
                id: suggestion,
                target: decode_json(&target)?,
                reasons: decode_json(&reasons)?,
            };
            match recordings.last_mut() {
                Some(recording) if recording.session_id == session_id => {
                    recording.suggestions.push(suggestion);
                }
                _ => recordings.push(UnfiledRecording {
                    session_id,
                    title,
                    started_at_utc_ms: started.ok_or(StoreError::Corrupt)?,
                    ended_at_utc_ms: ended,
                    suggestions: vec![suggestion],
                }),
            }
        }
        Ok(recordings)
    }

    pub(crate) fn pending_filing(&self, suggestion_id: &str) -> Result<PendingFiling, StoreError> {
        let (session, target): (String, String) = self
            .connection()?
            .query_row(
                "SELECT session_id, target_json FROM filing_suggestions
                  WHERE id = ?1 AND state = 'pending'",
                [suggestion_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        Ok(PendingFiling {
            session_id: MeetingSessionId::from_uuid(parse_uuid(&session)?),
            target: decode_json(&target)?,
        })
    }

    pub(crate) fn decline_filing(
        &self,
        suggestion_id: &str,
        now_utc_ms: i64,
    ) -> Result<(), StoreError> {
        let changed = self.connection()?.execute(
            "UPDATE filing_suggestions SET state = 'declined', decided_at_utc_ms = ?2
              WHERE id = ?1 AND state = 'pending'",
            params![suggestion_id, now_utc_ms],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// Marks a filing done once its write elsewhere has landed.
    pub(crate) fn accept_filing(
        &self,
        suggestion_id: &str,
        now_utc_ms: i64,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        accept_filing_in(&connection, suggestion_id, now_utc_ms)
    }

    /// Files a stray with the person whose voice matched: the same confirmed
    /// manual link the meeting's People row adds. Returns the people revision.
    pub(crate) fn file_with_person(
        &self,
        suggestion_id: &str,
        session_id: MeetingSessionId,
        person_id: PersonId,
        now_utc_ms: i64,
    ) -> Result<Option<u64>, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = upsert_link_in(
            &transaction,
            session_id,
            person_id,
            PersonLinkSource::Manual,
            PersonLinkConfidence::Confirmed,
            now_utc_ms,
        )?;
        let revision = changed
            .then(|| bump_people_revision_in(&transaction))
            .transpose()?;
        accept_filing_in(&transaction, suggestion_id, now_utc_ms)?;
        transaction.commit()?;
        Ok(revision)
    }

    /// Links the event's attendees the way a meeting started from that event
    /// would, and closes the stray's other event suggestions. The calendar
    /// facts themselves are already remembered. Returns the people revision.
    pub(crate) fn file_under_event(
        &self,
        suggestion_id: &str,
        session_id: MeetingSessionId,
        event: &CalendarEventSummary,
        now_utc_ms: i64,
    ) -> Result<Option<u64>, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changes =
            derive_calendar_links_in(&transaction, session_id, &event.attendees, now_utc_ms)?
                + recompute_organizations_in(&transaction)?;
        let revision = (changes != 0)
            .then(|| bump_people_revision_in(&transaction))
            .transpose()?;
        accept_filing_in(&transaction, suggestion_id, now_utc_ms)?;
        transaction.execute(
            "UPDATE filing_suggestions SET state = 'declined', decided_at_utc_ms = ?2
              WHERE session_id = ?1 AND target_kind = 'event' AND state = 'pending'",
            params![id(session_id), now_utc_ms],
        )?;
        transaction.commit()?;
        Ok(revision)
    }
}

pub(super) fn accept_filing_in(
    connection: &Connection,
    suggestion_id: &str,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE filing_suggestions SET state = 'accepted', decided_at_utc_ms = ?2
          WHERE id = ?1 AND state = 'pending'",
        params![suggestion_id, now_utc_ms],
    )?;
    Ok(())
}

fn evening_meeting(
    connection: &Connection,
    session_id: MeetingSessionId,
    title: String,
    (started_at_utc_ms, ended_at_utc_ms): (i64, Option<i64>),
    artifacts: Option<GeneratedMeetingArtifacts>,
    not_filed: bool,
    user_name: Option<&str>,
) -> Result<EveningMeeting, StoreError> {
    let people = confirmed_names_in(connection, session_id)?;
    let loops = loops_in(connection, session_id, user_name)?.rows;
    let due = |row: &MeetingLoopRow| {
        artifacts.as_ref().and_then(|artifacts| {
            artifacts
                .action_items
                .iter()
                .find(|item| item.text.text.trim().eq_ignore_ascii_case(row.text.trim()))
                .and_then(|item| item.due_text.clone())
        })
    };
    let open = |direction: MeetingLoopDirection| {
        loops
            .iter()
            .filter(|row| row.direction == direction && row.status == MeetingLoopStatus::Open)
            .map(|row| EveningLoop {
                text: row.text.clone(),
                who: row
                    .owner_display_name
                    .clone()
                    .or_else(|| row.owner_text.clone()),
                due: due(row),
            })
            .collect::<Vec<_>>()
    };
    let promises = open(MeetingLoopDirection::Mine);
    let waiting_on = open(MeetingLoopDirection::WaitingOn);
    let summary = artifacts
        .as_ref()
        .map(|artifacts| artifacts.summary.text.trim().to_string())
        .filter(|summary| !summary.is_empty());
    let decisions = artifacts
        .map(|artifacts| {
            artifacts
                .decisions
                .into_iter()
                .map(|decision| decision.text)
                .filter(|text| !text.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();
    Ok(EveningMeeting {
        session_id,
        title,
        started_at_utc_ms,
        ended_at_utc_ms,
        people,
        summary,
        decisions,
        promises,
        waiting_on,
        not_filed,
    })
}

pub(super) fn filed_event_keys_in(connection: &Connection) -> Result<HashSet<String>, StoreError> {
    let mut statement = connection.prepare("SELECT event_key FROM meeting_calendar_facts")?;
    let keys = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(keys)
}

pub(super) fn confirmed_people_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<HashSet<PersonId>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT person_id FROM meeting_person_links
          WHERE meeting_id = ?1 AND confidence = 'confirmed'",
    )?;
    let ids = statement
        .query_map([id(session_id)], |row| row.get::<_, String>(0))?
        .map(|person| Ok(PersonId::from_uuid(parse_uuid(&person?)?)))
        .collect::<Result<HashSet<_>, StoreError>>()?;
    Ok(ids)
}

/// The names of the people confirmed in a meeting, alphabetical.
pub(super) fn confirmed_names_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<String>, StoreError> {
    let mut statement = connection.prepare_cached(
        "SELECT p.display_name FROM meeting_person_links l
           JOIN persons p ON p.id = l.person_id
          WHERE l.meeting_id = ?1 AND l.confidence = 'confirmed'
          ORDER BY p.display_name COLLATE NOCASE, p.id",
    )?;
    let names = statement
        .query_map([id(session_id)], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

pub(super) fn target_identity(target: &FilingTarget) -> Result<(&'static str, String), StoreError> {
    Ok(match target {
        FilingTarget::Event { event_key, .. } => ("event", event_key.clone()),
        FilingTarget::Person { person_id, .. } => ("person", person_id.uuid().to_string()),
        FilingTarget::Project { project_id, .. } => ("project", project_id.clone()),
        FilingTarget::Kind { conversation_kind } => ("kind", encode_json(conversation_kind)?),
        FilingTarget::Place { location } => (
            "place",
            format!("{:.6},{:.6}", location.latitude, location.longitude),
        ),
        FilingTarget::Introducer { person_id, .. } => ("introducer", person_id.uuid().to_string()),
        FilingTarget::Meeting { plan_id, .. } => ("meeting", plan_id.uuid().to_string()),
    })
}

/// Leaves one maybe waiting on a recording. A target the recording already
/// has a suggestion for stays as it is, so a maybe turned down is never
/// offered again.
pub(super) fn insert_suggestion_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    target: &FilingTarget,
    reasons: &[FilingReason],
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    let (kind, key) = target_identity(target)?;
    connection.execute(
        "INSERT INTO filing_suggestions(
             id, session_id, target_kind, target_key, target_json, reasons_json, state,
             created_at_utc_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)
         ON CONFLICT(session_id, target_kind, target_key) DO NOTHING",
        params![
            Uuid::new_v4().to_string(),
            id(session_id),
            kind,
            key,
            encode_json(target)?,
            encode_json(reasons)?,
            now_utc_ms,
        ],
    )?;
    Ok(())
}

pub(super) fn event_target(event: &CalendarEventSummary) -> FilingTarget {
    FilingTarget::Event {
        event_key: event.event_key.clone(),
        title: event.title.clone(),
        start_utc_ms: event.start_utc_ms,
        end_utc_ms: event.end_utc_ms,
    }
}

/// What a review suggests for one stray: the calendar event nearest it, each
/// known voice in it, and the projects it kept talking about.
fn stray_suggestions(
    stray: &StrayCandidate,
    evidence: &StrayEvidence<'_>,
    spoken: &[Spoken],
    filed_events: &HashSet<String>,
    linked_people: &HashSet<PersonId>,
) -> Vec<(FilingTarget, Vec<FilingReason>)> {
    let mut suggestions = Vec::new();
    if let Some(event) = near_events(
        stray.started_at_utc_ms,
        stray.ended_at_utc_ms,
        evidence.events,
        filed_events,
    )
    .into_iter()
    .next()
    {
        let overlaps = event.start_utc_ms < stray.ended_at_utc_ms
            && stray.started_at_utc_ms < event.end_utc_ms;
        let mut reasons = vec![FilingReason::OnCalendar { overlaps }];
        let mut names = HashSet::new();
        for attendee in event.attendees.iter().filter(|attendee| !attendee.is_self) {
            let Some(first) = attendee.name.split_whitespace().next() else {
                continue;
            };
            if first.chars().count() < MIN_ATTENDEE_NAME_CHARS
                || !names.insert(first.to_lowercase())
            {
                continue;
            }
            reasons.extend(said(spoken, first));
        }
        suggestions.push((event_target(event), reasons));
    }
    let mut people = HashSet::new();
    for voice in evidence.voices {
        let Some(person_id) = voice.person_id else {
            continue;
        };
        if linked_people.contains(&person_id) || !people.insert(person_id) {
            continue;
        }
        suggestions.push((
            FilingTarget::Person {
                person_id,
                name: voice.display_name.clone(),
            },
            vec![FilingReason::VoiceMatches {
                name: voice.display_name.clone(),
                start_offset_ns: None,
            }],
        ));
    }
    let session = stray.session_id.uuid().to_string();
    let mut projects = evidence
        .projects
        .iter()
        .filter(|project| {
            !project.linked_meetings.contains(&session)
                && project
                    .title
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .count()
                    >= MIN_PROJECT_TITLE_CHARS
        })
        .filter_map(|project| {
            let reason = said(spoken, project.title.trim())?;
            let FilingReason::Said { count, .. } = &reason else {
                return None;
            };
            (*count >= MIN_PROJECT_MENTIONS).then_some((*count, project, reason))
        })
        .collect::<Vec<_>>();
    projects.sort_by(|left, right| right.0.cmp(&left.0));
    suggestions.extend(projects.into_iter().take(MAX_PROJECT_SUGGESTIONS).map(
        |(_, project, reason)| {
            (
                FilingTarget::Project {
                    project_id: project.project_id.clone(),
                    title: project.title.trim().to_string(),
                },
                vec![reason],
            )
        },
    ));
    suggestions
}

/// The timed events near a recording from `started_at_utc_ms` to
/// `ended_at_utc_ms`: the one that overlaps it most first, then the ones that
/// start or end nearest it. An event another meeting already holds is not a
/// candidate.
pub(super) fn near_events<'a>(
    started_at_utc_ms: i64,
    ended_at_utc_ms: i64,
    events: &'a [CalendarOccurrence],
    filed_events: &HashSet<String>,
) -> Vec<&'a CalendarEventSummary> {
    let mut near = events
        .iter()
        .map(|occurrence| &occurrence.summary)
        .filter(|event| {
            event.end_utc_ms > event.start_utc_ms
                && event.end_utc_ms - event.start_utc_ms < ALL_DAY_MS
                && event.start_utc_ms < ended_at_utc_ms + FILING_EVENT_SLACK_MS
                && started_at_utc_ms - FILING_EVENT_SLACK_MS < event.end_utc_ms
                && !filed_events.contains(&event.event_key)
        })
        .collect::<Vec<_>>();
    near.sort_by_key(|event| {
        let overlap =
            event.end_utc_ms.min(ended_at_utc_ms) - event.start_utc_ms.max(started_at_utc_ms);
        (std::cmp::Reverse(overlap), event.event_key.clone())
    });
    near
}

/// How often the transcript says `phrase` as whole words, and where first.
fn said(spoken: &[Spoken], phrase: &str) -> Option<FilingReason> {
    let mut hits = spoken
        .iter()
        .filter(|segment| exact_phrase_mentioned(&segment.text, phrase));
    let first = hits.next()?;
    let count = u32::try_from(1 + hits.count()).unwrap_or(u32::MAX);
    Some(FilingReason::Said {
        phrase: phrase.to_string(),
        count,
        segment_id: first.segment_id,
    })
}
