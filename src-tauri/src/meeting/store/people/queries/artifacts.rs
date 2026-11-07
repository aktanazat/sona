use crate::meeting::analytics::MeetingAnalytics;
use crate::meeting::ledger::LedgerOutcomeCounts;
use crate::meeting::loop_types::{waiting_on_is_stale, MeetingLoopId, MeetingLoopKind};
use crate::meeting::people_types::{
    Person, PersonCommitment, PersonId, PersonMeetingHeadline, PersonMeetingSummary, PersonOpenLoop,
};
use crate::meeting::store::loops::{ledger_loop_seeds, ledger_rows_in, rows_from_seeds_in};
use crate::meeting::store::people::{all_people_in, loop_concerns_person, owner_matches};
use crate::meeting::store::{listed_outcomes, utc_now_ms, StoreError, READABLE_ARTIFACT_CONTENT};
use crate::meeting::types::{GeneratedMeetingArtifacts, MeetingSessionId};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use uuid::Uuid;

pub(super) struct PersonFacts {
    pub meetings: Vec<PersonMeetingSummary>,
    pub open_loops: Vec<PersonOpenLoop>,
    pub commitments: Vec<PersonCommitment>,
}

pub(super) fn meeting_summary_in(
    connection: &Connection,
    meeting_id: MeetingSessionId,
    person_id: PersonId,
) -> Result<PersonMeetingSummary, StoreError> {
    let row = connection
        .query_row(
            &format!(
                "SELECT m.title, COALESCE(m.started_at_utc_ms, m.created_at_utc_ms),
                        ({READABLE_ARTIFACT_CONTENT})
                   FROM meeting_sessions m WHERE m.id = ?1"
            ),
            [meeting_id.uuid().to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound)?;
    let series_number: i64 = connection.query_row(
        "SELECT COUNT(*)
           FROM meeting_person_links l
           JOIN meeting_sessions m ON m.id = l.meeting_id
          WHERE l.person_id = ?1 AND l.confidence = 'confirmed'
            AND COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) <= ?2",
        params![person_id.uuid().to_string(), row.1],
        |row| row.get(0),
    )?;
    let content = decode_artifacts(row.2.as_deref())?;
    Ok(PersonMeetingSummary {
        id: meeting_id,
        title: row.0,
        at_utc_ms: row.1,
        headline: content.as_ref().and_then(artifact_headline),
        series_number: u64::try_from(series_number).map_err(|_| StoreError::Corrupt)?,
    })
}

