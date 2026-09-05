//! The plane's own rules, without a corpus: order, dedupe, cursor, links.
//!
//! The store-backed half — that a real search over a real fixture store returns
//! a row of every kind, with the reads and the semantic cache behind it — lives
//! in `meeting/store/query_plane.rs`, where the encrypted-store fixture is.

use super::*;

const NOW: i64 = 1_700_000_000_000;

fn row(kind: QueryRowKind, id: &str, when_utc_ms: i64) -> QueryRow {
    QueryRow {
        kind,
        id: id.to_string(),
        title: format!("{id} title"),
        snippet: format!("{id} snippet"),
        when_utc_ms,
        link: format!("sona://{id}"),
    }
}

/// What the sources would hand back for a page that starts at `cursor`: every
/// source filters to its own side of the boundary millisecond, and the merge
/// drops what was already returned.
fn candidates_from(all: &[QueryRow], cursor: Option<&QueryCursor>) -> Vec<QueryRow> {
    all.iter()
        .filter(|row| cursor.is_none_or(|cursor| row.when_utc_ms <= cursor.when_utc_ms))
        .cloned()
        .collect()
}

/// A workflow run keeps its outcome summary when an error replaces `detail`.
///
/// `MeetingActivity` writes `meeting_activity:decision=<event kind>` as its
/// summary. `action` is always the workflow id, and an error replaces
/// `detail`, so the summary must stay available separately.
#[test]
fn a_run_keeps_its_outcome_summary_when_it_errored() {
    let run = |status: &str, error: Option<&str>| {
        event_from_row(QueryEventRow::Run {
            run_id: "run-1".to_string(),
            workflow_id: "meeting_activity".to_string(),
            status: status.to_string(),
            outcome_summary: "meeting_activity:decision=meeting_auto_record_stopped".to_string(),
            error: error.map(str::to_string),
            session_id: None,
            when_utc_ms: NOW,
        })
    };

    let narrated = run("ok", None);
    assert_eq!(narrated.source, QueryEventSource::WorkflowRun);
    assert_eq!(
        narrated.action, "meeting_activity",
        "action carries the workflow id, never the event kind"
    );
    assert_eq!(
        narrated.detail, "meeting_activity:decision=meeting_auto_record_stopped",
        "the event kind remains in detail when the run did not fail"
    );
    assert_eq!(
        serde_json::to_value(&narrated).unwrap()["outcome_summary"],
        "meeting_activity:decision=meeting_auto_record_stopped",
        "the summary is explicit even when detail already carries it"
    );

    let failed = run("failed", Some("the store was locked"));
    assert_eq!(failed.result, QueryEventResult::Failed);
    assert_eq!(
        failed.detail, "the store was locked",
        "the durable error remains visible"
    );
    assert_eq!(
        serde_json::to_value(&failed).unwrap()["outcome_summary"],
        "meeting_activity:decision=meeting_auto_record_stopped",
        "the store-authored summary retains the event kind alongside the error"
    );
}

/// The query-link event is the cross-process boundary: the shell must receive
/// the exact dictation id a deep link named, not merely the destination.
#[test]
fn a_dictation_query_link_carries_its_history_id() {
    let payload = QueryLinkPayload {
        event_schema_version: 1,
        target: QueryLinkTarget::Dictation { history_id: 4218 },
    };

    assert_eq!(
        serde_json::to_value(payload).unwrap(),
        serde_json::json!({
            "event_schema_version": 1,
            "target": { "kind": "dictation", "history_id": 4218 },
        })
    );
}

#[test]
fn the_page_is_newest_first() {
    let (page, _) = merge(
        vec![
            row(QueryRowKind::Meeting, "old", NOW - 5_000),
            row(QueryRowKind::Dictation, "new", NOW),
            row(QueryRowKind::Person, "middle", NOW - 1_000),
        ],
        None,
        10,
    );

    assert_eq!(
        page.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["new", "middle", "old"]
    );
}

