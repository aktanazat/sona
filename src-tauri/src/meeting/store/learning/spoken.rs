//! Loop 1: spoken symbol phrases the replacement engine does not cover yet.
//!
//! # Why the candidates are phrases, not words
//!
//! Accepting a suggestion here writes a [`ReplacementRule`], and
//! [`crate::audio_toolkit::apply_text_replacements`] matches a rule
//! case-insensitively at every Unicode token boundary, preferring the longest
//! rule at a position. A rule whose spoken form is the bare word `dot`
//! therefore rewrites *polka dot* into *polka .* — the boundary check passes,
//! because a space is a boundary. Nothing downstream can recover from that: the
//! written form is inserted verbatim and never rescanned.
//!
//! So the hazard is not handled by a filter. It is handled by never generating
//! such a candidate:
//!
//! * A candidate is always either a multi-word phrase or an unambiguous noun,
//!   the same rule [`crate::settings::default_replacement_rules`] documents for
//!   the shipped starter library.
//! * Domain candidates are `dot` **plus the suffix**, and are counted only where
//!   the reading is syntactically live — a bare word label immediately before,
//!   which is what makes it a domain rather than a sentence.
//! * A precision floor over the whole corpus is the last gate: if most of a
//!   user's `dot`s are the English word, no `dot`-headed rule is offered at all,
//!   however live the few domain readings look.
//!
//! # What this loop deliberately does not own
//!
//! Sentence punctuation — comma, period, full stop, question mark, exclamation
//! mark, new line, new paragraph — belongs to
//! [`crate::audio_toolkit::apply_literal_punctuation`], which is gated on the
//! per-mode `literal_punctuation` choice. Suggesting a replacement rule for one
//! of those would both duplicate that owner and quietly override a user who
//! turned that choice off, so [`OWNED_ELSEWHERE`] names them and
//! [`written_forms`] removes them before anything can propose one.

use super::{
    advance_corpus_cursor_in, corpus_slice_in, covered_replacement_keys, insert_suggestions_in,
    local_day, normalized, observation_totals_in, prune_observations_in, record_observation_in,
    DictationCorpus, LearningInputs, MinedCandidate, ObservationExample,
};
use crate::meeting::learning_types::{LearningLoopKind, LearningSuggestion};
use crate::meeting::store::StoreError;
use rusqlite::Connection;
use std::collections::HashMap;

/// Spoken names for sentence punctuation, which
/// [`crate::audio_toolkit::apply_literal_punctuation`] owns. Removed from
/// [`written_forms`] rather than merely absent from the tables above, so no
/// later table entry can reach a candidate through this loop.
const OWNED_ELSEWHERE: &[&str] = &[
    "comma",
    "period",
    "full stop",
    "question mark",
    "exclamation mark",
    "exclamation point",
    "new line",
    "new paragraph",
];

/// How often a phrase must be read as a symbol before it is worth a sentence.
const MIN_OCCURRENCES: u64 = 4;
/// Across how many of the user's days. One busy afternoon is not a habit.
const MIN_DISTINCT_DAYS: u64 = 2;
/// What share of a phrase's occurrences must be live symbol readings, in parts
/// per thousand. Below this the phrase is mostly English and no rule is safe.
const MIN_PRECISION_PERMILLE: u32 = 600;

/// Domain suffixes worth a `dot X` rule, with the written form each produces.
/// Deliberately closed: an open suffix list would eventually admit an English
/// word after `dot`.
const DOMAIN_SUFFIXES: &[&str] = &[
    "com", "org", "net", "io", "ai", "co", "dev", "app", "gov", "edu", "me", "sh", "xyz", "info",
    "biz", "uk", "de", "fr", "jp", "ca", "au",
];

/// A `dot X` rule writes `.x`. Derived from the suffix rather than tabulated
/// beside it, so the two can never disagree.
fn domain_written(suffix: &str) -> String {
    format!(".{suffix}")
}

