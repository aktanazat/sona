use super::store::Refusal;
use super::types::{
    EvidenceLink, EvidenceSourceKind, PlanningCalendarItem, PlanningCalendarKind, PlanningTask,
};
use crate::meeting::loop_types::MeetingLoopId;
use crate::meeting::store::{loops, memory_index};
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

pub(crate) fn person(conn: &Connection, id: &str) -> Result<Option<String>> {
    let source = memory_index::person_source_in(conn, id)
        .map_err(|_| anyhow::anyhow!("people could not be read"))?;
    Ok(source.map(|person| person.id))
}

pub(crate) fn require_person(conn: &Connection, id: &str) -> Result<String> {
    person(conn, id)?.ok_or_else(|| Refusal::OwnerUnavailable.into())
}

pub(crate) fn resolve_owner(conn: &Connection, task: &mut PlanningTask) -> Result<()> {
    if let Some(id) = &task.owner_person_id {
        let resolved = person(conn, id)?;
        task.owner_available = resolved.is_some();
        task.owner_person_id = resolved;
    } else {
        task.owner_available = true;
    }
    Ok(())
}

fn retained_passage(conn: &Connection, meeting_id: &str, at_ms: i64) -> Result<bool> {
    let start_ns = at_ms.checked_mul(1_000_000).ok_or(Refusal::Invalid)?;
    let end_ns = start_ns.checked_add(1_000_000).ok_or(Refusal::Invalid)?;
    if start_ns < 0 {
        return Err(Refusal::Invalid.into());
    }
    Ok(conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM meeting_sessions m
            JOIN meeting_transcript_segments s ON s.transcript_revision_id = m.current_transcript_revision_id
            WHERE m.id = ?1 AND m.phase != 'deleting' AND m.transcript_purged_at_utc_ms IS NULL
              AND s.start_offset_ns < ?3 AND s.end_offset_ns > ?2
              AND COALESCE((SELECT e.removed FROM meeting_segment_edits e
                    WHERE e.segment_id = s.segment_id ORDER BY e.edit_sequence DESC LIMIT 1), 0) = 0
        )",
        params![meeting_id, start_ns, end_ns],
        |row| row.get(0),
    )?)
}

