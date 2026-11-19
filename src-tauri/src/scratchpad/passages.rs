use super::links::{
    describe, insert_source, ResolvedSource, ScratchSource, ScratchSourceKind, ScratchSourceRef,
};
use super::{emit_changed, failure_sentence, now_ms, run, ScratchNote};
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::note_evidence::{NoteEvidenceError, NoteEvidenceInput};
use crate::meeting::store::MeetingStore;
use crate::meeting::types::{MeetingReviewSnapshot, MeetingSessionId};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, State};
use uuid::Uuid;

pub const MIGRATION: &str = "CREATE TABLE scratch_note_passages (
    note_id TEXT PRIMARY KEY NOT NULL REFERENCES scratch_notes(id),
    meeting_id TEXT NOT NULL,
    segment_id TEXT NOT NULL,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    person_id TEXT,
    wording TEXT NOT NULL CHECK(wording IN ('raw', 'corrected'))
);
CREATE TRIGGER scratch_note_passages_delete AFTER DELETE ON scratch_notes BEGIN
    DELETE FROM scratch_note_passages WHERE note_id = old.id;
END;
CREATE TRIGGER scratch_note_passages_unlink AFTER DELETE ON scratch_note_sources
WHEN old.kind = 'meeting' BEGIN
    DELETE FROM scratch_note_passages WHERE note_id = old.note_id AND meeting_id = old.target_id;
END;";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ScratchPassageWording {
    Raw,
    Corrected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ScratchPassage {
    pub meeting_id: String,
    pub segment_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub person_id: Option<String>,
    pub wording: ScratchPassageWording,
    pub link: String,
}

