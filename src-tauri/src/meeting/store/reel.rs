//! Reels: the recorded moments behind a person or a promise, heard one after
//! another, oldest first.

use super::loops::{carry_chain_in, ledger_loop_seeds, readable_ledger_in, rows_from_seeds_in};
use super::people::{loop_concerns_person, person_by_id_in};
use super::said::SessionVoices;
use super::transcript_corrections::audio_retained_in;
use super::{effective_segments_for_session, id, parse_uuid, MeetingStore, StoreError};
use crate::meeting::conversation_review::{ConversationReel, ReelMoment, ReelMomentKind};
use crate::meeting::ledger::LedgerThreadState;
use crate::meeting::loop_types::{
    MeetingLoopDirection, MeetingLoopId, MeetingLoopKind, MeetingLoopStatus,
};
use crate::meeting::people_types::PersonId;
use crate::meeting::types::{
    EffectiveTranscriptSegment, MeetingSessionId, SaidMoment, TranscriptSegmentId,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;

const NS_PER_SECOND: u64 = 1_000_000_000;
/// How many of a person's latest meetings their reel reaches back through.
const PERSON_MEETINGS: i64 = 3;
/// A person reel's most moments, and its most audio.
const PERSON_MOMENTS_MAX: usize = 8;
const PERSON_PLAYABLE_NS_MAX: u64 = 120 * NS_PER_SECOND;
/// Below this many moments a person reel adds their own latest lines.
const PERSON_MOMENTS_MIN: usize = 3;
/// What plays around a line: a breath before it, a beat after it, and never
/// more than `MOMENT_NS_MAX` in all.
const LEAD_NS: u64 = 600_000_000;
const TAIL_NS: u64 = 400_000_000;
const MOMENT_NS_MAX: u64 = 20 * NS_PER_SECOND;

impl MeetingStore {
    /// One person's story across their latest meetings: what they or the user
    /// promised or were asked, what was decided, then the questions still
    /// open, each at its first cited line. Fewer than three moments adds the
    /// person's own latest lines. A speaker is named only from a confirmed
    /// identification.
    pub(crate) fn conversation_reel_for_person(
        &self,
        person_id: PersonId,
        user_name: Option<&str>,
    ) -> Result<ConversationReel, StoreError> {
        let connection = self.connection()?;
        let person = person_by_id_in(&connection, person_id)?;
        let mut sessions = Vec::new();
        for session_id in latest_meetings_in(&connection, person_id)? {
            if let Some(lines) = SessionLines::read_in(&connection, session_id, user_name)? {
                sessions.push(lines);
            }
        }
        let mut concerning = Vec::new();
        let mut decisions = Vec::new();
        let mut open_questions = Vec::new();
        for (session, lines) in sessions.iter().enumerate() {
            let Some((_, ledger)) = readable_ledger_in(&connection, lines.session_id)? else {
                continue;
            };
            let rows = rows_from_seeds_in(
                &connection,
                lines.session_id,
                ledger_loop_seeds(lines.session_id, &ledger),
                user_name,
            )?;
            for row in rows {
                let picks = if loop_concerns_person(&person, &row) {
                    &mut concerning
                } else if matches!(row.kind, MeetingLoopKind::Loop) && row.is_open() {
                    &mut open_questions
                } else {
                    continue;
                };
                if let Some(citation) = row.citations.first() {
                    picks.push(Pick {
                        session,
                        segment_id: citation.segment_id,
                        kind: moment_kind(row.kind),
                        direction: Some(row.direction),
                        status: Some(row.status),
                    });
                }
            }
            for thread in ledger.threads.iter().filter(|thread| {
                thread.substantive
                    && matches!(
                        thread.state,
                        LedgerThreadState::Decided | LedgerThreadState::Agreed
                    )
            }) {
                if let Some(citation) = thread.receipt.citations.first() {
                    decisions.push(Pick {
                        session,
                        segment_id: citation.segment_id,
                        kind: ReelMomentKind::Decision,
                        direction: None,
                        status: None,
                    });
                }
            }
        }
        let mut reel = Reel::default();
        for pick in concerning.iter().chain(&decisions).chain(&open_questions) {
            if reel.moments.len() == PERSON_MOMENTS_MAX {
                break;
            }
            let lines = &mut sessions[pick.session];
            let Some(index) = lines.position(pick.segment_id) else {
                continue;
            };
            if let Some(moment) =
                lines.moment_at(&connection, index, pick.kind, pick.direction, pick.status)?
            {
                reel.offer(moment);
            }
        }
        'lines: for lines in &mut sessions {
            for index in (0..lines.segments.len()).rev() {
                if reel.moments.len() >= PERSON_MOMENTS_MIN {
                    break 'lines;
                }
                if reel.holds(lines.segments[index].base.segment_id)
                    || !lines.said_by(&connection, index, person_id)?
                {
                    continue;
                }
                if let Some(moment) =
                    lines.moment_at(&connection, index, ReelMomentKind::Line, None, None)?
                {
                    reel.offer(moment);
                }
            }
        }
        Ok(reel.into_reel())
    }

    /// Every cited line of one promise or question across the meetings it was
    /// carried through, oldest first.
    pub(crate) fn conversation_reel_for_loop(
        &self,
        loop_id: &MeetingLoopId,
        user_name: Option<&str>,
    ) -> Result<ConversationReel, StoreError> {
        loop_id.session_id().ok_or(StoreError::Invalid)?;
        let connection = self.connection()?;
        let mut reel = Reel::default();
        for occurrence in carry_chain_in(&connection, loop_id)? {
            let Some(session_id) = occurrence.session_id() else {
                continue;
            };
            let row = match readable_ledger_in(&connection, session_id)? {
                Some((_, ledger)) => rows_from_seeds_in(
                    &connection,
                    session_id,
                    ledger_loop_seeds(session_id, &ledger),
                    user_name,
                )?
                .into_iter()
                .find(|row| row.loop_id == occurrence),
                None => None,
            };
            let (Some(row), Some(mut lines)) = (
                row,
                SessionLines::read_in(&connection, session_id, user_name)?,
            ) else {
                if occurrence == *loop_id {
                    return Err(StoreError::NotFound);
                }
                continue;
            };
            for citation in &row.citations {
                if reel.holds(citation.segment_id) {
                    continue;
                }
                let Some(index) = lines.position(citation.segment_id) else {
                    continue;
                };
                if let Some(moment) = lines.moment_at(
                    &connection,
                    index,
                    moment_kind(row.kind),
                    Some(row.direction),
                    Some(row.status),
                )? {
                    reel.add(moment);
                }
            }
        }
        Ok(reel.into_reel())
    }
}