/// Spoken symbol names that cannot fire inside ordinary prose: each is either a
/// multi-word phrase or a noun with no other meaning.
///
/// Entries already in the shipped starter library stay listed. They are filtered
/// out by the covered-rule check rather than by omission, so a user who deleted
/// a starter rule and kept dictating the phrase is offered it back.
const SYMBOL_PHRASES: &[(&str, &str)] = &[
    ("at sign", "@"),
    ("hashtag", "#"),
    ("underscore", "_"),
    ("backslash", "\\"),
    ("forward slash", "/"),
    ("ampersand", "&"),
    ("asterisk", "*"),
    ("semicolon", ";"),
    ("percent sign", "%"),
    ("dollar sign", "$"),
    ("plus sign", "+"),
    ("equals sign", "="),
    ("caret", "^"),
    ("tilde", "~"),
    ("backtick", "`"),
    ("pipe symbol", "|"),
    ("open paren", "("),
    ("close paren", ")"),
    ("open bracket", "["),
    ("close bracket", "]"),
    ("open brace", "{"),
    ("close brace", "}"),
    ("greater than sign", ">"),
    ("less than sign", "<"),
    ("degree sign", "\u{00b0}"),
    ("bullet point", "\u{2022}"),
    ("right arrow", "\u{2192}"),
    ("em dash", "\u{2014}"),
    ("en dash", "\u{2013}"),
    ("ellipsis", "\u{2026}"),
    ("open quote", "\u{201c}"),
    ("close quote", "\u{201d}"),
];

/// One mining pass. Returns how many suggestions it added.
pub(in crate::meeting::store) fn mine_spoken_punctuation_in(
    connection: &Connection,
    inputs: &dyn LearningInputs,
    corpus: &DictationCorpus,
    now_utc_ms: i64,
) -> Result<u64, StoreError> {
    let loop_kind = LearningLoopKind::SpokenPunctuation;
    let slice = corpus_slice_in(connection, loop_kind, corpus)?;
    for row in &slice {
        // A retry is a second machine reading of audio a model already read, so
        // its text is not new evidence about how the user speaks.
        if row.is_retry {
            continue;
        }
        let day = local_day(row.completed_at_ms);
        for observation in observations_in_text(&row.delivered_text) {
            record_observation_in(
                connection,
                loop_kind,
                &observation.key,
                &day,
                observation.live,
                observation.total,
                &observation.phrase,
                observation
                    .example
                    .as_deref()
                    .map(|context| ObservationExample {
                        context,
                        // Dictation, not a meeting: this excerpt is bounded by the
                        // retention horizon and by nothing else.
                        session_id: None,
                    }),
            )?;
        }
    }
    if let Some(highest) = slice.last().map(|row| row.id) {
        advance_corpus_cursor_in(connection, loop_kind, highest)?;
    }
    prune_observations_in(connection, now_utc_ms)?;

    let covered = covered_replacement_keys(&inputs.replacement_rules());
    let written = written_forms();
    let totals = observation_totals_in(connection, loop_kind)?;
    let mut candidates = totals
        .iter()
        .filter(|(key, total)| {
            total.occurrences >= MIN_OCCURRENCES
                && total.distinct_days >= MIN_DISTINCT_DAYS
                && total.share_permille().unwrap_or(0) >= MIN_PRECISION_PERMILLE
                && !covered.contains(key.as_str())
        })
        .filter_map(|(key, total)| {
            let written = written.get(key.as_str())?;
            Some(MinedCandidate {
                key: key.clone(),
                suggestion: LearningSuggestion::SpokenPunctuation {
                    spoken: total.display_text.clone(),
                    written: written.clone(),
                },
                evidence: total.evidence(),
            })
        })
        .collect::<Vec<_>>();
    // Strongest evidence first, so a cap drops the weakest claim.
    candidates.sort_by(|left, right| {
        right
            .evidence
            .occurrences
            .cmp(&left.evidence.occurrences)
            .then_with(|| left.key.cmp(&right.key))
    });
    let added = insert_suggestions_in(connection, loop_kind, candidates, now_utc_ms)?;
    u64::try_from(added.len()).map_err(|_| StoreError::Corrupt)
}