#[test]
fn one_millisecond_is_broken_by_kind_then_id() {
    let (page, _) = merge(
        vec![
            row(QueryRowKind::Loop, "loop", NOW),
            row(QueryRowKind::Dictation, "b", NOW),
            row(QueryRowKind::Meeting, "meeting", NOW),
            row(QueryRowKind::Dictation, "a", NOW),
        ],
        None,
        10,
    );

    assert_eq!(
        page.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["meeting", "a", "b", "loop"],
        "kind declaration order, then id, so the order is total"
    );
}

/// Lexical candidates are pushed before semantic ones, so this is the rule that
/// makes "the words that literally matched" the snippet a reader sees.
#[test]
fn the_first_candidate_for_a_noun_wins_the_row() {
    let session = "1e1a5f0e-0000-4000-8000-000000000001";
    let mut lexical = row(QueryRowKind::Meeting, session, NOW);
    lexical.snippet = "the tier the trial converts into".to_string();
    let mut semantic = row(QueryRowKind::Meeting, session, NOW);
    semantic.snippet = "pricing came back at the end".to_string();

    let (page, _) = merge(vec![lexical, semantic], None, 10);

    assert_eq!(page.len(), 1, "one meeting, one row");
    assert_eq!(page[0].snippet, "the tier the trial converts into");
}

/// A row of a different kind that happens to share an id is a different noun.
#[test]
fn dedupe_is_per_kind() {
    let shared = "42";
    let (page, _) = merge(
        vec![
            row(QueryRowKind::Dictation, shared, NOW),
            row(QueryRowKind::Person, shared, NOW),
        ],
        None,
        10,
    );

    assert_eq!(page.len(), 2);
}

