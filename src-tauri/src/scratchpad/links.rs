use super::{
    emit_changed, failure_sentence, note_with_connection, run, ScratchNote, ScratchpadRefusal,
};
use crate::managers::history::HistoryManager;
use crate::meeting::people_types::PersonId;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::note_evidence::{NoteEvidenceError, NoteEvidenceRemoval};
use crate::meeting::store::MeetingStore;
use crate::meeting::types::{MeetingPhase, MeetingSessionId, MeetingSessionSnapshot};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;
use std::sync::Arc;
use tauri::{AppHandle, State};
use uuid::Uuid;

pub const MIGRATION: &str = "CREATE TABLE scratch_note_sources (
    note_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('note', 'meeting', 'person')),
    target_id TEXT NOT NULL,
    title TEXT NOT NULL,
    revision INTEGER,
    evidence TEXT,
    PRIMARY KEY(note_id, kind, target_id),
    FOREIGN KEY(note_id) REFERENCES scratch_notes(id)
);
CREATE INDEX scratch_note_sources_target ON scratch_note_sources(kind, target_id);
CREATE TRIGGER scratch_note_sources_note_delete AFTER DELETE ON scratch_notes BEGIN
    DELETE FROM scratch_note_sources WHERE note_id = old.id;
    UPDATE scratch_note_sources SET title = 'Unavailable source', evidence = NULL
        WHERE kind = 'note' AND target_id = old.id;
END;";

pub const MEETING_REFERENCES_MIGRATION: &str = "UPDATE scratch_note_sources
    SET title = 'Conversation', revision = NULL, evidence = NULL WHERE kind = 'meeting';
CREATE TRIGGER scratch_note_sources_meeting_reference_insert
BEFORE INSERT ON scratch_note_sources
WHEN NEW.kind = 'meeting'
    AND (NEW.title IS NOT 'Conversation' OR NEW.revision IS NOT NULL OR NEW.evidence IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'conversation sources must be references'); END;
CREATE TRIGGER scratch_note_sources_meeting_reference_update
BEFORE UPDATE ON scratch_note_sources
WHEN NEW.kind = 'meeting'
    AND (NEW.title IS NOT 'Conversation' OR NEW.revision IS NOT NULL OR NEW.evidence IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'conversation sources must be references'); END;";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ScratchSourceKind {
    Note,
    Meeting,
    Person,
}

impl ScratchSourceKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Meeting => "meeting",
            Self::Person => "person",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
pub struct ScratchSourceRef {
    pub kind: ScratchSourceKind,
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ScratchSource {
    pub kind: ScratchSourceKind,
    pub id: String,
    pub title: String,
    pub link: String,
    pub revision: Option<i64>,
    pub available: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ScratchNoteMemory {
    pub note: ScratchNote,
    pub sources: Vec<ScratchSource>,
    pub phone_sources: Vec<super::phone_import::PhoneThoughtReference>,
    pub related_notes: Vec<ScratchNote>,
    pub passages: Vec<super::passages::ScratchPassage>,
    pub interpretation: bool,
    pub connections: Vec<ScratchConnection>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ScratchConnection {
    pub note_id: String,
    pub reasons: Vec<String>,
}

pub(crate) struct ResolvedSource {
    pub source: ScratchSource,
    pub evidence: Option<String>,
}

pub(crate) fn describe(
    history: &HistoryManager,
    store: &MeetingStore,
    target: &ScratchSourceRef,
) -> Result<ScratchSource, String> {
    let id = Uuid::parse_str(&target.id).map_err(|_| "Choose a saved source.".to_string())?;
    let mut resolved_person = None;
    let (title, revision) = match target.kind {
        ScratchSourceKind::Note => history
            .with_connection(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT title, updated_at_ms FROM scratch_notes WHERE id = ?1",
                        params![target.id],
                        |row| Ok((row.get::<_, String>(0)?, Some(row.get::<_, i64>(1)?))),
                    )
                    .optional()?)
            })
            .map_err(|_| "Notes could not be read.".to_string())?
            .ok_or_else(|| "This source no longer exists.".to_string())?,
        ScratchSourceKind::Meeting => {
            let session = store
                .session_snapshot(MeetingSessionId::from_uuid(id))
                .map_err(|_| "This conversation is unavailable.".to_string())?;
            require_retained_transcript(&session)?;
            (session.title, i64::try_from(session.revision).ok())
        }
        ScratchSourceKind::Person => {
            let person = store
                .resolve_person_source(PersonId(id))
                .map_err(|_| "People could not be read.".to_string())?
                .ok_or_else(|| "This person no longer exists.".to_string())?;
            resolved_person = Some(person.id.uuid());
            (person.display_name, None)
        }
    };
    Ok(ScratchSource {
        kind: target.kind,
        id: target.id.clone(),
        title,
        link: format!(
            "sona://{}/{}",
            target.kind.as_str(),
            resolved_person.unwrap_or(id)
        ),
        revision,
        available: true,
    })
}