/// A cited line a person reel may take, before its moment is read.
struct Pick {
    session: usize,
    segment_id: TranscriptSegmentId,
    kind: ReelMomentKind,
    direction: Option<MeetingLoopDirection>,
    status: Option<MeetingLoopStatus>,
}

#[derive(Default)]
struct Reel {
    moments: Vec<ReelMoment>,
    segments: HashSet<TranscriptSegmentId>,
}

impl Reel {
    fn holds(&self, segment_id: TranscriptSegmentId) -> bool {
        self.segments.contains(&segment_id)
    }

    fn add(&mut self, moment: ReelMoment) {
        if self.segments.insert(moment.said.segment_id) {
            self.moments.push(moment);
        }
    }

    /// Adds a moment while a person reel has room for it. A moment whose
    /// audio is gone takes a place but no time.
    fn offer(&mut self, moment: ReelMoment) {
        let playable: u64 = self.moments.iter().map(playable_ns).sum();
        if self.moments.len() < PERSON_MOMENTS_MAX
            && playable.saturating_add(playable_ns(&moment)) <= PERSON_PLAYABLE_NS_MAX
        {
            self.add(moment);
        }
    }

    fn into_reel(mut self) -> ConversationReel {
        self.moments.sort_by_key(|moment| {
            (
                moment.said.meeting_started_at_utc_ms,
                moment.said.session_id.uuid(),
                moment.said.start_offset_ns,
            )
        });
        ConversationReel {
            moments: self.moments,
        }
    }
}

fn playable_ns(moment: &ReelMoment) -> u64 {
    if moment.said.audio_retained {
        moment
            .play_end_offset_ns
            .saturating_sub(moment.play_start_offset_ns)
    } else {
        0
    }
}

const fn moment_kind(kind: MeetingLoopKind) -> ReelMomentKind {
    match kind {
        MeetingLoopKind::Commitment => ReelMomentKind::Promise,
        MeetingLoopKind::Loop => ReelMomentKind::Question,
    }
}

