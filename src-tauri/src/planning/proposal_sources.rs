use super::proposal::{PlanningPassage, PlanningReviewSource, PlanningSourceRef};
use super::store::{self, Refusal};
use super::types::EvidenceSourceKind;
use crate::meeting::loop_types::{MeetingLoopId, MeetingLoopKind};
use crate::meeting::store::{effective_segments_for_session, loops};
use crate::meeting::types::MeetingSessionId;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use uuid::Uuid;

pub(super) const MAX_SOURCES: usize = 12;

#[derive(Serialize)]
pub(super) struct Passage {
    pub id: String,
    pub at_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub text: String,
    pub original_text: Option<String>,
}

#[derive(Serialize)]
pub(super) struct Commitment {
    pub loop_id: String,
    pub text: String,
    pub at_ms: i64,
    pub passage_ids: Vec<String>,
}

pub(super) struct ResolvedSource {
    pub review: PlanningReviewSource,
    pub passages: Vec<Passage>,
    pub commitments: Vec<Commitment>,
}

pub(super) fn fingerprint(value: &impl Serialize) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(super) fn selected_sources(targets: Vec<PlanningSourceRef>) -> Result<Vec<PlanningSourceRef>> {
    if targets.len() > MAX_SOURCES {
        return Err(Refusal::Invalid.into());
    }
    let mut seen = HashSet::new();
    targets
        .into_iter()
        .map(|mut target| {
            target.id = super::sources::canonical_source_id(target.kind, &target.id)?;
            if !seen.insert((target.kind.as_str(), target.id.clone())) {
                return Err(Refusal::Invalid.into());
            }
            Ok(target)
        })
        .collect()
}

