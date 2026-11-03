use super::links::{describe, ScratchSource, ScratchSourceKind, ScratchSourceRef};
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::MeetingSessionId;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use specta::Type;
use std::sync::Arc;
use tauri::State;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Type)]
pub struct ScratchSourceEvidence {
    pub source: ScratchSource,
    pub text: String,
}

#[tauri::command]
#[specta::specta]
pub async fn scratchpad_source_evidence(
    history: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    id: String,
    source: ScratchSourceRef,
) -> Result<Option<ScratchSourceEvidence>, String> {
    let store = meetings
        .store()
        .await
        .map_err(|_| "Conversations are still unlocking.".to_string())?;
    describe(&history, &store, &source)?;
    if source.kind == ScratchSourceKind::Meeting {
        let linked = history
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM scratch_note_sources
                     WHERE note_id = ?1 AND kind = 'meeting' AND target_id = ?2)",
                    params![id, source.id],
                    |row| row.get::<_, bool>(0),
                )?)
            })
            .map_err(|_| "The captured source could not be read.".to_string())?;
        if !linked {
            return Ok(None);
        }
        let note_id = Uuid::parse_str(&id)
            .map_err(|_| "The captured source could not be read.".to_string())?;
        let session_id = Uuid::parse_str(&source.id)
            .map(MeetingSessionId::from_uuid)
            .map_err(|_| "This conversation is unavailable.".to_string())?;
        let captured = store
            .captured_note_evidence(note_id, session_id)
            .map_err(|_| "This conversation is unavailable.".to_string())?;
        return Ok(captured.map(|captured| {
            let source_id = captured.source.session_id.uuid().to_string();
            ScratchSourceEvidence {
                source: ScratchSource {
                    kind: ScratchSourceKind::Meeting,
                    link: format!("sona://meeting/{source_id}"),
                    id: source_id,
                    title: captured.source.title,
                    revision: i64::try_from(captured.source.input_revision).ok(),
                    available: true,
                },
                text: captured.text,
            }
        }));
    }
    history
        .with_connection(|conn| {
            let captured = conn
                .query_row(
                    "SELECT title, revision, evidence FROM scratch_note_sources
             WHERE note_id = ?1 AND kind = ?2 AND target_id = ?3 AND evidence IS NOT NULL",
                    params![id, source.kind.as_str(), source.id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            Ok(
                captured.map(|(title, revision, text)| ScratchSourceEvidence {
                    source: ScratchSource {
                        kind: source.kind,
                        id: source.id.clone(),
                        title,
                        link: format!("sona://{}/{}", source.kind.as_str(), source.id),
                        revision,
                        available: true,
                    },
                    text,
                }),
            )
        })
        .map_err(|_| "The captured source could not be read.".to_string())
}
