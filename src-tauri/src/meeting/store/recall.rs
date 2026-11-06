//! What the words a running capture just heard were already said in.
//!
//! One read for the live screen, with no model in it: a few distinctive terms
//! out of the last minute of recognized words, the past lines the search
//! index holds for them, and what each line's own meeting made of it.

use super::context::application_bundle_ids;
use super::loops::{current_ledger_in, ledger_loop_seeds, states_in};
use super::people::confirmed_person_ids_in;
use super::said::{matched_lines_in, MatchedLine, SessionVoices};
use super::{id, search_projection, MeetingStore, StoreError};
use crate::meeting::ledger::LedgerThreadState;
use crate::meeting::people_types::PersonId;
use crate::meeting::types::{
    MeetingRecallKind, MeetingRecallMatch, MeetingSessionId, TranscriptSegmentId,
};
use crate::query::tools::STOPWORDS;
use rusqlite::{params, Connection};
use std::cmp::Reverse;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

/// Cards the live screen holds at once, one per past meeting.
const MATCHES_MAX: usize = 3;
/// Terms searched on one read. Each is one index lookup.
const TERMS_MAX: usize = 4;
/// Candidates whose reach through past meetings one read counts.
const CANDIDATES_MAX: usize = 48;
/// Past lines read back for one term before they are ranked.
const LINES_PER_TERM: usize = 12;
/// A term on more than one past line in this many is everyday talk here,
/// however rare it is in English...
const COMMON_LINE_SHARE: usize = 50;
/// ...though no term on this few lines is.
const COMMON_LINES_FLOOR: usize = 3;
/// A term said in more than half the past meetings is the corpus's own
/// vocabulary, once there are this many meetings to say it in.
const COMMON_MEETINGS_FLOOR: usize = 4;

/// Talk that names no topic, beyond the shared stopwords: what people say on
/// every call, and the calendar words every plan carries.
const SMALL_TALK: &[&str] = &[
    "able",
    "absolutely",
    "actually",
    "afternoon",
    "already",
    "always",
    "another",
    "anybody",
    "anyone",
    "anything",
    "anyway",
    "april",
    "august",
    "awesome",
    "back",
    "basically",
    "come",
    "coming",
    "cool",
    "december",
    "definitely",
    "else",
    "even",
    "ever",
    "every",
    "everybody",
    "everyone",
    "everything",
    "exactly",
    "february",
    "feel",
    "fine",
    "first",
    "friday",
    "getting",
    "going",
    "gonna",
    "good",
    "great",
    "guess",
    "guys",
    "hear",
    "hello",
    "honestly",
    "january",
    "july",
    "june",
    "kind",
    "kinda",
    "know",
    "last",
    "like",
    "little",
    "look",
    "looking",
    "lots",
    "make",
    "many",
    "march",
    "may",
    "maybe",
    "mean",
    "might",
    "monday",
    "month",
    "months",
    "morning",
    "much",
    "must",
    "need",
    "never",
    "next",
    "nice",
    "november",
    "october",
    "people",
    "pretty",
    "probably",
    "really",
    "right",
    "said",
    "saturday",
    "saying",
    "says",
    "september",
    "somebody",
    "someone",
    "something",
    "sorry",
    "sort",
    "still",
    "stuff",
    "sunday",
    "sure",
    "take",
    "talk",
    "talking",
    "thank",
    "thanks",
    "thing",
    "things",
    "think",
    "thinking",
    "thursday",
    "time",
    "today",
    "tomorrow",
    "tonight",
    "totally",
    "tuesday",
    "wanna",
    "want",
    "wanted",
    "wednesday",
    "week",
    "weeks",
    "well",
    "year",
    "years",
    "yesterday",
];

/// What a capture's recall checks have read from the store, kept as long as
/// the capture. Each check's minute of words overlaps the last by most of
/// it, so a check reads only the terms and meetings no earlier one did. Who
/// is confirmed in the call is not kept: that can change mid-call.
#[derive(Default)]
pub(crate) struct RecallMemory {
    /// The index's reach, counted once it holds any line.
    corpus: Option<Reach>,
    /// Each candidate's reach, by phrase.
    reach: HashMap<String, Reach>,
    /// The past lines each chosen term found, by phrase.
    said: HashMap<String, Vec<MatchedLine>>,
    meetings: HashMap<MeetingSessionId, PastMeeting>,
}