/// Every phrase in the tables, keyed the way the ledger keys it, minus every
/// head [`OWNED_ELSEWHERE`] claims.
fn written_forms() -> HashMap<String, String> {
    let mut forms = SYMBOL_PHRASES
        .iter()
        .map(|(spoken, written)| (normalized(spoken), (*written).to_string()))
        .collect::<HashMap<_, _>>();
    for suffix in DOMAIN_SUFFIXES {
        forms.insert(normalized(&format!("dot {suffix}")), domain_written(suffix));
    }
    for head in OWNED_ELSEWHERE {
        forms.remove(&normalized(head));
    }
    forms
}

struct PhraseObservation {
    key: String,
    phrase: String,
    /// Occurrences whose reading is live: this is what the floors test.
    live: u64,
    /// Occurrences of the phrase at all: the denominator of the precision ratio.
    total: u64,
    example: Option<String>,
}

/// Counts every table phrase in one delivered transcript.
fn observations_in_text(text: &str) -> Vec<PhraseObservation> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut counts = HashMap::<String, PhraseObservation>::new();
    for index in 0..tokens.len() {
        for (phrase, live) in phrases_at(&tokens, index) {
            let key = normalized(&phrase);
            let entry = counts
                .entry(key.clone())
                .or_insert_with(|| PhraseObservation {
                    key,
                    phrase,
                    live: 0,
                    total: 0,
                    example: None,
                });
            entry.total = entry.total.saturating_add(1);
            if live {
                entry.live = entry.live.saturating_add(1);
                if entry.example.is_none() {
                    entry.example = Some(example_context(&tokens, index));
                }
            }
        }
    }
    counts.into_values().collect()
}

/// Which table phrases start at `index`, and whether each reading is live.
///
/// A phrase can match without being live. That gap is the precision ratio: it
/// is what tells the difference between a user who dictates domains and a user
/// who talks about polka dots.
fn phrases_at(tokens: &[&str], index: usize) -> Vec<(String, bool)> {
    let mut matches = Vec::new();
    let head = tokens[index];
    // A literal mention — "the word underscore" — is never a symbol reading.
    // Same guard `apply_literal_punctuation` uses for its own table.
    let mention = index > 0 && strip_word(tokens[index - 1]).eq_ignore_ascii_case("word");

    if strip_word(head).eq_ignore_ascii_case("dot") {
        if let Some(next) = tokens.get(index + 1) {
            let suffix = strip_word(next).to_ascii_lowercase();
            if DOMAIN_SUFFIXES.contains(&suffix.as_str()) {
                // Live only with a bare word label immediately before: that
                // label is the domain. `Dot com` opening a sentence, or `dot`
                // after punctuation, is prose.
                let live =
                    !mention && index > 0 && is_bare_word(tokens[index - 1]) && is_bare_word(head);
                matches.push((format!("dot {suffix}"), live));
            }
        }
    }

    for (phrase, _) in SYMBOL_PHRASES {
        let words: Vec<&str> = phrase.split(' ').collect();
        if words.len() > tokens.len() - index {
            continue;
        }
        let aligned = words
            .iter()
            .enumerate()
            .all(|(offset, word)| strip_word(tokens[index + offset]).eq_ignore_ascii_case(word));
        if aligned {
            matches.push(((*phrase).to_string(), !mention));
        }
    }
    matches
}

/// A token with its surrounding punctuation removed, which is how the phrase
/// tables compare tokens. Interior characters are left alone.
fn strip_word(token: &str) -> &str {
    token.trim_matches(|character: char| !character.is_alphanumeric())
}

