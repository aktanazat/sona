//! Loops that close.
//!
//! The words of a loop live in the artifact revision that produced them, and
//! are re-read out of the transcript on every regeneration. What a person did
//! about a loop lives here, in `meeting_loop_states`, keyed by an id derived
//! from those words — which is what lets a resolution outlive the ledger that
//! phrased it.
//!
//! The table stores only departures from open. A loop nobody has touched has
//! no row, which is why nothing had to be backfilled when this landed, and why
//! revision 0 means "no row yet": a first write and a tenth are fenced the same
//! way.
//!
//! One rule keeps the two halves honest: a mutation only accepts a loop id the
//! *current* ledger still contains. An id an agent invented, or one left behind
//! by a regeneration that dropped the row, is `NotFound` rather than a state
//! row pointing at nothing.

use super::{
    committed_receipt, decode_json, from_i64, id, insert_operation_receipt, rejected_receipt,
    session_row, to_i64, utc_now_ms, MeetingStore, StoreError, StoreMutation,
};
use crate::meeting::ledger::{LedgerFirmness, LedgerOutcome, MeetingLedger};
use crate::meeting::loop_types::{
    loop_direction, MeetingLoopAssignRequest, MeetingLoopId, MeetingLoopKind,
    MeetingLoopMutationResult, MeetingLoopReopenRequest, MeetingLoopResolveRequest, MeetingLoopRow,
    MeetingLoopStatus, MeetingLoopsResult,
};
use crate::meeting::people_types::PersonId;
use crate::meeting::types::{
    ArtifactCitation, GeneratedMeetingArtifacts, MeetingCommandKind, MeetingOperationId,
    MeetingPhase, MeetingReasonCode, MeetingSessionId, OperationActor, OperationReceipt,
    SourceKind,
};
use crate::meeting::workflow_types::WorkflowId;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::collections::HashMap;
use uuid::Uuid;

pub(crate) const SCHEMA_VERSION: u32 = 1;

/// How many earlier occurrences a carried-forward chain is walked back before
/// the walk gives up. A series that has genuinely carried the same loop this
/// many times has a bigger problem than a missing date, and a corrupt chain
/// must not spin.
const MAX_CARRY_CHAIN: usize = 64;

/// A ledger row that can be acted on, before its stored state is attached.
/// Words only: the ledger is the sole owner of these.
pub(crate) struct LoopSeed {
    pub loop_id: MeetingLoopId,
    pub kind: MeetingLoopKind,
    pub text: String,
    pub owner_text: Option<String>,
    pub at_ms: u64,
    pub instead: Option<String>,
    pub firmness: Option<LedgerFirmness>,
    pub quote: Option<String>,
    pub speaker: Option<String>,
    pub citations: Vec<ArtifactCitation>,
}

/// The stored half of a loop: what somebody did about it.
#[derive(Clone)]
pub(crate) struct LoopState {
    pub status: MeetingLoopStatus,
    pub owner_person_id: Option<PersonId>,
    pub resolved_at_utc_ms: Option<i64>,
    pub resolving_operation_id: Option<String>,
    pub carried_into_loop_id: Option<MeetingLoopId>,
    pub revision: u64,
}

impl Default for LoopState {
    /// An untouched loop: open, unassigned, revision zero.
    fn default() -> Self {
        Self {
            status: MeetingLoopStatus::Open,
            owner_person_id: None,
            resolved_at_utc_ms: None,
            resolving_operation_id: None,
            carried_into_loop_id: None,
            revision: 0,
        }
    }
}

/// Which change a loop mutation is making. One enum so the fencing, the
/// receipt and the upsert are written once.
#[derive(Clone, Copy)]
enum LoopChange {
    Resolve(MeetingLoopStatus),
    Reopen,
    Assign(Option<PersonId>),
    /// Carried into a successor loop. Only the ledger pass writes this.
    Carry,
}

