use super::list_notes_with_connection;
use crate::managers::history::HistoryManager;
use crate::meeting::store::memory_index::{meeting_source_in, person_source_in};
use crate::meeting::store::StoreError;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Serialize)]
pub(crate) struct MemoryNote {
    pub id: String,
    pub title: String,
    pub body: String,
    pub updated_at_ms: i64,
    pub sources: Vec<MemoryNoteSource>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MemoryNoteSource {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub available: bool,
}

/// Reads only saved current notes while the caller holds the meeting snapshot.
pub(crate) fn notes_in_snapshot(
    history: &HistoryManager,
    meetings: &Connection,
) -> Result<Vec<MemoryNote>, StoreError> {
    history
        .with_connection(|connection| {
            let transaction = connection.transaction()?;
            let mut notes: Vec<MemoryNote> = list_notes_with_connection(&transaction, "")?
                .into_iter()
                .map(|note| MemoryNote {
                    id: note.id,
                    title: note.title,
                    body: note.body,
                    updated_at_ms: note.updated_at_ms,
                    sources: Vec::new(),
                })
                .collect();
            notes.sort_unstable_by(|left, right| left.id.cmp(&right.id));
            let positions: HashMap<String, usize> = notes
                .iter()
                .enumerate()
                .map(|(index, note)| (note.id.clone(), index))
                .collect();
            {
                let mut statement = transaction.prepare(
                    "SELECT source.note_id, source.kind, source.target_id, source.title,
                            target.id IS NOT NULL
                       FROM scratch_note_sources source
                       JOIN scratch_notes note ON note.id = source.note_id
                       LEFT JOIN scratch_notes target
                         ON source.kind = 'note' AND target.id = source.target_id
                      ORDER BY source.note_id, source.kind, source.target_id",
                )?;
                let mut rows = statement.query([])?;
                while let Some(row) = rows.next()? {
                    let note_id: String = row.get(0)?;
                    let kind: String = row.get(1)?;
                    let mut id: String = row.get(2)?;
                    let mut title: String = row.get(3)?;
                    let available = match kind.as_str() {
                        "note" => row.get::<_, bool>(4)?,
                        "meeting" => {
                            let source =
                                meeting_source_in(meetings, &note_id, &id).map_err(|_| {
                                    anyhow::anyhow!("conversation source could not be read")
                                })?;
                            if let Some(source) = source {
                                id = source.id;
                                title = source.title;
                                true
                            } else {
                                false
                            }
                        }
                        "person" => {
                            let source = person_source_in(meetings, &id)
                                .map_err(|_| anyhow::anyhow!("person source could not be read"))?;
                            if let Some(source) = source {
                                id = source.id;
                                title = source.title;
                                true
                            } else {
                                false
                            }
                        }
                        _ => anyhow::bail!("unknown saved note source kind"),
                    };
                    if !available {
                        title = "Unavailable source".to_owned();
                    }
                    let index = positions
                        .get(&note_id)
                        .copied()
                        .ok_or_else(|| anyhow::anyhow!("saved source has no note"))?;
                    notes[index].sources.push(MemoryNoteSource {
                        kind,
                        id,
                        title,
                        available,
                    });
                }
            }
            for note in &mut notes {
                note.sources.sort_unstable_by(|left, right| {
                    left.kind
                        .cmp(&right.kind)
                        .then_with(|| left.id.cmp(&right.id))
                });
                note.sources
                    .dedup_by(|left, right| left.kind == right.kind && left.id == right.id);
            }
            transaction.commit()?;
            Ok(notes)
        })
        .map_err(|_| StoreError::Unavailable)
}