pub(crate) fn resolve_note(
    history: &HistoryManager,
    target: &ScratchSourceRef,
) -> Result<ResolvedSource, String> {
    if target.kind != ScratchSourceKind::Note {
        return Err("Choose a saved note.".to_string());
    }
    let id = Uuid::parse_str(&target.id).map_err(|_| "Choose a saved source.".to_string())?;
    let note = history
        .with_connection(|conn| note_with_connection(conn, &target.id))
        .map_err(|_| "Notes could not be read.".to_string())?
        .ok_or_else(|| "This source no longer exists.".to_string())?;
    Ok(ResolvedSource {
        source: ScratchSource {
            kind: ScratchSourceKind::Note,
            id: target.id.clone(),
            title: note.title,
            link: format!("sona://note/{id}"),
            revision: Some(note.updated_at_ms),
            available: true,
        },
        evidence: Some(note.body),
    })
}

pub(crate) fn resolve(
    history: &HistoryManager,
    store: &MeetingStore,
    target: &ScratchSourceRef,
) -> Result<ResolvedSource, String> {
    let id = Uuid::parse_str(&target.id).map_err(|_| "Choose a saved source.".to_string())?;
    let mut resolved_person = None;
    let (title, revision, evidence) = match target.kind {
        ScratchSourceKind::Note => return resolve_note(history, target),
        ScratchSourceKind::Meeting => {
            let snapshot = store
                .review_snapshot(MeetingSessionId::from_uuid(id))
                .map_err(|_| "This conversation is unavailable.".to_string())?;
            require_retained_transcript(&snapshot.session)?;
            let mut lines = Vec::new();
            for segment in snapshot
                .transcript
                .iter()
                .filter(|segment| !segment.removed)
            {
                let name = snapshot
                    .speakers
                    .iter()
                    .find(|speaker| speaker.speaker_id == segment.assigned_speaker_id)
                    .map(|speaker| speaker.display_name.as_str())
                    .unwrap_or("Unknown speaker");
                lines.push(format!(
                    "{}ms {} — transcript quote: {}",
                    segment.base.start_offset_ns / 1_000_000,
                    name,
                    segment.base.text
                ));
                if let Some(corrected) = &segment.replacement_text {
                    lines.push(format!("Corrected wording: {corrected}"));
                }
            }
            for note in &snapshot.notes {
                lines.push(format!("Explicit meeting note: {}", note.body));
            }
            (
                snapshot.session.title,
                i64::try_from(snapshot.session.revision).ok(),
                Some(lines.join("\n")),
            )
        }
        ScratchSourceKind::Person => {
            let person = store
                .resolve_person_source(PersonId(id))
                .map_err(|_| "People could not be read.".to_string())?
                .ok_or_else(|| "This person no longer exists.".to_string())?;
            resolved_person = Some(person.id.uuid());
            (person.display_name, None, None)
        }
    };
    Ok(ResolvedSource {
        source: ScratchSource {
            kind: target.kind,
            id: target.id.clone(),
            title,
            link: format!(
                "sona://{}/{}",
                target.kind.as_str(),
                resolved_person.unwrap_or(id)
            ),
            revision,
            available: true,
        },
        evidence,
    })
}