#[test]
fn paging_repeats_nothing_and_skips_nothing() {
    let all = vec![
        row(QueryRowKind::Meeting, "m1", NOW),
        row(QueryRowKind::Dictation, "9", NOW),
        row(QueryRowKind::Person, "p1", NOW - 1),
        row(QueryRowKind::Loop, "l1", NOW - 1),
        row(QueryRowKind::Meeting, "m2", NOW - 2),
    ];

    let mut seen = Vec::new();
    let mut cursor = None;
    for _ in 0..4 {
        let (page, next) = merge(candidates_from(&all, cursor.as_ref()), cursor.as_ref(), 2);
        seen.extend(page.iter().map(|row| row.id.clone()));
        match next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    assert_eq!(
        seen,
        ["m1", "9", "p1", "l1", "m2"],
        "every row exactly once, in one order, across pages of two"
    );
}

/// Dictation search pages by row id rather than by time, so the cursor has to
/// carry the position that source understands.
#[test]
fn the_cursor_carries_the_oldest_dictation_returned() {
    let (_, cursor) = merge(
        vec![
            row(QueryRowKind::Dictation, "90", NOW),
            row(QueryRowKind::Dictation, "80", NOW - 1),
            row(QueryRowKind::Meeting, "m", NOW - 2),
        ],
        None,
        2,
    );

    let cursor = cursor.expect("more rows remain");
    assert_eq!(cursor.dictation_id, Some(80));
    assert_eq!(cursor.kind, QueryRowKind::Dictation);
    assert_eq!(cursor.when_utc_ms, NOW - 1);
}

/// Nothing came back from the dictation source on this page, so the position it
/// was already given has to survive: dropping it would replay dictations the
/// caller has already seen.
#[test]
fn a_page_without_dictations_keeps_the_dictation_position() {
    let carried = QueryCursor {
        when_utc_ms: NOW,
        kind: QueryRowKind::Dictation,
        id: "80".to_string(),
        dictation_id: Some(80),
    };

    let (_, cursor) = merge(
        vec![
            row(QueryRowKind::Meeting, "m1", NOW - 1),
            row(QueryRowKind::Meeting, "m2", NOW - 2),
            row(QueryRowKind::Meeting, "m3", NOW - 3),
        ],
        Some(&carried),
        2,
    );

    assert_eq!(cursor.expect("more rows remain").dictation_id, Some(80));
}

#[test]
fn a_page_that_ends_the_result_has_no_cursor() {
    let (page, cursor) = merge(
        vec![
            row(QueryRowKind::Meeting, "m1", NOW),
            row(QueryRowKind::Meeting, "m2", NOW - 1),
        ],
        None,
        2,
    );

    assert_eq!(page.len(), 2);
    assert!(cursor.is_none(), "a full page is not evidence of another");
}

#[test]
fn every_scope_narrows_to_its_own_kinds() {
    for kind in [
        QueryRowKind::Meeting,
        QueryRowKind::Dictation,
        QueryRowKind::Person,
        QueryRowKind::Loop,
    ] {
        assert!(QueryScope::All.includes(kind), "{kind:?} is in all");
    }
    // Declared by the row union, produced by no scope: see `QueryRowKind`.
    assert!(!QueryScope::All.includes(QueryRowKind::Series));
    assert!(!QueryScope::All.includes(QueryRowKind::Receipt));

    assert!(QueryScope::Meetings.includes(QueryRowKind::Meeting));
    assert!(!QueryScope::Meetings.includes(QueryRowKind::Dictation));
    assert!(QueryScope::Dictations.includes(QueryRowKind::Dictation));
    assert!(!QueryScope::Dictations.includes(QueryRowKind::Person));
    assert!(QueryScope::People.includes(QueryRowKind::Person));
    assert!(!QueryScope::People.includes(QueryRowKind::Loop));
    assert!(QueryScope::Loops.includes(QueryRowKind::Loop));
    assert!(!QueryScope::Loops.includes(QueryRowKind::Meeting));
}

#[test]
fn tokens_are_folded_and_anded() {
    assert!(tokens("").is_empty());
    assert!(tokens("   \n\t ").is_empty());
    assert_eq!(tokens("Pricing  TIER"), ["pricing", "tier"]);

    let haystack = "Which tier does the trial convert into?";
    assert!(matches_every_token(haystack, &tokens("tier trial")));
    assert!(
        !matches_every_token(haystack, &tokens("tier billing")),
        "every token has to be present, like the FTS5 halves"
    );
}

#[test]
fn snippets_are_cut_on_a_character_boundary() {
    assert_eq!(bounded("  trimmed  ", 40), "trimmed");
    assert_eq!(bounded("ええええ", 2), "ええ…");
    assert_eq!(bounded("exact", 5), "exact");
}

#[test]
fn a_page_size_is_bounded_and_never_zero() {
    assert_eq!(page_size(None), Ok(DEFAULT_PAGE_SIZE));
    assert_eq!(page_size(Some(7)), Ok(7));
    assert_eq!(page_size(Some(usize::MAX)), Ok(MAX_PAGE_SIZE));
    assert_eq!(page_size(Some(0)), Err(QueryError::InvalidRequest));
}

/// The link column is the plane's contract with everything downstream: an agent
/// cites it, a human clicks it, `deeplink.rs` parses it. Its shapes are asserted
/// here and round-tripped through the route table in `deeplink.rs`.
#[test]
fn links_have_one_shape_per_noun() {
    use crate::meeting::loop_types::{MeetingLoopId, MeetingLoopKind};
    use crate::meeting::people_types::PersonId;

    let session_id = MeetingSessionId::new();
    let person_id = PersonId::new();
    let loop_id = MeetingLoopId::derive(session_id, MeetingLoopKind::Loop, "Trial tier");

    assert_eq!(
        meeting_link(session_id),
        format!("sona://meeting/{}", session_id.uuid())
    );
    assert_eq!(
        person_link(person_id),
        format!("sona://person/{}", person_id.uuid())
    );
    assert_eq!(
        loop_link(&loop_id),
        format!("sona://loop/{}", loop_id.as_str())
    );
    assert_eq!(dictation_link(4218), "sona://dictation/4218");
    assert_eq!(
        search_link("promised Steven?"),
        "sona://search?q=promised+Steven%3F"
    );
}

/// A blank query and a corpus with nothing in it are the same empty array, and
/// an agent that cannot tell them apart asks the same question again. This is
/// the page [`search`] hands back before it opens the store, so its token is
/// the only thing that says which nothing it is.
#[test]
fn a_page_that_searched_for_nothing_says_so_rather_than_looking_empty() {
    let page = QuerySearchPage::nothing_searchable();

    assert!(page.entries.is_empty());
    assert_eq!(
        serde_json::to_value(&page).unwrap()["reason"],
        "no_searchable_tokens",
        "the token an outside reader parses"
    );
}