impl MeetingStore {
    /// The past lines outside `session_id` that its `recent` live words were
    /// already said in, at most three and one per meeting.
    ///
    /// Ranked: open promises and questions involving someone confirmed in
    /// this meeting first, then other open ones, then decisions, then plain
    /// lines. A person is named only from a confirmed identification of the
    /// line's speaker.
    ///
    /// The connection is taken for one read at a time rather than for the
    /// whole check, so the capture's writes can get in between reads.
    pub(crate) fn live_recall(
        &self,
        session_id: MeetingSessionId,
        recent: &[String],
        memory: &mut RecallMemory,
        user_name: Option<&str>,
    ) -> Result<Vec<MeetingRecallMatch>, StoreError> {
        let candidates = candidate_terms(recent);
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        {
            let mut connection = self.connection()?;
            search_projection::refresh_in(&mut connection)?;
        }
        let terms = self.distinctive_terms(session_id, candidates, memory)?;
        let attendees: HashSet<PersonId> =
            confirmed_person_ids_in(&*self.connection()?, session_id)?
                .into_iter()
                .collect();
        let RecallMemory { said, meetings, .. } = memory;
        let mut found = Vec::new();
        for (term_rank, term) in terms.iter().enumerate() {
            let lines = match said.entry(term.phrase.clone()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => entry.insert(matched_lines_in(
                    &*self.connection()?,
                    &term.query(),
                    LINES_PER_TERM,
                    Some(session_id),
                )?),
            };
            for (line_rank, line) in lines.iter().enumerate() {
                let meeting = match meetings.entry(line.session_id) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => entry.insert(PastMeeting::read_in(
                        &*self.connection()?,
                        line.session_id,
                        user_name,
                    )?),
                };
                let person = meeting
                    .voices
                    .voice_in(
                        &*self.connection()?,
                        line.session_id,
                        line.speaker_id,
                        line.assignment,
                    )?
                    .person;
                let (kind, owned_here) = meeting.kind_of(line.segment_id, &attendees);
                let involves = owned_here
                    || meeting.shares_people_with(&attendees)
                    || person
                        .as_ref()
                        .is_some_and(|person| attendees.contains(&person.person_id));
                found.push(Found {
                    involves,
                    term_rank,
                    line_rank,
                    recall: MeetingRecallMatch {
                        kind,
                        text: line.text.clone(),
                        session_id: line.session_id,
                        segment_id: line.segment_id,
                        start_offset_ns: line.start_offset_ns,
                        meeting_title: line.meeting_title.clone(),
                        meeting_started_at_utc_ms: line.meeting_started_at_utc_ms,
                        app_bundle_id: meeting.app_bundle_id.clone(),
                        person,
                        term: term.phrase.clone(),
                    },
                });
            }
        }
        found.sort_by_key(|found| {
            (
                found.recall.kind,
                !found.involves,
                found.term_rank,
                Reverse(found.recall.meeting_started_at_utc_ms),
                found.line_rank,
            )
        });
        let mut recalled = HashSet::new();
        Ok(found
            .into_iter()
            .filter(|found| recalled.insert(found.recall.session_id))
            .take(MATCHES_MAX)
            .map(|found| found.recall)
            .collect())
    }

    /// The few candidates past meetings said but do not say all the time:
    /// pairs, then names and numbers, then the rarest, a pair over the words
    /// inside it.
    fn distinctive_terms(
        &self,
        session_id: MeetingSessionId,
        candidates: Vec<Term>,
        memory: &mut RecallMemory,
    ) -> Result<Vec<Term>, StoreError> {
        let corpus = match memory.corpus {
            Some(corpus) => corpus,
            None => corpus_in(&*self.connection()?, session_id)?,
        };
        if corpus.lines == 0 {
            return Ok(Vec::new());
        }
        memory.corpus = Some(corpus);
        let mut heard = Vec::new();
        for term in candidates {
            let reach = match memory.reach.entry(term.phrase.clone()) {
                Entry::Occupied(entry) => *entry.get(),
                Entry::Vacant(entry) => *entry.insert(reach_in(
                    &*self.connection()?,
                    session_id,
                    &term,
                    corpus.lines_cap(),
                )?),
            };
            if reach.lines > 0 && !reach.common_in(&corpus) {
                heard.push((reach.lines, term));
            }
        }
        heard.sort_by_key(|(lines, term)| {
            (
                Reverse(term.pair),
                Reverse(term.marked),
                *lines,
                Reverse(term.phrase.chars().count()),
            )
        });
        let mut chosen: Vec<Term> = Vec::new();
        for (_, term) in heard {
            if chosen.len() == TERMS_MAX {
                break;
            }
            if !chosen.iter().any(|pair| term.inside(pair)) {
                chosen.push(term);
            }
        }
        Ok(chosen)
    }
}