impl MeetingStore {
    /// Every actionable row in one meeting, words from the ledger and state
    /// from the store.
    pub(crate) fn meeting_loops(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingLoopsResult, StoreError> {
        let connection = self.connection()?;
        loops_in(&connection, session_id)
    }

    /// Every actionable row in the corpus, newest meeting first, beside what
    /// the gate below held back.
    ///
    /// The corpus-wide counterpart of [`Self::meeting_loops`], gated the way
    /// the open-loops inbox is: until a meeting's continuity pass has
    /// succeeded its rows have not been matched against the previous session,
    /// so a loop that is really carried forward would still read as open. It
    /// keeps both registers and every status, because "what is still open",
    /// "what got done" and "what did somebody commit to" are three questions
    /// about one set of rows — which is what the read-only external plane
    /// asks, and why it asks the store rather than opening ledger JSON itself.
    ///
    /// The gate reports itself because a reader cannot tell one empty list
    /// from another: a corpus with no commitments in it and a corpus whose
    /// ledgers are all waiting on their continuity pass both scan to nothing.
    pub(crate) fn corpus_loops(&self) -> Result<CorpusLoops, StoreError> {
        let connection = self.connection()?;
        let mut meetings = Vec::new();
        let mut awaiting_continuity = false;
        for meeting in ledger_rows_in(&connection)? {
            if super::workflows::workflow_succeeded_for_session_in(
                &connection,
                WorkflowId::Continuity,
                meeting.session_id,
            )? {
                meetings.push(meeting);
            } else {
                awaiting_continuity |= !meeting.rows.is_empty();
            }
        }
        Ok(CorpusLoops {
            meetings,
            awaiting_continuity,
        })
    }

    pub(crate) fn resolve_loop(
        &self,
        request: MeetingLoopResolveRequest,
        actor: OperationActor,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingLoopMutationResult, StoreError> {
        self.mutate_loop(
            request.operation_id,
            actor,
            requested_at_utc_ms,
            MeetingCommandKind::LoopResolve,
            &request.loop_id,
            request.expected_revision,
            LoopChange::Resolve(request.resolution.status()),
        )
    }

    pub(crate) fn reopen_loop(
        &self,
        request: MeetingLoopReopenRequest,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingLoopMutationResult, StoreError> {
        self.mutate_loop(
            request.operation_id,
            OperationActor::User,
            requested_at_utc_ms,
            MeetingCommandKind::LoopReopen,
            &request.loop_id,
            request.expected_revision,
            LoopChange::Reopen,
        )
    }

    pub(crate) fn assign_loop(
        &self,
        request: MeetingLoopAssignRequest,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingLoopMutationResult, StoreError> {
        self.mutate_loop(
            request.operation_id,
            OperationActor::User,
            requested_at_utc_ms,
            MeetingCommandKind::LoopAssign,
            &request.loop_id,
            request.expected_revision,
            LoopChange::Assign(request.owner_person_id),
        )
    }

    /// The ledger pass for one freshly generated meeting: every open loop in
    /// it that the previous session of the same series also left open closes
    /// that earlier occurrence as carried, and points it at its successor.
    ///
    /// Runs after the artifact revision lands, because the ledger it reads is
    /// the one that just landed. Returns one receipt per loop carried, so the
    /// pass is as verifiable as a hand-made resolution.
    pub(crate) fn carry_loops_forward(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<OperationReceipt>, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(previous) = previous_series_session_in(&transaction, session_id)? else {
            transaction.commit()?;
            return Ok(Vec::new());
        };
        let successors = seeds_in(&transaction, session_id)?
            .into_iter()
            .filter_map(|seed| {
                let key = seed.loop_id.content_key()?.to_string();
                Some((key, seed.loop_id))
            })
            .collect::<HashMap<_, _>>();
        if successors.is_empty() {
            transaction.commit()?;
            return Ok(Vec::new());
        }
        let previous_states = states_in(&transaction, previous)?;
        let mut receipts = Vec::new();
        let phase = session_row(&transaction, previous)?.phase;
        for seed in seeds_in(&transaction, previous)? {
            let Some(key) = seed.loop_id.content_key() else {
                continue;
            };
            let Some(successor) = successors.get(key) else {
                continue;
            };
            let state = previous_states
                .get(seed.loop_id.as_str())
                .cloned()
                .unwrap_or_default();
            // Only a loop still open moves: a resolved one is finished, and a
            // carried one already points at its successor.
            if !state.status.is_open() {
                continue;
            }
            let now = utc_now_ms();
            let next_revision = state.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
            let operation_id = MeetingOperationId::new();
            write_state_in(
                &transaction,
                previous,
                &seed,
                &state,
                LoopChange::Carry,
                Some(successor),
                operation_id,
                next_revision,
                now,
            )?;
            let receipt = committed_receipt(
                StoreMutation {
                    operation_id,
                    // Nobody pressed anything: this pass runs itself once a
                    // new occurrence's artifacts land.
                    actor: OperationActor::System,
                    requested_at_utc_ms: now,
                    session_id: previous,
                    expected_revision: state.revision,
                    command: MeetingCommandKind::LoopCarry,
                },
                phase,
                phase,
                now,
                next_revision,
                vec![seed.loop_id.0.clone(), successor.0.clone()],
            );
            insert_operation_receipt(&transaction, &receipt, now)?;
            receipts.push(receipt);
        }
        transaction.commit()?;
        Ok(receipts)
    }

    fn mutate_loop(
        &self,
        operation_id: MeetingOperationId,
        actor: OperationActor,
        requested_at_utc_ms: i64,
        command: MeetingCommandKind,
        loop_id: &MeetingLoopId,
        expected_revision: u64,
        change: LoopChange,
    ) -> Result<MeetingLoopMutationResult, StoreError> {
        let session_id = loop_id.session_id().ok_or(StoreError::Invalid)?;
        if let Some(receipt) = self.operation_receipt(operation_id)? {
            let loops = self.meeting_loops(session_id)?;
            return Ok(MeetingLoopMutationResult { receipt, loops });
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = session_row(&transaction, session_id)?;
        if session.phase == MeetingPhase::Deleting {
            return Err(StoreError::NotFound);
        }
        let seed = seeds_in(&transaction, session_id)?
            .into_iter()
            .find(|seed| seed.loop_id == *loop_id)
            .ok_or(StoreError::NotFound)?;
        let state = state_in(&transaction, loop_id)?.unwrap_or_default();
        let mutation = StoreMutation {
            operation_id,
            actor,
            requested_at_utc_ms,
            session_id,
            expected_revision,
            command,
        };
        if expected_revision != state.revision {
            let receipt = rejected_receipt(
                mutation,
                session.phase,
                state.revision,
                MeetingReasonCode::StaleRevision,
            );
            insert_operation_receipt(&transaction, &receipt, utc_now_ms())?;
            let loops = loops_in(&transaction, session_id)?;
            transaction.commit()?;
            return Ok(MeetingLoopMutationResult { receipt, loops });
        }
        if let LoopChange::Assign(Some(person_id)) = change {
            let known: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM persons WHERE id = ?1)",
                params![person_id.uuid().to_string()],
                |row| row.get(0),
            )?;
            if !known {
                return Err(StoreError::NotFound);
            }
        }
        let now = utc_now_ms();
        let next_revision = state.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
        write_state_in(
            &transaction,
            session_id,
            &seed,
            &state,
            change,
            None,
            operation_id,
            next_revision,
            now,
        )?;
        let receipt = committed_receipt(
            mutation,
            session.phase,
            session.phase,
            now,
            next_revision,
            vec![loop_id.0.clone()],
        );
        insert_operation_receipt(&transaction, &receipt, now)?;
        let loops = loops_in(&transaction, session_id)?;
        transaction.commit()?;
        Ok(MeetingLoopMutationResult { receipt, loops })
    }
}

/// One meeting's actionable rows, with the meeting facts every corpus-wide
/// reader needs beside them.
pub(crate) struct MeetingLedgerRows {
    pub session_id: MeetingSessionId,
    pub title: String,
    /// When the meeting happened: its start, or its creation for one that
    /// never started.
    pub at_utc_ms: i64,
    pub rows: Vec<MeetingLoopRow>,
}

/// One corpus-wide ledger read: the rows a reader may have, and the one fact
/// about the rows it may not that an empty list cannot carry.
pub(crate) struct CorpusLoops {
    pub meetings: Vec<MeetingLedgerRows>,
    /// Whether some meeting holds rows the continuity gate has not released.
    pub awaiting_continuity: bool,
}

/// Every retained meeting that has a current ledger, newest first, with its
/// rows already joined to their stored state.
///
/// Loops live in artifact JSON rather than in a table, so every question about
/// the whole corpus — the open-loops inbox, the digest's overdue count — is
/// this walk. It is written once so those answers cannot disagree about what a
/// meeting's rows are.
pub(crate) fn ledger_rows_in(
    connection: &Connection,
) -> Result<Vec<MeetingLedgerRows>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT m.id, m.title, COALESCE(m.started_at_utc_ms, m.created_at_utc_ms),
                (SELECT a.content_json
                   FROM meeting_artifact_revisions a
                  WHERE a.session_id = m.id AND a.state = 'current'
                    AND a.content_json IS NOT NULL
                  ORDER BY a.generated_at_utc_ms DESC LIMIT 1)
           FROM meeting_sessions m
          WHERE m.phase != 'deleting'
          ORDER BY COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) DESC, m.id DESC",
    )?;
    let meetings = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let mut walked = Vec::with_capacity(meetings.len());
    for (session_id, title, at_utc_ms, content_json) in meetings {
        let session_id = MeetingSessionId::from_uuid(
            Uuid::parse_str(&session_id).map_err(|_| StoreError::Corrupt)?,
        );
        let Some(content_json) = content_json else {
            continue;
        };
        let content: GeneratedMeetingArtifacts = decode_json(&content_json)?;
        let Some(ledger) = content.ledger else {
            continue;
        };
        walked.push(MeetingLedgerRows {
            session_id,
            title,
            at_utc_ms,
            rows: rows_from_seeds_in(
                connection,
                session_id,
                ledger_loop_seeds(session_id, &ledger),
            )?,
        });
    }
    Ok(walked)
}