pub(super) fn resolve(
    history: &Connection,
    meetings: Option<&Connection>,
    target: &PlanningSourceRef,
) -> Result<ResolvedSource> {
    if super::sources::canonical_source_id(target.kind, &target.id)? != target.id {
        return Err(Refusal::Invalid.into());
    }
    if target.kind == EvidenceSourceKind::Note {
        super::sources::require_note(history, &target.id, None)?;
    } else {
        let meetings = meetings.ok_or(Refusal::SourceUnavailable)?;
        super::sources::require_source(history, meetings, target.kind, &target.id, None)?;
    }
    let (title, revision, passages, commitments) = match target.kind {
        EvidenceSourceKind::Dictation | EvidenceSourceKind::Link => {
            return Err(Refusal::Invalid.into())
        }
        EvidenceSourceKind::Note => {
            let note = crate::scratchpad::note_with_connection(history, &target.id)?
                .ok_or(Refusal::SourceUnavailable)?;
            let passage = Passage {
                id: "body".to_string(),
                at_ms: None,
                end_ms: None,
                text: note.body,
                original_text: None,
            };
            (note.title, note.updated_at_ms, vec![passage], Vec::new())
        }
        EvidenceSourceKind::Meeting | EvidenceSourceKind::Loop => {
            let meetings = meetings.ok_or(Refusal::SourceUnavailable)?;
            let meeting_id = match target.kind {
                EvidenceSourceKind::Meeting => {
                    MeetingSessionId::from_uuid(Uuid::parse_str(&store::uuid(&target.id)?)?)
                }
                EvidenceSourceKind::Loop => MeetingLoopId(target.id.clone())
                    .session_id()
                    .ok_or(Refusal::Invalid)?,
                EvidenceSourceKind::Note
                | EvidenceSourceKind::Dictation
                | EvidenceSourceKind::Link => return Err(Refusal::Invalid.into()),
            };
            let id = meeting_id.uuid().to_string();
            let (title, revision): (String, i64) = meetings
                .query_row(
                    "SELECT title, revision FROM meeting_sessions WHERE id = ?1
                     AND phase != 'deleting' AND transcript_purged_at_utc_ms IS NULL
                     AND current_transcript_revision_id IS NOT NULL",
                    [&id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or(Refusal::SourceUnavailable)?;
            let segments = effective_segments_for_session(meetings, meeting_id)
                .map_err(|_| anyhow::anyhow!("retained transcript could not be read"))?;
            let mut passages = Vec::new();
            for segment in segments.into_iter().filter(|segment| !segment.removed) {
                let start = i64::try_from(segment.base.start_offset_ns / 1_000_000)?;
                let end = i64::try_from(segment.base.end_offset_ns / 1_000_000)?;
                let (text, original_text) = match segment.replacement_text {
                    Some(text) => (text, Some(segment.base.text)),
                    None => (segment.base.text, None),
                };
                passages.push(Passage {
                    id: segment.base.segment_id.uuid().to_string(),
                    at_ms: Some(start),
                    end_ms: Some(end),
                    text,
                    original_text,
                });
            }
            let seeds = loops::seeds_in(meetings, meeting_id)
                .map_err(|_| anyhow::anyhow!("meeting commitments could not be read"))?;
            let mut commitments = Vec::new();
            let mut selected_loop_passages = None;
            for seed in seeds {
                let at_ms = i64::try_from(seed.at_ms)?;
                let passage_ids = if seed.citations.is_empty() {
                    passages
                        .iter()
                        .filter(|passage| {
                            passage.at_ms.is_some_and(|start| start <= at_ms)
                                && passage.end_ms.is_some_and(|end| at_ms < end)
                        })
                        .map(|passage| passage.id.clone())
                        .collect::<Vec<_>>()
                } else {
                    seed.citations
                        .iter()
                        .map(|citation| citation.segment_id.uuid().to_string())
                        .collect::<Vec<_>>()
                };
                let retained = !passage_ids.is_empty()
                    && passage_ids.iter().all(|id| {
                        passages
                            .iter()
                            .any(|passage| passage.id == *id && !passage.text.trim().is_empty())
                    });
                if seed.loop_id.as_str() == target.id {
                    if !retained {
                        return Err(Refusal::SourceUnavailable.into());
                    }
                    selected_loop_passages = Some(passage_ids.clone());
                }
                if seed.kind == MeetingLoopKind::Commitment && retained {
                    commitments.push(Commitment {
                        loop_id: seed.loop_id.0,
                        text: seed.text,
                        at_ms,
                        passage_ids,
                    });
                }
            }
            if target.kind == EvidenceSourceKind::Loop {
                let ids = selected_loop_passages.ok_or(Refusal::SourceUnavailable)?;
                passages.retain(|passage| ids.contains(&passage.id));
                commitments
                    .retain(|commitment| commitment.passage_ids.iter().any(|id| ids.contains(id)));
            } else {
                let mut statement = meetings.prepare(
                    "SELECT note_id, start_offset_ns, end_offset_ns, body FROM meeting_notes
                     WHERE session_id = ?1 ORDER BY created_at_utc_ms, note_id",
                )?;
                let notes = statement
                    .query_map(params![id], |row| {
                        Ok(Passage {
                            id: format!("note:{}", row.get::<_, String>(0)?),
                            at_ms: row.get::<_, Option<i64>>(1)?.map(|value| value / 1_000_000),
                            end_ms: row.get::<_, Option<i64>>(2)?.map(|value| value / 1_000_000),
                            text: row.get(3)?,
                            original_text: None,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                passages.extend(notes);
            }
            (title, revision, passages, commitments)
        }
    };
    let passages = passages
        .into_iter()
        .filter(|passage| !passage.text.trim().is_empty())
        .collect::<Vec<_>>();
    if passages.is_empty() {
        return Err(Refusal::SourceUnavailable.into());
    }
    let content_hash = fingerprint(&(&title, revision, &passages, &commitments))?;
    let passage_refs = passages
        .iter()
        .map(|passage| {
            Ok(PlanningPassage {
                id: passage.id.clone(),
                at_ms: passage.at_ms,
                content_hash: fingerprint(passage)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ResolvedSource {
        review: PlanningReviewSource {
            source: target.clone(),
            title,
            revision,
            content_hash,
            passages: passage_refs,
        },
        passages,
        commitments,
    })
}

pub(super) fn meeting_note_id(kind: EvidenceSourceKind, passage_id: &str) -> Option<&str> {
    if kind == EvidenceSourceKind::Meeting {
        passage_id.strip_prefix("note:")
    } else {
        None
    }
}

pub(super) fn link_offset(
    kind: EvidenceSourceKind,
    passage_id: &str,
    at_ms: Option<i64>,
) -> Option<i64> {
    match kind {
        EvidenceSourceKind::Meeting if meeting_note_id(kind, passage_id).is_none() => at_ms,
        EvidenceSourceKind::Meeting
        | EvidenceSourceKind::Note
        | EvidenceSourceKind::Loop
        | EvidenceSourceKind::Dictation
        | EvidenceSourceKind::Link => None,
    }
}
