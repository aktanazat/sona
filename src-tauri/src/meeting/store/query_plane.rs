//! The meeting store's half of the one query plane: read paths, plus the
//! disposable semantic chunk cache the plane scans.
//!
//! Every meeting read the plane needs lives here rather than in `crate::query`
//! because the store owns this database — its key, its retention sweep, its
//! cascade on deletion. A second connection opened from outside would own none
//! of that. Nothing in this file mutates a meeting: the only writes are to
//! `meeting_semantic_chunks` and `meeting_semantic_index_state`, which are a
//! cache of vectors derived from rows this file also reads, rebuildable from
//! them at any time, and deleted with the session they describe.

use super::{MeetingStore, StoreError};
use crate::meeting::types::{MeetingSessionId, OperationReceipt};
use rusqlite::{params, params_from_iter, OptionalExtension};

/// One meeting as the query plane reports it. `snippet` is the text that
/// matched, not a summary of the meeting: the plane's job is to show the reader
/// why a row is in front of them.
pub(crate) struct MeetingQueryCandidate {
    pub session_id: MeetingSessionId,
    pub title: String,
    pub when_utc_ms: i64,
    pub snippet: String,
}

/// One stored chunk vector, without its text.
///
/// The scan reads every comparable vector in the corpus, so it deliberately
/// leaves `text` in the database: a few thousand 256-lane vectors are a few
/// megabytes, the same transcript as strings is not, and only the handful of
/// chunks that win need their text read back.
pub(crate) struct SemanticChunkVector {
    pub chunk_id: i64,
    pub session_id: MeetingSessionId,
    pub when_utc_ms: i64,
    pub embedding: Vec<u8>,
}

/// What one session contributes to the semantic index.
///
/// `key` is what the chunks were built from — the current artifact revision and
/// the current transcript revision. It is compared, never parsed: a different
/// key means the cache is stale and the session is indexed again.
pub(crate) struct SemanticIndexInputs {
    pub key: String,
    pub summaries: Vec<String>,
    pub transcript: Vec<String>,
}

/// One row of the merged event stream, before the plane renders it.
pub(crate) enum QueryEventRow {
    Receipt {
        receipt: OperationReceipt,
        when_utc_ms: i64,
    },
    Run {
        run_id: String,
        workflow_id: String,
        status: String,
        outcome_summary: String,
        error: Option<String>,
        session_id: Option<MeetingSessionId>,
        when_utc_ms: i64,
    },
}

/// The newest current artifact revision id for the session under join alias
/// `m`. The companion of [`super::CURRENT_ARTIFACT_CONTENT`], which quotes the
/// same revision's content.
const CURRENT_ARTIFACT_ID: &str = "SELECT a.artifact_id
                       FROM meeting_artifact_revisions a
                      WHERE a.session_id = m.id
                        AND a.state = 'current'
                        AND a.content_json IS NOT NULL
                      ORDER BY a.generated_at_utc_ms DESC LIMIT 1";

/// What a session's chunks were built from, as SQL. Both halves move whenever
/// the meeting's words change: a regenerated artifact mints a new id, and an
/// edited transcript mints a new revision.
const SEMANTIC_INDEX_KEY: &str = "COALESCE(({CURRENT_ARTIFACT_ID}), '-') || ':' ||
                     COALESCE(m.current_transcript_revision_id, '-')";

/// The longest snippet the plane will carry per row. Evidence, not payload:
/// every row of every page rides into a context pack eventually, and the
/// question a reader is answering is answered by a sentence.
const MAX_SNIPPET_BYTES: usize = 320;

fn semantic_index_key_sql() -> String {
    SEMANTIC_INDEX_KEY.replace("{CURRENT_ARTIFACT_ID}", CURRENT_ARTIFACT_ID)
}