fn meeting_title(conn: &Connection, id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT title FROM meeting_sessions WHERE id = ?1
            AND phase != 'deleting' AND transcript_purged_at_utc_ms IS NULL",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

fn source_title(
    history: &Connection,
    meetings: Option<&Connection>,
    kind: EvidenceSourceKind,
    id: &str,
    at_ms: Option<i64>,
    meeting_note_id: Option<&str>,
) -> Result<Option<String>> {
    match kind {
        EvidenceSourceKind::Note => {
            Uuid::parse_str(id).map_err(|_| Refusal::Invalid)?;
            if at_ms.is_some() {
                return Err(Refusal::Invalid.into());
            }
            Ok(history
                .query_row(
                    "SELECT title FROM scratch_notes WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?)
        }
        EvidenceSourceKind::Meeting => {
            let meetings = meetings.ok_or(Refusal::SourceUnavailable)?;
            Uuid::parse_str(id).map_err(|_| Refusal::Invalid)?;
            let title = meeting_title(meetings, id)?;
            if title.is_none() {
                return Ok(None);
            }
            if let Some(note_id) = meeting_note_id {
                Uuid::parse_str(note_id).map_err(|_| Refusal::Invalid)?;
                if at_ms.is_some() {
                    return Err(Refusal::Invalid.into());
                }
                let retained: bool = meetings.query_row(
                    "SELECT EXISTS(SELECT 1 FROM meeting_notes WHERE session_id = ?1 AND note_id = ?2)",
                    params![id, note_id],
                    |row| row.get(0),
                )?;
                if !retained {
                    return Ok(None);
                }
            }
            if let Some(offset) = at_ms {
                if !retained_passage(meetings, id, offset)? {
                    return Ok(None);
                }
            }
            Ok(title)
        }
        EvidenceSourceKind::Loop => {
            let meetings = meetings.ok_or(Refusal::SourceUnavailable)?;
            let loop_id = MeetingLoopId(id.to_string());
            let session_id = loop_id.session_id().ok_or(Refusal::Invalid)?;
            let meeting_id = session_id.uuid().to_string();
            if meeting_title(meetings, &meeting_id)?.is_none() {
                return Ok(None);
            }
            let seed = loops::seeds_in(meetings, session_id)
                .map_err(|_| anyhow::anyhow!("meeting evidence could not be read"))?
                .into_iter()
                .find(|seed| seed.loop_id == loop_id);
            let Some(seed) = seed else { return Ok(None) };
            if seed.citations.is_empty() {
                let Ok(offset) = i64::try_from(seed.at_ms) else {
                    return Ok(None);
                };
                if !retained_passage(meetings, &meeting_id, offset)? {
                    return Ok(None);
                }
            } else {
                let mut statement = meetings.prepare(
                    "SELECT EXISTS(
                        SELECT 1 FROM meeting_sessions m
                        JOIN meeting_transcript_segments s ON s.transcript_revision_id = m.current_transcript_revision_id
                        WHERE m.id = ?1 AND s.segment_id = ?2
                          AND COALESCE((SELECT e.removed FROM meeting_segment_edits e
                                WHERE e.segment_id = s.segment_id ORDER BY e.edit_sequence DESC LIMIT 1), 0) = 0
                    )",
                )?;
                for citation in &seed.citations {
                    let retained: bool = statement.query_row(
                        params![meeting_id, citation.segment_id.uuid().to_string()],
                        |row| row.get(0),
                    )?;
                    if !retained {
                        return Ok(None);
                    }
                }
            }
            if let Some(offset) = at_ms {
                if u64::try_from(offset).ok() != Some(seed.at_ms)
                    || !retained_passage(meetings, &meeting_id, offset)?
                {
                    return Ok(None);
                }
            }
            Ok(Some(seed.text))
        }
    }
}

pub(crate) fn require_source(
    history: &Connection,
    meetings: &Connection,
    kind: EvidenceSourceKind,
    id: &str,
    at_ms: Option<i64>,
) -> Result<()> {
    source_title(history, Some(meetings), kind, id, at_ms, None)?
        .map(|_| ())
        .ok_or_else(|| Refusal::SourceUnavailable.into())
}

/// The words of a meeting loop whose receipt is still retained.
pub(crate) fn require_loop(
    history: &Connection,
    meetings: &Connection,
    id: &str,
) -> Result<String> {
    source_title(
        history,
        Some(meetings),
        EvidenceSourceKind::Loop,
        id,
        None,
        None,
    )?
    .ok_or_else(|| Refusal::SourceUnavailable.into())
}

pub(crate) fn require_meeting_note(
    history: &Connection,
    meetings: &Connection,
    meeting_id: &str,
    note_id: &str,
) -> Result<()> {
    source_title(
        history,
        Some(meetings),
        EvidenceSourceKind::Meeting,
        meeting_id,
        None,
        Some(note_id),
    )?
    .map(|_| ())
    .ok_or_else(|| Refusal::SourceUnavailable.into())
}

pub(crate) fn resolve_link(
    history: &Connection,
    meetings: &Connection,
    link: &mut EvidenceLink,
) -> Result<()> {
    link.title = source_title(
        history,
        Some(meetings),
        link.source_kind,
        &link.source_id,
        link.at_ms,
        link.meeting_note_id.as_deref(),
    )?;
    link.available = link.title.is_some();
    Ok(())
}

pub(crate) fn require_note(history: &Connection, id: &str, at_ms: Option<i64>) -> Result<()> {
    source_title(history, None, EvidenceSourceKind::Note, id, at_ms, None)?
        .map(|_| ())
        .ok_or_else(|| Refusal::SourceUnavailable.into())
}

pub(crate) fn resolve_note(history: &Connection, link: &mut EvidenceLink) -> Result<()> {
    if link.source_kind != EvidenceSourceKind::Note {
        return Err(Refusal::Invalid.into());
    }
    link.title = source_title(
        history,
        None,
        link.source_kind,
        &link.source_id,
        link.at_ms,
        None,
    )?;
    link.available = link.title.is_some();
    Ok(())
}

pub(crate) fn calendar(
    conn: &Connection,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<PlanningCalendarItem>> {
    let mut statement = conn.prepare(
        "SELECT id, title, started_at_utc_ms FROM meeting_sessions
         WHERE phase != 'deleting' AND started_at_utc_ms >= ?1 AND started_at_utc_ms < ?2
         ORDER BY started_at_utc_ms, id",
    )?;
    let rows = statement.query_map(params![start_ms, end_ms], |row| {
        let id: String = row.get(0)?;
        Ok(PlanningCalendarItem {
            meeting_id: Some(id.clone()),
            id,
            kind: PlanningCalendarKind::Meeting,
            title: row.get(1)?,
            project_id: None,
            due_at_ms: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn canonical_source_id(kind: EvidenceSourceKind, id: &str) -> Result<String> {
    match kind {
        EvidenceSourceKind::Meeting | EvidenceSourceKind::Note => super::store::uuid(id),
        EvidenceSourceKind::Loop => {
            let loop_id = MeetingLoopId(id.to_string());
            let session_id = loop_id.session_id().ok_or(Refusal::Invalid)?;
            let (_, content) = id.split_once(':').ok_or(Refusal::Invalid)?;
            Ok(format!("{}:{content}", session_id.uuid()))
        }
    }
}
