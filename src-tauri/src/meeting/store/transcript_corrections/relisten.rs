use super::scope::contains_phrase;
use super::{
    audio_retained_in, canonical_speaker_ids, confirmed_person_for_speaker_in, editable_phase,
    effective_segments_for_session, from_i64, id, mark_artifacts_out_of_date, parse_uuid,
    phrase_at, session_row, usable_assignment, utc_now_ms, validate_phrases, word_character,
    MeetingStore, StoreError,
};
use crate::meeting::store::{
    context::application_bundle_ids, decode_json, insert_segment_edit_in, meeting_fts_match_query,
    rebuild_search_documents_in, search_projection, speaker_assignment_from_db, to_sql_error,
    StoreMutation,
};
use crate::meeting::transcript_correction_types::{
    TranscriptCorrectionChangedSession, TranscriptRelistenDecision, TranscriptRelistenFix,
    TranscriptRelistenLine, TranscriptRelistenOffer, TranscriptRelistenPiece,
    TranscriptRelistenResult,
};
use crate::meeting::types::{
    EffectiveTranscriptSegment, MeetingCommandKind, MeetingOperationId, MeetingSessionId,
    OperationActor, OperationResult, SaidPerson, SpeakerAssignmentKind, SpeakerId,
    TranscriptSegmentId,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::ops::Range;

const WORD_LIST: &str = "/usr/share/dict/words";
const LINES_SHOWN: usize = 50;
const CLIP_LEAD_NS: u64 = 1_000_000_000;

/// Which other uses of the misheard words a fix may change.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Reach {
    /// Words no dictionary holds, such as a misheard name: every whole use
    /// in the same letter case.
    Everywhere,
    /// Everyday words corrected to someone's name: only a capitalized use
    /// inside a sentence, where the capital marks a name.
    InsideSentences,
    /// Everyday words corrected to anything else: no other line.
    Nowhere,
}

struct Candidate {
    session_id: MeetingSessionId,
    segment_id: TranscriptSegmentId,
    text: String,
    meeting_title: String,
    meeting_started_at_utc_ms: i64,
    preflight_json: String,
    start_offset_ns: u64,
    end_offset_ns: u64,
    speaker_id: SpeakerId,
    assignment: SpeakerAssignmentKind,
}

struct Plan {
    session_id: MeetingSessionId,
    revision: u64,
    segments: Vec<EffectiveTranscriptSegment>,
    edits: Vec<(TranscriptSegmentId, String)>,
}

impl MeetingStore {
    /// The lines of other meetings that still read the way `fix` corrected,
    /// newest meeting first, without the ones already decided for it.
    /// Unavailable when the store cannot be read; an error only when the fix
    /// is not one the owner made.
    pub(crate) fn transcript_relisten_offer(
        &self,
        fix: &TranscriptRelistenFix,
    ) -> Result<TranscriptRelistenOffer, StoreError> {
        let offer = self.connection().and_then(|mut connection| {
            search_projection::refresh_in(&mut connection)?;
            offer_in(&connection, fix)
        });
        match offer {
            Err(
                StoreError::Unavailable
                | StoreError::StorageUnavailable
                | StoreError::EncryptionUnavailable
                | StoreError::Corrupt
                | StoreError::Io,
            ) => Ok(TranscriptRelistenOffer::Unavailable),
            offer => offer,
        }
    }

