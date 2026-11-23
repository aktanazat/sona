use crate::meeting::people_types::{PersonId, PersonLinkConfidence, PersonLinkSource};
use crate::meeting::store::StoreError;
use crate::meeting::types::MeetingSessionId;
use rusqlite::{params, Connection, OptionalExtension};

pub(in crate::meeting::store) fn upsert_link_in(
    connection: &Connection,
    meeting_id: MeetingSessionId,
    person_id: PersonId,
    source: PersonLinkSource,
    confidence: PersonLinkConfidence,
    now_utc_ms: i64,
) -> Result<bool, StoreError> {
    let current = connection
        .query_row(
            "SELECT source, confidence FROM meeting_person_links
              WHERE meeting_id = ?1 AND person_id = ?2",
            params![meeting_id.uuid().to_string(), person_id.uuid().to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let incoming = (confidence_rank(confidence), source_rank(source));
    if let Some((current_source, current_confidence)) = current {
        let current = (
            confidence_rank(confidence_from_db(&current_confidence)?),
            source_rank(source_from_db(&current_source)?),
        );
        if incoming <= current {
            return Ok(false);
        }
        connection.execute(
            "UPDATE meeting_person_links SET source = ?1, confidence = ?2
              WHERE meeting_id = ?3 AND person_id = ?4",
            params![
                source_to_db(source),
                confidence_to_db(confidence),
                meeting_id.uuid().to_string(),
                person_id.uuid().to_string()
            ],
        )?;
        return Ok(true);
    }
    connection.execute(
        "INSERT INTO meeting_person_links
            (meeting_id, person_id, source, confidence, created_at_utc_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            meeting_id.uuid().to_string(),
            person_id.uuid().to_string(),
            source_to_db(source),
            confidence_to_db(confidence),
            now_utc_ms
        ],
    )?;
    Ok(true)
}

pub(super) fn repoint_meeting_links_in(
    connection: &Connection,
    source_id: PersonId,
    target_id: PersonId,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    let mut statement = connection.prepare(
        "SELECT meeting_id, source, confidence FROM meeting_person_links WHERE person_id = ?1",
    )?;
    let links = statement
        .query_map([source_id.uuid().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (meeting_id, source, confidence) in links {
        let meeting_id = uuid::Uuid::parse_str(&meeting_id).map_err(|_| StoreError::Corrupt)?;
        upsert_link_in(
            connection,
            MeetingSessionId::from_uuid(meeting_id),
            target_id,
            source_from_db(&source)?,
            confidence_from_db(&confidence)?,
            now_utc_ms,
        )?;
    }
    Ok(())
}

pub(super) fn source_to_db(source: PersonLinkSource) -> &'static str {
    match source {
        PersonLinkSource::Calendar => "calendar",
        PersonLinkSource::Speaker => "speaker",
        PersonLinkSource::Title => "title",
        PersonLinkSource::Transcript => "transcript",
        PersonLinkSource::Manual => "manual",
    }
}

pub(super) fn source_from_db(value: &str) -> Result<PersonLinkSource, StoreError> {
    match value {
        "calendar" => Ok(PersonLinkSource::Calendar),
        "speaker" => Ok(PersonLinkSource::Speaker),
        "title" => Ok(PersonLinkSource::Title),
        "transcript" => Ok(PersonLinkSource::Transcript),
        "manual" => Ok(PersonLinkSource::Manual),
        _ => Err(StoreError::Corrupt),
    }
}

pub(super) fn confidence_to_db(confidence: PersonLinkConfidence) -> &'static str {
    match confidence {
        PersonLinkConfidence::Confirmed => "confirmed",
        PersonLinkConfidence::Suggested => "suggested",
    }
}

pub(super) fn confidence_from_db(value: &str) -> Result<PersonLinkConfidence, StoreError> {
    match value {
        "confirmed" => Ok(PersonLinkConfidence::Confirmed),
        "suggested" => Ok(PersonLinkConfidence::Suggested),
        _ => Err(StoreError::Corrupt),
    }
}

fn confidence_rank(confidence: PersonLinkConfidence) -> u8 {
    match confidence {
        PersonLinkConfidence::Suggested => 0,
        PersonLinkConfidence::Confirmed => 1,
    }
}

fn source_rank(source: PersonLinkSource) -> u8 {
    match source {
        PersonLinkSource::Title => 0,
        PersonLinkSource::Transcript => 1,
        PersonLinkSource::Speaker => 2,
        PersonLinkSource::Calendar => 3,
        PersonLinkSource::Manual => 4,
    }
}
