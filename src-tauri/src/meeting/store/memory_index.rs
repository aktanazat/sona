use super::{conversation_review, parse_uuid, MeetingStore, StoreError};
use crate::meeting::people_types::PersonPhotoProvenance;
use crate::meeting::types::MeetingSessionId;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

const MAX_PHOTO_BYTES: i64 = 1024 * 1024;

#[derive(Debug, Serialize)]
pub(crate) struct MemoryPerson {
    pub id: String,
    pub display_name: String,
    pub photo_png_base64: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MemoryAttribution {
    pub meeting_id: String,
    pub speaker_id: String,
    pub person_id: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct MemoryBookmark {
    pub meeting_id: String,
    pub start_offset_ns: u64,
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MemoryIndexRecords {
    pub people: Vec<MemoryPerson>,
    pub attributions: Vec<MemoryAttribution>,
    pub bookmarks: Vec<MemoryBookmark>,
}

pub(crate) struct MemorySourceDescription {
    pub id: String,
    pub title: String,
}

impl MeetingStore {
    /// Holds the meeting snapshot before the caller takes the history lock.
    pub(crate) fn with_memory_index_snapshot<T>(
        &self,
        read: impl FnOnce(MemoryIndexRecords, &Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let records = MemoryIndexRecords {
            people: people_in(&transaction)?,
            attributions: attributions_in(&transaction)?,
            bookmarks: bookmarks_in(&transaction)?,
        };
        let result = read(records, &transaction)?;
        transaction.commit()?;
        Ok(result)
    }
}

fn people_in(connection: &Connection) -> Result<Vec<MemoryPerson>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT person.id, person.display_name,
                CASE WHEN length(avatar.png) <= ?1 AND avatar.provenance_json IS NOT NULL
                     THEN avatar.png END,
                CASE WHEN length(avatar.png) <= ?1 THEN avatar.provenance_json END
           FROM persons person
           LEFT JOIN person_avatars avatar ON avatar.person_id = person.id
          ORDER BY person.id",
    )?;
    let mut rows = statement.query([MAX_PHOTO_BYTES])?;
    let mut people = Vec::new();
    while let Some(row) = rows.next()? {
        let png: Option<Vec<u8>> = row.get(2)?;
        let provenance: Option<String> = row.get(3)?;
        let photo_png_base64 = match (png, provenance) {
            (Some(png), Some(provenance)) => {
                serde_json::from_str::<PersonPhotoProvenance>(&provenance)
                    .map_err(|_| StoreError::Corrupt)?;
                Some(STANDARD.encode(png))
            }
            _ => None,
        };
        people.push(MemoryPerson {
            id: row.get(0)?,
            display_name: row.get(1)?,
            photo_png_base64,
        });
    }
    Ok(people)
}

fn attributions_in(connection: &Connection) -> Result<Vec<MemoryAttribution>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT session.id, speaker.speaker_id, person.id
           FROM meeting_voice_speaker_resolutions resolution
           JOIN meeting_speakers speaker ON speaker.speaker_id = resolution.speaker_id
           JOIN meeting_sessions session ON session.id = speaker.session_id
           JOIN persons person ON person.id = resolution.resolved_person_id
           JOIN meeting_person_links link ON link.meeting_id = session.id AND link.person_id = person.id
          WHERE resolution.is_current = 1 AND resolution.resolution_kind = 'identified'
            AND speaker.merged_into_speaker_id IS NULL
            AND session.phase != 'deleting' AND session.transcript_purged_at_utc_ms IS NULL
          ORDER BY session.id, speaker.speaker_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(MemoryAttribution {
            meeting_id: row.get(0)?,
            speaker_id: row.get(1)?,
            person_id: row.get(2)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn bookmarks_in(connection: &Connection) -> Result<Vec<MemoryBookmark>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT session.id FROM conversation_bookmarks bookmark
           JOIN meeting_sessions session ON session.id = bookmark.session_id
          WHERE session.phase != 'deleting' ORDER BY session.id",
    )?;
    let mut rows = statement.query([])?;
    let mut bookmarks = Vec::new();
    while let Some(row) = rows.next()? {
        let meeting_id: String = row.get(0)?;
        let session_id = MeetingSessionId::from_uuid(parse_uuid(&meeting_id)?);
        for bookmark in conversation_review::export_bookmarks_in(connection, session_id)? {
            bookmarks.push(MemoryBookmark {
                meeting_id: bookmark.session_id.uuid().to_string(),
                start_offset_ns: bookmark.start_offset_ns,
                label: bookmark.label,
            });
        }
    }
    Ok(bookmarks)
}

pub(crate) fn meeting_source_in(
    connection: &Connection,
    note_id: &str,
    meeting_id: &str,
) -> Result<Option<MemorySourceDescription>, StoreError> {
    connection
        .query_row(
            "SELECT session.id, COALESCE(evidence.title, session.title)
               FROM meeting_sessions session
               LEFT JOIN meeting_note_evidence evidence
                 ON evidence.session_id = session.id AND evidence.note_id = ?1
              WHERE session.id = ?2 AND session.phase != 'deleting'
                AND session.transcript_purged_at_utc_ms IS NULL",
            params![note_id, meeting_id],
            |row| {
                Ok(MemorySourceDescription {
                    id: row.get(0)?,
                    title: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn person_source_in(
    connection: &Connection,
    person_id: &str,
) -> Result<Option<MemorySourceDescription>, StoreError> {
    connection
        .query_row(
            "SELECT id, display_name FROM persons
              WHERE id = COALESCE((SELECT target_person_id FROM person_merge_redirects
                                    WHERE source_person_id = ?1), ?1)",
            [person_id],
            |row| {
                Ok(MemorySourceDescription {
                    id: row.get(0)?,
                    title: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}
