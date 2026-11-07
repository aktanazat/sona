use super::said::SessionVoices;
use super::{effective_segments_for_session, id, MeetingStore, StoreError};
use crate::meeting::document_types::{
    Document, DocumentDifference, DocumentId, DocumentListResult, DocumentMutationResult,
    DocumentSummary, MeetingDocument,
};
use crate::meeting::people_types::PersonId;
use crate::meeting::said_and_written;
use crate::meeting::types::{ArtifactCitation, MeetingOperationId, MeetingSessionId};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use uuid::Uuid;

const SCHEMA_VERSION: u32 = 1;
const PDF_MEDIA_TYPE: &str = "application/pdf";
/// Between a PDF's pages in its stored text; page text never holds one.
const PAGE_BREAK: char = '\u{c}';

pub(crate) enum DocumentBody {
    Text {
        markdown: bool,
        content: String,
    },
    Pdf {
        pages: Vec<String>,
        original: Vec<u8>,
    },
}

impl MeetingStore {
    pub(crate) fn documents_list(
        &self,
        person_id: Option<PersonId>,
    ) -> Result<DocumentListResult, StoreError> {
        let connection = self.connection()?;
        let revision = document_revision_in(&connection)?;
        let sql = if person_id.is_some() {
            "SELECT d.id, d.title, d.source_name, d.media_type, d.content,
                    d.created_at_utc_ms
               FROM documents d
               JOIN document_person_links l ON l.document_id = d.id
              WHERE l.person_id = ?1
              ORDER BY d.created_at_utc_ms DESC, d.id DESC"
        } else {
            "SELECT d.id, d.title, d.source_name, d.media_type, d.content,
                    d.created_at_utc_ms
               FROM documents d
              ORDER BY d.created_at_utc_ms DESC, d.id DESC"
        };
        let mut statement = connection.prepare(sql)?;
        let rows = if let Some(person_id) = person_id {
            statement.query_map([person_id.uuid().to_string()], document_from_row)?
        } else {
            statement.query_map([], document_from_row)?
        };
        let entries = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(DocumentListResult {
            schema_version: SCHEMA_VERSION,
            revision,
            entries,
        })
    }

    pub(crate) fn ingest_document(
        &self,
        operation_id: MeetingOperationId,
        title: String,
        source_name: String,
        body: DocumentBody,
        session_id: Option<MeetingSessionId>,
        now_utc_ms: i64,
    ) -> Result<DocumentMutationResult, StoreError> {
        let title = title.trim().to_string();
        let source_name = source_name.trim().to_string();
        if title.is_empty() || source_name.is_empty() {
            return Err(StoreError::Invalid);
        }
        let (media_type, content, original) = match body {
            DocumentBody::Text { markdown, content } => (
                if markdown {
                    "text/markdown"
                } else {
                    "text/plain"
                },
                content,
                None,
            ),
            DocumentBody::Pdf { pages, original } => (
                PDF_MEDIA_TYPE,
                pages.join(&PAGE_BREAK.to_string()),
                Some(original),
            ),
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let operation_id = operation_id.uuid().to_string();
        let existing: Option<String> = transaction
            .query_row(
                "SELECT result_json FROM document_ingest_operations WHERE operation_id = ?1",
                [&operation_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            let result = serde_json::from_str(&existing).map_err(|_| StoreError::Corrupt)?;
            transaction.commit()?;
            return Ok(result);
        }
        if let Some(session_id) = session_id {
            transaction
                .query_row(
                    "SELECT 1 FROM meeting_sessions WHERE id = ?1 AND phase != 'deleting'",
                    params![id(session_id)],
                    |_| Ok(()),
                )
                .optional()?
                .ok_or(StoreError::NotFound)?;
        }

        let document_id = DocumentId::new();
        transaction.execute(
            "INSERT INTO documents (
                id, title, source_name, media_type, content,
                created_at_utc_ms, updated_at_utc_ms, session_id, original
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8)",
            params![
                document_id.uuid().to_string(),
                title,
                source_name,
                media_type,
                content,
                now_utc_ms,
                session_id.map(id),
                original
            ],
        )?;
        let revision = bump_document_revision_in(&transaction)?;
        let document = document_by_id_in(&transaction, document_id)?;
        let result = DocumentMutationResult {
            schema_version: SCHEMA_VERSION,
            revision,
            document: Some(document),
            removed: false,
        };
        let result_json = serde_json::to_string(&result).map_err(|_| StoreError::Corrupt)?;
        transaction.execute(
            "INSERT INTO document_ingest_operations (
                operation_id, result_json, created_at_utc_ms
             ) VALUES (?1, ?2, ?3)",
            params![operation_id, result_json, now_utc_ms],
        )?;
        transaction.commit()?;
        Ok(result)
    }

