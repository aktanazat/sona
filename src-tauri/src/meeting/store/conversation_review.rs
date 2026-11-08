use super::{
    decode_json, decrypt_durable_record, from_i64, id, parse_uuid, session_row, to_i64,
    track_snapshots, utc_now_ms, DurableRecordDescriptor, DurableTrackRecord, MeetingStore,
    StoreError,
};
use crate::meeting::context::ConversationLocation;
use crate::meeting::conversation_review::{
    ConversationAudioClip, ConversationBookmark, ConversationBookmarkKind,
    ConversationBookmarkRequest, ConversationPlaceReview, ConversationPlaceSession,
};
use crate::meeting::types::{MeetingPhase, MeetingSessionId, SourceTrackId, TranscriptSegmentId};
use base64::Engine;
use rusqlite::{params, OptionalExtension};
use std::fs::File;

const SAMPLE_RATE: u32 = 16_000;
const NS_PER_SECOND: u64 = 1_000_000_000;
const SURROUNDING_NS: u64 = 4 * NS_PER_SECOND;
const CLIP_NS: u64 = 45 * NS_PER_SECOND;
/// Two saved places this close are the same place.
pub(super) const SAME_PLACE_METERS: f64 = 100.0;

impl MeetingStore {
    /// A stretch of one conversation's audio, at most `CLIP_NS` long. Around a
    /// contribution it runs from `start_offset_ns`, or `SURROUNDING_NS` before
    /// it, to `end_offset_ns`, or `SURROUNDING_NS` past it.
    pub(crate) fn conversation_audio_clip(
        &self,
        session_id: MeetingSessionId,
        segment_id: Option<TranscriptSegmentId>,
        start_offset_ns: Option<u64>,
        end_offset_ns: Option<u64>,
    ) -> Result<ConversationAudioClip, StoreError> {
        let (track_id, contribution_start, contribution_end, tracks) = {
            let connection = self.connection()?;
            super::audio_retention::require_retained_audio_in(&connection, session_id)?;
            let session = session_row(&connection, session_id)?;
            if session.phase == MeetingPhase::Deleting {
                return Err(StoreError::NotFound);
            }
            let tracks = track_snapshots(&connection, session_id)?;
            if let Some(segment_id) = segment_id {
                let source = connection.query_row(
                    "SELECT segment.track_id, segment.start_offset_ns, segment.end_offset_ns
                     FROM meeting_sessions session JOIN meeting_transcript_segments segment
                       ON segment.transcript_revision_id = session.current_transcript_revision_id
                     WHERE session.id = ?1 AND segment.segment_id = ?2
                       AND COALESCE((SELECT edit.removed FROM meeting_segment_edits edit
                           WHERE edit.segment_id = segment.segment_id ORDER BY edit.edit_sequence DESC LIMIT 1), 0) = 0",
                    params![id(session_id), id(segment_id)],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
                ).optional()?.ok_or(StoreError::NotFound)?;
                (
                    Some(SourceTrackId::from_uuid(parse_uuid(&source.0)?)),
                    from_i64(source.1)?,
                    from_i64(source.2)?,
                    tracks,
                )
            } else {
                let start = start_offset_ns.ok_or(StoreError::Invalid)?;
                (None, start, start, tracks)
            }
        };
        let earliest = contribution_start.saturating_sub(SURROUNDING_NS);
        let latest = tracks
            .iter()
            .filter(|track| track.durable_record_count > 0)
            .filter_map(|track| track.last_offset_ns)
            .max()
            .ok_or(StoreError::NotFound)?;
        let through = match end_offset_ns {
            Some(end) => end.min(latest),
            None if segment_id.is_some() => {
                contribution_end.saturating_add(SURROUNDING_NS).min(latest)
            }
            None => latest,
        };
        let start = start_offset_ns.unwrap_or(earliest);
        if start < earliest || start >= through {
            return Err(StoreError::Invalid);
        }
        let end = through.min(start.saturating_add(CLIP_NS));
        if end <= start {
            return Err(StoreError::Invalid);
        }
        let frames = usize::try_from((end - start) * u64::from(SAMPLE_RATE) / NS_PER_SECOND)
            .map_err(|_| StoreError::Invalid)?;
        let mut mixed = vec![0_f32; frames];
        let mut coverage = vec![0_u16; frames];
        let mut contribution_present = false;
        for track in tracks
            .into_iter()
            .filter(|track| track.durable_record_count > 0)
        {
            let descriptors = {
                let connection = self.connection()?;
                super::audio_retention::require_retained_audio_in(&connection, session_id)?;
                let mut query = connection.prepare(
                    "SELECT source_sequence, source_epoch, start_offset_ns, duration_ns,
                            frame_count, record_offset_bytes, record_bytes
                     FROM meeting_track_records WHERE track_id = ?1
                       AND start_offset_ns < ?3 AND start_offset_ns + duration_ns > ?2
                     ORDER BY source_sequence",
                )?;
                let rows = query
                    .query_map(
                        params![id(track.track_id), to_i64(start)?, to_i64(end)?],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, i64>(5)?,
                                row.get::<_, i64>(6)?,
                            ))
                        },
                    )?
                    .collect::<Result<Vec<_>, _>>()?;
                rows.into_iter()
                    .map(|row| {
                        Ok(DurableRecordDescriptor {
                            sequence: from_i64(row.0)?,
                            epoch: from_i64(row.1)?,
                            start_offset_ns: from_i64(row.2)?,
                            duration_ns: from_i64(row.3)?,
                            frame_count: u32::try_from(row.4).map_err(|_| StoreError::Corrupt)?,
                            record_offset: from_i64(row.5)?,
                            record_bytes: from_i64(row.6)?,
                        })
                    })
                    .collect::<Result<Vec<_>, StoreError>>()?
            };
            if descriptors.is_empty() {
                continue;
            }
            let key = self.track_key(session_id, track.track_id)?;
            let mut file = File::open(self.track_files(session_id, track.track_id).records)?;
            for descriptor in descriptors {
                let record = decrypt_durable_record(&mut file, &key, track.track_id, descriptor)?;
                if Some(track.track_id) == track_id
                    && record.start_offset_ns < contribution_end
                    && record.start_offset_ns.saturating_add(record.duration_ns)
                        > contribution_start
                {
                    contribution_present = true;
                }
                mix_record(&record, start, end, &mut mixed, &mut coverage)?;
            }
        }
        {
            let connection = self.connection()?;
            super::audio_retention::require_retained_audio_in(&connection, session_id)?;
        }
        if (segment_id.is_some() && !(contribution_present || start >= contribution_end))
            || !coverage.iter().any(|count| *count > 0)
        {
            return Err(StoreError::NotFound);
        }
        let wav = wav_bytes(&mixed, &coverage)?;
        Ok(ConversationAudioClip {
            session_id,
            segment_id,
            start_offset_ns: start,
            end_offset_ns: end,
            contribution_offset_ns: contribution_start.saturating_sub(start),
            next_offset_ns: (end < through).then_some(end),
            wav_base64: base64::engine::general_purpose::STANDARD.encode(wav),
        })
    }

    pub(crate) fn conversation_bookmarks(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<ConversationBookmark>, StoreError> {
        let connection = self.connection()?;
        export_bookmarks_in(&connection, session_id)
    }

    /// Every bookmark with its meeting's title, newest meeting first, then in
    /// the order they were pressed.
    pub(crate) fn all_conversation_bookmarks(
        &self,
    ) -> Result<Vec<(ConversationBookmark, String)>, StoreError> {
        let connection = self.connection()?;
        let mut query = connection.prepare(
            "SELECT bookmark.bookmark_id, bookmark.session_id, bookmark.start_offset_ns, bookmark.label,
                    bookmark.kind, bookmark.created_at_utc_ms, session.title
             FROM conversation_bookmarks bookmark JOIN meeting_sessions session ON session.id = bookmark.session_id
             WHERE session.phase != 'deleting'
             ORDER BY COALESCE(session.started_at_utc_ms, session.created_at_utc_ms) DESC, session.id,
                      bookmark.start_offset_ns, bookmark.bookmark_id",
        )?;
        let mut bookmarks = Vec::new();
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            bookmarks.push((
                ConversationBookmark {
                    bookmark_id: row.get(0)?,
                    session_id: MeetingSessionId::from_uuid(parse_uuid(&row.get::<_, String>(1)?)?),
                    start_offset_ns: from_i64(row.get(2)?)?,
                    label: row.get(3)?,
                    kind: ConversationBookmarkKind::parse(&row.get::<_, String>(4)?)
                        .ok_or(StoreError::Corrupt)?,
                    created_at_utc_ms: row.get(5)?,
                },
                row.get(6)?,
            ));
        }
        Ok(bookmarks)
    }

    pub(crate) fn conversation_bookmark_create(
        &self,
        request: ConversationBookmarkRequest,
    ) -> Result<ConversationBookmark, StoreError> {
        parse_uuid(&request.bookmark_id).map_err(|_| StoreError::Invalid)?;
        let label = request
            .label
            .map(|label| label.trim().to_owned())
            .filter(|label| !label.is_empty());
        if label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 120 || label.chars().any(char::is_control))
        {
            return Err(StoreError::Invalid);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let session = session_row(&transaction, request.session_id)?;
        if session.phase != MeetingPhase::CapturingRecording {
            return Err(StoreError::Conflict);
        }
        super::transcript_retention::require_retained_transcript_in(
            &transaction,
            request.session_id,
        )?;
        let created_at = utc_now_ms();
        transaction.execute(
            "INSERT INTO conversation_bookmarks(bookmark_id, session_id, start_offset_ns, label, kind, created_at_utc_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(bookmark_id) DO NOTHING",
            params![request.bookmark_id, id(request.session_id), to_i64(request.start_offset_ns)?, label, request.kind.as_str(), created_at],
        )?;
        let saved = transaction.query_row(
            "SELECT session_id, start_offset_ns, label, kind, created_at_utc_ms FROM conversation_bookmarks WHERE bookmark_id = ?1",
            params![request.bookmark_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?, row.get::<_, String>(3)?, row.get::<_, i64>(4)?)),
        )?;
        if saved.0 != id(request.session_id)
            || from_i64(saved.1)? != request.start_offset_ns
            || saved.2 != label
            || saved.3 != request.kind.as_str()
        {
            return Err(StoreError::Conflict);
        }
        transaction.commit()?;
        Ok(ConversationBookmark {
            bookmark_id: request.bookmark_id,
            session_id: request.session_id,
            start_offset_ns: request.start_offset_ns,
            label,
            kind: request.kind,
            created_at_utc_ms: saved.4,
        })
    }

    /// The moments a device bookmarked while it recorded, filed as plain
    /// bookmarks on the meeting its recording became, each dated when it was
    /// pressed. A device clock that ran ahead of its samples can put a
    /// last-instant bookmark past the audio; it lands on the end.
    pub(crate) fn add_device_bookmarks(
        &self,
        session_id: MeetingSessionId,
        offsets_ns: &[u64],
    ) -> Result<(), StoreError> {
        if offsets_ns.is_empty() {
            return Ok(());
        }
        let session = self.session_snapshot(session_id)?;
        let end_ns = session.elapsed_offset_ns.unwrap_or(0);
        let started_at = session.started_at_utc_ms.unwrap_or_else(utc_now_ms);
        let mut bookmarks = Vec::with_capacity(offsets_ns.len());
        for &offset_ns in offsets_ns {
            let start_offset_ns = offset_ns.min(end_ns);
            bookmarks.push(ConversationBookmark {
                bookmark_id: uuid::Uuid::new_v4().to_string(),
                session_id,
                start_offset_ns,
                label: None,
                kind: ConversationBookmarkKind::Mark,
                created_at_utc_ms: started_at.saturating_add(to_i64(start_offset_ns / 1_000_000)?),
            });
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        import_bookmarks_in(&transaction, session_id, &bookmarks)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn conversation_place_review(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<ConversationPlaceReview, StoreError> {
        let connection = self.connection()?;
        let context = super::context::context_in(&connection, session_id)?;
        let Some(location) = context.location.filter(ConversationLocation::is_valid) else {
            return Ok(ConversationPlaceReview {
                session_id,
                location: None,
                sessions: Vec::new(),
            });
        };
        let mut query = connection.prepare(
            "SELECT session.id, session.title, session.started_at_utc_ms, context.location_json
             FROM conversation_context context JOIN meeting_sessions session ON session.id = context.session_id
             WHERE session.id != ?1 AND session.phase != 'deleting' AND context.location_json IS NOT NULL
             ORDER BY session.started_at_utc_ms DESC, session.id",
        )?;
        let mut sessions = Vec::new();
        let mut rows = query.query(params![id(session_id)])?;
        while let Some(row) = rows.next()? {
            let candidate: ConversationLocation = decode_json(&row.get::<_, String>(3)?)?;
            if !candidate.is_valid() || distance_meters(&location, &candidate) > SAME_PLACE_METERS {
                continue;
            }
            let nearby = MeetingSessionId::from_uuid(parse_uuid(&row.get::<_, String>(0)?)?);
            sessions.push(ConversationPlaceSession {
                session_id: nearby,
                title: row.get(1)?,
                started_at_utc_ms: row.get(2)?,
                location: candidate,
                people: super::evening::confirmed_names_in(&connection, nearby)?,
            });
        }
        Ok(ConversationPlaceReview {
            session_id,
            location: Some(location),
            sessions,
        })
    }
}

pub(super) fn export_bookmarks_in(
    connection: &rusqlite::Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<ConversationBookmark>, StoreError> {
    let session = session_row(connection, session_id)?;
    if session.phase == MeetingPhase::Deleting {
        return Err(StoreError::NotFound);
    }
    let mut query = connection.prepare(
        "SELECT bookmark_id, start_offset_ns, label, kind, created_at_utc_ms
         FROM conversation_bookmarks WHERE session_id = ?1 ORDER BY start_offset_ns, bookmark_id",
    )?;
    let rows = query
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|row| {
            Ok(ConversationBookmark {
                bookmark_id: row.0,
                session_id,
                start_offset_ns: from_i64(row.1)?,
                label: row.2,
                kind: ConversationBookmarkKind::parse(&row.3).ok_or(StoreError::Corrupt)?,
                created_at_utc_ms: row.4,
            })
        })
        .collect()
}

pub(super) fn import_bookmarks_in(
    connection: &rusqlite::Transaction<'_>,
    session_id: MeetingSessionId,
    bookmarks: &[ConversationBookmark],
) -> Result<(), StoreError> {
    session_row(connection, session_id)?;
    let mut seen = std::collections::HashSet::with_capacity(bookmarks.len());
    for bookmark in bookmarks {
        if bookmark.session_id != session_id
            || parse_uuid(&bookmark.bookmark_id).is_err()
            || !seen.insert(&bookmark.bookmark_id)
            || bookmark.label.as_ref().is_some_and(|label| {
                label.trim().is_empty()
                    || label.chars().count() > 120
                    || label.chars().any(char::is_control)
            })
        {
            return Err(StoreError::Invalid);
        }
        to_i64(bookmark.start_offset_ns)?;
    }
    for bookmark in bookmarks {
        connection.execute(
            "INSERT INTO conversation_bookmarks(bookmark_id, session_id, start_offset_ns, label, kind, created_at_utc_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![bookmark.bookmark_id, id(session_id), to_i64(bookmark.start_offset_ns)?, bookmark.label, bookmark.kind.as_str(), bookmark.created_at_utc_ms],
        )?;
    }
    Ok(())
}

fn mix_record(
    record: &DurableTrackRecord,
    start: u64,
    end: u64,
    mixed: &mut [f32],
    coverage: &mut [u16],
) -> Result<(), StoreError> {
    let channels = usize::from(record.format.channels);
    if channels == 0
        || record.format.sample_rate_hz == 0
        || !record.samples.len().is_multiple_of(channels)
    {
        return Err(StoreError::Corrupt);
    }
    let from = record.start_offset_ns.max(start);
    let through = record
        .start_offset_ns
        .saturating_add(record.duration_ns)
        .min(end);
    if through <= from {
        return Ok(());
    }
    let first = usize::try_from(((from - start) * u64::from(SAMPLE_RATE)).div_ceil(NS_PER_SECOND))
        .map_err(|_| StoreError::Corrupt)?;
    let last = usize::try_from((through - start) * u64::from(SAMPLE_RATE) / NS_PER_SECOND)
        .map_err(|_| StoreError::Corrupt)?
        .min(mixed.len());
    for index in first..last {
        let offset = start
            + u64::try_from(index).map_err(|_| StoreError::Corrupt)? * NS_PER_SECOND
                / u64::from(SAMPLE_RATE);
        let source = usize::try_from(
            (offset - record.start_offset_ns) * u64::from(record.format.sample_rate_hz)
                / NS_PER_SECOND,
        )
        .map_err(|_| StoreError::Corrupt)?;
        let source_start = source.checked_mul(channels).ok_or(StoreError::Corrupt)?;
        let Some(frame) = record
            .samples
            .get(source_start..source_start.saturating_add(channels))
        else {
            continue;
        };
        let value = frame.iter().copied().sum::<f32>() / f32::from(record.format.channels);
        if !value.is_finite() {
            return Err(StoreError::Corrupt);
        }
        mixed[index] += value;
        coverage[index] = coverage[index].checked_add(1).ok_or(StoreError::Corrupt)?;
    }
    Ok(())
}

fn wav_bytes(samples: &[f32], coverage: &[u16]) -> Result<Vec<u8>, StoreError> {
    let data_size = u32::try_from(samples.len().checked_mul(2).ok_or(StoreError::Invalid)?)
        .map_err(|_| StoreError::Invalid)?;
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    for (sample, count) in samples.iter().zip(coverage) {
        let value = (*sample / f32::from((*count).max(1))).clamp(-1.0, 1.0);
        bytes.extend_from_slice(&((value * f32::from(i16::MAX)) as i16).to_le_bytes());
    }
    Ok(bytes)
}

pub(super) fn distance_meters(left: &ConversationLocation, right: &ConversationLocation) -> f64 {
    let latitude = (right.latitude - left.latitude).to_radians();
    let longitude = (right.longitude - left.longitude).to_radians();
    let angle = (latitude / 2.0).sin().powi(2)
        + left.latitude.to_radians().cos()
            * right.latitude.to_radians().cos()
            * (longitude / 2.0).sin().powi(2);
    6_371_000.0 * 2.0 * angle.clamp(0.0, 1.0).sqrt().asin()
}