impl MeetingStore {
    /// Every retained meeting whose indexed text matches `query`, newest first.
    ///
    /// One row per meeting: `meeting_search_documents` holds the title, each
    /// transcript segment and each manual note separately, so a query that
    /// appears in nine segments of one meeting would otherwise return that
    /// meeting nine times. The group keeps the best-scoring document's text as
    /// the snippet — `bm25` is lowest for the strongest match, and SQLite's
    /// bare-column rule takes the other columns from the row that produced the
    /// aggregate — while the page itself stays in recency order, which is the
    /// order every search surface in this app already has.
    ///
    /// The match is scored in a `MATERIALIZED` CTE rather than as
    /// `MIN(bm25(…))` over the join: SQLite refuses an fts5 auxiliary function
    /// in an aggregate context ("unable to use function bm25 in the requested
    /// context"), and a plain subquery is flattened back into one. The hint is
    /// what keeps the score a value the group can aggregate; it is not an
    /// optimisation, and `query_plane` tests fail without it.
    pub(crate) fn query_meetings_lexical(
        &self,
        query: &str,
        before_utc_ms: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MeetingQueryCandidate>, StoreError> {
        let Some(match_query) = super::meeting_fts_match_query(query) else {
            return Ok(Vec::new());
        };
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "WITH matched AS MATERIALIZED (
                    SELECT d.session_id AS session_id, m.title AS title,
                           m.created_at_utc_ms AS created_at_utc_ms,
                           d.content AS content,
                           bm25(meeting_search_fts) AS score
                      FROM meeting_search_fts
                      JOIN meeting_search_documents d ON d.id = meeting_search_fts.rowid
                      JOIN meeting_sessions m ON m.id = d.session_id
                     WHERE meeting_search_fts MATCH ?1
                       AND m.phase != 'deleting'
                       AND (?2 IS NULL OR m.created_at_utc_ms <= ?2)
             )
             SELECT session_id, title, created_at_utc_ms, content, MIN(score)
               FROM matched
              GROUP BY session_id
              ORDER BY created_at_utc_ms DESC, session_id ASC
              LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                match_query,
                before_utc_ms,
                i64::try_from(limit).map_err(|_| StoreError::Invalid)?,
            ],
            candidate_columns,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Every comparable chunk vector in the retained corpus.
    ///
    /// An exact scan, for the same reason dictation recall scans exactly: a
    /// corpus of hundreds of meetings is thousands of dot products, and an
    /// approximate index would add a second thing to keep true about a cache
    /// whose whole value is that it can be thrown away.
    pub(crate) fn query_semantic_chunk_vectors(
        &self,
        model_revision: &str,
        before_utc_ms: Option<i64>,
    ) -> Result<Vec<SemanticChunkVector>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT c.chunk_id, c.session_id, m.created_at_utc_ms, c.embedding
               FROM meeting_semantic_chunks c
               JOIN meeting_sessions m ON m.id = c.session_id
              WHERE c.model_revision = ?1
                AND m.phase != 'deleting'
                AND (?2 IS NULL OR m.created_at_utc_ms <= ?2)",
        )?;
        let rows = statement.query_map(params![model_revision, before_utc_ms], |row| {
            Ok(SemanticChunkVector {
                chunk_id: row.get(0)?,
                session_id: MeetingSessionId::from_uuid(
                    super::parse_uuid(&row.get::<_, String>(1)?).map_err(super::to_sql_error)?,
                ),
                when_utc_ms: row.get(2)?,
                embedding: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// The meetings behind a set of winning chunks, each snippet being the
    /// chunk that won rather than the meeting's first words.
    pub(crate) fn query_meetings_by_chunk(
        &self,
        chunk_ids: &[i64],
    ) -> Result<Vec<MeetingQueryCandidate>, StoreError> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (1..=chunk_ids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT c.session_id, m.title, m.created_at_utc_ms, c.text, 0.0
               FROM meeting_semantic_chunks c
               JOIN meeting_sessions m ON m.id = c.session_id
              WHERE c.chunk_id IN ({placeholders})
                AND m.phase != 'deleting'
              ORDER BY m.created_at_utc_ms DESC, c.session_id ASC"
        ))?;
        let rows = statement.query_map(
            params_from_iter(chunk_ids.iter().map(|chunk_id| *chunk_id)),
            candidate_columns,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// The ledger headline of each listed meeting that has one, from its
    /// current artifact. One read for a page of rows: the chat tools attach
    /// it to the meeting rows a search or a listing returns, so the model can
    /// tell where a meeting landed without opening it.
    pub(crate) fn query_ledger_headlines(
        &self,
        session_ids: &[MeetingSessionId],
    ) -> Result<Vec<(MeetingSessionId, String)>, StoreError> {
        if session_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (1..=session_ids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT m.id, json_extract(({content}), '$.ledger.headline')
               FROM meeting_sessions m
              WHERE m.id IN ({placeholders})
                AND m.phase != 'deleting'",
            content = super::CURRENT_ARTIFACT_CONTENT,
        ))?;
        let rows = statement.query_map(
            params_from_iter(session_ids.iter().map(|session_id| super::id(*session_id))),
            |row| {
                Ok((
                    MeetingSessionId::from_uuid(
                        super::parse_uuid(&row.get::<_, String>(0)?)
                            .map_err(super::to_sql_error)?,
                    ),
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )?;
        let mut headlines = Vec::new();
        for row in rows {
            let (session_id, headline) = row?;
            if let Some(headline) = headline.map(|text| text.trim().to_string()) {
                if !headline.is_empty() {
                    headlines.push((session_id, headline));
                }
            }
        }
        Ok(headlines)
    }

    /// Retained meetings whose semantic chunks are missing, built from older
    /// words, or written by a different model — newest first, because the
    /// meeting a reader is most likely to ask about is the one that just ended.
    pub(crate) fn semantic_index_targets(
        &self,
        model_revision: &str,
        limit: usize,
    ) -> Result<Vec<MeetingSessionId>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT m.id
               FROM meeting_sessions m
               LEFT JOIN meeting_semantic_index_state s ON s.session_id = m.id
              WHERE m.phase = 'review_ready'
                AND m.current_transcript_revision_id IS NOT NULL
                AND (s.session_id IS NULL
                     OR s.model_revision != ?1
                     OR s.indexed_key != ({key}))
              ORDER BY m.created_at_utc_ms DESC
              LIMIT ?2",
            key = semantic_index_key_sql(),
        ))?;
        let rows = statement.query_map(
            params![
                model_revision,
                i64::try_from(limit).map_err(|_| StoreError::Invalid)?
            ],
            |row| {
                Ok(MeetingSessionId::from_uuid(
                    super::parse_uuid(&row.get::<_, String>(0)?).map_err(super::to_sql_error)?,
                ))
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// The text one session contributes to the semantic index.
    ///
    /// The summary and the ledger headline come from the current artifact —
    /// they are the reading of the meeting FTS5 never sees, which is the gap
    /// this index exists to close. The transcript comes from
    /// `meeting_search_documents` rather than the segment table, because that
    /// is where the store already keeps each segment's *effective* text with
    /// human edits applied; re-deriving it here would be a second answer to a
    /// question the store has already answered.
    pub(crate) fn semantic_index_inputs(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Option<SemanticIndexInputs>, StoreError> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                &format!(
                    "SELECT ({key}),
                            json_extract(({content}), '$.summary.text'),
                            json_extract(({content}), '$.ledger.headline')
                       FROM meeting_sessions m
                      WHERE m.id = ?1 AND m.phase != 'deleting'",
                    key = semantic_index_key_sql(),
                    content = super::CURRENT_ARTIFACT_CONTENT,
                ),
                params![super::id(session_id)],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((key, summary, headline)) = row else {
            return Ok(None);
        };
        let summaries = [summary, headline]
            .into_iter()
            .flatten()
            .filter(|text| !text.trim().is_empty())
            .collect();
        let mut statement = connection.prepare(
            "SELECT content
               FROM meeting_search_documents
              WHERE session_id = ?1 AND entity_kind = 'segment'
              ORDER BY id",
        )?;
        let transcript = statement
            .query_map(params![super::id(session_id)], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(SemanticIndexInputs {
            key,
            summaries,
            transcript,
        }))
    }

    /// Replace one session's chunks with a freshly embedded set.
    ///
    /// Delete-then-insert inside one transaction, so a reader never sees a
    /// half-reindexed meeting, and the state row is written last: a crash
    /// between the two leaves the session looking stale, which costs one
    /// re-index and cannot leave a stale vector claiming to be current.
    pub(crate) fn replace_semantic_chunks(
        &self,
        session_id: MeetingSessionId,
        key: &str,
        model_revision: &str,
        indexed_at_utc_ms: i64,
        chunks: &[(String, Vec<u8>)],
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let session = super::id(session_id);
        transaction.execute(
            "DELETE FROM meeting_semantic_chunks WHERE session_id = ?1",
            params![session],
        )?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO meeting_semantic_chunks (session_id, text, embedding, model_revision)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (text, embedding) in chunks {
                statement.execute(params![session, text, embedding, model_revision])?;
            }
        }
        transaction.execute(
            "INSERT INTO meeting_semantic_index_state (
                session_id, indexed_key, model_revision, indexed_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id) DO UPDATE SET
                indexed_key = excluded.indexed_key,
                model_revision = excluded.model_revision,
                indexed_at_utc_ms = excluded.indexed_at_utc_ms",
            params![session, key, model_revision, indexed_at_utc_ms],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// One page of the merged ledger: operation receipts and workflow runs in
    /// one order, newest first.
    ///
    /// Both halves are read here rather than merged from two paged reads
    /// because they share a database and an order. `before` is the position of
    /// the last row the caller saw — `(time, id)`, descending on both, which is
    /// the order `workflow_runs` already pages in, so a cursor taken from
    /// either half means the same thing to the other.
    pub(crate) fn query_events(
        &self,
        before: Option<(i64, String)>,
        limit: usize,
    ) -> Result<Vec<QueryEventRow>, StoreError> {
        let (before_ms, before_id) = match before {
            Some((when, id)) => (Some(when), Some(id)),
            None => (None, None),
        };
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT source, id, when_ms, receipt_json, session_id, workflow_id, status,
                    outcome_summary, error
               FROM (
                    SELECT 'receipt' AS source, r.operation_id AS id,
                           r.created_at_utc_ms AS when_ms, r.receipt_json AS receipt_json,
                           r.session_id AS session_id, NULL AS workflow_id, NULL AS status,
                           NULL AS outcome_summary, NULL AS error
                      FROM meeting_operation_receipts r
                    UNION ALL
                    SELECT 'run', w.id, w.started_at_utc_ms, NULL,
                           json_extract(e.payload_json, '$.session_id'), w.workflow_id, w.status,
                           w.outcome_summary, w.error
                      FROM workflow_runs w
                      JOIN workflow_events e ON e.id = w.event_id
               )
              WHERE (?1 IS NULL OR when_ms < ?1 OR (when_ms = ?1 AND id < ?2))
              ORDER BY when_ms DESC, id DESC
              LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                before_ms,
                before_id,
                i64::try_from(limit).map_err(|_| StoreError::Invalid)?,
            ],
            |row| {
                let session_id = row
                    .get::<_, Option<String>>(4)?
                    .as_deref()
                    .map(super::parse_uuid)
                    .transpose()
                    .map_err(super::to_sql_error)?
                    .map(MeetingSessionId::from_uuid);
                match row.get::<_, String>(0)?.as_str() {
                    "receipt" => Ok(QueryEventRow::Receipt {
                        receipt: super::decode_json(&row.get::<_, String>(3)?)
                            .map_err(super::to_sql_error)?,
                        when_utc_ms: row.get(2)?,
                    }),
                    _ => Ok(QueryEventRow::Run {
                        run_id: row.get(1)?,
                        workflow_id: row.get(5)?,
                        status: row.get(6)?,
                        outcome_summary: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                        error: row.get(8)?,
                        session_id,
                        when_utc_ms: row.get(2)?,
                    }),
                }
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Where one event id sits in the merged order, or `None` when nothing
    /// carries that id any more — a receipt goes with the meeting it described.
    pub(crate) fn query_event_position(&self, event_id: &str) -> Result<Option<i64>, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT created_at_utc_ms FROM meeting_operation_receipts WHERE operation_id = ?1
                 UNION ALL
                 SELECT started_at_utc_ms FROM workflow_runs WHERE id = ?1
                 LIMIT 1",
                params![event_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(Into::into)
    }
}

fn candidate_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<MeetingQueryCandidate> {
    Ok(MeetingQueryCandidate {
        session_id: MeetingSessionId::from_uuid(
            super::parse_uuid(&row.get::<_, String>(0)?).map_err(super::to_sql_error)?,
        ),
        title: row.get(1)?,
        when_utc_ms: row.get(2)?,
        snippet: super::bounded_text(&row.get::<_, String>(3)?, MAX_SNIPPET_BYTES),
    })
}

#[cfg(test)]
mod tests {
    //! The plane over a corpus that holds one of every noun.
    //!
    //! `crate::query::tests` owns the rules that need no store — order, dedupe,
    //! cursor, link shapes. What needs one is everything this file is: that
    //! each source reads, matches the words a reader typed, and addresses the
    //! row it returns. So the assertions here are per source, and the fixture
    //! is built the way the app builds it — a typed note is what puts a
    //! meeting in the FTS5 index and what mints the receipt the event stream
    //! reports, rather than two hand-written rows claiming to be both.

    use super::super::workflow_core_tests::{event, inputs, meeting, person, store, transcript};
    use super::*;
    use crate::managers::history::semantic::encode_vector;
    use crate::managers::history::{HistoryEntry, HistoryMatchKind};
    use crate::meeting::loop_types::{MeetingLoopId, MeetingLoopKind};
    use crate::meeting::people_types::PersonId;
    use crate::meeting::types::{ManualNote, ManualNoteId, MeetingOperationId};
    use crate::meeting::workflow_types::WorkflowEventKind;
    use crate::query::{
        assemble, dictation_link, event_from_row, loop_link, meeting_link, person_link,
        QueryEventResult, QueryEventSource, QueryPageReason, QueryRowKind, QueryScope,
        QuerySearchPage, SearchRequest, QUERY_SCHEMA_VERSION,
    };
    use std::sync::Arc;
    use tempfile::TempDir;
    use uuid::Uuid;

    const NOW: i64 = 1_700_000_000_000;

    /// The one word every noun in this corpus answers to, so a single search
    /// is evidence about all of them at once.
    const QUERY: &str = "dana";

    const TITLE: &str = "Pricing review";
    const SEGMENT: &str = "Dana asked which tier the trial converts into.";
    const NOTE: &str = "Dana still owes the tier comparison.";
    const SUMMARY: &str = "Pricing stayed open.";
    const HEADLINE: &str = "Dana's tier question stayed open.";
    const LOOP_TEXT: &str = "Dana's trial conversion tier";
    const DICTATION_ID: i64 = 4218;
    const DICTATION_TEXT: &str = "Remind Dana about the tier comparison.";

    /// A revision string no model in this process claims, so the cache under
    /// test is the one these tests wrote.
    const MODEL_REVISION: &str = "query-plane-test-1";

    struct Corpus {
        _directory: TempDir,
        store: Arc<MeetingStore>,
        session_id: MeetingSessionId,
        person_id: PersonId,
        loop_id: MeetingLoopId,
    }

    /// One meeting whose transcript, notes and ledger all name Dana, the person
    /// herself, and the continuity run that makes her open loop reachable.
    fn corpus() -> Corpus {
        let (directory, store) = store();
        let session_id = meeting(&store, TITLE, NOW);
        transcript(&store, session_id, SEGMENT);
        // Before the artifact, deliberately: a note marks the current artifact
        // out of date, and a ledger nobody can read is a ledger with no loops.
        note(&store, session_id);
        artifact(&store, session_id);
        finalize(&store, session_id);
        let person_id = person(&store, "Dana Reyes", &["Dana"], &["dana@example.com"]);
        let loop_id = MeetingLoopId::derive(session_id, MeetingLoopKind::Loop, LOOP_TEXT);
        Corpus {
            _directory: directory,
            store,
            session_id,
            person_id,
            loop_id,
        }
    }

    /// A note the reader typed, through the mutation that rebuilds the search
    /// documents and records the receipt.
    fn note(store: &MeetingStore, session_id: MeetingSessionId) {
        store
            .create_note(
                MeetingOperationId::new(),
                NOW,
                &ManualNote {
                    note_id: ManualNoteId::new(),
                    session_id,
                    start_offset_ns: None,
                    end_offset_ns: None,
                    body: NOTE.to_string(),
                    revision: 0,
                    created_at_utc_ms: NOW,
                    updated_at_utc_ms: NOW,
                },
                0,
            )
            .unwrap();
    }

    /// The current artifact revision, generated from the transcript the session
    /// already points at, with one thread nobody answered.
    fn artifact(store: &MeetingStore, session_id: MeetingSessionId) {
        let content = serde_json::json!({
            "summary": {"text": SUMMARY, "citations": []},
            "outline": [],
            "decisions": [],
            "action_items": [],
            "key_questions": [],
            "risks": [],
            "follow_up_draft": {"text": "", "citations": []},
            "ledger": {
                "headline": HEADLINE,
                "threads": [{
                    "topic": LOOP_TEXT,
                    "state": "open",
                    "substantive": true,
                    "receipt": {
                        "quote": "which tier does the trial convert into",
                        "speaker": "Dana",
                        "t_ms": 12000,
                        "citations": []
                    },
                    "owner": "Dana Reyes"
                }],
                "open_loops": [],
                "commitments": [],
                "stances": [],
                "caveats": [],
                "receipts": {"status": "verified"}
            }
        });
        let artifact_id = Uuid::new_v4();
        let connection = store.connection().unwrap();
        let transcript_revision_id: String = connection
            .query_row(
                "SELECT current_transcript_revision_id FROM meeting_sessions WHERE id = ?1",
                params![session_id.uuid().to_string()],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO meeting_artifact_revisions (
                    artifact_id, session_id, transcript_revision_id, input_revision,
                    template_id, template_version, generation_key, state,
                    content_json, generated_at_utc_ms
                 ) VALUES (?1, ?2, ?3, 0, 'test', 1, ?4, 'current', ?5, ?6)",
                params![
                    artifact_id.to_string(),
                    session_id.uuid().to_string(),
                    transcript_revision_id,
                    format!("test-{artifact_id}"),
                    content.to_string(),
                    NOW
                ],
            )
            .unwrap();
    }

    /// The continuity run. An open loop is only in the inbox once the workflow
    /// that reads loops has succeeded for its meeting, which is the same gate
    /// the review screen and the pre-meeting brief pass through.
    fn finalize(store: &MeetingStore, session_id: MeetingSessionId) {
        store
            .record_and_run_workflow_event(
                event(
                    WorkflowEventKind::MeetingFinalized,
                    serde_json::json!({
                        "session_id": session_id.uuid().to_string(),
                        "known_vocabulary": []
                    }),
                    "query-plane-finalized",
                ),
                &inputs(),
            )
            .unwrap();
    }

    /// The one row the plane does not read itself: dictation search is async
    /// and behind `HistoryManager`, so the seam takes its rows as an argument.
    /// This one has no title, which is the ordinary case. Its timestamp is in
    /// UNIX seconds, the unit the history store keeps, one second before the
    /// meeting.
    fn dictation() -> HistoryEntry {
        HistoryEntry {
            id: DICTATION_ID,
            file_name: "4218.wav".to_string(),
            timestamp: NOW / 1000 - 1,
            saved: true,
            title: String::new(),
            transcription_text: "reminder to send dana the tier comparison".to_string(),
            post_processed_text: Some(DICTATION_TEXT.to_string()),
            post_process_requested: true,
            parent_id: None,
            match_kind: Some(HistoryMatchKind::Text),
        }
    }

    fn search(
        store: &MeetingStore,
        scope: QueryScope,
        dictations: Vec<HistoryEntry>,
    ) -> QuerySearchPage {
        assemble(
            store,
            None,
            dictations,
            SearchRequest {
                scope,
                query: QUERY,
                limit: 25,
                cursor: None,
            },
        )
        .unwrap()
    }

    fn row_of(page: &QuerySearchPage, kind: QueryRowKind) -> &crate::query::QueryRow {
        page.entries
            .iter()
            .find(|row| row.kind == kind)
            .unwrap_or_else(|| panic!("a {kind:?} row: {:?}", page.entries))
    }

    #[test]
    fn every_noun_in_the_corpus_comes_back_addressed() {
        let corpus = corpus();

        let page = search(&corpus.store, QueryScope::All, vec![dictation()]);

        assert_eq!(page.schema_version, QUERY_SCHEMA_VERSION);
        assert_eq!(
            page.entries
                .iter()
                .map(|row| (row.kind, row.link.clone()))
                .collect::<Vec<_>>(),
            vec![
                (QueryRowKind::Meeting, meeting_link(corpus.session_id)),
                // The loop was raised in that meeting, so it shares its
                // millisecond and sorts by kind.
                (QueryRowKind::Loop, loop_link(&corpus.loop_id)),
                (QueryRowKind::Dictation, dictation_link(DICTATION_ID)),
                (QueryRowKind::Person, person_link(corpus.person_id)),
            ],
            "one row per noun, newest first, each at a sona:// address"
        );
        assert!(page.next_cursor.is_none(), "four rows are not a full page");
    }

    /// History keeps UNIX seconds and the plane orders by milliseconds. A row
    /// that was not converted would date to 1970 and sort below every meeting.
    #[test]
    fn a_dictation_from_today_sorts_before_a_meeting_from_yesterday() {
        let (_directory, store) = store();
        let yesterday = NOW - 86_400_000;
        let session_id = meeting(&store, TITLE, yesterday);
        transcript(&store, session_id, SEGMENT);
        note(&store, session_id);
        let mut today = dictation();
        today.timestamp = NOW / 1000;

        let page = search(&store, QueryScope::All, vec![today]);

        assert_eq!(
            page.entries
                .iter()
                .map(|row| (row.kind, row.when_utc_ms))
                .collect::<Vec<_>>(),
            vec![
                (QueryRowKind::Dictation, NOW),
                (QueryRowKind::Meeting, yesterday),
            ],
            "the dictation is dated in milliseconds and sorts newest first"
        );
    }

    /// Every row's snippet is the text that put it in front of the reader.
    #[test]
    fn each_row_says_why_it_matched() {
        let corpus = corpus();

        let page = search(&corpus.store, QueryScope::All, vec![dictation()]);

        let meeting_row = row_of(&page, QueryRowKind::Meeting);
        assert_eq!(meeting_row.title, TITLE);
        assert!(
            [SEGMENT, NOTE].contains(&meeting_row.snippet.as_str()),
            "the document that matched, not the meeting's first words: {:?}",
            meeting_row.snippet
        );

        let dictation_row = row_of(&page, QueryRowKind::Dictation);
        assert_eq!(
            (dictation_row.title.as_str(), dictation_row.snippet.as_str()),
            (DICTATION_TEXT, DICTATION_TEXT),
            "the post-processed text is the dictation, and an untitled row is titled by it"
        );

        let person_row = row_of(&page, QueryRowKind::Person);
        assert_eq!(person_row.title, "Dana Reyes");
        assert_eq!(
            person_row.snippet, "Dana",
            "a person with no meeting behind her is described by the names she answers to"
        );

        let loop_row = row_of(&page, QueryRowKind::Loop);
        assert_eq!(loop_row.title, LOOP_TEXT);
        assert_eq!(
            loop_row.snippet, TITLE,
            "a loop's own words are its title, so the snippet is where it was raised"
        );
    }

    /// Names are matched over everything a person answers to, which is the
    /// people source's whole index.
    #[test]
    fn a_person_is_found_by_alias_and_by_calendar_address() {
        let corpus = corpus();

        for query in ["dana reyes", "Dana", "dana@example.com"] {
            let page = assemble(
                &corpus.store,
                None,
                Vec::new(),
                SearchRequest {
                    scope: QueryScope::People,
                    query,
                    limit: 25,
                    cursor: None,
                },
            )
            .unwrap();
            assert_eq!(
                page.entries
                    .iter()
                    .map(|row| row.id.as_str())
                    .collect::<Vec<_>>(),
                [corpus.person_id.uuid().to_string().as_str()],
                "{query:?}"
            );
        }
    }

    /// A narrowed scope is a source that is never read, not a page that is
    /// filtered afterwards — which is why the caller only fetches dictations
    /// for a scope that includes them.
    #[test]
    fn a_scope_reads_only_its_own_source() {
        let corpus = corpus();

        for (scope, kind) in [
            (QueryScope::Meetings, QueryRowKind::Meeting),
            (QueryScope::Dictations, QueryRowKind::Dictation),
            (QueryScope::People, QueryRowKind::Person),
            (QueryScope::Loops, QueryRowKind::Loop),
        ] {
            let dictations = match scope {
                QueryScope::Dictations => vec![dictation()],
                _ => Vec::new(),
            };

            let page = search(&corpus.store, scope, dictations);

            assert_eq!(
                page.entries.iter().map(|row| row.kind).collect::<Vec<_>>(),
                [kind],
                "{scope:?}"
            );
        }
    }

    /// Nothing searchable, nothing read: the sources are never asked, so a
    /// corpus this full still answers with an empty page.
    #[test]
    fn a_query_that_matches_nothing_returns_an_empty_page() {
        let corpus = corpus();

        let page = assemble(
            &corpus.store,
            None,
            Vec::new(),
            SearchRequest {
                scope: QueryScope::All,
                query: "steven",
                limit: 25,
                cursor: None,
            },
        )
        .unwrap();

        assert!(page.entries.is_empty(), "{:?}", page.entries);
        assert!(page.next_cursor.is_none());
    }

    /// An empty page is not one fact, and neither is a full one. Recall by
    /// meaning is missing whenever the model is not on this machine, which is
    /// worth saying even when literal matches came back — those are the rows
    /// this corpus spells the way the reader did, and nothing else was
    /// reachable. A scope that never asks the model gets to blame the corpus.
    #[test]
    fn a_page_says_whether_every_source_behind_it_was_read() {
        let corpus = corpus();
        let by_scope = |scope, query| {
            assemble(
                &corpus.store,
                None,
                Vec::new(),
                SearchRequest {
                    scope,
                    query,
                    limit: 25,
                    cursor: None,
                },
            )
            .unwrap()
        };

        let matched = search(&corpus.store, QueryScope::All, vec![dictation()]);
        let missed = by_scope(QueryScope::All, "steven");
        let people = by_scope(QueryScope::People, "steven");

        assert!(!matched.entries.is_empty(), "the corpus answers {QUERY:?}");
        assert_eq!(
            matched.reason,
            Some(QueryPageReason::SemanticUnavailable),
            "a page of literal matches still missed everything phrased differently"
        );
        assert_eq!(
            missed.reason,
            Some(QueryPageReason::SemanticUnavailable),
            "an unread source outranks an empty corpus: asking again in other \
             words is worth something here, and `no_rows` would say it is not"
        );
        assert!(people.entries.is_empty());
        assert_eq!(
            people.reason,
            Some(QueryPageReason::NoRows),
            "the people index is matched literally, so no model was missing \
             from this answer and the corpus is the whole of it"
        );
    }

    /// The note that indexed the meeting is also the mutation the event stream
    /// reports, which is the receipt half of the plane.
    #[test]
    fn a_committed_mutation_is_a_receipt_that_names_its_meeting() {
        let corpus = corpus();

        let events = corpus
            .store
            .query_events(None, 25)
            .unwrap()
            .into_iter()
            .map(event_from_row)
            .collect::<Vec<_>>();

        let receipt = events
            .iter()
            .find(|entry| entry.source == QueryEventSource::OperationReceipt)
            .unwrap_or_else(|| panic!("the note's receipt: {events:?}"));
        assert_eq!(receipt.action, "note_create");
        assert_eq!(receipt.result, QueryEventResult::Committed);
        assert_eq!(receipt.link, Some(meeting_link(corpus.session_id)));
        assert!(
            receipt.detail.is_empty(),
            "a committed mutation carries no reason codes: {:?}",
            receipt.detail
        );
        assert_eq!(
            corpus.store.query_event_position(&receipt.id).unwrap(),
            Some(receipt.when_utc_ms),
            "an event the caller has seen has a position in this order"
        );
        assert_eq!(
            corpus
                .store
                .query_event_position(&Uuid::new_v4().to_string())
                .unwrap(),
            None,
            "an id the corpus no longer holds has none"
        );
    }

    /// The runs in the same stream: local work, addressed to the meeting the
    /// event that triggered it named.
    #[test]
    fn a_workflow_run_is_an_event_addressed_to_its_meeting() {
        let corpus = corpus();

        let runs = corpus
            .store
            .query_events(None, 25)
            .unwrap()
            .into_iter()
            .map(event_from_row)
            .filter(|entry| entry.source == QueryEventSource::WorkflowRun)
            .collect::<Vec<_>>();

        let continuity = runs
            .iter()
            .find(|run| run.action == "continuity")
            .unwrap_or_else(|| panic!("the continuity run: {runs:?}"));
        assert_eq!(continuity.result, QueryEventResult::Ok);
        assert!(
            runs.iter()
                .all(|run| run.link == Some(meeting_link(corpus.session_id))),
            "every run this meeting's finalization started points back at it: {runs:?}"
        );
    }

    /// The semantic cache's contract, without a model: what the index is built
    /// from, that the state row ends the backfill, and that a winning chunk
    /// carries its own words back as the snippet.
    #[test]
    fn indexing_a_session_takes_it_off_the_backfill_and_into_the_scan() {
        let corpus = corpus();
        let store = &corpus.store;
        let embedding = encode_vector(&[1.0, 0.0, 0.0]);

        assert_eq!(
            store.semantic_index_targets(MODEL_REVISION, 5).unwrap(),
            [corpus.session_id],
            "a meeting whose words were never embedded"
        );
        let inputs = store
            .semantic_index_inputs(corpus.session_id)
            .unwrap()
            .expect("a session with words in it");
        assert_eq!(
            inputs.summaries,
            [SUMMARY, HEADLINE],
            "the generated reading of the meeting, which FTS5 never sees"
        );
        assert_eq!(
            inputs.transcript,
            [SEGMENT],
            "the effective segment text the store already keeps"
        );

        store
            .replace_semantic_chunks(
                corpus.session_id,
                &inputs.key,
                MODEL_REVISION,
                NOW,
                &[(HEADLINE.to_string(), embedding.clone())],
            )
            .unwrap();

        assert!(
            store
                .semantic_index_targets(MODEL_REVISION, 5)
                .unwrap()
                .is_empty(),
            "the state row is what stops the backfill selecting it on every search"
        );
        let vectors = store
            .query_semantic_chunk_vectors(MODEL_REVISION, None)
            .unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].session_id, corpus.session_id);
        assert_eq!(vectors[0].when_utc_ms, NOW);
        assert_eq!(vectors[0].embedding, embedding);
        assert!(
            store
                .query_semantic_chunk_vectors("another-model", None)
                .unwrap()
                .is_empty(),
            "vectors from another model are not comparable to this one's query"
        );

        let candidates = store
            .query_meetings_by_chunk(&[vectors[0].chunk_id])
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].session_id, corpus.session_id);
        assert_eq!(candidates[0].title, TITLE);
        assert_eq!(
            candidates[0].snippet, HEADLINE,
            "the chunk that won, not the meeting's first words"
        );
    }
}