/// Every ledger row in `ledger` that can be acted on, in the order the review
/// screen reads them: threads first because they carry the owner and the
/// receipt, then the questions nobody answered, then the commitments.
///
/// Rows collapse by id, so a thread that also appears in the open-loops table
/// is one loop with both halves of its evidence rather than two.
pub(crate) fn ledger_loop_seeds(
    session_id: MeetingSessionId,
    ledger: &MeetingLedger,
) -> Vec<LoopSeed> {
    let mut seeds: Vec<LoopSeed> = Vec::new();
    let mut index = HashMap::<String, usize>::new();
    for thread in &ledger.threads {
        if thread.state.outcome() == LedgerOutcome::Landed {
            continue;
        }
        push_seed(
            &mut seeds,
            &mut index,
            LoopSeed {
                loop_id: MeetingLoopId::derive(session_id, MeetingLoopKind::Loop, &thread.topic),
                kind: MeetingLoopKind::Loop,
                text: thread.topic.clone(),
                owner_text: thread.owner.clone(),
                at_ms: thread.receipt.t_ms,
                instead: None,
                firmness: None,
                quote: Some(thread.receipt.quote.clone()),
                speaker: thread.receipt.speaker.clone(),
                citations: thread.receipt.citations.clone(),
            },
        );
    }
    for open_loop in &ledger.open_loops {
        push_seed(
            &mut seeds,
            &mut index,
            LoopSeed {
                loop_id: MeetingLoopId::derive(
                    session_id,
                    MeetingLoopKind::Loop,
                    &open_loop.question,
                ),
                kind: MeetingLoopKind::Loop,
                text: open_loop.question.clone(),
                owner_text: None,
                at_ms: open_loop.at_ms,
                instead: Some(open_loop.instead.clone()),
                firmness: None,
                quote: None,
                speaker: None,
                citations: open_loop.citations.clone(),
            },
        );
    }
    for commitment in &ledger.commitments {
        push_seed(
            &mut seeds,
            &mut index,
            LoopSeed {
                loop_id: MeetingLoopId::derive(
                    session_id,
                    MeetingLoopKind::Commitment,
                    &commitment.what,
                ),
                kind: MeetingLoopKind::Commitment,
                text: commitment.what.clone(),
                owner_text: Some(commitment.who.clone()),
                at_ms: commitment.receipt.t_ms,
                instead: None,
                firmness: Some(commitment.firmness),
                quote: Some(commitment.receipt.quote.clone()),
                speaker: commitment.receipt.speaker.clone(),
                citations: commitment.receipt.citations.clone(),
            },
        );
    }
    seeds
}