/// The person's latest meetings they are confirmed in, newest first, while
/// their transcripts are kept.
fn latest_meetings_in(
    connection: &Connection,
    person_id: PersonId,
) -> Result<Vec<MeetingSessionId>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT m.id
           FROM meeting_person_links l
           JOIN meeting_sessions m ON m.id = l.meeting_id
          WHERE l.person_id = ?1 AND l.confidence = 'confirmed'
            AND m.phase != 'deleting' AND m.transcript_purged_at_utc_ms IS NULL
          ORDER BY COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) DESC, m.id DESC
          LIMIT ?2",
    )?;
    let ids = statement
        .query_map(
            params![person_id.uuid().to_string(), PERSON_MEETINGS],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    ids.iter()
        .map(|value| parse_uuid(value).map(MeetingSessionId::from_uuid))
        .collect()
}

/// One meeting's transcript lines as they read now, and who said them.
struct SessionLines {
    session_id: MeetingSessionId,
    title: String,
    started_at_utc_ms: i64,
    segments: Vec<EffectiveTranscriptSegment>,
    voices: SessionVoices,
}

impl SessionLines {
    /// `None` once the meeting is being deleted or its transcript is gone.
    fn read_in(
        connection: &Connection,
        session_id: MeetingSessionId,
        user_name: Option<&str>,
    ) -> Result<Option<Self>, StoreError> {
        let header: Option<(String, i64)> = connection
            .query_row(
                "SELECT title, COALESCE(started_at_utc_ms, created_at_utc_ms)
                   FROM meeting_sessions
                  WHERE id = ?1 AND phase != 'deleting' AND transcript_purged_at_utc_ms IS NULL",
                params![id(session_id)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((title, started_at_utc_ms)) = header else {
            return Ok(None);
        };
        Ok(Some(Self {
            session_id,
            title,
            started_at_utc_ms,
            segments: effective_segments_for_session(connection, session_id)?,
            voices: SessionVoices::read_in(connection, session_id, user_name)?,
        }))
    }

    fn position(&self, segment_id: TranscriptSegmentId) -> Option<usize> {
        self.segments
            .iter()
            .position(|segment| segment.base.segment_id == segment_id)
    }

    fn said_by(
        &mut self,
        connection: &Connection,
        index: usize,
        person_id: PersonId,
    ) -> Result<bool, StoreError> {
        let segment = &self.segments[index];
        let voice = self.voices.voice_in(
            connection,
            self.session_id,
            segment.assigned_speaker_id,
            segment.speaker_assignment,
        )?;
        Ok(voice
            .person
            .is_some_and(|person| person.person_id == person_id))
    }

    /// The line at `index` as a reel moment, or `None` when it was removed or
    /// says nothing.
    fn moment_at(
        &mut self,
        connection: &Connection,
        index: usize,
        kind: ReelMomentKind,
        direction: Option<MeetingLoopDirection>,
        status: Option<MeetingLoopStatus>,
    ) -> Result<Option<ReelMoment>, StoreError> {
        let segment = &self.segments[index];
        let text = segment
            .replacement_text
            .as_deref()
            .unwrap_or(&segment.base.text)
            .trim();
        if segment.removed || text.is_empty() {
            return Ok(None);
        }
        let voice = self.voices.voice_in(
            connection,
            self.session_id,
            segment.assigned_speaker_id,
            segment.speaker_assignment,
        )?;
        let play_start_offset_ns = segment.base.start_offset_ns.saturating_sub(LEAD_NS);
        Ok(Some(ReelMoment {
            said: SaidMoment {
                session_id: self.session_id,
                segment_id: segment.base.segment_id,
                meeting_title: self.title.clone(),
                meeting_started_at_utc_ms: self.started_at_utc_ms,
                start_offset_ns: segment.base.start_offset_ns,
                end_offset_ns: segment.base.end_offset_ns,
                text: text.to_string(),
                speaker_label: voice.label,
                person: voice.person,
                audio_retained: audio_retained_in(
                    connection,
                    self.session_id,
                    segment.base.segment_id,
                )?,
            },
            kind,
            direction,
            status,
            play_start_offset_ns,
            play_end_offset_ns: segment
                .base
                .end_offset_ns
                .saturating_add(TAIL_NS)
                .min(play_start_offset_ns.saturating_add(MOMENT_NS_MAX)),
        }))
    }
}
