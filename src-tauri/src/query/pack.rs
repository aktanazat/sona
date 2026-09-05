//! The evidence for one question, in one string.
//!
//! A pack is what the plane hands an answering model: the top rows one search
//! returned, each as a verbatim quote with the noun it came from, when it was
//! said, and its `sona://` address. Nothing here summarises, paraphrases or
//! ranks — the model gets the words that matched and the links to check them,
//! which is what makes a cited answer checkable.
//!
//! # Provenance, and what the plane can honestly attribute
//!
//! Every quote carries the noun it came out of and that noun's time. What it
//! cannot carry is a per-sentence speaker: the query plane's row union
//! ([`QueryRow`]) is one row per noun, so a meeting quote is attributed to the
//! meeting rather than to whoever said it in the room. A person row is the one
//! kind whose title *is* a speaker. Inventing an attribution the corpus did not
//! return would be worse than naming the noun, because the whole point of a
//! pack is that every line in it can be followed back to a link.
//!
//! # Why the cap is the panel's cap
//!
//! [`MAX_CONTEXT_PACK_BYTES`] is the ceiling the panel enforces on the wire
//! (`agent_panel/protocol.rs`), so it is the ceiling this builder truncates to.
//! A second number here would be a second thing to keep true, and the failure
//! mode of getting it wrong is a question that is refused after the reader has
//! already asked it.
//!
//! # Why a series can be missing from its own pack
//!
//! A pack is the one thing on this surface that leaves the machine verbatim:
//! `agent_panel` posts it to the operator's relay as a `sona_chat` turn. D14's
//! per-series escape hatch — "a series listed here is always written on this
//! Mac, even while meeting intelligence is on" — therefore has to hold here as
//! well as in `processing::choose_text_engine`, and it holds the same way:
//! [`without_excluded_series`] joins each meeting and loop row to the series
//! behind it and drops the rows that series kept local. An unreadable
//! preference counts as excluded, the direction `processing.rs` already leans
//! for exactly this fact.
//!
//! The header says nothing about it. A dropped quote at the byte ceiling is
//! reported because the model would otherwise answer "nothing else came up"
//! from a truncated bundle; an excluded series is not evidence that was cut
//! short, it is evidence the operator said may not be sent, and naming its
//! absence on the wire would put the fact of the exclusion on the server that
//! was not allowed to see the series.

use super::{
    loop_link, meeting_link, QueryError, QueryRow, QueryRowKind, QueryScope, QUERY_SCHEMA_VERSION,
};
use crate::agent_panel::protocol::MAX_CONTEXT_PACK_BYTES;
use crate::managers::history::HistoryManager;
use crate::meeting::detection::calendar::CalendarSource;
use crate::meeting::loop_types::MeetingLoopId;
use crate::meeting::people_types::{PersonDetail, PersonLinkConfidence};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::MeetingStore;
use crate::meeting::types::MeetingSessionId;
use chrono::TimeZone;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

/// How many rows one question is answered from.
///
/// The page is recency-ordered (see the module header of [`super`]), so this is
/// "the newest dozen things that matched", not "the twelve best". A dozen quotes
/// is roughly five kilobytes — enough evidence for a question about a week, far
/// enough inside the ceiling that the truncation path is the exception.
const PACK_HITS: usize = 12;

/// One question's evidence bundle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct QueryPack {
    pub schema_version: u32,
    /// The pack itself, ready to ride as a turn's `context_pack`.
    pub pack: String,
    /// Exactly the rows quoted in `pack`, in the order they are quoted. A
    /// caller that renders citations beside an answer renders these, so the two
    /// can never disagree about what the model was shown.
    pub sources: Vec<QueryRow>,
}