/// One past line a term found, with what ranks it.
struct Found {
    /// Someone confirmed in the live meeting owns it, said it, or was there.
    involves: bool,
    term_rank: usize,
    line_rank: usize,
    recall: MeetingRecallMatch,
}

/// What one past meeting made of its lines, read once however many of them
/// match.
struct PastMeeting {
    /// Lines an open promise or question cites, each with the people who
    /// own one of those.
    open: HashMap<TranscriptSegmentId, Vec<PersonId>>,
    /// Lines a decision cites.
    decided: HashSet<TranscriptSegmentId>,
    /// Everyone confirmed in this meeting.
    people: Vec<PersonId>,
    app_bundle_id: Option<String>,
    voices: SessionVoices,
}

impl PastMeeting {
    fn read_in(
        connection: &Connection,
        session_id: MeetingSessionId,
        user_name: Option<&str>,
    ) -> Result<Self, StoreError> {
        let mut open = HashMap::<TranscriptSegmentId, Vec<PersonId>>::new();
        let mut decided = HashSet::new();
        if let Some(ledger) = current_ledger_in(connection, session_id)? {
            for thread in &ledger.threads {
                if matches!(
                    thread.state,
                    LedgerThreadState::Decided | LedgerThreadState::Agreed
                ) {
                    decided.extend(
                        thread
                            .receipt
                            .citations
                            .iter()
                            .map(|citation| citation.segment_id),
                    );
                }
            }
            let states = states_in(connection, session_id)?;
            for seed in ledger_loop_seeds(session_id, &ledger) {
                let state = states.get(seed.loop_id.as_str());
                if !state.is_none_or(|state| state.status.is_open()) {
                    continue;
                }
                let owner = state.and_then(|state| state.owner_person_id);
                for citation in seed.citations {
                    open.entry(citation.segment_id).or_default().extend(owner);
                }
            }
        }
        let preflight: String = connection.query_row(
            "SELECT preflight_json FROM meeting_sessions WHERE id = ?1",
            params![id(session_id)],
            |row| row.get(0),
        )?;
        Ok(Self {
            open,
            decided,
            people: confirmed_person_ids_in(connection, session_id)?,
            app_bundle_id: application_bundle_ids(&preflight)?.into_iter().next(),
            voices: SessionVoices::read_in(connection, session_id, user_name)?,
        })
    }

    /// What became of `segment_id` here, and whether an open row citing it
    /// is owned by one of `attendees`.
    fn kind_of(
        &self,
        segment_id: TranscriptSegmentId,
        attendees: &HashSet<PersonId>,
    ) -> (MeetingRecallKind, bool) {
        if let Some(owners) = self.open.get(&segment_id) {
            (
                MeetingRecallKind::StillOpen,
                owners.iter().any(|owner| attendees.contains(owner)),
            )
        } else if self.decided.contains(&segment_id) {
            (MeetingRecallKind::Decided, false)
        } else {
            (MeetingRecallKind::SaidBefore, false)
        }
    }

    /// Whether someone confirmed here is one of `attendees`.
    fn shares_people_with(&self, attendees: &HashSet<PersonId>) -> bool {
        self.people.iter().any(|person| attendees.contains(person))
    }
}

/// A word or two from the last minute, as the index spells them.
struct Term {
    /// Lowercase words, one space apart.
    phrase: String,
    /// Two words said together: "pricing sheet" names a thing, "pricing" a
    /// topic.
    pair: bool,
    /// A name or a number: capitalized away from a sentence's start, in
    /// capitals, or holding a digit.
    marked: bool,
}

impl Term {
    /// The FTS5 phrase that finds exactly these words, in this order.
    fn query(&self) -> String {
        format!("\"{}\"", self.phrase)
    }

    /// Whether this single word is one of `pair`'s two.
    fn inside(&self, pair: &Self) -> bool {
        !self.pair && pair.pair && pair.phrase.split(' ').any(|word| word == self.phrase)
    }
}