/// Add a seed, or fold it into the one already holding that id: the same words
/// in two registers are one loop, and each register knows something the other
/// does not.
fn push_seed(seeds: &mut Vec<LoopSeed>, index: &mut HashMap<String, usize>, seed: LoopSeed) {
    if let Some(position) = index.get(seed.loop_id.as_str()).copied() {
        let existing = &mut seeds[position];
        if existing.owner_text.is_none() {
            existing.owner_text = seed.owner_text;
        }
        if existing.instead.is_none() {
            existing.instead = seed.instead;
        }
        if existing.quote.is_none() {
            existing.quote = seed.quote;
            existing.speaker = seed.speaker;
        }
        for citation in seed.citations {
            if !existing
                .citations
                .iter()
                .any(|held| held.segment_id == citation.segment_id)
            {
                existing.citations.push(citation);
            }
        }
        return;
    }
    index.insert(seed.loop_id.0.clone(), seeds.len());
    seeds.push(seed);
}

/// Attach stored state to ledger words. The one place a `MeetingLoopRow` is
/// built, so every surface reads the same join.
pub(crate) fn rows_from_seeds_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    seeds: Vec<LoopSeed>,
) -> Result<Vec<MeetingLoopRow>, StoreError> {
    let states = states_in(connection, session_id)?;
    let mine = microphone_speaker_labels_in(connection, session_id)?;
    let mut owner_names = HashMap::<PersonId, String>::new();
    let mut rows = Vec::with_capacity(seeds.len());
    for seed in seeds {
        let state = states
            .get(seed.loop_id.as_str())
            .cloned()
            .unwrap_or_default();
        let owner_display_name = match state.owner_person_id {
            Some(person_id) => match owner_names.get(&person_id) {
                Some(name) => Some(name.clone()),
                None => {
                    let name: Option<String> = connection
                        .query_row(
                            "SELECT display_name FROM persons WHERE id = ?1",
                            params![person_id.uuid().to_string()],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if let Some(name) = name.as_ref() {
                        owner_names.insert(person_id, name.clone());
                    }
                    name
                }
            },
            None => None,
        };
        let carried_since_at_utc_ms = carried_since_in(connection, &seed.loop_id)?;
        let direction = loop_direction(
            state.owner_person_id,
            seed.owner_text.as_deref(),
            seed.speaker.as_deref(),
            &mine,
        );
        let resolved_by = resolved_by_in(connection, state.resolving_operation_id.as_deref())?;
        rows.push(MeetingLoopRow {
            loop_id: seed.loop_id,
            session_id,
            kind: seed.kind,
            text: seed.text,
            owner_text: seed.owner_text,
            owner_person_id: state.owner_person_id,
            owner_display_name,
            direction,
            status: state.status,
            resolved_at_utc_ms: state.resolved_at_utc_ms,
            resolving_operation_id: state.resolving_operation_id,
            resolved_by,
            carried_into_loop_id: state.carried_into_loop_id,
            carried_since_at_utc_ms,
            at_ms: seed.at_ms,
            revision: state.revision,
            instead: seed.instead,
            firmness: seed.firmness,
            quote: seed.quote,
            speaker: seed.speaker,
            citations: seed.citations,
        });
    }
    Ok(rows)
}

/// The display names of this session's microphone speakers, merged-away ones
/// excluded.
///
/// This is the whole answer to "which voice is the user's". Sona has no
/// voiceprints and no self `Person`, so the microphone track is the evidence:
/// what was captured on this machine's own input was said by whoever is
/// sitting at it. Renaming that speaker to a real name keeps working, because
/// the row is matched by the name it currently carries.
pub(in crate::meeting::store) fn microphone_speaker_labels_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<String>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT display_name FROM meeting_speakers
          WHERE session_id = ?1 AND source_kind = ?2 AND merged_into_speaker_id IS NULL",
    )?;
    let labels = statement
        .query_map(
            params![id(session_id), SourceKind::Microphone.as_str()],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(labels)
}