pub(super) fn read(conn: &Connection, note_id: &str) -> anyhow::Result<Vec<ScratchPassage>> {
    let mut statement = conn.prepare("SELECT meeting_id, segment_id, start_ms, end_ms, person_id, wording FROM scratch_note_passages WHERE note_id = ?1")?;
    let rows = statement.query_map(params![note_id], |row| {
        let meeting_id: String = row.get(0)?;
        let segment_id: String = row.get(1)?;
        let start_ms: i64 = row.get(2)?;
        Ok(ScratchPassage {
            link: format!("sona://meeting/{meeting_id}?atMs={start_ms}&segmentId={segment_id}"),
            meeting_id,
            segment_id,
            start_ms,
            end_ms: row.get(3)?,
            person_id: row.get(4)?,
            wording: if row.get::<_, String>(5)? == "raw" {
                ScratchPassageWording::Raw
            } else {
                ScratchPassageWording::Corrected
            },
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[tauri::command]
#[specta::specta]
pub async fn scratchpad_capture_passage(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    meeting_id: String,
    segment_id: String,
    excerpt: String,
    person_id: Option<String>,
    body: String,
) -> Result<ScratchNote, String> {
    let session_id = Uuid::parse_str(&meeting_id)
        .map(MeetingSessionId::from_uuid)
        .map_err(|_| "This conversation is unavailable.".to_string())?;
    let store = meetings
        .store()
        .await
        .map_err(|_| "Conversations are still unlocking.".to_string())?;
    let snapshot = store
        .review_snapshot(session_id)
        .map_err(|_| "This conversation is unavailable.".to_string())?;
    passage(&history, store, snapshot, segment_id, &excerpt, person_id, body)?
        .save(&app, &history, super::Author::Me)
}

/// A passage of a meeting's transcript, checked and worded as the note that
/// keeps it. Finding one writes nothing.
pub(crate) struct Passage {
    store: Arc<MeetingStore>,
    session_id: MeetingSessionId,
    revision: u64,
    title: String,
    meeting_id: String,
    segment_id: String,
    meeting: ScratchSource,
    person: Option<ScratchSource>,
    person_id: Option<String>,
    start_ms: i64,
    end_ms: i64,
    wording: &'static str,
    captured: String,
    body: String,
}

/// Finds `excerpt` in the line `segment_id` of the meeting `snapshot` shows,
/// raw or corrected. A blank `body` becomes a line naming where it is.
pub(crate) fn passage(
    history: &HistoryManager,
    store: Arc<MeetingStore>,
    snapshot: MeetingReviewSnapshot,
    segment_id: String,
    excerpt: &str,
    person_id: Option<String>,
    body: String,
) -> Result<Passage, String> {
    if excerpt.trim().is_empty() {
        return Err("Choose a passage.".to_string());
    }
    let session_id = snapshot.session.session_id;
    let meeting_id = session_id.uuid().to_string();
    let meeting = describe(
        history,
        &store,
        &ScratchSourceRef {
            kind: ScratchSourceKind::Meeting,
            id: meeting_id.clone(),
        },
    )?;
    let segment = snapshot
        .transcript
        .iter()
        .find(|segment| {
            segment.base.segment_id.uuid().to_string() == segment_id && !segment.removed
        })
        .ok_or_else(|| "This passage is unavailable.".to_string())?;
    let corrected = segment
        .replacement_text
        .as_deref()
        .unwrap_or(&segment.base.text);
    if !corrected.contains(excerpt) && !segment.base.text.contains(excerpt) {
        return Err("The passage changed. Select it again.".to_string());
    }
    let person = person_id
        .as_ref()
        .map(|id| {
            describe(
                history,
                &store,
                &ScratchSourceRef {
                    kind: ScratchSourceKind::Person,
                    id: id.clone(),
                },
            )
        })
        .transpose()?;
    let start_ms = i64::try_from(segment.base.start_offset_ns / 1_000_000)
        .map_err(|_| "This passage is unavailable.".to_string())?;
    let end_ms = i64::try_from(segment.base.end_offset_ns / 1_000_000)
        .map_err(|_| "This passage is unavailable.".to_string())?;
    let body = if body.trim().is_empty() {
        format!("{}\nsona://meeting/{meeting_id}?atMs={start_ms}&segmentId={segment_id}", excerpt.trim())
    } else {
        body
    };
    let (wording, label) = if segment.base.text.contains(excerpt) {
        ("raw", "Direct quote from raw transcript")
    } else {
        ("corrected", "Selected corrected wording")
    };
    let captured = format!(
        "{label}\n{excerpt}\n\nRaw transcript\n{}\n\nCorrected transcript\n{corrected}",
        segment.base.text
    );
    Ok(Passage {
        store,
        session_id,
        revision: snapshot.session.revision,
        title: snapshot.session.title,
        meeting_id,
        segment_id,
        meeting,
        person,
        person_id,
        start_ms,
        end_ms,
        wording,
        captured,
        body,
    })
}

impl Passage {
    /// The note's text as it will be saved.
    pub(crate) fn body(&self) -> &str {
        &self.body
    }

    /// Saves the passage as a new note linked to its meeting, and to the
    /// person when one was named.
    pub(crate) fn save(
        self,
        app: &AppHandle,
        history: &HistoryManager,
        author: super::Author<'_>,
    ) -> Result<ScratchNote, String> {
        let capture = NoteEvidenceInput {
            session_id: self.session_id,
            input_revision: self.revision,
            title: &self.title,
            text: &self.captured,
        };
        let note_id = Uuid::new_v4();
        let note = self.store.capture_note_evidence(note_id, &[capture], || {
            run(history, |conn| {
                let transaction = conn.transaction()?;
                let now = now_ms();
                let note = ScratchNote { id: note_id.to_string(), title: super::derive_title(&self.body), body: self.body, pinned: false, created_at_ms: now, updated_at_ms: now };
                transaction.execute("INSERT INTO scratch_notes (id, title, body, pinned, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, 0, ?4, ?4)", params![note.id, note.title, note.body, now])?;
                super::record_version(&transaction, &note.id, &note.body, now, super::ScratchVersionKind::Edit, author)?;
                insert_source(&transaction, &note.id, &ResolvedSource { source: self.meeting, evidence: None })?;
                if let Some(source) = self.person { insert_source(&transaction, &note.id, &ResolvedSource { source, evidence: None })?; }
                transaction.execute("INSERT INTO scratch_note_passages (note_id, meeting_id, segment_id, start_ms, end_ms, person_id, wording) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![note.id, self.meeting_id, self.segment_id, self.start_ms, self.end_ms, self.person_id, self.wording])?;
                transaction.commit()?;
                Ok(note)
            })
        }).map_err(|error| match error {
            NoteEvidenceError::Persist(error) => failure_sentence(history, error, "The passage could not be saved."),
            NoteEvidenceError::Store(_) => "The passage changed or was deleted. Select it again.".to_string(),
        })?;
        emit_changed(app, &note.id, false);
        Ok(note)
    }
}