/// Whether a token is a bare word: no trailing punctuation, so the next token
/// continues the same construct rather than starting a new one.
fn is_bare_word(token: &str) -> bool {
    token.chars().next_back().is_some_and(char::is_alphanumeric)
        && token.chars().any(char::is_alphanumeric)
}

/// A few words either side of an occurrence, so a card can show the reader their
/// own dictation instead of asking them to take it on faith.
fn example_context(tokens: &[&str], index: usize) -> String {
    const WINDOW: usize = 4;
    let start = index.saturating_sub(WINDOW);
    let end = (index + WINDOW + 1).min(tokens.len());
    let mut context = tokens[start..end].join(" ");
    if context.chars().count() > 120 {
        context = context.chars().take(117).collect::<String>() + "...";
    }
    context
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The polka-dot hazard, stated as a test: a bare `dot` is never a
    /// candidate, and a `dot` with no domain label before it is never live.
    #[test]
    fn a_bare_dot_is_never_a_candidate_and_prose_dots_are_not_live() {
        let observations = observations_in_text("she wore a polka dot dress to the party");
        assert!(
            observations.is_empty(),
            "prose `dot` produced candidates: {:?}",
            observations
                .iter()
                .map(|o| o.phrase.clone())
                .collect::<Vec<_>>()
        );

        let observations = observations_in_text("polka dot com is not a domain label.");
        let domain = observations
            .iter()
            .find(|observation| observation.phrase == "dot com")
            .expect("dot com counted");
        // It matched, so it is in the denominator; `polka` is a bare word, so
        // this reading is live. That is honest: the phrase really was said.
        assert_eq!(domain.total, 1);

        let observations = observations_in_text("Dot com sites are everywhere");
        let domain = observations
            .iter()
            .find(|observation| observation.phrase == "dot com")
            .expect("dot com counted");
        assert_eq!(domain.total, 1);
        assert_eq!(domain.live, 0, "a sentence-initial dot com is not live");
    }

    #[test]
    fn a_domain_reading_is_live_and_carries_its_context() {
        let observations = observations_in_text("email me at aktan at sign example dot com please");
        let domain = observations
            .iter()
            .find(|observation| observation.phrase == "dot com")
            .expect("dot com counted");
        assert_eq!((domain.live, domain.total), (1, 1));
        assert!(domain
            .example
            .as_deref()
            .is_some_and(|context| context.contains("example dot com")));

        let at_sign = observations
            .iter()
            .find(|observation| observation.phrase == "at sign")
            .expect("at sign counted");
        assert_eq!((at_sign.live, at_sign.total), (1, 1));
    }

    #[test]
    fn a_literal_mention_is_counted_but_never_live() {
        let observations = observations_in_text("type the word underscore then underscore again");
        let underscore = observations
            .iter()
            .find(|observation| observation.phrase == "underscore")
            .expect("underscore counted");
        assert_eq!(underscore.total, 2);
        assert_eq!(underscore.live, 1, "the mention after `word` is not live");
    }

    /// Sentence punctuation has one owner — `apply_literal_punctuation`, gated
    /// on the per-mode `literal_punctuation` choice — and this loop is not it.
    #[test]
    fn no_sentence_punctuation_head_can_become_a_candidate() {
        let forms = written_forms();
        for head in OWNED_ELSEWHERE {
            assert!(
                !forms.contains_key(&normalized(head)),
                "{head} belongs to the literal-punctuation table"
            );
        }
    }

    /// A suggested symbol rule must neither erase text nor match an empty phrase.
    #[test]
    fn symbol_rules_never_erase_text_or_match_an_empty_phrase() {
        for (spoken, written) in SYMBOL_PHRASES {
            assert!(!written.is_empty(), "{spoken} has no written form");
            assert!(!normalized(spoken).is_empty(), "{spoken} normalizes empty");
        }
    }
}