/// Assemble the pack for one question: the corpus card, then the evidence.
///
/// One search over every scope, the series exclusions applied, then
/// [`build`]. An empty question is [`QueryError::InvalidRequest`] rather than
/// an empty pack: the plane refuses to match the whole corpus on no tokens,
/// and a pack of nothing sent to a model is a question asked without its
/// evidence.
///
/// The card ([`super::card::corpus_card`]) says what the corpus is in numbers,
/// so the model can answer an aggregate question without a lookup and pick
/// the right tool for one that needs it. It heads the pack under the same
/// ceiling: the evidence is cut to the room the card leaves, not the other
/// way round, because a card is two kilobytes and a pack of quotes is whatever
/// fits. `sources` are the quotes' rows; the card quotes nothing.
pub async fn for_question(
    meetings: &Arc<MeetingSessionManager>,
    history: &Arc<HistoryManager>,
    calendar: &Arc<dyn CalendarSource>,
    question: &str,
) -> Result<QueryPack, QueryError> {
    let question = question.trim();
    if question.is_empty() {
        return Err(QueryError::InvalidRequest);
    }
    // One debug line per assembled pack, naming where the wall time went. A
    // pack is the only thing between a typed question and a signed submission,
    // so a reader who sees the panel sit still needs to be able to tell a slow
    // corpus read from a slow card from a slow relay without a rebuild.
    let started = Instant::now();
    let card = super::card::corpus_card(meetings, history, calendar, chrono::Local::now()).await;
    let card_done = started.elapsed();
    let page = super::search(
        meetings,
        history,
        QueryScope::All,
        question,
        Some(PACK_HITS),
        None,
    )
    .await?;
    let search_done = started.elapsed();
    // The same mount the search just read through, so the exclusion is decided
    // against the store that produced the rows.
    let store = meetings.store().await?;
    let rows = without_excluded_series(&store, page.entries);
    let ceiling = MAX_CONTEXT_PACK_BYTES.saturating_sub(card.len() + 2);
    let evidence = build_within(question, rows, page.next_cursor.is_some(), ceiling);
    let finished = started.elapsed();
    log::debug!(
        "Context pack assembled in {finished:?}: card {card_done:?}, search {:?}, \
         exclude+render {:?}",
        search_done - card_done,
        finished - search_done,
    );
    Ok(QueryPack {
        schema_version: evidence.schema_version,
        pack: format!("{card}\n\n{}", evidence.pack),
        sources: evidence.sources,
    })
}

/// Assemble the pack for one person: their meetings, and what is open with them.
///
/// Store-only and synchronous, unlike [`for_question`], because its one caller
/// is the artifact pass — which holds a mounted store on a job thread and no
/// async runtime, and whose question is a person rather than a search. The rows
/// are that person's own facts read straight off their page, so no index is
/// consulted and no dictation is drawn in: a dictation belongs to nobody.
///
/// The detail is passed in rather than read here: the caller needs the person's
/// name for its prompt anyway, and one read answers both.
///
/// Everything else is the same pack: the same header, the same byte ceiling,
/// and the same series exclusion — this bundle can ride to the operator's relay
/// exactly as a question's can, so a series kept on this Mac is dropped here
/// too.
pub(crate) fn for_person(store: &MeetingStore, detail: &PersonDetail) -> QueryPack {
    let mut rows = Vec::new();
    for link in detail
        .links
        .iter()
        .filter(|link| link.confidence == PersonLinkConfidence::Confirmed)
        .take(PACK_HITS)
    {
        rows.push(QueryRow {
            kind: QueryRowKind::Meeting,
            id: link.meeting.id.uuid().to_string(),
            title: link.meeting.title.clone(),
            snippet: link
                .meeting
                .headline
                .clone()
                .unwrap_or_else(|| link.meeting.title.clone()),
            when_utc_ms: link.meeting.at_utc_ms,
            link: meeting_link(link.meeting.id),
        });
    }
    for open_loop in detail.open_loops.iter().take(PACK_HITS) {
        rows.push(QueryRow {
            kind: QueryRowKind::Loop,
            id: open_loop.loop_id.as_str().to_string(),
            title: open_loop.text.clone(),
            snippet: open_loop.title.clone(),
            when_utc_ms: open_loop.at_utc_ms,
            link: loop_link(&open_loop.loop_id),
        });
    }
    for commitment in detail.commitments.iter().take(PACK_HITS) {
        rows.push(QueryRow {
            kind: QueryRowKind::Loop,
            id: commitment.loop_id.as_str().to_string(),
            title: commitment.text.clone(),
            snippet: commitment.title.clone(),
            when_utc_ms: commitment.at_utc_ms,
            link: loop_link(&commitment.loop_id),
        });
    }
    build(
        &detail.person.display_name,
        without_excluded_series(store, rows),
        false,
    )
}