    /// Applies `fix` to the accepted lines, one revision per meeting, each
    /// edit kept beside the words it replaced and marked with the fix it
    /// came from. Skipped lines are recorded so the fix never offers them
    /// again. Returns the meetings that changed and what is still on offer.
    pub(crate) fn decide_transcript_relisten(
        &self,
        decision: TranscriptRelistenDecision,
    ) -> Result<TranscriptRelistenResult, StoreError> {
        let TranscriptRelistenDecision { fix, accept, skip } = decision;
        let decided_at_utc_ms = utc_now_ms();
        let (fixed_at_utc_ms, plans) = {
            let connection = self.connection()?;
            let fixed_at_utc_ms = fixed_at_in(&connection, &fix)?;
            let reach = reach_in(&connection, &fix)?;
            let mut plans: Vec<Plan> = Vec::new();
            for (target, accepted) in accept
                .iter()
                .map(|target| (target, true))
                .chain(skip.iter().map(|target| (target, false)))
            {
                if target.session_id == fix.session_id {
                    return Err(StoreError::Invalid);
                }
                let index = match plans
                    .iter()
                    .position(|plan| plan.session_id == target.session_id)
                {
                    Some(index) => index,
                    None => {
                        plans.push(plan_in(&connection, target.session_id)?);
                        plans.len() - 1
                    }
                };
                let plan = &mut plans[index];
                let line = plan
                    .segments
                    .iter()
                    .find(|segment| {
                        segment.base.segment_id == target.segment_id && !segment.removed
                    })
                    .ok_or(StoreError::NotFound)?;
                if !accepted
                    || plan
                        .edits
                        .iter()
                        .any(|(segment_id, _)| *segment_id == target.segment_id)
                {
                    continue;
                }
                let text = line.replacement_text.as_deref().unwrap_or(&line.base.text);
                let spots = spots(text, &fix.original_phrase, reach);
                if !spots.is_empty() {
                    let fixed = replaced(text, &spots, &fix.replacement_phrase);
                    plan.edits.push((target.segment_id, fixed));
                }
            }
            (fixed_at_utc_ms, plans)
        };
        if !skip.is_empty() {
            let mut connection = self.connection()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for target in &skip {
                insert_decision_in(
                    &transaction,
                    &fix,
                    target.segment_id,
                    None,
                    fixed_at_utc_ms,
                    decided_at_utc_ms,
                )?;
            }
            transaction.commit()?;
        }
        let mut changed_sessions = Vec::new();
        for plan in plans.into_iter().filter(|plan| !plan.edits.is_empty()) {
            let receipt = self.edit_session(
                StoreMutation {
                    operation_id: MeetingOperationId::new(),
                    actor: OperationActor::User,
                    requested_at_utc_ms: decided_at_utc_ms,
                    session_id: plan.session_id,
                    expected_revision: plan.revision,
                    command: MeetingCommandKind::SegmentEdit,
                },
                "segment_edited",
                |transaction| {
                    for (segment_id, text) in &plan.edits {
                        let sequence = insert_segment_edit_in(
                            transaction,
                            plan.session_id,
                            *segment_id,
                            text,
                            false,
                        )?;
                        insert_decision_in(
                            transaction,
                            &fix,
                            *segment_id,
                            Some(sequence),
                            fixed_at_utc_ms,
                            decided_at_utc_ms,
                        )?;
                    }
                    mark_artifacts_out_of_date(transaction, plan.session_id)?;
                    rebuild_search_documents_in(transaction, plan.session_id)
                },
            )?;
            if let (OperationResult::Committed, Some(revision)) =
                (receipt.result, receipt.new_revision)
            {
                changed_sessions.push(TranscriptCorrectionChangedSession {
                    session_id: plan.session_id,
                    revision,
                });
            }
        }
        Ok(TranscriptRelistenResult {
            changed_sessions,
            offer: self.transcript_relisten_offer(&fix)?,
        })
    }
}

fn offer_in(
    connection: &Connection,
    fix: &TranscriptRelistenFix,
) -> Result<TranscriptRelistenOffer, StoreError> {
    let fixed_at_utc_ms = fixed_at_in(connection, fix)?;
    let reach = reach_in(connection, fix)?;
    let fixed_meeting_started: i64 = connection.query_row(
        "SELECT COALESCE(started_at_utc_ms, created_at_utc_ms) FROM meeting_sessions WHERE id = ?1",
        params![id(fix.session_id)],
        |row| row.get(0),
    )?;
    let mut total = 0_u32;
    let mut all_older = true;
    let mut lines = Vec::new();
    let mut speakers = HashMap::new();
    for candidate in candidates_in(connection, fix, reach)? {
        let spots = spots(&candidate.text, &fix.original_phrase, reach);
        if spots.is_empty() {
            continue;
        }
        total = total.saturating_add(1);
        all_older &= candidate.meeting_started_at_utc_ms < fixed_meeting_started;
        if lines.len() < LINES_SHOWN {
            lines.push(line_in(connection, &mut speakers, candidate, &spots)?);
        }
    }
    Ok(TranscriptRelistenOffer::Available {
        fixed_at_utc_ms,
        total,
        all_older,
        lines,
    })
}

