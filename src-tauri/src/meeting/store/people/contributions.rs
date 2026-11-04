use super::{people_revision_in, person_by_id_in, SCHEMA_VERSION};
use crate::meeting::people_types::{
    PersonContribution, PersonContributionAttribution, PersonContributionsResult, PersonId,
    PersonLinkConfidence,
};
use crate::meeting::store::transcript_corrections::resolved_person_for_speaker_in;
use crate::meeting::store::{
    canonical_speaker_ids, effective_segments_for_session, MeetingStore, StoreError,
};
use crate::meeting::types::{MeetingSessionId, SpeakerAssignmentKind};
use std::collections::HashMap;
use uuid::Uuid;

impl MeetingStore {
    pub(crate) fn person_contributions(
        &self,
        person_id: PersonId,
        offset: u64,
        limit: usize,
    ) -> Result<PersonContributionsResult, StoreError> {
        let offset = usize::try_from(offset).map_err(|_| StoreError::Invalid)?;
        let limit = limit.clamp(1, 100);
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        person_by_id_in(&transaction, person_id)?;
        let revision = people_revision_in(&transaction)?;
        let mut meetings = transaction.prepare(
            "SELECT m.id, m.title, COALESCE(m.started_at_utc_ms, m.created_at_utc_ms), l.confidence
               FROM meeting_sessions m
               JOIN meeting_person_links l ON l.meeting_id = m.id
              WHERE l.person_id = ?1
                AND m.phase != 'deleting' AND m.transcript_purged_at_utc_ms IS NULL
                AND m.current_transcript_revision_id IS NOT NULL
              ORDER BY COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) DESC, m.id DESC",
        )?;
        let mut rows = meetings.query([person_id.uuid().to_string()])?;
        let mut entries = Vec::with_capacity(limit);
        let mut matched = 0_usize;
        let mut has_more = false;
        'meetings: while let Some(row) = rows.next()? {
            let meeting_id: String = row.get(0)?;
            let meeting_id = Uuid::parse_str(&meeting_id)
                .map(MeetingSessionId::from_uuid)
                .map_err(|_| StoreError::Corrupt)?;
            let title: String = row.get(1)?;
            let at_utc_ms: i64 = row.get(2)?;
            let confidence: String = row.get(3)?;
            let link_confidence = match confidence.as_str() {
                "confirmed" => PersonLinkConfidence::Confirmed,
                "suggested" => PersonLinkConfidence::Suggested,
                _ => return Err(StoreError::Corrupt),
            };
            let canonical = canonical_speaker_ids(&transaction, meeting_id)?;
            let mut resolved = HashMap::new();
            for segment in effective_segments_for_session(&transaction, meeting_id)? {
                if segment.removed
                    || !matches!(
                        segment.speaker_assignment,
                        SpeakerAssignmentKind::LocalSpeaker | SpeakerAssignmentKind::SystemSpeaker
                    )
                {
                    continue;
                }
                let speaker_id = canonical
                    .get(&segment.assigned_speaker_id)
                    .copied()
                    .unwrap_or(segment.assigned_speaker_id);
                let attribution = if let Some(attribution) = resolved.get(&speaker_id) {
                    *attribution
                } else {
                    let person =
                        resolved_person_for_speaker_in(&transaction, meeting_id, speaker_id)?;
                    let attribution = if person == Some(person_id) {
                        let confirmed: bool = transaction.query_row(
                            "SELECT EXISTS(
                                SELECT 1 FROM meeting_voice_speaker_resolutions
                                 WHERE speaker_id = ?1 AND is_current = 1
                                   AND resolution_kind = 'identified' AND resolved_person_id = ?2
                             )",
                            rusqlite::params![
                                speaker_id.uuid().to_string(),
                                person_id.uuid().to_string()
                            ],
                            |row| row.get(0),
                        )?;
                        Some(if confirmed {
                            PersonContributionAttribution::Confirmed
                        } else {
                            PersonContributionAttribution::Recognized
                        })
                    } else {
                        None
                    };
                    resolved.insert(speaker_id, attribution);
                    attribution
                };
                let Some(attribution) = attribution else {
                    continue;
                };
                if segment
                    .replacement_text
                    .as_deref()
                    .unwrap_or(&segment.base.text)
                    .trim()
                    .is_empty()
                {
                    continue;
                }
                if matched < offset {
                    matched += 1;
                    continue;
                }
                if entries.len() == limit {
                    has_more = true;
                    break 'meetings;
                }
                entries.push(PersonContribution {
                    meeting_id,
                    meeting_title: title.clone(),
                    meeting_at_utc_ms: at_utc_ms,
                    transcript_revision_id: segment.base.transcript_revision_id,
                    segment_id: segment.base.segment_id,
                    track_id: segment.base.track_id,
                    speaker_id,
                    attribution,
                    link_confidence,
                    start_offset_ns: segment.base.start_offset_ns,
                    end_offset_ns: segment.base.end_offset_ns,
                    corrected_text: segment
                        .replacement_text
                        .filter(|text| text != &segment.base.text),
                    raw_text: segment.base.text,
                });
            }
        }
        drop(rows);
        drop(meetings);
        let next_offset = if has_more {
            Some(
                u64::try_from(
                    offset
                        .checked_add(entries.len())
                        .ok_or(StoreError::Invalid)?,
                )
                .map_err(|_| StoreError::Invalid)?,
            )
        } else {
            None
        };
        transaction.commit()?;
        Ok(PersonContributionsResult {
            schema_version: SCHEMA_VERSION,
            revision,
            entries,
            next_offset,
        })
    }
}