/// How far a term, or the whole index, reaches through past meetings.
#[derive(Clone, Copy)]
struct Reach {
    lines: usize,
    meetings: usize,
}

impl Reach {
    /// The most past lines a term outside everyday talk is on.
    fn lines_cap(&self) -> usize {
        COMMON_LINES_FLOOR.max(self.lines / COMMON_LINE_SHARE)
    }

    /// Whether a term reaching this far through `corpus` is everyday talk
    /// there.
    fn common_in(&self, corpus: &Self) -> bool {
        self.lines > corpus.lines_cap()
            || (corpus.meetings >= COMMON_MEETINGS_FLOOR
                && self.meetings.saturating_mul(2) > corpus.meetings)
    }
}

/// One word as it was heard.
struct Word {
    /// Lowercase.
    text: String,
    marked: bool,
}

/// Every word in `recent` that can name a topic, and every two of them said
/// together, once each: names and numbers first, then pairs, then longer
/// words, cut at what one read counts.
fn candidate_terms(recent: &[String]) -> Vec<Term> {
    let mut terms = Vec::<Term>::new();
    let mut index = HashMap::<String, usize>::new();
    for line in recent {
        let words = heard_words(line);
        for (position, word) in words.iter().enumerate() {
            let Some(word) = word else { continue };
            if let Some(Some(next)) = words.get(position + 1) {
                note(
                    &mut terms,
                    &mut index,
                    format!("{} {}", word.text, next.text),
                    true,
                    word.marked || next.marked,
                );
            }
            note(
                &mut terms,
                &mut index,
                word.text.clone(),
                false,
                word.marked,
            );
        }
    }
    terms.sort_by_key(|term| {
        (
            Reverse(term.marked),
            Reverse(term.pair),
            Reverse(term.phrase.chars().count()),
        )
    });
    terms.truncate(CANDIDATES_MAX);
    terms
}

/// Count `phrase` once, marked if any hearing of it was.
fn note(
    terms: &mut Vec<Term>,
    index: &mut HashMap<String, usize>,
    phrase: String,
    pair: bool,
    marked: bool,
) {
    match index.entry(phrase) {
        Entry::Occupied(entry) => terms[*entry.get()].marked |= marked,
        Entry::Vacant(entry) => {
            let phrase = entry.key().clone();
            entry.insert(terms.len());
            terms.push(Term {
                phrase,
                pair,
                marked,
            });
        }
    }
}

/// The words of `line` in order, `None` for each that names no topic and
/// after each sentence, so no pair spans either.
fn heard_words(line: &str) -> Vec<Option<Word>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut opens_sentence = true;
    for character in line.chars().chain(std::iter::once(' ')) {
        if character.is_alphanumeric() {
            word.push(character);
            continue;
        }
        if !word.is_empty() {
            words.push(telling(&word, opens_sentence));
            word.clear();
            opens_sentence = false;
        }
        if matches!(character, '.' | '!' | '?') {
            words.push(None);
            opens_sentence = true;
        }
    }
    words
}

/// `word` as a candidate, when it can name a topic: four letters or more, or
/// two when it is a name or a number.
fn telling(word: &str, opens_sentence: bool) -> Option<Word> {
    let text = word.to_lowercase();
    if STOPWORDS.binary_search(&text.as_str()).is_ok() || SMALL_TALK.contains(&text.as_str()) {
        return None;
    }
    let letters = word.chars().count();
    let capitalized = word.chars().next().is_some_and(char::is_uppercase);
    let marked = word.chars().any(char::is_numeric)
        || (capitalized && !opens_sentence)
        || (letters >= 2 && word.chars().all(char::is_uppercase));
    (letters >= 4 || (marked && letters >= 2)).then_some(Word { text, marked })
}

