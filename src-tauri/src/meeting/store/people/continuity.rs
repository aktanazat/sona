use super::{people_revision_in, person_by_id_in};
use crate::meeting::ledger::LedgerReceipt;
use crate::meeting::loop_types::MeetingLoopKind;
use crate::meeting::people_types::{
    PersonContributionAttribution, PersonId, PersonLinkConfidence, PersonLinkSource,
};
use crate::meeting::person_memory::{
    PersonMemoryCatchUp, PersonMemoryChange, PersonMemoryChangeKind, PersonMemoryCommitment,
    PersonMemoryCommitmentRelation, PersonMemoryComparisonSide, PersonMemoryContribution,
    PersonMemoryConversation, PersonMemoryNote, PersonMemoryPassage, PersonMemorySnapshot,
    PersonMemorySource, PersonMemorySourceKind, PersonMemoryThread, PersonMemoryThreadOrigin,
};
use crate::meeting::store::transcript_corrections::{
    audio_retained_in, resolved_person_for_speaker_in,
};
use crate::meeting::store::{
    canonical_speaker_ids, effective_segments_for_session,
    loops::{current_ledger_in, ledger_loop_seeds, rows_from_seeds_in},
    notes_for_session, speakers_for_session, utc_now_ms, MeetingStore, StoreError,
};
use crate::meeting::types::{
    EffectiveTranscriptSegment, MeetingSessionId, SpeakerAssignmentKind, TranscriptSegmentId,
};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

