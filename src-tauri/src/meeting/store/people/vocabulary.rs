use super::{all_people_in, identity_names, normalized, people_revision_in, SCHEMA_VERSION};
use crate::meeting::learning_types::LearningLoopKind;
use crate::meeting::people_types::{VocabularyCandidate, VocabularyCandidatesResult};
use crate::meeting::store::learning::decided_keys_in;
use crate::meeting::store::workflows::workflow_has_successful_run_in;
use crate::meeting::store::{effective_segments_for_session, MeetingStore, StoreError};
use crate::meeting::types::MeetingSessionId;
use crate::meeting::workflow_types::WorkflowId;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

impl MeetingStore {
    pub(crate) fn vocabulary_candidates(
        &self,
        known_terms: &[String],
    ) -> Result<VocabularyCandidatesResult, StoreError> {
        self.vocabulary_candidates_without(known_terms, &HashSet::new())
    }

    /// The heard words for a reader the `hidden` meetings are kept from: no
    /// word or count comes from them.
    pub(crate) fn vocabulary_candidates_without(
        &self,
        known_terms: &[String],
        hidden: &HashSet<MeetingSessionId>,
    ) -> Result<VocabularyCandidatesResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let revision = people_revision_in(&transaction)?;
        let entries = if workflow_has_successful_run_in(&transaction, WorkflowId::VocabularyMining)?
        {
            candidates_in(&transaction, known_terms, hidden)?
        } else {
            Vec::new()
        };
        let result = VocabularyCandidatesResult {
            schema_version: SCHEMA_VERSION,
            revision,
            entries,
        };
        transaction.commit()?;
        Ok(result)
    }
}

pub(in crate::meeting::store) fn vocabulary_candidates_in(
    connection: &Connection,
    known_terms: &[String],
) -> Result<Vec<VocabularyCandidate>, StoreError> {
    candidates_in(connection, known_terms, &HashSet::new())
}