/// How many past lines, and meetings, the index holds outside `session_id`.
fn corpus_in(connection: &Connection, session_id: MeetingSessionId) -> Result<Reach, StoreError> {
    let (lines, meetings): (i64, i64) = connection.query_row(
        "SELECT COUNT(*), COUNT(DISTINCT session_id) FROM meeting_search_documents
          WHERE entity_kind = 'segment' AND session_id != ?1",
        params![id(session_id)],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    reach(lines, meetings)
}

/// How many past lines, and meetings, outside `session_id` say `term`,
/// counted no further than one line past `lines_cap`.
fn reach_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    term: &Term,
    lines_cap: usize,
) -> Result<Reach, StoreError> {
    let (lines, meetings): (i64, i64) = connection.query_row(
        "SELECT COUNT(*), COUNT(DISTINCT session_id) FROM (
            SELECT d.session_id AS session_id
              FROM meeting_search_fts
              JOIN meeting_search_documents d ON d.id = meeting_search_fts.rowid
             WHERE meeting_search_fts MATCH ?1
               AND d.entity_kind = 'segment'
               AND d.session_id != ?2
             LIMIT ?3)",
        params![
            term.query(),
            id(session_id),
            i64::try_from(lines_cap.saturating_add(1)).map_err(|_| StoreError::Invalid)?
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    reach(lines, meetings)
}

fn reach(lines: i64, meetings: i64) -> Result<Reach, StoreError> {
    Ok(Reach {
        lines: usize::try_from(lines).map_err(|_| StoreError::Corrupt)?,
        meetings: usize::try_from(meetings).map_err(|_| StoreError::Corrupt)?,
    })
}

#[cfg(test)]
mod tests {
    use super::super::workflow_core_tests::{current_artifact, link, meeting, person, store};
    use super::*;
    use crate::meeting::types::{SaidPerson, SpeakerId};
    use uuid::Uuid;

    /// `text`, the one line `session_id` holds, said by a speaker under
    /// `label` who is identified only when `identified` names someone.
    fn line(
        store: &MeetingStore,
        session_id: MeetingSessionId,
        label: &str,
        text: &str,
        identified: Option<PersonId>,
    ) -> Result<TranscriptSegmentId, StoreError> {
        let plan_id = Uuid::new_v4().to_string();
        let track_id = Uuid::new_v4().to_string();
        let revision_id = Uuid::new_v4().to_string();
        let speaker_id = SpeakerId::new();
        let segment_id = TranscriptSegmentId::new();
        let connection = store.connection()?;
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
            "INSERT INTO meeting_speakers (speaker_id, session_id, source_kind, display_name, revision)
             VALUES (?1, ?2, 'microphone', ?3, 0)",
            params![id(speaker_id), id(session_id), label],
        )?;
        connection.execute(
            "INSERT INTO meeting_transcript_revisions (
                transcript_revision_id, session_id, engine_id, destination_json,
                source_set_json, language, state, created_at_utc_ms
             ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 1)",
            params![revision_id, id(session_id)],
        )?;
        connection.execute(
            "INSERT INTO meeting_transcript_segments (
                segment_id, transcript_revision_id, track_id, ordinal,
                start_offset_ns, end_offset_ns, speaker_id, base_text
             ) VALUES (?1, ?2, ?3, 0, 0, 1000000000, ?4, ?5)",
            params![id(segment_id), revision_id, track_id, id(speaker_id), text],
        )?;
        connection.execute(
            "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
            params![revision_id, id(session_id)],
        )?;
        if let Some(person_id) = identified {
            connection.execute(
                "INSERT INTO meeting_voice_speaker_resolutions (
                    operation_id, speaker_id, resolution_kind, resolved_person_id, is_current
                 ) VALUES (?1, ?2, 'identified', ?3, 1)",
                params![Uuid::new_v4().to_string(), id(speaker_id), id(person_id)],
            )?;
        }
        Ok(segment_id)
    }

    /// A current ledger for `session_id` holding one open promise, `what` by
    /// `who`, read off `segment_id`.
    fn promise(
        store: &MeetingStore,
        session_id: MeetingSessionId,
        who: &str,
        what: &str,
        segment_id: TranscriptSegmentId,
    ) {
        let receipt = serde_json::json!({
            "quote": "",
            "speaker": null,
            "t_ms": 0,
            "citations": [{
                "segment_id": segment_id,
                "start_offset_ns": 0,
                "end_offset_ns": 1_000_000_000
            }]
        });
        let content = serde_json::json!({
            "summary": {"text": what, "citations": []},
            "outline": [],
            "decisions": [],
            "action_items": [],
            "key_questions": [],
            "risks": [],
            "follow_up_draft": {"text": "", "citations": []},
            "ledger": {
                "headline": what,
                "threads": [],
                "open_loops": [],
                "commitments": [{"who": who, "what": what, "firmness": "firm", "receipt": receipt}],
                "stances": [],
                "caveats": [],
                "receipts": {"status": "verified"}
            }
        });
        current_artifact(store, session_id, &content, 1);
    }

    #[test]
    fn an_open_promise_by_someone_here_leads_and_only_confirmed_speakers_are_named(
    ) -> Result<(), StoreError> {
        let (_directory, store) = store();
        let dana = person(&store, "Dana Reyes", &[], &[]);
        let ada = person(&store, "Ada Lovelace", &[], &[]);
        let live = meeting(&store, "Pricing review", 4_000);
        link(&store, live, dana, "calendar", "confirmed");
        line(
            &store,
            live,
            "Speaker 1",
            "The pricing sheet is on screen.",
            None,
        )?;
        let promised = meeting(&store, "Q3 planning", 1_000);
        let said_by_dana = line(
            &store,
            promised,
            "Dana",
            "I'll send the pricing sheet by Friday.",
            Some(dana),
        )?;
        promise(
            &store,
            promised,
            "Dana Reyes",
            "Send the pricing sheet",
            said_by_dana,
        );
        let taken = meeting(&store, "Vendor call", 2_000);
        let said_by_ada = line(
            &store,
            taken,
            "Ada Lovelace",
            "I can take the pricing sheet.",
            None,
        )?;
        link(&store, taken, ada, "title", "suggested");
        promise(
            &store,
            taken,
            "Ada Lovelace",
            "Take the pricing sheet",
            said_by_ada,
        );
        let mentioned = meeting(&store, "Team sync", 3_000);
        line(
            &store,
            mentioned,
            "Speaker 1",
            "The pricing sheet looked fine.",
            None,
        )?;

        let recalled = store.live_recall(
            live,
            &["Did Dana send the pricing sheet yet?".to_owned()],
            &mut RecallMemory::default(),
            None,
        )?;

        let order: Vec<_> = recalled
            .iter()
            .map(|recall| (recall.session_id, recall.kind))
            .collect();
        assert_eq!(
            order,
            vec![
                (promised, MeetingRecallKind::StillOpen),
                (taken, MeetingRecallKind::StillOpen),
                (mentioned, MeetingRecallKind::SaidBefore),
            ]
        );
        assert_eq!(recalled[0].segment_id, said_by_dana);
        assert_eq!(recalled[0].term, "pricing sheet");
        assert_eq!(
            recalled[0].person,
            Some(SaidPerson {
                person_id: dana,
                display_name: "Dana Reyes".to_owned()
            })
        );
        assert!(recalled[1..].iter().all(|recall| recall.person.is_none()));
        Ok(())
    }

    #[test]
    fn a_check_that_reuses_the_capture_memory_still_sees_who_was_confirmed_since(
    ) -> Result<(), StoreError> {
        let (_directory, store) = store();
        let dana = person(&store, "Dana Reyes", &[], &[]);
        let ada = person(&store, "Ada Lovelace", &[], &[]);
        let live = meeting(&store, "Pricing review", 4_000);
        link(&store, live, dana, "calendar", "confirmed");
        let older = meeting(&store, "Q3 planning", 1_000);
        let said_by_dana = line(
            &store,
            older,
            "Dana",
            "I'll send the pricing sheet by Friday.",
            Some(dana),
        )?;
        promise(
            &store,
            older,
            "Dana Reyes",
            "Send the pricing sheet",
            said_by_dana,
        );
        let newer = meeting(&store, "Vendor call", 2_000);
        let said_by_ada = line(&store, newer, "Ada", "I can take the pricing sheet.", None)?;
        link(&store, newer, ada, "calendar", "confirmed");
        promise(
            &store,
            newer,
            "Ada Lovelace",
            "Take the pricing sheet",
            said_by_ada,
        );
        let recent = ["Did Dana send the pricing sheet yet?".to_owned()];
        let mut memory = RecallMemory::default();
        let mut check = || -> Result<Vec<MeetingSessionId>, StoreError> {
            Ok(store
                .live_recall(live, &recent, &mut memory, None)?
                .iter()
                .map(|recall| recall.session_id)
                .collect())
        };

        assert_eq!(check()?, vec![older, newer]);
        assert_eq!(check()?, vec![older, newer]);
        link(&store, live, ada, "calendar", "confirmed");
        assert_eq!(check()?, vec![newer, older]);
        Ok(())
    }
}