impl MeetingStore {
    pub(crate) fn person_memory_snapshot(
        &self,
        person_id: PersonId,
        since_utc_ms: Option<i64>,
    ) -> Result<PersonMemorySnapshot, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let person = person_by_id_in(&transaction, person_id)?;
        let mut snapshot = PersonMemorySnapshot {
            schema_version: 1, person_id, display_name: person.display_name,
            people_revision: people_revision_in(&transaction)?, read_at_utc_ms: utc_now_ms(), since_utc_ms,
            conversations: Vec::new(), contributions: Vec::new(), notes: Vec::new(), commitments: Vec::new(),
            threads: Vec::new(), changes: Vec::new(),
            catch_up: PersonMemoryCatchUp { last_conversation: None, contribution_ids: Vec::new(), commitment_ids: Vec::new(), note_ids: Vec::new(), change_ids: Vec::new() },
            limitations: vec![
                "Suggested conversations include recognized speech only; other notes and commitments need confirmation.".to_string(),
                "Topic labels and commitments are AI interpretations, not direct quotes.".to_string(),
                "Different wording is not proof that a decision changed.".to_string(),
                "Note comparisons need retained saved versions; merged or expired edits cannot be compared.".to_string(),
            ],
        };
        let mut statement = transaction.prepare(
            "SELECT m.id, m.title, COALESCE(m.started_at_utc_ms,m.created_at_utc_ms), m.revision,
                    l.source, l.confidence, m.transcript_purged_at_utc_ms IS NULL,
                    m.current_transcript_revision_id IS NOT NULL
             FROM meeting_sessions m JOIN meeting_person_links l ON l.meeting_id = m.id
             WHERE l.person_id = ?1 AND m.phase != 'deleting'
             ORDER BY COALESCE(m.started_at_utc_ms,m.created_at_utc_ms) DESC, m.id DESC",
        )?;
        let meetings = statement
            .query_map(params![person_id.uuid().to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, bool>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut topic_indices = HashMap::<String, usize>::new();
        for (id, title, at, revision, link_source, confidence, retained, has_transcript) in meetings
        {
            let session_id = Uuid::parse_str(&id)
                .map(MeetingSessionId::from_uuid)
                .map_err(|_| StoreError::Corrupt)?;
            let source = PersonMemorySource {
                kind: PersonMemorySourceKind::Conversation,
                id: id.clone(),
                title,
                link: format!("sona://meeting/{id}"),
                at_utc_ms: at,
                meeting_id: Some(id),
                start_offset_ns: None,
                revision: Some(revision.to_string()),
                transcript_available: Some(retained && has_transcript),
                audio_retained: None,
            };
            let confidence = match confidence.as_str() {
                "confirmed" => PersonLinkConfidence::Confirmed,
                "suggested" => PersonLinkConfidence::Suggested,
                _ => return Err(StoreError::Corrupt),
            };
            let link_source = match link_source.as_str() {
                "calendar" => PersonLinkSource::Calendar,
                "speaker" => PersonLinkSource::Speaker,
                "title" => PersonLinkSource::Title,
                "manual" => PersonLinkSource::Manual,
                _ => return Err(StoreError::Corrupt),
            };
            let mut conversation = PersonMemoryConversation {
                source,
                confidence,
                link_source,
                transcript_available: retained && has_transcript,
                attributed_contributions: 0,
            };
            if retained {
                read_conversation(
                    &transaction,
                    session_id,
                    person_id,
                    &mut conversation,
                    &mut snapshot,
                    &mut topic_indices,
                )?;
            }
            snapshot.conversations.push(conversation);
        }
        snapshot.contributions.sort_by(|left, right| {
            right
                .passage
                .source
                .at_utc_ms
                .cmp(&left.passage.source.at_utc_ms)
                .then_with(|| {
                    right
                        .passage
                        .source
                        .start_offset_ns
                        .cmp(&left.passage.source.start_offset_ns)
                })
                .then_with(|| left.id.cmp(&right.id))
        });
        snapshot.changes.retain(|change| {
            since_utc_ms.is_none_or(|since| change.later.source.at_utc_ms > since)
        });
        transaction.commit()?;
        Ok(snapshot)
    }
}

fn read_conversation(
    connection: &Connection,
    session_id: MeetingSessionId,
    person_id: PersonId,
    conversation: &mut PersonMemoryConversation,
    snapshot: &mut PersonMemorySnapshot,
    topic_indices: &mut HashMap<String, usize>,
) -> Result<(), StoreError> {
    let canonical = canonical_speaker_ids(connection, session_id)?;
    let speakers = speakers_for_session(connection, session_id)?;
    let segments = effective_segments_for_session(connection, session_id)?;
    let mut attributions = HashMap::new();
    for speaker in &speakers {
        let speaker_id = canonical
            .get(&speaker.speaker_id)
            .copied()
            .unwrap_or(speaker.speaker_id);
        if attributions.contains_key(&speaker_id) {
            continue;
        }
        let attribution = if resolved_person_for_speaker_in(connection, session_id, speaker_id)?
            == Some(person_id)
        {
            let confirmed: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM meeting_voice_speaker_resolutions WHERE speaker_id = ?1
                 AND is_current = 1 AND resolution_kind = 'identified' AND resolved_person_id = ?2)",
                params![speaker_id.uuid().to_string(), person_id.uuid().to_string()], |row| row.get(0)
            )?;
            Some(if confirmed {
                PersonContributionAttribution::Confirmed
            } else {
                PersonContributionAttribution::Recognized
            })
        } else {
            None
        };
        attributions.insert(speaker_id, attribution);
    }
    let mut active = segments
        .iter()
        .filter(|segment| !segment.removed && !effective_text(segment).trim().is_empty())
        .collect::<Vec<_>>();
    active.sort_by_key(|segment| (segment.base.start_offset_ns, segment.base.ordinal));
    let segments_by_id = active
        .iter()
        .map(|segment| (segment.base.segment_id, *segment))
        .collect::<HashMap<_, _>>();
    let audio_retained = active
        .iter()
        .map(|segment| {
            Ok((
                segment.base.segment_id,
                audio_retained_in(connection, session_id, segment.base.segment_id)?,
            ))
        })
        .collect::<Result<HashMap<_, _>, StoreError>>()?;
    let mut person_segments = HashSet::new();
    let passage = |segment: &EffectiveTranscriptSegment| {
        let speaker_id = canonical
            .get(&segment.assigned_speaker_id)
            .copied()
            .unwrap_or(segment.assigned_speaker_id);
        let label = if matches!(
            segment.speaker_assignment,
            SpeakerAssignmentKind::Unknown | SpeakerAssignmentKind::Overlap
        ) {
            "Unknown speaker".to_string()
        } else {
            speakers
                .iter()
                .find(|speaker| speaker.speaker_id == speaker_id)
                .map(|speaker| speaker.display_name.clone())
                .unwrap_or_else(|| "Unknown speaker".to_string())
        };
        passage_for(
            &conversation.source,
            segment,
            label,
            audio_retained[&segment.base.segment_id],
        )
    };
    for (index, segment) in active.iter().enumerate() {
        if !matches!(
            segment.speaker_assignment,
            SpeakerAssignmentKind::LocalSpeaker | SpeakerAssignmentKind::SystemSpeaker
        ) {
            continue;
        }
        let speaker_id = canonical
            .get(&segment.assigned_speaker_id)
            .copied()
            .unwrap_or(segment.assigned_speaker_id);
        let Some(attribution) = attributions.get(&speaker_id).copied().flatten() else {
            continue;
        };
        person_segments.insert(segment.base.segment_id);
        snapshot.contributions.push(PersonMemoryContribution {
            id: segment.base.segment_id.uuid().to_string(),
            passage: passage(segment),
            attribution,
            before: index
                .checked_sub(1)
                .map(|previous| passage(active[previous])),
            after: active.get(index + 1).map(|next| passage(next)),
        });
        conversation.attributed_contributions += 1;
    }
    if conversation.confidence != PersonLinkConfidence::Confirmed {
        return Ok(());
    }
    for note in notes_for_session(connection, session_id)? {
        let mut source = conversation.source.clone();
        source.kind = PersonMemorySourceKind::MeetingNote;
        source.id = note.note_id.uuid().to_string();
        source.at_utc_ms = note.updated_at_utc_ms;
        source.start_offset_ns = note.start_offset_ns;
        source.revision = Some(note.revision.to_string());
        source.link = moment_link(session_id, note.start_offset_ns);
        snapshot.notes.push(PersonMemoryNote {
            source,
            body: note.body,
            includes_ai_synthesis: false,
            directly_linked: false,
            related_sources: vec![conversation.source.clone()],
        });
    }
    let Some(ledger) = current_ledger_in(connection, session_id)? else {
        return Ok(());
    };
    for row in rows_from_seeds_in(
        connection,
        session_id,
        ledger_loop_seeds(session_id, &ledger),
    )? {
        if row.kind != MeetingLoopKind::Commitment {
            continue;
        }
        let relation = if row.owner_person_id == Some(person_id) {
            PersonMemoryCommitmentRelation::ExplicitOwner
        } else if row.owner_person_id.is_none()
            && row
                .citations
                .iter()
                .any(|citation| person_segments.contains(&citation.segment_id))
        {
            PersonMemoryCommitmentRelation::AttributedSpeech
        } else {
            PersonMemoryCommitmentRelation::SharedConversation
        };
        let mut cited = HashSet::new();
        let evidence = row
            .citations
            .iter()
            .filter(|citation| cited.insert(citation.segment_id))
            .filter_map(|citation| segments_by_id.get(&citation.segment_id))
            .map(|segment| passage(segment))
            .collect::<Vec<_>>();
        if evidence.is_empty() {
            continue;
        }
        snapshot.commitments.push(PersonMemoryCommitment {
            id: row.loop_id.0,
            interpretation: row.text,
            status: row.status,
            relation,
            evidence,
        });
    }
    for thread in &ledger.threads {
        if !thread.substantive {
            continue;
        }
        let evidence = receipt_evidence(&thread.receipt, &segments_by_id, &passage);
        if evidence.is_empty() {
            continue;
        }
        let key = thread.topic.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        if let Some(index) = topic_indices.get(&key).copied() {
            let later = snapshot.threads[index].evidence.last();
            if let (Some(later), Some(earlier)) = (later, evidence.first()) {
                let later_text = &later.raw_text;
                let earlier_text = &earlier.raw_text;
                if later.source.meeting_id != earlier.source.meeting_id
                    && later_text != earlier_text
                {
                    snapshot.changes.push(PersonMemoryChange {
                        id: format!("thread:{}:{}:{}", key, earlier.source.id, later.source.id),
                        kind: PersonMemoryChangeKind::SubjectEvidenceDiffers,
                        title: thread.topic.clone(),
                        earlier: PersonMemoryComparisonSide {
                            source: earlier.source.clone(),
                            text: earlier_text.to_string(),
                            includes_ai_interpretation: false,
                        },
                        later: PersonMemoryComparisonSide {
                            source: later.source.clone(),
                            text: later_text.to_string(),
                            includes_ai_interpretation: false,
                        },
                    });
                }
            }
            for passage in evidence {
                let target = &mut snapshot.threads[index];
                if target
                    .evidence
                    .iter()
                    .any(|existing| existing.source.id == passage.source.id)
                {
                    continue;
                }
                target.sources.push(passage.source.clone());
                target.evidence.push(passage);
            }
        } else {
            topic_indices.insert(key.clone(), snapshot.threads.len());
            snapshot.threads.push(PersonMemoryThread {
                id: format!("topic:{key}"),
                title: thread.topic.clone(),
                interpretation: "AI topic · matched labels, not a confirmed decision change"
                    .to_string(),
                origin: PersonMemoryThreadOrigin::AiTopic,
                sources: evidence
                    .iter()
                    .map(|passage| passage.source.clone())
                    .collect(),
                evidence,
            });
        }
    }
    Ok(())
}