pub(crate) fn loops_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<MeetingLoopsResult, StoreError> {
    let revision: i64 = connection
        .query_row(
            "SELECT revision FROM meeting_sessions WHERE id = ?1",
            params![id(session_id)],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound)?;
    let seeds = seeds_in(connection, session_id)?;
    Ok(MeetingLoopsResult {
        schema_version: SCHEMA_VERSION,
        revision: from_i64(revision)?,
        rows: rows_from_seeds_in(connection, session_id, seeds)?,
    })
}

/// The actionable rows of the newest artifact revision that carries content.
/// A meeting with no generated ledger has no loops, which is not an error.
pub(crate) fn seeds_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<LoopSeed>, StoreError> {
    let Some(ledger) = current_ledger_in(connection, session_id)? else {
        return Ok(Vec::new());
    };
    Ok(ledger_loop_seeds(session_id, &ledger))
}

pub(crate) fn current_ledger_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Option<MeetingLedger>, StoreError> {
    let content_json: Option<String> = connection
        .query_row(
            "SELECT content_json FROM meeting_artifact_revisions
              WHERE session_id = ?1 AND state = 'current' AND content_json IS NOT NULL
              ORDER BY generated_at_utc_ms DESC LIMIT 1",
            params![id(session_id)],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let Some(content_json) = content_json else {
        return Ok(None);
    };
    let content: GeneratedMeetingArtifacts = decode_json(&content_json)?;
    Ok(content.ledger)
}

pub(crate) fn states_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<HashMap<String, LoopState>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT loop_id, status, owner_person_id, resolved_at_utc_ms,
                resolving_operation_id, carried_into_loop_id, revision
           FROM meeting_loop_states WHERE session_id = ?1",
    )?;
    let rows = statement
        .query_map(params![id(session_id)], state_columns)?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    rows.into_iter()
        .map(|(loop_id, columns)| Ok((loop_id, state_from_columns(columns)?)))
        .collect()
}