/// Drop the rows whose series the operator kept on this Mac.
///
/// Meetings and loops only: a dictation belongs to no series, and a person row
/// quotes the headline of whichever meeting they were last in rather than that
/// meeting's own words. `more_matches` is left alone — it answers "does the
/// corpus hold more matches than this page", which an exclusion here does not
/// change.
///
/// One store read per distinct meeting, memoised, because a page of twelve is
/// usually a handful of meetings and the read is a two-statement join.
pub(crate) fn without_excluded_series(store: &MeetingStore, rows: Vec<QueryRow>) -> Vec<QueryRow> {
    let mut excluded: HashMap<MeetingSessionId, bool> = HashMap::new();
    rows.into_iter()
        .filter(|row| {
            let Some(session_id) = session_behind(row) else {
                return true;
            };
            !*excluded
                .entry(session_id)
                .or_insert_with(|| series_opted_out_of_remote(store, session_id))
        })
        .collect()
}

/// The meeting a row's own words came out of, for the two kinds that have one.
///
/// A loop's id leads with its session uuid by construction, so neither kind
/// needs a lookup to find its meeting.
fn session_behind(row: &QueryRow) -> Option<MeetingSessionId> {
    match row.kind {
        QueryRowKind::Meeting => Uuid::parse_str(&row.id)
            .ok()
            .map(MeetingSessionId::from_uuid),
        QueryRowKind::Loop => MeetingLoopId(row.id.clone()).session_id(),
        _ => None,
    }
}

/// Whether this meeting's series has been kept off the server.
///
/// A preference the store cannot read counts as opted out, which is the
/// direction `processing::series_opted_out_of_remote` already leans for the
/// same fact: the failure being guarded is evidence leaving the machine for a
/// series whose answer we could not read.
pub(super) fn series_opted_out_of_remote(
    store: &MeetingStore,
    session_id: MeetingSessionId,
) -> bool {
    match store.series_preferences_for_session(session_id) {
        Ok(preferences) => preferences.remote_intelligence_opt_out,
        Err(error) => {
            log::warn!(
                "Keeping {session_id:?} out of the context pack: its remote-intelligence preference could not be read: {error:?}"
            );
            true
        }
    }
}

/// Render the pack for rows that have already been found.
///
/// Split from [`for_question`] so the whole of the format — the header, the
/// count, what a dropped quote does to it — is provable without a corpus. The
/// output is a pure function of its arguments: no clock, no locale, no
/// iteration order that a map could shuffle.
pub(crate) fn build(question: &str, rows: Vec<QueryRow>, more_matches: bool) -> QueryPack {
    build_within(question, rows, more_matches, MAX_CONTEXT_PACK_BYTES)
}

/// [`build`] under a smaller ceiling, for a pack that shares the panel's
/// ceiling with a card in front of it. The header still names the panel's
/// number: that is the ceiling the reader can look up, and what was dropped
/// was dropped because of it.
fn build_within(
    question: &str,
    rows: Vec<QueryRow>,
    more_matches: bool,
    ceiling: usize,
) -> QueryPack {
    let question = one_line(question);
    let offered = rows.len();
    let entries = rows
        .iter()
        .enumerate()
        .map(|(index, row)| entry(index + 1, row))
        .collect::<Vec<_>>();

    // Down from every quote until the whole pack fits. The header states the
    // count, and the count changes what the header costs, so the fit is found
    // by composing rather than by arithmetic on a reserved budget.
    let mut quoted = entries.len();
    loop {
        let pack = compose(&question, &entries[..quoted], offered, more_matches);
        if pack.len() <= ceiling || quoted == 0 {
            return QueryPack {
                schema_version: QUERY_SCHEMA_VERSION,
                pack,
                sources: rows.into_iter().take(quoted).collect(),
            };
        }
        quoted -= 1;
    }
}

/// The pack's first lines: what this is, what was asked, and how much of the
/// answer is actually in front of the model. A pack that quietly dropped its
/// oldest evidence would let a model answer "nothing else came up" from a
/// truncated bundle.
fn compose(question: &str, entries: &[String], offered: usize, more_matches: bool) -> String {
    let dropped = offered - entries.len();
    let mut pack = format!(
        "sona context pack {QUERY_SCHEMA_VERSION}\nquestion: {question}\nquotes: {quoted} of {offered}",
        quoted = entries.len(),
    );
    if dropped > 0 {
        pack.push_str(&format!(
            " ({dropped} dropped at the {MAX_CONTEXT_PACK_BYTES}-byte ceiling)"
        ));
    }
    if more_matches {
        pack.push_str("\nmore matches exist beyond these.");
    }
    for entry in entries {
        pack.push_str("\n\n");
        pack.push_str(entry);
    }
    pack
}