fn require_retained_transcript(session: &MeetingSessionSnapshot) -> Result<(), String> {
    if session.phase == MeetingPhase::Deleting {
        return Err("This conversation is unavailable.".to_string());
    }
    if session.transcript_purged_at_utc_ms.is_some() {
        return Err("This conversation's transcript was deleted.".to_string());
    }
    Ok(())
}

pub(crate) fn sources_with_connection(
    conn: &Connection,
    note_id: &str,
) -> anyhow::Result<Vec<ScratchSource>> {
    let mut statement = conn.prepare(
        "SELECT kind, target_id, title, revision FROM scratch_note_sources WHERE note_id = ?1 ORDER BY rowid"
    )?;
    let sources = statement
        .query_map(params![note_id], |row| {
            let kind = match row.get::<_, String>(0)?.as_str() {
                "meeting" => ScratchSourceKind::Meeting,
                "person" => ScratchSourceKind::Person,
                _ => ScratchSourceKind::Note,
            };
            let id: String = row.get(1)?;
            Ok(ScratchSource {
                kind,
                link: format!("sona://{}/{}", kind.as_str(), id),
                id,
                title: row.get(2)?,
                revision: row.get(3)?,
                available: true,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(sources)
}

pub(crate) fn insert_source(
    conn: &Connection,
    note_id: &str,
    resolved: &ResolvedSource,
) -> rusqlite::Result<()> {
    let source = &resolved.source;
    let (title, revision, evidence) = if source.kind == ScratchSourceKind::Meeting {
        ("Conversation", None, None)
    } else {
        (
            source.title.as_str(),
            source.revision,
            resolved.evidence.as_deref(),
        )
    };
    conn.execute(
        "INSERT INTO scratch_note_sources (note_id, kind, target_id, title, revision, evidence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(note_id, kind, target_id) DO NOTHING",
        params![
            note_id,
            source.kind.as_str(),
            source.id,
            title,
            revision,
            evidence
        ],
    )?;
    Ok(())
}

fn has_non_note_sources(history: &HistoryManager, id: &str) -> Result<bool, String> {
    history
        .with_connection(|conn| {
            Ok(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM scratch_note_sources WHERE note_id = ?1 AND kind != 'note')",
                params![id],
                |row| row.get(0),
            )?)
        })
        .map_err(|_| "The note's sources could not be read.".to_string())
}

pub(crate) fn read_memory(
    history: &HistoryManager,
    store: Option<&MeetingStore>,
    id: &str,
) -> Result<Option<ScratchNoteMemory>, String> {
    let memory = history.with_connection(|conn| {
        let Some(note) = note_with_connection(conn, id)? else { return Ok(None) };
        let sources = sources_with_connection(conn, id)?;
        let mut statement = conn.prepare(
            "SELECT DISTINCT n.id, n.title, n.body, n.pinned, n.created_at_ms, n.updated_at_ms
             FROM scratch_notes n WHERE n.id != ?1 AND (
                n.id IN (SELECT target_id FROM scratch_note_sources WHERE note_id = ?1 AND kind = 'note')
                OR n.id IN (SELECT note_id FROM scratch_note_sources WHERE kind = 'note' AND target_id = ?1)
                OR n.id IN (SELECT other.note_id FROM scratch_note_sources own
                    JOIN scratch_note_sources other ON own.kind = other.kind AND own.target_id = other.target_id
                    WHERE own.note_id = ?1)
             ) ORDER BY n.updated_at_ms DESC, n.id LIMIT 30"
        )?;
        let related_notes = statement.query_map(params![id], super::read_note_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let passages = super::passages::read(conn, id)?;
        let interpretation = conn.query_row("SELECT EXISTS(SELECT 1 FROM scratch_note_origins WHERE note_id = ?1)", params![id], |row| row.get(0))?;
        Ok(Some(ScratchNoteMemory { note, sources, related_notes, passages, interpretation, connections: Vec::new(), phone_sources: Vec::new() }))
    }).map_err(|_| "The note's sources could not be read.".to_string())?;
    let Some(mut memory) = memory else {
        return Ok(None);
    };
    if store.is_none()
        && memory
            .sources
            .iter()
            .any(|source| source.kind != ScratchSourceKind::Note)
    {
        return Err("Conversations are still unlocking.".to_string());
    }
    memory.phone_sources = super::phone_import::sources_for_note(history, id)
        .map_err(|_| "The note's phone sources could not be read.".to_string())?;
    let mut captured = if memory
        .sources
        .iter()
        .any(|source| source.kind == ScratchSourceKind::Meeting)
    {
        let note_id =
            Uuid::parse_str(id).map_err(|_| "The note's sources could not be read.".to_string())?;
        store
            .ok_or_else(|| "Conversations are still unlocking.".to_string())?
            .captured_note_sources(note_id)
            .map_err(|_| "The note's sources could not be read.".to_string())?
    } else {
        Vec::new()
    };
    for source in &mut memory.sources {
        let target = ScratchSourceRef {
            kind: source.kind,
            id: source.id.clone(),
        };
        let current = if source.kind == ScratchSourceKind::Note {
            resolve_note(history, &target).map(|resolved| resolved.source)
        } else {
            describe(
                history,
                store.ok_or_else(|| "Conversations are still unlocking.".to_string())?,
                &target,
            )
        };
        match current {
            Ok(current) if source.kind == ScratchSourceKind::Meeting => {
                *source = current;
                let session_id = Uuid::parse_str(&source.id).ok();
                if let Some(index) = captured
                    .iter()
                    .position(|saved| Some(saved.session_id.uuid()) == session_id)
                {
                    let saved = captured.swap_remove(index);
                    source.title = saved.title;
                    source.revision = i64::try_from(saved.input_revision).ok();
                }
            }
            Ok(current) if source.kind == ScratchSourceKind::Person => {
                source.title = current.title;
                source.link = current.link;
            }
            Ok(_) => {}
            Err(_) => {
                source.available = false;
                source.title = "Unavailable source".to_string();
                source.revision = None;
                source.link.clear();
            }
        }
    }
    memory.passages.retain(|passage| {
        memory.sources.iter().any(|source| {
            source.kind == ScratchSourceKind::Meeting
                && source.id == passage.meeting_id
                && source.available
        })
    });
    for related in &memory.related_notes {
        let related_sources = history
            .with_connection(|conn| sources_with_connection(conn, &related.id))
            .map_err(|_| "The note's connections could not be read.".to_string())?;
        let mut reasons = Vec::new();
        if memory
            .sources
            .iter()
            .any(|source| source.kind == ScratchSourceKind::Note && source.id == related.id)
        {
            reasons.push("Linked from this note".to_string());
        }
        if related_sources
            .iter()
            .any(|source| source.kind == ScratchSourceKind::Note && source.id == id)
        {
            reasons.push("Links to this note".to_string());
        }
        for source in memory.sources.iter().filter(|source| source.available) {
            if related_sources
                .iter()
                .any(|other| other.kind == source.kind && other.id == source.id)
            {
                reasons.push(format!("Shared {}: {}", source.kind.as_str(), source.title));
            }
        }
        if !reasons.is_empty() {
            memory.connections.push(ScratchConnection {
                note_id: related.id.clone(),
                reasons,
            });
        }
    }
    memory.related_notes.retain(|note| {
        memory
            .connections
            .iter()
            .any(|connection| connection.note_id == note.id)
    });
    Ok(Some(memory))
}

#[tauri::command]
#[specta::specta]
pub async fn scratchpad_memory(
    history: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    id: String,
) -> Result<Option<ScratchNoteMemory>, String> {
    let store = if has_non_note_sources(&history, &id)? {
        Some(
            meetings
                .store()
                .await
                .map_err(|_| "Conversations are still unlocking.".to_string())?,
        )
    } else {
        None
    };
    read_memory(&history, store.as_deref(), &id)
}

#[tauri::command]
#[specta::specta]
pub async fn scratchpad_link(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    id: String,
    source: ScratchSourceRef,
) -> Result<ScratchNoteMemory, String> {
    if source.kind == ScratchSourceKind::Note && source.id == id {
        return Err("A note cannot link to itself.".to_string());
    }
    let store = if source.kind == ScratchSourceKind::Note && !has_non_note_sources(&history, &id)? {
        None
    } else {
        Some(
            meetings
                .store()
                .await
                .map_err(|_| "Conversations are still unlocking.".to_string())?,
        )
    };
    let source = if source.kind == ScratchSourceKind::Note {
        resolve_note(&history, &source)?.source
    } else {
        describe(
            &history,
            store
                .as_deref()
                .ok_or_else(|| "Conversations are still unlocking.".to_string())?,
            &source,
        )?
    };
    run(&history, |conn| {
        if note_with_connection(conn, &id)?.is_none() {
            return Err(ScratchpadRefusal::NotFound.into());
        }
        // Explicit links do not copy source text. Only synthesis freezes its evidence.
        insert_source(
            conn,
            &id,
            &ResolvedSource {
                source,
                evidence: None,
            },
        )?;
        Ok(())
    })
    .map_err(|error| failure_sentence(&history, error, "The source could not be linked."))?;
    emit_changed(&app, &id, false);
    read_memory(&history, store.as_deref(), &id)?
        .ok_or_else(|| "This note no longer exists.".to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn scratchpad_unlink(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    id: String,
    source: ScratchSourceRef,
) -> Result<(), String> {
    let persist = || {
        run(&history, |conn| {
            if note_with_connection(conn, &id)?.is_none() {
                return Err(ScratchpadRefusal::NotFound.into());
            }
            conn.execute(
                "DELETE FROM scratch_note_sources WHERE note_id = ?1 AND kind = ?2 AND target_id = ?3",
                params![id, source.kind.as_str(), source.id],
            )?;
            Ok(())
        })
        .map_err(|error| failure_sentence(&history, error, "The source could not be removed."))
    };
    if source.kind == ScratchSourceKind::Meeting {
        let note_id =
            Uuid::parse_str(&id).map_err(|_| "This note no longer exists.".to_string())?;
        let session_id = Uuid::parse_str(&source.id)
            .map(MeetingSessionId::from_uuid)
            .map_err(|_| "Choose a saved source.".to_string())?;
        let store = meetings
            .store()
            .await
            .map_err(|_| "Conversations are still unlocking.".to_string())?;
        store
            .remove_note_evidence(
                NoteEvidenceRemoval::Source {
                    note_id,
                    session_id,
                },
                persist,
            )
            .map_err(|error| match error {
                NoteEvidenceError::Persist(error) => error,
                NoteEvidenceError::Store(_) => "The source could not be removed.".to_string(),
            })?;
    } else {
        persist()?;
    }
    emit_changed(&app, &id, false);
    Ok(())
}

pub(crate) fn unique_targets(targets: Vec<ScratchSourceRef>) -> Vec<ScratchSourceRef> {
    let mut seen = HashSet::new();
    targets
        .into_iter()
        .map(|mut target| {
            if let Ok(id) = Uuid::parse_str(&target.id) {
                target.id = id.to_string();
            }
            target
        })
        .filter(|target| seen.insert(target.clone()))
        .collect()
}