fn state_in(
    transaction: &Transaction<'_>,
    loop_id: &MeetingLoopId,
) -> Result<Option<LoopState>, StoreError> {
    let columns = transaction
        .query_row(
            "SELECT loop_id, status, owner_person_id, resolved_at_utc_ms,
                    resolving_operation_id, carried_into_loop_id, revision
               FROM meeting_loop_states WHERE loop_id = ?1",
            params![loop_id.as_str()],
            state_columns,
        )
        .optional()?;
    columns
        .map(|(_, columns)| state_from_columns(columns))
        .transpose()
}

type StateColumns = (
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    i64,
);

fn state_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, StateColumns)> {
    Ok((
        row.get(0)?,
        (
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        ),
    ))
}

fn state_from_columns(columns: StateColumns) -> Result<LoopState, StoreError> {
    let (
        status,
        owner_person_id,
        resolved_at_utc_ms,
        resolving_operation_id,
        carried_into,
        revision,
    ) = columns;
    Ok(LoopState {
        status: MeetingLoopStatus::from_str(&status).ok_or(StoreError::Corrupt)?,
        owner_person_id: owner_person_id
            .as_deref()
            .map(|value| {
                Uuid::parse_str(value)
                    .map(PersonId)
                    .map_err(|_| StoreError::Corrupt)
            })
            .transpose()?,
        resolved_at_utc_ms,
        resolving_operation_id,
        carried_into_loop_id: carried_into.map(MeetingLoopId),
        revision: from_i64(revision)?,
    })
}