fn receipt_evidence(
    receipt: &LedgerReceipt,
    segments: &HashMap<TranscriptSegmentId, &EffectiveTranscriptSegment>,
    passage: &impl Fn(&EffectiveTranscriptSegment) -> PersonMemoryPassage,
) -> Vec<PersonMemoryPassage> {
    if receipt.quote.trim().is_empty() {
        return Vec::new();
    }
    let mut cited = HashSet::new();
    let mut evidence = receipt
        .citations
        .iter()
        .filter(|citation| cited.insert(citation.segment_id))
        .filter_map(|citation| segments.get(&citation.segment_id))
        .filter(|segment| effective_text(segment).contains(receipt.quote.trim()))
        .map(|segment| passage(segment))
        .collect::<Vec<_>>();
    evidence.sort_by_key(|passage| passage.source.start_offset_ns);
    evidence
}

fn effective_text(segment: &EffectiveTranscriptSegment) -> &str {
    segment
        .replacement_text
        .as_deref()
        .unwrap_or(&segment.base.text)
}

fn passage_for(
    source: &PersonMemorySource,
    segment: &EffectiveTranscriptSegment,
    speaker_label: String,
    audio_retained: bool,
) -> PersonMemoryPassage {
    let mut source = source.clone();
    source.kind = PersonMemorySourceKind::Transcript;
    source.id = segment.base.segment_id.uuid().to_string();
    source.start_offset_ns = Some(segment.base.start_offset_ns);
    source.revision = Some(segment.base.transcript_revision_id.uuid().to_string());
    source.audio_retained = Some(audio_retained);
    if let Some(meeting_id) = source.meeting_id.as_deref() {
        source.link = format!(
            "sona://meeting/{meeting_id}?atMs={}&segmentId={}",
            segment.base.start_offset_ns / 1_000_000,
            segment.base.segment_id.uuid()
        );
    }
    PersonMemoryPassage {
        source,
        speaker_label,
        raw_text: segment.base.text.clone(),
        corrected_text: segment
            .replacement_text
            .as_ref()
            .filter(|text| *text != &segment.base.text)
            .cloned(),
        edit_revision: segment.edit_revision,
    }
}

fn moment_link(session_id: MeetingSessionId, offset: Option<u64>) -> String {
    match offset {
        Some(offset) => format!(
            "sona://meeting/{}?atMs={}",
            session_id.uuid(),
            offset / 1_000_000
        ),
        None => format!("sona://meeting/{}", session_id.uuid()),
    }
}