fn candidates_in(
    connection: &Connection,
    known_terms: &[String],
    hidden: &HashSet<MeetingSessionId>,
) -> Result<Vec<VocabularyCandidate>, StoreError> {
    let excluded = excluded_terms(connection, known_terms)?;
    let mut statement = connection.prepare(
        "SELECT id FROM meeting_sessions
          WHERE current_transcript_revision_id IS NOT NULL
          ORDER BY created_at_utc_ms, id",
    )?;
    let sessions = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let mut counts = HashMap::<String, CandidateCount>::new();
    for session in sessions {
        let session_id = Uuid::parse_str(&session)
            .map(MeetingSessionId::from_uuid)
            .map_err(|_| StoreError::Corrupt)?;
        if hidden.contains(&session_id) {
            continue;
        }
        /* Each turn is its own text: a turn cut off without punctuation
         * still starts the next one fresh, and no run crosses a speaker. */
        for segment in effective_segments_for_session(connection, session_id)? {
            if segment.removed {
                continue;
            }
            let text = segment
                .replacement_text
                .as_deref()
                .unwrap_or(&segment.base.text);
            for candidate in candidates_in_text(text) {
                let key = normalized(&candidate);
                if key.is_empty() || excluded.contains(&key) {
                    continue;
                }
                let count = counts.entry(key).or_insert_with(|| CandidateCount {
                    display: candidate,
                    occurrences: 0,
                    meetings: HashSet::new(),
                });
                count.occurrences = count.occurrences.saturating_add(1);
                count.meetings.insert(session_id);
            }
        }
    }

    let mut candidates = counts
        .into_values()
        .filter(|candidate| candidate.occurrences >= 3 && candidate.meetings.len() >= 2)
        .map(|candidate| {
            Ok(VocabularyCandidate {
                text: candidate.display,
                occurrences: candidate.occurrences,
                meetings_count: u64::try_from(candidate.meetings.len())
                    .map_err(|_| StoreError::Corrupt)?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    candidates.sort_by(|left, right| {
        right
            .occurrences
            .cmp(&left.occurrences)
            .then_with(|| left.text.cmp(&right.text))
    });
    Ok(candidates)
}

struct CandidateCount {
    display: String,
    occurrences: u64,
    meetings: HashSet<MeetingSessionId>,
}

/// Terms this list must not offer: what the user already has, who Sona already
/// knows about, and — the reason this reads the store rather than a client's
/// local storage — every term the user has already answered about.
fn excluded_terms(
    connection: &Connection,
    known_terms: &[String],
) -> Result<HashSet<String>, StoreError> {
    let mut excluded = known_terms
        .iter()
        .map(|term| normalized(term))
        .collect::<HashSet<_>>();
    for person in all_people_in(connection)? {
        excluded.extend(identity_names(&person).map(normalized));
    }
    excluded.extend(decided_keys_in(
        connection,
        LearningLoopKind::VocabularyTerm,
    )?);
    Ok(excluded)
}

/// A capital letter is evidence of a name only where grammar does not force
/// one: a lone word opening a sentence and the pronoun "I" are capitalized
/// whatever they are, so a transcript full of "Yeah." and "I'm" would
/// otherwise offer them as words to learn. A sentence opening with two
/// capitals, as in "North Star shipped", still names something, unless the
/// first is a word that only opens sentences, as in "So Maya said". A run
/// of capitalized words ends at punctuation, so "Sona. So" is two words, not
/// a phrase, except after a title: "Dr. Patel" is one name.
fn candidates_in_text(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut run = Vec::new();
    let mut opener: Option<String> = None;
    let mut opens_sentence = true;
    for raw in text.split_whitespace() {
        let starts_sentence = opens_sentence;
        let title = is_title(raw);
        opens_sentence = !title && ends_sentence(raw);
        let ends_run = !title && raw.ends_with(|character: char| !character.is_alphanumeric());
        let token = clean_token(raw).filter(|token| is_capitalized(token) && !is_pronoun_i(raw));
        let Some(token) = token else {
            opener = None;
            push_run(&mut run, &mut candidates);
            continue;
        };
        if starts_sentence {
            push_run(&mut run, &mut candidates);
            opener = (!ends_run && !is_sentence_word(&token)).then_some(token);
            continue;
        }
        run.extend(opener.take());
        run.push(token);
        if ends_run {
            push_run(&mut run, &mut candidates);
        }
    }
    push_run(&mut run, &mut candidates);
    candidates
}

/// A sentence ends at its mark even inside a closing quote or bracket:
/// `"fine."` ends one.
fn ends_sentence(raw: &str) -> bool {
    raw.trim_end_matches(['"', '\'', '\u{201D}', '\u{2019}', ')', ']'])
        .ends_with(['.', '!', '?', '\u{2026}'])
}

fn is_title(raw: &str) -> bool {
    raw.ends_with('.')
        && matches!(
            raw.trim_end_matches('.').to_ascii_lowercase().as_str(),
            "mr" | "mrs" | "ms" | "dr" | "prof" | "st" | "jr" | "sr"
        )
}

fn push_run(run: &mut Vec<String>, candidates: &mut Vec<String>) {
    for chunk in run.chunks(3) {
        if chunk.len() == 1 && is_sentence_word(&chunk[0]) {
            continue;
        }
        candidates.push(chunk.join(" "));
    }
    run.clear();
}

fn clean_token(token: &str) -> Option<String> {
    let token = token
        .trim_matches(|character: char| !character.is_alphanumeric())
        .chars()
        .filter(|character| character.is_alphanumeric() || matches!(character, '-' | '\''))
        .collect::<String>();
    (!token.is_empty()).then_some(token)
}

fn is_pronoun_i(raw: &str) -> bool {
    let word = raw
        .trim_matches(|character: char| !character.is_alphanumeric())
        .to_lowercase();
    word == "i" || word.starts_with("i'") || word.starts_with("i\u{2019}")
}

fn is_capitalized(token: &str) -> bool {
    token.chars().next().is_some_and(char::is_uppercase) && token.chars().any(char::is_alphabetic)
}

/// Words a capital proves nothing about: the ones that open sentences or
/// fill a pause.
fn is_sentence_word(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "a" | "also"
            | "an"
            | "and"
            | "but"
            | "hey"
            | "hi"
            | "how"
            | "i"
            | "if"
            | "it"
            | "it's"
            | "its"
            | "just"
            | "let's"
            | "like"
            | "now"
            | "oh"
            | "ok"
            | "okay"
            | "right"
            | "so"
            | "sure"
            | "thanks"
            | "that"
            | "that's"
            | "the"
            | "then"
            | "there's"
            | "this"
            | "we"
            | "we're"
            | "well"
            | "what"
            | "when"
            | "why"
            | "yeah"
            | "yes"
            | "you"
            | "you're"
    )
}

#[cfg(test)]
mod tests {
    use super::candidates_in_text;

    #[test]
    fn grammar_capitals_are_not_names() {
        let text = "Yeah. I'll ask Maya about it. Sona. So we ship. Like I said. \
                    It's fine. I'm sure Maya and Open AI agreed with Kubernetes. \
                    North Star shipped. So Maya said it. Thanks Maya. \
                    We talked to Dr. Patel about it. She said \"fine.\" Then Maya left.";
        assert_eq!(
            candidates_in_text(text),
            [
                "Maya",
                "Maya",
                "Open AI",
                "Kubernetes",
                "North Star",
                "Maya",
                "Maya",
                "Dr Patel",
                "Maya"
            ]
        );
    }
}
