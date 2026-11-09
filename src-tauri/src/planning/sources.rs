use super::store::Refusal;
use super::types::{
    EvidenceLink, EvidenceSourceKind, PlanningCalendarItem, PlanningCalendarKind, PlanningTask,
};
use crate::meeting::loop_types::MeetingLoopId;
use crate::meeting::store::{loops, memory_index};
use crate::meeting::types::ArtifactCitation;
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

pub(crate) fn retained_passage(conn: &Connection, meeting_id: &str, at_ms: i64) -> Result<bool> {
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

/// Whether a meeting's transcript is still kept, so its words may be shown.
pub(crate) fn meeting_retained(conn: &Connection, id: &str) -> Result<bool> {
    Ok(meeting_title(conn, id)?.is_some())
}

/// Whether the words a loop cites are still in its meeting's transcript;
/// a loop citing nothing is checked at the moment it was said.
pub(crate) fn words_retained(
    conn: &Connection,
    meeting_id: &str,
    citations: &[ArtifactCitation],
    at_ms: u64,
) -> Result<bool> {
    if citations.is_empty() {
        return match i64::try_from(at_ms) {
            Ok(offset) => retained_passage(conn, meeting_id, offset),
            Err(_) => Ok(false),
        };
    }
    let mut statement = conn.prepare_cached(
        "SELECT EXISTS(
            SELECT 1 FROM meeting_sessions m
            JOIN meeting_transcript_segments s ON s.transcript_revision_id = m.current_transcript_revision_id
            WHERE m.id = ?1 AND s.segment_id = ?2
              AND COALESCE((SELECT e.removed FROM meeting_segment_edits e
                    WHERE e.segment_id = s.segment_id ORDER BY e.edit_sequence DESC LIMIT 1), 0) = 0
        )",
    )?;
    for citation in citations {
        let retained: bool = statement.query_row(
            params![meeting_id, citation.segment_id.uuid().to_string()],
            |row| row.get(0),
        )?;
        if !retained {
            return Ok(false);
        }
    }
    Ok(true)
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
            if !words_retained(meetings, &meeting_id, &seed.citations, seed.at_ms)? {
                return Ok(None);
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
        EvidenceSourceKind::Dictation => {
            let history_id = dictation_id(id)?;
            if meeting_note_id.is_some() {
                return Err(Refusal::Invalid.into());
            }
            Ok(history
                .query_row(
                    "SELECT title FROM transcription_history WHERE id = ?1",
                    [history_id],
                    |row| row.get(0),
                )
                .optional()?)
        }
        EvidenceSourceKind::Link => {
            if at_ms.is_some() || meeting_note_id.is_some() {
                return Err(Refusal::Invalid.into());
            }
            Ok(Some(link_url(id)?))
        }
    }
}

fn dictation_id(id: &str) -> Result<i64> {
    id.parse::<i64>()
        .ok()
        .filter(|history_id| *history_id > 0)
        .ok_or_else(|| Refusal::Invalid.into())
}

/// An address a link may hold: a web page or a file, absolute.
pub(crate) fn link_url(id: &str) -> Result<String> {
    let url = url::Url::parse(id.trim()).map_err(|_| Refusal::Invalid)?;
    if !matches!(url.scheme(), "http" | "https" | "file") {
        return Err(Refusal::Invalid.into());
    }
    Ok(url.into())
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

/// The words of a meeting loop, or none once its receipt is gone.
pub(crate) fn loop_words(
    history: &Connection,
    meetings: &Connection,
    id: &str,
) -> Result<Option<String>> {
    source_title(
        history,
        Some(meetings),
        EvidenceSourceKind::Loop,
        id,
        None,
        None,
    )
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
    let title = source_title(
        history,
        Some(meetings),
        link.source_kind,
        &link.source_id,
        link.at_ms,
        link.meeting_note_id.as_deref(),
    )?;
    resolved(link, title);
    Ok(())
}

/// What a link shows once its source is read: the words it kept go with a
/// source that is gone.
fn resolved(link: &mut EvidenceLink, title: Option<String>) {
    link.available = title.is_some();
    if !link.available {
        link.caption = None;
    }
    link.title = title;
}

/// Whether evidence of this kind is read from the conversations, which
/// may be locked, rather than from the history database beside the
/// tracker.
pub(crate) const fn in_conversations(kind: EvidenceSourceKind) -> bool {
    match kind {
        EvidenceSourceKind::Meeting | EvidenceSourceKind::Loop => true,
        EvidenceSourceKind::Note | EvidenceSourceKind::Dictation | EvidenceSourceKind::Link => {
            false
        }
    }
}

pub(crate) fn require_local(
    history: &Connection,
    kind: EvidenceSourceKind,
    id: &str,
    at_ms: Option<i64>,
) -> Result<()> {
    if in_conversations(kind) {
        return Err(Refusal::Invalid.into());
    }
    source_title(history, None, kind, id, at_ms, None)?
        .map(|_| ())
        .ok_or_else(|| Refusal::SourceUnavailable.into())
}

/// Evidence read without the conversations: a note, a dictation or a
/// link.
pub(crate) fn resolve_local(history: &Connection, link: &mut EvidenceLink) -> Result<()> {
    if in_conversations(link.source_kind) {
        return Err(Refusal::Invalid.into());
    }
    let title = source_title(
        history,
        None,
        link.source_kind,
        &link.source_id,
        link.at_ms,
        link.meeting_note_id.as_deref(),
    )?;
    resolved(link, title);
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
        EvidenceSourceKind::Dictation => Ok(dictation_id(id)?.to_string()),
        EvidenceSourceKind::Link => link_url(id),
    }
}