    /// The documents added to a meeting, oldest first, each with what was
    /// said in the meeting that differs from it.
    pub(crate) fn meeting_documents(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<MeetingDocument>, StoreError> {
        let user_name = self.user_name();
        let connection = self.connection()?;
        let transcript_kept: bool = connection
            .query_row(
                "SELECT transcript_purged_at_utc_ms IS NULL FROM meeting_sessions
                  WHERE id = ?1 AND phase != 'deleting'",
                params![id(session_id)],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        let mut statement = connection.prepare(
            "SELECT id, title, source_name, media_type, content, created_at_utc_ms
               FROM documents WHERE session_id = ?1
              ORDER BY created_at_utc_ms, id",
        )?;
        let documents = statement
            .query_map(params![id(session_id)], document_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let segments = if transcript_kept {
            effective_segments_for_session(&connection, session_id)?
        } else {
            Vec::new()
        };
        let segments: Vec<_> = segments.iter().filter(|segment| !segment.removed).collect();
        let lines: Vec<&str> = segments
            .iter()
            .map(|segment| {
                segment
                    .replacement_text
                    .as_deref()
                    .unwrap_or(&segment.base.text)
            })
            .collect();
        let mut voices = SessionVoices::read_in(&connection, session_id, user_name.as_deref())?;
        let mut meeting_documents = Vec::with_capacity(documents.len());
        for document in documents {
            let pdf = document.summary.media_type == PDF_MEDIA_TYPE;
            let pages: Vec<&str> = if pdf {
                document.content.split(PAGE_BREAK).collect()
            } else {
                vec![document.content.as_str()]
            };
            let differences = if transcript_kept {
                let mut differences = Vec::new();
                for difference in said_and_written::differences(&pages, &lines) {
                    let segment = segments[difference.line];
                    let voice = voices.voice_in(
                        &connection,
                        session_id,
                        segment.assigned_speaker_id,
                        segment.speaker_assignment,
                    )?;
                    let page = pages[difference.page];
                    let line = lines[difference.line];
                    differences.push(DocumentDifference {
                        page: if pdf {
                            Some(
                                u32::try_from(difference.page + 1)
                                    .map_err(|_| StoreError::Corrupt)?,
                            )
                        } else {
                            None
                        },
                        passage: page[difference.passage].to_string(),
                        written: page[difference.written].to_string(),
                        said: line[difference.said].to_string(),
                        quote: line[difference.quote].to_string(),
                        speaker: voice
                            .person
                            .map(|person| person.display_name)
                            .or(voice.label),
                        citation: ArtifactCitation {
                            segment_id: segment.base.segment_id,
                            start_offset_ns: segment.base.start_offset_ns,
                            end_offset_ns: segment.base.end_offset_ns,
                        },
                    });
                }
                Some(differences)
            } else {
                None
            };
            meeting_documents.push(MeetingDocument {
                document: document.summary,
                differences,
            });
        }
        Ok(meeting_documents)
    }

    /// The PDF a meeting's document was added from, byte for byte.
    pub(crate) fn meeting_document_original(
        &self,
        session_id: MeetingSessionId,
        document_id: DocumentId,
    ) -> Result<Vec<u8>, StoreError> {
        self.connection()?
            .query_row(
                "SELECT original FROM documents
                  WHERE id = ?1 AND session_id = ?2 AND original IS NOT NULL",
                params![document_id.uuid().to_string(), id(session_id)],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    pub(crate) fn delete_document(
        &self,
        document_id: DocumentId,
        expected_revision: u64,
    ) -> Result<DocumentMutationResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_document_revision_in(&transaction, expected_revision)?;
        let removed = transaction.execute(
            "DELETE FROM documents WHERE id = ?1",
            [document_id.uuid().to_string()],
        )? != 0;
        if !removed {
            return Err(StoreError::NotFound);
        }
        let revision = bump_document_revision_in(&transaction)?;
        transaction.commit()?;
        Ok(DocumentMutationResult {
            schema_version: SCHEMA_VERSION,
            revision,
            document: None,
            removed: true,
        })
    }
}

pub(super) fn document_by_id_in(
    connection: &Connection,
    document_id: DocumentId,
) -> Result<Document, StoreError> {
    connection
        .query_row(
            "SELECT id, title, source_name, media_type, content, created_at_utc_ms
               FROM documents WHERE id = ?1",
            [document_id.uuid().to_string()],
            document_from_row,
        )
        .optional()?
        .ok_or(StoreError::NotFound)
}

pub(super) fn bump_document_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    connection.execute(
        "UPDATE document_state SET revision = revision + 1 WHERE singleton = 1",
        [],
    )?;
    document_revision_in(connection)
}

fn document_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    let revision: i64 = connection.query_row(
        "SELECT revision FROM document_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(revision).map_err(|_| StoreError::Corrupt)
}

fn require_document_revision_in(
    connection: &Connection,
    expected_revision: u64,
) -> Result<(), StoreError> {
    if document_revision_in(connection)? != expected_revision {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn document_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Document> {
    let id: String = row.get(0)?;
    let id = Uuid::parse_str(&id).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Document {
        summary: DocumentSummary {
            id: DocumentId(id),
            title: row.get(1)?,
            source_name: row.get(2)?,
            media_type: row.get(3)?,
            created_at_utc_ms: row.get(5)?,
        },
        content: row.get(4)?,
    })
}