/// One row: what it is, what it is called, when, where to check it, and the
/// words that matched.
fn entry(index: usize, row: &QueryRow) -> String {
    format!(
        "[{index}] {kind} · {title} · {when}\nlink: {link}\nquote: {quote}",
        kind = super::token(&row.kind),
        title = one_line(&row.title),
        when = when(row.when_utc_ms),
        link = row.link,
        quote = one_line(&row.snippet),
    )
}

/// UTC, to the minute. Not the reader's zone: a pack is read by a model that
/// has no zone, and two quotes from one meeting must not drift apart because
/// the machine that built the pack moved.
fn when(when_utc_ms: i64) -> String {
    chrono::Utc
        .timestamp_millis_opt(when_utc_ms)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown time".to_string())
}

/// Verbatim, minus the bytes that would let a quote forge structure.
///
/// Every whitespace run collapses to one space and control characters are
/// dropped: the panel refuses a pack containing control bytes at all
/// (`protocol.rs::is_message_text`), and a newline inside a quote would
/// otherwise be indistinguishable from the newline that ends it — which is how
/// a transcript could write its own `link:` line.
pub(super) fn one_line(text: &str) -> String {
    let mut line = String::with_capacity(text.len());
    let mut pending_space = false;
    for character in text.chars() {
        if character.is_whitespace() {
            pending_space = !line.is_empty();
            continue;
        }
        if character.is_control() {
            continue;
        }
        if pending_space {
            line.push(' ');
            pending_space = false;
        }
        line.push(character);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::loop_types::MeetingLoopKind;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::store::workflow_core_tests::{meeting, store};
    use crate::meeting::types::MeetingOperationId;

    /// 2026-08-14 09:32 UTC, so the rendered time is readable in the assertions
    /// rather than being a number this test also has to compute.
    const WHEN: i64 = 1_786_699_920_000;

    fn row(kind: QueryRowKind, id: &str, title: &str, snippet: &str) -> QueryRow {
        QueryRow {
            kind,
            id: id.to_string(),
            title: title.to_string(),
            snippet: snippet.to_string(),
            when_utc_ms: WHEN,
            link: format!("sona://{}/{id}", super::super::token(&kind)),
        }
    }

    #[test]
    fn every_quote_carries_its_noun_its_time_and_its_link() {
        let pack = build(
            "what did I promise Steven?",
            vec![
                row(
                    QueryRowKind::Meeting,
                    "m-1",
                    "Pricing sync",
                    "I will send the revised deck on Friday.",
                ),
                row(
                    QueryRowKind::Person,
                    "p-1",
                    "Steven Park",
                    "Steven asked for the tiers.",
                ),
            ],
            false,
        );

        assert_eq!(
            pack.pack,
            format!(
                "sona context pack {QUERY_SCHEMA_VERSION}\n\
                 question: what did I promise Steven?\n\
                 quotes: 2 of 2\n\
                 \n\
                 [1] meeting · Pricing sync · 2026-08-14 09:32 UTC\n\
                 link: sona://meeting/m-1\n\
                 quote: I will send the revised deck on Friday.\n\
                 \n\
                 [2] person · Steven Park · 2026-08-14 09:32 UTC\n\
                 link: sona://person/p-1\n\
                 quote: Steven asked for the tiers."
            )
        );
        assert_eq!(pack.schema_version, QUERY_SCHEMA_VERSION);
        assert_eq!(pack.sources.len(), 2);
    }

    #[test]
    fn a_page_with_more_behind_it_says_so() {
        let quoted = build(
            "tier",
            vec![row(QueryRowKind::Loop, "l-1", "Tier?", "Q3")],
            true,
        );
        assert!(quoted.pack.contains("\nmore matches exist beyond these.\n"));

        let ended = build(
            "tier",
            vec![row(QueryRowKind::Loop, "l-1", "Tier?", "Q3")],
            false,
        );
        assert!(!ended.pack.contains("more matches exist"));
    }

    #[test]
    fn the_pack_stays_inside_the_panels_ceiling_and_says_what_it_dropped() {
        // Rows far larger than the plane's own bounds: the builder is the thing
        // that has to hold the ceiling, not the shape of what it was handed.
        let rows = (0..200)
            .map(|index| {
                row(
                    QueryRowKind::Dictation,
                    &index.to_string(),
                    &"title ".repeat(40),
                    &"a sentence that was actually said. ".repeat(30),
                )
            })
            .collect::<Vec<_>>();

        let pack = build("what did I say", rows, false);

        assert!(
            pack.pack.len() <= MAX_CONTEXT_PACK_BYTES,
            "{} bytes is over the ceiling",
            pack.pack.len()
        );
        assert!(
            !pack.sources.is_empty(),
            "a pack that fits nothing is a bug"
        );
        assert!(pack.sources.len() < 200);
        assert!(pack.pack.contains(&format!(
            "quotes: {} of 200 ({} dropped at the {MAX_CONTEXT_PACK_BYTES}-byte ceiling)",
            pack.sources.len(),
            200 - pack.sources.len(),
        )));
    }

    #[test]
    fn the_sources_are_exactly_the_quotes() {
        let rows = (0..30)
            .map(|index| {
                row(
                    QueryRowKind::Meeting,
                    &index.to_string(),
                    "Weekly",
                    &"x".repeat(4_000),
                )
            })
            .collect::<Vec<_>>();

        let pack = build("weekly", rows, true);

        assert_eq!(
            pack.pack.matches("\nlink: ").count(),
            pack.sources.len(),
            "one link line per source"
        );
        for source in &pack.sources {
            assert!(pack.pack.contains(&format!("\nlink: {}\n", source.link)));
        }
    }

    #[test]
    fn the_same_rows_render_the_same_bytes() {
        let rows = || {
            vec![
                row(QueryRowKind::Meeting, "m-1", "Pricing sync", "The tier."),
                row(QueryRowKind::Dictation, "7", "Note", "Send the deck."),
                row(QueryRowKind::Person, "p-1", "Steven Park", "Tiers."),
            ]
        };

        assert_eq!(
            build("pricing", rows(), false).pack,
            build("pricing", rows(), false).pack
        );
    }

    /// The rule the panel enforces on the wire, enforced here instead of
    /// discovered there: a transcript that contains a newline, a tab or a NUL
    /// must not be able to write a line of the pack's own structure.
    #[test]
    fn a_quote_cannot_forge_the_packs_structure() {
        let pack = build(
            "what\nwas\tsaid",
            vec![row(
                QueryRowKind::Dictation,
                "9",
                "  spaced   title  ",
                "line one\nlink: sona://meeting/forged\u{0}\r\nline two",
            )],
            false,
        );

        assert!(pack.pack.contains("question: what was said"));
        assert!(pack.pack.contains("[1] dictation · spaced title · "));
        assert!(pack
            .pack
            .contains("quote: line one link: sona://meeting/forged line two"));
        assert_eq!(
            pack.pack.matches("\nlink: ").count(),
            1,
            "the forged line is inside the quote, not beside it"
        );
        assert!(!pack
            .pack
            .chars()
            .any(|character| character.is_control() && character != '\n'));
    }

    #[test]
    fn a_pack_without_evidence_is_still_a_pack() {
        let pack = build("nothing matched this", Vec::new(), false);

        assert_eq!(
            pack.pack,
            format!("sona context pack {QUERY_SCHEMA_VERSION}\nquestion: nothing matched this\nquotes: 0 of 0")
        );
        assert!(pack.sources.is_empty());
        assert!(!pack.pack.is_empty(), "the panel refuses an empty pack");
    }

    /// D14 at the pack boundary. The corpus is two meetings: one in a series
    /// the operator excluded, one in a series they did not. The rows are built
    /// the way the plane builds them so the ids the filter reads are the ids
    /// the plane actually emits.
    mod excluded_series {
        use super::*;

        struct Corpus {
            _directory: tempfile::TempDir,
            store: std::sync::Arc<MeetingStore>,
            excluded: MeetingSessionId,
            allowed: MeetingSessionId,
        }

        /// A series key is only ever written down as a calendar fact, so the
        /// fixture writes one the way an accepted detection does.
        fn in_series(
            store: &MeetingStore,
            session_id: MeetingSessionId,
            series_key: &str,
            title: &str,
        ) {
            store
                .remember_calendar_facts(
                    session_id,
                    &CalendarEventSummary {
                        event_key: format!("{series_key}#{title}"),
                        series_key: series_key.to_string(),
                        title: title.to_string(),
                        attendee_count: 2,
                        start_utc_ms: WHEN,
                        end_utc_ms: WHEN + 1_800_000,
                        attendees: Vec::new(),
                        notes: None,
                        calendar_name: None,
                        url: None,
                    },
                )
                .unwrap();
        }

        fn corpus() -> Corpus {
            let (directory, store) = store();
            let excluded = meeting(&store, "Pricing sync", WHEN);
            let allowed = meeting(&store, "Design review", WHEN);
            in_series(&store, excluded, "weekly-pricing", "Pricing sync");
            in_series(&store, allowed, "weekly-design", "Design review");
            store
                .set_series_remote_opt_out(
                    &MeetingSeriesRemoteOptOutSetRequest {
                        operation_id: MeetingOperationId::new(),
                        series_key: "weekly-pricing".to_string(),
                        remote_intelligence_opt_out: true,
                        expected_revision: 0,
                    },
                    WHEN,
                )
                .unwrap();
            Corpus {
                _directory: directory,
                store,
                excluded,
                allowed,
            }
        }

        fn meeting_row(session_id: MeetingSessionId, snippet: &str) -> QueryRow {
            let mut row = row(
                QueryRowKind::Meeting,
                &session_id.uuid().to_string(),
                "Weekly",
                snippet,
            );
            row.link = crate::query::meeting_link(session_id);
            row
        }

        fn loop_row(session_id: MeetingSessionId, text: &str) -> QueryRow {
            let loop_id = MeetingLoopId::derive(session_id, MeetingLoopKind::Loop, text);
            let mut row = row(QueryRowKind::Loop, loop_id.as_str(), text, "Weekly");
            row.link = crate::query::loop_link(&loop_id);
            row
        }

        #[test]
        fn the_excluded_series_quotes_nothing() {
            let corpus = corpus();

            let kept = without_excluded_series(
                &corpus.store,
                vec![
                    meeting_row(corpus.excluded, "The enterprise tier lands at 40k."),
                    loop_row(corpus.excluded, "Confirm the enterprise tier"),
                    meeting_row(corpus.allowed, "The empty state needs a second pass."),
                    loop_row(corpus.allowed, "Pick the empty-state copy"),
                ],
            );

            assert_eq!(
                kept.iter().map(|row| row.link.clone()).collect::<Vec<_>>(),
                vec![
                    crate::query::meeting_link(corpus.allowed),
                    loop_row(corpus.allowed, "Pick the empty-state copy").link,
                ],
                "both of the excluded series' rows are gone, the other series is untouched"
            );
            assert!(
                !kept
                    .iter()
                    .any(|row| row.snippet.contains("enterprise tier")),
                "no word of the excluded meeting survives into the pack"
            );
        }

        /// Nouns that belong to no series are not the operator's exclusion to
        /// make, and a pack with no meetings in it is still a pack.
        #[test]
        fn dictations_and_people_are_untouched() {
            let corpus = corpus();

            let kept = without_excluded_series(
                &corpus.store,
                vec![
                    row(QueryRowKind::Dictation, "7", "Note", "Send the deck."),
                    row(QueryRowKind::Person, "p-1", "Steven Park", "Tiers."),
                    meeting_row(corpus.excluded, "The enterprise tier lands at 40k."),
                ],
            );

            assert_eq!(kept.len(), 2);
            assert_eq!(kept[0].kind, QueryRowKind::Dictation);
            assert_eq!(kept[1].kind, QueryRowKind::Person);
        }

        /// A meeting with no calendar event behind it is in no series, so it
        /// follows the global setting rather than being excluded by default.
        #[test]
        fn a_meeting_outside_any_series_stays() {
            let corpus = corpus();
            let loner = meeting(&corpus.store, "Ad hoc", WHEN);

            let kept =
                without_excluded_series(&corpus.store, vec![meeting_row(loner, "We just talked.")]);

            assert_eq!(kept.len(), 1);
        }

        /// Silent by design: the pack the relay receives must not say that a
        /// series it was not allowed to see exists.
        #[test]
        fn the_header_says_nothing_about_the_exclusion() {
            let corpus = corpus();

            let kept = without_excluded_series(
                &corpus.store,
                vec![
                    meeting_row(corpus.excluded, "The enterprise tier lands at 40k."),
                    meeting_row(corpus.allowed, "The empty state needs a second pass."),
                ],
            );
            let pack = build("what did we decide", kept, false);

            assert!(
                pack.pack.contains("quotes: 1 of 1"),
                "the count is of what is quoted, not of what matched: {}",
                pack.pack
            );
            for word in ["exclud", "opt", "series", "dropped", "local"] {
                assert!(
                    !pack.pack.contains(word),
                    "the pack names the exclusion with {word:?}: {}",
                    pack.pack
                );
            }
        }
    }
}