/// Everything one person's page is built from.
///
/// The loop lists carry both directions: what this person owes, and what the
/// user owes out of the meetings they were both in. Grouping the two is the
/// point — "what did I promise Steven" and "what is Steven still sitting on"
/// are the two halves of one relationship, and a page that showed only the
/// second would answer half the question.
pub(super) fn facts_for_person_in(
    connection: &Connection,
    person: &Person,
) -> Result<PersonFacts, StoreError> {
    let now_utc_ms = utc_now_ms();
    let mut statement = connection.prepare(&format!(
        "SELECT m.id, m.title, COALESCE(m.started_at_utc_ms, m.created_at_utc_ms),
                ({READABLE_ARTIFACT_CONTENT})
           FROM meeting_sessions m
           JOIN meeting_person_links l ON l.meeting_id = m.id
          WHERE l.person_id = ?1 AND l.confidence = 'confirmed'
          ORDER BY COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) DESC, m.id DESC"
    ))?;
    let rows = statement
        .query_map([person.id.uuid().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let total = u64::try_from(rows.len()).map_err(|_| StoreError::Corrupt)?;
    let mut meetings = Vec::with_capacity(rows.len());
    let mut open_loops = Vec::new();
    let mut commitments = Vec::new();
    for (index, (meeting_id, title, at_utc_ms, content_json)) in rows.into_iter().enumerate() {
        let meeting_id = parse_meeting_id(&meeting_id)?;
        let content = decode_artifacts(content_json.as_deref())?;
        let headline = content.as_ref().and_then(artifact_headline);
        let index = u64::try_from(index).map_err(|_| StoreError::Corrupt)?;
        meetings.push(PersonMeetingSummary {
            id: meeting_id,
            title: title.clone(),
            at_utc_ms,
            headline,
            series_number: total.saturating_sub(index),
        });
        let Some(ledger) = content.and_then(|content| content.ledger) else {
            continue;
        };
        let loop_rows = rows_from_seeds_in(
            connection,
            meeting_id,
            ledger_loop_seeds(meeting_id, &ledger),
        )?;
        for row in loop_rows {
            if !loop_concerns_person(person, &row) {
                continue;
            }
            let waiting_on_stale = waiting_on_is_stale(
                row.direction,
                row.status,
                row.outstanding_since(at_utc_ms),
                now_utc_ms,
            );
            match row.kind {
                MeetingLoopKind::Loop => {
                    if !row.is_open() {
                        continue;
                    }
                    open_loops.push(PersonOpenLoop {
                        loop_id: row.loop_id,
                        meeting_id,
                        title: title.clone(),
                        at_utc_ms,
                        text: row.text,
                        owner_person_id: row.owner_person_id,
                        status: row.status,
                        direction: row.direction,
                        waiting_on_stale,
                        carried_since_at_utc_ms: row.carried_since_at_utc_ms,
                        carried_into_meeting_id: row
                            .carried_into_loop_id
                            .as_ref()
                            .and_then(MeetingLoopId::session_id),
                    });
                }
                // A commitment stays on the page once it is kept: the point of
                // the list is what somebody undertook, not only what is left.
                MeetingLoopKind::Commitment => commitments.push(PersonCommitment {
                    loop_id: row.loop_id,
                    meeting_id,
                    title: title.clone(),
                    at_utc_ms,
                    text: row.text,
                    status: row.status,
                    direction: row.direction,
                    waiting_on_stale,
                    resolved_at_utc_ms: row.resolved_at_utc_ms,
                }),
            }
        }
    }
    Ok(PersonFacts {
        meetings,
        open_loops,
        commitments,
    })
}

/// Every loop still open, newest meeting first. The one query the open-loops
/// inbox, the pre-meeting brief and the consent panel's series line all read,
/// so "open" means the same thing on all three: a loop somebody resolved,
/// dropped or carried forward is gone from every one of them at once.
pub(super) fn all_open_loops_in(
    connection: &Connection,
) -> Result<Vec<PersonOpenLoop>, StoreError> {
    let people = all_people_in(connection)?;
    let now_utc_ms = utc_now_ms();
    let mut loops = Vec::new();
    for meeting in ledger_rows_in(connection)? {
        for row in meeting.rows {
            if row.kind != MeetingLoopKind::Loop || !row.is_open() {
                continue;
            }
            let owner_person_id = match row.owner_person_id {
                Some(owner) => Some(owner),
                None => row.owner_text.as_deref().and_then(|owner| {
                    people
                        .iter()
                        .find(|person| owner_matches(person, owner))
                        .map(|person| person.id)
                }),
            };
            let waiting_on_stale = waiting_on_is_stale(
                row.direction,
                row.status,
                row.outstanding_since(meeting.at_utc_ms),
                now_utc_ms,
            );
            loops.push(PersonOpenLoop {
                loop_id: row.loop_id,
                meeting_id: meeting.session_id,
                title: meeting.title.clone(),
                at_utc_ms: meeting.at_utc_ms,
                text: row.text,
                owner_person_id,
                status: row.status,
                direction: row.direction,
                waiting_on_stale,
                carried_since_at_utc_ms: row.carried_since_at_utc_ms,
                carried_into_meeting_id: None,
            });
        }
    }
    Ok(loops)
}

pub(super) fn talk_share_average_in(
    connection: &Connection,
    person: &Person,
    hidden: &HashSet<MeetingSessionId>,
) -> Result<Option<u32>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT c.metrics_json, s.speaker_id, s.display_name
           FROM meeting_person_links l
           JOIN meeting_conversation_metrics c ON c.session_id = l.meeting_id
           JOIN meeting_speakers s ON s.session_id = l.meeting_id
          WHERE l.person_id = ?1 AND l.confidence = 'confirmed'
            AND s.merged_into_speaker_id IS NULL
            AND l.meeting_id NOT IN (SELECT value FROM json_each(?2))",
    )?;
    let rows = statement
        .query_map(
            params![person.id.uuid().to_string(), super::hidden_json(hidden)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let mut shares = Vec::new();
    for (metrics_json, speaker_id, display_name) in rows {
        if !owner_matches(person, &display_name) {
            continue;
        }
        let metrics: MeetingAnalytics =
            serde_json::from_str(&metrics_json).map_err(|_| StoreError::Corrupt)?;
        let speaker_id = Uuid::parse_str(&speaker_id).map_err(|_| StoreError::Corrupt)?;
        if let Some(share) = metrics
            .talk
            .speakers
            .iter()
            .find(|share| share.speaker_id.uuid() == speaker_id)
        {
            shares.push(u64::from(share.share_permille));
        }
    }
    if shares.is_empty() {
        return Ok(None);
    }
    let total: u64 = shares.iter().sum();
    let count = u64::try_from(shares.len()).map_err(|_| StoreError::Corrupt)?;
    u32::try_from((total + count / 2) / count)
        .map(Some)
        .map_err(|_| StoreError::Corrupt)
}

/// The time recorded and what became of the threads across a person's
/// confirmed meetings outside `hidden`, by the same rules a meeting's list row
/// uses. Each is nil when none of those meetings has one.
pub(super) fn confirmed_totals_in(
    connection: &Connection,
    person_id: PersonId,
    hidden: &HashSet<MeetingSessionId>,
) -> Result<(Option<u64>, Option<LedgerOutcomeCounts>), StoreError> {
    let mut statement = connection.prepare(&format!(
        "SELECT (SELECT SUM(w.end_offset_ns - w.start_offset_ns)
                   FROM meeting_capture_windows w
                  WHERE w.session_id = m.id AND w.end_offset_ns IS NOT NULL),
                json_extract(({READABLE_ARTIFACT_CONTENT}), '$.ledger.threads')
           FROM meeting_person_links l
           JOIN meeting_sessions m ON m.id = l.meeting_id
          WHERE l.person_id = ?1 AND l.confidence = 'confirmed'
            AND l.meeting_id NOT IN (SELECT value FROM json_each(?2))"
    ))?;
    let rows = statement
        .query_map(
            params![person_id.uuid().to_string(), super::hidden_json(hidden)],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let mut recorded_ns: Option<u64> = None;
    let mut outcomes: Option<LedgerOutcomeCounts> = None;
    for (duration_ns, threads) in rows {
        if let Some(duration_ns) = duration_ns {
            let duration_ns = u64::try_from(duration_ns).map_err(|_| StoreError::Corrupt)?;
            *recorded_ns.get_or_insert(0) += duration_ns;
        }
        if let Some(counts) = listed_outcomes(threads.as_deref()) {
            *outcomes.get_or_insert_with(LedgerOutcomeCounts::default) += counts;
        }
    }
    Ok((recorded_ns.map(|ns| ns / 1_000_000), outcomes))
}

pub(super) fn document_summaries_in(
    connection: &Connection,
    person_id: PersonId,
) -> Result<Vec<crate::meeting::document_types::DocumentSummary>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT d.id, d.title, d.source_name, d.media_type, d.created_at_utc_ms
           FROM documents d
           JOIN document_person_links l ON l.document_id = d.id
          WHERE l.person_id = ?1
          ORDER BY d.created_at_utc_ms DESC, d.id DESC",
    )?;
    let summaries = statement
        .query_map([person_id.uuid().to_string()], |row| {
            let id: String = row.get(0)?;
            let id = Uuid::parse_str(&id).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(crate::meeting::document_types::DocumentSummary {
                id: crate::meeting::document_types::DocumentId(id),
                title: row.get(1)?,
                source_name: row.get(2)?,
                media_type: row.get(3)?,
                created_at_utc_ms: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(summaries)
}

fn decode_artifacts(
    content_json: Option<&str>,
) -> Result<Option<GeneratedMeetingArtifacts>, StoreError> {
    content_json
        .map(|content| serde_json::from_str(content).map_err(|_| StoreError::Corrupt))
        .transpose()
}

/// The same headline, plus where it came from — the people surfaces mark a
/// ledger reading differently from a summary sentence. The precedence itself
/// stays in `GeneratedMeetingArtifacts::headline`; this only reads the tag off
/// the answer.
pub(super) fn person_meeting_headline_from_json(
    content_json: Option<&str>,
) -> Result<Option<PersonMeetingHeadline>, StoreError> {
    let Some(content) = decode_artifacts(content_json)? else {
        return Ok(None);
    };
    let Some(headline) = content.headline() else {
        return Ok(None);
    };
    let from_ledger = content
        .ledger
        .as_ref()
        .is_some_and(|ledger| ledger.headline.trim() == headline);
    let text = headline.to_string();
    Ok(Some(if from_ledger {
        PersonMeetingHeadline::Ledger { text }
    } else {
        PersonMeetingHeadline::Summary { text }
    }))
}

fn artifact_headline(content: &GeneratedMeetingArtifacts) -> Option<String> {
    content.headline().map(str::to_string)
}

fn parse_meeting_id(value: &str) -> Result<MeetingSessionId, StoreError> {
    Uuid::parse_str(value)
        .map(MeetingSessionId::from_uuid)
        .map_err(|_| StoreError::Corrupt)
}