/// When the owner made `fix`: the first edit of its line that reads the new
/// words, or the phrase he taught from that line, whichever came first. The
/// line must still read the new words where the transcriber wrote the old.
fn fixed_at_in(connection: &Connection, fix: &TranscriptRelistenFix) -> Result<i64, StoreError> {
    validate_phrases(&fix.original_phrase, &fix.replacement_phrase)?;
    let line = effective_segments_for_session(connection, fix.session_id)?
        .into_iter()
        .find(|segment| segment.base.segment_id == fix.segment_id && !segment.removed)
        .ok_or(StoreError::NotFound)?;
    let reads = line.replacement_text.as_deref().unwrap_or(&line.base.text);
    if !contains_phrase(&line.base.text, &fix.original_phrase)
        || !contains_phrase(reads, &fix.replacement_phrase)
    {
        return Err(StoreError::NotFound);
    }
    let mut statement = connection.prepare(
        "SELECT replacement_text, operator_at_utc_ms FROM meeting_segment_edits
         WHERE segment_id = ?1 AND removed = 0 ORDER BY edit_sequence",
    )?;
    let edits = statement.query_map(params![id(fix.segment_id)], |row| {
        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut edited = None;
    for edit in edits {
        let (text, at) = edit?;
        if text.is_some_and(|text| contains_phrase(&text, &fix.replacement_phrase)) {
            edited = Some(at);
            break;
        }
    }
    let taught: Option<i64> = connection.query_row(
        "SELECT MIN(created_at_utc_ms) FROM person_transcript_corrections
         WHERE source_segment_id = ?1 AND original_phrase = ?2 AND replacement_phrase = ?3",
        params![
            id(fix.segment_id),
            fix.original_phrase,
            fix.replacement_phrase
        ],
        |row| row.get(0),
    )?;
    edited
        .into_iter()
        .chain(taught)
        .min()
        .ok_or(StoreError::NotFound)
}

fn reach_in(connection: &Connection, fix: &TranscriptRelistenFix) -> Result<Reach, StoreError> {
    if !everyday(&fix.original_phrase)? {
        return Ok(Reach::Everywhere);
    }
    let capitalized = fix
        .original_phrase
        .chars()
        .next()
        .is_some_and(char::is_uppercase);
    Ok(
        if capitalized && known_name_in(connection, &fix.replacement_phrase)? {
            Reach::InsideSentences
        } else {
            Reach::Nowhere
        },
    )
}

/// Whether every word of `phrase` is an everyday word: one the system word
/// list holds in lower case. A misheard name rarely is; "will" and "grace"
/// are, and a fix to them must not spread to every sentence that uses them.
fn everyday(phrase: &str) -> Result<bool, StoreError> {
    let list = std::fs::read_to_string(WORD_LIST)?;
    Ok(phrase
        .split(|character: char| !word_character(character))
        .filter(|word| !word.is_empty())
        .all(|word| {
            let word = word.to_lowercase();
            list.lines().any(|entry| entry == word)
        }))
}

/// Whether someone in People goes by `name`: their whole name, one word of
/// it, or an alias.
fn known_name_in(connection: &Connection, name: &str) -> Result<bool, StoreError> {
    let mut statement = connection.prepare("SELECT display_name, aliases_json FROM persons")?;
    let people = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for person in people {
        let (display_name, aliases_json) = person?;
        if display_name == name
            || display_name.split_whitespace().any(|word| word == name)
            || decode_json::<Vec<String>>(&aliases_json)?
                .iter()
                .any(|alias| alias == name)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Where `phrase` stands in `text` as whole words that `reach` lets the fix
/// change.
fn spots(text: &str, phrase: &str, reach: Reach) -> Vec<Range<usize>> {
    text.match_indices(phrase)
        .map(|(start, _)| start..start + phrase.len())
        .filter(|spot| phrase_at(text, spot.start, phrase))
        .filter(|spot| match reach {
            Reach::Everywhere => true,
            Reach::InsideSentences => !opens_sentence(&text[..spot.start]),
            Reach::Nowhere => false,
        })
        .collect()
}

/// Whether the text after `before` begins a sentence.
fn opens_sentence(before: &str) -> bool {
    before
        .trim_end()
        .chars()
        .next_back()
        .is_none_or(|character| matches!(character, '.' | '?' | '!' | '…'))
}

fn pieces(text: &str, spots: &[Range<usize>]) -> Vec<TranscriptRelistenPiece> {
    let mut pieces = Vec::with_capacity(spots.len() * 2 + 1);
    let mut copied = 0;
    for spot in spots {
        if copied < spot.start {
            pieces.push(TranscriptRelistenPiece {
                text: text[copied..spot.start].to_owned(),
                misheard: false,
            });
        }
        pieces.push(TranscriptRelistenPiece {
            text: text[spot.clone()].to_owned(),
            misheard: true,
        });
        copied = spot.end;
    }
    if copied < text.len() {
        pieces.push(TranscriptRelistenPiece {
            text: text[copied..].to_owned(),
            misheard: false,
        });
    }
    pieces
}

fn replaced(text: &str, spots: &[Range<usize>], replacement: &str) -> String {
    pieces(text, spots)
        .iter()
        .map(|piece| {
            if piece.misheard {
                replacement
            } else {
                piece.text.as_str()
            }
        })
        .collect()
}

fn candidates_in(
    connection: &Connection,
    fix: &TranscriptRelistenFix,
    reach: Reach,
) -> Result<Vec<Candidate>, StoreError> {
    if reach == Reach::Nowhere {
        return Ok(Vec::new());
    }
    let Some(match_query) = meeting_fts_match_query(&fix.original_phrase) else {
        return Ok(Vec::new());
    };
    let mut statement = connection.prepare(
        "SELECT d.session_id, d.entity_id, d.content, m.title,
                COALESCE(m.started_at_utc_ms, m.created_at_utc_ms), m.preflight_json,
                s.start_offset_ns, s.end_offset_ns,
                COALESCE(a.speaker_id, s.speaker_id),
                COALESCE(a.assignment_kind,
                    CASE t.source_kind WHEN 'microphone' THEN 'local_speaker' ELSE 'unknown' END)
           FROM meeting_search_fts
           JOIN meeting_search_documents d ON d.id = meeting_search_fts.rowid
           JOIN meeting_sessions m ON m.id = d.session_id
           JOIN meeting_transcript_segments s
             ON s.segment_id = d.entity_id
            AND s.transcript_revision_id = m.current_transcript_revision_id
           JOIN meeting_source_tracks t ON t.track_id = s.track_id
           LEFT JOIN meeting_diarization_assignments a
             ON a.generation_id = m.current_diarization_generation_id
            AND a.segment_id = s.segment_id
          WHERE meeting_search_fts MATCH ?1
            AND d.entity_kind = 'segment'
            AND d.session_id != ?2
            AND m.phase IN ('review_ready', 'recovery_required')
            AND m.transcript_purged_at_utc_ms IS NULL
            AND NOT EXISTS (
                SELECT 1 FROM transcript_relisten_decisions decision
                 WHERE decision.segment_id = s.segment_id
                   AND decision.original_phrase = ?3
                   AND decision.replacement_phrase = ?4)
          ORDER BY COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) DESC, m.id,
                   s.start_offset_ns, s.ordinal",
    )?;
    let rows = statement.query_map(
        params![
            match_query,
            id(fix.session_id),
            fix.original_phrase,
            fix.replacement_phrase
        ],
        |row| {
            Ok(Candidate {
                session_id: MeetingSessionId::from_uuid(
                    parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
                ),
                segment_id: TranscriptSegmentId::from_uuid(
                    parse_uuid(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
                ),
                text: row.get(2)?,
                meeting_title: row.get(3)?,
                meeting_started_at_utc_ms: row.get(4)?,
                preflight_json: row.get(5)?,
                start_offset_ns: from_i64(row.get(6)?).map_err(to_sql_error)?,
                end_offset_ns: from_i64(row.get(7)?).map_err(to_sql_error)?,
                speaker_id: SpeakerId::from_uuid(
                    parse_uuid(&row.get::<_, String>(8)?).map_err(to_sql_error)?,
                ),
                assignment: speaker_assignment_from_db(&row.get::<_, String>(9)?)
                    .map_err(to_sql_error)?,
            })
        },
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn line_in(
    connection: &Connection,
    speakers: &mut HashMap<MeetingSessionId, HashMap<SpeakerId, SpeakerId>>,
    candidate: Candidate,
    spots: &[Range<usize>],
) -> Result<TranscriptRelistenLine, StoreError> {
    let person = if usable_assignment(candidate.assignment) {
        let canonical = match speakers.entry(candidate.session_id) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                entry.insert(canonical_speaker_ids(connection, candidate.session_id)?)
            }
        };
        let speaker_id = canonical
            .get(&candidate.speaker_id)
            .copied()
            .unwrap_or(candidate.speaker_id);
        confirmed_person_in(connection, candidate.session_id, speaker_id)?
    } else {
        None
    };
    Ok(TranscriptRelistenLine {
        app_bundle_id: application_bundle_ids(&candidate.preflight_json)?
            .into_iter()
            .next(),
        audio_retained: audio_retained_in(connection, candidate.session_id, candidate.segment_id)?,
        clip_start_offset_ns: clip_start(&candidate, spots)?,
        pieces: pieces(&candidate.text, spots),
        person,
        session_id: candidate.session_id,
        segment_id: candidate.segment_id,
        meeting_title: candidate.meeting_title,
        meeting_started_at_utc_ms: candidate.meeting_started_at_utc_ms,
    })
}

fn confirmed_person_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
) -> Result<Option<SaidPerson>, StoreError> {
    let Some(person_id) = confirmed_person_for_speaker_in(connection, session_id, speaker_id)?
    else {
        return Ok(None);
    };
    Ok(connection
        .query_row(
            "SELECT display_name FROM persons WHERE id = ?1",
            params![id(person_id)],
            |row| row.get(0),
        )
        .optional()?
        .map(|display_name| SaidPerson {
            person_id,
            display_name,
        }))
}

/// One second before the first misheard words, placed by their share of the
/// line's characters.
fn clip_start(candidate: &Candidate, spots: &[Range<usize>]) -> Result<u64, StoreError> {
    let before = spots
        .first()
        .map_or(0, |spot| candidate.text[..spot.start].chars().count());
    let length = candidate.text.chars().count().max(1);
    let span = candidate
        .end_offset_ns
        .saturating_sub(candidate.start_offset_ns);
    let into = span / u64::try_from(length).map_err(|_| StoreError::Corrupt)?
        * u64::try_from(before).map_err(|_| StoreError::Corrupt)?;
    Ok(candidate
        .start_offset_ns
        .saturating_add(into)
        .saturating_sub(CLIP_LEAD_NS))
}

/// The meeting's lines as they read now, refused when it can no longer be
/// edited from review.
fn plan_in(connection: &Connection, session_id: MeetingSessionId) -> Result<Plan, StoreError> {
    let session = session_row(connection, session_id)?;
    if !editable_phase(session.phase) || session.transcript_purged_at_utc_ms.is_some() {
        return Err(StoreError::Conflict);
    }
    Ok(Plan {
        session_id,
        revision: session.revision,
        segments: effective_segments_for_session(connection, session_id)?,
        edits: Vec::new(),
    })
}

fn insert_decision_in(
    transaction: &Transaction<'_>,
    fix: &TranscriptRelistenFix,
    segment_id: TranscriptSegmentId,
    edit_sequence: Option<i64>,
    fixed_at_utc_ms: i64,
    decided_at_utc_ms: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO transcript_relisten_decisions (
            segment_id, original_phrase, replacement_phrase, accepted, edit_sequence,
            source_session_id, source_segment_id, fixed_at_utc_ms, decided_at_utc_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (segment_id, original_phrase, replacement_phrase) DO UPDATE SET
            accepted = excluded.accepted, edit_sequence = excluded.edit_sequence,
            source_session_id = excluded.source_session_id,
            source_segment_id = excluded.source_segment_id,
            fixed_at_utc_ms = excluded.fixed_at_utc_ms,
            decided_at_utc_ms = excluded.decided_at_utc_ms
         WHERE excluded.accepted = 1",
        params![
            id(segment_id),
            fix.original_phrase,
            fix.replacement_phrase,
            i64::from(edit_sequence.is_some()),
            edit_sequence,
            id(fix.session_id),
            id(fix.segment_id),
            fixed_at_utc_ms,
            decided_at_utc_ms
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::super::workflow_core_tests::store;
    use super::*;
    use crate::meeting::store::SegmentEdit;
    use crate::meeting::transcript_correction_types::TranscriptRelistenTarget;
    use uuid::Uuid;

    /// A finished meeting started at `started_at_utc_ms` whose microphone
    /// track holds `lines`, one line each, all said by one speaker.
    fn meeting(
        store: &MeetingStore,
        started_at_utc_ms: i64,
        lines: &[&str],
    ) -> Result<(MeetingSessionId, Vec<TranscriptSegmentId>), StoreError> {
        let session_id = MeetingSessionId::new();
        let speaker_id = SpeakerId::new();
        let plan_id = Uuid::new_v4().to_string();
        let track_id = Uuid::new_v4().to_string();
        let revision_id = Uuid::new_v4().to_string();
        let connection = store.connection()?;
        connection.execute(
            "INSERT INTO meeting_sessions (
                id, phase, revision, title, origin_kind, preflight_json, created_at_utc_ms,
                started_at_utc_ms, processing_status, retention_policy_json
             ) VALUES (?1, 'review_ready', 0, 'Budget sync', '\"manual\"', '{}', ?2, ?2,
                       '{\"kind\":\"succeeded\"}', '{\"kind\":\"keep_forever\"}')",
            params![id(session_id), started_at_utc_ms],
        )?;
        connection.execute(
            "INSERT INTO meeting_run_plans (
                plan_id, session_id, attempt_number, schema_version, consent_id,
                canonical_plan_json, created_at_utc_ms
             ) VALUES (?1, ?2, 1, 1, ?3, '{}', 1)",
            params![plan_id, id(session_id), Uuid::new_v4().to_string()],
        )?;
        connection.execute(
            "INSERT INTO meeting_source_tracks (
                track_id, session_id, plan_id, source_kind, required, requested,
                descriptor_json, timestamp_bridge_json, health
             ) VALUES (?1, ?2, ?3, 'microphone', 1, 1, '{}', '{}', '\"healthy\"')",
            params![track_id, id(session_id), plan_id],
        )?;
        connection.execute(
            "INSERT INTO meeting_transcript_revisions (
                transcript_revision_id, session_id, engine_id, destination_json,
                source_set_json, language, state, created_at_utc_ms
             ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 1)",
            params![revision_id, id(session_id)],
        )?;
        connection.execute(
            "INSERT INTO meeting_speakers (speaker_id, session_id, source_kind, display_name, revision)
             VALUES (?1, ?2, 'microphone', 'Speaker 1', 0)",
            params![id(speaker_id), id(session_id)],
        )?;
        let mut segments = Vec::with_capacity(lines.len());
        for (ordinal, text) in lines.iter().enumerate() {
            let segment_id = TranscriptSegmentId::new();
            let ordinal = i64::try_from(ordinal).map_err(|_| StoreError::Invalid)?;
            connection.execute(
                "INSERT INTO meeting_transcript_segments (
                    segment_id, transcript_revision_id, track_id, ordinal,
                    start_offset_ns, end_offset_ns, speaker_id, base_text
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id(segment_id),
                    revision_id,
                    track_id,
                    ordinal,
                    ordinal * 4_000_000_000,
                    ordinal * 4_000_000_000 + 3_000_000_000,
                    id(speaker_id),
                    text
                ],
            )?;
            segments.push(segment_id);
        }
        connection.execute(
            "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
            params![revision_id, id(session_id)],
        )?;
        Ok((session_id, segments))
    }

    fn lines_of(
        store: &MeetingStore,
        session_id: MeetingSessionId,
    ) -> Result<Vec<(String, Option<String>)>, StoreError> {
        let connection = store.connection()?;
        Ok(effective_segments_for_session(&connection, session_id)?
            .into_iter()
            .map(|segment| (segment.base.text, segment.replacement_text))
            .collect())
    }

    #[test]
    fn a_fixed_name_reaches_whole_words_in_older_meetings_once_each() -> Result<(), StoreError> {
        let (_directory, store) = store();
        let (older, older_lines) = meeting(&store, 1_000, &["Mira said the deck is ready."])?;
        let (middle, middle_lines) = meeting(
            &store,
            2_000,
            &[
                "Ask Mira about the budget.",
                "Miracles happen when the deck ships.",
            ],
        )?;
        let (newest, newest_lines) = meeting(&store, 3_000, &["Mira will send the deck."])?;
        store.edit_segment(SegmentEdit {
            mutation: StoreMutation {
                operation_id: MeetingOperationId::new(),
                actor: OperationActor::User,
                requested_at_utc_ms: 3_000,
                session_id: newest,
                expected_revision: 0,
                command: MeetingCommandKind::SegmentEdit,
            },
            segment_id: newest_lines[0],
            replacement_text: "Meera will send the deck.".to_owned(),
            removed: false,
        })?;
        let fixed_at_utc_ms: i64 = store.connection()?.query_row(
            "SELECT operator_at_utc_ms FROM meeting_segment_edits WHERE segment_id = ?1",
            params![id(newest_lines[0])],
            |row| row.get(0),
        )?;
        let fix = TranscriptRelistenFix {
            session_id: newest,
            segment_id: newest_lines[0],
            original_phrase: "Mira".to_owned(),
            replacement_phrase: "Meera".to_owned(),
        };

        let TranscriptRelistenOffer::Available {
            fixed_at_utc_ms: offered_fixed_at,
            total,
            all_older,
            lines,
        } = store.transcript_relisten_offer(&fix)?
        else {
            return Err(StoreError::Unavailable);
        };

        assert_eq!(
            (offered_fixed_at, total, all_older),
            (fixed_at_utc_ms, 2, true)
        );
        assert_eq!(
            lines
                .iter()
                .map(|line| (line.session_id, line.segment_id))
                .collect::<Vec<_>>(),
            vec![(middle, middle_lines[0]), (older, older_lines[0])]
        );
        assert_eq!(
            lines[0].pieces,
            vec![
                TranscriptRelistenPiece {
                    text: "Ask ".to_owned(),
                    misheard: false
                },
                TranscriptRelistenPiece {
                    text: "Mira".to_owned(),
                    misheard: true
                },
                TranscriptRelistenPiece {
                    text: " about the budget.".to_owned(),
                    misheard: false
                },
            ]
        );

        let result = store.decide_transcript_relisten(TranscriptRelistenDecision {
            fix,
            accept: vec![TranscriptRelistenTarget {
                session_id: middle,
                segment_id: middle_lines[0],
            }],
            skip: vec![TranscriptRelistenTarget {
                session_id: older,
                segment_id: older_lines[0],
            }],
        })?;

        assert_eq!(
            result
                .changed_sessions
                .iter()
                .map(|changed| changed.session_id)
                .collect::<Vec<_>>(),
            vec![middle]
        );
        assert_eq!(
            result.offer,
            TranscriptRelistenOffer::Available {
                fixed_at_utc_ms,
                total: 0,
                all_older: true,
                lines: Vec::new(),
            }
        );
        assert_eq!(
            lines_of(&store, middle)?,
            vec![
                (
                    "Ask Mira about the budget.".to_owned(),
                    Some("Ask Meera about the budget.".to_owned())
                ),
                ("Miracles happen when the deck ships.".to_owned(), None),
            ]
        );
        assert_eq!(
            lines_of(&store, older)?,
            vec![("Mira said the deck is ready.".to_owned(), None)]
        );
        let provenance: (String, String, i64) = store.connection()?.query_row(
            "SELECT source_session_id, source_segment_id, fixed_at_utc_ms
             FROM transcript_relisten_decisions WHERE segment_id = ?1 AND accepted = 1",
            params![id(middle_lines[0])],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            provenance,
            (id(newest), id(newest_lines[0]), fixed_at_utc_ms)
        );
        Ok(())
    }
}