/// When this loop was first raised, walking the carry chain back to its root.
/// Derived from the stored links rather than from matching words a second time,
/// so "carried since" and "carried into" can never disagree.
pub(crate) fn carried_since_in(
    connection: &Connection,
    loop_id: &MeetingLoopId,
) -> Result<Option<i64>, StoreError> {
    let mut current = loop_id.clone();
    let mut earliest = None;
    for _ in 0..MAX_CARRY_CHAIN {
        let predecessor: Option<(String, i64)> = connection
            .query_row(
                "SELECT l.loop_id, COALESCE(m.started_at_utc_ms, m.created_at_utc_ms)
                   FROM meeting_loop_states l
                   JOIN meeting_sessions m ON m.id = l.session_id
                  WHERE l.carried_into_loop_id = ?1",
                params![current.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((predecessor_id, at_utc_ms)) = predecessor else {
            return Ok(earliest);
        };
        earliest = Some(at_utc_ms);
        current = MeetingLoopId(predecessor_id);
    }
    Ok(earliest)
}

/// Who the operation that closed this row was run by, off the receipt it
/// already points at.
///
/// A lookup rather than a column on `meeting_loop_states`, because the answer
/// is written down once when the mutation commits and a second copy would be
/// a second thing to keep true. A resolution whose receipt row is gone is a
/// row that does not say who closed it, not a ledger read that fails: the
/// words and the state are both still honest without it.
fn resolved_by_in(
    connection: &Connection,
    resolving_operation_id: Option<&str>,
) -> Result<Option<OperationActor>, StoreError> {
    let Some(operation_id) = resolving_operation_id else {
        return Ok(None);
    };
    let receipt: Option<String> = connection
        .query_row(
            "SELECT receipt_json FROM meeting_operation_receipts WHERE operation_id = ?1",
            params![operation_id],
            |row| row.get(0),
        )
        .optional()?;
    receipt
        .map(|value| decode_json::<OperationReceipt>(&value).map(|receipt| receipt.actor))
        .transpose()
}

/// The session immediately before this one in the same calendar series, or
/// `None` when this meeting is not part of one.
fn previous_series_session_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Option<MeetingSessionId>, StoreError> {
    let previous: Option<String> = connection
        .query_row(
            "SELECT f.session_id
               FROM meeting_calendar_facts f
               JOIN meeting_sessions m ON m.id = f.session_id
              WHERE json_extract(f.event_json, '$.seriesKey') = (
                        SELECT json_extract(event_json, '$.seriesKey')
                          FROM meeting_calendar_facts WHERE session_id = ?1
                    )
                AND f.session_id != ?1
                AND COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) < (
                        SELECT COALESCE(started_at_utc_ms, created_at_utc_ms)
                          FROM meeting_sessions WHERE id = ?1
                    )
                AND m.phase != 'deleting'
              ORDER BY COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) DESC, m.id DESC
              LIMIT 1",
            params![id(session_id)],
            |row| row.get(0),
        )
        .optional()?;
    previous
        .map(|value| {
            Uuid::parse_str(&value)
                .map(MeetingSessionId::from_uuid)
                .map_err(|_| StoreError::Corrupt)
        })
        .transpose()
}

#[allow(clippy::too_many_arguments)]
fn write_state_in(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
    seed: &LoopSeed,
    state: &LoopState,
    change: LoopChange,
    successor: Option<&MeetingLoopId>,
    operation_id: MeetingOperationId,
    next_revision: u64,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    // The resolving operation is the one that set resolved_at: a resolve or a
    // carry writes its own id, a reopen clears both, and an assignment leaves
    // both alone. Readers that replay a resolution look the receipt up by
    // this id, so it cannot point at an assignment.
    let (status, owner_person_id, resolved_at_utc_ms, resolving_operation_id, carried_into) =
        match change {
            LoopChange::Resolve(status) => (
                status,
                state.owner_person_id,
                Some(now_utc_ms),
                Some(id(operation_id)),
                None,
            ),
            // Reopening undoes a resolution and a carry alike: the loop is live
            // again, so it no longer has a successor.
            LoopChange::Reopen => (
                MeetingLoopStatus::Open,
                state.owner_person_id,
                None,
                None,
                None,
            ),
            LoopChange::Assign(owner) => (
                state.status,
                owner,
                state.resolved_at_utc_ms,
                state.resolving_operation_id.clone(),
                state.carried_into_loop_id.clone(),
            ),
            LoopChange::Carry => (
                MeetingLoopStatus::Carried,
                state.owner_person_id,
                Some(now_utc_ms),
                Some(id(operation_id)),
                successor.cloned(),
            ),
        };
    transaction.execute(
        "INSERT INTO meeting_loop_states (
            loop_id, session_id, kind, status, owner_person_id, resolved_at_utc_ms,
            resolving_operation_id, carried_into_loop_id, revision, updated_at_utc_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(loop_id) DO UPDATE SET
            status = excluded.status,
            owner_person_id = excluded.owner_person_id,
            resolved_at_utc_ms = excluded.resolved_at_utc_ms,
            resolving_operation_id = excluded.resolving_operation_id,
            carried_into_loop_id = excluded.carried_into_loop_id,
            revision = excluded.revision,
            updated_at_utc_ms = excluded.updated_at_utc_ms",
        params![
            seed.loop_id.as_str(),
            id(session_id),
            seed.kind.as_str(),
            status.as_str(),
            owner_person_id.map(|person_id| person_id.uuid().to_string()),
            resolved_at_utc_ms,
            resolving_operation_id,
            carried_into.as_ref().map(MeetingLoopId::as_str),
            to_i64(next_revision)?,
            now_utc_ms,
        ],
    )?;
    Ok(())
}
