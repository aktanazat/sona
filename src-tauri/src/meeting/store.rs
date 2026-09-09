#[cfg(test)]
mod artifact_trace_tests;
pub(crate) mod automations;
#[cfg(test)]
mod automations_run_tests;
#[cfg(test)]
mod automations_tests;
pub(crate) mod digest;
mod documents;
#[cfg(test)]
mod external_tests;
mod fence;
mod follow_up;
pub(crate) mod learning;
#[cfg(test)]
mod learning_tests;
pub(crate) mod loops;
#[cfg(test)]
mod loops_tests;
mod people;
pub(crate) mod prompts;
pub(crate) mod query_plane;
#[cfg(test)]
mod remote_intelligence_tests;
pub(crate) mod series;
#[cfg(test)]
mod series_tests;
#[cfg(test)]
mod title_tests;
pub(crate) mod voice_identity;
#[cfg(test)]
mod voice_identity_tests;
/// The encrypted-store fixture, and the one place a test builds one. Reachable
/// from the whole crate rather than just this module because the query plane
/// spans this store and dictation history and so lives outside `meeting/`
/// (see `MeetingSessionManager::store`), and a second fixture that opened its
/// own database would be a second answer to what a fresh corpus looks like.
#[cfg(test)]
pub(crate) mod workflow_core_tests;
#[cfg(test)]
mod workflow_receipt_tests;
mod workflows;

use super::analytics::{
    AnalyticsSegment, MeetingActionItemState, MeetingAnalytics, MeetingNotesTemplate,
    MeetingUserNotes,
};
use super::capture::SessionClock;
use super::cloud_bundle;
use super::types::*;
use crate::analytics::{local_days_start_utc_ms, DashboardTrendRequest, LocalCalendarRange};
use crate::secrets::MeetingStorageKey;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use chrono::{DateTime, Local};
use hkdf::Hkdf;
use rusqlite::{
    params, params_from_iter, Connection, OptionalExtension, Transaction, TransactionBehavior,
};
use rusqlite_migration::{Migrations, M};
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const STORE_SCHEMA_VERSION: u32 = 2;
const RECORD_FORMAT_VERSION: u8 = 1;
const RECORD_MAGIC: [u8; 4] = *b"SMR1";
const RECORD_HEADER_BYTES: usize = 92;
const INDEX_PLAINTEXT_BYTES: usize = 48;
const INDEX_RECORD_BYTES: usize = 76;
const MISSING_OFFSET: u64 = u64::MAX;

/// How long a deleted meeting stays undoable.
///
/// Thirty days, the same horizon as the default retention policy and the same
/// one every desktop trash uses: long enough that "I deleted the wrong meeting"
/// is recoverable after a holiday, short enough that deleting still means
/// something. It is a constant rather than a setting because a bin whose depth
/// is configurable is a second retention policy to explain.
const TRASH_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

/// The three workflow tables, rebuilt around a widened allowed-value list.
///
/// SQLite cannot alter a `CHECK` constraint, and `workflow_runs` carries
/// foreign keys into both of the other two tables, so widening either
/// `workflow_settings.workflow_id` or `workflow_events.kind` means rebuilding
/// all three. Only the lists differ between migrations that do it, which is why
/// they are the only parameters: the column sets, the index definitions, the
/// rename order that lets `ALTER TABLE ... RENAME` rewrite the foreign keys,
/// the `INSERT ... SELECT` copies and the child-first drop order live here once
/// instead of once per migration.
///
/// The shadow tables are named the same way every time. A migration creates
/// them and drops them again before it returns, so no two rebuilds ever hold
/// the names at once.
///
/// The index definitions here are the live ones, not the ones that were live
/// when the first rebuild ran. `workflow_runs_once_idx` narrowed to `status =
/// 'ok'` in a later migration, and a rebuild that re-emitted the old predicate
/// would silently widen it back — so a migration that changes an index changes
/// it here as well. Earlier rebuilds now create the narrow index and the
/// narrowing migration recreates the same one, which leaves both a fresh
/// database and an upgraded one in the same state.
macro_rules! rebuilt_workflow_tables {
    (
        workflow_ids: $workflow_ids:literal,
        seeded: $seeded:literal,
        event_kinds: $event_kinds:literal $(,)?
    ) => {
        concat!(
            "
        DROP INDEX workflow_runs_once_idx;
        DROP INDEX workflow_runs_list_idx;
        ALTER TABLE workflow_runs RENAME TO workflow_runs_rebuilding;
        ALTER TABLE workflow_events RENAME TO workflow_events_rebuilding;
        ALTER TABLE workflow_settings RENAME TO workflow_settings_rebuilding;

        CREATE TABLE workflow_settings (
            workflow_id TEXT PRIMARY KEY NOT NULL CHECK (workflow_id IN (",
            $workflow_ids,
            ")),
            enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
        );
        INSERT INTO workflow_settings(workflow_id, enabled)
        SELECT workflow_id, enabled FROM workflow_settings_rebuilding;
        INSERT INTO workflow_settings(workflow_id, enabled) VALUES ",
            $seeded,
            ";

        CREATE TABLE workflow_events (
            id TEXT PRIMARY KEY NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN (",
            $event_kinds,
            ")),
            payload_json TEXT NOT NULL,
            occurred_at_utc_ms INTEGER NOT NULL,
            source TEXT NOT NULL,
            dedupe_key TEXT NOT NULL UNIQUE
        );
        INSERT INTO workflow_events
        SELECT * FROM workflow_events_rebuilding;

        CREATE TABLE workflow_runs (
            id TEXT PRIMARY KEY NOT NULL,
            workflow_id TEXT NOT NULL REFERENCES workflow_settings(workflow_id),
            event_id TEXT NOT NULL REFERENCES workflow_events(id) ON DELETE CASCADE,
            status TEXT NOT NULL CHECK (status IN ('ok', 'failed', 'skipped')),
            started_at_utc_ms INTEGER NOT NULL,
            finished_at_utc_ms INTEGER NOT NULL,
            outcome_summary TEXT NOT NULL,
            error TEXT
        );
        INSERT INTO workflow_runs
        SELECT * FROM workflow_runs_rebuilding;
        CREATE UNIQUE INDEX workflow_runs_once_idx
            ON workflow_runs(workflow_id, event_id)
            WHERE status = 'ok';
        CREATE INDEX workflow_runs_list_idx
            ON workflow_runs(started_at_utc_ms DESC, id DESC);

        DROP TABLE workflow_runs_rebuilding;
        DROP TABLE workflow_events_rebuilding;
        DROP TABLE workflow_settings_rebuilding;
        "
        )
    };
}

static MIGRATIONS: &[M] = &[
    M::up(
        "
        CREATE TABLE meeting_sessions (
            id TEXT PRIMARY KEY NOT NULL,
            phase TEXT NOT NULL CHECK (phase IN (
                'preflight', 'starting', 'capturing_recording', 'capturing_pausing',
                'capturing_paused', 'capturing_resuming', 'stopping', 'processing',
                'review_ready', 'recovery_required', 'deleting'
            )),
            revision INTEGER NOT NULL CHECK (revision >= 0),
            title TEXT NOT NULL,
            origin_kind TEXT NOT NULL,
            preflight_json TEXT NOT NULL,
            created_at_utc_ms INTEGER NOT NULL,
            started_at_utc_ms INTEGER,
            ended_at_utc_ms INTEGER,
            recovered_at_utc_ms INTEGER,
            successful_plan_id TEXT,
            processing_status TEXT NOT NULL,
            retention_policy_json TEXT NOT NULL,
            delete_after_utc_ms INTEGER,
            current_transcript_revision_id TEXT
        );
        CREATE TABLE meeting_session_events (
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            sequence INTEGER NOT NULL,
            prior_phase TEXT,
            next_phase TEXT NOT NULL,
            event_kind TEXT NOT NULL,
            observed_at_utc_ms INTEGER NOT NULL,
            session_offset_ns INTEGER,
            details_json TEXT NOT NULL,
            PRIMARY KEY (session_id, sequence)
        );
        CREATE TABLE meeting_run_plans (
            plan_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            attempt_number INTEGER NOT NULL CHECK (attempt_number > 0),
            schema_version INTEGER NOT NULL,
            consent_id TEXT NOT NULL,
            canonical_plan_json TEXT NOT NULL,
            created_at_utc_ms INTEGER NOT NULL,
            UNIQUE (session_id, attempt_number)
        );
        CREATE TRIGGER meeting_run_plans_immutable
        BEFORE UPDATE ON meeting_run_plans
        BEGIN SELECT RAISE(ABORT, 'meeting run plans are immutable'); END;
        CREATE TABLE meeting_consents (
            consent_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            attempt_number INTEGER NOT NULL CHECK (attempt_number > 0),
            preflight_revision INTEGER NOT NULL,
            policy_version INTEGER NOT NULL,
            acknowledgement_json TEXT NOT NULL,
            acknowledged_at_utc_ms INTEGER NOT NULL,
            UNIQUE (session_id, attempt_number)
        );
        CREATE TRIGGER meeting_consents_immutable
        BEFORE UPDATE ON meeting_consents
        BEGIN SELECT RAISE(ABORT, 'meeting consents are immutable'); END;
        CREATE TABLE meeting_capture_windows (
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            sequence INTEGER NOT NULL,
            start_offset_ns INTEGER NOT NULL CHECK (start_offset_ns >= 0),
            end_offset_ns INTEGER,
            close_reason TEXT,
            PRIMARY KEY (session_id, sequence)
        );
        CREATE TABLE meeting_source_tracks (
            track_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            plan_id TEXT NOT NULL REFERENCES meeting_run_plans(plan_id) ON DELETE CASCADE,
            source_kind TEXT NOT NULL CHECK (source_kind IN ('microphone', 'system_audio')),
            required INTEGER NOT NULL,
            requested INTEGER NOT NULL,
            descriptor_json TEXT NOT NULL,
            timestamp_bridge_json TEXT NOT NULL,
            format_json TEXT,
            first_offset_ns INTEGER,
            last_offset_ns INTEGER,
            health TEXT NOT NULL,
            UNIQUE (session_id, source_kind)
        );
        CREATE TABLE meeting_source_clock_epochs (
            track_id TEXT NOT NULL REFERENCES meeting_source_tracks(track_id) ON DELETE CASCADE,
            source_epoch INTEGER NOT NULL,
            format_epoch INTEGER NOT NULL,
            bridge_json TEXT NOT NULL,
            observed_host_monotonic_ns INTEGER NOT NULL,
            PRIMARY KEY (track_id, source_epoch, format_epoch)
        );
        CREATE TABLE meeting_track_records (
            track_id TEXT NOT NULL REFERENCES meeting_source_tracks(track_id) ON DELETE CASCADE,
            source_sequence INTEGER NOT NULL,
            source_epoch INTEGER NOT NULL,
            start_offset_ns INTEGER,
            duration_ns INTEGER NOT NULL CHECK (duration_ns >= 0),
            frame_count INTEGER NOT NULL CHECK (frame_count >= 0),
            record_offset_bytes INTEGER NOT NULL CHECK (record_offset_bytes >= 0),
            record_bytes INTEGER NOT NULL CHECK (record_bytes > 0),
            durable_at_utc_ms INTEGER NOT NULL,
            PRIMARY KEY (track_id, source_sequence)
        );
        CREATE TABLE meeting_track_checkpoints (
            track_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_source_tracks(track_id) ON DELETE CASCADE,
            next_sequence INTEGER NOT NULL,
            durable_offset_ns INTEGER,
            durable_bytes INTEGER NOT NULL,
            updated_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_source_gaps (
            gap_id INTEGER PRIMARY KEY AUTOINCREMENT,
            track_id TEXT NOT NULL REFERENCES meeting_source_tracks(track_id) ON DELETE CASCADE,
            source_epoch INTEGER NOT NULL,
            start_offset_ns INTEGER,
            end_offset_ns INTEGER,
            reason TEXT NOT NULL,
            dropped_frames INTEGER,
            observed_at_utc_ms INTEGER NOT NULL,
            details_json TEXT NOT NULL
        );
        CREATE TABLE meeting_operation_receipts (
            operation_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT,
            receipt_json TEXT NOT NULL,
            created_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_retention_policy (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            policy_json TEXT NOT NULL,
            revision INTEGER NOT NULL
        );
        INSERT INTO meeting_retention_policy(singleton, policy_json, revision)
        VALUES (1, '{\"kind\":\"delete_after_days\",\"days\":30}', 0);
        CREATE TABLE meeting_deletion_jobs (
            job_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL,
            cause TEXT NOT NULL,
            live_relative_path TEXT NOT NULL,
            trash_relative_path TEXT NOT NULL,
            state TEXT NOT NULL,
            created_at_utc_ms INTEGER NOT NULL,
            updated_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_deletion_receipts (
            job_id TEXT PRIMARY KEY NOT NULL,
            cause TEXT NOT NULL,
            completed_at_utc_ms INTEGER NOT NULL
        );
        CREATE INDEX meeting_sessions_created_idx ON meeting_sessions(created_at_utc_ms DESC);
        CREATE INDEX meeting_session_events_session_idx ON meeting_session_events(session_id, sequence);
        CREATE INDEX meeting_tracks_session_idx ON meeting_source_tracks(session_id, source_kind);
        CREATE INDEX meeting_records_track_idx ON meeting_track_records(track_id, source_sequence);
        CREATE INDEX meeting_gaps_track_idx ON meeting_source_gaps(track_id, gap_id);
        CREATE INDEX meeting_deletion_jobs_state_idx ON meeting_deletion_jobs(state);
        ",
    ),
    M::up(
        "
        CREATE TABLE meeting_speakers (
            speaker_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            source_kind TEXT NOT NULL,
            display_name TEXT NOT NULL,
            revision INTEGER NOT NULL,
            merged_into_speaker_id TEXT
        );
        CREATE TABLE meeting_transcript_revisions (
            transcript_revision_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            engine_id TEXT NOT NULL,
            model_version TEXT,
            destination_json TEXT NOT NULL,
            source_set_json TEXT NOT NULL,
            language TEXT NOT NULL,
            state TEXT NOT NULL,
            created_at_utc_ms INTEGER NOT NULL,
            completed_at_utc_ms INTEGER,
            error_code TEXT
        );
        CREATE TABLE meeting_transcript_segments (
            segment_id TEXT PRIMARY KEY NOT NULL,
            transcript_revision_id TEXT NOT NULL REFERENCES meeting_transcript_revisions(transcript_revision_id) ON DELETE CASCADE,
            track_id TEXT NOT NULL REFERENCES meeting_source_tracks(track_id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            start_offset_ns INTEGER NOT NULL,
            end_offset_ns INTEGER NOT NULL,
            speaker_id TEXT NOT NULL REFERENCES meeting_speakers(speaker_id) ON DELETE RESTRICT,
            base_text TEXT NOT NULL,
            confidence_milli INTEGER,
            UNIQUE (transcript_revision_id, ordinal)
        );
        CREATE TRIGGER meeting_transcript_segments_immutable
        BEFORE UPDATE ON meeting_transcript_segments
        BEGIN SELECT RAISE(ABORT, 'meeting transcript segments are immutable'); END;
        CREATE TABLE meeting_segment_edits (
            segment_id TEXT NOT NULL REFERENCES meeting_transcript_segments(segment_id) ON DELETE CASCADE,
            edit_sequence INTEGER NOT NULL,
            replacement_text TEXT NOT NULL,
            removed INTEGER NOT NULL,
            operator_at_utc_ms INTEGER NOT NULL,
            PRIMARY KEY (segment_id, edit_sequence)
        );
        CREATE TABLE meeting_notes (
            note_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            start_offset_ns INTEGER,
            end_offset_ns INTEGER,
            body TEXT NOT NULL,
            note_revision INTEGER NOT NULL,
            created_at_utc_ms INTEGER NOT NULL,
            updated_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_artifacts (
            artifact_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            kind TEXT NOT NULL,
            transcript_revision_id TEXT,
            input_revision INTEGER NOT NULL,
            state TEXT NOT NULL,
            created_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_search_documents (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            entity_kind TEXT NOT NULL,
            entity_id TEXT NOT NULL,
            content TEXT NOT NULL,
            UNIQUE (session_id, entity_kind, entity_id)
        );
        CREATE VIRTUAL TABLE meeting_search_fts USING fts5(
            content,
            content='meeting_search_documents',
            content_rowid='id'
        );
        CREATE TRIGGER meeting_search_documents_insert AFTER INSERT ON meeting_search_documents BEGIN
            INSERT INTO meeting_search_fts(rowid, content) VALUES (new.id, new.content);
        END;
        CREATE TRIGGER meeting_search_documents_delete AFTER DELETE ON meeting_search_documents BEGIN
            INSERT INTO meeting_search_fts(meeting_search_fts, rowid, content)
            VALUES ('delete', old.id, old.content);
        END;
        CREATE TRIGGER meeting_search_documents_update AFTER UPDATE OF content ON meeting_search_documents BEGIN
            INSERT INTO meeting_search_fts(meeting_search_fts, rowid, content)
            VALUES ('delete', old.id, old.content);
            INSERT INTO meeting_search_fts(rowid, content) VALUES (new.id, new.content);
        END;
        CREATE TABLE meeting_questions (
            question_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            question_text TEXT NOT NULL,
            answer_state TEXT NOT NULL,
            answer_text TEXT,
            revision INTEGER NOT NULL,
            created_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_question_citations (
            question_id TEXT NOT NULL REFERENCES meeting_questions(question_id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            citation_json TEXT NOT NULL,
            PRIMARY KEY (question_id, ordinal)
        );
        CREATE TABLE meeting_remote_jobs (
            job_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT,
            destination_id TEXT NOT NULL,
            non_secret_reference TEXT,
            state TEXT NOT NULL,
            cancellation_requested INTEGER NOT NULL,
            created_at_utc_ms INTEGER NOT NULL,
            updated_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_export_receipts (
            export_receipt_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            format TEXT NOT NULL,
            snapshot_revision INTEGER NOT NULL,
            capture_completeness TEXT NOT NULL,
            transcript_revision_id TEXT,
            created_at_utc_ms INTEGER NOT NULL
        );
        CREATE INDEX meeting_segments_revision_idx ON meeting_transcript_segments(transcript_revision_id, ordinal);
        CREATE INDEX meeting_notes_session_idx ON meeting_notes(session_id, updated_at_utc_ms);
        CREATE INDEX meeting_questions_session_idx ON meeting_questions(session_id, created_at_utc_ms);
        ",
    ),
    M::up(
        "
        ALTER TABLE meeting_sessions ADD COLUMN current_diarization_generation_id TEXT;
        ALTER TABLE meeting_sessions ADD COLUMN diarization_status TEXT NOT NULL DEFAULT 'not_requested';
        ALTER TABLE meeting_sessions ADD COLUMN diarization_model_id TEXT;
        ALTER TABLE meeting_sessions ADD COLUMN diarization_model_version TEXT;
        ALTER TABLE meeting_questions ADD COLUMN scope_json TEXT NOT NULL DEFAULT '{\"kind\":\"this_meeting\"}';
        ALTER TABLE meeting_questions ADD COLUMN input_revision INTEGER NOT NULL DEFAULT 0;
        CREATE TABLE meeting_artifact_revisions (
            artifact_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            transcript_revision_id TEXT NOT NULL REFERENCES meeting_transcript_revisions(transcript_revision_id) ON DELETE CASCADE,
            input_revision INTEGER NOT NULL,
            template_id TEXT NOT NULL,
            template_version INTEGER NOT NULL,
            generation_key TEXT NOT NULL,
            state TEXT NOT NULL,
            content_json TEXT,
            generated_at_utc_ms INTEGER NOT NULL,
            UNIQUE (session_id, generation_key)
        );
        CREATE TABLE meeting_diarization_generations (
            generation_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            transcript_revision_id TEXT NOT NULL REFERENCES meeting_transcript_revisions(transcript_revision_id) ON DELETE CASCADE,
            input_revision INTEGER NOT NULL,
            model_id TEXT NOT NULL,
            model_version TEXT NOT NULL,
            state TEXT NOT NULL,
            created_at_utc_ms INTEGER NOT NULL,
            completed_at_utc_ms INTEGER
        );
        CREATE TABLE meeting_diarization_assignments (
            generation_id TEXT NOT NULL REFERENCES meeting_diarization_generations(generation_id) ON DELETE CASCADE,
            segment_id TEXT NOT NULL REFERENCES meeting_transcript_segments(segment_id) ON DELETE CASCADE,
            speaker_id TEXT NOT NULL REFERENCES meeting_speakers(speaker_id) ON DELETE RESTRICT,
            assignment_kind TEXT NOT NULL,
            PRIMARY KEY (generation_id, segment_id)
        );
        CREATE INDEX meeting_artifact_revisions_session_idx
            ON meeting_artifact_revisions(session_id, generated_at_utc_ms DESC);
        CREATE INDEX meeting_diarization_generations_session_idx
            ON meeting_diarization_generations(session_id, created_at_utc_ms DESC);
        CREATE INDEX meeting_diarization_assignments_segment_idx
            ON meeting_diarization_assignments(segment_id, generation_id);
        ",
    ),
    M::up(
        "
        CREATE TABLE meeting_cloud_state (
            singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
            vault_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            endpoint TEXT NOT NULL,
            cursor TEXT,
            snapshot_high_water TEXT,
            clock_offset_ms INTEGER NOT NULL,
            paused INTEGER NOT NULL CHECK (paused IN (0, 1)),
            updated_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_cloud_capabilities (
            endpoint TEXT PRIMARY KEY NOT NULL,
            capabilities_json TEXT NOT NULL,
            fetched_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_cloud_heads (
            object_id TEXT PRIMARY KEY NOT NULL,
            source_session_id TEXT,
            remote_revision_id TEXT,
            tombstone INTEGER NOT NULL CHECK (tombstone IN (0, 1)),
            acknowledged_revision_id TEXT,
            change_sequence INTEGER NOT NULL CHECK (change_sequence >= 0),
            updated_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_cloud_outbox (
            outbox_id TEXT PRIMARY KEY NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('object', 'tombstone', 'share')),
            object_id TEXT NOT NULL,
            source_session_id TEXT,
            source_revision INTEGER,
            base_remote_revision_id TEXT,
            share_content_kind TEXT CHECK (share_content_kind IN ('capability_bundle', 'browser_markdown')),
            remote_revision_id TEXT,
            upload_id TEXT,
            idempotency_key TEXT NOT NULL UNIQUE,
            state TEXT NOT NULL CHECK (state IN ('pending', 'claimed', 'completed', 'cancelled', 'terminal')),
            attempt_count INTEGER NOT NULL CHECK (attempt_count >= 0),
            next_attempt_utc_ms INTEGER NOT NULL,
            terminal_error TEXT,
            payload_relative_dir TEXT NOT NULL CHECK (payload_relative_dir GLOB '.cloud-outbox/*'),
            claim_token TEXT,
            claimed_at_utc_ms INTEGER,
            created_at_utc_ms INTEGER NOT NULL,
            updated_at_utc_ms INTEGER NOT NULL,
            CHECK ((kind = 'share' AND share_content_kind IS NOT NULL)
                OR (kind != 'share' AND share_content_kind IS NULL))
        );
        CREATE TABLE meeting_cloud_outbox_chunks (
            outbox_id TEXT NOT NULL REFERENCES meeting_cloud_outbox(outbox_id) ON DELETE CASCADE,
            chunk_index INTEGER NOT NULL CHECK (chunk_index >= 0),
            size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
            sha256 TEXT NOT NULL,
            accepted INTEGER NOT NULL CHECK (accepted IN (0, 1)),
            accepted_at_utc_ms INTEGER,
            PRIMARY KEY (outbox_id, chunk_index)
        );
        CREATE TABLE meeting_cloud_conflicts (
            object_id TEXT PRIMARY KEY NOT NULL,
            source_session_id TEXT,
            source_revision INTEGER,
            remote_revision_id TEXT NOT NULL,
            remote_sequence INTEGER NOT NULL CHECK (remote_sequence >= 0),
            remote_bundle_relative_path TEXT NOT NULL CHECK (remote_bundle_relative_path GLOB '.cloud-conflicts/*'),
            detected_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_cloud_shares (
            share_id TEXT PRIMARY KEY NOT NULL,
            object_id TEXT NOT NULL,
            source_session_id TEXT,
            expires_at_utc_ms INTEGER NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('pending', 'active', 'revoked', 'failed')),
            content_kind TEXT NOT NULL CHECK (content_kind IN ('capability_bundle', 'browser_markdown')),
            encrypted_link_material TEXT NOT NULL,
            outbox_id TEXT,
            revoked_at_utc_ms INTEGER,
            created_at_utc_ms INTEGER NOT NULL,
            updated_at_utc_ms INTEGER NOT NULL
        );
        CREATE INDEX meeting_cloud_heads_session_idx
            ON meeting_cloud_heads(source_session_id, updated_at_utc_ms DESC);
        CREATE INDEX meeting_cloud_outbox_due_idx
            ON meeting_cloud_outbox(state, next_attempt_utc_ms, created_at_utc_ms);
        CREATE INDEX meeting_cloud_outbox_session_idx
            ON meeting_cloud_outbox(source_session_id, state, created_at_utc_ms);
        CREATE INDEX meeting_cloud_conflicts_session_idx
            ON meeting_cloud_conflicts(source_session_id, detected_at_utc_ms DESC);
        CREATE INDEX meeting_cloud_shares_session_idx
            ON meeting_cloud_shares(source_session_id, state, expires_at_utc_ms);
        CREATE TRIGGER meeting_cloud_outbox_intent_immutable
        BEFORE UPDATE ON meeting_cloud_outbox
        WHEN NEW.kind IS NOT OLD.kind
          OR NEW.object_id IS NOT OLD.object_id
          OR NEW.source_session_id IS NOT OLD.source_session_id
          OR NEW.source_revision IS NOT OLD.source_revision
          OR NEW.base_remote_revision_id IS NOT OLD.base_remote_revision_id
          OR NEW.idempotency_key IS NOT OLD.idempotency_key
          OR NEW.payload_relative_dir IS NOT OLD.payload_relative_dir
          OR NEW.created_at_utc_ms IS NOT OLD.created_at_utc_ms
        BEGIN SELECT RAISE(ABORT, 'meeting cloud outbox intent is immutable'); END;
        CREATE TRIGGER meeting_cloud_outbox_chunks_mutable_only_before_claim
        BEFORE INSERT ON meeting_cloud_outbox_chunks
        WHEN (SELECT state FROM meeting_cloud_outbox WHERE outbox_id = NEW.outbox_id) != 'pending'
        BEGIN SELECT RAISE(ABORT, 'meeting cloud chunks must be staged before claim'); END;
        CREATE TRIGGER meeting_cloud_outbox_chunk_metadata_immutable
        BEFORE UPDATE ON meeting_cloud_outbox_chunks
        WHEN NEW.outbox_id IS NOT OLD.outbox_id
          OR NEW.chunk_index IS NOT OLD.chunk_index
          OR NEW.size_bytes IS NOT OLD.size_bytes
          OR NEW.sha256 IS NOT OLD.sha256
        BEGIN SELECT RAISE(ABORT, 'meeting cloud chunk metadata is immutable'); END;
        CREATE TRIGGER meeting_cloud_outbox_chunk_acceptance_monotonic
        BEFORE UPDATE OF accepted ON meeting_cloud_outbox_chunks
        WHEN OLD.accepted = 1 AND NEW.accepted != 1
        BEGIN SELECT RAISE(ABORT, 'meeting cloud chunk acceptance is monotonic'); END;
        ",
    ),
    M::up(
        "
        CREATE TABLE meeting_user_notes (
            session_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            body TEXT NOT NULL,
            template_id TEXT NOT NULL,
            note_revision INTEGER NOT NULL CHECK (note_revision >= 0),
            updated_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_action_item_states (
            artifact_id TEXT NOT NULL REFERENCES meeting_artifact_revisions(artifact_id) ON DELETE CASCADE,
            action_index INTEGER NOT NULL CHECK (action_index >= 0),
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            done INTEGER NOT NULL CHECK (done IN (0, 1)),
            updated_at_utc_ms INTEGER NOT NULL,
            PRIMARY KEY (artifact_id, action_index)
        );
        CREATE TABLE meeting_conversation_metrics (
            session_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            input_revision INTEGER NOT NULL,
            metrics_json TEXT NOT NULL,
            computed_at_utc_ms INTEGER NOT NULL
        );
        CREATE INDEX meeting_action_item_states_session_idx
            ON meeting_action_item_states(session_id, artifact_id, action_index);
        ",
    ),
    M::up(
        "
        CREATE TABLE people_state (
            singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
            revision INTEGER NOT NULL CHECK (revision >= 0)
        );
        INSERT INTO people_state(singleton, revision) VALUES (1, 0);
        CREATE TABLE persons (
            id TEXT PRIMARY KEY NOT NULL,
            display_name TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
            aliases_json TEXT NOT NULL,
            calendar_emails_json TEXT NOT NULL,
            created_at_utc_ms INTEGER NOT NULL,
            updated_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_person_links (
            meeting_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            person_id TEXT NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
            source TEXT NOT NULL CHECK (source IN ('calendar', 'speaker', 'title', 'manual')),
            confidence TEXT NOT NULL CHECK (confidence IN ('confirmed', 'suggested')),
            created_at_utc_ms INTEGER NOT NULL,
            PRIMARY KEY (meeting_id, person_id)
        );
        CREATE INDEX meeting_person_links_person_idx
            ON meeting_person_links(person_id, confidence, created_at_utc_ms DESC);

        CREATE TABLE document_state (
            singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
            revision INTEGER NOT NULL CHECK (revision >= 0)
        );
        INSERT INTO document_state(singleton, revision) VALUES (1, 0);
        CREATE TABLE documents (
            id TEXT PRIMARY KEY NOT NULL,
            title TEXT NOT NULL CHECK (length(trim(title)) > 0),
            source_name TEXT NOT NULL CHECK (length(trim(source_name)) > 0),
            media_type TEXT NOT NULL CHECK (media_type IN ('text/plain', 'text/markdown')),
            content TEXT NOT NULL,
            created_at_utc_ms INTEGER NOT NULL,
            updated_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE document_person_links (
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            person_id TEXT NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
            created_at_utc_ms INTEGER NOT NULL,
            PRIMARY KEY (document_id, person_id)
        );
        CREATE INDEX document_person_links_person_idx
            ON document_person_links(person_id, created_at_utc_ms DESC);

        CREATE TABLE workflow_state (
            singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
            revision INTEGER NOT NULL CHECK (revision >= 0)
        );
        INSERT INTO workflow_state(singleton, revision) VALUES (1, 0);
        CREATE TABLE workflow_settings (
            workflow_id TEXT PRIMARY KEY NOT NULL CHECK (workflow_id IN (
                'person_linking', 'pre_meeting_briefing', 'continuity',
                'vocabulary_mining', 'document_linking'
            )),
            enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
        );
        INSERT INTO workflow_settings(workflow_id, enabled) VALUES
            ('person_linking', 1),
            ('pre_meeting_briefing', 1),
            ('continuity', 1),
            ('vocabulary_mining', 1),
            ('document_linking', 1);
        CREATE TABLE workflow_events (
            id TEXT PRIMARY KEY NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN (
                'meeting_finalized', 'meeting_started', 'speaker_renamed',
                'audio_imported', 'doc_ingested', 'calendar_meeting_detected',
                'agent_hook_event'
            )),
            payload_json TEXT NOT NULL,
            occurred_at_utc_ms INTEGER NOT NULL,
            source TEXT NOT NULL,
            dedupe_key TEXT NOT NULL UNIQUE
        );
        CREATE TABLE workflow_runs (
            id TEXT PRIMARY KEY NOT NULL,
            workflow_id TEXT NOT NULL REFERENCES workflow_settings(workflow_id),
            event_id TEXT NOT NULL REFERENCES workflow_events(id) ON DELETE CASCADE,
            status TEXT NOT NULL CHECK (status IN ('ok', 'failed', 'skipped')),
            started_at_utc_ms INTEGER NOT NULL,
            finished_at_utc_ms INTEGER NOT NULL,
            outcome_summary TEXT NOT NULL,
            error TEXT
        );
        CREATE UNIQUE INDEX workflow_runs_once_idx
            ON workflow_runs(workflow_id, event_id)
            WHERE status IN ('ok', 'failed');
        CREATE INDEX workflow_runs_list_idx
            ON workflow_runs(started_at_utc_ms DESC, id DESC);
        CREATE TABLE meeting_calendar_facts (
            session_id TEXT PRIMARY KEY NOT NULL
                REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            event_key TEXT NOT NULL,
            event_json TEXT NOT NULL
        );
        ",
    ),
    M::up(
        "
        ALTER TABLE workflow_state
            ADD COLUMN run_revision INTEGER NOT NULL DEFAULT 0
            CHECK (run_revision >= 0);
        CREATE TABLE document_ingest_operations (
            operation_id TEXT PRIMARY KEY NOT NULL,
            result_json TEXT NOT NULL,
            created_at_utc_ms INTEGER NOT NULL
        );
        ",
    ),
    M::up(concat!(
        "
        CREATE TABLE meeting_series_consents (
            series_key TEXT PRIMARY KEY NOT NULL,
            policy_version INTEGER NOT NULL,
            granted_at_utc_ms INTEGER NOT NULL,
            acknowledged_sources_json TEXT NOT NULL,
            revoked_at_utc_ms INTEGER
        );
        CREATE TABLE meeting_consent_panel_state (
            singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
            first_prompt_shown_at_utc_ms INTEGER
        );
        INSERT INTO meeting_consent_panel_state(singleton, first_prompt_shown_at_utc_ms)
        VALUES (1, NULL);
        ",
        rebuilt_workflow_tables!(
            workflow_ids: "
                'person_linking', 'pre_meeting_briefing', 'continuity',
                'vocabulary_mining', 'document_linking', 'meeting_activity'
            ",
            seeded: "('meeting_activity', 1)",
            event_kinds: "
                'meeting_finalized', 'meeting_started', 'speaker_renamed',
                'audio_imported', 'doc_ingested', 'calendar_meeting_detected',
                'agent_hook_event', 'meeting_prompt_recorded',
                'meeting_prompt_ignored', 'meeting_auto_record_started',
                'meeting_auto_record_stopped'
            ",
        ),
    )),
    // The six local learning loops. Two allowed-value lists have to widen —
    // `workflow_settings.workflow_id` and `workflow_events.kind` — so the three
    // workflow tables go through the shared rebuild.
    M::up(concat!(
        "
        ALTER TABLE workflow_state
            ADD COLUMN learning_revision INTEGER NOT NULL DEFAULT 0
            CHECK (learning_revision >= 0);
        ",
        rebuilt_workflow_tables!(
            workflow_ids: "
                'person_linking', 'pre_meeting_briefing', 'continuity',
                'vocabulary_mining', 'document_linking', 'meeting_activity',
                'spoken_punctuation', 'correction_learning', 'mode_habits',
                'capture_advisor', 'series_priming'
            ",
            seeded: "
                ('spoken_punctuation', 1),
                ('correction_learning', 1),
                ('mode_habits', 1),
                ('capture_advisor', 1),
                ('series_priming', 1)
            ",
            event_kinds: "
                'meeting_finalized', 'meeting_started', 'speaker_renamed',
                'audio_imported', 'doc_ingested', 'calendar_meeting_detected',
                'agent_hook_event', 'meeting_prompt_recorded',
                'meeting_prompt_ignored', 'meeting_auto_record_started',
                'meeting_auto_record_stopped', 'dictation_corpus_swept',
                'dictation_correction_recorded'
            ",
        ),
        "

        CREATE TABLE learning_decisions (
            loop_kind TEXT NOT NULL CHECK (loop_kind IN (
                'spoken_punctuation', 'vocabulary_term', 'vocabulary_correction',
                'mode_habit', 'capture_advice'
            )),
            candidate_key TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('accepted', 'dismissed')),
            display_text TEXT NOT NULL,
            decided_at_utc_ms INTEGER NOT NULL,
            PRIMARY KEY (loop_kind, candidate_key)
        );
        CREATE TABLE learning_suggestions (
            loop_kind TEXT NOT NULL CHECK (loop_kind IN (
                'spoken_punctuation', 'vocabulary_correction', 'mode_habit',
                'capture_advice'
            )),
            candidate_key TEXT NOT NULL,
            suggestion_json TEXT NOT NULL,
            evidence_json TEXT NOT NULL,
            generated_at_utc_ms INTEGER NOT NULL,
            PRIMARY KEY (loop_kind, candidate_key)
        );
        CREATE INDEX learning_suggestions_recent_idx
            ON learning_suggestions(generated_at_utc_ms DESC, loop_kind, candidate_key);
        CREATE TABLE learning_cursors (
            loop_kind TEXT PRIMARY KEY NOT NULL CHECK (loop_kind IN (
                'spoken_punctuation', 'mode_habit', 'capture_advice'
            )),
            last_run_receipt_id INTEGER NOT NULL CHECK (last_run_receipt_id >= 0)
        );
        INSERT INTO learning_cursors(loop_kind, last_run_receipt_id) VALUES
            ('spoken_punctuation', 0),
            ('mode_habit', 0),
            ('capture_advice', 0);
        -- The one evidence ledger every loop counts into.
        --
        -- A loop reads a bounded slice of the corpus per run, so no single pass
        -- can see enough to clear an evidence floor; the floors are read from
        -- here instead. `occurrences` is the count a floor tests and
        -- `sample_size` is the count it is a fraction of, which is 0 for the
        -- loops that have no ratio. Rows are bucketed by local day because
        -- three times today is not the same evidence as three days running,
        -- and the day is also what bounds this table's growth.
        CREATE TABLE learning_observations (
            loop_kind TEXT NOT NULL CHECK (loop_kind IN (
                'spoken_punctuation', 'vocabulary_term', 'vocabulary_correction',
                'mode_habit', 'capture_advice'
            )),
            candidate_key TEXT NOT NULL,
            local_day TEXT NOT NULL,
            occurrences INTEGER NOT NULL CHECK (occurrences >= 0),
            sample_size INTEGER NOT NULL CHECK (sample_size >= 0),
            display_text TEXT NOT NULL,
            example_context TEXT,
            PRIMARY KEY (loop_kind, candidate_key, local_day)
        );
        CREATE INDEX learning_observations_day_idx
            ON learning_observations(local_day);
        -- What number loop 6 last told the user about a subject, and which
        -- generation of that advice it was.
        --
        -- Dismissal memory is absolute: a dismissed candidate never comes back.
        -- Advice that must reappear when a statistic moves materially therefore
        -- has to become a *different* candidate, and the generation counted here
        -- is what makes it one. It advances only on a material move, so a
        -- statistic drifting across a threshold cannot flip a candidate back and
        -- forth.
        CREATE TABLE learning_advice_baselines (
            subject_key TEXT PRIMARY KEY NOT NULL,
            stat_permille INTEGER NOT NULL CHECK (stat_permille >= 0),
            generation INTEGER NOT NULL CHECK (generation >= 0),
            advised_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_series_priming (
            session_id TEXT PRIMARY KEY NOT NULL
                REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            series_key TEXT NOT NULL,
            blob_json TEXT NOT NULL,
            assembled_at_utc_ms INTEGER NOT NULL
        );
        ",
    )),
    // Verbatim evidence dies with the meeting it came from.
    //
    // `learning_observations.example_context` holds up to 120 characters of the
    // user's own text, and for the correction loop that text is a slice of a
    // meeting transcript edit. Without this column the excerpt outlived the
    // meeting by up to `OBSERVATION_RETENTION_DAYS`, and its second copy in
    // `learning_suggestions.evidence_json` outlived it too. `NULL` is the
    // dictation-sourced case, whose lifetime is the retention horizon rather
    // than a session's.
    //
    // The index exists for the cascade: SQLite scans the child column on every
    // parent delete, and meeting deletion is not a rare path.
    //
    // The trigger is the second copy's owner. A pending suggestion carries the
    // excerpts its card renders inside `evidence_json`, so losing the ledger
    // rows is not enough on its own — and putting the rule here rather than in
    // one Rust delete path is what makes it hold for both ways rows leave the
    // ledger, the cascade above and the retention horizon, without a caller
    // having to remember either.
    M::up(
        "
        ALTER TABLE learning_observations
            ADD COLUMN source_session_id TEXT
            REFERENCES meeting_sessions(id) ON DELETE CASCADE;
        CREATE INDEX learning_observations_session_idx
            ON learning_observations(source_session_id);
        CREATE TRIGGER learning_suggestions_need_evidence
        AFTER DELETE ON learning_observations
        BEGIN
            DELETE FROM learning_suggestions
             WHERE loop_kind = OLD.loop_kind
               AND candidate_key = OLD.candidate_key
               AND NOT EXISTS (
                    SELECT 1 FROM learning_observations
                     WHERE loop_kind = OLD.loop_kind
                       AND candidate_key = OLD.candidate_key
               );
        END;
        ",
    ),
    // A failed run is no longer the last word for every event kind, so the
    // once-only index narrows to the invariant that is actually absolute: a
    // workflow succeeds at most once per event. See
    // `WorkflowEventKind::retries_after_failure`.
    M::up(
        "
        DROP INDEX workflow_runs_once_idx;
        CREATE UNIQUE INDEX workflow_runs_once_idx
            ON workflow_runs(workflow_id, event_id)
            WHERE status = 'ok';
        ",
    ),
    // Loops that close. A ledger row's words live in the artifact revision
    // that produced them and are re-read on every regeneration, so what a
    // person did about a row cannot live there. This table holds only the
    // departures from open: no row means open, unassigned, never carried, and
    // that is why nothing has to be backfilled for meetings recorded before
    // it existed. `revision` is the per-row fence, and revision 0 is the
    // absent row, so a first write and a later one are fenced the same way.
    //
    // `carried_into_loop_id` is deliberately not a foreign key. It holds a
    // derived loop id, and the successor it names is by definition the loop
    // nobody has touched yet — open, so no row of its own. A self-reference
    // here would make the only write that sets the column impossible.
    M::up(
        "
        CREATE TABLE meeting_loop_states (
            loop_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            kind TEXT NOT NULL CHECK (kind IN ('loop', 'commitment')),
            status TEXT NOT NULL CHECK (status IN ('open', 'done', 'dropped', 'carried')),
            owner_person_id TEXT REFERENCES persons(id) ON DELETE SET NULL,
            resolved_at_utc_ms INTEGER,
            resolving_operation_id TEXT,
            carried_into_loop_id TEXT,
            revision INTEGER NOT NULL CHECK (revision > 0),
            updated_at_utc_ms INTEGER NOT NULL
        );
        CREATE INDEX meeting_loop_states_session_idx
            ON meeting_loop_states(session_id, kind);
        CREATE INDEX meeting_loop_states_carry_idx
            ON meeting_loop_states(carried_into_loop_id);
        CREATE INDEX meeting_loop_states_owner_idx
            ON meeting_loop_states(owner_person_id);
        ",
    ),
    // D21 series templates and D20's evening digest.
    //
    // A series preference is keyed on the same `series_key` standing consent
    // and loop 4's priming already use — EventKit's calendar-item identifier —
    // so a recurring meeting's choice is remembered by the thing that makes it
    // recurring, and a meeting with no calendar event simply has no row.
    // `revision` is global rather than per row because the surfaces that write
    // it read the whole preference at once; the counter is what fences two
    // windows writing the same series.
    //
    // The digest needs its own workflow id and its own event kind, and both
    // allowed-value lists are `CHECK` constraints, so the three workflow
    // tables go through the shared rebuild. `daily_digest` is seeded enabled:
    // its real switch is `meeting_digest_enabled` in app settings, and the
    // scheduler is what reads it.
    M::up(concat!(
        "
        CREATE TABLE meeting_series_state (
            singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
            revision INTEGER NOT NULL CHECK (revision >= 0)
        );
        INSERT INTO meeting_series_state(singleton, revision) VALUES (1, 0);
        CREATE TABLE meeting_series_preferences (
            series_key TEXT PRIMARY KEY NOT NULL CHECK (length(trim(series_key)) > 0),
            template_id TEXT NOT NULL CHECK (length(trim(template_id)) > 0),
            updated_at_utc_ms INTEGER NOT NULL
        );
        ",
        rebuilt_workflow_tables!(
            workflow_ids: "
                'person_linking', 'pre_meeting_briefing', 'continuity',
                'vocabulary_mining', 'document_linking', 'meeting_activity',
                'spoken_punctuation', 'correction_learning', 'mode_habits',
                'capture_advisor', 'series_priming', 'daily_digest'
            ",
            seeded: "('daily_digest', 1)",
            event_kinds: "
                'meeting_finalized', 'meeting_started', 'speaker_renamed',
                'audio_imported', 'doc_ingested', 'calendar_meeting_detected',
                'agent_hook_event', 'meeting_prompt_recorded',
                'meeting_prompt_ignored', 'meeting_auto_record_started',
                'meeting_auto_record_stopped', 'dictation_corpus_swept',
                'dictation_correction_recorded', 'daily_digest_due'
            ",
        ),
    )),
    // The semantic half of the query plane. Both tables are a cache of vectors
    // derived from this session's own artifact and transcript: dropping them
    // costs one re-index and loses nothing, which is why they carry no
    // revision, no receipt, and no consent of their own. They live here rather
    // than beside the dictation index because a meeting's words are encrypted
    // in this database and are deleted with it — a copy in another file would
    // outlive the retention sweep that is supposed to reach it.
    M::up(
        "
        CREATE TABLE meeting_semantic_chunks (
            chunk_id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            text TEXT NOT NULL,
            embedding BLOB NOT NULL,
            model_revision TEXT NOT NULL
        );
        CREATE INDEX meeting_semantic_chunks_session
            ON meeting_semantic_chunks(session_id);
        CREATE TABLE meeting_semantic_index_state (
            session_id TEXT PRIMARY KEY NOT NULL
                REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            indexed_key TEXT NOT NULL,
            model_revision TEXT NOT NULL,
            indexed_at_utc_ms INTEGER NOT NULL
        );
        ",
    ),
    // D28. A series preference is no longer only a template, so `template_id`
    // stops being mandatory: a series that has been taken out of the evening
    // digest but never chose a template is a legitimate row, and before this it
    // was unrepresentable. SQLite cannot relax a NOT NULL in place, so the
    // table is rebuilt — which is also the cheapest moment to add the column.
    //
    // Every existing row is digest-included, because every existing row was
    // written before the choice existed and inclusion is what the digest did.
    M::up(
        "
        CREATE TABLE meeting_series_preferences_rebuilt (
            series_key TEXT PRIMARY KEY NOT NULL CHECK (length(trim(series_key)) > 0),
            template_id TEXT CHECK (template_id IS NULL OR length(trim(template_id)) > 0),
            digest_included INTEGER NOT NULL DEFAULT 1 CHECK (digest_included IN (0, 1)),
            updated_at_utc_ms INTEGER NOT NULL
        );
        INSERT INTO meeting_series_preferences_rebuilt (
            series_key, template_id, digest_included, updated_at_utc_ms
        )
        SELECT series_key, template_id, 1, updated_at_utc_ms
          FROM meeting_series_preferences;
        DROP TABLE meeting_series_preferences;
        ALTER TABLE meeting_series_preferences_rebuilt
            RENAME TO meeting_series_preferences;
        ",
    ),
    // D22 local automations: what a series does after a meeting, and what
    // happened when it did.
    //
    // `meeting_series_automations` is one row per series per kind, holding the
    // switch and its target together. That pairing is the point: "webhook
    // enabled" with no URL is unrunnable, so the two are written under one
    // receipt and no reader can ever see half a decision. There is no row for a
    // kind nobody has touched, which is why nothing is backfilled — absent means
    // off, everywhere.
    //
    // `meeting_automation_runs` is the run log, and its primary key is the
    // once-per-artifact-revision gate: an attempt inserts before it touches
    // anything outside this process, so a second attempt for the same notes is
    // refused by SQLite rather than by timing. Nothing retries, so `started`
    // with no finish is a real terminal state — the app was quit or crashed mid
    // attempt — and it is left visible instead of swept.
    //
    // `series_key` here is not a foreign key. There is no series table: the key
    // is EventKit's calendar-item identifier, the same string standing consent
    // and D21's preference use, and a series is only ever known through the
    // meetings that carry it.
    M::up(
        "
        CREATE TABLE meeting_automation_state (
            singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
            revision INTEGER NOT NULL CHECK (revision >= 0)
        );
        INSERT INTO meeting_automation_state(singleton, revision) VALUES (1, 0);
        CREATE TABLE meeting_series_automations (
            series_key TEXT NOT NULL CHECK (length(trim(series_key)) > 0),
            kind TEXT NOT NULL CHECK (kind IN ('reminders', 'shortcut', 'webhook')),
            enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
            target TEXT,
            updated_at_utc_ms INTEGER NOT NULL,
            PRIMARY KEY (series_key, kind)
        );
        CREATE TABLE meeting_automation_runs (
            artifact_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('reminders', 'shortcut', 'webhook')),
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            series_key TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('started', 'committed', 'failed')),
            failure TEXT,
            detail TEXT,
            effects INTEGER NOT NULL CHECK (effects >= 0),
            started_at_utc_ms INTEGER NOT NULL,
            finished_at_utc_ms INTEGER,
            PRIMARY KEY (artifact_id, kind)
        );
        CREATE INDEX meeting_automation_runs_session_idx
            ON meeting_automation_runs(session_id, started_at_utc_ms DESC);
        ",
    ),
    // D14. Which series keep their text on this Mac while meeting intelligence
    // is routed to the operator's own server.
    //
    // A column beside the template and the digest flag rather than a table of
    // its own: it is one boolean per series, written by the same fenced,
    // receipted path as the other two, and a second per-series table would be
    // a second place to look for what one series has decided.
    //
    // Default 0 means "follow the global setting", so nothing is backfilled and
    // a series that has never been considered behaves exactly as the switch
    // says. The exclusion is the only thing worth storing.
    M::up(
        "
        ALTER TABLE meeting_series_preferences
            ADD COLUMN remote_intelligence_opt_out INTEGER NOT NULL DEFAULT 0
            CHECK (remote_intelligence_opt_out IN (0, 1));
        ",
    ),
    // Organization is a disposable projection of calendar facts, recomputed by
    // the person-linking workflow rather than edited as a second identity.
    M::up(
        "
        ALTER TABLE persons ADD COLUMN organization TEXT;
        ",
    ),
    // PREP and WRAP are meeting-activity receipts. Their event kinds widen the
    // workflow event constraint; no new configurable workflow is introduced.
    M::up(rebuilt_workflow_tables!(
        workflow_ids: "
            'person_linking', 'pre_meeting_briefing', 'continuity',
            'vocabulary_mining', 'document_linking', 'meeting_activity',
            'spoken_punctuation', 'correction_learning', 'mode_habits',
            'capture_advisor', 'series_priming', 'daily_digest'
        ",
        seeded: "('daily_digest', 1) ON CONFLICT(workflow_id) DO NOTHING",
        event_kinds: "
            'meeting_finalized', 'meeting_started', 'speaker_renamed',
            'audio_imported', 'doc_ingested', 'calendar_meeting_detected',
            'agent_hook_event', 'meeting_prompt_recorded',
            'meeting_prompt_ignored', 'meeting_auto_record_started',
            'meeting_auto_record_stopped', 'dictation_corpus_swept',
            'dictation_correction_recorded', 'daily_digest_due',
            'meeting_prep_presented', 'meeting_prep_record_armed',
            'meeting_prep_brief_opened', 'meeting_prep_dismissed',
            'meeting_wrap_presented', 'meeting_wrap_notes_opened',
            'meeting_wrap_follow_up_copied', 'meeting_wrap_done'
        ",
    )),
    // A deleted meeting's thirty-day undo bin, and the disclosure a recording
    // was asked to post about itself.
    //
    // The deletion receipt already says a meeting was deleted and when. What it
    // could not say is where the audio went and what would have to be put back,
    // so a restorable deletion adds exactly that: the trashed directory's
    // relative path, and the meeting's own cloud bundle — the portable,
    // non-audio snapshot the sync path already builds — as the thing a restore
    // imports. The bundle is captured on the job when the deletion is reserved,
    // because that is the last moment the meeting is still in a phase the bundle
    // accepts, and moves to the receipt when the rows go. All three are NULL for
    // a deletion this build cannot restore: the ones an earlier build reserved,
    // and the ones whose bundle could not be built.
    //
    // The bundle lives in this database rather than beside the audio because
    // this database is the encrypted one. A meeting's transcript must not be
    // readable on disk for thirty days as the price of an undo button.
    //
    // `announce_in_chat` is the series decision behind the consent panel's
    // announce checkbox, and `disclosure_json` is what one session's disclosure
    // did — asked for, and then delivery's own receipt for the attempt. Both
    // default to absent, which is what every meeting recorded before this build
    // was: nothing was announced.
    M::up(
        "
        ALTER TABLE meeting_deletion_receipts ADD COLUMN trash_relative_path TEXT;
        ALTER TABLE meeting_deletion_receipts ADD COLUMN restore_bundle_json TEXT;
        ALTER TABLE meeting_deletion_jobs ADD COLUMN restore_bundle_json TEXT;
        ALTER TABLE meeting_series_preferences
            ADD COLUMN announce_in_chat INTEGER NOT NULL DEFAULT 0
            CHECK (announce_in_chat IN (0, 1));
        ALTER TABLE meeting_sessions ADD COLUMN disclosure_json TEXT;
        ",
    ),
    // The relationship paragraph a person's page reads under their name.
    //
    // Three columns beside `organization` rather than a table of its own: it is
    // one paragraph per person, written by the same artifact pass that recomputes
    // the organization, and it is as disposable as that projection — regenerating
    // it is one model call away, so nothing here is backfilled and a person
    // Sona has never processed a meeting for simply has none.
    //
    // `summary_generated_at_utc_ms` and `summary_model_id` travel with the text
    // because a paragraph whose engine is not named cannot be read honestly: the
    // same three sentences mean something different written on this Mac and
    // written on the operator's own server.
    M::up(
        "
        ALTER TABLE persons ADD COLUMN summary TEXT;
        ALTER TABLE persons ADD COLUMN summary_generated_at_utc_ms INTEGER;
        ALTER TABLE persons ADD COLUMN summary_model_id TEXT;
        ",
    ),
    // Saved prompts: a question the operator wrote once, and every answer it
    // has produced.
    //
    // `saved_prompts` is a preference table like the series ones — fenced on
    // one shared revision, written under an `OperationReceipt`. The three rows
    // seeded here are ordinary rows with fixed ids, editable and deletable:
    // there is no built-in prompt whose behaviour a reader cannot inspect, and
    // the ids are fixed only so a re-run of this migration cannot double them.
    //
    // `saved_prompt_runs` is the run log, and it *is* the receipt for one
    // generation rather than a copy of one — the same rule
    // `meeting_automation_runs` follows. Two deletions reach it. Deleting the
    // prompt takes its answers with it, because an answer is derived from the
    // prompt and an orphan run has no name to show and no way to be re-run.
    // Deleting the meeting a run was anchored to takes it as well, which is
    // what `anchor_session_id` is for: a run quotes words that live in this
    // database and must not outlive the retention sweep that reaches them.
    // A person or series run is anchored to the newest meeting behind that
    // noun, so it is cut on the conservative side of the same rule.
    M::up(
        "
        CREATE TABLE saved_prompt_state (
            singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
            revision INTEGER NOT NULL CHECK (revision >= 0)
        );
        INSERT INTO saved_prompt_state(singleton, revision) VALUES (1, 0);
        CREATE TABLE saved_prompts (
            prompt_id TEXT PRIMARY KEY NOT NULL,
            name TEXT NOT NULL CHECK (length(trim(name)) > 0),
            body TEXT NOT NULL CHECK (length(trim(body)) > 0),
            output_kind TEXT NOT NULL CHECK (output_kind IN ('text', 'schema')),
            json_schema TEXT,
            target TEXT NOT NULL CHECK (target IN ('meeting', 'person', 'series')),
            created_at_utc_ms INTEGER NOT NULL,
            updated_at_utc_ms INTEGER NOT NULL,
            CHECK ((output_kind = 'schema') = (json_schema IS NOT NULL))
        );
        INSERT INTO saved_prompts (
            prompt_id, name, body, output_kind, json_schema, target,
            created_at_utc_ms, updated_at_utc_ms
        )
        SELECT prompt_id, name, body, 'text', NULL, 'meeting', seeded, seeded
          FROM (SELECT CAST(strftime('%s', 'now') AS INTEGER) * 1000 AS seeded)
          JOIN (
            SELECT '5a7ed01d-0000-4000-8000-000000000001' AS prompt_id,
                   'Decisions with owners' AS name,
                   'List the decisions this meeting reached. Name who owns each one, and quote the words it was decided in. If nothing was decided, say so.' AS body
            UNION ALL SELECT '5a7ed01d-0000-4000-8000-000000000002',
                   'Risks and open questions',
                   'List the risks this meeting raised and the questions it left open. One sentence each. Leave out anything that was settled.'
            UNION ALL SELECT '5a7ed01d-0000-4000-8000-000000000003',
                   'Customer asks',
                   'List what the customer asked for, in their own words where you can. Mark anything that was already promised to them.'
          );
        CREATE TABLE saved_prompt_runs (
            run_id TEXT PRIMARY KEY NOT NULL,
            prompt_id TEXT NOT NULL REFERENCES saved_prompts(prompt_id) ON DELETE CASCADE,
            target_kind TEXT NOT NULL CHECK (target_kind IN ('meeting', 'person', 'series')),
            target_id TEXT NOT NULL,
            anchor_session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            artifact_id TEXT,
            model_id TEXT NOT NULL,
            model_version TEXT NOT NULL,
            produced_at_utc_ms INTEGER NOT NULL,
            result_kind TEXT NOT NULL CHECK (result_kind IN ('text', 'json', 'failed')),
            result TEXT NOT NULL
        );
        CREATE INDEX saved_prompt_runs_target_idx
            ON saved_prompt_runs(target_kind, target_id, produced_at_utc_ms DESC);
        CREATE INDEX saved_prompt_runs_session_idx
            ON saved_prompt_runs(anchor_session_id);
        ",
    ),
    // D22 gains a fourth kind. SQLite cannot alter a `CHECK`, so both tables
    // that name the kinds are rebuilt; only the allowed-value list changes.
    // Nothing is backfilled — absent still means off, everywhere.
    M::up(
        "
        CREATE TABLE meeting_series_automations_rebuilt (
            series_key TEXT NOT NULL CHECK (length(trim(series_key)) > 0),
            kind TEXT NOT NULL CHECK (kind IN ('reminders', 'shortcut', 'webhook', 'run_prompt')),
            enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
            target TEXT,
            updated_at_utc_ms INTEGER NOT NULL,
            PRIMARY KEY (series_key, kind)
        );
        INSERT INTO meeting_series_automations_rebuilt
        SELECT * FROM meeting_series_automations;
        DROP TABLE meeting_series_automations;
        ALTER TABLE meeting_series_automations_rebuilt
            RENAME TO meeting_series_automations;

        DROP INDEX meeting_automation_runs_session_idx;
        CREATE TABLE meeting_automation_runs_rebuilt (
            artifact_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('reminders', 'shortcut', 'webhook', 'run_prompt')),
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            series_key TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('started', 'committed', 'failed')),
            failure TEXT,
            detail TEXT,
            effects INTEGER NOT NULL CHECK (effects >= 0),
            started_at_utc_ms INTEGER NOT NULL,
            finished_at_utc_ms INTEGER,
            PRIMARY KEY (artifact_id, kind)
        );
        INSERT INTO meeting_automation_runs_rebuilt
        SELECT * FROM meeting_automation_runs;
        DROP TABLE meeting_automation_runs;
        ALTER TABLE meeting_automation_runs_rebuilt
            RENAME TO meeting_automation_runs;
        CREATE INDEX meeting_automation_runs_session_idx
            ON meeting_automation_runs(session_id, started_at_utc_ms DESC);
        ",
    ),
    // Voice identity is local evidence inside the existing SQLCipher corpus. It
    // deliberately has no cloud-facing projection or export path.
    // The model-key columns hold revisioned tags the embedding module owns, so
    // SQL only requires their presence. Rust compares the whole key before a
    // profile is written or matched, and every centroid is rebuilt through
    // `SpeakerEmbedding`, which is what actually enforces L2 normalization.
    M::up(
        "
        CREATE TABLE voice_profiles (
            person_id TEXT PRIMARY KEY NOT NULL REFERENCES persons(id) ON DELETE RESTRICT,
            model_id TEXT NOT NULL CHECK (length(trim(model_id)) > 0),
            model_revision TEXT NOT NULL CHECK (length(trim(model_revision)) > 0),
            model_sha256 TEXT NOT NULL CHECK (length(model_sha256) = 64),
            embedding_dimensions INTEGER NOT NULL CHECK (embedding_dimensions = 256),
            sample_rate_hz INTEGER NOT NULL CHECK (sample_rate_hz = 16000),
            feature_bins INTEGER NOT NULL CHECK (feature_bins = 80),
            feature_pipeline_revision TEXT NOT NULL CHECK (length(trim(feature_pipeline_revision)) > 0),
            normalization TEXT NOT NULL CHECK (length(trim(normalization)) > 0),
            centroid BLOB NOT NULL CHECK (length(centroid) = 1024),
            sample_count INTEGER NOT NULL CHECK (sample_count > 0),
            profile_revision INTEGER NOT NULL CHECK (profile_revision > 0),
            consent_version INTEGER NOT NULL CHECK (consent_version > 0),
            created_at_utc_ms INTEGER NOT NULL,
            updated_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE voice_profile_samples (
            person_id TEXT NOT NULL REFERENCES voice_profiles(person_id) ON DELETE CASCADE,
            source_session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE RESTRICT,
            source_generation_id TEXT NOT NULL REFERENCES meeting_diarization_generations(generation_id) ON DELETE RESTRICT,
            source_speaker_id TEXT NOT NULL REFERENCES meeting_speakers(speaker_id) ON DELETE RESTRICT,
            source_track_id TEXT NOT NULL REFERENCES meeting_source_tracks(track_id) ON DELETE RESTRICT,
            start_offset_ns INTEGER NOT NULL CHECK (start_offset_ns >= 0),
            end_offset_ns INTEGER NOT NULL CHECK (end_offset_ns > start_offset_ns),
            embedding BLOB NOT NULL CHECK (length(embedding) = 1024),
            created_at_utc_ms INTEGER NOT NULL,
            PRIMARY KEY (person_id, source_generation_id, source_speaker_id)
        );
        CREATE TABLE voice_speaker_matches (
            speaker_id TEXT PRIMARY KEY NOT NULL REFERENCES meeting_speakers(speaker_id) ON DELETE CASCADE,
            session_id TEXT NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
            person_id TEXT NOT NULL REFERENCES persons(id) ON DELETE RESTRICT,
            profile_revision INTEGER NOT NULL CHECK (profile_revision > 0),
            speaker_revision INTEGER NOT NULL CHECK (speaker_revision >= 0),
            matched_at_utc_ms INTEGER NOT NULL
        );
        CREATE TABLE meeting_diarization_evidence_spans (
            generation_id TEXT NOT NULL REFERENCES meeting_diarization_generations(generation_id) ON DELETE CASCADE,
            speaker_id TEXT NOT NULL REFERENCES meeting_speakers(speaker_id) ON DELETE CASCADE,
            track_id TEXT NOT NULL REFERENCES meeting_source_tracks(track_id) ON DELETE CASCADE,
            start_offset_ns INTEGER NOT NULL CHECK (start_offset_ns >= 0),
            end_offset_ns INTEGER NOT NULL CHECK (end_offset_ns > start_offset_ns),
            evidence_kind TEXT NOT NULL CHECK (evidence_kind IN ('sortformer_exclusive', 'wespeaker_unambiguous_window')),
            PRIMARY KEY (generation_id, speaker_id, start_offset_ns, end_offset_ns)
        );
        CREATE INDEX voice_profile_samples_session_idx
            ON voice_profile_samples(source_session_id, person_id);
        CREATE INDEX voice_profile_samples_speaker_idx
            ON voice_profile_samples(source_speaker_id, person_id);
        CREATE INDEX voice_speaker_matches_person_idx
            ON voice_speaker_matches(person_id);
        CREATE INDEX meeting_diarization_evidence_spans_generation_idx
            ON meeting_diarization_evidence_spans(generation_id, speaker_id);
        CREATE TRIGGER voice_profile_samples_source_session
        BEFORE INSERT ON voice_profile_samples
        WHEN NEW.source_session_id != (SELECT session_id FROM meeting_diarization_generations WHERE generation_id = NEW.source_generation_id)
          OR NEW.source_session_id != (SELECT session_id FROM meeting_speakers WHERE speaker_id = NEW.source_speaker_id)
          OR NEW.source_session_id != (SELECT session_id FROM meeting_source_tracks WHERE track_id = NEW.source_track_id)
        BEGIN SELECT RAISE(ABORT, 'voice profile source session mismatch'); END;
        ",
    ),
    // Manual voice decisions are separate from automatic profile overlays. The
    // immutable operation key keeps an idempotent response available after a
    // later correction replaces the current decision for the same speaker.
    M::up(
        "
        CREATE TABLE meeting_voice_speaker_resolutions (
            operation_id TEXT PRIMARY KEY NOT NULL,
            speaker_id TEXT NOT NULL REFERENCES meeting_speakers(speaker_id) ON DELETE CASCADE,
            resolution_kind TEXT NOT NULL CHECK (resolution_kind IN ('identified', 'unknown')),
            resolved_person_id TEXT,
            is_current INTEGER NOT NULL CHECK (is_current IN (0, 1)),
            CHECK (
                (resolution_kind = 'identified' AND resolved_person_id IS NOT NULL)
                OR (resolution_kind = 'unknown' AND resolved_person_id IS NULL)
            )
        );
        CREATE UNIQUE INDEX meeting_voice_speaker_resolutions_current_speaker_idx
            ON meeting_voice_speaker_resolutions(speaker_id)
            WHERE is_current = 1;
        ",
    ),
    // A diarized source can train exactly one person. The store also checks
    // this before inserting so callers receive Conflict rather than SQLite's
    // generic constraint error.
    M::up(
        "
        CREATE INDEX voice_profile_samples_enrollment_evidence_idx
            ON voice_profile_samples(source_generation_id, source_speaker_id);
        CREATE TRIGGER voice_profile_samples_unique_enrollment_evidence
        BEFORE INSERT ON voice_profile_samples
        WHEN EXISTS (
            SELECT 1 FROM voice_profile_samples
             WHERE source_generation_id = NEW.source_generation_id
               AND source_speaker_id = NEW.source_speaker_id
        )
        BEGIN SELECT RAISE(ABORT, 'voice profile enrollment evidence already claimed'); END;
        ",
    ),
];
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreError {
    Unavailable,
    StorageUnavailable,
    EncryptionUnavailable,
    Corrupt,
    NotFound,
    Conflict,
    Invalid,
    ConsentStale,
    ExplicitConsentRequired,
    StaleRevision,
    PersonNotFound,
    SpeakerNotFound,
    LocalEvidenceUnavailable,
    LocalModelUnavailable,
    InsufficientEnrollmentEvidence,
    ProfileModelIncompatible,
    ProfileMergeResolutionRequired,
    VoiceInvariant,
    Io,
}

/// The single durable Cloudflare vault cursor and clock state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudState {
    pub vault_id: String,
    pub device_id: String,
    pub endpoint: String,
    pub cursor: Option<String>,
    pub snapshot_high_water: Option<String>,
    pub clock_offset_ms: i64,
    pub paused: bool,
}

/// Endpoint-scoped worker capability metadata that must never be reused across endpoints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudCapabilitiesCache {
    pub endpoint: String,
    pub capabilities_json: String,
    pub fetched_at_utc_ms: i64,
}

/// The immutable kind of durable cloud work represented by an outbox row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloudOutboxKind {
    Object,
    Tombstone,
    Share,
}

/// The exclusive ownership state of a durable outbox row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloudOutboxState {
    Pending,
    Claimed,
    Completed,
    Cancelled,
    Terminal,
}

/// Immutable input used to allocate one uniquely staged cloud outbox row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudOutboxInput {
    pub kind: CloudOutboxKind,
    pub object_id: String,
    pub source_session_id: Option<MeetingSessionId>,
    pub source_revision: Option<u64>,
    pub base_remote_revision_id: Option<String>,
    pub share_content_kind: Option<CloudShareContentKind>,
    pub remote_revision_id: Option<String>,
    pub idempotency_key: String,
    pub next_attempt_utc_ms: i64,
}

/// One durable cloud work item whose intent columns never change after enqueue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudOutboxRecord {
    pub outbox_id: String,
    pub kind: CloudOutboxKind,
    pub object_id: String,
    pub source_session_id: Option<MeetingSessionId>,
    pub source_revision: Option<u64>,
    pub base_remote_revision_id: Option<String>,
    pub share_content_kind: Option<CloudShareContentKind>,
    pub remote_revision_id: Option<String>,
    pub upload_id: Option<String>,
    pub idempotency_key: String,
    pub state: CloudOutboxState,
    pub attempt_count: u32,
    pub next_attempt_utc_ms: i64,
    pub terminal_error: Option<String>,
    pub payload_relative_dir: String,
    pub claim_token: Option<String>,
}

/// Mutable completion data written only by the current outbox claimant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudOutboxUpdate {
    pub state: CloudOutboxState,
    pub remote_revision_id: Option<String>,
    pub upload_id: Option<String>,
    pub terminal_error: Option<String>,
}

/// Immutable chunk metadata with monotonic acceptance acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudOutboxChunk {
    pub chunk_index: u32,
    pub size_bytes: u64,
    pub sha256: String,
    pub accepted: bool,
}

/// The last known remote head for one object, independent of local session deletion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudHead {
    pub object_id: String,
    pub source_session_id: Option<MeetingSessionId>,
    pub remote_revision_id: Option<String>,
    pub tombstone: bool,
    pub acknowledged_revision_id: Option<String>,
    pub change_sequence: u64,
}

/// A locally cached remote bundle that blocks automatic conflict resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudConflict {
    pub object_id: String,
    pub source_session_id: Option<MeetingSessionId>,
    pub source_revision: Option<u64>,
    pub remote_revision_id: String,
    pub remote_sequence: u64,
    pub remote_bundle_relative_path: String,
}

/// The lifecycle state of opaque share-root material retained only in SQLCipher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloudShareState {
    Pending,
    Active,
    Revoked,
    Failed,
}

/// The distinct portable share content contract, never inferred from an outbox payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloudShareContentKind {
    CapabilityBundle,
    BrowserMarkdown,
}

/// Input for a share whose secret link material must never leave encrypted local storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudShareInput {
    pub share_id: String,
    pub object_id: String,
    pub source_session_id: Option<MeetingSessionId>,
    pub expires_at_utc_ms: i64,
    pub content_kind: CloudShareContentKind,
    pub encrypted_link_material: String,
    pub outbox_id: Option<String>,
}

/// Durable share state and opaque encrypted link material with no derived URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudShareRecord {
    pub share_id: String,
    pub object_id: String,
    pub source_session_id: Option<MeetingSessionId>,
    pub expires_at_utc_ms: i64,
    pub state: CloudShareState,
    pub content_kind: CloudShareContentKind,
    pub encrypted_link_material: String,
    pub outbox_id: Option<String>,
    pub revoked_at_utc_ms: Option<i64>,
}

/// Mutable share lifecycle fields that do not expose the secret link material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloudShareUpdate {
    pub expires_at_utc_ms: i64,
    pub state: CloudShareState,
    pub outbox_id: Option<String>,
    pub revoked_at_utc_ms: Option<i64>,
}

/// Aggregate local work counts used to render cloud status without inspecting payloads.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct CloudStatusCounts {
    pub queued_outbox: u32,
    pub claimed_outbox: u32,
    pub pending_tombstones: u32,
    pub conflicts: u32,
    pub active_shares: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DueRetentionSession {
    pub session_id: MeetingSessionId,
    pub revision: u64,
}

/// One meeting a previous launch left mid-flight, with the phase it was in
/// when that launch ended. The prior phase is the only discriminator left
/// between a meeting whose audio was still being written and one that had
/// already been stopped, and it decides what recovery may attempt on its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveredMeeting {
    pub session_id: MeetingSessionId,
    pub prior_phase: MeetingPhase,
}

/// What one startup reconciliation changed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InterruptedRecovery {
    /// Meetings moved out of a live phase by this pass.
    pub recovered: Vec<RecoveredMeeting>,
    /// Meetings already parked in recovery whose stale processing status this
    /// pass resolved. Their phase, revision, and history are untouched.
    pub status_resolved: Vec<MeetingSessionId>,
    /// Start gates a previous launch left open, deleted by this pass. Nothing
    /// was ever recorded against them.
    pub discarded: Vec<MeetingSessionId>,
}
#[derive(Clone, Copy)]
pub(crate) struct StoreMutation {
    pub operation_id: MeetingOperationId,
    /// Who asked for this. Almost always a person at this keyboard, and the
    /// receipt said exactly that unconditionally until `sona --loop-resolve`
    /// gave an outside program a way in.
    pub actor: OperationActor,
    pub requested_at_utc_ms: i64,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub command: MeetingCommandKind,
}

pub(crate) struct StoreTransition<'a> {
    pub operation_id: Option<MeetingOperationId>,
    pub actor: OperationActor,
    pub command: MeetingCommandKind,
    pub requested_at_utc_ms: i64,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub allowed_from: &'a [MeetingPhase],
    pub next_phase: MeetingPhase,
    pub event_kind: &'a str,
    pub reason_codes: Vec<MeetingReasonCode>,
}

pub(crate) struct TrackCreation<'a> {
    pub session_id: MeetingSessionId,
    pub plan_id: MeetingPlanId,
    pub source_kind: SourceKind,
    pub required: bool,
    pub requested: bool,
    pub descriptor_json: &'a str,
    pub report: SourceStartReport,
}

pub(crate) struct SegmentEdit {
    pub mutation: StoreMutation,
    pub segment_id: TranscriptSegmentId,
    pub replacement_text: String,
    pub removed: bool,
}

pub(crate) struct DurableTrackRecord {
    pub track_id: SourceTrackId,
    pub sequence: u64,
    pub source_epoch: SourceEpoch,
    pub start_offset_ns: u64,
    pub duration_ns: u64,
    pub format: AudioFormat,
    pub samples: Vec<f32>,
}

pub(crate) struct TranscriptRevisionInput<'a> {
    pub session_id: MeetingSessionId,
    pub engine_id: &'a str,
    pub model_version: Option<&'a str>,
    pub destination: &'a ProcessingDestination,
    pub source_set: &'a [SourceKind],
    pub language: &'a str,
}

pub(crate) struct TranscriptSegmentInput {
    pub track_id: SourceTrackId,
    pub source_kind: SourceKind,
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
    pub text: String,
    pub confidence_milli: Option<u16>,
    /// The speaker this utterance is already attributed to, for a transcript
    /// that arrived with names on it. `None` — every recognizer segment — takes
    /// the track's default speaker and waits for diarization to say more.
    pub speaker: Option<String>,
}

pub(crate) struct DiarizationAssignmentInput {
    pub segment_id: TranscriptSegmentId,
    pub speaker_id: SpeakerId,
    pub assignment: SpeakerAssignmentKind,
}

pub(crate) struct ArtifactRevisionInput<'a> {
    pub session_id: MeetingSessionId,
    pub transcript_revision_id: TranscriptRevisionId,
    pub input_revision: u64,
    pub template_id: &'a str,
    pub template_version: u32,
    pub generation_key: &'a str,
    pub state: MeetingArtifactState,
    pub content: Option<&'a GeneratedMeetingArtifacts>,
    pub generated_at_utc_ms: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct MeetingEvidence {
    pub citation: MeetingCitation,
    pub text: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ArtifactEvidence {
    pub transcript: Vec<MeetingEvidence>,
    pub manual_notes: Vec<MeetingEvidence>,
    /// The rough notes the user typed for this meeting. They steer the shape
    /// of the generated notes; they are never citable evidence.
    pub user_notes: String,
    pub template: MeetingNotesTemplate,
}

impl From<rusqlite::Error> for StoreError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Unavailable
    }
}

impl From<std::io::Error> for StoreError {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}

#[derive(Clone, Copy, Default)]
struct MeetingTrendValues {
    meetings: u64,
    verified_captured_duration_ns: u64,
    transcript_segments: u64,
    generated_action_items: u64,
}

impl MeetingTrendValues {
    fn add(&mut self, other: Self) -> Result<(), StoreError> {
        self.meetings = self
            .meetings
            .checked_add(other.meetings)
            .ok_or(StoreError::Corrupt)?;
        self.verified_captured_duration_ns = self
            .verified_captured_duration_ns
            .checked_add(other.verified_captured_duration_ns)
            .ok_or(StoreError::Corrupt)?;
        self.transcript_segments = self
            .transcript_segments
            .checked_add(other.transcript_segments)
            .ok_or(StoreError::Corrupt)?;
        self.generated_action_items = self
            .generated_action_items
            .checked_add(other.generated_action_items)
            .ok_or(StoreError::Corrupt)?;
        Ok(())
    }

    fn totals(self) -> MeetingTrendTotals {
        MeetingTrendTotals {
            meetings: self.meetings,
            verified_captured_duration_ms: self.verified_captured_duration_ns / 1_000_000,
            transcript_segments: self.transcript_segments,
            generated_action_items: self.generated_action_items,
        }
    }
}

fn meeting_trend_value(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StandingSeriesConsent {
    pub series_key: String,
    pub policy_version: u32,
    pub granted_at_utc_ms: i64,
    pub acknowledged_sources: Vec<SourceKind>,
}

/// Grants, or re-grants, the standing consent for one series.
///
/// Connection-scoped rather than a method so a caller that is already inside a
/// transaction — the series-preferences setter, which has to write its receipt
/// and this row together or neither — reaches the same statement the consent
/// panel does. A grant with no acknowledged source is rejected: permission to
/// record "something" is not permission.
pub(super) fn grant_series_consent_in(
    connection: &Connection,
    series_key: &str,
    policy_version: u32,
    acknowledged_sources: &[SourceKind],
    granted_at_utc_ms: i64,
) -> Result<StandingSeriesConsent, StoreError> {
    let series_key = series_key.trim();
    if series_key.is_empty() || acknowledged_sources.is_empty() {
        return Err(StoreError::Invalid);
    }
    let acknowledged_sources_json = encode_json(&acknowledged_sources)?;
    connection.execute(
        "INSERT INTO meeting_series_consents (
            series_key, policy_version, granted_at_utc_ms,
            acknowledged_sources_json, revoked_at_utc_ms
         ) VALUES (?1, ?2, ?3, ?4, NULL)
         ON CONFLICT(series_key) DO UPDATE SET
            policy_version = excluded.policy_version,
            granted_at_utc_ms = excluded.granted_at_utc_ms,
            acknowledged_sources_json = excluded.acknowledged_sources_json,
            revoked_at_utc_ms = NULL",
        params![
            series_key,
            i64::from(policy_version),
            granted_at_utc_ms,
            acknowledged_sources_json
        ],
    )?;
    Ok(StandingSeriesConsent {
        series_key: series_key.to_string(),
        policy_version,
        granted_at_utc_ms,
        acknowledged_sources: acknowledged_sources.to_vec(),
    })
}

pub(super) fn live_series_consent_in(
    connection: &Connection,
    series_key: &str,
) -> Result<Option<StandingSeriesConsent>, StoreError> {
    let row = connection
        .query_row(
            "SELECT policy_version, granted_at_utc_ms, acknowledged_sources_json
               FROM meeting_series_consents
              WHERE series_key = ?1 AND revoked_at_utc_ms IS NULL",
            [series_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    row.map(|(policy_version, granted_at_utc_ms, sources)| {
        Ok(StandingSeriesConsent {
            series_key: series_key.to_string(),
            policy_version: u32::try_from(policy_version).map_err(|_| StoreError::Corrupt)?,
            granted_at_utc_ms,
            acknowledged_sources: decode_json(&sources)?,
        })
    })
    .transpose()
}

pub(super) fn revoke_series_consent_in(
    connection: &Connection,
    series_key: &str,
    revoked_at_utc_ms: i64,
) -> Result<bool, StoreError> {
    Ok(connection.execute(
        "UPDATE meeting_series_consents
            SET revoked_at_utc_ms = ?2
          WHERE series_key = ?1 AND revoked_at_utc_ms IS NULL",
        params![series_key, revoked_at_utc_ms],
    )? != 0)
}

pub struct MeetingStore {
    root: PathBuf,
    database_path: PathBuf,
    connection: Mutex<Connection>,
    master_key: Arc<MeetingStorageKey>,
}

impl MeetingStore {
    pub(crate) fn open(
        root: PathBuf,
        master_key: MeetingStorageKey,
    ) -> Result<Arc<Self>, StoreError> {
        ensure_private_directory(&root)?;
        let database_path = root.join("meeting-store.db");
        let mut connection = open_encrypted_connection(&database_path, &master_key)?;
        let migrations = Migrations::new(MIGRATIONS.to_vec());
        #[cfg(debug_assertions)]
        migrations.validate().map_err(|_| StoreError::Corrupt)?;
        migrations
            .to_latest(&mut connection)
            .map_err(|_| StoreError::Unavailable)?;
        configure_connection(&connection)?;
        set_private_file_permissions(&database_path)?;
        Ok(Arc::new(Self {
            root,
            database_path,
            connection: Mutex::new(connection),
            master_key: Arc::new(master_key),
        }))
    }
    pub(crate) fn grant_series_consent(
        &self,
        series_key: &str,
        policy_version: u32,
        acknowledged_sources: &[SourceKind],
        granted_at_utc_ms: i64,
    ) -> Result<StandingSeriesConsent, StoreError> {
        let connection = self.connection()?;
        grant_series_consent_in(
            &connection,
            series_key,
            policy_version,
            acknowledged_sources,
            granted_at_utc_ms,
        )
    }

    pub(crate) fn live_series_consent(
        &self,
        series_key: &str,
    ) -> Result<Option<StandingSeriesConsent>, StoreError> {
        let connection = self.connection()?;
        live_series_consent_in(&connection, series_key)
    }

    pub(crate) fn revoke_series_consent(
        &self,
        series_key: &str,
        revoked_at_utc_ms: i64,
    ) -> Result<bool, StoreError> {
        let connection = self.connection()?;
        revoke_series_consent_in(&connection, series_key, revoked_at_utc_ms)
    }

    pub(crate) fn consent_panel_introduction_needed(&self) -> Result<bool, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT first_prompt_shown_at_utc_ms IS NULL
                   FROM meeting_consent_panel_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub(crate) fn mark_consent_panel_introduction_shown(
        &self,
        shown_at_utc_ms: i64,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE meeting_consent_panel_state
                SET first_prompt_shown_at_utc_ms = COALESCE(first_prompt_shown_at_utc_ms, ?1)
              WHERE singleton = 1",
            [shown_at_utc_ms],
        )?;
        Ok(())
    }

    pub(crate) fn latest_consent_for_session(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Option<MeetingConsent>, StoreError> {
        let connection = self.connection()?;
        let acknowledgement: Option<String> = connection
            .query_row(
                "SELECT acknowledgement_json FROM meeting_consents
                  WHERE session_id = ?1
                  ORDER BY attempt_number DESC LIMIT 1",
                [id(session_id)],
                |row| row.get(0),
            )
            .optional()?;
        acknowledgement.map(|json| decode_json(&json)).transpose()
    }

    pub(crate) fn cloud_state(&self) -> Result<Option<CloudState>, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT vault_id, device_id, endpoint, cursor, snapshot_high_water,
                        clock_offset_ms, paused
                 FROM meeting_cloud_state WHERE singleton = 1",
                [],
                |row| {
                    Ok(CloudState {
                        vault_id: row.get(0)?,
                        device_id: row.get(1)?,
                        endpoint: row.get(2)?,
                        cursor: row.get(3)?,
                        snapshot_high_water: row.get(4)?,
                        clock_offset_ms: row.get(5)?,
                        paused: row.get::<_, i64>(6)? != 0,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn upsert_cloud_state(&self, state: &CloudState) -> Result<(), StoreError> {
        validate_cloud_state(state)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO meeting_cloud_state (
                singleton, vault_id, device_id, endpoint, cursor, snapshot_high_water,
                clock_offset_ms, paused, updated_at_utc_ms
             ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(singleton) DO UPDATE SET
                vault_id = excluded.vault_id,
                device_id = excluded.device_id,
                endpoint = excluded.endpoint,
                cursor = excluded.cursor,
                snapshot_high_water = excluded.snapshot_high_water,
                clock_offset_ms = excluded.clock_offset_ms,
                paused = excluded.paused,
                updated_at_utc_ms = excluded.updated_at_utc_ms",
            params![
                state.vault_id,
                state.device_id,
                state.endpoint,
                state.cursor,
                state.snapshot_high_water,
                state.clock_offset_ms,
                bool_to_i64(state.paused),
                utc_now_ms(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn update_cloud_cursor(
        &self,
        cursor: Option<String>,
        snapshot_high_water: Option<String>,
    ) -> Result<(), StoreError> {
        validate_optional_cloud_text(&cursor)?;
        validate_optional_cloud_text(&snapshot_high_water)?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_cloud_state
             SET cursor = ?1, snapshot_high_water = ?2, updated_at_utc_ms = ?3
             WHERE singleton = 1",
            params![cursor, snapshot_high_water, utc_now_ms()],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub(crate) fn set_cloud_clock_offset(&self, clock_offset_ms: i64) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_cloud_state
             SET clock_offset_ms = ?1, updated_at_utc_ms = ?2 WHERE singleton = 1",
            params![clock_offset_ms, utc_now_ms()],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub(crate) fn set_cloud_paused(&self, paused: bool) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_cloud_state
             SET paused = ?1, updated_at_utc_ms = ?2 WHERE singleton = 1",
            params![bool_to_i64(paused), utc_now_ms()],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub(crate) fn cloud_capabilities(
        &self,
        endpoint: &str,
    ) -> Result<Option<CloudCapabilitiesCache>, StoreError> {
        validate_cloud_text(endpoint)?;
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT endpoint, capabilities_json, fetched_at_utc_ms
                 FROM meeting_cloud_capabilities WHERE endpoint = ?1",
                params![endpoint],
                |row| {
                    Ok(CloudCapabilitiesCache {
                        endpoint: row.get(0)?,
                        capabilities_json: row.get(1)?,
                        fetched_at_utc_ms: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn upsert_cloud_capabilities(
        &self,
        cache: &CloudCapabilitiesCache,
    ) -> Result<(), StoreError> {
        validate_cloud_text(&cache.endpoint)?;
        validate_cloud_text(&cache.capabilities_json)?;
        serde_json::from_str::<serde_json::Value>(&cache.capabilities_json)
            .map_err(|_| StoreError::Invalid)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO meeting_cloud_capabilities (endpoint, capabilities_json, fetched_at_utc_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(endpoint) DO UPDATE SET
                capabilities_json = excluded.capabilities_json,
                fetched_at_utc_ms = excluded.fetched_at_utc_ms",
            params![cache.endpoint, cache.capabilities_json, cache.fetched_at_utc_ms],
        )?;
        Ok(())
    }

    pub(crate) fn cloud_head(&self, object_id: &str) -> Result<Option<CloudHead>, StoreError> {
        validate_cloud_identifier(object_id)?;
        let connection = self.connection()?;
        cloud_head_in(&connection, object_id)
    }
    pub(crate) fn cloud_head_for_session(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Option<CloudHead>, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT object_id, source_session_id, remote_revision_id, tombstone,
                        acknowledged_revision_id, change_sequence
                 FROM meeting_cloud_heads
                 WHERE source_session_id = ?1
                 ORDER BY updated_at_utc_ms DESC, object_id DESC LIMIT 1",
                params![id(session_id)],
                cloud_head_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn upsert_cloud_head(&self, head: &CloudHead) -> Result<(), StoreError> {
        validate_cloud_head(head)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "INSERT INTO meeting_cloud_heads (
                object_id, source_session_id, remote_revision_id, tombstone,
                acknowledged_revision_id, change_sequence, updated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(object_id) DO UPDATE SET
                source_session_id = excluded.source_session_id,
                remote_revision_id = excluded.remote_revision_id,
                tombstone = excluded.tombstone,
                acknowledged_revision_id = excluded.acknowledged_revision_id,
                change_sequence = excluded.change_sequence,
                updated_at_utc_ms = excluded.updated_at_utc_ms
             WHERE excluded.change_sequence >= meeting_cloud_heads.change_sequence",
            params![
                head.object_id,
                head.source_session_id.map(id),
                head.remote_revision_id,
                bool_to_i64(head.tombstone),
                head.acknowledged_revision_id,
                to_i64(head.change_sequence)?,
                utc_now_ms(),
            ],
        )?;
        if changed == 0 {
            transaction.commit()?;
            return Err(StoreError::Conflict);
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn enqueue_cloud_outbox(
        &self,
        input: CloudOutboxInput,
    ) -> Result<CloudOutboxRecord, StoreError> {
        validate_cloud_outbox_input(&input)?;
        {
            let connection = self.connection()?;
            if let Some(existing) =
                cloud_outbox_by_idempotency_in(&connection, &input.idempotency_key)?
            {
                return Ok(existing);
            }
        }
        let outbox_id = Uuid::new_v4().to_string();
        let payload_relative_dir = cloud_outbox_relative_dir(&outbox_id);
        let payload_directory =
            validated_cloud_outbox_dir(&self.root, &outbox_id, &payload_relative_dir)?;
        ensure_private_directory(&payload_directory)?;
        let now = utc_now_ms();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        validate_cloud_outbox_source(&transaction, &input)?;
        transaction.execute(
            "INSERT INTO meeting_cloud_outbox (
                outbox_id, kind, object_id, source_session_id, source_revision,
                base_remote_revision_id, share_content_kind, remote_revision_id, upload_id,
                idempotency_key, state, attempt_count, next_attempt_utc_ms, terminal_error,
                payload_relative_dir, claim_token, claimed_at_utc_ms, created_at_utc_ms,
                updated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, 'pending', 0, ?10,
                       NULL, ?11, NULL, NULL, ?12, ?12)",
            params![
                outbox_id,
                cloud_outbox_kind_to_db(input.kind),
                input.object_id,
                input.source_session_id.map(id),
                input.source_revision.map(to_i64).transpose()?,
                input.base_remote_revision_id,
                input.share_content_kind.map(cloud_share_content_kind_to_db),
                input.remote_revision_id,
                input.idempotency_key,
                input.next_attempt_utc_ms,
                payload_relative_dir,
                now,
            ],
        )?;
        transaction.commit()?;
        cloud_outbox_in(&connection, &outbox_id)?.ok_or(StoreError::Corrupt)
    }

    pub(crate) fn cloud_outbox(
        &self,
        outbox_id: &str,
    ) -> Result<Option<CloudOutboxRecord>, StoreError> {
        validate_cloud_local_id(outbox_id)?;
        let connection = self.connection()?;
        cloud_outbox_in(&connection, outbox_id)
    }
    pub(crate) fn cloud_outboxes_for_session(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<CloudOutboxRecord>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT outbox_id, kind, object_id, source_session_id, source_revision,
                    base_remote_revision_id, share_content_kind, remote_revision_id, upload_id,
                    idempotency_key, state, attempt_count, next_attempt_utc_ms, terminal_error,
                    payload_relative_dir, claim_token
             FROM meeting_cloud_outbox
             WHERE source_session_id = ?1
             ORDER BY created_at_utc_ms, outbox_id",
        )?;
        let rows = statement.query_map(params![id(session_id)], cloud_outbox_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn cloud_latest_terminal_error(&self) -> Result<Option<String>, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT terminal_error FROM meeting_cloud_outbox
                 WHERE state = 'terminal'
                 ORDER BY updated_at_utc_ms DESC, outbox_id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
    pub(crate) fn cloud_outbox_chunks(
        &self,
        outbox_id: &str,
    ) -> Result<Vec<CloudOutboxChunk>, StoreError> {
        validate_cloud_local_id(outbox_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT chunk_index, size_bytes, sha256, accepted
             FROM meeting_cloud_outbox_chunks
             WHERE outbox_id = ?1 ORDER BY chunk_index",
        )?;
        let rows = statement.query_map(params![outbox_id], |row| {
            Ok(CloudOutboxChunk {
                chunk_index: u32::try_from(row.get::<_, i64>(0)?)
                    .map_err(|_| to_sql_error(StoreError::Corrupt))?,
                size_bytes: from_i64(row.get(1)?).map_err(to_sql_error)?,
                sha256: row.get(2)?,
                accepted: row.get::<_, i64>(3)? != 0,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn due_cloud_outbox(
        &self,
        now_utc_ms: i64,
        limit: usize,
    ) -> Result<Vec<CloudOutboxRecord>, StoreError> {
        let limit = i64::try_from(limit.clamp(1, 100)).map_err(|_| StoreError::Invalid)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT outbox_id, kind, object_id, source_session_id, source_revision,
                    base_remote_revision_id, share_content_kind, remote_revision_id, upload_id,
                    idempotency_key, state, attempt_count, next_attempt_utc_ms, terminal_error,
                    payload_relative_dir, claim_token
             FROM meeting_cloud_outbox
             WHERE state = 'pending' AND next_attempt_utc_ms <= ?1
             ORDER BY next_attempt_utc_ms, created_at_utc_ms, outbox_id LIMIT ?2",
        )?;
        let rows = statement.query_map(params![now_utc_ms, limit], cloud_outbox_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn claim_cloud_outbox(
        &self,
        outbox_id: &str,
        claim_token: &str,
        now_utc_ms: i64,
    ) -> Result<Option<CloudOutboxRecord>, StoreError> {
        validate_cloud_local_id(outbox_id)?;
        validate_cloud_text(claim_token)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let existing = cloud_outbox_in(&transaction, outbox_id)?;
        let Some(existing) = existing else {
            transaction.commit()?;
            return Ok(None);
        };
        if existing.state == CloudOutboxState::Claimed
            && existing.claim_token.as_deref() == Some(claim_token)
        {
            transaction.commit()?;
            return Ok(Some(existing));
        }
        let changed = transaction.execute(
            "UPDATE meeting_cloud_outbox
             SET state = 'claimed', claim_token = ?1, claimed_at_utc_ms = ?2,
                 updated_at_utc_ms = ?2
             WHERE outbox_id = ?3 AND state = 'pending' AND next_attempt_utc_ms <= ?2",
            params![claim_token, now_utc_ms, outbox_id],
        )?;
        if changed != 1 {
            transaction.commit()?;
            return Ok(None);
        }
        let claimed = cloud_outbox_in(&transaction, outbox_id)?.ok_or(StoreError::Corrupt)?;
        transaction.commit()?;
        Ok(Some(claimed))
    }
    pub(crate) fn recover_claimed_cloud_outbox(
        &self,
        now_utc_ms: i64,
    ) -> Result<usize, StoreError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_cloud_outbox
             SET state = 'pending', claim_token = NULL, claimed_at_utc_ms = NULL,
                 next_attempt_utc_ms = MIN(next_attempt_utc_ms, ?1), updated_at_utc_ms = ?1
             WHERE state = 'claimed'",
            params![now_utc_ms],
        )?;
        Ok(changed)
    }

    pub(crate) fn release_cloud_outbox_claim(
        &self,
        outbox_id: &str,
        claim_token: &str,
        next_attempt_utc_ms: i64,
    ) -> Result<CloudOutboxRecord, StoreError> {
        validate_cloud_local_id(outbox_id)?;
        validate_cloud_text(claim_token)?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_cloud_outbox
             SET state = 'pending', claim_token = NULL, claimed_at_utc_ms = NULL,
                 next_attempt_utc_ms = ?1, updated_at_utc_ms = ?2
             WHERE outbox_id = ?3 AND state = 'claimed' AND claim_token = ?4",
            params![next_attempt_utc_ms, utc_now_ms(), outbox_id, claim_token],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict);
        }
        cloud_outbox_in(&connection, outbox_id)?.ok_or(StoreError::Corrupt)
    }

    pub(crate) fn update_cloud_outbox(
        &self,
        outbox_id: &str,
        claim_token: &str,
        update: CloudOutboxUpdate,
    ) -> Result<CloudOutboxRecord, StoreError> {
        validate_cloud_local_id(outbox_id)?;
        validate_cloud_text(claim_token)?;
        validate_cloud_outbox_update(&update)?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_cloud_outbox
             SET state = ?1, remote_revision_id = ?2, upload_id = ?3, terminal_error = ?4,
                 claim_token = NULL, claimed_at_utc_ms = NULL, updated_at_utc_ms = ?5
             WHERE outbox_id = ?6 AND state = 'claimed' AND claim_token = ?7",
            params![
                cloud_outbox_state_to_db(update.state),
                update.remote_revision_id,
                update.upload_id,
                update.terminal_error,
                utc_now_ms(),
                outbox_id,
                claim_token,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict);
        }
        cloud_outbox_in(&connection, outbox_id)?.ok_or(StoreError::Corrupt)
    }

    pub(crate) fn set_cloud_outbox_upload_id(
        &self,
        outbox_id: &str,
        claim_token: &str,
        upload_id: String,
    ) -> Result<CloudOutboxRecord, StoreError> {
        validate_cloud_local_id(outbox_id)?;
        validate_cloud_text(claim_token)?;
        validate_cloud_identifier(&upload_id)?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_cloud_outbox
             SET upload_id = ?1, updated_at_utc_ms = ?2
             WHERE outbox_id = ?3 AND state = 'claimed' AND claim_token = ?4
               AND (upload_id IS NULL OR upload_id = ?1)",
            params![upload_id, utc_now_ms(), outbox_id, claim_token],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict);
        }
        cloud_outbox_in(&connection, outbox_id)?.ok_or(StoreError::Corrupt)
    }

    pub(crate) fn retry_cloud_outbox(
        &self,
        outbox_id: &str,
        claim_token: &str,
        failure: &str,
        next_attempt_utc_ms: i64,
    ) -> Result<CloudOutboxRecord, StoreError> {
        validate_cloud_local_id(outbox_id)?;
        validate_cloud_text(claim_token)?;
        validate_cloud_text(failure)?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_cloud_outbox
             SET state = 'pending', attempt_count = attempt_count + 1,
                 next_attempt_utc_ms = ?1, terminal_error = NULL, claim_token = NULL,
                 claimed_at_utc_ms = NULL, updated_at_utc_ms = ?2
             WHERE outbox_id = ?3 AND state = 'claimed' AND claim_token = ?4",
            params![next_attempt_utc_ms, utc_now_ms(), outbox_id, claim_token],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict);
        }
        cloud_outbox_in(&connection, outbox_id)?.ok_or(StoreError::Corrupt)
    }
    pub(crate) fn retry_terminal_cloud_outbox(
        &self,
        outbox_id: &str,
        next_attempt_utc_ms: i64,
    ) -> Result<CloudOutboxRecord, StoreError> {
        validate_cloud_local_id(outbox_id)?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_cloud_outbox
             SET state = 'pending', attempt_count = attempt_count + 1,
                 next_attempt_utc_ms = ?1, terminal_error = NULL,
                 claim_token = NULL, claimed_at_utc_ms = NULL, updated_at_utc_ms = ?2
             WHERE outbox_id = ?3 AND state = 'terminal'",
            params![next_attempt_utc_ms, utc_now_ms(), outbox_id],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict);
        }
        cloud_outbox_in(&connection, outbox_id)?.ok_or(StoreError::Corrupt)
    }

    pub(crate) fn cancel_cloud_outbox(&self, outbox_id: &str) -> Result<(), StoreError> {
        validate_cloud_local_id(outbox_id)?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_cloud_outbox
             SET state = 'cancelled', claim_token = NULL, claimed_at_utc_ms = NULL,
                 updated_at_utc_ms = ?1
             WHERE outbox_id = ?2 AND state IN ('pending', 'claimed')",
            params![utc_now_ms(), outbox_id],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict);
        }
        Ok(())
    }

    pub(crate) fn cloud_outbox_payload_directory(
        &self,
        outbox_id: &str,
    ) -> Result<PathBuf, StoreError> {
        let record = self.cloud_outbox(outbox_id)?.ok_or(StoreError::NotFound)?;
        let path = validated_cloud_outbox_dir(&self.root, outbox_id, &record.payload_relative_dir)?;
        ensure_private_directory(&path)?;
        Ok(path)
    }

    pub(crate) fn stage_cloud_outbox_chunks(
        &self,
        outbox_id: &str,
        chunks: &[CloudOutboxChunk],
    ) -> Result<(), StoreError> {
        validate_cloud_local_id(outbox_id)?;
        validate_cloud_chunks(chunks)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let record = cloud_outbox_in(&transaction, outbox_id)?.ok_or(StoreError::NotFound)?;
        if record.state != CloudOutboxState::Pending {
            return Err(StoreError::Conflict);
        }
        for chunk in chunks {
            transaction.execute(
                "INSERT INTO meeting_cloud_outbox_chunks (
                    outbox_id, chunk_index, size_bytes, sha256, accepted, accepted_at_utc_ms
                 ) VALUES (?1, ?2, ?3, ?4, 0, NULL)",
                params![
                    outbox_id,
                    i64::from(chunk.chunk_index),
                    to_i64(chunk.size_bytes)?,
                    chunk.sha256,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn missing_cloud_outbox_chunks(
        &self,
        outbox_id: &str,
    ) -> Result<Vec<CloudOutboxChunk>, StoreError> {
        validate_cloud_local_id(outbox_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT chunk_index, size_bytes, sha256, accepted
             FROM meeting_cloud_outbox_chunks
             WHERE outbox_id = ?1 AND accepted = 0 ORDER BY chunk_index",
        )?;
        let rows = statement.query_map(params![outbox_id], |row| {
            Ok(CloudOutboxChunk {
                chunk_index: u32::try_from(row.get::<_, i64>(0)?)
                    .map_err(|_| to_sql_error(StoreError::Corrupt))?,
                size_bytes: from_i64(row.get(1)?).map_err(to_sql_error)?,
                sha256: row.get(2)?,
                accepted: row.get::<_, i64>(3)? != 0,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn mark_cloud_outbox_chunk_accepted(
        &self,
        outbox_id: &str,
        chunk_index: u32,
        claim_token: &str,
        accepted_at_utc_ms: i64,
    ) -> Result<(), StoreError> {
        validate_cloud_local_id(outbox_id)?;
        validate_cloud_text(claim_token)?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_cloud_outbox_chunks
             SET accepted = 1, accepted_at_utc_ms = ?1
             WHERE outbox_id = ?2 AND chunk_index = ?3
               AND EXISTS (
                    SELECT 1 FROM meeting_cloud_outbox
                    WHERE outbox_id = ?2 AND state = 'claimed' AND claim_token = ?4
               )",
            params![
                accepted_at_utc_ms,
                outbox_id,
                i64::from(chunk_index),
                claim_token
            ],
        )?;
        if changed == 1 {
            return Ok(());
        }
        let accepted: Option<i64> = connection
            .query_row(
                "SELECT accepted FROM meeting_cloud_outbox_chunks
                 WHERE outbox_id = ?1 AND chunk_index = ?2",
                params![outbox_id, i64::from(chunk_index)],
                |row| row.get(0),
            )
            .optional()?;
        match accepted {
            Some(1) => Ok(()),
            Some(_) => Err(StoreError::Conflict),
            None => Err(StoreError::NotFound),
        }
    }

    pub(crate) fn enqueue_cloud_tombstone_for_session(
        &self,
        session_id: MeetingSessionId,
        revision: u64,
        idempotency_key: String,
        next_attempt_utc_ms: i64,
    ) -> Result<Option<CloudOutboxRecord>, StoreError> {
        let connection = self.connection()?;
        let head = connection
            .query_row(
                "SELECT object_id, source_session_id, remote_revision_id, tombstone,
                        acknowledged_revision_id, change_sequence
                 FROM meeting_cloud_heads
                 WHERE source_session_id = ?1 AND tombstone = 0
                 ORDER BY updated_at_utc_ms DESC LIMIT 1",
                params![id(session_id)],
                cloud_head_from_row,
            )
            .optional()?;
        drop(connection);
        let Some(head) = head else {
            return Ok(None);
        };
        self.enqueue_cloud_outbox(CloudOutboxInput {
            kind: CloudOutboxKind::Tombstone,
            object_id: head.object_id,
            source_session_id: Some(session_id),
            source_revision: Some(revision),
            base_remote_revision_id: head.remote_revision_id,
            share_content_kind: None,
            remote_revision_id: Some(Uuid::new_v4().simple().to_string()),
            idempotency_key,
            next_attempt_utc_ms,
        })
        .map(Some)
    }

    pub(crate) fn cloud_conflict(
        &self,
        object_id: &str,
    ) -> Result<Option<CloudConflict>, StoreError> {
        validate_cloud_identifier(object_id)?;
        let connection = self.connection()?;
        cloud_conflict_in(&connection, object_id)
    }

    pub(crate) fn cache_cloud_conflict(&self, conflict: &CloudConflict) -> Result<(), StoreError> {
        validate_cloud_conflict(conflict)?;
        let bundle_path = validated_cloud_conflict_bundle_path(
            &self.root,
            &conflict.remote_bundle_relative_path,
        )?;
        if !bundle_path.is_file() {
            return Err(StoreError::NotFound);
        }
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO meeting_cloud_conflicts (
                object_id, source_session_id, source_revision, remote_revision_id,
                remote_sequence, remote_bundle_relative_path, detected_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(object_id) DO UPDATE SET
                source_session_id = excluded.source_session_id,
                source_revision = excluded.source_revision,
                remote_revision_id = excluded.remote_revision_id,
                remote_sequence = excluded.remote_sequence,
                remote_bundle_relative_path = excluded.remote_bundle_relative_path,
                detected_at_utc_ms = excluded.detected_at_utc_ms",
            params![
                conflict.object_id,
                conflict.source_session_id.map(id),
                conflict.source_revision.map(to_i64).transpose()?,
                conflict.remote_revision_id,
                to_i64(conflict.remote_sequence)?,
                conflict.remote_bundle_relative_path,
                utc_now_ms(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn cloud_conflict_bundle_path(
        &self,
        object_id: &str,
    ) -> Result<PathBuf, StoreError> {
        let conflict = self
            .cloud_conflict(object_id)?
            .ok_or(StoreError::NotFound)?;
        let path = validated_cloud_conflict_bundle_path(
            &self.root,
            &conflict.remote_bundle_relative_path,
        )?;
        if !path.is_file() {
            return Err(StoreError::NotFound);
        }
        Ok(path)
    }

    pub(crate) fn cloud_conflict_staging_path(
        &self,
        object_id: &str,
    ) -> Result<PathBuf, StoreError> {
        validate_cloud_identifier(object_id)?;
        let relative = format!(".cloud-conflicts/{object_id}.bundle");
        let path = validated_cloud_conflict_bundle_path(&self.root, &relative)?;
        let parent = path.parent().ok_or(StoreError::Invalid)?;
        ensure_private_directory(parent)?;
        Ok(path)
    }

    /// Where a pulled device recording is decrypted before the importer decodes
    /// it. The bytes are plaintext audio, so they stage inside the store's own
    /// private root rather than a shared temporary directory; the caller owns
    /// the file and removes it.
    pub(crate) fn cloud_recording_staging_path(
        &self,
        object_id: &str,
    ) -> Result<PathBuf, StoreError> {
        validate_cloud_identifier(object_id)?;
        let path = validated_relative(&self.root, &format!(".cloud-inbox/{object_id}.wav"))?;
        let parent = path.parent().ok_or(StoreError::Invalid)?;
        ensure_private_directory(parent)?;
        Ok(path)
    }

    pub(crate) fn resolve_cloud_conflict_keep_local(
        &self,
        object_id: &str,
        idempotency_key: String,
        next_attempt_utc_ms: i64,
    ) -> Result<CloudOutboxRecord, StoreError> {
        validate_cloud_identifier(object_id)?;
        let conflict = self
            .cloud_conflict(object_id)?
            .ok_or(StoreError::NotFound)?;
        let session_id = conflict.source_session_id.ok_or(StoreError::Conflict)?;
        let session = self.session_snapshot(session_id)?;
        if !matches!(
            session.phase,
            MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired
        ) {
            return Err(StoreError::Conflict);
        }
        let outbox = self.enqueue_cloud_outbox(CloudOutboxInput {
            kind: CloudOutboxKind::Object,
            object_id: conflict.object_id.clone(),
            source_session_id: Some(session_id),
            source_revision: Some(session.revision),
            base_remote_revision_id: Some(conflict.remote_revision_id.clone()),
            share_content_kind: None,
            remote_revision_id: Some(Uuid::new_v4().simple().to_string()),
            idempotency_key,
            next_attempt_utc_ms,
        })?;
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM meeting_cloud_conflicts
             WHERE object_id = ?1 AND remote_revision_id = ?2",
            params![conflict.object_id, conflict.remote_revision_id],
        )?;
        Ok(outbox)
    }

    pub(crate) fn resolve_cloud_conflict_use_remote(
        &self,
        object_id: &str,
        installed_session_id: MeetingSessionId,
    ) -> Result<CloudHead, StoreError> {
        validate_cloud_identifier(object_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let conflict = cloud_conflict_in(&transaction, object_id)?.ok_or(StoreError::NotFound)?;
        let session = session_row(&transaction, installed_session_id)?;
        if !matches!(
            session.phase,
            MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired
        ) {
            return Err(StoreError::Conflict);
        }
        let head = CloudHead {
            object_id: conflict.object_id.clone(),
            source_session_id: Some(installed_session_id),
            remote_revision_id: Some(conflict.remote_revision_id.clone()),
            tombstone: false,
            acknowledged_revision_id: Some(conflict.remote_revision_id.clone()),
            change_sequence: conflict.remote_sequence,
        };
        upsert_cloud_head_in(&transaction, &head)?;
        let changed = transaction.execute(
            "DELETE FROM meeting_cloud_conflicts
             WHERE object_id = ?1 AND remote_revision_id = ?2",
            params![conflict.object_id, conflict.remote_revision_id],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict);
        }
        transaction.commit()?;
        Ok(head)
    }

    pub(crate) fn create_cloud_share(
        &self,
        input: CloudShareInput,
    ) -> Result<CloudShareRecord, StoreError> {
        validate_cloud_share_input(&input)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        validate_cloud_share_source(&transaction, input.source_session_id)?;
        let now = utc_now_ms();
        transaction.execute(
            "INSERT INTO meeting_cloud_shares (
                share_id, object_id, source_session_id, expires_at_utc_ms, state, content_kind,
                encrypted_link_material, outbox_id, revoked_at_utc_ms, created_at_utc_ms,
                updated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7, NULL, ?8, ?8)",
            params![
                input.share_id,
                input.object_id,
                input.source_session_id.map(id),
                input.expires_at_utc_ms,
                cloud_share_content_kind_to_db(input.content_kind),
                input.encrypted_link_material,
                input.outbox_id,
                now,
            ],
        )?;
        transaction.commit()?;
        cloud_share_in(&connection, &input.share_id)?.ok_or(StoreError::Corrupt)
    }

    pub(crate) fn cloud_share(
        &self,
        share_id: &str,
    ) -> Result<Option<CloudShareRecord>, StoreError> {
        validate_cloud_identifier(share_id)?;
        let connection = self.connection()?;
        cloud_share_in(&connection, share_id)
    }
    pub(crate) fn cloud_share_for_outbox(
        &self,
        outbox_id: &str,
    ) -> Result<Option<CloudShareRecord>, StoreError> {
        validate_cloud_local_id(outbox_id)?;
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT share_id, object_id, source_session_id, expires_at_utc_ms, state,
                        content_kind, encrypted_link_material, outbox_id, revoked_at_utc_ms
                 FROM meeting_cloud_shares WHERE outbox_id = ?1",
                params![outbox_id],
                cloud_share_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn cloud_share_count_for_session(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<u32, StoreError> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM meeting_cloud_shares
             WHERE source_session_id = ?1 AND state IN ('pending', 'active')",
            params![id(session_id)],
            |row| row.get(0),
        )?;
        u32::try_from(count).map_err(|_| StoreError::Corrupt)
    }

    pub(crate) fn update_cloud_share(
        &self,
        share_id: &str,
        update: CloudShareUpdate,
    ) -> Result<CloudShareRecord, StoreError> {
        validate_cloud_identifier(share_id)?;
        validate_cloud_share_update(&update)?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_cloud_shares
             SET expires_at_utc_ms = ?1, state = ?2, outbox_id = ?3, revoked_at_utc_ms = ?4,
                 updated_at_utc_ms = ?5
             WHERE share_id = ?6",
            params![
                update.expires_at_utc_ms,
                cloud_share_state_to_db(update.state),
                update.outbox_id,
                update.revoked_at_utc_ms,
                utc_now_ms(),
                share_id,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound);
        }
        cloud_share_in(&connection, share_id)?.ok_or(StoreError::Corrupt)
    }

    pub(crate) fn revoke_cloud_share(
        &self,
        share_id: &str,
        revoked_at_utc_ms: i64,
    ) -> Result<CloudShareRecord, StoreError> {
        let current = self.cloud_share(share_id)?.ok_or(StoreError::NotFound)?;
        self.update_cloud_share(
            share_id,
            CloudShareUpdate {
                expires_at_utc_ms: current.expires_at_utc_ms,
                state: CloudShareState::Revoked,
                outbox_id: current.outbox_id,
                revoked_at_utc_ms: Some(revoked_at_utc_ms),
            },
        )
    }

    pub(crate) fn cloud_status_counts(&self) -> Result<CloudStatusCounts, StoreError> {
        let connection = self.connection()?;
        let (queued_outbox, claimed_outbox, pending_tombstones, conflicts, active_shares): (
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = connection.query_row(
            "SELECT
                (SELECT COUNT(*) FROM meeting_cloud_outbox WHERE state = 'pending'),
                (SELECT COUNT(*) FROM meeting_cloud_outbox WHERE state = 'claimed'),
                (SELECT COUNT(*) FROM meeting_cloud_outbox
                    WHERE kind = 'tombstone' AND state IN ('pending', 'claimed')),
                (SELECT COUNT(*) FROM meeting_cloud_conflicts),
                (SELECT COUNT(*) FROM meeting_cloud_shares WHERE state = 'active')",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        Ok(CloudStatusCounts {
            queued_outbox: u32::try_from(queued_outbox).map_err(|_| StoreError::Corrupt)?,
            claimed_outbox: u32::try_from(claimed_outbox).map_err(|_| StoreError::Corrupt)?,
            pending_tombstones: u32::try_from(pending_tombstones)
                .map_err(|_| StoreError::Corrupt)?,
            conflicts: u32::try_from(conflicts).map_err(|_| StoreError::Corrupt)?,
            active_shares: u32::try_from(active_shares).map_err(|_| StoreError::Corrupt)?,
        })
    }

    pub(crate) fn export_cloud_meeting_bundle(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<cloud_bundle::CloudMeetingBundleV1, StoreError> {
        let connection = self.connection()?;
        let bundle = export_cloud_meeting_bundle_in(&connection, session_id)?;
        bundle.validate()?;
        Ok(bundle)
    }

    pub(crate) fn import_cloud_meeting_bundle(
        &self,
        bundle: &cloud_bundle::CloudMeetingBundleV1,
    ) -> Result<MeetingSessionId, StoreError> {
        bundle.validate()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        import_cloud_meeting_bundle_in(&transaction, bundle)?;
        rebuild_search_documents_in(&transaction, bundle.session.session_id)?;
        transaction.commit()?;
        Ok(bundle.session.session_id)
    }

    pub fn next_plan_attempt(&self, session_id: MeetingSessionId) -> Result<u32, StoreError> {
        let connection = self.connection()?;
        let attempt: i64 = connection.query_row(
            "SELECT COALESCE(MAX(attempt_number), 0) + 1 FROM meeting_run_plans WHERE session_id = ?1",
            params![id(session_id)],
            |row| row.get(0),
        )?;
        u32::try_from(attempt).map_err(|_| StoreError::Corrupt)
    }

    pub(crate) fn processing_plan(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingRunPlan, StoreError> {
        let connection = self.connection()?;
        let plan: String = connection
            .query_row(
                "SELECT p.canonical_plan_json
                 FROM meeting_sessions s
                 JOIN meeting_run_plans p ON p.plan_id = s.successful_plan_id
                 WHERE s.id = ?1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        decode_json(&plan)
    }

    pub fn set_processing_status(
        &self,
        session_id: MeetingSessionId,
        status: ProcessingStatus,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE meeting_sessions SET processing_status = ?1 WHERE id = ?2",
            params![encode_json(&status)?, id(session_id)],
        )?;
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn diarization_model_directory(&self) -> PathBuf {
        self.root.join("models")
    }

    pub fn set_diarization_status(
        &self,
        session_id: MeetingSessionId,
        status: DiarizationStatus,
        model_id: &str,
        model_version: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE meeting_sessions
             SET diarization_status = ?1, diarization_model_id = ?2, diarization_model_version = ?3
             WHERE id = ?4",
            params![
                diarization_status_to_db(status),
                model_id,
                model_version,
                id(session_id)
            ],
        )?;
        Ok(())
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub fn default_retention_policy(&self) -> Result<(MeetingRetentionPolicy, u64), StoreError> {
        let connection = self.connection()?;
        let (policy, revision): (String, i64) = connection.query_row(
            "SELECT policy_json, revision FROM meeting_retention_policy WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((
            decode_json(&policy)?,
            u64::try_from(revision).map_err(|_| StoreError::Corrupt)?,
        ))
    }
    pub fn session_retention_policy(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingRetentionPolicy, StoreError> {
        let connection = self.connection()?;
        let policy: String = connection
            .query_row(
                "SELECT retention_policy_json FROM meeting_sessions WHERE id = ?1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        decode_json(&policy)
    }
    pub(crate) fn due_retention_sessions(
        &self,
        now_utc_ms: i64,
    ) -> Result<Vec<DueRetentionSession>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, revision FROM meeting_sessions
             WHERE phase = 'review_ready'
               AND delete_after_utc_ms IS NOT NULL
               AND delete_after_utc_ms <= ?1
             ORDER BY delete_after_utc_ms, id",
        )?;
        let rows = statement
            .query_map(params![now_utc_ms], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(session_id, revision)| {
                Ok(DueRetentionSession {
                    session_id: MeetingSessionId::from_uuid(parse_uuid(&session_id)?),
                    revision: u64::try_from(revision).map_err(|_| StoreError::Corrupt)?,
                })
            })
            .collect()
    }

    pub fn set_default_retention_policy(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        expected_revision: u64,
        policy: &MeetingRetentionPolicy,
    ) -> Result<(OperationReceipt, u64), StoreError> {
        if let Some(receipt) = self.operation_receipt(operation_id)? {
            let revision = receipt.new_revision.ok_or(StoreError::Corrupt)?;
            return Ok((receipt, revision));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        if let Some(receipt) = operation_receipt_in(&transaction, operation_id)? {
            let revision = receipt.new_revision.ok_or(StoreError::Corrupt)?;
            transaction.commit()?;
            return Ok((receipt, revision));
        }
        let revision: i64 = transaction.query_row(
            "SELECT revision FROM meeting_retention_policy WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        let revision = u64::try_from(revision).map_err(|_| StoreError::Corrupt)?;
        if revision != expected_revision {
            let receipt = rejected_global_receipt(
                operation_id,
                MeetingCommandKind::RetentionSet,
                expected_revision,
                revision,
                requested_at_utc_ms,
                MeetingReasonCode::StaleRevision,
            );
            insert_operation_receipt(&transaction, &receipt, utc_now_ms())?;
            transaction.commit()?;
            return Ok((receipt, revision));
        }
        let next = revision.checked_add(1).ok_or(StoreError::Corrupt)?;
        transaction.execute(
            "UPDATE meeting_retention_policy SET policy_json = ?1, revision = ?2 WHERE singleton = 1",
            params![encode_json(policy)?, to_i64(next)?],
        )?;
        let now = utc_now_ms();
        let receipt = committed_global_receipt(
            operation_id,
            MeetingCommandKind::RetentionSet,
            expected_revision,
            requested_at_utc_ms,
            now,
            next,
        );
        insert_operation_receipt(&transaction, &receipt, now)?;
        transaction.commit()?;
        Ok((receipt, next))
    }

    pub fn operation_receipt(
        &self,
        operation_id: MeetingOperationId,
    ) -> Result<Option<OperationReceipt>, StoreError> {
        let connection = self.connection()?;
        let receipt = connection
            .query_row(
                "SELECT receipt_json FROM meeting_operation_receipts WHERE operation_id = ?1",
                params![id(operation_id)],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        receipt.map(|value| decode_json(&value)).transpose()
    }
    pub fn export_result_for_operation(
        &self,
        operation_id: MeetingOperationId,
    ) -> Result<Option<MeetingExportResult>, StoreError> {
        let connection = self.connection()?;
        let receipt = connection
            .query_row(
                "SELECT receipt_json FROM meeting_operation_receipts WHERE operation_id = ?1",
                params![id(operation_id)],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let receipt: Option<OperationReceipt> =
            receipt.map(|value| decode_json(&value)).transpose()?;
        let Some(receipt) = receipt else {
            return Ok(None);
        };
        if receipt.command != MeetingCommandKind::Export {
            return Err(StoreError::Invalid);
        }
        if receipt.result != OperationResult::Committed {
            return Ok(None);
        }
        Ok(Some(export_result_for_receipt(&connection, receipt)?))
    }

    pub fn record_export(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        session_id: MeetingSessionId,
        expected_revision: u64,
        format: MeetingExportFormat,
    ) -> Result<MeetingExportResult, StoreError> {
        if let Some(result) = self.export_result_for_operation(operation_id)? {
            return Ok(result);
        }
        if self.operation_receipt(operation_id)?.is_some() {
            return Err(StoreError::Invalid);
        }

        let mutation = StoreMutation {
            operation_id,
            actor: OperationActor::User,
            requested_at_utc_ms,
            session_id,
            expected_revision,
            command: MeetingCommandKind::Export,
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        if let Some(receipt) = operation_receipt_in(&transaction, operation_id)? {
            let result = export_result_for_receipt(&transaction, receipt)?;
            transaction.commit()?;
            return Ok(result);
        }
        let current = session_row(&transaction, session_id)?;
        if validate_mutation(
            &transaction,
            mutation,
            &current,
            &[MeetingPhase::ReviewReady, MeetingPhase::RecoveryRequired],
        )?
        .is_some()
        {
            transaction.commit()?;
            return Err(StoreError::Conflict);
        }

        let transcript_revision_id = transaction
            .query_row(
                "SELECT current_transcript_revision_id FROM meeting_sessions WHERE id = ?1",
                params![id(session_id)],
                |row| row.get::<_, Option<String>>(0),
            )?
            .map(|value| parse_uuid(&value).map(TranscriptRevisionId::from_uuid))
            .transpose()?;
        let sources = source_snapshots(&transaction, session_id)?;
        let capture_completeness = derive_completeness(&transaction, session_id, &sources)?;
        let now = utc_now_ms();
        let export_receipt = MeetingExportReceipt {
            export_receipt_id: MeetingExportReceiptId::new(),
            session_id,
            format,
            snapshot_revision: current.revision,
            capture_completeness,
            transcript_revision_id,
            created_at_utc_ms: now,
        };
        transaction.execute(
            "INSERT INTO meeting_export_receipts (
                export_receipt_id, session_id, format, snapshot_revision, capture_completeness,
                transcript_revision_id, created_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id(export_receipt.export_receipt_id),
                id(session_id),
                encode_json(&export_receipt.format)?,
                to_i64(export_receipt.snapshot_revision)?,
                encode_json(&export_receipt.capture_completeness)?,
                export_receipt.transcript_revision_id.map(id),
                export_receipt.created_at_utc_ms,
            ],
        )?;
        let receipt = committed_receipt(
            mutation,
            current.phase,
            current.phase,
            now,
            current.revision,
            vec![id(export_receipt.export_receipt_id)],
        );
        insert_operation_receipt(&transaction, &receipt, now)?;
        transaction.commit()?;
        Ok(MeetingExportResult {
            receipt,
            export_receipt,
        })
    }

    pub(crate) fn create_preflight(
        &self,
        mutation: StoreMutation,
        title: String,
        origin: MeetingOrigin,
        preflight: MeetingPreflightSnapshot,
        retention_policy: MeetingRetentionPolicy,
    ) -> Result<OperationReceipt, StoreError> {
        if let Some(receipt) = self.operation_receipt(mutation.operation_id)? {
            return Ok(receipt);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        if let Some(receipt) = operation_receipt_in(&transaction, mutation.operation_id)? {
            return Ok(receipt);
        }
        let now = utc_now_ms();
        transaction.execute(
            "INSERT INTO meeting_sessions (
                id, phase, revision, title, origin_kind, preflight_json, created_at_utc_ms,
                processing_status, retention_policy_json
             ) VALUES (?1, 'preflight', 0, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id(mutation.session_id),
                title,
                encode_json(&origin)?,
                encode_json(&preflight)?,
                now,
                encode_json(&ProcessingStatus::Pending)?,
                encode_json(&retention_policy)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO meeting_session_events (
                session_id, sequence, prior_phase, next_phase, event_kind, observed_at_utc_ms,
                session_offset_ns, details_json
            ) VALUES (?1, 0, NULL, 'preflight', 'preflight_created', ?2, NULL, '{}')",
            params![id(mutation.session_id), now],
        )?;
        let receipt = OperationReceipt {
            schema_version: STORE_SCHEMA_VERSION,
            operation_id: mutation.operation_id,
            session_id: Some(mutation.session_id),
            actor: OperationActor::User,
            command: mutation.command,
            expected_revision: mutation.expected_revision,
            from_phase: None,
            to_phase: Some(MeetingPhase::Preflight),
            requested_at_utc_ms: mutation.requested_at_utc_ms,
            committed_at_utc_ms: Some(now),
            result: OperationResult::Committed,
            reason_codes: Vec::new(),
            new_revision: Some(0),
            effect_ids: vec![id(mutation.session_id)],
        };
        insert_operation_receipt(&transaction, &receipt, now)?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn refresh_preflight(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        session_id: MeetingSessionId,
        expected_revision: u64,
        preflight: MeetingPreflightSnapshot,
    ) -> Result<OperationReceipt, StoreError> {
        self.update_preflight(
            operation_id,
            requested_at_utc_ms,
            session_id,
            expected_revision,
            preflight,
            MeetingCommandKind::PreflightRefresh,
        )
    }

    fn update_preflight(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        session_id: MeetingSessionId,
        expected_revision: u64,
        preflight: MeetingPreflightSnapshot,
        command: MeetingCommandKind,
    ) -> Result<OperationReceipt, StoreError> {
        if let Some(receipt) = self.operation_receipt(operation_id)? {
            return Ok(receipt);
        }
        let mutation = StoreMutation {
            operation_id,
            actor: OperationActor::User,
            requested_at_utc_ms,
            session_id,
            expected_revision,
            command,
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current = session_row(&transaction, session_id)?;
        let receipt =
            validate_mutation(&transaction, mutation, &current, &[MeetingPhase::Preflight])?;
        if let Some(receipt) = receipt {
            transaction.commit()?;
            return Ok(receipt);
        }
        let next_revision = current.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
        let now = utc_now_ms();
        transaction.execute(
            "UPDATE meeting_sessions SET preflight_json = ?1, revision = ?2 WHERE id = ?3",
            params![
                encode_json(&preflight)?,
                to_i64(next_revision)?,
                id(session_id)
            ],
        )?;
        append_event(
            &transaction,
            session_id,
            next_revision,
            current.phase,
            current.phase,
            "preflight_refreshed",
            None,
        )?;
        let receipt = committed_receipt(
            mutation,
            current.phase,
            current.phase,
            now,
            next_revision,
            Vec::new(),
        );
        insert_operation_receipt(&transaction, &receipt, now)?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn cancel_preflight(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        session_id: MeetingSessionId,
        expected_revision: u64,
    ) -> Result<OperationReceipt, StoreError> {
        if let Some(receipt) = self.operation_receipt(operation_id)? {
            return Ok(receipt);
        }
        let mutation = StoreMutation {
            operation_id,
            actor: OperationActor::User,
            requested_at_utc_ms,
            session_id,
            expected_revision,
            command: MeetingCommandKind::PreflightCancel,
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current = session_row(&transaction, session_id)?;
        let validation =
            validate_mutation(&transaction, mutation, &current, &[MeetingPhase::Preflight])?;
        if let Some(receipt) = validation {
            transaction.commit()?;
            return Ok(receipt);
        }
        let now = utc_now_ms();
        let receipt = OperationReceipt {
            schema_version: STORE_SCHEMA_VERSION,
            operation_id: mutation.operation_id,
            session_id: Some(mutation.session_id),
            actor: OperationActor::User,
            command: mutation.command,
            expected_revision: mutation.expected_revision,
            from_phase: Some(MeetingPhase::Preflight),
            to_phase: None,
            requested_at_utc_ms: mutation.requested_at_utc_ms,
            committed_at_utc_ms: Some(now),
            result: OperationResult::Committed,
            reason_codes: Vec::new(),
            new_revision: None,
            effect_ids: Vec::new(),
        };
        transaction.execute(
            "DELETE FROM meeting_sessions WHERE id = ?1",
            params![id(session_id)],
        )?;
        insert_operation_receipt(&transaction, &receipt, now)?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn start_with_plan_and_consent(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        plan: &MeetingRunPlan,
        consent: &MeetingConsent,
        expected_revision: u64,
    ) -> Result<OperationReceipt, StoreError> {
        if let Some(receipt) = self.operation_receipt(operation_id)? {
            return Ok(receipt);
        }
        let mutation = StoreMutation {
            operation_id,
            actor: OperationActor::User,
            requested_at_utc_ms,
            session_id: plan.session_id,
            expected_revision,
            command: MeetingCommandKind::Start,
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current = session_row(&transaction, plan.session_id)?;
        let validation =
            validate_mutation(&transaction, mutation, &current, &[MeetingPhase::Preflight])?;
        if let Some(receipt) = validation {
            transaction.commit()?;
            return Ok(receipt);
        }
        if let MeetingConsentProvenance::StandingSeries {
            series_key,
            granted_at_utc_ms,
        } = &consent.provenance
        {
            let standing = transaction
                .query_row(
                    "SELECT policy_version, granted_at_utc_ms, acknowledged_sources_json
                       FROM meeting_series_consents
                      WHERE series_key = ?1 AND revoked_at_utc_ms IS NULL",
                    [series_key],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            let Some((policy_version, stored_granted_at, sources_json)) = standing else {
                return Err(StoreError::ConsentStale);
            };
            let sources: Vec<SourceKind> = decode_json(&sources_json)?;
            let source_acknowledgements_match = sources.contains(&SourceKind::Microphone)
                == consent.microphone_acknowledged
                && sources.contains(&SourceKind::SystemAudio) == consent.system_audio_acknowledged;
            if policy_version != i64::from(consent.policy_version)
                || stored_granted_at != *granted_at_utc_ms
                || !source_acknowledgements_match
            {
                return Err(StoreError::ConsentStale);
            }
        }
        let next_revision = current.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
        let now = utc_now_ms();
        transaction.execute(
            "INSERT INTO meeting_consents (
                consent_id, session_id, attempt_number, preflight_revision, policy_version,
                acknowledgement_json, acknowledged_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id(consent.consent_id),
                id(consent.session_id),
                i64::from(consent.attempt_number),
                to_i64(consent.preflight_revision)?,
                i64::from(consent.policy_version),
                encode_json(consent)?,
                consent.acknowledged_at_utc_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO meeting_run_plans (
                plan_id, session_id, attempt_number, schema_version, consent_id,
                canonical_plan_json, created_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id(plan.plan_id),
                id(plan.session_id),
                i64::from(plan.attempt_number),
                i64::from(plan.schema_version),
                id(plan.consent_id),
                encode_json(plan)?,
                now,
            ],
        )?;
        transaction.execute(
            "UPDATE meeting_sessions
             SET phase = 'starting', revision = ?1, successful_plan_id = ?2, started_at_utc_ms = ?3
             WHERE id = ?4",
            params![
                to_i64(next_revision)?,
                id(plan.plan_id),
                now,
                id(plan.session_id)
            ],
        )?;
        append_event(
            &transaction,
            plan.session_id,
            next_revision,
            MeetingPhase::Preflight,
            MeetingPhase::Starting,
            "start_authorized",
            None,
        )?;
        let receipt = committed_receipt(
            mutation,
            MeetingPhase::Preflight,
            MeetingPhase::Starting,
            now,
            next_revision,
            vec![id(plan.plan_id), id(consent.consent_id)],
        );
        insert_operation_receipt(&transaction, &receipt, now)?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub(crate) fn transition(
        &self,
        transition: StoreTransition<'_>,
    ) -> Result<Option<OperationReceipt>, StoreError> {
        let StoreTransition {
            operation_id,
            actor,
            command,
            requested_at_utc_ms,
            session_id,
            expected_revision,
            allowed_from,
            next_phase,
            event_kind,
            reason_codes,
        } = transition;
        if let Some(operation_id) = operation_id {
            if let Some(receipt) = self.operation_receipt(operation_id)? {
                return Ok(Some(receipt));
            }
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current = session_row(&transaction, session_id)?;
        if let Some(operation_id) = operation_id {
            let mutation = StoreMutation {
                operation_id,
                actor: OperationActor::User,
                requested_at_utc_ms,
                session_id,
                expected_revision,
                command,
            };
            if let Some(receipt) =
                validate_mutation(&transaction, mutation, &current, allowed_from)?
            {
                transaction.commit()?;
                return Ok(Some(receipt));
            }
        } else if !allowed_from.contains(&current.phase) {
            return Err(StoreError::Conflict);
        }
        let next_revision = current.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
        let now = utc_now_ms();
        let started_at_utc_ms = (current.started_at_utc_ms.is_none()
            && next_phase == MeetingPhase::Starting)
            .then_some(now);
        let completed = next_phase == MeetingPhase::ReviewReady;
        let delete_after_utc_ms = if completed {
            let policy: MeetingRetentionPolicy = decode_json(&current.retention_policy_json)?;
            policy.delete_after_utc_ms(now)
        } else {
            None
        };
        transaction.execute(
            "UPDATE meeting_sessions
             SET phase = ?1, revision = ?2,
                 started_at_utc_ms = COALESCE(started_at_utc_ms, ?3),
                 ended_at_utc_ms = CASE WHEN ?4 = 1 THEN ?5 ELSE ended_at_utc_ms END,
                 delete_after_utc_ms = CASE WHEN ?4 = 1 THEN ?6 ELSE delete_after_utc_ms END
             WHERE id = ?7",
            params![
                phase_db(next_phase),
                to_i64(next_revision)?,
                started_at_utc_ms,
                bool_to_i64(completed),
                completed.then_some(now),
                delete_after_utc_ms,
                id(session_id),
            ],
        )?;
        append_event(
            &transaction,
            session_id,
            next_revision,
            current.phase,
            next_phase,
            event_kind,
            None,
        )?;
        let receipt = operation_id.map(|operation_id| {
            let mut receipt = committed_receipt(
                StoreMutation {
                    operation_id,
                    actor: OperationActor::User,
                    requested_at_utc_ms,
                    session_id,
                    expected_revision,
                    command,
                },
                current.phase,
                next_phase,
                now,
                next_revision,
                Vec::new(),
            );
            receipt.actor = actor;
            receipt.reason_codes = reason_codes;
            receipt
        });
        if let Some(receipt) = &receipt {
            insert_operation_receipt(&transaction, receipt, now)?;
        }
        transaction.commit()?;
        Ok(receipt)
    }

    pub(crate) fn create_track(&self, creation: TrackCreation<'_>) -> Result<(), StoreError> {
        let TrackCreation {
            session_id,
            plan_id,
            source_kind,
            required,
            requested,
            descriptor_json,
            report,
        } = creation;
        if source_kind != report.source_kind {
            return Err(StoreError::Invalid);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO meeting_source_tracks (
                track_id, session_id, plan_id, source_kind, required, requested, descriptor_json,
                timestamp_bridge_json, format_json, health
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id(report.track_id),
                id(session_id),
                id(plan_id),
                report.source_kind.as_str(),
                bool_to_i64(required),
                bool_to_i64(requested),
                descriptor_json,
                encode_json(&report.timestamp_bridge)?,
                encode_json(&report.format)?,
                encode_json(&SourceHealth::Starting)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO meeting_track_checkpoints (
                track_id, next_sequence, durable_offset_ns, durable_bytes, updated_at_utc_ms
             ) VALUES (?1, 0, NULL, 0, ?2)",
            params![id(report.track_id), utc_now_ms()],
        )?;
        transaction.execute(
            "INSERT INTO meeting_source_clock_epochs (
                track_id, source_epoch, format_epoch, bridge_json, observed_host_monotonic_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id(report.track_id),
                to_i64(report.epoch.get())?,
                to_i64(report.format_epoch)?,
                encode_json(&report.timestamp_bridge)?,
                to_i64(report.timestamp_bridge.host_monotonic_anchor_ns)?,
            ],
        )?;
        transaction.commit()?;
        self.ensure_session_directory(session_id)?;
        Ok(())
    }

    pub fn update_track_health(
        &self,
        session_id: MeetingSessionId,
        track_id: SourceTrackId,
        health: SourceHealth,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE meeting_source_tracks SET health = ?1 WHERE track_id = ?2 AND session_id = ?3",
            params![encode_json(&health)?, id(track_id), id(session_id)],
        )?;
        note_health_change(&transaction, session_id, health)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn open_capture_window(
        &self,
        session_id: MeetingSessionId,
        start_offset_ns: u64,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let sequence: i64 = connection.query_row(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM meeting_capture_windows WHERE session_id = ?1",
            params![id(session_id)],
            |row| row.get(0),
        )?;
        connection.execute(
            "INSERT INTO meeting_capture_windows (session_id, sequence, start_offset_ns, end_offset_ns, close_reason)
             VALUES (?1, ?2, ?3, NULL, NULL)",
            params![id(session_id), sequence, to_i64(start_offset_ns)?],
        )?;
        Ok(())
    }

    pub fn close_open_capture_window(
        &self,
        session_id: MeetingSessionId,
        end_offset_ns: u64,
        close_reason: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE meeting_capture_windows
             SET end_offset_ns = ?1, close_reason = ?2
             WHERE session_id = ?3 AND end_offset_ns IS NULL",
            params![to_i64(end_offset_ns)?, close_reason, id(session_id)],
        )?;
        Ok(())
    }

    /// A gap is an interval, not an event. A condition that persists reports
    /// itself once per buffer - a format the bridge cannot read, a lane the
    /// writer could not drain - and one autocommit insert per report is an
    /// fsync per report on the connection both lanes share, which is what
    /// starved the microphone writer. Extending the run's open row keeps the
    /// interval and the frame total while holding the write rate at one row
    /// per condition per epoch.
    pub fn record_gap(&self, gap: &SourceGap) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let extended = transaction.execute(
            "UPDATE meeting_source_gaps SET
                end_offset_ns = COALESCE(?1, ?2, end_offset_ns),
                dropped_frames = CASE
                    WHEN ?3 IS NULL THEN dropped_frames
                    ELSE COALESCE(dropped_frames, 0) + ?3
                END,
                observed_at_utc_ms = ?4
             WHERE gap_id = (
                    SELECT gap_id FROM meeting_source_gaps
                     WHERE track_id = ?5 ORDER BY gap_id DESC LIMIT 1
                 )
               AND source_epoch = ?6
               AND reason = ?7",
            params![
                optional_i64(gap.end_offset_ns)?,
                optional_i64(gap.start_offset_ns)?,
                optional_i64(gap.dropped_frames)?,
                utc_now_ms(),
                id(gap.track_id),
                to_i64(gap.epoch.get())?,
                encode_json(&gap.reason)?,
            ],
        )?;
        if extended == 0 {
            transaction.execute(
                "INSERT INTO meeting_source_gaps (
                    track_id, source_epoch, start_offset_ns, end_offset_ns, reason, dropped_frames,
                    observed_at_utc_ms, details_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '{}')",
                params![
                    id(gap.track_id),
                    to_i64(gap.epoch.get())?,
                    optional_i64(gap.start_offset_ns)?,
                    optional_i64(gap.end_offset_ns)?,
                    encode_json(&gap.reason)?,
                    optional_i64(gap.dropped_frames)?,
                    utc_now_ms(),
                ],
            )?;
        }
        // A lane that lost audio is not healthy, and the health column is its
        // own memo: the predicate matches only the first loss per track, so a
        // condition that reports itself once per buffer still writes one
        // verdict and one revision. A pause is the one gap nobody lost: the
        // operator asked for that silence, so it stays on the ledger and
        // leaves the verdict alone. The startup interval never comes through
        // here - `commit_durable_records` writes it directly.
        let degraded = if gap.reason == SourceGapReason::Paused {
            0
        } else {
            transaction.execute(
                "UPDATE meeting_source_tracks SET health = ?1
                 WHERE track_id = ?2 AND health IN (?3, ?4)",
                params![
                    encode_json(&SourceHealth::Degraded)?,
                    id(gap.track_id),
                    encode_json(&SourceHealth::Starting)?,
                    encode_json(&SourceHealth::Healthy)?,
                ],
            )?
        };
        if degraded > 0 {
            let session_id = track_session(&transaction, gap.track_id)?;
            note_health_change(&transaction, session_id, SourceHealth::Degraded)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// The seal is where a lane's health stops being provisional. A track still
    /// `starting` never committed a packet - `commit_durable_records` is the
    /// only writer that grants `healthy` - so its audio does not exist, which
    /// is a failure however cleanly its source shut down.
    pub fn seal_source_health(&self, session_id: MeetingSessionId) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let failed = transaction.execute(
            "UPDATE meeting_source_tracks SET health = ?1
             WHERE session_id = ?2 AND health = ?3",
            params![
                encode_json(&SourceHealth::Failed)?,
                id(session_id),
                encode_json(&SourceHealth::Starting)?,
            ],
        )?;
        if failed > 0 {
            note_health_change(&transaction, session_id, SourceHealth::Failed)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn record_clock_epoch(&self, epoch: SourceClockEpoch) -> Result<(), StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT OR REPLACE INTO meeting_source_clock_epochs (
                track_id, source_epoch, format_epoch, bridge_json, observed_host_monotonic_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id(epoch.track_id),
                to_i64(epoch.epoch.get())?,
                to_i64(epoch.format_epoch)?,
                encode_json(&epoch.bridge)?,
                to_i64(epoch.bridge.host_monotonic_anchor_ns)?,
            ],
        )?;
        Ok(())
    }

    pub fn open_track_writer(
        self: &Arc<Self>,
        session_id: MeetingSessionId,
        track_id: SourceTrackId,
        plan: MeetingStoragePlan,
    ) -> Result<MeetingTrackWriter, StoreError> {
        let track = self.track_row(track_id)?;
        if track.session_id != session_id {
            return Err(StoreError::NotFound);
        }
        let files = self.track_files(session_id, track_id);
        let key = self.track_key(session_id, track_id)?;
        MeetingTrackWriter::open(Arc::clone(self), track_id, files, key, plan)
    }

    pub fn session_snapshot(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingSessionSnapshot, StoreError> {
        let connection = self.connection()?;
        let row = session_row(&connection, session_id)?;
        let preflight = if row.phase == MeetingPhase::Preflight {
            Some(decode_json::<MeetingPreflightSnapshot>(
                &row.preflight_json,
            )?)
        } else {
            None
        };
        let sources = match preflight.as_ref() {
            Some(snapshot) => snapshot.sources.clone(),
            None => source_snapshots(&connection, session_id)?,
        };
        let completeness = derive_completeness(&connection, session_id, &sources)?;
        let open_capture_window_started_at_ns = connection
            .query_row(
                "SELECT start_offset_ns FROM meeting_capture_windows
                 WHERE session_id = ?1 AND end_offset_ns IS NULL ORDER BY sequence DESC LIMIT 1",
                params![id(session_id)],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(from_i64)
            .transpose()?;
        let allowed_actions = allowed_actions(row.phase);
        let elapsed_offset_ns = sources
            .iter()
            .filter_map(|source| source.last_durable_offset_ns)
            .max();
        let preflight_local_processing =
            preflight.as_ref().map(|snapshot| snapshot.local_processing);
        let storage = preflight
            .as_ref()
            .map(|snapshot| snapshot.storage)
            .unwrap_or(StorageAvailability::Available);
        Ok(MeetingSessionSnapshot {
            session_id,
            phase: row.phase,
            revision: row.revision,
            title: row.title,
            started_at_utc_ms: row.started_at_utc_ms,
            elapsed_offset_ns,
            sources,
            open_capture_window_started_at_ns,
            capture_completeness: completeness,
            storage,
            processing_status: decode_json(&row.processing_status_json)?,
            preflight_local_processing,
            retention_deadline_utc_ms: row.delete_after_utc_ms,
            allowed_actions,
        })
    }

    /// The immutable origin selected when this session was created.
    pub(crate) fn meeting_origin(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingOrigin, StoreError> {
        let connection = self.connection()?;
        let origin_json = connection
            .query_row(
                "SELECT origin_kind FROM meeting_sessions WHERE id = ?1",
                params![id(session_id)],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        decode_json(&origin_json)
    }

    /// What one session's recording disclosure is doing.
    ///
    /// An absent column is [`MeetingSessionDisclosure::NotAsked`], which is
    /// every meeting ever recorded before the announce checkbox existed and
    /// every meeting whose operator left it clear.
    pub(crate) fn session_disclosure(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingSessionDisclosure, StoreError> {
        let connection = self.connection()?;
        session_disclosure_in(&connection, session_id)
    }

    /// Ask this session to announce itself, naming who the room is told the
    /// notes are for.
    ///
    /// Only ever moves a session from `NotAsked` to `Pending`. A session that
    /// already attempted its disclosure keeps that record: the line is posted
    /// once per recording, and a second request would be a second line in
    /// somebody's chat.
    pub(crate) fn request_session_disclosure(
        &self,
        session_id: MeetingSessionId,
        notetaker: &str,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if session_disclosure_in(&transaction, session_id)? != MeetingSessionDisclosure::NotAsked {
            return Ok(());
        }
        write_session_disclosure_in(
            &transaction,
            session_id,
            &MeetingSessionDisclosure::Pending {
                notetaker: notetaker.to_string(),
            },
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Write down what the one attempt did, and answer with the record.
    ///
    /// Idempotent on the attempt rather than on an operation id: the disclosure
    /// is one insertion into somebody else's chat, so the first receipt is the
    /// record and a later call reads it back instead of pasting again.
    pub(crate) fn record_session_disclosure(
        &self,
        session_id: MeetingSessionId,
        receipt: &crate::delivery::DeliveryReceipt,
    ) -> Result<MeetingSessionDisclosure, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let held = session_disclosure_in(&transaction, session_id)?;
        if let MeetingSessionDisclosure::Attempted { .. } = held {
            return Ok(held);
        }
        let attempted = MeetingSessionDisclosure::Attempted {
            receipt: receipt.clone(),
        };
        write_session_disclosure_in(&transaction, session_id, &attempted)?;
        transaction.commit()?;
        Ok(attempted)
    }

    pub fn preflight_snapshot(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingPreflightSnapshot, StoreError> {
        let connection = self.connection()?;
        let row = session_row(&connection, session_id)?;
        if row.phase != MeetingPhase::Preflight {
            return Err(StoreError::Conflict);
        }
        decode_json(&row.preflight_json)
    }

    /// One page of retained meetings, newest first, narrowed by `filter`.
    ///
    /// Every row carries what the list actually renders — the recorded
    /// duration, the sources that opened, the diarized speaker labels, and the
    /// one line a reader gets before opening the meeting — so the page is one
    /// read rather than a request per row from the webview.
    pub fn list_sessions(
        &self,
        cursor_utc_ms: Option<i64>,
        limit: usize,
        filter: &MeetingListFilter,
    ) -> Result<PaginatedMeetings, StoreError> {
        let page_size = limit.clamp(1, 100);
        let connection = self.connection()?;
        let window_start_utc_ms = match filter.window.days() {
            None => None,
            Some(days) => {
                Some(local_days_start_utc_ms(Local::now(), days).map_err(|_| StoreError::Invalid)?)
            }
        };
        let title_pattern = match filter.title_query.trim() {
            "" => None,
            needle => Some(like_contains(needle)),
        };
        let mut statement = connection.prepare(&format!(
            "SELECT m.id, m.title, m.phase, m.created_at_utc_ms, m.processing_status,
                    (SELECT SUM(w.end_offset_ns - w.start_offset_ns)
                       FROM meeting_capture_windows w
                      WHERE w.session_id = m.id AND w.end_offset_ns IS NOT NULL),
                    json_extract(({CURRENT_ARTIFACT_CONTENT}), '$.ledger.headline'),
                    json_extract(({CURRENT_ARTIFACT_CONTENT}), '$.summary.text'),
                    m.current_transcript_revision_id IS NOT NULL
             FROM meeting_sessions m
             WHERE m.phase != 'deleting'
               AND (?1 IS NULL OR m.created_at_utc_ms < ?1)
               AND (?3 IS NULL OR m.created_at_utc_ms >= ?3)
               AND (?4 IS NULL OR m.title LIKE ?4 ESCAPE '\\')
               AND ({status})
             ORDER BY m.created_at_utc_ms DESC LIMIT ?2",
            status = status_predicate(filter.status),
        ))?;
        let rows = statement.query_map(
            params![
                cursor_utc_ms,
                i64::try_from(page_size + 1).map_err(|_| StoreError::Invalid)?,
                window_start_utc_ms,
                title_pattern,
            ],
            |row| {
                Ok(ListedSessionRow {
                    session: row.get::<_, String>(0)?,
                    title: row.get::<_, String>(1)?,
                    phase: row.get::<_, String>(2)?,
                    created_at_utc_ms: row.get::<_, i64>(3)?,
                    processing_status: row.get::<_, String>(4)?,
                    recorded_duration_ns: row.get::<_, Option<i64>>(5)?,
                    ledger_headline: row.get::<_, Option<String>>(6)?,
                    summary_text: row.get::<_, Option<String>>(7)?,
                    has_transcript: row.get::<_, i64>(8)? != 0,
                })
            },
        )?;
        let mut entries = Vec::new();
        for row in rows {
            let row = row?;
            let session_id = MeetingSessionId::from_uuid(parse_uuid(&row.session)?);
            let sources = source_snapshots(&connection, session_id)?;
            entries.push(MeetingHistorySummary {
                kind: HistoryItemKind::Meeting,
                session_id,
                title: row.title,
                phase: phase_from_db(&row.phase)?,
                created_at_utc_ms: row.created_at_utc_ms,
                capture_completeness: derive_completeness(&connection, session_id, &sources)?,
                processing_status: decode_json(&row.processing_status)?,
                recorded_duration_ms: row
                    .recorded_duration_ns
                    .map(|duration| duration / 1_000_000),
                /* `meeting_source_tracks` is UNIQUE per (session, source_kind)
                 * and read back ORDER BY source_kind, so the track list is
                 * already the deduplicated microphone-then-system-audio run. */
                sources: sources.iter().map(|source| source.source_kind).collect(),
                speaker_labels: speaker_labels_for_session(&connection, session_id)?,
                headline: row_headline(
                    &connection,
                    session_id,
                    row.ledger_headline.as_deref(),
                    row.summary_text.as_deref(),
                    row.has_transcript,
                )?,
            });
        }
        let has_more = entries.len() > page_size;
        entries.truncate(page_size);
        Ok(PaginatedMeetings { entries, has_more })
    }

    /// Return the dense meeting trend from one read-only projection. All
    /// counters are derived from retained rows; the captured duration unions
    /// durable audio intervals so simultaneous sources are never double-counted.
    pub(crate) fn trend_projection(
        &self,
        request: DashboardTrendRequest,
    ) -> Result<MeetingTrendProjection, StoreError> {
        self.trend_projection_at(request, Local::now())
    }

    /// The same projection against a caller's clock, so a surface that prints
    /// the moment it read the corpus at reads it at that moment.
    pub(crate) fn trend_projection_at(
        &self,
        request: DashboardTrendRequest,
        now: DateTime<Local>,
    ) -> Result<MeetingTrendProjection, StoreError> {
        let mut connection = self.connection()?;
        Self::trend_projection_with_connection_at(&mut connection, request, now)
    }

    fn trend_projection_with_connection_at(
        connection: &mut Connection,
        request: DashboardTrendRequest,
        now: DateTime<Local>,
    ) -> Result<MeetingTrendProjection, StoreError> {
        let calendar =
            LocalCalendarRange::at(now, request.range).map_err(|_| StoreError::Invalid)?;
        let rows = {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let mut statement = transaction.prepare(
                "WITH retained_sessions AS (
                    SELECT id, revision, created_at_utc_ms, current_transcript_revision_id
                     FROM meeting_sessions
                     WHERE phase != 'deleting'
                 ),
                 durable_intervals AS (
                    SELECT
                        tracks.session_id,
                        tracks.track_id,
                        records.source_sequence,
                        records.start_offset_ns,
                        records.start_offset_ns + records.duration_ns AS end_offset_ns
                     FROM meeting_source_tracks AS tracks
                     JOIN retained_sessions AS sessions ON sessions.id = tracks.session_id
                     JOIN meeting_track_records AS records ON records.track_id = tracks.track_id
                     WHERE records.start_offset_ns IS NOT NULL
                       AND records.duration_ns > 0
                 ),
                 ordered_intervals AS (
                    SELECT
                        session_id,
                        track_id,
                        source_sequence,
                        start_offset_ns,
                        end_offset_ns,
                        MAX(end_offset_ns) OVER (
                            PARTITION BY session_id
                            ORDER BY start_offset_ns, end_offset_ns, track_id, source_sequence
                            ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
                        ) AS previous_end_offset_ns
                     FROM durable_intervals
                 ),
                 interval_groups AS (
                    SELECT
                        session_id,
                        start_offset_ns,
                        end_offset_ns,
                        SUM(
                            CASE
                                WHEN previous_end_offset_ns IS NULL
                                  OR start_offset_ns > previous_end_offset_ns
                                THEN 1
                                ELSE 0
                            END
                        ) OVER (
                            PARTITION BY session_id
                            ORDER BY start_offset_ns, end_offset_ns, track_id, source_sequence
                            ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
                        ) AS interval_group
                     FROM ordered_intervals
                 ),
                 merged_intervals AS (
                    SELECT
                        session_id,
                        interval_group,
                        MIN(start_offset_ns) AS start_offset_ns,
                        MAX(end_offset_ns) AS end_offset_ns
                     FROM interval_groups
                     GROUP BY session_id, interval_group
                 ),
                 verified_duration_by_session AS (
                    SELECT
                        session_id,
                        COALESCE(SUM(end_offset_ns - start_offset_ns), 0)
                            AS verified_captured_duration_ns
                     FROM merged_intervals
                     GROUP BY session_id
                 ),
                 segments_by_session AS (
                    SELECT
                        sessions.id AS session_id,
                        COUNT(segments.segment_id) AS transcript_segments
                     FROM retained_sessions AS sessions
                     LEFT JOIN meeting_transcript_segments AS segments
                       ON segments.transcript_revision_id = sessions.current_transcript_revision_id
                      AND COALESCE((SELECT e.removed
                                    FROM meeting_segment_edits AS e
                                    WHERE e.segment_id = segments.segment_id
                                    ORDER BY e.edit_sequence DESC LIMIT 1), 0) = 0
                     GROUP BY sessions.id
                 ),
                 actions_by_session AS (
                    SELECT
                        sessions.id AS session_id,
                        COALESCE(
                            SUM(
                                COALESCE(
                                    json_array_length(
                                        artifacts.content_json,
                                        '$.action_items'
                                    ),
                                    0
                                )
                            ),
                            0
                        ) AS generated_action_items
                     FROM retained_sessions AS sessions
                     LEFT JOIN meeting_artifact_revisions AS artifacts
                       ON artifacts.session_id = sessions.id
                      AND artifacts.transcript_revision_id =
                          sessions.current_transcript_revision_id
                      AND artifacts.input_revision = sessions.revision
                      AND artifacts.state = 'current'
                     GROUP BY sessions.id
                 ),
                 session_metrics AS (
                    SELECT
                        sessions.created_at_utc_ms,
                        date(
                            sessions.created_at_utc_ms / 1000,
                            'unixepoch',
                            'localtime'
                        ) AS local_date,
                        COALESCE(
                            duration.verified_captured_duration_ns,
                            0
                        ) AS verified_captured_duration_ns,
                        COALESCE(segments.transcript_segments, 0) AS transcript_segments,
                        COALESCE(actions.generated_action_items, 0) AS generated_action_items
                     FROM retained_sessions AS sessions
                     LEFT JOIN verified_duration_by_session AS duration
                       ON duration.session_id = sessions.id
                     LEFT JOIN segments_by_session AS segments
                       ON segments.session_id = sessions.id
                     LEFT JOIN actions_by_session AS actions
                       ON actions.session_id = sessions.id
                 ),
                 range_days AS (
                    SELECT
                        local_date,
                        COUNT(*) AS meetings,
                        COALESCE(SUM(verified_captured_duration_ns), 0)
                            AS verified_captured_duration_ns,
                        COALESCE(SUM(transcript_segments), 0) AS transcript_segments,
                        COALESCE(SUM(generated_action_items), 0) AS generated_action_items
                     FROM session_metrics
                     WHERE created_at_utc_ms >= ?1 AND created_at_utc_ms < ?2
                     GROUP BY local_date
                 ),
                 all_time AS (
                    SELECT
                        COUNT(*) AS meetings,
                        COALESCE(SUM(verified_captured_duration_ns), 0)
                            AS verified_captured_duration_ns,
                        COALESCE(SUM(transcript_segments), 0) AS transcript_segments,
                        COALESCE(SUM(generated_action_items), 0) AS generated_action_items
                     FROM session_metrics
                 )
                 SELECT
                    'range_day' AS projection,
                    local_date,
                    meetings,
                    verified_captured_duration_ns,
                    transcript_segments,
                    generated_action_items
                 FROM range_days
                 UNION ALL
                 SELECT
                    'all_time' AS projection,
                    NULL AS local_date,
                    meetings,
                    verified_captured_duration_ns,
                    transcript_segments,
                    generated_action_items
                 FROM all_time",
            )?;
            let rows = statement.query_map(
                params![calendar.start_utc_ms(), calendar.end_exclusive_utc_ms()],
                |row| {
                    Ok((
                        row.get::<_, String>("projection")?,
                        row.get::<_, Option<String>>("local_date")?,
                        row.get::<_, i64>("meetings")?,
                        row.get::<_, i64>("verified_captured_duration_ns")?,
                        row.get::<_, i64>("transcript_segments")?,
                        row.get::<_, i64>("generated_action_items")?,
                    ))
                },
            )?;
            let collected = rows.collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);
            transaction.commit()?;
            collected
        };

        let mut daily_values = HashMap::<String, MeetingTrendValues>::new();
        let mut all_time_values = MeetingTrendValues::default();
        for row in rows {
            let (
                projection,
                local_date,
                meetings,
                verified_captured_duration_ns,
                transcript_segments,
                generated_action_items,
            ) = row;
            let values = MeetingTrendValues {
                meetings: meeting_trend_value(meetings)?,
                verified_captured_duration_ns: meeting_trend_value(verified_captured_duration_ns)?,
                transcript_segments: meeting_trend_value(transcript_segments)?,
                generated_action_items: meeting_trend_value(generated_action_items)?,
            };
            match projection.as_str() {
                "range_day" => {
                    let local_date = local_date.ok_or(StoreError::Corrupt)?;
                    daily_values.entry(local_date).or_default().add(values)?;
                }
                "all_time" => all_time_values.add(values)?,
                _ => return Err(StoreError::Corrupt),
            }
        }

        let mut range_values = MeetingTrendValues::default();
        let mut points = Vec::with_capacity(request.range.days());
        for date in calendar.local_dates().map_err(|_| StoreError::Invalid)? {
            let values = daily_values
                .remove(&date.format("%F").to_string())
                .unwrap_or_default();
            range_values.add(values)?;
            let totals = values.totals();
            points.push(MeetingTrendPoint {
                local_date: date.format("%F").to_string(),
                meetings: totals.meetings,
                verified_captured_duration_ms: totals.verified_captured_duration_ms,
                transcript_segments: totals.transcript_segments,
                generated_action_items: totals.generated_action_items,
            });
        }
        if !daily_values.is_empty() {
            return Err(StoreError::Corrupt);
        }

        Ok(MeetingTrendProjection::Available {
            range: request.range,
            range_start_local_date: calendar.start_local_date(),
            range_end_local_date: calendar.end_local_date(),
            all_time: all_time_values.totals(),
            range_total: range_values.totals(),
            points,
        })
    }

    pub fn set_title(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        session_id: MeetingSessionId,
        expected_revision: u64,
        title: String,
    ) -> Result<OperationReceipt, StoreError> {
        self.edit_session(
            StoreMutation {
                operation_id,
                actor: OperationActor::User,
                requested_at_utc_ms,
                session_id,
                expected_revision,
                command: MeetingCommandKind::TitleSet,
            },
            "title_set",
            |transaction| {
                transaction.execute(
                    "UPDATE meeting_sessions SET title = ?1 WHERE id = ?2",
                    params![title, id(session_id)],
                )?;
                mark_artifacts_out_of_date(transaction, session_id)?;
                rebuild_search_documents_in(transaction, session_id)
            },
        )
    }

    /// Title a meeting from what was said in it, once its notes exist.
    ///
    /// Only the manual default is replaced. A title a person typed and a title
    /// a calendar event supplied are both somebody's answer to this question
    /// already, and overwriting either would be the app arguing with its user.
    ///
    /// Deliberately not an `edit_session`: this runs immediately after an
    /// artifact revision lands, and `edit_session` marks artifacts out of date.
    /// A title read *from* the current artifact must not invalidate it —
    /// that is a meeting whose every generation retires itself.
    ///
    /// Returns `None` when there was nothing to do, which is the ordinary
    /// case: a titled meeting, or a headline no title could be read from.
    pub(crate) fn derive_title_from_headline(
        &self,
        session_id: MeetingSessionId,
        headline: &str,
    ) -> Result<Option<OperationReceipt>, StoreError> {
        let Some(title) = derived_title(headline) else {
            return Ok(None);
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = session_row(&transaction, session_id)?;
        if current.title != MANUAL_DEFAULT_TITLE || title == current.title {
            transaction.commit()?;
            return Ok(None);
        }
        let next_revision = current.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
        let now = utc_now_ms();
        transaction.execute(
            "UPDATE meeting_sessions SET title = ?1, revision = ?2 WHERE id = ?3",
            params![title, to_i64(next_revision)?, id(session_id)],
        )?;
        rebuild_search_documents_in(&transaction, session_id)?;
        // The event log is where a receipt's details live in this store, and
        // the receipt's `new_revision` is the sequence that reads them back.
        append_event_with_details(
            &transaction,
            session_id,
            next_revision,
            current.phase,
            current.phase,
            "title_derived",
            None,
            &encode_json(&serde_json::json!({
                "source": "summary_headline",
                "headline": headline.trim(),
                "from": MANUAL_DEFAULT_TITLE,
                "to": title,
            }))?,
        )?;
        let receipt = OperationReceipt {
            schema_version: STORE_SCHEMA_VERSION,
            operation_id: MeetingOperationId::new(),
            session_id: Some(session_id),
            // Nobody asked for this one: the pipeline read it off the notes.
            actor: OperationActor::System,
            command: MeetingCommandKind::TitleSet,
            expected_revision: current.revision,
            from_phase: Some(current.phase),
            to_phase: Some(current.phase),
            requested_at_utc_ms: now,
            committed_at_utc_ms: Some(now),
            result: OperationResult::Committed,
            reason_codes: Vec::new(),
            new_revision: Some(next_revision),
            effect_ids: vec![title],
        };
        insert_operation_receipt(&transaction, &receipt, now)?;
        transaction.commit()?;
        Ok(Some(receipt))
    }

    pub fn create_note(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        note: &ManualNote,
        expected_revision: u64,
    ) -> Result<OperationReceipt, StoreError> {
        self.edit_session(
            StoreMutation {
                operation_id,
                actor: OperationActor::User,
                requested_at_utc_ms,
                session_id: note.session_id,
                expected_revision,
                command: MeetingCommandKind::NoteCreate,
            },
            "note_created",
            |transaction| {
                transaction.execute(
                    "INSERT INTO meeting_notes (
                        note_id, session_id, start_offset_ns, end_offset_ns, body, note_revision,
                        created_at_utc_ms, updated_at_utc_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        id(note.note_id),
                        id(note.session_id),
                        optional_i64(note.start_offset_ns)?,
                        optional_i64(note.end_offset_ns)?,
                        note.body,
                        to_i64(note.revision)?,
                        note.created_at_utc_ms,
                        note.updated_at_utc_ms,
                    ],
                )?;
                mark_artifacts_out_of_date(transaction, note.session_id)?;
                rebuild_search_documents_in(transaction, note.session_id)
            },
        )
    }

    pub fn update_note(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        note: &ManualNote,
        expected_session_revision: u64,
        expected_note_revision: u64,
    ) -> Result<OperationReceipt, StoreError> {
        self.edit_session(
            StoreMutation {
                operation_id,
                actor: OperationActor::User,
                requested_at_utc_ms,
                session_id: note.session_id,
                expected_revision: expected_session_revision,
                command: MeetingCommandKind::NoteUpdate,
            },
            "note_updated",
            |transaction| {
                let changed = transaction.execute(
                    "UPDATE meeting_notes
                     SET start_offset_ns = ?1, end_offset_ns = ?2, body = ?3,
                         note_revision = ?4, updated_at_utc_ms = ?5
                     WHERE note_id = ?6 AND session_id = ?7 AND note_revision = ?8",
                    params![
                        optional_i64(note.start_offset_ns)?,
                        optional_i64(note.end_offset_ns)?,
                        note.body,
                        to_i64(note.revision)?,
                        note.updated_at_utc_ms,
                        id(note.note_id),
                        id(note.session_id),
                        to_i64(expected_note_revision)?,
                    ],
                )?;
                if changed != 1 {
                    return Err(StoreError::Conflict);
                }
                mark_artifacts_out_of_date(transaction, note.session_id)?;
                rebuild_search_documents_in(transaction, note.session_id)
            },
        )
    }

    pub fn delete_note(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        session_id: MeetingSessionId,
        expected_session_revision: u64,
        note_id: ManualNoteId,
        expected_note_revision: u64,
    ) -> Result<OperationReceipt, StoreError> {
        self.edit_session(
            StoreMutation {
                operation_id,
                actor: OperationActor::User,
                requested_at_utc_ms,
                session_id,
                expected_revision: expected_session_revision,
                command: MeetingCommandKind::NoteDelete,
            },
            "note_deleted",
            |transaction| {
                let changed = transaction.execute(
                    "DELETE FROM meeting_notes WHERE note_id = ?1 AND session_id = ?2 AND note_revision = ?3",
                    params![id(note_id), id(session_id), to_i64(expected_note_revision)?],
                )?;
                if changed != 1 {
                    return Err(StoreError::Conflict);
                }
                mark_artifacts_out_of_date(transaction, session_id)?;
                rebuild_search_documents_in(transaction, session_id)
            },
        )
    }

    pub fn rename_speaker(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        session_id: MeetingSessionId,
        expected_revision: u64,
        speaker_id: SpeakerId,
        display_name: String,
    ) -> Result<OperationReceipt, StoreError> {
        self.edit_session(
            StoreMutation {
                operation_id,
                actor: OperationActor::User,
                requested_at_utc_ms,
                session_id,
                expected_revision,
                command: MeetingCommandKind::SpeakerRename,
            },
            "speaker_renamed",
            |transaction| {
                let changed = transaction.execute(
                    "UPDATE meeting_speakers SET display_name = ?1, revision = revision + 1
                     WHERE speaker_id = ?2 AND session_id = ?3",
                    params![display_name, id(speaker_id), id(session_id)],
                )?;
                if changed != 1 {
                    return Err(StoreError::NotFound);
                }
                voice_identity::clear_automatic_match_for_speaker_in(transaction, speaker_id)?;
                mark_artifacts_out_of_date(transaction, session_id)
            },
        )
    }

    pub fn merge_speaker(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        session_id: MeetingSessionId,
        expected_revision: u64,
        source_speaker_id: SpeakerId,
        target_speaker_id: SpeakerId,
    ) -> Result<OperationReceipt, StoreError> {
        if source_speaker_id == target_speaker_id {
            return Err(StoreError::Invalid);
        }
        self.edit_session(
            StoreMutation {
                operation_id,
                actor: OperationActor::User,
                requested_at_utc_ms,
                session_id,
                expected_revision,
                command: MeetingCommandKind::SpeakerMerge,
            },
            "speaker_merged",
            |transaction| {
                let changed = transaction.execute(
                    "UPDATE meeting_speakers SET merged_into_speaker_id = ?1, revision = revision + 1
                     WHERE speaker_id = ?2 AND session_id = ?3",
                    params![id(target_speaker_id), id(source_speaker_id), id(session_id)],
                )?;
                if changed != 1 {
                    return Err(StoreError::NotFound);
                }
                voice_identity::clear_automatic_matches_for_speakers_in(
                    transaction,
                    source_speaker_id,
                    target_speaker_id,
                )?;
                if voice_identity::remove_profile_samples_for_speaker_in(
                    transaction,
                    source_speaker_id,
                )? {
                    people::bump_people_revision_in(transaction)?;
                }
                mark_artifacts_out_of_date(transaction, session_id)
            },
        )
    }

    pub(crate) fn edit_segment(&self, edit: SegmentEdit) -> Result<OperationReceipt, StoreError> {
        let SegmentEdit {
            mutation,
            segment_id,
            replacement_text,
            removed,
        } = edit;
        let session_id = mutation.session_id;
        self.edit_session(mutation, "segment_edited", |transaction| {
            let segment_exists: Option<i64> = transaction
                .query_row(
                    "SELECT 1 FROM meeting_transcript_segments s
                     JOIN meeting_transcript_revisions r ON r.transcript_revision_id = s.transcript_revision_id
                     WHERE s.segment_id = ?1 AND r.session_id = ?2",
                    params![id(segment_id), id(session_id)],
                    |row| row.get(0),
                )
                .optional()?;
            if segment_exists.is_none() {
                return Err(StoreError::NotFound);
            }
            let sequence: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(edit_sequence), -1) + 1 FROM meeting_segment_edits WHERE segment_id = ?1",
                params![id(segment_id)],
                |row| row.get(0),
            )?;
            transaction.execute(
                "INSERT INTO meeting_segment_edits (
                    segment_id, edit_sequence, replacement_text, removed, operator_at_utc_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id(segment_id), sequence, replacement_text, bool_to_i64(removed), utc_now_ms()],
            )?;
            mark_artifacts_out_of_date(transaction, session_id)?;
            rebuild_search_documents_in(transaction, session_id)
        })
    }

    pub fn forget_question(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        session_id: MeetingSessionId,
        expected_revision: u64,
        question_id: MeetingQuestionId,
    ) -> Result<OperationReceipt, StoreError> {
        self.edit_session(
            StoreMutation {
                operation_id,
                actor: OperationActor::User,
                requested_at_utc_ms,
                session_id,
                expected_revision,
                command: MeetingCommandKind::QuestionForget,
            },
            "question_forgotten",
            |transaction| {
                let changed = transaction.execute(
                    "DELETE FROM meeting_questions WHERE question_id = ?1 AND session_id = ?2",
                    params![id(question_id), id(session_id)],
                )?;
                if changed != 1 {
                    return Err(StoreError::NotFound);
                }
                Ok(())
            },
        )
    }

    pub fn review_snapshot(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingReviewSnapshot, StoreError> {
        let session = self.session_snapshot(session_id)?;
        let connection = self.connection()?;
        let tracks = track_snapshots(&connection, session_id)?;
        let gaps = gaps_for_session(&connection, session_id)?;
        let speakers = speakers_for_session(&connection, session_id)?;
        let transcript = effective_segments_for_session(&connection, session_id)?;
        let notes = notes_for_session(&connection, session_id)?;
        let artifacts = artifact_revisions_for_session(&connection, session_id)?;
        let questions = question_history_for_session(&connection, session_id)?;
        let diarization = diarization_snapshot_for_session(&connection, session_id)?;
        let remote_cancellation_pending: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM meeting_remote_jobs WHERE session_id = ?1 AND cancellation_requested = 1)",
            params![id(session_id)],
            |row| row.get(0),
        )?;
        Ok(MeetingReviewSnapshot {
            can_export: matches!(
                session.phase,
                MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired
            ),
            session,
            tracks,
            gaps,
            speakers,
            transcript,
            notes,
            artifacts,
            questions,
            diarization,
            remote_cancellation_pending,
        })
    }

    pub(crate) fn begin_transcript_revision(
        &self,
        input: TranscriptRevisionInput<'_>,
    ) -> Result<TranscriptRevisionId, StoreError> {
        let revision_id = TranscriptRevisionId::new();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        session_row(&transaction, input.session_id)?;
        transaction.execute(
            "INSERT INTO meeting_transcript_revisions (
                transcript_revision_id, session_id, engine_id, model_version, destination_json,
                source_set_json, language, state, created_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', ?8)",
            params![
                id(revision_id),
                id(input.session_id),
                input.engine_id,
                input.model_version,
                encode_json(input.destination)?,
                encode_json(input.source_set)?,
                input.language,
                utc_now_ms(),
            ],
        )?;
        transaction.commit()?;
        Ok(revision_id)
    }

    pub(crate) fn append_transcript_segments(
        &self,
        session_id: MeetingSessionId,
        transcript_revision_id: TranscriptRevisionId,
        segments: &[TranscriptSegmentInput],
    ) -> Result<(), StoreError> {
        if segments.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let owner: String = transaction.query_row(
            "SELECT session_id FROM meeting_transcript_revisions WHERE transcript_revision_id = ?1 AND state = 'running'",
            params![id(transcript_revision_id)],
            |row| row.get(0),
        )?;
        if parse_uuid(&owner)? != session_id.uuid() {
            return Err(StoreError::Invalid);
        }
        let next_ordinal: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(ordinal), -1) + 1
             FROM meeting_transcript_segments WHERE transcript_revision_id = ?1",
            params![id(transcript_revision_id)],
            |row| row.get(0),
        )?;
        for (index, segment) in segments.iter().enumerate() {
            if segment.start_offset_ns >= segment.end_offset_ns || segment.text.trim().is_empty() {
                return Err(StoreError::Invalid);
            }
            let source_kind = source_kind_from_db(&transaction.query_row(
                "SELECT source_kind FROM meeting_source_tracks WHERE track_id = ?1 AND session_id = ?2",
                params![id(segment.track_id), id(session_id)],
                |row| row.get::<_, String>(0),
            )?)
            .map_err(|_| StoreError::Corrupt)?;
            if source_kind != segment.source_kind {
                return Err(StoreError::Invalid);
            }
            let speaker_id = match segment.speaker.as_deref().map(str::trim) {
                Some(display_name) if !display_name.is_empty() => {
                    named_speaker_for(&transaction, session_id, source_kind, display_name)?
                }
                _ => source_speaker_for(&transaction, session_id, source_kind)?,
            };
            let ordinal = next_ordinal
                .checked_add(i64::try_from(index).map_err(|_| StoreError::Corrupt)?)
                .ok_or(StoreError::Corrupt)?;
            transaction.execute(
                "INSERT INTO meeting_transcript_segments (
                    segment_id, transcript_revision_id, track_id, ordinal, start_offset_ns,
                    end_offset_ns, speaker_id, base_text, confidence_milli
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id(TranscriptSegmentId::new()),
                    id(transcript_revision_id),
                    id(segment.track_id),
                    ordinal,
                    to_i64(segment.start_offset_ns)?,
                    to_i64(segment.end_offset_ns)?,
                    id(speaker_id),
                    segment.text,
                    segment.confidence_milli.map(i64::from),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn complete_transcript_revision(
        &self,
        session_id: MeetingSessionId,
        transcript_revision_id: TranscriptRevisionId,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let owner: String = transaction.query_row(
            "SELECT session_id FROM meeting_transcript_revisions WHERE transcript_revision_id = ?1 AND state = 'running'",
            params![id(transcript_revision_id)],
            |row| row.get(0),
        )?;
        if parse_uuid(&owner)? != session_id.uuid() {
            return Err(StoreError::Invalid);
        }
        transaction.execute(
            "UPDATE meeting_transcript_revisions
             SET state = 'completed', completed_at_utc_ms = ?1
             WHERE transcript_revision_id = ?2",
            params![utc_now_ms(), id(transcript_revision_id)],
        )?;
        transaction.execute(
            "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
            params![id(transcript_revision_id), id(session_id)],
        )?;
        mark_artifacts_out_of_date(&transaction, session_id)?;
        rebuild_search_documents_in(&transaction, session_id)?;
        transaction.commit()?;
        Ok(())
    }

    /// File the session under the moment its audio was recorded rather than the
    /// moment it was imported.
    ///
    /// Every surface that dates a meeting reads `started_at_utc_ms`, so an
    /// imported recording of last Tuesday's call has to carry last Tuesday.
    /// `ended_at_utc_ms` is deliberately left to the ReviewReady transition:
    /// that field is when the meeting finished being processed, which for an
    /// import genuinely is now.
    pub(crate) fn set_imported_start(
        &self,
        session_id: MeetingSessionId,
        started_at_utc_ms: i64,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_sessions SET started_at_utc_ms = ?1 WHERE id = ?2",
            params![started_at_utc_ms, id(session_id)],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub(crate) fn current_transcript_revision_id(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<TranscriptRevisionId, StoreError> {
        let connection = self.connection()?;
        let value = connection
            .query_row(
                "SELECT current_transcript_revision_id FROM meeting_sessions WHERE id = ?1",
                params![id(session_id)],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .ok_or(StoreError::NotFound)?;
        Ok(TranscriptRevisionId::from_uuid(parse_uuid(&value)?))
    }

    pub(crate) fn transcript_segments_overlapping(
        &self,
        transcript_revision_id: TranscriptRevisionId,
        track_id: SourceTrackId,
        start_offset_ns: u64,
        end_offset_ns: u64,
    ) -> Result<Vec<TranscriptSegmentId>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT segment_id FROM meeting_transcript_segments
             WHERE transcript_revision_id = ?1 AND track_id = ?2
               AND start_offset_ns < ?3 AND end_offset_ns > ?4
             ORDER BY start_offset_ns, ordinal",
        )?;
        let rows = statement.query_map(
            params![
                id(transcript_revision_id),
                id(track_id),
                to_i64(end_offset_ns)?,
                to_i64(start_offset_ns)?,
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|row| {
            row.map_err(Into::into)
                .and_then(|value| Ok(TranscriptSegmentId::from_uuid(parse_uuid(&value)?)))
        })
        .collect()
    }

    pub(crate) fn begin_diarization_generation(
        &self,
        session_id: MeetingSessionId,
        transcript_revision_id: TranscriptRevisionId,
        input_revision: u64,
        model_id: &str,
        model_version: &str,
    ) -> Result<MeetingDiarizationGenerationId, StoreError> {
        let generation_id = MeetingDiarizationGenerationId::new();
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO meeting_diarization_generations (
                generation_id, session_id, transcript_revision_id, input_revision, model_id,
                model_version, state, created_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7)",
            params![
                id(generation_id),
                id(session_id),
                id(transcript_revision_id),
                to_i64(input_revision)?,
                model_id,
                model_version,
                utc_now_ms(),
            ],
        )?;
        Ok(generation_id)
    }

    pub(crate) fn diarization_speaker(
        &self,
        session_id: MeetingSessionId,
        cluster: u32,
    ) -> Result<SpeakerId, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let speaker_id = named_speaker_for(
            &transaction,
            session_id,
            SourceKind::SystemAudio,
            &format!("Speaker {}", cluster.saturating_add(1)),
        )?;
        transaction.commit()?;
        Ok(speaker_id)
    }

    pub(crate) fn write_diarization_assignments(
        &self,
        generation_id: MeetingDiarizationGenerationId,
        assignments: &[DiarizationAssignmentInput],
    ) -> Result<(), StoreError> {
        if assignments.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let state: String = transaction.query_row(
            "SELECT state FROM meeting_diarization_generations WHERE generation_id = ?1",
            params![id(generation_id)],
            |row| row.get(0),
        )?;
        if state != "running" {
            return Err(StoreError::Conflict);
        }
        for assignment in assignments {
            transaction.execute(
                "INSERT INTO meeting_diarization_assignments (
                    generation_id, segment_id, speaker_id, assignment_kind
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(generation_id, segment_id) DO UPDATE SET
                    speaker_id = excluded.speaker_id,
                    assignment_kind = excluded.assignment_kind",
                params![
                    id(generation_id),
                    id(assignment.segment_id),
                    id(assignment.speaker_id),
                    speaker_assignment_to_db(assignment.assignment),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn publish_diarization_generation(
        &self,
        session_id: MeetingSessionId,
        generation_id: MeetingDiarizationGenerationId,
    ) -> Result<u64, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let (owner, transcript_revision, model_id, model_version): (
            String,
            String,
            String,
            String,
        ) = transaction.query_row(
            "SELECT session_id, transcript_revision_id, model_id, model_version
                 FROM meeting_diarization_generations
                 WHERE generation_id = ?1 AND state = 'running'",
            params![id(generation_id)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        if parse_uuid(&owner)? != session_id.uuid() {
            return Err(StoreError::Invalid);
        }
        let current: String = transaction.query_row(
            "SELECT current_transcript_revision_id FROM meeting_sessions WHERE id = ?1",
            params![id(session_id)],
            |row| row.get(0),
        )?;
        if current != transcript_revision {
            return Err(StoreError::Conflict);
        }
        // A generation that assigned nobody, over a transcript that has
        // segments, is not a diarization result: it is the absence of one, and
        // publishing it makes every segment speakerless. Both counts are taken
        // in the publishing transaction, so neither can race an assignment
        // write. The generation is left `failed` for the caller to report.
        let assigned: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM meeting_diarization_assignments WHERE generation_id = ?1",
            params![id(generation_id)],
            |row| row.get(0),
        )?;
        let segments: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM meeting_transcript_segments WHERE transcript_revision_id = ?1",
            params![&transcript_revision],
            |row| row.get(0),
        )?;
        if assigned == 0 && segments > 0 {
            transaction.execute(
                "UPDATE meeting_diarization_generations
                 SET state = 'failed', completed_at_utc_ms = ?1
                 WHERE generation_id = ?2",
                params![utc_now_ms(), id(generation_id)],
            )?;
            transaction.commit()?;
            return Err(StoreError::Invalid);
        }
        let session = session_row(&transaction, session_id)?;
        let next_revision = session.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
        transaction.execute(
            "UPDATE meeting_diarization_generations
             SET state = 'completed', completed_at_utc_ms = ?1
             WHERE generation_id = ?2",
            params![utc_now_ms(), id(generation_id)],
        )?;
        transaction.execute(
            "UPDATE meeting_sessions
             SET current_diarization_generation_id = ?1, diarization_status = 'succeeded',
                 diarization_model_id = ?2, diarization_model_version = ?3, revision = ?4
             WHERE id = ?5",
            params![
                id(generation_id),
                model_id,
                model_version,
                to_i64(next_revision)?,
                id(session_id),
            ],
        )?;
        mark_artifacts_out_of_date(&transaction, session_id)?;
        append_event(
            &transaction,
            session_id,
            next_revision,
            session.phase,
            session.phase,
            "diarization_published",
            None,
        )?;
        transaction.commit()?;
        Ok(next_revision)
    }

    pub(crate) fn artifact_by_generation_key(
        &self,
        session_id: MeetingSessionId,
        generation_key: &str,
    ) -> Result<Option<MeetingArtifactRevision>, StoreError> {
        let connection = self.connection()?;
        artifact_revision_for_key(&connection, session_id, generation_key)
    }

    pub(crate) fn store_artifact_revision(
        &self,
        input: ArtifactRevisionInput<'_>,
    ) -> Result<MeetingArtifactRevision, StoreError> {
        if matches!(input.state, MeetingArtifactState::Current) && input.content.is_none() {
            return Err(StoreError::Invalid);
        }
        let existing = self.artifact_by_generation_key(input.session_id, input.generation_key)?;
        if existing
            .as_ref()
            .is_some_and(|artifact| artifact.state != MeetingArtifactState::Failed)
        {
            return existing.ok_or(StoreError::Corrupt);
        }
        let artifact_id = existing
            .as_ref()
            .map(|artifact| artifact.artifact_id)
            .unwrap_or_else(MeetingArtifactId::new);
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let session = session_row(&transaction, input.session_id)?;
        let state = if input.state == MeetingArtifactState::Current
            && session.revision != input.input_revision
        {
            MeetingArtifactState::OutOfDate
        } else {
            input.state
        };
        let content = input.content.map(encode_json).transpose()?;
        transaction.execute(
            "INSERT INTO meeting_artifact_revisions (
                artifact_id, session_id, transcript_revision_id, input_revision, template_id,
                template_version, generation_key, state, content_json, generated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(session_id, generation_key) DO UPDATE SET
                transcript_revision_id = excluded.transcript_revision_id,
                input_revision = excluded.input_revision,
                template_id = excluded.template_id,
                template_version = excluded.template_version,
                state = excluded.state,
                content_json = excluded.content_json,
                generated_at_utc_ms = excluded.generated_at_utc_ms",
            params![
                id(artifact_id),
                id(input.session_id),
                id(input.transcript_revision_id),
                to_i64(input.input_revision)?,
                input.template_id,
                i64::from(input.template_version),
                input.generation_key,
                artifact_state_to_db(state),
                content,
                input.generated_at_utc_ms,
            ],
        )?;
        transaction.commit()?;
        /* The read below takes this same lock, and a `MutexGuard` lives to the
         * end of its scope rather than to its last use, so the write has to
         * hand the connection back before the row it just wrote can be read. */
        drop(connection);
        self.artifact_by_generation_key(input.session_id, input.generation_key)?
            .ok_or(StoreError::Corrupt)
    }

    pub(crate) fn search_evidence(
        &self,
        session_ids: &[MeetingSessionId],
        query: &str,
        limit: usize,
    ) -> Result<Vec<MeetingEvidence>, StoreError> {
        let Some(match_query) = meeting_fts_match_query(query) else {
            return Ok(Vec::new());
        };
        if session_ids.is_empty() {
            return Err(StoreError::Invalid);
        }
        let mut placeholders = String::new();
        for index in 0..session_ids.len() {
            if index > 0 {
                placeholders.push_str(", ");
            }
            placeholders.push('?');
            placeholders.push_str(&(index + 2).to_string());
        }
        let limit = limit.clamp(1, 50);
        let limit_parameter = session_ids.len() + 2;
        let sql = format!(
            "SELECT d.session_id, d.entity_kind, d.entity_id, d.content,
                    s.start_offset_ns, s.end_offset_ns, n.start_offset_ns, n.end_offset_ns
             FROM meeting_search_fts
             JOIN meeting_search_documents d ON d.id = meeting_search_fts.rowid
             LEFT JOIN meeting_sessions m ON m.id = d.session_id
             LEFT JOIN meeting_transcript_segments s
               ON d.entity_kind = 'segment' AND s.segment_id = d.entity_id
              AND s.transcript_revision_id = m.current_transcript_revision_id
             LEFT JOIN meeting_notes n
               ON d.entity_kind = 'note' AND n.note_id = d.entity_id
             WHERE meeting_search_fts MATCH ?1 AND d.session_id IN ({placeholders})
             ORDER BY bm25(meeting_search_fts)
             LIMIT ?{limit_parameter}"
        );
        let mut values = Vec::with_capacity(session_ids.len() + 2);
        values.push(rusqlite::types::Value::Text(match_query));
        values.extend(
            session_ids
                .iter()
                .map(|session_id| rusqlite::types::Value::Text(id(*session_id))),
        );
        values.push(rusqlite::types::Value::Integer(
            i64::try_from(limit).map_err(|_| StoreError::Invalid)?,
        ));
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values.iter()), |row| {
            let session_id = MeetingSessionId::from_uuid(
                parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
            );
            let entity_kind: String = row.get(1)?;
            let (kind, start, end) = match entity_kind.as_str() {
                "segment" => (
                    CitationKind::Transcript,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                ),
                "note" => (
                    CitationKind::ManualNote,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ),
                "title" => (CitationKind::Title, None, None),
                _ => return Err(to_sql_error(StoreError::Corrupt)),
            };
            let citation = MeetingCitation {
                kind,
                session_id,
                entity_id: row.get(2)?,
                start_offset_ns: start.map(from_i64).transpose().map_err(to_sql_error)?,
                end_offset_ns: end.map(from_i64).transpose().map_err(to_sql_error)?,
            };
            Ok(MeetingEvidence {
                citation,
                text: bounded_text(&row.get::<_, String>(3)?, 4_096),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn artifact_evidence(
        &self,
        session_id: MeetingSessionId,
        max_bytes: usize,
        default_template: MeetingNotesTemplate,
    ) -> Result<ArtifactEvidence, StoreError> {
        if max_bytes == 0 {
            return Err(StoreError::Invalid);
        }
        let connection = self.connection()?;
        // The user's own notes are read first and capped well below the total
        // budget, so a long note can never displace the transcript it steers.
        let notes = user_notes_row(&connection, session_id, default_template)?;
        let user_notes = bounded_text(notes.body.trim(), max_bytes / 8);
        let mut used = user_notes.len();
        let mut transcript = Vec::new();
        let mut segments = connection.prepare(
            "SELECT s.segment_id, s.start_offset_ns, s.end_offset_ns,
                    COALESCE((SELECT e.replacement_text FROM meeting_segment_edits e
                              WHERE e.segment_id = s.segment_id
                              ORDER BY e.edit_sequence DESC LIMIT 1), s.base_text)
             FROM meeting_sessions m
             JOIN meeting_transcript_segments s
               ON s.transcript_revision_id = m.current_transcript_revision_id
             WHERE m.id = ?1
               AND COALESCE((SELECT e.removed FROM meeting_segment_edits e
                             WHERE e.segment_id = s.segment_id
                             ORDER BY e.edit_sequence DESC LIMIT 1), 0) = 0
             ORDER BY s.start_offset_ns, s.ordinal",
        )?;
        let mut rows = segments.query(params![id(session_id)])?;
        while let Some(row) = rows.next()? {
            let text: String = row.get(3)?;
            if text.trim().is_empty() || used >= max_bytes {
                if used >= max_bytes {
                    break;
                }
                continue;
            }
            let remaining = max_bytes.saturating_sub(used);
            let text = bounded_text(&text, remaining);
            used = used.saturating_add(text.len());
            transcript.push(MeetingEvidence {
                citation: MeetingCitation {
                    kind: CitationKind::Transcript,
                    session_id,
                    entity_id: row.get(0)?,
                    start_offset_ns: Some(from_i64(row.get(1)?)?),
                    end_offset_ns: Some(from_i64(row.get(2)?)?),
                },
                text,
            });
        }
        drop(rows);
        drop(segments);
        let mut manual_notes = Vec::new();
        if used < max_bytes {
            let mut notes = connection.prepare(
                "SELECT note_id, start_offset_ns, end_offset_ns, body
                 FROM meeting_notes WHERE session_id = ?1 ORDER BY updated_at_utc_ms",
            )?;
            let mut rows = notes.query(params![id(session_id)])?;
            while let Some(row) = rows.next()? {
                if used >= max_bytes {
                    break;
                }
                let body: String = row.get(3)?;
                if body.trim().is_empty() {
                    continue;
                }
                let text = bounded_text(&body, max_bytes.saturating_sub(used));
                used = used.saturating_add(text.len());
                let start: Option<i64> = row.get(1)?;
                let end: Option<i64> = row.get(2)?;
                manual_notes.push(MeetingEvidence {
                    citation: MeetingCitation {
                        kind: CitationKind::ManualNote,
                        session_id,
                        entity_id: row.get(0)?,
                        start_offset_ns: start.map(from_i64).transpose()?,
                        end_offset_ns: end.map(from_i64).transpose()?,
                    },
                    text,
                });
            }
        }
        Ok(ArtifactEvidence {
            transcript,
            manual_notes,
            user_notes,
            template: notes.template,
        })
    }

    /// Every canonical speaker's display name for one meeting.
    ///
    /// The ledger pass needs the name twice: once in the pack, so the model
    /// can attribute a receipt to the person who said it, and once on the
    /// page its checks are run against, which otherwise labels everyone
    /// `Speaker 1a2b3c4d`. `review_snapshot` carries the same names and a
    /// whole meeting's transcript, notes and artifacts with them.
    pub(crate) fn speaker_display_names(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<HashMap<SpeakerId, String>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT speaker_id, display_name FROM meeting_speakers
             WHERE session_id = ?1",
        )?;
        let rows = statement.query_map(params![id(session_id)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut display_names = HashMap::new();
        for row in rows {
            let (speaker_id, display_name) = row?;
            display_names.insert(SpeakerId::from_uuid(parse_uuid(&speaker_id)?), display_name);
        }
        let canonical_ids = canonical_speaker_ids(&connection, session_id)?;
        let mut names = HashMap::new();
        for (&speaker_id, display_name) in &display_names {
            let canonical_id = canonical_ids
                .get(&speaker_id)
                .copied()
                .unwrap_or(speaker_id);
            let canonical_name = display_names
                .get(&canonical_id)
                .cloned()
                .unwrap_or_else(|| display_name.clone());
            names.insert(canonical_id, canonical_name);
        }
        Ok(names)
    }

    /// The diarized transcript reduced to what conversation metrics and
    /// trackers need. Removed segments are excluded because they are not part
    /// of the meeting any more.
    pub(crate) fn analytics_segments(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<AnalyticsSegment>, StoreError> {
        let connection = self.connection()?;
        analytics_segments_in(&connection, session_id)
    }

    /// The transcript captured so far, from the newest revision whether or not
    /// it has finished. Transcription runs after capture stops and appends
    /// segments as it goes, so this is the rolling buffer a catch-up reads.
    pub(crate) fn pending_transcript_evidence(
        &self,
        session_id: MeetingSessionId,
        max_bytes: usize,
    ) -> Result<Vec<MeetingEvidence>, StoreError> {
        if max_bytes == 0 {
            return Err(StoreError::Invalid);
        }
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT s.segment_id, s.start_offset_ns, s.end_offset_ns,
                    COALESCE((SELECT e.replacement_text FROM meeting_segment_edits e
                              WHERE e.segment_id = s.segment_id
                              ORDER BY e.edit_sequence DESC LIMIT 1), s.base_text)
             FROM meeting_transcript_segments s
             WHERE s.transcript_revision_id = (
                     SELECT r.transcript_revision_id FROM meeting_transcript_revisions r
                     WHERE r.session_id = ?1 ORDER BY r.created_at_utc_ms DESC LIMIT 1)
               AND COALESCE((SELECT e.removed FROM meeting_segment_edits e
                             WHERE e.segment_id = s.segment_id
                             ORDER BY e.edit_sequence DESC LIMIT 1), 0) = 0
             ORDER BY s.start_offset_ns, s.ordinal",
        )?;
        let mut rows = statement.query(params![id(session_id)])?;
        let mut evidence = Vec::new();
        let mut used = 0_usize;
        while let Some(row) = rows.next()? {
            if used >= max_bytes {
                break;
            }
            let text: String = row.get(3)?;
            if text.trim().is_empty() {
                continue;
            }
            let text = bounded_text(&text, max_bytes - used);
            used = used.saturating_add(text.len());
            evidence.push(MeetingEvidence {
                citation: MeetingCitation {
                    kind: CitationKind::Transcript,
                    session_id,
                    entity_id: row.get(0)?,
                    start_offset_ns: Some(from_i64(row.get(1)?)?),
                    end_offset_ns: Some(from_i64(row.get(2)?)?),
                },
                text,
            });
        }
        Ok(evidence)
    }

    /// Replace the derived metrics for a meeting. This is a disposable cache
    /// over the transcript, so an overwrite never needs an operation receipt.
    pub(crate) fn store_conversation_metrics(
        &self,
        session_id: MeetingSessionId,
        input_revision: u64,
        metrics: &MeetingAnalytics,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO meeting_conversation_metrics (
                session_id, input_revision, metrics_json, computed_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id) DO UPDATE SET
                input_revision = excluded.input_revision,
                metrics_json = excluded.metrics_json,
                computed_at_utc_ms = excluded.computed_at_utc_ms",
            params![
                id(session_id),
                to_i64(input_revision)?,
                encode_json(metrics)?,
                utc_now_ms(),
            ],
        )?;
        Ok(())
    }

    /// When the metrics were last derived, so a caller can tell a cached read
    /// from a fresh one.
    pub(crate) fn conversation_metrics_computed_at(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Option<i64>, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT computed_at_utc_ms FROM meeting_conversation_metrics WHERE session_id = ?1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn user_notes(
        &self,
        session_id: MeetingSessionId,
        default_template: MeetingNotesTemplate,
    ) -> Result<MeetingUserNotes, StoreError> {
        let connection = self.connection()?;
        user_notes_row(&connection, session_id, default_template)
    }

    /// Save the user's own notes layer. This deliberately bypasses the audited
    /// session-mutation path: notes autosave while a person types, and bumping
    /// the session revision on every keystroke burst would invalidate every
    /// other in-flight edit. Concurrency is guarded by the note revision alone.
    pub(crate) fn save_user_notes(
        &self,
        session_id: MeetingSessionId,
        body: &str,
        template: MeetingNotesTemplate,
        expected_revision: u64,
    ) -> Result<MeetingUserNotes, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        session_row(&transaction, session_id)?;
        let current: Option<i64> = transaction
            .query_row(
                "SELECT note_revision FROM meeting_user_notes WHERE session_id = ?1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .optional()?;
        let current_revision = current.map(from_i64).transpose()?.unwrap_or(0);
        if current_revision != expected_revision {
            return Err(StoreError::Conflict);
        }
        let next_revision = current_revision.checked_add(1).ok_or(StoreError::Corrupt)?;
        let updated_at_utc_ms = utc_now_ms();
        transaction.execute(
            "INSERT INTO meeting_user_notes (
                session_id, body, template_id, note_revision, updated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id) DO UPDATE SET
                body = excluded.body,
                template_id = excluded.template_id,
                note_revision = excluded.note_revision,
                updated_at_utc_ms = excluded.updated_at_utc_ms",
            params![
                id(session_id),
                body,
                template.artifact_template_id(),
                to_i64(next_revision)?,
                updated_at_utc_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(MeetingUserNotes {
            session_id,
            body: body.to_string(),
            template,
            revision: next_revision,
            updated_at_utc_ms,
        })
    }

    pub(crate) fn action_item_states(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<MeetingActionItemState>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT artifact_id, action_index, done FROM meeting_action_item_states
             WHERE session_id = ?1 ORDER BY artifact_id, action_index",
        )?;
        let rows = statement.query_map(params![id(session_id)], |row| {
            let action_index: i64 = row.get(1)?;
            Ok(MeetingActionItemState {
                artifact_id: MeetingArtifactId::from_uuid(
                    parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
                ),
                action_index: u32::try_from(action_index)
                    .map_err(|_| to_sql_error(StoreError::Corrupt))?,
                done: row.get::<_, i64>(2)? != 0,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Tick or untick one extracted action item. The state belongs to the
    /// generated revision that produced the item, which is why the artifact id
    /// is part of the key: regenerated notes start from an unticked list.
    pub(crate) fn set_action_item_done(
        &self,
        session_id: MeetingSessionId,
        artifact_id: MeetingArtifactId,
        action_index: u32,
        done: bool,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let action_count: i64 = transaction.query_row(
            "SELECT COALESCE(json_array_length(content_json, '$.action_items'), 0)
             FROM meeting_artifact_revisions WHERE artifact_id = ?1 AND session_id = ?2",
            params![id(artifact_id), id(session_id)],
            |row| row.get(0),
        )?;
        if i64::from(action_index) >= action_count {
            return Err(StoreError::Invalid);
        }
        transaction.execute(
            "INSERT INTO meeting_action_item_states (
                artifact_id, action_index, session_id, done, updated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(artifact_id, action_index) DO UPDATE SET
                done = excluded.done,
                updated_at_utc_ms = excluded.updated_at_utc_ms",
            params![
                id(artifact_id),
                i64::from(action_index),
                id(session_id),
                i64::from(done),
                utc_now_ms(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn fallback_system_speaker(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<SpeakerId, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let speaker_id = source_speaker_for(&transaction, session_id, SourceKind::SystemAudio)?;
        transaction.commit()?;
        Ok(speaker_id)
    }

    /// The receipt for one rewrite of a meeting's notes.
    ///
    /// `engine` is the id of the generator that actually wrote the revision —
    /// `apple-intelligence` or `sona-relay` — and it is recorded here, beside
    /// the artifact it produced, because D14 makes that a question with two
    /// answers. A reader auditing what left their machine needs the engine
    /// named by the operation that ran, not inferred afterwards from a setting
    /// that may have changed since.
    pub(crate) fn record_artifact_regeneration(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        session_id: MeetingSessionId,
        expected_revision: u64,
        artifact_id: MeetingArtifactId,
        engine: &str,
    ) -> Result<OperationReceipt, StoreError> {
        if let Some(receipt) = self.operation_receipt(operation_id)? {
            return Ok(receipt);
        }
        let mutation = StoreMutation {
            operation_id,
            actor: OperationActor::User,
            requested_at_utc_ms,
            session_id,
            expected_revision,
            command: MeetingCommandKind::ArtifactsRegenerate,
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current = session_row(&transaction, session_id)?;
        if current.revision != expected_revision {
            let receipt = rejected_receipt(
                mutation,
                current.phase,
                current.revision,
                MeetingReasonCode::StaleRevision,
            );
            insert_operation_receipt(&transaction, &receipt, requested_at_utc_ms)?;
            transaction.commit()?;
            return Ok(receipt);
        }
        if !matches!(
            current.phase,
            MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired
        ) {
            return Err(StoreError::Conflict);
        }
        let receipt = committed_receipt(
            mutation,
            current.phase,
            current.phase,
            requested_at_utc_ms,
            current.revision,
            vec![id(artifact_id), engine.to_string()],
        );
        insert_operation_receipt(&transaction, &receipt, requested_at_utc_ms)?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn reserve_deletion(
        &self,
        operation_id: MeetingOperationId,
        requested_at_utc_ms: i64,
        session_id: MeetingSessionId,
        expected_revision: u64,
        cause: DeletionCause,
    ) -> Result<(OperationReceipt, MeetingDeletionJobId), StoreError> {
        if let Some(receipt) = self.operation_receipt(operation_id)? {
            let job_id = receipt
                .effect_ids
                .first()
                .and_then(|value| parse_uuid(value).ok())
                .map(MeetingDeletionJobId::from_uuid)
                .ok_or(StoreError::Corrupt)?;
            return Ok((receipt, job_id));
        }
        let mutation = StoreMutation {
            operation_id,
            actor: OperationActor::User,
            requested_at_utc_ms,
            session_id,
            expected_revision,
            command: MeetingCommandKind::Delete,
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current = session_row(&transaction, session_id)?;
        if current.phase == MeetingPhase::Deleting {
            return Err(StoreError::Conflict);
        }
        if expected_revision != current.revision {
            let receipt = rejected_receipt(
                mutation,
                current.phase,
                current.revision,
                MeetingReasonCode::StaleRevision,
            );
            insert_operation_receipt(&transaction, &receipt, utc_now_ms())?;
            transaction.commit()?;
            return Ok((receipt, MeetingDeletionJobId::new()));
        }
        let next_revision = current.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
        let job_id = MeetingDeletionJobId::new();
        let now = utc_now_ms();
        let live_relative_path = session_id.uuid().to_string();
        let trash_relative_path = format!(".trash/{}", job_id.uuid());
        // What an undo would have to put back, captured here rather than in
        // `finish_deletion`: the phase this row is about to leave is one the
        // bundle accepts, and `deleting` is not. A meeting whose bundle cannot be
        // built is deleted the way this app always deleted — rows, then
        // directory, then a receipt with nothing to restore from — because
        // deleting must not fail over an undo that could not be prepared.
        let restore_bundle_json = match export_cloud_meeting_bundle_in(&transaction, session_id)
            .and_then(|bundle| bundle.validate().map(|()| bundle))
        {
            Ok(bundle) => encode_json(&bundle).ok(),
            Err(error) => {
                log::info!(
                    "Meeting {session_id:?} is being deleted without an undo: no restorable bundle ({error:?})"
                );
                None
            }
        };
        transaction.execute(
            "UPDATE meeting_sessions SET phase = 'deleting', revision = ?1 WHERE id = ?2",
            params![to_i64(next_revision)?, id(session_id)],
        )?;
        transaction.execute(
            "INSERT INTO meeting_deletion_jobs (
                job_id, session_id, cause, live_relative_path, trash_relative_path, state,
                created_at_utc_ms, updated_at_utc_ms, restore_bundle_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'reserved', ?6, ?6, ?7)",
            params![
                id(job_id),
                id(session_id),
                encode_json(&cause)?,
                live_relative_path,
                trash_relative_path,
                now,
                restore_bundle_json,
            ],
        )?;
        append_event(
            &transaction,
            session_id,
            next_revision,
            current.phase,
            MeetingPhase::Deleting,
            "deletion_reserved",
            None,
        )?;
        let receipt = committed_receipt(
            mutation,
            current.phase,
            MeetingPhase::Deleting,
            now,
            next_revision,
            vec![id(job_id)],
        );
        insert_operation_receipt(&transaction, &receipt, now)?;
        transaction.commit()?;
        Ok((receipt, job_id))
    }

    /// Finish one reserved deletion: the rows go, the audio moves to `.trash/`,
    /// and the receipt records what it would take to put both back.
    ///
    /// The directory is deliberately *not* removed here any more. A deletion is
    /// undoable for thirty days, so the job stops at the trash and
    /// [`MeetingStore::purge_expired_trash`] is what finally removes it.
    /// The bundle an undo needs was captured when the deletion was reserved; it
    /// moves to the receipt in the same transaction that deletes the rows, so a
    /// launch that dies between them cannot leave a trashed directory nothing
    /// can restore.
    ///
    /// A deletion with no bundle behind it — a meeting in a phase the bundle
    /// refuses, or a job reserved by an older build — is completed the way this
    /// app always completed one: rows, then directory, then a receipt with
    /// nothing to restore from.
    pub fn finish_deletion(&self, job_id: MeetingDeletionJobId) -> Result<Option<u64>, StoreError> {
        let job = match self.deletion_job(job_id) {
            Ok(job) => job,
            Err(StoreError::NotFound) => {
                if self.deletion_receipt_exists(job_id)? {
                    return Ok(None);
                }
                return Err(StoreError::NotFound);
            }
            Err(error) => return Err(error),
        };
        // The rows and the receipt commit together, so a job row that outlived
        // its own receipt is a launch that died in between: the deletion is
        // already done and only the job row is left to clear.
        if self.deletion_receipt_exists(job_id)? {
            self.delete_deletion_job(job_id)?;
            return Ok(None);
        }
        let live = validated_relative(&self.root, &job.live_relative_path)?;
        let trash = validated_relative(&self.root, &job.trash_relative_path)?;
        if live.exists() {
            if let Some(parent) = trash.parent() {
                ensure_private_directory(parent)?;
            }
            fs::rename(&live, &trash)?;
            self.update_deletion_job_state(job_id, "trashed")?;
        }
        let restorable = job.restore_bundle_json;
        let people_revision = {
            let mut connection = self.connection()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let affected_people: bool = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM meeting_person_links WHERE meeting_id = ?1
                 )",
                [&job.session_id],
                |row| row.get(0),
            )?;
            let session_id = MeetingSessionId::from_uuid(parse_uuid(&job.session_id)?);
            let voice_change =
                voice_identity::purge_session_voice_evidence_in(&transaction, session_id)?;
            transaction.execute(
                "DELETE FROM meeting_sessions WHERE id = ?1",
                params![job.session_id],
            )?;
            let people_revision = if affected_people || voice_change.people_changed() {
                Some(people::bump_people_revision_in(&transaction)?)
            } else {
                None
            };
            transaction.execute(
                "INSERT OR REPLACE INTO meeting_deletion_receipts (
                    job_id, cause, completed_at_utc_ms, trash_relative_path, restore_bundle_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    id(job_id),
                    job.cause_json,
                    utc_now_ms(),
                    restorable
                        .is_some()
                        .then_some(job.trash_relative_path.as_str()),
                    restorable.as_deref(),
                ],
            )?;
            transaction.commit()?;
            people_revision
        };
        if restorable.is_none() && trash.exists() {
            fs::remove_dir_all(&trash)?;
        }
        self.delete_deletion_job(job_id)?;
        Ok(people_revision)
    }

    fn deletion_receipt_exists(&self, job_id: MeetingDeletionJobId) -> Result<bool, StoreError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT 1 FROM meeting_deletion_receipts WHERE job_id = ?1",
                params![id(job_id)],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some())
    }

    fn delete_deletion_job(&self, job_id: MeetingDeletionJobId) -> Result<(), StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM meeting_deletion_jobs WHERE job_id = ?1",
            params![id(job_id)],
        )?;
        Ok(())
    }

    /// The meetings a person could still get back, newest deletion first.
    ///
    /// Only receipts that carry both halves of an undo are listed. A receipt
    /// from a build before the bin existed, one whose bundle could not be built,
    /// and one the sweep has already purged all say the same thing here: that
    /// deletion is final, so it is not offered.
    pub fn meeting_trash(&self, now_utc_ms: i64) -> Result<Vec<MeetingTrashEntry>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT job_id, completed_at_utc_ms,
                    json_extract(restore_bundle_json, '$.session.title') AS title
               FROM meeting_deletion_receipts
              WHERE restore_bundle_json IS NOT NULL AND trash_relative_path IS NOT NULL
              ORDER BY completed_at_utc_ms DESC, job_id DESC",
        )?;
        let entries = statement
            .query_map([], |row| {
                let job_id: String = row.get(0)?;
                let deleted_at_utc_ms: i64 = row.get(1)?;
                Ok(MeetingTrashEntry {
                    job_id: MeetingDeletionJobId::from_uuid(
                        parse_uuid(&job_id).map_err(to_sql_error)?,
                    ),
                    title: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    deleted_at_utc_ms,
                    expires_at_utc_ms: deleted_at_utc_ms.saturating_add(TRASH_RETENTION_MS),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.expires_at_utc_ms > now_utc_ms)
            .collect())
    }

    /// Put one trashed meeting back: its audio directory, then its rows.
    ///
    /// The bundle is imported through the same path a meeting arriving from
    /// another Mac takes, which restores everything the database knew except the
    /// durable record index — the per-record offsets into the audio files, which
    /// the bundle deliberately does not carry. [`Self::repair_session_tracks`]
    /// rebuilds exactly that by reading the files back, which is the same work
    /// startup does for a meeting a crash left mid-capture.
    ///
    /// Refuses with `NotFound` once the sweep has purged the entry: after that
    /// there is nothing on disk to move back, and a row restored without its
    /// audio would be a meeting that cannot be played or reprocessed.
    pub fn restore_trashed_meeting(
        &self,
        job_id: MeetingDeletionJobId,
        now_utc_ms: i64,
    ) -> Result<MeetingSessionId, StoreError> {
        let (trash_relative_path, bundle_json, deleted_at_utc_ms) = self
            .connection()?
            .query_row(
                "SELECT trash_relative_path, restore_bundle_json, completed_at_utc_ms
                   FROM meeting_deletion_receipts WHERE job_id = ?1",
                params![id(job_id)],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        let (Some(trash_relative_path), Some(bundle_json)) = (trash_relative_path, bundle_json)
        else {
            return Err(StoreError::NotFound);
        };
        if deleted_at_utc_ms.saturating_add(TRASH_RETENTION_MS) <= now_utc_ms {
            return Err(StoreError::NotFound);
        }
        let bundle: cloud_bundle::CloudMeetingBundleV1 = decode_json(&bundle_json)?;
        let session_id = bundle.session.session_id;
        let trash = validated_relative(&self.root, &trash_relative_path)?;
        let live = validated_relative(&self.root, &session_id.uuid().to_string())?;
        if trash.exists() && !live.exists() {
            fs::rename(&trash, &live)?;
        }
        self.import_cloud_meeting_bundle(&bundle)?;
        // The audio index is the one thing the bundle does not carry.
        self.repair_session_tracks(session_id)?;
        let connection = self.connection()?;
        connection.execute(
            "UPDATE meeting_deletion_receipts
                SET trash_relative_path = NULL, restore_bundle_json = NULL
              WHERE job_id = ?1",
            params![id(job_id)],
        )?;
        Ok(session_id)
    }

    /// Drop every trashed meeting past its thirty days, and forget what it
    /// would have taken to restore it.
    ///
    /// The receipt row itself stays: it is the record that this meeting was
    /// deleted and when, and that record has no expiry. What expires is the
    /// undo — the directory on disk and the bundle beside it.
    ///
    /// Returns how many entries it purged, so the sweep that calls it can say so
    /// in one line.
    pub fn purge_expired_trash(&self, now_utc_ms: i64) -> Result<usize, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT job_id, trash_relative_path FROM meeting_deletion_receipts
              WHERE trash_relative_path IS NOT NULL AND completed_at_utc_ms <= ?1",
        )?;
        let expired = statement
            .query_map(
                params![now_utc_ms.saturating_sub(TRASH_RETENTION_MS)],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (job_id, trash_relative_path) in &expired {
            let trash = validated_relative(&self.root, trash_relative_path)?;
            if trash.exists() {
                fs::remove_dir_all(&trash)?;
            }
            connection.execute(
                "UPDATE meeting_deletion_receipts
                    SET trash_relative_path = NULL, restore_bundle_json = NULL
                  WHERE job_id = ?1",
                params![job_id],
            )?;
        }
        Ok(expired.len())
    }

    pub fn resume_deletions(&self) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT job_id FROM meeting_deletion_jobs ORDER BY created_at_utc_ms")?;
        let job_ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        for job_id in job_ids {
            self.finish_deletion(MeetingDeletionJobId::from_uuid(parse_uuid(&job_id)?))?;
        }
        Ok(())
    }

    /// Reconcile every meeting a previous launch left mid-flight, so that no
    /// row can advertise processing that no job is doing.
    ///
    /// Three shapes need it. A live phase means the launch ended mid-capture
    /// or mid-processing: the phase moves to `recovery_required` and the
    /// processing status becomes terminal in the same transaction, because a
    /// phase that parks the meeting for a human while the status still reads
    /// `pending` is the state that showed "Processing" forever. A row already
    /// parked in `recovery_required` with a non-terminal status is the same
    /// state left by launches that flipped the phase alone; it is healed
    /// without touching the phase, the revision, or the event log, since
    /// nothing about the meeting changed — only what was always true about it
    /// is now written down. A row still sitting at its start gate is the
    /// third, and it is discarded outright. Terminal statuses are fixpoints,
    /// and a discarded row is gone, so a second pass in the same launch
    /// changes nothing.
    pub fn recover_interrupted(&self) -> Result<InterruptedRecovery, StoreError> {
        self.resume_deletions()?;
        let discarded = self.discard_abandoned_preflights()?;
        let status_resolved = self.resolve_abandoned_recovery_status()?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id FROM meeting_sessions WHERE phase IN (
                'starting', 'capturing_recording', 'capturing_pausing', 'capturing_paused',
                'capturing_resuming', 'stopping', 'processing'
             )",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        let interrupted = encode_json(&ProcessingStatus::Failed {
            reason: ProcessingFailure::Interrupted,
            cause: None,
        })?;
        let mut recovered = Vec::new();
        for value in ids {
            let session_id = MeetingSessionId::from_uuid(parse_uuid(&value)?);
            self.repair_session_tracks(session_id)?;
            let mut connection = self.connection()?;
            let transaction = connection.transaction()?;
            let current = session_row(&transaction, session_id)?;
            let next_revision = current.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
            transaction.execute(
                "UPDATE meeting_sessions
                 SET phase = 'recovery_required', revision = ?1, recovered_at_utc_ms = ?2,
                     processing_status = ?3
                 WHERE id = ?4",
                params![
                    to_i64(next_revision)?,
                    utc_now_ms(),
                    interrupted,
                    id(session_id)
                ],
            )?;
            // The phase the launch died in is the only surviving discriminator
            // between a meeting whose audio was still being written and one
            // that was already past the stop, and it is gone the moment this
            // transaction commits. The event ledger is where it keeps.
            append_event_with_details(
                &transaction,
                session_id,
                next_revision,
                current.phase,
                MeetingPhase::RecoveryRequired,
                "recovery_required",
                None,
                &recovery_details(current.phase),
            )?;
            transaction.commit()?;
            recovered.push(RecoveredMeeting {
                session_id,
                prior_phase: current.phase,
            });
        }
        Ok(InterruptedRecovery {
            recovered,
            status_resolved,
            discarded,
        })
    }

    /// Meetings still parked at their start gate, deleted outright. Returned
    /// so the caller can tell the windows those rows are gone.
    ///
    /// A preflight row is the draft the gate writes before anyone consents to
    /// recording: no consent, no run plan, no track, no audio, and a title
    /// nobody typed. Leaving the gate deletes it, which is why it is the one
    /// phase with a cancel and no stop. It outlives its gate only when the
    /// launch ends while the gate is still open, and then it is a row in the
    /// history of meetings that never happened — born `pending`, so both the
    /// Processing filter and the row chip read it as work in flight, forever.
    /// This is the cancel the closing window never sent, so it deletes the
    /// row the same way [`Self::cancel_preflight`] does. Nothing is lost that
    /// pressing Start does not rebuild, and a row the person can still return
    /// to is one whose gate is open in this launch, which no startup pass can
    /// be looking at.
    fn discard_abandoned_preflights(&self) -> Result<Vec<MeetingSessionId>, StoreError> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT id FROM meeting_sessions WHERE phase = 'preflight'")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|value| parse_uuid(&value).map(MeetingSessionId::from_uuid))
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        if ids.is_empty() {
            return Ok(ids);
        }
        connection.execute("DELETE FROM meeting_sessions WHERE phase = 'preflight'", [])?;
        Ok(ids)
    }

    /// Meetings parked in `recovery_required` whose processing status never
    /// reached a terminal value, healed in one statement. Returned so the
    /// caller can tell the windows their rows changed.
    fn resolve_abandoned_recovery_status(&self) -> Result<Vec<MeetingSessionId>, StoreError> {
        const ABANDONED: &str = "phase = 'recovery_required'
             AND json_extract(processing_status, '$.kind') IN ('pending', 'running')";
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT id FROM meeting_sessions WHERE {ABANDONED}"
        ))?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|value| parse_uuid(&value).map(MeetingSessionId::from_uuid))
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        if ids.is_empty() {
            return Ok(ids);
        }
        connection.execute(
            &format!("UPDATE meeting_sessions SET processing_status = ?1 WHERE {ABANDONED}"),
            params![encode_json(&ProcessingStatus::Failed {
                reason: ProcessingFailure::Interrupted,
                cause: None,
            })?],
        )?;
        Ok(ids)
    }

    /// True when a track of this meeting has lost its records on disk. The
    /// repair pass writes a `MissingRecord` gap for exactly that, and it is
    /// the one shape automatic reprocessing must leave alone: a transcript
    /// rebuilt from the tracks that survived would quietly replace the
    /// meeting with a fraction of itself, which is a decision for the person
    /// who was in the room.
    pub fn has_missing_record_gap(&self, session_id: MeetingSessionId) -> Result<bool, StoreError> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*)
               FROM meeting_source_gaps g
               JOIN meeting_source_tracks t ON t.track_id = g.track_id
              WHERE t.session_id = ?1 AND g.reason = ?2",
            params![
                id(session_id),
                encode_json(&SourceGapReason::MissingRecord)?
            ],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    fn edit_session<F>(
        &self,
        mutation: StoreMutation,
        event_kind: &str,
        edit: F,
    ) -> Result<OperationReceipt, StoreError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<(), StoreError>,
    {
        if let Some(receipt) = self.operation_receipt(mutation.operation_id)? {
            return Ok(receipt);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current = session_row(&transaction, mutation.session_id)?;
        if let Some(receipt) = validate_mutation(
            &transaction,
            mutation,
            &current,
            &[
                MeetingPhase::CapturingRecording,
                MeetingPhase::CapturingPaused,
                MeetingPhase::Processing,
                MeetingPhase::ReviewReady,
                MeetingPhase::RecoveryRequired,
            ],
        )? {
            transaction.commit()?;
            return Ok(receipt);
        }
        edit(&transaction)?;
        let next_revision = current.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
        let now = utc_now_ms();
        transaction.execute(
            "UPDATE meeting_sessions SET revision = ?1 WHERE id = ?2",
            params![to_i64(next_revision)?, id(mutation.session_id)],
        )?;
        append_event(
            &transaction,
            mutation.session_id,
            next_revision,
            current.phase,
            current.phase,
            event_kind,
            None,
        )?;
        let receipt = committed_receipt(
            mutation,
            current.phase,
            current.phase,
            now,
            next_revision,
            Vec::new(),
        );
        insert_operation_receipt(&transaction, &receipt, now)?;
        transaction.commit()?;
        Ok(receipt)
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection.lock().map_err(|_| StoreError::Unavailable)
    }

    fn ensure_session_directory(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<PathBuf, StoreError> {
        let path = validated_relative(&self.root, &session_id.uuid().to_string())?;
        ensure_private_directory(&path)?;
        Ok(path)
    }

    /// Streams independently authenticated durable records in sequence order,
    /// starting after `after_sequence` — `None` for the whole track.
    /// The metadata lookup releases the database mutex before decryption and
    /// callback work, so callers can checkpoint transcript batches without
    /// retaining a meeting-sized PCM buffer.
    ///
    /// A mark is what lets the provisional pass over a running capture read
    /// only the records that arrived since its last one. Records are committed
    /// after their bytes are synced, so a committed record is always readable
    /// here even while the writer is still appending to the same file.
    pub(crate) fn visit_durable_track_records<F>(
        &self,
        session_id: MeetingSessionId,
        track_id: SourceTrackId,
        after_sequence: Option<u64>,
        mut visitor: F,
    ) -> Result<(), StoreError>
    where
        F: FnMut(DurableTrackRecord) -> Result<(), StoreError>,
    {
        {
            let connection = self.connection()?;
            let owner: String = connection.query_row(
                "SELECT session_id FROM meeting_source_tracks WHERE track_id = ?1",
                params![id(track_id)],
                |row| row.get(0),
            )?;
            if parse_uuid(&owner)? != session_id.uuid() {
                return Err(StoreError::Invalid);
            }
        }
        let files = self.track_files(session_id, track_id);
        let key = self.track_key(session_id, track_id)?;
        let mut file = File::open(&files.records)?;
        let mut after_sequence = after_sequence;

        loop {
            let descriptor = {
                let connection = self.connection()?;
                connection
                    .query_row(
                        "SELECT source_sequence, source_epoch, start_offset_ns, duration_ns,
                                frame_count, record_offset_bytes, record_bytes
                         FROM meeting_track_records
                         WHERE track_id = ?1 AND source_sequence > ?2
                         ORDER BY source_sequence LIMIT 1",
                        params![
                            id(track_id),
                            after_sequence.map(to_i64).transpose()?.unwrap_or(-1_i64),
                        ],
                        |row| {
                            Ok(DurableRecordDescriptor {
                                sequence: from_i64(row.get(0)?).map_err(to_sql_error)?,
                                epoch: from_i64(row.get(1)?).map_err(to_sql_error)?,
                                start_offset_ns: row
                                    .get::<_, Option<i64>>(2)?
                                    .map(from_i64)
                                    .transpose()
                                    .map_err(to_sql_error)?
                                    .ok_or_else(|| to_sql_error(StoreError::Corrupt))?,
                                duration_ns: from_i64(row.get(3)?).map_err(to_sql_error)?,
                                frame_count: u32::try_from(row.get::<_, i64>(4)?)
                                    .map_err(|_| to_sql_error(StoreError::Corrupt))?,
                                record_offset: from_i64(row.get(5)?).map_err(to_sql_error)?,
                                record_bytes: from_i64(row.get(6)?).map_err(to_sql_error)?,
                            })
                        },
                    )
                    .optional()?
            };
            let Some(descriptor) = descriptor else {
                break;
            };
            let record = decrypt_durable_record(&mut file, &key, track_id, descriptor)?;
            after_sequence = Some(record.sequence);
            visitor(record)?;
        }
        Ok(())
    }

    fn track_files(&self, session_id: MeetingSessionId, track_id: SourceTrackId) -> TrackFiles {
        let session = self.root.join(session_id.uuid().to_string());
        let tracks = session.join("tracks");
        let stem = track_id.uuid().to_string();
        TrackFiles {
            records: tracks.join(format!("{stem}.records")),
            index: tracks.join(format!("{stem}.index")),
        }
    }

    fn track_key(
        &self,
        session_id: MeetingSessionId,
        track_id: SourceTrackId,
    ) -> Result<Zeroizing<[u8; 32]>, StoreError> {
        let hkdf = Hkdf::<Sha256>::new(Some(b"sona-meeting-track-v1"), self.master_key.as_bytes());
        let mut info = [0_u8; 32];
        info[..16].copy_from_slice(session_id.uuid().as_bytes());
        info[16..].copy_from_slice(track_id.uuid().as_bytes());
        let mut key = Zeroizing::new([0_u8; 32]);
        hkdf.expand(&info, &mut *key)
            .map_err(|_| StoreError::Corrupt)?;
        Ok(key)
    }

    fn track_row(&self, track_id: SourceTrackId) -> Result<TrackRow, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT session_id, timestamp_bridge_json
                 FROM meeting_source_tracks WHERE track_id = ?1",
                params![id(track_id)],
                |row| {
                    Ok(TrackRow {
                        session_id: MeetingSessionId::from_uuid(
                            parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
                        ),
                        bridge: decode_json(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
                    })
                },
            )
            .map_err(Into::into)
    }

    fn deletion_job(&self, job_id: MeetingDeletionJobId) -> Result<DeletionJobRow, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT session_id, cause, live_relative_path, trash_relative_path,
                        restore_bundle_json
                 FROM meeting_deletion_jobs WHERE job_id = ?1",
                params![id(job_id)],
                |row| {
                    Ok(DeletionJobRow {
                        session_id: row.get(0)?,
                        cause_json: row.get(1)?,
                        live_relative_path: row.get(2)?,
                        trash_relative_path: row.get(3)?,
                        restore_bundle_json: row.get(4)?,
                    })
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    fn update_deletion_job_state(
        &self,
        job_id: MeetingDeletionJobId,
        state: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE meeting_deletion_jobs SET state = ?1, updated_at_utc_ms = ?2 WHERE job_id = ?3",
            params![state, utc_now_ms(), id(job_id)],
        )?;
        Ok(())
    }

    fn repair_session_tracks(&self, session_id: MeetingSessionId) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT track_id FROM meeting_source_tracks WHERE session_id = ?1 ORDER BY track_id",
        )?;
        let tracks = statement
            .query_map(params![id(session_id)], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        for track in tracks {
            let track_id = SourceTrackId::from_uuid(parse_uuid(&track)?);
            let files = self.track_files(session_id, track_id);
            if !files.records.exists() || !files.index.exists() {
                self.record_gap(&SourceGap {
                    track_id,
                    epoch: SourceEpoch::new(0),
                    start_offset_ns: None,
                    end_offset_ns: None,
                    reason: SourceGapReason::MissingRecord,
                    dropped_frames: None,
                })?;
                continue;
            }
            let key = self.track_key(session_id, track_id)?;
            let repaired = repair_track_files(&files, &key, track_id)?;
            self.reconcile_repaired_records(track_id, &repaired)?;
            if repaired.truncated {
                self.record_gap(&SourceGap {
                    track_id,
                    epoch: SourceEpoch::new(repaired.last_epoch.unwrap_or(0)),
                    start_offset_ns: repaired.last_end_offset_ns,
                    end_offset_ns: None,
                    reason: SourceGapReason::RecoveryTail,
                    dropped_frames: None,
                })?;
            }
        }
        Ok(())
    }

    fn reconcile_repaired_records(
        &self,
        track_id: SourceTrackId,
        repaired: &RepairResult,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let mut statement = transaction
            .prepare("SELECT source_sequence FROM meeting_track_records WHERE track_id = ?1")?;
        let existing = statement
            .query_map(params![id(track_id)], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for record in &repaired.records {
            if !existing.contains(&to_i64(record.sequence)?) {
                insert_durable_record(&transaction, track_id, record)?;
            }
        }
        transaction.execute(
            "DELETE FROM meeting_track_records WHERE track_id = ?1 AND source_sequence > ?2",
            params![
                id(track_id),
                to_i64(
                    repaired
                        .records
                        .last()
                        .map(|record| record.sequence)
                        .unwrap_or(0)
                )?
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

pub struct MeetingTrackWriter {
    store: Arc<MeetingStore>,
    track_id: SourceTrackId,
    files: TrackFiles,
    records: File,
    index: File,
    key: Zeroizing<[u8; 32]>,
    pending: Vec<PendingRecord>,
    next_sequence: u64,
    durable_end_offset_ns: Option<u64>,
    checkpoint_interval_ns: u64,
}

impl MeetingTrackWriter {
    fn open(
        store: Arc<MeetingStore>,
        track_id: SourceTrackId,
        files: TrackFiles,
        key: Zeroizing<[u8; 32]>,
        plan: MeetingStoragePlan,
    ) -> Result<Self, StoreError> {
        let tracks_directory = files.records.parent().ok_or(StoreError::Invalid)?;
        ensure_private_directory(tracks_directory)?;
        let records = open_private_append_file(&files.records)?;
        let index = open_private_append_file(&files.index)?;
        let next_sequence = store.next_track_sequence(track_id)?;
        Ok(Self {
            store,
            track_id,
            files,
            records,
            index,
            key,
            pending: Vec::new(),
            next_sequence,
            durable_end_offset_ns: None,
            checkpoint_interval_ns: u64::from(plan.checkpoint_interval_ms)
                .saturating_mul(1_000_000),
        })
    }

    fn latest_end_offset_ns(&self) -> Option<u64> {
        self.pending
            .last()
            .and_then(|record| record.end_offset_ns)
            .or(self.durable_end_offset_ns)
    }

    pub fn accept(
        &mut self,
        packet: CapturedPacket,
        samples: &[f32],
    ) -> Result<PacketPushResult, StoreError> {
        let bridge = self.store.track_row(self.track_id)?.bridge;
        self.accept_with_bridge(packet, samples, bridge)
    }

    pub fn accept_with_bridge(
        &mut self,
        packet: CapturedPacket,
        samples: &[f32],
        bridge: TimestampBridge,
    ) -> Result<PacketPushResult, StoreError> {
        let next_packet_sequence = packet.sequence.checked_add(1).ok_or(StoreError::Corrupt)?;
        if packet.track_id != self.track_id
            || packet.sample_rate_hz == 0
            || packet.channels == 0
            || packet.format().checked_frame_samples(packet.frame_count) != Some(samples.len())
        {
            self.store.record_gap(&SourceGap {
                track_id: self.track_id,
                epoch: packet.source_epoch,
                start_offset_ns: self.latest_end_offset_ns(),
                end_offset_ns: None,
                reason: SourceGapReason::InvalidFormat,
                dropped_frames: Some(u64::from(packet.frame_count)),
            })?;
            if packet.track_id == self.track_id {
                self.next_sequence = self.next_sequence.max(next_packet_sequence);
            }
            return Ok(PacketPushResult::Dropped {
                frames: packet.frame_count,
            });
        }
        if packet.sequence < self.next_sequence {
            return Ok(PacketPushResult::Dropped {
                frames: packet.frame_count,
            });
        }

        let skipped_source_packet = packet.sequence > self.next_sequence;
        let duration_ns = u64::from(packet.frame_count)
            .checked_mul(1_000_000_000)
            .and_then(|frames| frames.checked_div(u64::from(packet.sample_rate_hz)))
            .ok_or(StoreError::Invalid)?;
        let previous_end_offset_ns = self.latest_end_offset_ns();
        let clock = SessionClock::new(SessionClockAnchor {
            host_monotonic_anchor_ns: bridge.host_monotonic_anchor_ns,
            wall_start_utc_ms: 0,
            clock_policy_version: 1,
        });
        let start_offset_ns = clock
            .map_packet(bridge, packet)
            .unwrap_or_else(|| previous_end_offset_ns.unwrap_or(bridge.session_offset_ns));

        let has_source_boundary = packet.discontinuity_flags.timestamp_reset
            || packet.discontinuity_flags.source_restarted
            || packet.discontinuity_flags.route_changed;
        if !skipped_source_packet && !has_source_boundary {
            if let Some(previous_end_offset_ns) = previous_end_offset_ns {
                let tolerance_ns =
                    (duration_ns / 2).max(1_000_000_000 / u64::from(packet.sample_rate_hz));
                let drift_ns = start_offset_ns.abs_diff(previous_end_offset_ns);
                if drift_ns > tolerance_ns {
                    let dropped_frames = (start_offset_ns > previous_end_offset_ns).then(|| {
                        drift_ns.saturating_mul(u64::from(packet.sample_rate_hz)) / 1_000_000_000
                    });
                    self.store.record_gap(&SourceGap {
                        track_id: self.track_id,
                        epoch: packet.source_epoch,
                        start_offset_ns: Some(previous_end_offset_ns.min(start_offset_ns)),
                        end_offset_ns: Some(previous_end_offset_ns.max(start_offset_ns)),
                        reason: SourceGapReason::TimestampDiscontinuity,
                        dropped_frames,
                    })?;
                }
            }
        }

        let payload = f32_payload(samples);
        let record_offset = self.records.seek(SeekFrom::End(0))?;
        let pending = write_encrypted_record(
            &mut self.records,
            &mut self.index,
            &self.key,
            self.track_id,
            packet.sequence,
            packet.source_epoch,
            start_offset_ns,
            duration_ns,
            packet.frame_count,
            packet.format(),
            packet.discontinuity_flags,
            &payload,
            record_offset,
        )?;
        self.next_sequence = next_packet_sequence;
        self.pending.push(pending);
        // The interval is measured from the last durable end, and from the
        // first pending record when nothing is durable yet. Without that
        // second case the comparison has no left-hand side on a fresh track,
        // so the first checkpoint never arrives and the whole meeting is held
        // in memory until the seal — which is also what left an interrupted
        // capture with no committed records to recover from, and a running
        // capture with nothing on disk for a mid-meeting reader.
        let measured_from_ns = self.durable_end_offset_ns.or_else(|| {
            self.pending
                .first()
                .and_then(|record| record.start_offset_ns)
        });
        let should_checkpoint = self
            .pending
            .last()
            .and_then(|record| record.end_offset_ns)
            .zip(measured_from_ns)
            .is_some_and(|(end, from)| end.saturating_sub(from) >= self.checkpoint_interval_ns);
        if should_checkpoint {
            self.checkpoint()?;
        }
        Ok(PacketPushResult::Accepted)
    }

    pub fn checkpoint(&mut self) -> Result<(), StoreError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.records.sync_data()?;
        self.index.sync_data()?;
        self.store
            .commit_durable_records(self.track_id, &self.pending)?;
        self.durable_end_offset_ns = self.pending.last().and_then(|record| record.end_offset_ns);
        self.pending.clear();
        Ok(())
    }

    pub fn seal(mut self) -> Result<(), StoreError> {
        self.checkpoint()?;
        self.records.sync_all()?;
        self.index.sync_all()?;
        sync_parent_directory(&self.files.records)?;
        Ok(())
    }

    pub fn abandon(mut self, reason: SourceGapReason) -> Result<(), StoreError> {
        self.checkpoint()?;
        self.store.record_gap(&SourceGap {
            track_id: self.track_id,
            epoch: SourceEpoch::new(0),
            start_offset_ns: self.durable_end_offset_ns,
            end_offset_ns: None,
            reason,
            dropped_frames: None,
        })
    }
}

impl MeetingStore {
    fn next_track_sequence(&self, track_id: SourceTrackId) -> Result<u64, StoreError> {
        let connection = self.connection()?;
        let sequence: i64 = connection.query_row(
            "SELECT COALESCE(MAX(source_sequence), -1) + 1 FROM meeting_track_records WHERE track_id = ?1",
            params![id(track_id)],
            |row| row.get(0),
        )?;
        u64::try_from(sequence).map_err(|_| StoreError::Corrupt)
    }

    fn commit_durable_records(
        &self,
        track_id: SourceTrackId,
        pending: &[PendingRecord],
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for record in pending {
            insert_durable_record(&transaction, track_id, record)?;
        }
        let first = pending.first().ok_or(StoreError::Invalid)?;
        let last = pending.last().ok_or(StoreError::Invalid)?;
        transaction.execute(
            "INSERT INTO meeting_track_checkpoints (
                track_id, next_sequence, durable_offset_ns, durable_bytes, updated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(track_id) DO UPDATE SET
                next_sequence = excluded.next_sequence,
                durable_offset_ns = excluded.durable_offset_ns,
                durable_bytes = excluded.durable_bytes,
                updated_at_utc_ms = excluded.updated_at_utc_ms",
            params![
                id(track_id),
                to_i64(last.sequence.checked_add(1).ok_or(StoreError::Corrupt)?)?,
                optional_i64(last.end_offset_ns)?,
                to_i64(
                    last.record_offset
                        .checked_add(last.record_bytes)
                        .ok_or(StoreError::Corrupt)?
                )?,
                utc_now_ms(),
            ],
        )?;
        transaction.execute(
            "UPDATE meeting_source_tracks
             SET first_offset_ns = COALESCE(first_offset_ns, ?1), last_offset_ns = ?2
             WHERE track_id = ?3",
            params![
                optional_i64(first.start_offset_ns)?,
                optional_i64(last.end_offset_ns)?,
                id(track_id),
            ],
        )?;
        // A track earns `Healthy` when its audio reaches the disk, and nothing
        // else in this file can grant it: `create_track` writes `starting` and
        // the only other writer records a failure. The promotion rides the same
        // transaction as the records that earn it, so health can never disagree
        // with the durable checkpoint, and the `starting` predicate makes it
        // fire once per track without overwriting a recorded failure.
        let promoted = transaction.execute(
            "UPDATE meeting_source_tracks SET health = ?1
             WHERE track_id = ?2 AND health = ?3",
            params![
                encode_json(&SourceHealth::Healthy)?,
                id(track_id),
                encode_json(&SourceHealth::Starting)?,
            ],
        )?;
        if promoted > 0 {
            // The session clock starts before any device does. This lane's
            // audio begins at its first record, so the interval before it is
            // missing from the meeting and the ledger should say so once, here,
            // where that offset is first known. It is written directly rather
            // than through `record_gap`: this row must not coalesce with a
            // later loss, and startup latency is not a failing lane.
            if let Some(start_offset_ns) = first.start_offset_ns.filter(|offset| *offset > 0) {
                transaction.execute(
                    "INSERT INTO meeting_source_gaps (
                        track_id, source_epoch, start_offset_ns, end_offset_ns, reason,
                        dropped_frames, observed_at_utc_ms, details_json
                     ) VALUES (?1, ?2, 0, ?3, ?4, NULL, ?5, '{}')",
                    params![
                        id(track_id),
                        to_i64(first.epoch)?,
                        to_i64(start_offset_ns)?,
                        encode_json(&SourceGapReason::SourceUnavailable)?,
                        utc_now_ms(),
                    ],
                )?;
            }
            let session_id = track_session(&transaction, track_id)?;
            note_health_change(&transaction, session_id, SourceHealth::Healthy)?;
        }
        transaction.commit()?;
        Ok(())
    }
}

struct SessionRow {
    phase: MeetingPhase,
    revision: u64,
    title: String,
    preflight_json: String,
    processing_status_json: String,
    retention_policy_json: String,
    started_at_utc_ms: Option<i64>,
    delete_after_utc_ms: Option<i64>,
}

struct TrackRow {
    session_id: MeetingSessionId,
    bridge: TimestampBridge,
}

struct DeletionJobRow {
    session_id: String,
    cause_json: String,
    live_relative_path: String,
    trash_relative_path: String,
    /// The portable meeting snapshot an undo would import, captured when the
    /// deletion was reserved. `None` for a meeting in a phase the bundle refuses
    /// and for jobs reserved by a build before the undo bin existed.
    restore_bundle_json: Option<String>,
}

#[derive(Clone)]
struct TrackFiles {
    records: PathBuf,
    index: PathBuf,
}

#[derive(Clone)]
struct PendingRecord {
    sequence: u64,
    epoch: u64,
    start_offset_ns: Option<u64>,
    end_offset_ns: Option<u64>,
    duration_ns: u64,
    frame_count: u32,
    record_offset: u64,
    record_bytes: u64,
}

#[derive(Clone, Copy)]
struct DurableRecordDescriptor {
    sequence: u64,
    epoch: u64,
    start_offset_ns: u64,
    duration_ns: u64,
    frame_count: u32,
    record_offset: u64,
    record_bytes: u64,
}

struct RepairResult {
    records: Vec<PendingRecord>,
    truncated: bool,
    last_epoch: Option<u64>,
    last_end_offset_ns: Option<u64>,
}

fn open_encrypted_connection(
    path: &Path,
    key: &MeetingStorageKey,
) -> Result<Connection, StoreError> {
    let connection = Connection::open(path)?;
    let key_hex = Zeroizing::new(hex::encode(key.as_bytes()));
    let pragma = Zeroizing::new(format!("PRAGMA key = \"x'{}'\";", key_hex.as_str()));
    connection.execute_batch(pragma.as_str())?;
    let cipher_version: String =
        connection.query_row("PRAGMA cipher_version", [], |row| row.get(0))?;
    if cipher_version.trim().is_empty() {
        return Err(StoreError::EncryptionUnavailable);
    }
    configure_connection(&connection)?;
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = FULL;
        PRAGMA secure_delete = ON;
        PRAGMA cipher_memory_security = ON;
        ",
    )?;
    Ok(())
}

fn session_row(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<SessionRow, StoreError> {
    connection
        .query_row(
            "SELECT phase, revision, title, preflight_json, processing_status,
                    retention_policy_json, started_at_utc_ms, delete_after_utc_ms
             FROM meeting_sessions WHERE id = ?1",
            params![id(session_id)],
            |row| {
                let revision: i64 = row.get(1)?;
                Ok(SessionRow {
                    phase: phase_from_db(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
                    revision: u64::try_from(revision)
                        .map_err(|_| to_sql_error(StoreError::Corrupt))?,
                    title: row.get(2)?,
                    preflight_json: row.get(3)?,
                    processing_status_json: row.get(4)?,
                    retention_policy_json: row.get(5)?,
                    started_at_utc_ms: row.get(6)?,
                    delete_after_utc_ms: row.get(7)?,
                })
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound)
}

fn session_disclosure_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<MeetingSessionDisclosure, StoreError> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT disclosure_json FROM meeting_sessions WHERE id = ?1",
            params![id(session_id)],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound)?;
    match stored {
        Some(json) => decode_json(&json),
        None => Ok(MeetingSessionDisclosure::NotAsked),
    }
}

fn write_session_disclosure_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    disclosure: &MeetingSessionDisclosure,
) -> Result<(), StoreError> {
    let changed = connection.execute(
        "UPDATE meeting_sessions SET disclosure_json = ?1 WHERE id = ?2",
        params![encode_json(disclosure)?, id(session_id)],
    )?;
    if changed == 0 {
        return Err(StoreError::NotFound);
    }
    Ok(())
}

fn operation_receipt_in(
    transaction: &Transaction<'_>,
    operation_id: MeetingOperationId,
) -> Result<Option<OperationReceipt>, StoreError> {
    let json = transaction
        .query_row(
            "SELECT receipt_json FROM meeting_operation_receipts WHERE operation_id = ?1",
            params![id(operation_id)],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    json.map(|value| decode_json(&value)).transpose()
}

fn validate_mutation(
    transaction: &Transaction<'_>,
    mutation: StoreMutation,
    current: &SessionRow,
    allowed_from: &[MeetingPhase],
) -> Result<Option<OperationReceipt>, StoreError> {
    if let Some(receipt) = operation_receipt_in(transaction, mutation.operation_id)? {
        return Ok(Some(receipt));
    }
    let reason = if mutation.expected_revision != current.revision {
        Some(MeetingReasonCode::StaleRevision)
    } else if !allowed_from.contains(&current.phase) {
        Some(MeetingReasonCode::InvalidTransition)
    } else {
        None
    };
    let Some(reason) = reason else {
        return Ok(None);
    };
    let receipt = rejected_receipt(mutation, current.phase, current.revision, reason);
    insert_operation_receipt(transaction, &receipt, utc_now_ms())?;
    Ok(Some(receipt))
}

fn committed_receipt(
    mutation: StoreMutation,
    from_phase: MeetingPhase,
    to_phase: MeetingPhase,
    committed_at_utc_ms: i64,
    new_revision: u64,
    effect_ids: Vec<String>,
) -> OperationReceipt {
    OperationReceipt {
        schema_version: STORE_SCHEMA_VERSION,
        operation_id: mutation.operation_id,
        session_id: Some(mutation.session_id),
        actor: mutation.actor,
        command: mutation.command,
        expected_revision: mutation.expected_revision,
        from_phase: Some(from_phase),
        to_phase: Some(to_phase),
        requested_at_utc_ms: mutation.requested_at_utc_ms,
        committed_at_utc_ms: Some(committed_at_utc_ms),
        result: OperationResult::Committed,
        reason_codes: Vec::new(),
        new_revision: Some(new_revision),
        effect_ids,
    }
}
fn committed_global_receipt(
    operation_id: MeetingOperationId,
    command: MeetingCommandKind,
    expected_revision: u64,
    requested_at_utc_ms: i64,
    committed_at_utc_ms: i64,
    new_revision: u64,
) -> OperationReceipt {
    OperationReceipt {
        schema_version: STORE_SCHEMA_VERSION,
        operation_id,
        session_id: None,
        actor: OperationActor::User,
        command,
        expected_revision,
        from_phase: None,
        to_phase: None,
        requested_at_utc_ms,
        committed_at_utc_ms: Some(committed_at_utc_ms),
        result: OperationResult::Committed,
        reason_codes: Vec::new(),
        new_revision: Some(new_revision),
        effect_ids: Vec::new(),
    }
}

fn rejected_global_receipt(
    operation_id: MeetingOperationId,
    command: MeetingCommandKind,
    expected_revision: u64,
    current_revision: u64,
    requested_at_utc_ms: i64,
    reason: MeetingReasonCode,
) -> OperationReceipt {
    OperationReceipt {
        schema_version: STORE_SCHEMA_VERSION,
        operation_id,
        session_id: None,
        actor: OperationActor::User,
        command,
        expected_revision,
        from_phase: None,
        to_phase: None,
        requested_at_utc_ms,
        committed_at_utc_ms: Some(utc_now_ms()),
        result: OperationResult::Rejected,
        reason_codes: vec![reason],
        new_revision: Some(current_revision),
        effect_ids: Vec::new(),
    }
}
fn export_result_for_receipt(
    connection: &Connection,
    receipt: OperationReceipt,
) -> Result<MeetingExportResult, StoreError> {
    if receipt.command != MeetingCommandKind::Export || receipt.result != OperationResult::Committed
    {
        return Err(StoreError::Invalid);
    }
    let [effect_id] = receipt.effect_ids.as_slice() else {
        return Err(StoreError::Corrupt);
    };
    let export_receipt_id = MeetingExportReceiptId::from_uuid(parse_uuid(effect_id)?);
    let (
        session_id,
        format,
        snapshot_revision,
        capture_completeness,
        transcript_revision_id,
        created_at_utc_ms,
    ): (String, String, i64, String, Option<String>, i64) = connection
        .query_row(
            "SELECT session_id, format, snapshot_revision, capture_completeness,
                    transcript_revision_id, created_at_utc_ms
             FROM meeting_export_receipts WHERE export_receipt_id = ?1",
            params![id(export_receipt_id)],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::Corrupt)?;
    Ok(MeetingExportResult {
        receipt,
        export_receipt: MeetingExportReceipt {
            export_receipt_id,
            session_id: MeetingSessionId::from_uuid(parse_uuid(&session_id)?),
            format: decode_json(&format)?,
            snapshot_revision: from_i64(snapshot_revision)?,
            capture_completeness: decode_json(&capture_completeness)?,
            transcript_revision_id: transcript_revision_id
                .map(|value| parse_uuid(&value).map(TranscriptRevisionId::from_uuid))
                .transpose()?,
            created_at_utc_ms,
        },
    })
}

fn rejected_receipt(
    mutation: StoreMutation,
    phase: MeetingPhase,
    current_revision: u64,
    reason: MeetingReasonCode,
) -> OperationReceipt {
    OperationReceipt {
        schema_version: STORE_SCHEMA_VERSION,
        operation_id: mutation.operation_id,
        session_id: Some(mutation.session_id),
        actor: mutation.actor,
        command: mutation.command,
        expected_revision: mutation.expected_revision,
        from_phase: Some(phase),
        to_phase: Some(phase),
        requested_at_utc_ms: mutation.requested_at_utc_ms,
        committed_at_utc_ms: Some(utc_now_ms()),
        result: OperationResult::Rejected,
        reason_codes: vec![reason],
        new_revision: Some(current_revision),
        effect_ids: Vec::new(),
    }
}

fn insert_operation_receipt(
    transaction: &Transaction<'_>,
    receipt: &OperationReceipt,
    created_at_utc_ms: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO meeting_operation_receipts (operation_id, session_id, receipt_json, created_at_utc_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            id(receipt.operation_id),
            receipt.session_id.map(id),
            encode_json(receipt)?,
            created_at_utc_ms,
        ],
    )?;
    Ok(())
}

/// One place that turns a health verdict into something a reader can see: the
/// snapshot revision moves, so a poll notices, and the event log keeps the
/// value. Three callers change health - the first committed packet, the first
/// lost audio, and the seal - and each owes the session this note.
fn note_health_change(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
    health: SourceHealth,
) -> Result<(), StoreError> {
    let current = session_row(transaction, session_id)?;
    let next_revision = current.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
    transaction.execute(
        "UPDATE meeting_sessions SET revision = ?1 WHERE id = ?2",
        params![to_i64(next_revision)?, id(session_id)],
    )?;
    append_event_with_details(
        transaction,
        session_id,
        next_revision,
        current.phase,
        current.phase,
        "source_health_changed",
        None,
        &format!("{{\"health\":{}}}", encode_json(&health)?),
    )
}

fn track_session(
    transaction: &Transaction<'_>,
    track_id: SourceTrackId,
) -> Result<MeetingSessionId, StoreError> {
    let owner: String = transaction.query_row(
        "SELECT session_id FROM meeting_source_tracks WHERE track_id = ?1",
        params![id(track_id)],
        |row| row.get(0),
    )?;
    Ok(MeetingSessionId::from_uuid(parse_uuid(&owner)?))
}

fn append_event(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
    sequence: u64,
    prior_phase: MeetingPhase,
    next_phase: MeetingPhase,
    event_kind: &str,
    session_offset_ns: Option<u64>,
) -> Result<(), StoreError> {
    append_event_with_details(
        transaction,
        session_id,
        sequence,
        prior_phase,
        next_phase,
        event_kind,
        session_offset_ns,
        "{}",
    )
}

/// The event log, with room for what the phase pair alone cannot say. Every
/// transition writes through here; `details_json` is an object because the
/// column has always held one.
#[allow(clippy::too_many_arguments)]
fn append_event_with_details(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
    sequence: u64,
    prior_phase: MeetingPhase,
    next_phase: MeetingPhase,
    event_kind: &str,
    session_offset_ns: Option<u64>,
    details_json: &str,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO meeting_session_events (
            session_id, sequence, prior_phase, next_phase, event_kind, observed_at_utc_ms,
            session_offset_ns, details_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id(session_id),
            to_i64(sequence)?,
            phase_db(prior_phase),
            phase_db(next_phase),
            event_kind,
            utc_now_ms(),
            optional_i64(session_offset_ns)?,
            details_json,
        ],
    )?;
    Ok(())
}

/// Why a meeting entered recovery, as the event ledger keeps it. `prior_phase`
/// is the phase the interrupted launch died in, written in the same spelling
/// the phase column uses.
fn recovery_details(prior_phase: MeetingPhase) -> String {
    format!(r#"{{"prior_phase":"{}"}}"#, phase_db(prior_phase))
}

/* --------------------------------------------------- meetings-list helpers */

/// The newest current artifact revision that carries content: the only
/// revision a list row is allowed to quote. Written once and interpolated at
/// both JSON paths, so the ledger headline and the notes summary on one row can
/// never come from two different revisions.
const CURRENT_ARTIFACT_CONTENT: &str = "SELECT a.content_json
                       FROM meeting_artifact_revisions a
                      WHERE a.session_id = m.id
                        AND a.state = 'current'
                        AND a.content_json IS NOT NULL
                      ORDER BY a.generated_at_utc_ms DESC LIMIT 1";

/// What one page row reads back before the per-session facts are attached.
struct ListedSessionRow {
    session: String,
    title: String,
    phase: String,
    created_at_utc_ms: i64,
    processing_status: String,
    recorded_duration_ns: Option<i64>,
    ledger_headline: Option<String>,
    summary_text: Option<String>,
    has_transcript: bool,
}

/// The stored state each list filter selects. `processing_status` holds the
/// serde JSON of `ProcessingStatus`, so its tag is read with `json_extract`
/// rather than matched as a bare string. Interpolated inside parentheses by the
/// caller, which is what keeps the `OR` arms from swallowing the cursor bound.
const fn status_predicate(filter: MeetingStatusFilter) -> &'static str {
    match filter {
        MeetingStatusFilter::Any => "1",
        MeetingStatusFilter::Ready => {
            "m.phase = 'review_ready'
                 AND json_extract(m.processing_status, '$.kind') = 'succeeded'"
        }
        MeetingStatusFilter::Processing => {
            "m.phase IN ('processing', 'stopping')
                 OR json_extract(m.processing_status, '$.kind') IN ('pending', 'running')"
        }
        MeetingStatusFilter::Failed => {
            "m.phase = 'recovery_required'
                 OR json_extract(m.processing_status, '$.kind') IN ('failed', 'cancelled')"
        }
    }
}

/// `needle` as a LIKE substring pattern, with the wildcards a title may
/// legitimately contain escaped so a typed `%` matches a literal `%`.
/// SQLite's LIKE folds case for ASCII only; a non-ASCII title matches on the
/// characters as typed.
fn like_contains(needle: &str) -> String {
    let mut pattern = String::with_capacity(needle.len() + 2);
    pattern.push('%');
    for character in needle.chars() {
        if matches!(character, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

/// Diarized speaker labels for one meeting, merged-away speakers excluded
/// because a merged speaker is no longer a person in the room.
fn speaker_labels_for_session(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<String>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT display_name FROM meeting_speakers
         WHERE session_id = ?1 AND merged_into_speaker_id IS NULL
         ORDER BY display_name",
    )?;
    let rows = statement.query_map(params![id(session_id)], |row| row.get::<_, String>(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// The one line a list row shows under the title, and which of the three real
/// sources it came from. The ledger headline is the news of the meeting, so it
/// wins; the notes summary's first sentence is the next best; a transcript with
/// no prose yet reports its own size rather than nothing.
fn row_headline(
    connection: &Connection,
    session_id: MeetingSessionId,
    ledger_headline: Option<&str>,
    summary_text: Option<&str>,
    has_transcript: bool,
) -> Result<MeetingHistoryHeadline, StoreError> {
    if let Some(text) = ledger_headline
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Ok(MeetingHistoryHeadline::Ledger {
            text: text.to_string(),
        });
    }
    if let Some(text) = summary_text
        .map(first_sentence)
        .filter(|text| !text.is_empty())
    {
        return Ok(MeetingHistoryHeadline::Summary { text });
    }
    if !has_transcript {
        return Ok(MeetingHistoryHeadline::None);
    }
    let words = transcript_word_count(connection, session_id)?;
    Ok(if words == 0 {
        MeetingHistoryHeadline::None
    } else {
        MeetingHistoryHeadline::Words { words }
    })
}

/// The summary's first sentence, which is all of it a one-line row can carry.
/// Cuts after the first `.`, `!` or `?` that whitespace follows, keeping the
/// terminator; a summary written as one unpunctuated line comes back whole.
/// Slicing on those ASCII bytes is always a char boundary.
fn first_sentence(text: &str) -> String {
    let trimmed = text.trim();
    let bytes = trimmed.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if !matches!(byte, b'.' | b'!' | b'?') {
            continue;
        }
        match bytes.get(index + 1) {
            None => break,
            Some(next) if next.is_ascii_whitespace() => return trimmed[..=index].to_string(),
            Some(_) => {}
        }
    }
    trimmed.to_string()
}

/// How long a derived title is allowed to be. A title is read at a glance in a
/// list row, so it stops being one somewhere around here.
const MAX_DERIVED_TITLE_CHARS: usize = 64;

/// A title read out of a meeting's own headline: its first sentence, in
/// sentence case, without the full stop, cut at a word boundary if the
/// sentence runs long.
///
/// `None` when the headline yields nothing a person would recognise as a
/// title — which is how a meeting with no speech in it keeps the name it was
/// born with rather than being given an empty one.
fn derived_title(headline: &str) -> Option<String> {
    let sentence = first_sentence(headline);
    let trimmed = sentence.trim_end_matches(['.', '!', '?', ' ']).trim();
    if trimmed.is_empty() {
        return None;
    }
    let cut = shortened_to_words(trimmed, MAX_DERIVED_TITLE_CHARS);
    let mut characters = cut.chars();
    let first = characters.next()?;
    // Sentence case: the first letter is the app's, the rest is the meeting's.
    // Upper-casing one character can widen it, so the capacity is a hint.
    let mut title = String::with_capacity(cut.len() + 3);
    title.extend(first.to_uppercase());
    title.push_str(characters.as_str());
    Some(title)
}

/// `text` cut to at most `limit` characters, on a word boundary when there is
/// one to cut on. No ellipsis: a shortened title is still a title, not a
/// truncated sentence.
fn shortened_to_words(text: &str, limit: usize) -> &str {
    let Some((end, _)) = text.char_indices().nth(limit) else {
        return text;
    };
    let head = &text[..end];
    match head.rfind(char::is_whitespace) {
        Some(space) => head[..space].trim_end_matches([',', ';', ':', '-']).trim(),
        None => head,
    }
}

/// Words in the current transcript, whitespace-delimited, with editor
/// replacements applied and removed segments dropped. Runs only for a row whose
/// line two has no generated prose to show, so a page of finished meetings
/// never reads a transcript at all.
fn transcript_word_count(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<u32, StoreError> {
    let mut statement = connection.prepare(
        "SELECT COALESCE((SELECT e.replacement_text FROM meeting_segment_edits e
                           WHERE e.segment_id = s.segment_id
                           ORDER BY e.edit_sequence DESC LIMIT 1), s.base_text)
         FROM meeting_sessions m
         JOIN meeting_transcript_segments s
           ON s.transcript_revision_id = m.current_transcript_revision_id
         WHERE m.id = ?1
           AND COALESCE((SELECT e.removed FROM meeting_segment_edits e
                          WHERE e.segment_id = s.segment_id
                          ORDER BY e.edit_sequence DESC LIMIT 1), 0) = 0",
    )?;
    let mut rows = statement.query(params![id(session_id)])?;
    let mut words: u32 = 0;
    while let Some(row) = rows.next()? {
        let text: String = row.get(0)?;
        words = words
            .saturating_add(u32::try_from(text.split_whitespace().count()).unwrap_or(u32::MAX));
    }
    Ok(words)
}

fn source_snapshots(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<MeetingSourceSnapshot>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT track_id, source_kind, required, format_json, health, last_offset_ns,
                (SELECT COUNT(*) FROM meeting_source_gaps g WHERE g.track_id = t.track_id)
         FROM meeting_source_tracks t WHERE session_id = ?1 ORDER BY source_kind",
    )?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        let required: i64 = row.get(2)?;
        let format: Option<String> = row.get(3)?;
        let last_offset: Option<i64> = row.get(5)?;
        let gap_count: i64 = row.get(6)?;
        Ok(MeetingSourceSnapshot {
            track_id: Some(SourceTrackId::from_uuid(
                parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
            )),
            source_kind: source_kind_from_db(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
            required: required != 0,
            availability: SourceAvailability::Available,
            health: decode_json(&row.get::<_, String>(4)?).map_err(to_sql_error)?,
            format: format
                .map(|value| decode_json(&value))
                .transpose()
                .map_err(to_sql_error)?,
            last_durable_offset_ns: last_offset
                .map(from_i64)
                .transpose()
                .map_err(to_sql_error)?,
            gap_count: u64::try_from(gap_count).map_err(|_| to_sql_error(StoreError::Corrupt))?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn derive_completeness(
    connection: &Connection,
    session_id: MeetingSessionId,
    sources: &[MeetingSourceSnapshot],
) -> Result<CaptureCompleteness, StoreError> {
    let window_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM meeting_capture_windows WHERE session_id = ?1",
        params![id(session_id)],
        |row| row.get(0),
    )?;
    if window_count == 0 {
        return Ok(CaptureCompleteness::NotStarted);
    }
    // `Healthy` is granted by the packet that reached the disk and taken away
    // by the first loss; nothing at the seal changes it, so a sealed meeting
    // is as complete as it was live. `Degraded` and `Failed` are not, and
    // neither is `Starting`, which means no audio ever landed.
    if sources.len() < SourceKind::ALL.len()
        || sources.iter().any(|source| {
            source.health != SourceHealth::Healthy || source.last_durable_offset_ns.is_none()
        })
    {
        return Ok(CaptureCompleteness::Partial);
    }
    // `gap_count` counts every interval a lane reported, including the one
    // between the session's start and that lane's first buffer: two devices
    // cannot be handed the same instant, so counting startup latency as lost
    // audio would make `Complete` unreachable for every multi-source meeting
    // and teach a reader to ignore the word. A gap that closes at or before a
    // lane's first delivered audio is that latency and is already visible as a
    // gap, and a pause is silence the operator asked for; anything else is
    // audio the meeting was recording and lost.
    let lost_gaps: i64 = connection.query_row(
        "SELECT COUNT(*) FROM meeting_source_gaps g
           JOIN meeting_source_tracks t ON t.track_id = g.track_id
          WHERE t.session_id = ?1
            AND g.reason <> ?2
            AND NOT (
                 g.end_offset_ns IS NOT NULL
                 AND t.first_offset_ns IS NOT NULL
                 AND g.end_offset_ns <= t.first_offset_ns
            )",
        params![id(session_id), encode_json(&SourceGapReason::Paused)?],
        |row| row.get(0),
    )?;
    if lost_gaps > 0 {
        return Ok(CaptureCompleteness::Partial);
    }
    let open_windows: i64 = connection.query_row(
        "SELECT COUNT(*) FROM meeting_capture_windows WHERE session_id = ?1 AND end_offset_ns IS NULL",
        params![id(session_id)],
        |row| row.get(0),
    )?;
    if open_windows > 0 {
        return Ok(CaptureCompleteness::Partial);
    }
    Ok(CaptureCompleteness::Complete)
}

fn allowed_actions(phase: MeetingPhase) -> Vec<AllowedMeetingAction> {
    match phase {
        MeetingPhase::Preflight => vec![
            AllowedMeetingAction::RefreshPreflight,
            AllowedMeetingAction::CancelPreflight,
            AllowedMeetingAction::Start,
        ],
        MeetingPhase::Starting
        | MeetingPhase::CapturingRecording
        | MeetingPhase::CapturingPausing
        | MeetingPhase::CapturingResuming => vec![
            AllowedMeetingAction::Pause,
            AllowedMeetingAction::Stop,
            AllowedMeetingAction::Discard,
        ],
        MeetingPhase::CapturingPaused => vec![
            AllowedMeetingAction::Resume,
            AllowedMeetingAction::Stop,
            AllowedMeetingAction::Discard,
            AllowedMeetingAction::Edit,
        ],
        MeetingPhase::Stopping | MeetingPhase::Processing => vec![AllowedMeetingAction::Discard],
        MeetingPhase::ReviewReady => vec![
            AllowedMeetingAction::Edit,
            AllowedMeetingAction::Regenerate,
            AllowedMeetingAction::Export,
            AllowedMeetingAction::Delete,
        ],
        MeetingPhase::RecoveryRequired => vec![
            AllowedMeetingAction::FinalizePartial,
            AllowedMeetingAction::Discard,
            AllowedMeetingAction::Delete,
        ],
        MeetingPhase::Deleting => Vec::new(),
    }
}

fn track_snapshots(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<MeetingTrackSnapshot>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT t.track_id, t.source_kind, t.format_json, t.first_offset_ns, t.last_offset_ns,
                (SELECT COUNT(*) FROM meeting_track_records r WHERE r.track_id = t.track_id)
         FROM meeting_source_tracks t WHERE t.session_id = ?1 ORDER BY t.source_kind",
    )?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        let format: Option<String> = row.get(2)?;
        let first: Option<i64> = row.get(3)?;
        let last: Option<i64> = row.get(4)?;
        let count: i64 = row.get(5)?;
        Ok(MeetingTrackSnapshot {
            track_id: SourceTrackId::from_uuid(
                parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
            ),
            source_kind: source_kind_from_db(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
            format: format
                .map(|value| decode_json(&value))
                .transpose()
                .map_err(to_sql_error)?,
            first_offset_ns: first.map(from_i64).transpose().map_err(to_sql_error)?,
            last_offset_ns: last.map(from_i64).transpose().map_err(to_sql_error)?,
            durable_record_count: u64::try_from(count)
                .map_err(|_| to_sql_error(StoreError::Corrupt))?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn gaps_for_session(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<SourceGap>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT g.track_id, g.source_epoch, g.start_offset_ns, g.end_offset_ns, g.reason, g.dropped_frames
         FROM meeting_source_gaps g JOIN meeting_source_tracks t ON t.track_id = g.track_id
         WHERE t.session_id = ?1 ORDER BY g.gap_id",
    )?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        let epoch: i64 = row.get(1)?;
        let start: Option<i64> = row.get(2)?;
        let end: Option<i64> = row.get(3)?;
        let dropped: Option<i64> = row.get(5)?;
        Ok(SourceGap {
            track_id: SourceTrackId::from_uuid(
                parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
            ),
            epoch: SourceEpoch::new(
                u64::try_from(epoch).map_err(|_| to_sql_error(StoreError::Corrupt))?,
            ),
            start_offset_ns: start.map(from_i64).transpose().map_err(to_sql_error)?,
            end_offset_ns: end.map(from_i64).transpose().map_err(to_sql_error)?,
            reason: decode_json(&row.get::<_, String>(4)?).map_err(to_sql_error)?,
            dropped_frames: dropped.map(from_i64).transpose().map_err(to_sql_error)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn speakers_for_session(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<MeetingSpeaker>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT speaker_id, source_kind, display_name, revision FROM meeting_speakers WHERE session_id = ?1 ORDER BY display_name",
    )?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        let revision: i64 = row.get(3)?;
        Ok(MeetingSpeaker {
            speaker_id: SpeakerId::from_uuid(
                parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
            ),
            session_id,
            source_kind: source_kind_from_db(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
            display_name: row.get(2)?,
            revision: u64::try_from(revision).map_err(|_| to_sql_error(StoreError::Corrupt))?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn effective_segments_for_session(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<EffectiveTranscriptSegment>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT s.segment_id, s.transcript_revision_id, s.track_id, s.ordinal, s.start_offset_ns,
                s.end_offset_ns, s.speaker_id, s.base_text, s.confidence_milli,
                e.replacement_text, e.removed, e.edit_sequence,
                COALESCE(a.speaker_id, s.speaker_id),
                COALESCE(a.assignment_kind,
                    CASE t.source_kind WHEN 'microphone' THEN 'local_speaker' ELSE 'unknown' END)
         FROM meeting_sessions m
         JOIN meeting_transcript_segments s
           ON s.transcript_revision_id = m.current_transcript_revision_id
         JOIN meeting_source_tracks t ON t.track_id = s.track_id
         LEFT JOIN meeting_diarization_assignments a
           ON a.generation_id = m.current_diarization_generation_id AND a.segment_id = s.segment_id
         LEFT JOIN meeting_segment_edits e ON e.segment_id = s.segment_id
             AND e.edit_sequence = (SELECT MAX(edit_sequence) FROM meeting_segment_edits WHERE segment_id = s.segment_id)
         WHERE m.id = ?1 ORDER BY s.start_offset_ns, s.ordinal",
    )?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        let ordinal: i64 = row.get(3)?;
        let start: i64 = row.get(4)?;
        let end: i64 = row.get(5)?;
        let confidence: Option<i64> = row.get(8)?;
        let edit_sequence: Option<i64> = row.get(11)?;
        Ok(EffectiveTranscriptSegment {
            base: TranscriptSegment {
                segment_id: TranscriptSegmentId::from_uuid(
                    parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
                ),
                transcript_revision_id: TranscriptRevisionId::from_uuid(
                    parse_uuid(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
                ),
                track_id: SourceTrackId::from_uuid(
                    parse_uuid(&row.get::<_, String>(2)?).map_err(to_sql_error)?,
                ),
                ordinal: u64::try_from(ordinal).map_err(|_| to_sql_error(StoreError::Corrupt))?,
                start_offset_ns: from_i64(start).map_err(to_sql_error)?,
                end_offset_ns: from_i64(end).map_err(to_sql_error)?,
                speaker_id: SpeakerId::from_uuid(
                    parse_uuid(&row.get::<_, String>(6)?).map_err(to_sql_error)?,
                ),
                text: row.get(7)?,
                confidence_milli: confidence
                    .map(|value| {
                        u16::try_from(value).map_err(|_| to_sql_error(StoreError::Corrupt))
                    })
                    .transpose()?,
            },
            replacement_text: row.get(9)?,
            removed: row.get::<_, Option<i64>>(10)?.unwrap_or(0) != 0,
            edit_revision: edit_sequence
                .map(|value| u64::try_from(value).map_err(|_| to_sql_error(StoreError::Corrupt)))
                .transpose()?,
            assigned_speaker_id: SpeakerId::from_uuid(
                parse_uuid(&row.get::<_, String>(12)?).map_err(to_sql_error)?,
            ),
            speaker_assignment: speaker_assignment_from_db(&row.get::<_, String>(13)?)
                .map_err(to_sql_error)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Resolve merged speaker ids once at the store boundary. Analytics and every
/// prompt built from it therefore share the same canonical identity.
fn canonical_speaker_ids(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<HashMap<SpeakerId, SpeakerId>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT speaker_id, merged_into_speaker_id FROM meeting_speakers
         WHERE session_id = ?1",
    )?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    let mut parent_by_speaker = HashMap::new();
    for row in rows {
        let (speaker_id, merged_into) = row?;
        let speaker_id = SpeakerId::from_uuid(parse_uuid(&speaker_id)?);
        let merged_into = merged_into
            .map(|raw| parse_uuid(&raw).map(SpeakerId::from_uuid))
            .transpose()?;
        parent_by_speaker.insert(speaker_id, merged_into);
    }

    let mut canonical_ids = HashMap::new();
    for source in parent_by_speaker.keys().copied().collect::<Vec<_>>() {
        let mut path = Vec::new();
        let mut current = source;
        while let Some(Some(parent)) = parent_by_speaker.get(&current) {
            if path.contains(&current) {
                return Err(StoreError::Corrupt);
            }
            path.push(current);
            current = *parent;
        }
        for speaker_id in path {
            canonical_ids.insert(speaker_id, current);
        }
        canonical_ids.insert(source, current);
    }
    Ok(canonical_ids)
}

/// The canonical transcript as speaker-attributed utterances. Speaker identity
/// follows the current diarization assignment, and edited text replaces the
/// recognizer's, so metrics describe the transcript a person actually reads.
fn analytics_segments_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<AnalyticsSegment>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT s.segment_id, s.start_offset_ns, s.end_offset_ns,
                COALESCE(a.speaker_id, s.speaker_id),
                COALESCE(e.replacement_text, s.base_text)
         FROM meeting_sessions m
         JOIN meeting_transcript_segments s
           ON s.transcript_revision_id = m.current_transcript_revision_id
         LEFT JOIN meeting_diarization_assignments a
           ON a.generation_id = m.current_diarization_generation_id AND a.segment_id = s.segment_id
         LEFT JOIN meeting_segment_edits e ON e.segment_id = s.segment_id
             AND e.edit_sequence = (SELECT MAX(edit_sequence) FROM meeting_segment_edits WHERE segment_id = s.segment_id)
         WHERE m.id = ?1 AND COALESCE(e.removed, 0) = 0
         ORDER BY s.start_offset_ns, s.ordinal",
    )?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        let start: i64 = row.get(1)?;
        let end: i64 = row.get(2)?;
        Ok(AnalyticsSegment {
            segment_id: TranscriptSegmentId::from_uuid(
                parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
            ),
            speaker_id: SpeakerId::from_uuid(
                parse_uuid(&row.get::<_, String>(3)?).map_err(to_sql_error)?,
            ),
            start_offset_ns: from_i64(start).map_err(to_sql_error)?,
            end_offset_ns: from_i64(end).map_err(to_sql_error)?,
            text: row.get(4)?,
        })
    })?;
    let mut segments = rows.collect::<Result<Vec<_>, _>>()?;
    let canonical_ids = canonical_speaker_ids(connection, session_id)?;
    for segment in &mut segments {
        if let Some(canonical_id) = canonical_ids.get(&segment.speaker_id) {
            segment.speaker_id = *canonical_id;
        }
    }
    Ok(segments)
}

/// A meeting with no saved notes still has a notes layer: an empty body under
/// the caller's default template. An unrecognized stored template id falls
/// back the same way rather than failing the read.
fn user_notes_row(
    connection: &Connection,
    session_id: MeetingSessionId,
    default_template: MeetingNotesTemplate,
) -> Result<MeetingUserNotes, StoreError> {
    let row: Option<(String, String, i64, i64)> = connection
        .query_row(
            "SELECT body, template_id, note_revision, updated_at_utc_ms
             FROM meeting_user_notes WHERE session_id = ?1",
            params![id(session_id)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((body, template_id, note_revision, updated_at_utc_ms)) = row else {
        return Ok(MeetingUserNotes::empty(session_id, default_template));
    };
    Ok(MeetingUserNotes {
        session_id,
        body,
        template: MeetingNotesTemplate::from_artifact_template_id(&template_id)
            .unwrap_or(default_template),
        revision: from_i64(note_revision)?,
        updated_at_utc_ms,
    })
}

fn notes_for_session(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<ManualNote>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT note_id, start_offset_ns, end_offset_ns, body, note_revision, created_at_utc_ms, updated_at_utc_ms
         FROM meeting_notes WHERE session_id = ?1 ORDER BY created_at_utc_ms",
    )?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        let start: Option<i64> = row.get(1)?;
        let end: Option<i64> = row.get(2)?;
        let revision: i64 = row.get(4)?;
        Ok(ManualNote {
            note_id: ManualNoteId::from_uuid(
                parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
            ),
            session_id,
            start_offset_ns: start.map(from_i64).transpose().map_err(to_sql_error)?,
            end_offset_ns: end.map(from_i64).transpose().map_err(to_sql_error)?,
            body: row.get(3)?,
            revision: u64::try_from(revision).map_err(|_| to_sql_error(StoreError::Corrupt))?,
            created_at_utc_ms: row.get(5)?,
            updated_at_utc_ms: row.get(6)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn source_speaker_for(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
    source_kind: SourceKind,
) -> Result<SpeakerId, StoreError> {
    let display_name = match source_kind {
        SourceKind::Microphone => "Local speaker",
        SourceKind::SystemAudio => "Unknown speaker",
    };
    named_speaker_for(transaction, session_id, source_kind, display_name)
}

fn named_speaker_for(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
    source_kind: SourceKind,
    display_name: &str,
) -> Result<SpeakerId, StoreError> {
    let existing: Option<String> = transaction
        .query_row(
            "SELECT speaker_id FROM meeting_speakers
             WHERE session_id = ?1 AND source_kind = ?2 AND display_name = ?3
               AND merged_into_speaker_id IS NULL
             ORDER BY speaker_id LIMIT 1",
            params![id(session_id), source_kind.as_str(), display_name],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        return Ok(SpeakerId::from_uuid(parse_uuid(&existing)?));
    }
    let speaker_id = SpeakerId::new();
    transaction.execute(
        "INSERT INTO meeting_speakers (
            speaker_id, session_id, source_kind, display_name, revision, merged_into_speaker_id
         ) VALUES (?1, ?2, ?3, ?4, 0, NULL)",
        params![
            id(speaker_id),
            id(session_id),
            source_kind.as_str(),
            display_name,
        ],
    )?;
    Ok(speaker_id)
}

fn artifact_revision_for_key(
    connection: &Connection,
    session_id: MeetingSessionId,
    generation_key: &str,
) -> Result<Option<MeetingArtifactRevision>, StoreError> {
    connection
        .query_row(
            "SELECT artifact_id, transcript_revision_id, input_revision, template_id,
                    template_version, generation_key, state, content_json, generated_at_utc_ms
             FROM meeting_artifact_revisions
             WHERE session_id = ?1 AND generation_key = ?2",
            params![id(session_id), generation_key],
            |row| artifact_revision_from_row(row, session_id),
        )
        .optional()
        .map_err(Into::into)
}

fn artifact_revisions_for_session(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<MeetingArtifactRevision>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT artifact_id, transcript_revision_id, input_revision, template_id,
                template_version, generation_key, state, content_json, generated_at_utc_ms
         FROM meeting_artifact_revisions
         WHERE session_id = ?1 ORDER BY generated_at_utc_ms DESC",
    )?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        artifact_revision_from_row(row, session_id)
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn artifact_revision_from_row(
    row: &rusqlite::Row<'_>,
    session_id: MeetingSessionId,
) -> rusqlite::Result<MeetingArtifactRevision> {
    let input_revision: i64 = row.get(2)?;
    let template_version: i64 = row.get(4)?;
    let content_json: Option<String> = row.get(7)?;
    Ok(MeetingArtifactRevision {
        artifact_id: MeetingArtifactId::from_uuid(
            parse_uuid(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
        ),
        session_id,
        transcript_revision_id: TranscriptRevisionId::from_uuid(
            parse_uuid(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
        ),
        input_revision: from_i64(input_revision).map_err(to_sql_error)?,
        template_id: row.get(3)?,
        template_version: u32::try_from(template_version)
            .map_err(|_| to_sql_error(StoreError::Corrupt))?,
        generation_key: row.get(5)?,
        state: artifact_state_from_db(&row.get::<_, String>(6)?).map_err(to_sql_error)?,
        generated_at_utc_ms: row.get(8)?,
        content: content_json
            .as_deref()
            .map(decode_json)
            .transpose()
            .map_err(to_sql_error)?,
    })
}

fn question_history_for_session(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<MeetingAnswer>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT question_id, question_text, scope_json, answer_state, answer_text,
                input_revision, revision, created_at_utc_ms
         FROM meeting_questions WHERE session_id = ?1 ORDER BY created_at_utc_ms",
    )?;
    let rows = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    rows.into_iter()
        .map(
            |(
                question_id,
                question,
                scope,
                state,
                answer,
                input_revision,
                revision,
                created_at,
            )| {
                let question_id = MeetingQuestionId::from_uuid(parse_uuid(&question_id)?);
                Ok(MeetingAnswer {
                    question_id,
                    session_id,
                    scope: decode_json(&scope)?,
                    question: Some(question),
                    state: meeting_answer_state_from_db(&state)?,
                    answer,
                    citations: question_citations(connection, question_id)?,
                    input_revision: from_i64(input_revision)?,
                    revision: from_i64(revision)?,
                    created_at_utc_ms: created_at,
                    /* A saved answer was read from a landed transcript: that
                     * is the only kind history holds. */
                    through_offset_ns: None,
                    provisional: false,
                })
            },
        )
        .collect()
}

fn question_citations(
    connection: &Connection,
    question_id: MeetingQuestionId,
) -> Result<Vec<MeetingCitation>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT citation_json FROM meeting_question_citations
         WHERE question_id = ?1 ORDER BY ordinal",
    )?;
    let rows = statement.query_map(params![id(question_id)], |row| row.get::<_, String>(0))?;
    rows.map(|row| {
        row.map_err(Into::into)
            .and_then(|citation| decode_json(&citation))
    })
    .collect()
}

fn diarization_snapshot_for_session(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<MeetingDiarizationSnapshot, StoreError> {
    let (status, model_id, model_version, generation_id): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = connection.query_row(
        "SELECT diarization_status, diarization_model_id, diarization_model_version,
                current_diarization_generation_id
         FROM meeting_sessions WHERE id = ?1",
        params![id(session_id)],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let generation_id = generation_id
        .as_deref()
        .map(parse_uuid)
        .transpose()?
        .map(MeetingDiarizationGenerationId::from_uuid);
    let assigned_segment_count = match generation_id {
        Some(generation_id) => connection.query_row(
            "SELECT COUNT(*) FROM meeting_diarization_assignments WHERE generation_id = ?1",
            params![id(generation_id)],
            |row| row.get::<_, i64>(0),
        )?,
        None => 0,
    };
    Ok(MeetingDiarizationSnapshot {
        status: diarization_status_from_db(&status)?,
        model_id: model_id
            .unwrap_or_else(|| crate::meeting::diarization::model_manifest().id.clone()),
        model_version: model_version.unwrap_or_else(|| {
            crate::meeting::diarization::model_manifest()
                .revision
                .clone()
        }),
        generation_id,
        assigned_segment_count: from_i64(assigned_segment_count)?,
    })
}

fn mark_artifacts_out_of_date(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE meeting_artifacts SET state = 'out_of_date' WHERE session_id = ?1 AND state = 'current'",
        params![id(session_id)],
    )?;
    transaction.execute(
        "UPDATE meeting_artifact_revisions
         SET state = 'out_of_date' WHERE session_id = ?1 AND state = 'current'",
        params![id(session_id)],
    )?;
    transaction.execute(
        "UPDATE meeting_questions
         SET answer_state = 'out_of_date'
         WHERE session_id = ?1 AND answer_state != 'forgotten'",
        params![id(session_id)],
    )?;
    Ok(())
}

fn rebuild_search_documents_in(
    transaction: &Transaction<'_>,
    session_id: MeetingSessionId,
) -> Result<(), StoreError> {
    transaction.execute(
        "DELETE FROM meeting_search_documents WHERE session_id = ?1",
        params![id(session_id)],
    )?;
    let title: String = transaction.query_row(
        "SELECT title FROM meeting_sessions WHERE id = ?1",
        params![id(session_id)],
        |row| row.get(0),
    )?;
    transaction.execute(
        "INSERT INTO meeting_search_documents (session_id, entity_kind, entity_id, content)
         VALUES (?1, 'title', ?1, ?2)",
        params![id(session_id), title],
    )?;
    transaction.execute(
        "INSERT INTO meeting_search_documents (session_id, entity_kind, entity_id, content)
         SELECT n.session_id, 'note', n.note_id, n.body FROM meeting_notes n WHERE n.session_id = ?1",
        params![id(session_id)],
    )?;
    transaction.execute(
        "INSERT INTO meeting_search_documents (session_id, entity_kind, entity_id, content)
         SELECT m.id, 'segment', s.segment_id,
            COALESCE((SELECT e.replacement_text FROM meeting_segment_edits e
                      WHERE e.segment_id = s.segment_id ORDER BY e.edit_sequence DESC LIMIT 1), s.base_text)
         FROM meeting_sessions m
         JOIN meeting_transcript_segments s
           ON s.transcript_revision_id = m.current_transcript_revision_id
         WHERE m.id = ?1
           AND COALESCE((SELECT e.removed FROM meeting_segment_edits e
                         WHERE e.segment_id = s.segment_id ORDER BY e.edit_sequence DESC LIMIT 1), 0) = 0",
        params![id(session_id)],
    )?;
    Ok(())
}

fn insert_durable_record(
    transaction: &Transaction<'_>,
    track_id: SourceTrackId,
    record: &PendingRecord,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT OR IGNORE INTO meeting_track_records (
            track_id, source_sequence, source_epoch, start_offset_ns, duration_ns, frame_count,
            record_offset_bytes, record_bytes, durable_at_utc_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id(track_id),
            to_i64(record.sequence)?,
            to_i64(record.epoch)?,
            optional_i64(record.start_offset_ns)?,
            to_i64(record.duration_ns)?,
            i64::from(record.frame_count),
            to_i64(record.record_offset)?,
            to_i64(record.record_bytes)?,
            utc_now_ms(),
        ],
    )?;
    Ok(())
}

fn f32_payload(samples: &[f32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(samples.len().saturating_mul(std::mem::size_of::<f32>()));
    for sample in samples {
        payload.extend_from_slice(&sample.to_le_bytes());
    }
    payload
}

#[allow(clippy::too_many_arguments)]
fn write_encrypted_record(
    records: &mut File,
    index: &mut File,
    key: &[u8; 32],
    track_id: SourceTrackId,
    sequence: u64,
    epoch: SourceEpoch,
    start_offset_ns: u64,
    duration_ns: u64,
    frame_count: u32,
    format: AudioFormat,
    flags: PacketDiscontinuityFlags,
    payload: &[u8],
    record_offset: u64,
) -> Result<PendingRecord, StoreError> {
    let payload_len = u32::try_from(payload.len()).map_err(|_| StoreError::Invalid)?;
    let mut nonce_bytes = [0_u8; 12];
    getrandom::fill(&mut nonce_bytes).map_err(|_| StoreError::Unavailable)?;
    let checksum = Sha256::digest(payload);
    let mut header = [0_u8; RECORD_HEADER_BYTES];
    header[..4].copy_from_slice(&RECORD_MAGIC);
    header[4] = RECORD_FORMAT_VERSION;
    header[5] = (u8::from(flags.timestamp_reset))
        | (u8::from(flags.route_changed) << 1)
        | (u8::from(flags.source_restarted) << 2);
    header[8..16].copy_from_slice(&sequence.to_le_bytes());
    header[16..24].copy_from_slice(&epoch.get().to_le_bytes());
    header[24..32].copy_from_slice(&start_offset_ns.to_le_bytes());
    header[32..36].copy_from_slice(&frame_count.to_le_bytes());
    header[36..40].copy_from_slice(&format.sample_rate_hz.to_le_bytes());
    header[40..42].copy_from_slice(&format.channels.to_le_bytes());
    header[44..48].copy_from_slice(&payload_len.to_le_bytes());
    header[48..80].copy_from_slice(&checksum);
    header[80..92].copy_from_slice(&nonce_bytes);
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| StoreError::Corrupt)?;
    let aad = audio_aad(track_id, &header);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: payload,
                aad: &aad,
            },
        )
        .map_err(|_| StoreError::Corrupt)?;
    records.write_all(&header)?;
    records.write_all(&ciphertext)?;
    let record_bytes = u64::try_from(RECORD_HEADER_BYTES)
        .ok()
        .and_then(|header_bytes| header_bytes.checked_add(u64::try_from(ciphertext.len()).ok()?))
        .ok_or(StoreError::Corrupt)?;
    let end_offset_ns = start_offset_ns.checked_add(duration_ns);

    let mut index_plaintext = [0_u8; INDEX_PLAINTEXT_BYTES];
    index_plaintext[..8].copy_from_slice(&sequence.to_le_bytes());
    index_plaintext[8..16].copy_from_slice(&record_offset.to_le_bytes());
    index_plaintext[16..24].copy_from_slice(&start_offset_ns.to_le_bytes());
    index_plaintext[24..32].copy_from_slice(&duration_ns.to_le_bytes());
    index_plaintext[32..36].copy_from_slice(&frame_count.to_le_bytes());
    index_plaintext[36..40].copy_from_slice(
        &u32::try_from(record_bytes)
            .map_err(|_| StoreError::Corrupt)?
            .to_le_bytes(),
    );
    index_plaintext[40..48].copy_from_slice(&epoch.get().to_le_bytes());
    let mut index_nonce = [0_u8; 12];
    getrandom::fill(&mut index_nonce).map_err(|_| StoreError::Unavailable)?;
    let index_aad = index_aad(track_id);
    let encrypted_index = cipher
        .encrypt(
            Nonce::from_slice(&index_nonce),
            Payload {
                msg: &index_plaintext,
                aad: &index_aad,
            },
        )
        .map_err(|_| StoreError::Corrupt)?;
    if encrypted_index.len() != INDEX_RECORD_BYTES - index_nonce.len() {
        return Err(StoreError::Corrupt);
    }
    index.write_all(&index_nonce)?;
    index.write_all(&encrypted_index)?;
    Ok(PendingRecord {
        sequence,
        epoch: epoch.get(),
        start_offset_ns: Some(start_offset_ns),
        end_offset_ns,
        duration_ns,
        frame_count,
        record_offset,
        record_bytes,
    })
}

fn decrypt_durable_record(
    file: &mut File,
    key: &[u8; 32],
    track_id: SourceTrackId,
    descriptor: DurableRecordDescriptor,
) -> Result<DurableTrackRecord, StoreError> {
    let mut header = [0_u8; RECORD_HEADER_BYTES];
    if !read_exact_at(file, descriptor.record_offset, &mut header)? {
        return Err(StoreError::Corrupt);
    }
    if header[..4] != RECORD_MAGIC || header[4] != RECORD_FORMAT_VERSION {
        return Err(StoreError::Corrupt);
    }
    let sequence = read_u64(&header[8..16]);
    let epoch = read_u64(&header[16..24]);
    let start_offset_ns = read_u64(&header[24..32]);
    let frame_count = read_u32(&header[32..36]);
    let format = AudioFormat {
        sample_rate_hz: read_u32(&header[36..40]),
        channels: read_u16(&header[40..42]),
    };
    let payload_len =
        usize::try_from(read_u32(&header[44..48])).map_err(|_| StoreError::Corrupt)?;
    let expected_samples = format
        .checked_frame_samples(frame_count)
        .ok_or(StoreError::Corrupt)?;
    if payload_len
        != expected_samples
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or(StoreError::Corrupt)?
    {
        return Err(StoreError::Corrupt);
    }
    let duration_ns = u64::from(frame_count)
        .checked_mul(1_000_000_000)
        .and_then(|frames| frames.checked_div(u64::from(format.sample_rate_hz)))
        .ok_or(StoreError::Corrupt)?;
    let ciphertext_len = payload_len.checked_add(16).ok_or(StoreError::Corrupt)?;
    let record_bytes = u64::try_from(RECORD_HEADER_BYTES)
        .ok()
        .and_then(|header_bytes| header_bytes.checked_add(u64::try_from(ciphertext_len).ok()?))
        .ok_or(StoreError::Corrupt)?;
    if sequence != descriptor.sequence
        || epoch != descriptor.epoch
        || start_offset_ns != descriptor.start_offset_ns
        || duration_ns != descriptor.duration_ns
        || frame_count != descriptor.frame_count
        || record_bytes != descriptor.record_bytes
    {
        return Err(StoreError::Corrupt);
    }
    let cipher_offset = descriptor
        .record_offset
        .checked_add(u64::try_from(RECORD_HEADER_BYTES).map_err(|_| StoreError::Corrupt)?)
        .ok_or(StoreError::Corrupt)?;
    let mut ciphertext = vec![0_u8; ciphertext_len];
    if !read_exact_at(file, cipher_offset, &mut ciphertext)? {
        return Err(StoreError::Corrupt);
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| StoreError::Corrupt)?;
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&header[80..92]),
            Payload {
                msg: &ciphertext,
                aad: &audio_aad(track_id, &header),
            },
        )
        .map_err(|_| StoreError::Corrupt)?;
    if Sha256::digest(&plaintext).as_slice() != &header[48..80] {
        return Err(StoreError::Corrupt);
    }
    let samples = parse_f32_payload(&plaintext, expected_samples)?;
    Ok(DurableTrackRecord {
        track_id,
        sequence,
        source_epoch: SourceEpoch::new(epoch),
        start_offset_ns,
        duration_ns,
        format,
        samples,
    })
}

fn parse_f32_payload(payload: &[u8], expected_samples: usize) -> Result<Vec<f32>, StoreError> {
    if payload.len()
        != expected_samples
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or(StoreError::Corrupt)?
    {
        return Err(StoreError::Corrupt);
    }
    let (chunks, remainder) = payload.as_chunks::<4>();
    if !remainder.is_empty() || chunks.len() != expected_samples {
        return Err(StoreError::Corrupt);
    }
    let mut samples = Vec::with_capacity(expected_samples);
    for bytes in chunks {
        let sample = f32::from_le_bytes(*bytes);
        if !sample.is_finite() {
            return Err(StoreError::Corrupt);
        }
        samples.push(sample);
    }
    Ok(samples)
}

fn repair_track_files(
    files: &TrackFiles,
    key: &[u8; 32],
    track_id: SourceTrackId,
) -> Result<RepairResult, StoreError> {
    let mut records_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&files.records)?;
    let mut index_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&files.index)?;
    let record_len = records_file.metadata()?.len();
    let mut records = Vec::new();
    let mut offset = 0_u64;
    let mut truncated = false;
    let mut last_epoch = None;
    let mut last_end_offset_ns = None;
    while offset < record_len {
        let mut header = [0_u8; RECORD_HEADER_BYTES];
        if !read_exact_at(&mut records_file, offset, &mut header)? {
            truncated = true;
            break;
        }
        if header[..4] != RECORD_MAGIC || header[4] != RECORD_FORMAT_VERSION {
            truncated = true;
            break;
        }
        let sequence = read_u64(&header[8..16]);
        let epoch = read_u64(&header[16..24]);
        let start = read_u64(&header[24..32]);
        let frame_count = read_u32(&header[32..36]);
        let sample_rate_hz = read_u32(&header[36..40]);
        let channels = read_u16(&header[40..42]);
        let payload_len =
            usize::try_from(read_u32(&header[44..48])).map_err(|_| StoreError::Corrupt)?;
        let Some(expected_samples) = AudioFormat {
            sample_rate_hz,
            channels,
        }
        .checked_frame_samples(frame_count) else {
            truncated = true;
            break;
        };
        if payload_len
            != expected_samples
                .checked_mul(std::mem::size_of::<f32>())
                .ok_or(StoreError::Corrupt)?
        {
            truncated = true;
            break;
        }
        let cipher_len = payload_len.checked_add(16).ok_or(StoreError::Corrupt)?;
        let mut ciphertext = vec![0_u8; cipher_len];
        let cipher_offset = offset
            .checked_add(u64::try_from(RECORD_HEADER_BYTES).map_err(|_| StoreError::Corrupt)?)
            .ok_or(StoreError::Corrupt)?;
        if !read_exact_at(&mut records_file, cipher_offset, &mut ciphertext)? {
            truncated = true;
            break;
        }
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| StoreError::Corrupt)?;
        let aad = audio_aad(track_id, &header);
        let plaintext = match cipher.decrypt(
            Nonce::from_slice(&header[80..92]),
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        ) {
            Ok(plaintext) => plaintext,
            Err(_) => {
                truncated = true;
                break;
            }
        };
        if Sha256::digest(&plaintext).as_slice() != &header[48..80] {
            truncated = true;
            break;
        }
        let duration_ns = u64::from(frame_count)
            .checked_mul(1_000_000_000)
            .and_then(|frames| frames.checked_div(u64::from(sample_rate_hz)))
            .ok_or(StoreError::Corrupt)?;
        let record_bytes = u64::try_from(RECORD_HEADER_BYTES)
            .ok()
            .and_then(|header_bytes| header_bytes.checked_add(u64::try_from(cipher_len).ok()?))
            .ok_or(StoreError::Corrupt)?;
        let end_offset_ns = start.checked_add(duration_ns);
        records.push(PendingRecord {
            sequence,
            epoch,
            start_offset_ns: Some(start),
            end_offset_ns,
            duration_ns,
            frame_count,
            record_offset: offset,
            record_bytes,
        });
        offset = offset
            .checked_add(record_bytes)
            .ok_or(StoreError::Corrupt)?;
        last_epoch = Some(epoch);
        last_end_offset_ns = end_offset_ns;
    }
    if offset != record_len {
        records_file.set_len(offset)?;
        records_file.sync_all()?;
    }
    let valid_index_bytes = rebuild_and_validate_index(&mut index_file, key, track_id, &records)?;
    if index_file.metadata()?.len() != valid_index_bytes {
        index_file.set_len(valid_index_bytes)?;
        index_file.sync_all()?;
        truncated = true;
    }
    Ok(RepairResult {
        records,
        truncated,
        last_epoch,
        last_end_offset_ns,
    })
}

fn rebuild_and_validate_index(
    file: &mut File,
    key: &[u8; 32],
    track_id: SourceTrackId,
    records: &[PendingRecord],
) -> Result<u64, StoreError> {
    let expected_len = u64::try_from(records.len())
        .ok()
        .and_then(|count| count.checked_mul(u64::try_from(INDEX_RECORD_BYTES).ok()?))
        .ok_or(StoreError::Corrupt)?;
    let current_len = file.metadata()?.len();
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| StoreError::Corrupt)?;
    let aad = index_aad(track_id);
    let mut valid_records = 0_u64;
    while valid_records < u64::try_from(records.len()).map_err(|_| StoreError::Corrupt)? {
        let offset = valid_records
            .checked_mul(u64::try_from(INDEX_RECORD_BYTES).map_err(|_| StoreError::Corrupt)?)
            .ok_or(StoreError::Corrupt)?;
        if offset
            .checked_add(u64::try_from(INDEX_RECORD_BYTES).map_err(|_| StoreError::Corrupt)?)
            .ok_or(StoreError::Corrupt)?
            > current_len
        {
            break;
        }
        let mut bytes = [0_u8; INDEX_RECORD_BYTES];
        if !read_exact_at(file, offset, &mut bytes)? {
            break;
        }
        let plaintext = match cipher.decrypt(
            Nonce::from_slice(&bytes[..12]),
            Payload {
                msg: &bytes[12..],
                aad: &aad,
            },
        ) {
            Ok(value) => value,
            Err(_) => break,
        };
        let record = records
            .get(usize::try_from(valid_records).map_err(|_| StoreError::Corrupt)?)
            .ok_or(StoreError::Corrupt)?;
        if plaintext.len() != INDEX_PLAINTEXT_BYTES
            || read_u64(&plaintext[..8]) != record.sequence
            || read_u64(&plaintext[8..16]) != record.record_offset
            || read_u64(&plaintext[16..24]) != record.start_offset_ns.unwrap_or(MISSING_OFFSET)
            || read_u64(&plaintext[24..32]) != record.duration_ns
            || read_u32(&plaintext[32..36]) != record.frame_count
            || u64::from(read_u32(&plaintext[36..40])) != record.record_bytes
            || read_u64(&plaintext[40..48]) != record.epoch
        {
            break;
        }
        valid_records = valid_records.checked_add(1).ok_or(StoreError::Corrupt)?;
    }
    if valid_records == u64::try_from(records.len()).map_err(|_| StoreError::Corrupt)? {
        Ok(expected_len)
    } else {
        Ok(valid_records
            .checked_mul(u64::try_from(INDEX_RECORD_BYTES).map_err(|_| StoreError::Corrupt)?)
            .ok_or(StoreError::Corrupt)?)
    }
}

fn audio_aad(track_id: SourceTrackId, header: &[u8; RECORD_HEADER_BYTES]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(16 + header.len());
    aad.extend_from_slice(track_id.uuid().as_bytes());
    aad.extend_from_slice(header);
    aad
}

fn index_aad(track_id: SourceTrackId) -> [u8; 21] {
    let mut aad = [0_u8; 21];
    aad[..16].copy_from_slice(track_id.uuid().as_bytes());
    aad[16..].copy_from_slice(b"index");
    aad
}

fn read_exact_at(file: &mut File, offset: u64, buffer: &mut [u8]) -> Result<bool, StoreError> {
    file.seek(SeekFrom::Start(offset))?;
    let mut read = 0_usize;
    while read < buffer.len() {
        let count = file.read(&mut buffer[read..])?;
        if count == 0 {
            return Ok(false);
        }
        read = read.checked_add(count).ok_or(StoreError::Corrupt)?;
    }
    Ok(true)
}

fn open_private_append_file(path: &Path) -> Result<File, StoreError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(StoreError::Invalid);
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    set_private_file_permissions(path)?;
    Ok(file)
}

fn ensure_private_directory(path: &Path) -> Result<(), StoreError> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StoreError::Invalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_private_file_permissions(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn sync_parent_directory(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        let parent = path.parent().ok_or(StoreError::Invalid)?;
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn validated_relative(root: &Path, relative: &str) -> Result<PathBuf, StoreError> {
    let candidate = Path::new(relative);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(StoreError::Invalid);
    }
    Ok(root.join(candidate))
}

fn phase_db(phase: MeetingPhase) -> &'static str {
    match phase {
        MeetingPhase::Preflight => "preflight",
        MeetingPhase::Starting => "starting",
        MeetingPhase::CapturingRecording => "capturing_recording",
        MeetingPhase::CapturingPausing => "capturing_pausing",
        MeetingPhase::CapturingPaused => "capturing_paused",
        MeetingPhase::CapturingResuming => "capturing_resuming",
        MeetingPhase::Stopping => "stopping",
        MeetingPhase::Processing => "processing",
        MeetingPhase::ReviewReady => "review_ready",
        MeetingPhase::RecoveryRequired => "recovery_required",
        MeetingPhase::Deleting => "deleting",
    }
}

fn cloud_outbox_kind_to_db(kind: CloudOutboxKind) -> &'static str {
    match kind {
        CloudOutboxKind::Object => "object",
        CloudOutboxKind::Tombstone => "tombstone",
        CloudOutboxKind::Share => "share",
    }
}

fn cloud_outbox_kind_from_db(value: &str) -> Result<CloudOutboxKind, StoreError> {
    match value {
        "object" => Ok(CloudOutboxKind::Object),
        "tombstone" => Ok(CloudOutboxKind::Tombstone),
        "share" => Ok(CloudOutboxKind::Share),
        _ => Err(StoreError::Corrupt),
    }
}

fn cloud_outbox_state_to_db(state: CloudOutboxState) -> &'static str {
    match state {
        CloudOutboxState::Pending => "pending",
        CloudOutboxState::Claimed => "claimed",
        CloudOutboxState::Completed => "completed",
        CloudOutboxState::Cancelled => "cancelled",
        CloudOutboxState::Terminal => "terminal",
    }
}

fn cloud_outbox_state_from_db(value: &str) -> Result<CloudOutboxState, StoreError> {
    match value {
        "pending" => Ok(CloudOutboxState::Pending),
        "claimed" => Ok(CloudOutboxState::Claimed),
        "completed" => Ok(CloudOutboxState::Completed),
        "cancelled" => Ok(CloudOutboxState::Cancelled),
        "terminal" => Ok(CloudOutboxState::Terminal),
        _ => Err(StoreError::Corrupt),
    }
}

fn cloud_share_state_to_db(state: CloudShareState) -> &'static str {
    match state {
        CloudShareState::Pending => "pending",
        CloudShareState::Active => "active",
        CloudShareState::Revoked => "revoked",
        CloudShareState::Failed => "failed",
    }
}

fn cloud_share_state_from_db(value: &str) -> Result<CloudShareState, StoreError> {
    match value {
        "pending" => Ok(CloudShareState::Pending),
        "active" => Ok(CloudShareState::Active),
        "revoked" => Ok(CloudShareState::Revoked),
        "failed" => Ok(CloudShareState::Failed),
        _ => Err(StoreError::Corrupt),
    }
}

fn cloud_share_content_kind_to_db(kind: CloudShareContentKind) -> &'static str {
    match kind {
        CloudShareContentKind::CapabilityBundle => "capability_bundle",
        CloudShareContentKind::BrowserMarkdown => "browser_markdown",
    }
}

fn cloud_share_content_kind_from_db(value: &str) -> Result<CloudShareContentKind, StoreError> {
    match value {
        "capability_bundle" => Ok(CloudShareContentKind::CapabilityBundle),
        "browser_markdown" => Ok(CloudShareContentKind::BrowserMarkdown),
        _ => Err(StoreError::Corrupt),
    }
}

fn cloud_head_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CloudHead> {
    let source_session_id: Option<String> = row.get(1)?;
    let sequence: i64 = row.get(5)?;
    Ok(CloudHead {
        object_id: row.get(0)?,
        source_session_id: source_session_id
            .as_deref()
            .map(parse_uuid)
            .transpose()
            .map_err(to_sql_error)?
            .map(MeetingSessionId::from_uuid),
        remote_revision_id: row.get(2)?,
        tombstone: row.get::<_, i64>(3)? != 0,
        acknowledged_revision_id: row.get(4)?,
        change_sequence: from_i64(sequence).map_err(to_sql_error)?,
    })
}

fn cloud_head_in(
    connection: &Connection,
    object_id: &str,
) -> Result<Option<CloudHead>, StoreError> {
    connection
        .query_row(
            "SELECT object_id, source_session_id, remote_revision_id, tombstone,
                    acknowledged_revision_id, change_sequence
             FROM meeting_cloud_heads WHERE object_id = ?1",
            params![object_id],
            cloud_head_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn upsert_cloud_head_in(transaction: &Transaction<'_>, head: &CloudHead) -> Result<(), StoreError> {
    validate_cloud_head(head)?;
    let changed = transaction.execute(
        "INSERT INTO meeting_cloud_heads (
            object_id, source_session_id, remote_revision_id, tombstone,
            acknowledged_revision_id, change_sequence, updated_at_utc_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(object_id) DO UPDATE SET
            source_session_id = excluded.source_session_id,
            remote_revision_id = excluded.remote_revision_id,
            tombstone = excluded.tombstone,
            acknowledged_revision_id = excluded.acknowledged_revision_id,
            change_sequence = excluded.change_sequence,
            updated_at_utc_ms = excluded.updated_at_utc_ms
         WHERE excluded.change_sequence >= meeting_cloud_heads.change_sequence",
        params![
            head.object_id,
            head.source_session_id.map(id),
            head.remote_revision_id,
            bool_to_i64(head.tombstone),
            head.acknowledged_revision_id,
            to_i64(head.change_sequence)?,
            utc_now_ms(),
        ],
    )?;
    if changed != 1 {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn cloud_outbox_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CloudOutboxRecord> {
    let source_session_id: Option<String> = row.get(3)?;
    let source_revision: Option<i64> = row.get(4)?;
    let share_content_kind: Option<String> = row.get(6)?;
    let attempt_count: i64 = row.get(11)?;
    Ok(CloudOutboxRecord {
        outbox_id: row.get(0)?,
        kind: cloud_outbox_kind_from_db(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
        object_id: row.get(2)?,
        source_session_id: source_session_id
            .as_deref()
            .map(parse_uuid)
            .transpose()
            .map_err(to_sql_error)?
            .map(MeetingSessionId::from_uuid),
        source_revision: source_revision
            .map(from_i64)
            .transpose()
            .map_err(to_sql_error)?,
        base_remote_revision_id: row.get(5)?,
        share_content_kind: share_content_kind
            .as_deref()
            .map(cloud_share_content_kind_from_db)
            .transpose()
            .map_err(to_sql_error)?,
        remote_revision_id: row.get(7)?,
        upload_id: row.get(8)?,
        idempotency_key: row.get(9)?,
        state: cloud_outbox_state_from_db(&row.get::<_, String>(10)?).map_err(to_sql_error)?,
        attempt_count: u32::try_from(attempt_count)
            .map_err(|_| to_sql_error(StoreError::Corrupt))?,
        next_attempt_utc_ms: row.get(12)?,
        terminal_error: row.get(13)?,
        payload_relative_dir: row.get(14)?,
        claim_token: row.get(15)?,
    })
}

fn cloud_outbox_in(
    connection: &Connection,
    outbox_id: &str,
) -> Result<Option<CloudOutboxRecord>, StoreError> {
    connection
        .query_row(
            "SELECT outbox_id, kind, object_id, source_session_id, source_revision,
                    base_remote_revision_id, share_content_kind, remote_revision_id, upload_id,
                    idempotency_key, state, attempt_count, next_attempt_utc_ms, terminal_error,
                    payload_relative_dir, claim_token
             FROM meeting_cloud_outbox WHERE outbox_id = ?1",
            params![outbox_id],
            cloud_outbox_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn cloud_outbox_by_idempotency_in(
    connection: &Connection,
    idempotency_key: &str,
) -> Result<Option<CloudOutboxRecord>, StoreError> {
    connection
        .query_row(
            "SELECT outbox_id, kind, object_id, source_session_id, source_revision,
                    base_remote_revision_id, share_content_kind, remote_revision_id, upload_id,
                    idempotency_key, state, attempt_count, next_attempt_utc_ms, terminal_error,
                    payload_relative_dir, claim_token
             FROM meeting_cloud_outbox WHERE idempotency_key = ?1",
            params![idempotency_key],
            cloud_outbox_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn cloud_conflict_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CloudConflict> {
    let source_session_id: Option<String> = row.get(1)?;
    let source_revision: Option<i64> = row.get(2)?;
    Ok(CloudConflict {
        object_id: row.get(0)?,
        source_session_id: source_session_id
            .as_deref()
            .map(parse_uuid)
            .transpose()
            .map_err(to_sql_error)?
            .map(MeetingSessionId::from_uuid),
        source_revision: source_revision
            .map(from_i64)
            .transpose()
            .map_err(to_sql_error)?,
        remote_revision_id: row.get(3)?,
        remote_sequence: from_i64(row.get(4)?).map_err(to_sql_error)?,
        remote_bundle_relative_path: row.get(5)?,
    })
}

fn cloud_conflict_in(
    connection: &Connection,
    object_id: &str,
) -> Result<Option<CloudConflict>, StoreError> {
    connection
        .query_row(
            "SELECT object_id, source_session_id, source_revision, remote_revision_id,
                    remote_sequence, remote_bundle_relative_path
             FROM meeting_cloud_conflicts WHERE object_id = ?1",
            params![object_id],
            cloud_conflict_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn cloud_share_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CloudShareRecord> {
    let source_session_id: Option<String> = row.get(2)?;
    Ok(CloudShareRecord {
        share_id: row.get(0)?,
        object_id: row.get(1)?,
        source_session_id: source_session_id
            .as_deref()
            .map(parse_uuid)
            .transpose()
            .map_err(to_sql_error)?
            .map(MeetingSessionId::from_uuid),
        expires_at_utc_ms: row.get(3)?,
        state: cloud_share_state_from_db(&row.get::<_, String>(4)?).map_err(to_sql_error)?,
        content_kind: cloud_share_content_kind_from_db(&row.get::<_, String>(5)?)
            .map_err(to_sql_error)?,
        encrypted_link_material: row.get(6)?,
        outbox_id: row.get(7)?,
        revoked_at_utc_ms: row.get(8)?,
    })
}

fn cloud_share_in(
    connection: &Connection,
    share_id: &str,
) -> Result<Option<CloudShareRecord>, StoreError> {
    connection
        .query_row(
            "SELECT share_id, object_id, source_session_id, expires_at_utc_ms, state,
                    content_kind, encrypted_link_material, outbox_id, revoked_at_utc_ms
             FROM meeting_cloud_shares WHERE share_id = ?1",
            params![share_id],
            cloud_share_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn validate_cloud_text(value: &str) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > 4_096 || value.contains(char::from(0)) {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

fn validate_optional_cloud_text(value: &Option<String>) -> Result<(), StoreError> {
    value.as_deref().map(validate_cloud_text).transpose()?;
    Ok(())
}

fn validate_cloud_identifier(value: &str) -> Result<(), StoreError> {
    validate_cloud_text(value)?;
    if value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

fn validate_optional_cloud_identifier(value: &Option<String>) -> Result<(), StoreError> {
    value
        .as_deref()
        .map(validate_cloud_identifier)
        .transpose()?;
    Ok(())
}

fn validate_cloud_local_id(value: &str) -> Result<(), StoreError> {
    Uuid::parse_str(value).map_err(|_| StoreError::Invalid)?;
    Ok(())
}

fn validate_cloud_state(state: &CloudState) -> Result<(), StoreError> {
    validate_cloud_identifier(&state.vault_id)?;
    validate_cloud_identifier(&state.device_id)?;
    validate_cloud_text(&state.endpoint)?;
    validate_optional_cloud_text(&state.cursor)?;
    validate_optional_cloud_text(&state.snapshot_high_water)
}

fn validate_cloud_head(head: &CloudHead) -> Result<(), StoreError> {
    validate_cloud_identifier(&head.object_id)?;
    validate_optional_cloud_identifier(&head.remote_revision_id)?;
    validate_optional_cloud_identifier(&head.acknowledged_revision_id)?;
    if head.tombstone && head.remote_revision_id.is_none() {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

fn validate_cloud_outbox_input(input: &CloudOutboxInput) -> Result<(), StoreError> {
    validate_cloud_identifier(&input.object_id)?;
    validate_optional_cloud_identifier(&input.base_remote_revision_id)?;
    validate_optional_cloud_identifier(&input.remote_revision_id)?;
    validate_cloud_text(&input.idempotency_key)?;
    match input.kind {
        CloudOutboxKind::Object => {
            if input.source_session_id.is_none()
                || input.source_revision.is_none()
                || input.share_content_kind.is_some()
            {
                return Err(StoreError::Invalid);
            }
        }
        CloudOutboxKind::Tombstone => {
            if input.share_content_kind.is_some() {
                return Err(StoreError::Invalid);
            }
        }
        CloudOutboxKind::Share => {
            if input.share_content_kind.is_none() {
                return Err(StoreError::Invalid);
            }
        }
    }
    Ok(())
}

fn validate_cloud_outbox_source(
    transaction: &Transaction<'_>,
    input: &CloudOutboxInput,
) -> Result<(), StoreError> {
    let Some(session_id) = input.source_session_id else {
        return Ok(());
    };
    let session = session_row(transaction, session_id)?;
    match input.kind {
        CloudOutboxKind::Object | CloudOutboxKind::Share => {
            if !matches!(
                session.phase,
                MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired
            ) || input
                .source_revision
                .is_some_and(|revision| revision != session.revision)
            {
                return Err(StoreError::Conflict);
            }
        }
        CloudOutboxKind::Tombstone => {
            if session.phase != MeetingPhase::Deleting
                || input
                    .source_revision
                    .is_some_and(|revision| revision != session.revision)
            {
                return Err(StoreError::Conflict);
            }
        }
    }
    Ok(())
}

fn validate_cloud_outbox_update(update: &CloudOutboxUpdate) -> Result<(), StoreError> {
    validate_optional_cloud_identifier(&update.remote_revision_id)?;
    validate_optional_cloud_identifier(&update.upload_id)?;
    validate_optional_cloud_text(&update.terminal_error)?;
    match update.state {
        CloudOutboxState::Completed if update.terminal_error.is_none() => Ok(()),
        CloudOutboxState::Terminal if update.terminal_error.is_some() => Ok(()),
        _ => Err(StoreError::Invalid),
    }
}

fn validate_cloud_chunks(chunks: &[CloudOutboxChunk]) -> Result<(), StoreError> {
    let mut indices = HashSet::new();
    for chunk in chunks {
        if chunk.accepted || !indices.insert(chunk.chunk_index) {
            return Err(StoreError::Invalid);
        }
        validate_cloud_text(&chunk.sha256)?;
    }
    Ok(())
}

fn cloud_outbox_relative_dir(outbox_id: &str) -> String {
    format!(".cloud-outbox/{outbox_id}")
}

fn validated_cloud_outbox_dir(
    root: &Path,
    outbox_id: &str,
    relative: &str,
) -> Result<PathBuf, StoreError> {
    if relative != cloud_outbox_relative_dir(outbox_id) {
        return Err(StoreError::Invalid);
    }
    validated_relative(root, relative)
}

fn validated_cloud_conflict_bundle_path(
    root: &Path,
    relative: &str,
) -> Result<PathBuf, StoreError> {
    if !relative.starts_with(".cloud-conflicts/") || !relative.ends_with(".bundle") {
        return Err(StoreError::Invalid);
    }
    validated_relative(root, relative)
}

fn validate_cloud_conflict(conflict: &CloudConflict) -> Result<(), StoreError> {
    validate_cloud_identifier(&conflict.object_id)?;
    validate_cloud_identifier(&conflict.remote_revision_id)?;
    if conflict.remote_bundle_relative_path.len() > 256 {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

fn validate_cloud_share_input(input: &CloudShareInput) -> Result<(), StoreError> {
    validate_cloud_identifier(&input.share_id)?;
    validate_cloud_identifier(&input.object_id)?;
    validate_cloud_text(&input.encrypted_link_material)?;
    if let Some(outbox_id) = &input.outbox_id {
        validate_cloud_local_id(outbox_id)?;
    }
    Ok(())
}

fn validate_cloud_share_source(
    transaction: &Transaction<'_>,
    session_id: Option<MeetingSessionId>,
) -> Result<(), StoreError> {
    let session_id = session_id.ok_or(StoreError::Invalid)?;
    let session = session_row(transaction, session_id)?;
    if !matches!(
        session.phase,
        MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired
    ) {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn validate_cloud_share_update(update: &CloudShareUpdate) -> Result<(), StoreError> {
    if let Some(outbox_id) = &update.outbox_id {
        validate_cloud_local_id(outbox_id)?;
    }
    match update.state {
        CloudShareState::Revoked if update.revoked_at_utc_ms.is_some() => Ok(()),
        CloudShareState::Revoked => Err(StoreError::Invalid),
        _ if update.revoked_at_utc_ms.is_none() => Ok(()),
        _ => Err(StoreError::Invalid),
    }
}

fn cloud_bundle_task_state_from_db(
    value: &str,
) -> Result<cloud_bundle::CloudBundleTaskState, StoreError> {
    match value {
        "running" => Ok(cloud_bundle::CloudBundleTaskState::Running),
        "completed" => Ok(cloud_bundle::CloudBundleTaskState::Completed),
        "failed" => Ok(cloud_bundle::CloudBundleTaskState::Failed),
        _ => Err(StoreError::Corrupt),
    }
}

fn cloud_bundle_task_state_to_db(state: cloud_bundle::CloudBundleTaskState) -> &'static str {
    match state {
        cloud_bundle::CloudBundleTaskState::Running => "running",
        cloud_bundle::CloudBundleTaskState::Completed => "completed",
        cloud_bundle::CloudBundleTaskState::Failed => "failed",
    }
}

fn export_cloud_meeting_bundle_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<cloud_bundle::CloudMeetingBundleV1, StoreError> {
    let session = connection
        .query_row(
            "SELECT phase, revision, title, origin_kind, preflight_json, created_at_utc_ms,
                started_at_utc_ms, ended_at_utc_ms, recovered_at_utc_ms, successful_plan_id,
                processing_status, retention_policy_json, delete_after_utc_ms,
                current_transcript_revision_id, current_diarization_generation_id,
                diarization_status, diarization_model_id, diarization_model_version
         FROM meeting_sessions WHERE id = ?1",
            params![id(session_id)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, Option<String>>(17)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound)?;
    let phase = phase_from_db(&session.0)?;
    if !matches!(
        phase,
        MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired
    ) {
        return Err(StoreError::Conflict);
    }
    let bundle_session = cloud_bundle::CloudBundleSession {
        session_id,
        phase,
        revision: from_i64(session.1)?,
        title: session.2,
        origin: decode_json(&session.3)?,
        preflight: decode_json(&session.4)?,
        created_at_utc_ms: session.5,
        started_at_utc_ms: session.6,
        ended_at_utc_ms: session.7,
        recovered_at_utc_ms: session.8,
        successful_plan_id: session
            .9
            .as_deref()
            .map(parse_uuid)
            .transpose()?
            .map(MeetingPlanId::from_uuid),
        processing_status: decode_json(&session.10)?,
        retention_policy: decode_json(&session.11)?,
        delete_after_utc_ms: session.12,
        current_transcript_revision_id: session
            .13
            .as_deref()
            .map(parse_uuid)
            .transpose()?
            .map(TranscriptRevisionId::from_uuid),
        current_diarization_generation_id: session
            .14
            .as_deref()
            .map(parse_uuid)
            .transpose()?
            .map(MeetingDiarizationGenerationId::from_uuid),
        diarization_status: diarization_status_from_db(&session.15)?,
        diarization_model_id: session.16,
        diarization_model_version: session.17,
    };

    let mut statement = connection.prepare(
        "SELECT plan_id, consent_id, attempt_number, schema_version, canonical_plan_json, created_at_utc_ms
         FROM meeting_run_plans WHERE session_id = ?1 ORDER BY attempt_number",
    )?;
    let run_plans = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleRunPlan {
                plan_id: MeetingPlanId::from_uuid(parse_uuid(&row.0)?),
                consent_id: ConsentId::from_uuid(parse_uuid(&row.1)?),
                attempt_number: u32::try_from(row.2).map_err(|_| StoreError::Corrupt)?,
                schema_version: u32::try_from(row.3).map_err(|_| StoreError::Corrupt)?,
                canonical_plan: decode_json(&row.4)?,
                created_at_utc_ms: row.5,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    let mut statement = connection.prepare(
        "SELECT consent_id, attempt_number, preflight_revision, policy_version, acknowledgement_json,
                acknowledged_at_utc_ms FROM meeting_consents WHERE session_id = ?1 ORDER BY attempt_number",
    )?;
    let consents = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleConsent {
                consent_id: ConsentId::from_uuid(parse_uuid(&row.0)?),
                attempt_number: u32::try_from(row.1).map_err(|_| StoreError::Corrupt)?,
                preflight_revision: from_i64(row.2)?,
                policy_version: u32::try_from(row.3).map_err(|_| StoreError::Corrupt)?,
                acknowledgement: decode_json(&row.4)?,
                acknowledged_at_utc_ms: row.5,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    let mut statement = connection.prepare(
        "SELECT track_id, plan_id, source_kind, required, requested, descriptor_json,
                timestamp_bridge_json, format_json, first_offset_ns, last_offset_ns, health
         FROM meeting_source_tracks WHERE session_id = ?1 ORDER BY source_kind",
    )?;
    let source_tracks = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, String>(10)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleSourceTrack {
                track_id: SourceTrackId::from_uuid(parse_uuid(&row.0)?),
                plan_id: MeetingPlanId::from_uuid(parse_uuid(&row.1)?),
                source_kind: source_kind_from_db(&row.2)?,
                required: row.3 != 0,
                requested: row.4 != 0,
                descriptor_json: row.5,
                timestamp_bridge: decode_json(&row.6)?,
                format: row.7.as_deref().map(decode_json).transpose()?,
                first_offset_ns: row.8.map(from_i64).transpose()?,
                last_offset_ns: row.9.map(from_i64).transpose()?,
                health: decode_json(&row.10)?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    let mut statement = connection.prepare(
        "SELECT e.track_id, e.source_epoch, e.format_epoch, e.bridge_json,
                e.observed_host_monotonic_ns
         FROM meeting_source_clock_epochs e
         JOIN meeting_source_tracks t ON t.track_id = e.track_id
         WHERE t.session_id = ?1 ORDER BY e.track_id, e.source_epoch, e.format_epoch",
    )?;
    let source_clock_epochs = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleSourceClockEpoch {
                track_id: SourceTrackId::from_uuid(parse_uuid(&row.0)?),
                source_epoch: SourceEpoch::new(from_i64(row.1)?),
                format_epoch: from_i64(row.2)?,
                bridge: decode_json(&row.3)?,
                observed_host_monotonic_ns: from_i64(row.4)?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    let mut statement = connection.prepare(
        "SELECT sequence, start_offset_ns, end_offset_ns, close_reason
         FROM meeting_capture_windows WHERE session_id = ?1 ORDER BY sequence",
    )?;
    let capture_windows = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleCaptureWindow {
                sequence: from_i64(row.0)?,
                start_offset_ns: from_i64(row.1)?,
                end_offset_ns: row.2.map(from_i64).transpose()?,
                close_reason: row.3,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    let mut statement = connection.prepare(
        "SELECT g.track_id, g.source_epoch, g.start_offset_ns, g.end_offset_ns, g.reason,
                g.dropped_frames, g.observed_at_utc_ms
         FROM meeting_source_gaps g JOIN meeting_source_tracks t ON t.track_id = g.track_id
         WHERE t.session_id = ?1 ORDER BY g.gap_id",
    )?;
    let source_gaps = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleSourceGap {
                track_id: SourceTrackId::from_uuid(parse_uuid(&row.0)?),
                source_epoch: SourceEpoch::new(from_i64(row.1)?),
                start_offset_ns: row.2.map(from_i64).transpose()?,
                end_offset_ns: row.3.map(from_i64).transpose()?,
                reason: decode_json(&row.4)?,
                dropped_frames: row.5.map(from_i64).transpose()?,
                observed_at_utc_ms: row.6,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    let mut statement = connection.prepare(
        "SELECT speaker_id, source_kind, display_name, revision, merged_into_speaker_id
         FROM meeting_speakers WHERE session_id = ?1 ORDER BY speaker_id",
    )?;
    let speakers = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleSpeaker {
                speaker_id: SpeakerId::from_uuid(parse_uuid(&row.0)?),
                source_kind: source_kind_from_db(&row.1)?,
                display_name: row.2,
                revision: from_i64(row.3)?,
                merged_into_speaker_id: row
                    .4
                    .as_deref()
                    .map(parse_uuid)
                    .transpose()?
                    .map(SpeakerId::from_uuid),
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    let mut statement = connection.prepare(
        "SELECT transcript_revision_id, engine_id, model_version, destination_json, source_set_json,
                language, state, created_at_utc_ms, completed_at_utc_ms, error_code
         FROM meeting_transcript_revisions WHERE session_id = ?1 ORDER BY created_at_utc_ms, transcript_revision_id",
    )?;
    let transcript_revisions = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleTranscriptRevision {
                transcript_revision_id: TranscriptRevisionId::from_uuid(parse_uuid(&row.0)?),
                engine_id: row.1,
                model_version: row.2,
                destination: decode_json(&row.3)?,
                source_set: decode_json(&row.4)?,
                language: row.5,
                state: cloud_bundle_task_state_from_db(&row.6)?,
                created_at_utc_ms: row.7,
                completed_at_utc_ms: row.8,
                error_code: row.9,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    let mut statement = connection.prepare(
        "SELECT s.segment_id, s.transcript_revision_id, s.track_id, s.ordinal, s.start_offset_ns,
                s.end_offset_ns, s.speaker_id, s.base_text, s.confidence_milli
         FROM meeting_transcript_segments s
         JOIN meeting_transcript_revisions r ON r.transcript_revision_id = s.transcript_revision_id
         WHERE r.session_id = ?1 ORDER BY s.transcript_revision_id, s.ordinal",
    )?;
    let transcript_segments = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<i64>>(8)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleTranscriptSegment {
                segment_id: TranscriptSegmentId::from_uuid(parse_uuid(&row.0)?),
                transcript_revision_id: TranscriptRevisionId::from_uuid(parse_uuid(&row.1)?),
                track_id: SourceTrackId::from_uuid(parse_uuid(&row.2)?),
                ordinal: from_i64(row.3)?,
                start_offset_ns: from_i64(row.4)?,
                end_offset_ns: from_i64(row.5)?,
                speaker_id: SpeakerId::from_uuid(parse_uuid(&row.6)?),
                base_text: row.7,
                confidence_milli: row
                    .8
                    .map(|value| u16::try_from(value).map_err(|_| StoreError::Corrupt))
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    let mut statement = connection.prepare(
        "SELECT e.segment_id, e.edit_sequence, e.replacement_text, e.removed, e.operator_at_utc_ms
         FROM meeting_segment_edits e
         JOIN meeting_transcript_segments s ON s.segment_id = e.segment_id
         JOIN meeting_transcript_revisions r ON r.transcript_revision_id = s.transcript_revision_id
         WHERE r.session_id = ?1 ORDER BY e.segment_id, e.edit_sequence",
    )?;
    let segment_edits = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleSegmentEdit {
                segment_id: TranscriptSegmentId::from_uuid(parse_uuid(&row.0)?),
                edit_sequence: from_i64(row.1)?,
                replacement_text: row.2,
                removed: row.3 != 0,
                operator_at_utc_ms: row.4,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    let notes = notes_for_session(connection, session_id)?;
    let mut statement = connection.prepare(
        "SELECT artifact_id, kind, transcript_revision_id, input_revision, state, created_at_utc_ms
         FROM meeting_artifacts WHERE session_id = ?1 ORDER BY created_at_utc_ms, artifact_id",
    )?;
    let artifacts = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleArtifact {
                artifact_id: MeetingArtifactId::from_uuid(parse_uuid(&row.0)?),
                kind: artifact_kind_from_db(&row.1)?,
                transcript_revision_id: row
                    .2
                    .as_deref()
                    .map(parse_uuid)
                    .transpose()?
                    .map(TranscriptRevisionId::from_uuid),
                input_revision: from_i64(row.3)?,
                state: artifact_state_from_db(&row.4)?,
                created_at_utc_ms: row.5,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    let artifact_revisions = artifact_revisions_for_session(connection, session_id)?;
    let questions = question_history_for_session(connection, session_id)?;
    let mut statement = connection.prepare(
        "SELECT generation_id, transcript_revision_id, input_revision, model_id, model_version,
                state, created_at_utc_ms, completed_at_utc_ms
         FROM meeting_diarization_generations WHERE session_id = ?1 ORDER BY created_at_utc_ms, generation_id",
    )?;
    let diarization_generations = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Option<i64>>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleDiarizationGeneration {
                generation_id: MeetingDiarizationGenerationId::from_uuid(parse_uuid(&row.0)?),
                transcript_revision_id: TranscriptRevisionId::from_uuid(parse_uuid(&row.1)?),
                input_revision: from_i64(row.2)?,
                model_id: row.3,
                model_version: row.4,
                state: cloud_bundle_task_state_from_db(&row.5)?,
                created_at_utc_ms: row.6,
                completed_at_utc_ms: row.7,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    let mut statement = connection.prepare(
        "SELECT a.generation_id, a.segment_id, a.speaker_id, a.assignment_kind
         FROM meeting_diarization_assignments a
         JOIN meeting_diarization_generations g ON g.generation_id = a.generation_id
         WHERE g.session_id = ?1 ORDER BY a.generation_id, a.segment_id",
    )?;
    let diarization_assignments = statement
        .query_map(params![id(session_id)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| {
            Ok(cloud_bundle::CloudBundleDiarizationAssignment {
                generation_id: MeetingDiarizationGenerationId::from_uuid(parse_uuid(&row.0)?),
                segment_id: TranscriptSegmentId::from_uuid(parse_uuid(&row.1)?),
                speaker_id: SpeakerId::from_uuid(parse_uuid(&row.2)?),
                assignment_kind: speaker_assignment_from_db(&row.3)?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

    Ok(cloud_bundle::CloudMeetingBundleV1 {
        format_version: cloud_bundle::CLOUD_MEETING_BUNDLE_VERSION,
        audio_included: false,
        session: bundle_session,
        run_plans,
        consents,
        source_tracks,
        source_clock_epochs,
        capture_windows,
        source_gaps,
        speakers,
        transcript_revisions,
        transcript_segments,
        segment_edits,
        notes,
        artifacts,
        artifact_revisions,
        questions,
        diarization_generations,
        diarization_assignments,
    })
}

fn import_cloud_meeting_bundle_in(
    transaction: &Transaction<'_>,
    bundle: &cloud_bundle::CloudMeetingBundleV1,
) -> Result<(), StoreError> {
    let session_id = bundle.session.session_id;
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM meeting_sessions WHERE id = ?1)",
        params![id(session_id)],
        |row| row.get(0),
    )?;
    if exists {
        return Err(StoreError::Conflict);
    }
    transaction.execute(
        "INSERT INTO meeting_sessions (
            id, phase, revision, title, origin_kind, preflight_json, created_at_utc_ms,
            started_at_utc_ms, ended_at_utc_ms, recovered_at_utc_ms, successful_plan_id,
            processing_status, retention_policy_json, delete_after_utc_ms,
            current_transcript_revision_id, current_diarization_generation_id,
            diarization_status, diarization_model_id, diarization_model_version
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, ?12, ?13,
                   NULL, NULL, ?14, ?15, ?16)",
        params![
            id(session_id),
            phase_db(bundle.session.phase),
            to_i64(bundle.session.revision)?,
            bundle.session.title,
            encode_json(&bundle.session.origin)?,
            encode_json(&bundle.session.preflight)?,
            bundle.session.created_at_utc_ms,
            bundle.session.started_at_utc_ms,
            bundle.session.ended_at_utc_ms,
            bundle.session.recovered_at_utc_ms,
            encode_json(&bundle.session.processing_status)?,
            encode_json(&bundle.session.retention_policy)?,
            bundle.session.delete_after_utc_ms,
            diarization_status_to_db(bundle.session.diarization_status),
            bundle.session.diarization_model_id,
            bundle.session.diarization_model_version,
        ],
    )?;

    for consent in &bundle.consents {
        transaction.execute(
            "INSERT INTO meeting_consents (
                consent_id, session_id, attempt_number, preflight_revision, policy_version,
                acknowledgement_json, acknowledged_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id(consent.consent_id),
                id(session_id),
                i64::from(consent.attempt_number),
                to_i64(consent.preflight_revision)?,
                i64::from(consent.policy_version),
                encode_json(&consent.acknowledgement)?,
                consent.acknowledged_at_utc_ms,
            ],
        )?;
    }
    for plan in &bundle.run_plans {
        transaction.execute(
            "INSERT INTO meeting_run_plans (
                plan_id, session_id, attempt_number, schema_version, consent_id,
                canonical_plan_json, created_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id(plan.plan_id),
                id(session_id),
                i64::from(plan.attempt_number),
                i64::from(plan.schema_version),
                id(plan.consent_id),
                encode_json(&plan.canonical_plan)?,
                plan.created_at_utc_ms,
            ],
        )?;
    }
    for window in &bundle.capture_windows {
        transaction.execute(
            "INSERT INTO meeting_capture_windows (
                session_id, sequence, start_offset_ns, end_offset_ns, close_reason
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id(session_id),
                to_i64(window.sequence)?,
                to_i64(window.start_offset_ns)?,
                window.end_offset_ns.map(to_i64).transpose()?,
                window.close_reason,
            ],
        )?;
    }
    for track in &bundle.source_tracks {
        transaction.execute(
            "INSERT INTO meeting_source_tracks (
                track_id, session_id, plan_id, source_kind, required, requested, descriptor_json,
                timestamp_bridge_json, format_json, first_offset_ns, last_offset_ns, health
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id(track.track_id),
                id(session_id),
                id(track.plan_id),
                track.source_kind.as_str(),
                bool_to_i64(track.required),
                bool_to_i64(track.requested),
                track.descriptor_json,
                encode_json(&track.timestamp_bridge)?,
                track.format.as_ref().map(encode_json).transpose()?,
                track.first_offset_ns.map(to_i64).transpose()?,
                track.last_offset_ns.map(to_i64).transpose()?,
                encode_json(&track.health)?,
            ],
        )?;
    }
    for epoch in &bundle.source_clock_epochs {
        transaction.execute(
            "INSERT INTO meeting_source_clock_epochs (
                track_id, source_epoch, format_epoch, bridge_json, observed_host_monotonic_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id(epoch.track_id),
                to_i64(epoch.source_epoch.get())?,
                to_i64(epoch.format_epoch)?,
                encode_json(&epoch.bridge)?,
                to_i64(epoch.observed_host_monotonic_ns)?,
            ],
        )?;
    }
    for gap in &bundle.source_gaps {
        transaction.execute(
            "INSERT INTO meeting_source_gaps (
                track_id, source_epoch, start_offset_ns, end_offset_ns, reason, dropped_frames,
                observed_at_utc_ms, details_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '{}')",
            params![
                id(gap.track_id),
                to_i64(gap.source_epoch.get())?,
                gap.start_offset_ns.map(to_i64).transpose()?,
                gap.end_offset_ns.map(to_i64).transpose()?,
                encode_json(&gap.reason)?,
                gap.dropped_frames.map(to_i64).transpose()?,
                gap.observed_at_utc_ms,
            ],
        )?;
    }
    for speaker in &bundle.speakers {
        transaction.execute(
            "INSERT INTO meeting_speakers (
                speaker_id, session_id, source_kind, display_name, revision, merged_into_speaker_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![
                id(speaker.speaker_id),
                id(session_id),
                speaker.source_kind.as_str(),
                speaker.display_name,
                to_i64(speaker.revision)?,
            ],
        )?;
    }
    for speaker in &bundle.speakers {
        if let Some(merged_into_speaker_id) = speaker.merged_into_speaker_id {
            transaction.execute(
                "UPDATE meeting_speakers SET merged_into_speaker_id = ?1 WHERE speaker_id = ?2",
                params![id(merged_into_speaker_id), id(speaker.speaker_id)],
            )?;
        }
    }
    for revision in &bundle.transcript_revisions {
        transaction.execute(
            "INSERT INTO meeting_transcript_revisions (
                transcript_revision_id, session_id, engine_id, model_version, destination_json,
                source_set_json, language, state, created_at_utc_ms, completed_at_utc_ms, error_code
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id(revision.transcript_revision_id),
                id(session_id),
                revision.engine_id,
                revision.model_version,
                encode_json(&revision.destination)?,
                encode_json(&revision.source_set)?,
                revision.language,
                cloud_bundle_task_state_to_db(revision.state),
                revision.created_at_utc_ms,
                revision.completed_at_utc_ms,
                revision.error_code,
            ],
        )?;
    }
    for segment in &bundle.transcript_segments {
        transaction.execute(
            "INSERT INTO meeting_transcript_segments (
                segment_id, transcript_revision_id, track_id, ordinal, start_offset_ns,
                end_offset_ns, speaker_id, base_text, confidence_milli
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id(segment.segment_id),
                id(segment.transcript_revision_id),
                id(segment.track_id),
                to_i64(segment.ordinal)?,
                to_i64(segment.start_offset_ns)?,
                to_i64(segment.end_offset_ns)?,
                id(segment.speaker_id),
                segment.base_text,
                segment.confidence_milli.map(i64::from),
            ],
        )?;
    }
    for edit in &bundle.segment_edits {
        transaction.execute(
            "INSERT INTO meeting_segment_edits (
                segment_id, edit_sequence, replacement_text, removed, operator_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id(edit.segment_id),
                to_i64(edit.edit_sequence)?,
                edit.replacement_text,
                bool_to_i64(edit.removed),
                edit.operator_at_utc_ms,
            ],
        )?;
    }
    for note in &bundle.notes {
        transaction.execute(
            "INSERT INTO meeting_notes (
                note_id, session_id, start_offset_ns, end_offset_ns, body, note_revision,
                created_at_utc_ms, updated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id(note.note_id),
                id(session_id),
                note.start_offset_ns.map(to_i64).transpose()?,
                note.end_offset_ns.map(to_i64).transpose()?,
                note.body,
                to_i64(note.revision)?,
                note.created_at_utc_ms,
                note.updated_at_utc_ms,
            ],
        )?;
    }
    for artifact in &bundle.artifacts {
        transaction.execute(
            "INSERT INTO meeting_artifacts (
                artifact_id, session_id, kind, transcript_revision_id, input_revision, state,
                created_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id(artifact.artifact_id),
                id(session_id),
                cloud_artifact_kind_to_db(artifact.kind),
                artifact.transcript_revision_id.map(id),
                to_i64(artifact.input_revision)?,
                artifact_state_to_db(artifact.state),
                artifact.created_at_utc_ms,
            ],
        )?;
    }
    for artifact in &bundle.artifact_revisions {
        transaction.execute(
            "INSERT INTO meeting_artifact_revisions (
                artifact_id, session_id, transcript_revision_id, input_revision, template_id,
                template_version, generation_key, state, content_json, generated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id(artifact.artifact_id),
                id(session_id),
                id(artifact.transcript_revision_id),
                to_i64(artifact.input_revision)?,
                artifact.template_id,
                i64::from(artifact.template_version),
                artifact.generation_key,
                artifact_state_to_db(artifact.state),
                artifact.content.as_ref().map(encode_json).transpose()?,
                artifact.generated_at_utc_ms,
            ],
        )?;
    }
    for question in &bundle.questions {
        let question_text = question.question.as_deref().ok_or(StoreError::Invalid)?;
        transaction.execute(
            "INSERT INTO meeting_questions (
                question_id, session_id, question_text, answer_state, answer_text, revision,
                created_at_utc_ms, scope_json, input_revision
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id(question.question_id),
                id(session_id),
                question_text,
                meeting_answer_state_to_db(question.state),
                question.answer,
                to_i64(question.revision)?,
                question.created_at_utc_ms,
                encode_json(&question.scope)?,
                to_i64(question.input_revision)?,
            ],
        )?;
        for (ordinal, citation) in question.citations.iter().enumerate() {
            transaction.execute(
                "INSERT INTO meeting_question_citations (question_id, ordinal, citation_json)
                 VALUES (?1, ?2, ?3)",
                params![
                    id(question.question_id),
                    i64::try_from(ordinal).map_err(|_| StoreError::Invalid)?,
                    encode_json(citation)?,
                ],
            )?;
        }
    }
    for generation in &bundle.diarization_generations {
        transaction.execute(
            "INSERT INTO meeting_diarization_generations (
                generation_id, session_id, transcript_revision_id, input_revision, model_id,
                model_version, state, created_at_utc_ms, completed_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id(generation.generation_id),
                id(session_id),
                id(generation.transcript_revision_id),
                to_i64(generation.input_revision)?,
                generation.model_id,
                generation.model_version,
                cloud_bundle_task_state_to_db(generation.state),
                generation.created_at_utc_ms,
                generation.completed_at_utc_ms,
            ],
        )?;
    }
    for assignment in &bundle.diarization_assignments {
        transaction.execute(
            "INSERT INTO meeting_diarization_assignments (
                generation_id, segment_id, speaker_id, assignment_kind
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                id(assignment.generation_id),
                id(assignment.segment_id),
                id(assignment.speaker_id),
                speaker_assignment_to_db(assignment.assignment_kind),
            ],
        )?;
    }
    let changed = transaction.execute(
        "UPDATE meeting_sessions SET successful_plan_id = ?1,
                current_transcript_revision_id = ?2, current_diarization_generation_id = ?3,
                diarization_status = ?4, diarization_model_id = ?5, diarization_model_version = ?6
         WHERE id = ?7",
        params![
            bundle.session.successful_plan_id.map(id),
            bundle.session.current_transcript_revision_id.map(id),
            bundle.session.current_diarization_generation_id.map(id),
            diarization_status_to_db(bundle.session.diarization_status),
            bundle.session.diarization_model_id,
            bundle.session.diarization_model_version,
            id(session_id),
        ],
    )?;
    if changed != 1 {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

fn cloud_artifact_kind_to_db(kind: MeetingArtifactKind) -> &'static str {
    match kind {
        MeetingArtifactKind::Notes => "notes",
        MeetingArtifactKind::Actions => "actions",
        MeetingArtifactKind::Decisions => "decisions",
        MeetingArtifactKind::Topics => "topics",
    }
}

fn artifact_kind_from_db(value: &str) -> Result<MeetingArtifactKind, StoreError> {
    match value {
        "notes" => Ok(MeetingArtifactKind::Notes),
        "actions" => Ok(MeetingArtifactKind::Actions),
        "decisions" => Ok(MeetingArtifactKind::Decisions),
        "topics" => Ok(MeetingArtifactKind::Topics),
        _ => Err(StoreError::Corrupt),
    }
}

fn phase_from_db(value: &str) -> Result<MeetingPhase, StoreError> {
    match value {
        "preflight" => Ok(MeetingPhase::Preflight),
        "starting" => Ok(MeetingPhase::Starting),
        "capturing_recording" => Ok(MeetingPhase::CapturingRecording),
        "capturing_pausing" => Ok(MeetingPhase::CapturingPausing),
        "capturing_paused" => Ok(MeetingPhase::CapturingPaused),
        "capturing_resuming" => Ok(MeetingPhase::CapturingResuming),
        "stopping" => Ok(MeetingPhase::Stopping),
        "processing" => Ok(MeetingPhase::Processing),
        "review_ready" => Ok(MeetingPhase::ReviewReady),
        "recovery_required" => Ok(MeetingPhase::RecoveryRequired),
        "deleting" => Ok(MeetingPhase::Deleting),
        _ => Err(StoreError::Corrupt),
    }
}

fn source_kind_from_db(value: &str) -> Result<SourceKind, StoreError> {
    match value {
        "microphone" => Ok(SourceKind::Microphone),
        "system_audio" => Ok(SourceKind::SystemAudio),
        _ => Err(StoreError::Corrupt),
    }
}

fn diarization_status_to_db(status: DiarizationStatus) -> &'static str {
    match status {
        DiarizationStatus::NotRequested => "not_requested",
        DiarizationStatus::ModelUnavailable => "model_unavailable",
        DiarizationStatus::Downloading => "downloading",
        DiarizationStatus::Running => "running",
        DiarizationStatus::Succeeded => "succeeded",
        DiarizationStatus::Failed => "failed",
    }
}

fn diarization_status_from_db(value: &str) -> Result<DiarizationStatus, StoreError> {
    match value {
        "not_requested" => Ok(DiarizationStatus::NotRequested),
        "model_unavailable" => Ok(DiarizationStatus::ModelUnavailable),
        "downloading" => Ok(DiarizationStatus::Downloading),
        "running" => Ok(DiarizationStatus::Running),
        "succeeded" => Ok(DiarizationStatus::Succeeded),
        "failed" => Ok(DiarizationStatus::Failed),
        _ => Err(StoreError::Corrupt),
    }
}

fn speaker_assignment_to_db(assignment: SpeakerAssignmentKind) -> &'static str {
    match assignment {
        SpeakerAssignmentKind::LocalSpeaker => "local_speaker",
        SpeakerAssignmentKind::SystemSpeaker => "system_speaker",
        SpeakerAssignmentKind::Unknown => "unknown",
        SpeakerAssignmentKind::Overlap => "overlap",
    }
}

fn speaker_assignment_from_db(value: &str) -> Result<SpeakerAssignmentKind, StoreError> {
    match value {
        "local_speaker" => Ok(SpeakerAssignmentKind::LocalSpeaker),
        "system_speaker" => Ok(SpeakerAssignmentKind::SystemSpeaker),
        "unknown" => Ok(SpeakerAssignmentKind::Unknown),
        "overlap" => Ok(SpeakerAssignmentKind::Overlap),
        _ => Err(StoreError::Corrupt),
    }
}

fn artifact_state_to_db(state: MeetingArtifactState) -> &'static str {
    match state {
        MeetingArtifactState::Current => "current",
        MeetingArtifactState::OutOfDate => "out_of_date",
        MeetingArtifactState::Failed => "failed",
    }
}

fn artifact_state_from_db(value: &str) -> Result<MeetingArtifactState, StoreError> {
    match value {
        "current" => Ok(MeetingArtifactState::Current),
        "out_of_date" => Ok(MeetingArtifactState::OutOfDate),
        "failed" => Ok(MeetingArtifactState::Failed),
        _ => Err(StoreError::Corrupt),
    }
}

fn meeting_answer_state_to_db(state: MeetingAnswerState) -> &'static str {
    match state {
        MeetingAnswerState::Supported => "supported",
        MeetingAnswerState::InsufficientEvidence => "insufficient_evidence",
        MeetingAnswerState::Unavailable => "unavailable",
        MeetingAnswerState::OutOfDate => "out_of_date",
        MeetingAnswerState::Forgotten => "forgotten",
    }
}

fn meeting_answer_state_from_db(value: &str) -> Result<MeetingAnswerState, StoreError> {
    match value {
        "supported" => Ok(MeetingAnswerState::Supported),
        "insufficient_evidence" => Ok(MeetingAnswerState::InsufficientEvidence),
        "unavailable" => Ok(MeetingAnswerState::Unavailable),
        "out_of_date" => Ok(MeetingAnswerState::OutOfDate),
        "forgotten" => Ok(MeetingAnswerState::Forgotten),
        _ => Err(StoreError::Corrupt),
    }
}

fn meeting_fts_match_query(user_text: &str) -> Option<String> {
    let mut expression = String::new();
    for token in user_text
        .split_whitespace()
        .filter(|token| token.chars().any(char::is_alphanumeric))
    {
        if !expression.is_empty() {
            expression.push(' ');
        }
        expression.push('"');
        for character in token.chars() {
            if character == '"' {
                expression.push('"');
            }
            expression.push(character);
        }
        expression.push_str("\"*");
    }
    (!expression.is_empty()).then_some(expression)
}

fn bounded_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_string()
}

fn id<T: Into<Uuid>>(value: T) -> String {
    value.into().to_string()
}

macro_rules! id_into_uuid {
    ($($type:ty),+ $(,)?) => {
        $(
            impl From<$type> for Uuid {
                fn from(value: $type) -> Self {
                    value.uuid()
                }
            }
        )+
    };
}

id_into_uuid!(
    MeetingSessionId,
    MeetingPlanId,
    ConsentId,
    SourceTrackId,
    TranscriptRevisionId,
    TranscriptSegmentId,
    SpeakerId,
    ManualNoteId,
    MeetingSuggestionId,
    MeetingOperationId,
    MeetingDeletionJobId,
    MeetingExportReceiptId,
    MeetingQuestionId,
    MeetingArtifactId,
    MeetingDiarizationGenerationId,
    SavedPromptId,
    PromptRunId,
);

fn encode_json<T: Serialize + ?Sized>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|_| StoreError::Corrupt)
}

fn decode_json<T: DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    serde_json::from_str(value).map_err(|_| StoreError::Corrupt)
}

fn parse_uuid(value: &str) -> Result<Uuid, StoreError> {
    Uuid::parse_str(value).map_err(|_| StoreError::Corrupt)
}

fn to_i64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Invalid)
}

fn from_i64(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt)
}

fn optional_i64(value: Option<u64>) -> Result<Option<i64>, StoreError> {
    value.map(to_i64).transpose()
}

fn bool_to_i64(value: bool) -> i64 {
    i64::from(value)
}

fn utc_now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn read_u64(bytes: &[u8]) -> u64 {
    let mut array = [0_u8; 8];
    array.copy_from_slice(bytes);
    u64::from_le_bytes(array)
}

fn read_u32(bytes: &[u8]) -> u32 {
    let mut array = [0_u8; 4];
    array.copy_from_slice(bytes);
    u32::from_le_bytes(array)
}

fn read_u16(bytes: &[u8]) -> u16 {
    let mut array = [0_u8; 2];
    array.copy_from_slice(bytes);
    u16::from_le_bytes(array)
}

fn to_sql_error(_: StoreError) -> rusqlite::Error {
    rusqlite::Error::InvalidQuery
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::DashboardTrendRange;
    use crate::meeting::processing::{
        MeetingTextGenerationError, MeetingTextGenerator, ReplyShape,
    };
    use crate::secrets::SecretManager;
    use std::borrow::Cow;
    use tempfile::TempDir;

    struct CapturingLedgerGenerator {
        citation: String,
        evidence: Arc<Mutex<Option<String>>>,
    }

    impl MeetingTextGenerator for CapturingLedgerGenerator {
        fn is_available(&self) -> bool {
            true
        }

        fn model_id(&self) -> &'static str {
            "test-ledger"
        }

        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("test-ledger-v1")
        }

        fn max_input_bytes(&self) -> usize {
            usize::MAX
        }

        fn generate(
            &self,
            _system_prompt: &str,
            evidence: &str,
            _max_tokens: i32,
            shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            assert_eq!(shape, ReplyShape::Json);
            *self
                .evidence
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(evidence.to_string());
            Ok(serde_json::json!({
                "headline": "The speakers were merged.",
                "threads": [{
                    "topic": "Transcript",
                    "state": "closed",
                    "substantive": false,
                    "receipt": {
                        "quote": "one",
                        "speaker": "Canonical speaker",
                        "citations": [self.citation],
                    },
                    "owner": null,
                }],
                "open_loops": [],
                "commitments": [],
                "stances": [],
                "caveats": [],
            })
            .to_string())
        }
    }

    fn store() -> (TempDir, Arc<MeetingStore>) {
        let directory = TempDir::new().unwrap();
        let manager =
            SecretManager::with_backend(Arc::new(crate::secrets::MemorySecretBackend::new()));
        let key = tauri::async_runtime::block_on(manager.meeting_storage_key()).unwrap();
        let store = MeetingStore::open(directory.path().join("meetings"), key).unwrap();
        (directory, store)
    }

    fn preflight(session_id: MeetingSessionId) -> MeetingPreflightSnapshot {
        MeetingPreflightSnapshot {
            session_id,
            revision: 0,
            proposed_title: "Design sync".to_string(),
            origin: MeetingOrigin::Manual,
            sources: SourceKind::ALL
                .into_iter()
                .map(|source_kind| MeetingSourceSnapshot {
                    track_id: None,
                    source_kind,
                    required: true,
                    availability: SourceAvailability::Available,
                    health: SourceHealth::NotStarted,
                    format: None,
                    last_durable_offset_ns: None,
                    gap_count: 0,
                })
                .collect(),
            storage: StorageAvailability::Available,
            local_processing: SourceAvailability::Available,
            destination: ProcessingDestination::Local,
            microphone_device_uid: None,
            frozen_system_audio_application_bundle_ids: Vec::new(),
            accepted_known_missing_sources: Vec::new(),
            degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
            required_acknowledgements: SourceKind::ALL.to_vec(),
            allowed_actions: vec![AllowedMeetingAction::Start],
        }
    }

    fn microphone_track(
        store: &Arc<MeetingStore>,
        timestamp_bridge: TimestampBridge,
    ) -> (MeetingSessionId, SourceTrackId, MeetingStoragePlan) {
        let session_id = MeetingSessionId::new();
        store
            .create_preflight(
                StoreMutation {
                    operation_id: MeetingOperationId::new(),
                    actor: OperationActor::User,
                    requested_at_utc_ms: 1,
                    session_id,
                    expected_revision: 0,
                    command: MeetingCommandKind::PreflightCreate,
                },
                "Design sync".to_string(),
                MeetingOrigin::Manual,
                preflight(session_id),
                MeetingRetentionPolicy::Forever,
            )
            .unwrap();
        let storage = MeetingStoragePlan {
            format_version: 1,
            record_max_payload_bytes: 4_096,
            checkpoint_interval_ms: 1,
            source_lane_sample_capacity: 1_024,
            source_lane_descriptor_capacity: 4,
        };
        let plan = MeetingRunPlan {
            plan_id: MeetingPlanId::new(),
            session_id,
            consent_id: ConsentId::new(),
            attempt_number: 1,
            schema_version: 1,
            app_build: "test".to_string(),
            preflight_revision: 0,
            requested_sources: vec![SourceKind::Microphone],
            required_sources: vec![SourceKind::Microphone],
            accepted_known_missing_sources: Vec::new(),
            degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
            microphone_device_uid: None,
            frozen_system_audio_application_bundle_ids: Vec::new(),
            session_clock_anchor: SessionClockAnchor {
                host_monotonic_anchor_ns: 0,
                wall_start_utc_ms: 0,
                clock_policy_version: 1,
            },
            storage: storage.clone(),
            language: "en".to_string(),
            asr_model_id: None,
            asr_model_version: None,
            diarization_model_id: None,
            diarization_model_version: None,
            destination: ProcessingDestination::Local,
            remote_acknowledgement: None,
            retention_policy: MeetingRetentionPolicy::Forever,
        };
        let consent = MeetingConsent {
            consent_id: plan.consent_id,
            session_id,
            attempt_number: 1,
            preflight_revision: 0,
            policy_version: 1,
            acknowledged_at_utc_ms: 0,
            provenance: MeetingConsentProvenance::Direct,
            microphone_acknowledged: true,
            system_audio_acknowledged: false,
            known_missing_sources_acknowledged: Vec::new(),
            degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
            destination: ProcessingDestination::Local,
            remote_acknowledgement: None,
        };
        store
            .start_with_plan_and_consent(MeetingOperationId::new(), 1, &plan, &consent, 0)
            .unwrap();
        let track_id = SourceTrackId::new();
        store
            .create_track(TrackCreation {
                session_id,
                plan_id: plan.plan_id,
                source_kind: SourceKind::Microphone,
                required: true,
                requested: true,
                descriptor_json: "{}",
                report: SourceStartReport {
                    track_id,
                    source_kind: SourceKind::Microphone,
                    format: AudioFormat {
                        sample_rate_hz: 48_000,
                        channels: 1,
                    },
                    epoch: SourceEpoch::new(0),
                    format_epoch: 1,
                    timestamp_bridge,
                },
            })
            .unwrap();
        (session_id, track_id, storage)
    }

    const TEST_PACKET_FRAMES: u32 = 512;
    const TEST_PACKET_RATE: u32 = 48_000;

    fn test_packet_duration_ns() -> i64 {
        i64::from(TEST_PACKET_FRAMES) * 1_000_000_000 / i64::from(TEST_PACKET_RATE)
    }

    fn captured_packet(
        track_id: SourceTrackId,
        sequence: u64,
        native_timestamp_value: Option<i64>,
    ) -> CapturedPacket {
        CapturedPacket {
            track_id,
            source_epoch: SourceEpoch::new(0),
            format_epoch: 1,
            sequence,
            native_timestamp_value,
            native_timestamp_timescale: native_timestamp_value.map(|_| 1_000_000_000),
            host_monotonic_anchor_ns: native_timestamp_value
                .and_then(|value| value.try_into().ok()),
            sample_rate_hz: TEST_PACKET_RATE,
            channels: 1,
            frame_count: TEST_PACKET_FRAMES,
            discontinuity_flags: PacketDiscontinuityFlags::default(),
        }
    }

    fn review_ready_session(store: &MeetingStore, session_id: MeetingSessionId) -> u64 {
        store
            .create_preflight(
                StoreMutation {
                    operation_id: MeetingOperationId::new(),
                    actor: OperationActor::User,
                    requested_at_utc_ms: 1,
                    session_id,
                    expected_revision: 0,
                    command: MeetingCommandKind::PreflightCreate,
                },
                "Design sync".to_string(),
                MeetingOrigin::Manual,
                preflight(session_id),
                MeetingRetentionPolicy::Forever,
            )
            .expect("preflight");
        let preflight = store
            .session_snapshot(session_id)
            .expect("preflight snapshot");
        store
            .transition(StoreTransition {
                operation_id: None,
                actor: OperationActor::System,
                command: MeetingCommandKind::RecoveryFinalize,
                requested_at_utc_ms: 2,
                session_id,
                expected_revision: preflight.revision,
                allowed_from: &[MeetingPhase::Preflight],
                next_phase: MeetingPhase::ReviewReady,
                event_kind: "test_review_ready",
                reason_codes: Vec::new(),
            })
            .expect("review transition");
        store
            .session_snapshot(session_id)
            .expect("review snapshot")
            .revision
    }

    fn local_noon_ms(date: chrono::NaiveDate) -> i64 {
        date.and_hms_opt(12, 0, 0)
            .expect("local noon")
            .and_local_timezone(Local)
            .earliest()
            .expect("representable local noon")
            .timestamp_millis()
    }

    fn trend_session(store: &MeetingStore, created_at_utc_ms: i64) -> MeetingSessionId {
        let session_id = MeetingSessionId::new();
        review_ready_session(store, session_id);
        store
            .connection()
            .expect("store connection")
            .execute(
                "UPDATE meeting_sessions SET created_at_utc_ms = ?1 WHERE id = ?2",
                params![created_at_utc_ms, id(session_id)],
            )
            .expect("set trend session date");
        session_id
    }

    fn trend_artifacts(action_count: usize) -> GeneratedMeetingArtifacts {
        let text = CitedArtifactText {
            text: "generated".to_string(),
            citations: Vec::new(),
        };
        GeneratedMeetingArtifacts {
            summary: text.clone(),
            summary_trace: Vec::new(),
            outline: Vec::new(),
            decisions: Vec::new(),
            action_items: (0..action_count)
                .map(|_| MeetingActionItem {
                    text: text.clone(),
                    owner_text: None,
                    due_text: None,
                })
                .collect(),
            key_questions: Vec::new(),
            risks: Vec::new(),
            follow_up_draft: text,
            ledger: None,
        }
    }

    fn seed_trend_metrics(store: &MeetingStore, session_id: MeetingSessionId) {
        let plan_id = MeetingPlanId::new();
        let microphone_track_id = SourceTrackId::new();
        let system_track_id = SourceTrackId::new();
        let current_transcript_revision_id = TranscriptRevisionId::new();
        let prior_transcript_revision_id = TranscriptRevisionId::new();
        let speaker_id = SpeakerId::new();
        let connection = store.connection().expect("store connection");
        let revision: i64 = connection
            .query_row(
                "SELECT revision FROM meeting_sessions WHERE id = ?1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .expect("session revision");

        connection
            .execute(
                "INSERT INTO meeting_run_plans (
                    plan_id, session_id, attempt_number, schema_version, consent_id,
                    canonical_plan_json, created_at_utc_ms
                 ) VALUES (?1, ?2, 1, 1, ?3, '{}', 1)",
                params![id(plan_id), id(session_id), id(ConsentId::new())],
            )
            .expect("insert trend plan");
        for (track_id, source_kind) in [
            (microphone_track_id, "microphone"),
            (system_track_id, "system_audio"),
        ] {
            connection
                .execute(
                    "INSERT INTO meeting_source_tracks (
                        track_id, session_id, plan_id, source_kind, required, requested,
                        descriptor_json, timestamp_bridge_json, health
                     ) VALUES (?1, ?2, ?3, ?4, 1, 1, '{}', '{}', '\"healthy\"')",
                    params![id(track_id), id(session_id), id(plan_id), source_kind],
                )
                .expect("insert trend track");
        }
        for (track_id, start_offset_ns) in [
            (microphone_track_id, 0_i64),
            (system_track_id, 1_000_000_000_i64),
        ] {
            connection
                .execute(
                    "INSERT INTO meeting_track_records (
                        track_id, source_sequence, source_epoch, start_offset_ns, duration_ns,
                        frame_count, record_offset_bytes, record_bytes, durable_at_utc_ms
                     ) VALUES (?1, 0, 0, ?2, 2000000000, 1, 0, 1, 1)",
                    params![id(track_id), start_offset_ns],
                )
                .expect("insert durable trend record");
        }
        for transcript_revision_id in [prior_transcript_revision_id, current_transcript_revision_id]
        {
            connection
                .execute(
                    "INSERT INTO meeting_transcript_revisions (
                        transcript_revision_id, session_id, engine_id, destination_json,
                        source_set_json, language, state, created_at_utc_ms, completed_at_utc_ms
                     ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'completed', 1, 1)",
                    params![id(transcript_revision_id), id(session_id)],
                )
                .expect("insert transcript revision");
        }
        connection
            .execute(
                "UPDATE meeting_sessions
                 SET current_transcript_revision_id = ?1
                 WHERE id = ?2",
                params![id(current_transcript_revision_id), id(session_id)],
            )
            .expect("set current transcript revision");
        connection
            .execute(
                "INSERT INTO meeting_speakers (
                    speaker_id, session_id, source_kind, display_name, revision
                 ) VALUES (?1, ?2, 'microphone', 'Speaker', 0)",
                params![id(speaker_id), id(session_id)],
            )
            .expect("insert trend speaker");
        for (transcript_revision_id, ordinal) in [
            (prior_transcript_revision_id, 0_i64),
            (current_transcript_revision_id, 0_i64),
            (current_transcript_revision_id, 1_i64),
        ] {
            connection
                .execute(
                    "INSERT INTO meeting_transcript_segments (
                        segment_id, transcript_revision_id, track_id, ordinal, start_offset_ns,
                        end_offset_ns, speaker_id, base_text, confidence_milli
                     ) VALUES (?1, ?2, ?3, ?4, 0, 1000000000, ?5, 'segment', NULL)",
                    params![
                        id(TranscriptSegmentId::new()),
                        id(transcript_revision_id),
                        id(microphone_track_id),
                        ordinal,
                        id(speaker_id)
                    ],
                )
                .expect("insert transcript segment");
        }
        let removed_current_segment_id = TranscriptSegmentId::new();
        connection
            .execute(
                "INSERT INTO meeting_transcript_segments (
                    segment_id, transcript_revision_id, track_id, ordinal, start_offset_ns,
                    end_offset_ns, speaker_id, base_text, confidence_milli
                 ) VALUES (?1, ?2, ?3, 2, 0, 1000000000, ?4, 'removed segment', NULL)",
                params![
                    id(removed_current_segment_id),
                    id(current_transcript_revision_id),
                    id(microphone_track_id),
                    id(speaker_id)
                ],
            )
            .expect("insert removed current transcript segment");
        connection
            .execute(
                "INSERT INTO meeting_segment_edits (
                    segment_id, edit_sequence, replacement_text, removed, operator_at_utc_ms
                 ) VALUES (?1, 0, '', 1, 1)",
                params![id(removed_current_segment_id)],
            )
            .expect("remove current transcript segment");
        for (artifact_id, transcript_revision_id, input_revision, state, content) in [
            (
                MeetingArtifactId::new(),
                prior_transcript_revision_id,
                revision,
                "current",
                trend_artifacts(5),
            ),
            (
                MeetingArtifactId::new(),
                current_transcript_revision_id,
                revision,
                "out_of_date",
                trend_artifacts(7),
            ),
            (
                MeetingArtifactId::new(),
                current_transcript_revision_id,
                revision
                    .checked_add(1)
                    .expect("test revision can be incremented"),
                "current",
                trend_artifacts(11),
            ),
            (
                MeetingArtifactId::new(),
                current_transcript_revision_id,
                revision,
                "current",
                trend_artifacts(2),
            ),
        ] {
            connection
                .execute(
                    "INSERT INTO meeting_artifact_revisions (
                        artifact_id, session_id, transcript_revision_id, input_revision,
                        template_id, template_version, generation_key, state, content_json,
                        generated_at_utc_ms
                     ) VALUES (?1, ?2, ?3, ?4, 'test', 1, ?5, ?6, ?7, 1)",
                    params![
                        id(artifact_id),
                        id(session_id),
                        id(transcript_revision_id),
                        input_revision,
                        id(artifact_id),
                        state,
                        encode_json(&content).expect("encode trend artifact")
                    ],
                )
                .expect("insert trend artifact");
        }
    }

    #[test]
    fn preflight_readiness_is_projected_and_refreshed() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        let mut initial = preflight(session_id);
        initial.local_processing = SourceAvailability::DeviceUnavailable;
        initial.sources[1].availability = SourceAvailability::DeviceUnavailable;
        store
            .create_preflight(
                StoreMutation {
                    operation_id: MeetingOperationId::new(),
                    actor: OperationActor::User,
                    requested_at_utc_ms: 1,
                    session_id,
                    expected_revision: 0,
                    command: MeetingCommandKind::PreflightCreate,
                },
                initial.proposed_title.clone(),
                initial.origin,
                initial,
                MeetingRetentionPolicy::Forever,
            )
            .expect("create preflight");

        let initial_snapshot = store
            .session_snapshot(session_id)
            .expect("initial session snapshot");
        assert_eq!(
            initial_snapshot.preflight_local_processing,
            Some(SourceAvailability::DeviceUnavailable)
        );
        assert_eq!(initial_snapshot.sources.len(), SourceKind::ALL.len());
        assert_eq!(
            initial_snapshot.sources[1].availability,
            SourceAvailability::DeviceUnavailable
        );

        let mut refreshed = preflight(session_id);
        refreshed.revision = 1;
        store
            .refresh_preflight(MeetingOperationId::new(), 2, session_id, 0, refreshed)
            .expect("refresh preflight");

        let refreshed_snapshot = store
            .session_snapshot(session_id)
            .expect("refreshed session snapshot");
        assert_eq!(
            refreshed_snapshot.preflight_local_processing,
            Some(SourceAvailability::Available)
        );
        assert!(refreshed_snapshot
            .sources
            .iter()
            .all(|source| source.availability == SourceAvailability::Available));
    }

    #[test]
    fn meeting_trend_zero_fills_an_empty_requested_range() {
        let (_directory, store) = store();
        let now = Local::now();
        for range in [
            DashboardTrendRange::Days7,
            DashboardTrendRange::Days30,
            DashboardTrendRange::Days180,
        ] {
            let request = DashboardTrendRequest { range };
            let trend = {
                let mut connection = store.connection().expect("store connection");
                MeetingStore::trend_projection_with_connection_at(
                    &mut connection,
                    request,
                    now.clone(),
                )
                .expect("empty meeting trend")
            };

            let MeetingTrendProjection::Available {
                range: returned_range,
                range_total,
                all_time,
                points,
                ..
            } = trend
            else {
                panic!("opened store must have an available trend projection");
            };
            assert_eq!(returned_range, range);
            assert_eq!(points.len(), range.days());
            assert!(points.iter().all(|point| {
                point.meetings == 0
                    && point.verified_captured_duration_ms == 0
                    && point.transcript_segments == 0
                    && point.generated_action_items == 0
            }));
            assert_eq!(range_total.meetings, 0);
            assert_eq!(all_time.meetings, 0);
        }
    }

    #[test]
    fn meeting_trend_uses_retained_sessions_current_segments_artifacts_and_unfiltered_data() {
        let (_directory, store) = store();
        let request = DashboardTrendRequest {
            range: DashboardTrendRange::Days7,
        };
        let now = Local::now();
        let calendar =
            LocalCalendarRange::at(now.clone(), request.range).expect("local calendar range");
        let dates = calendar.local_dates().expect("requested local dates");

        let captured = trend_session(&store, local_noon_ms(dates[0]));
        seed_trend_metrics(&store, captured);
        let uncaptured = trend_session(&store, local_noon_ms(dates[2]));
        let deleting = trend_session(&store, local_noon_ms(dates[1]));
        let deleted = trend_session(&store, local_noon_ms(dates[3]));
        let outside_range = trend_session(&store, calendar.end_exclusive_utc_ms());

        {
            let connection = store.connection().expect("store connection");
            connection
                .execute(
                    "UPDATE meeting_sessions SET phase = 'deleting' WHERE id = ?1",
                    params![id(deleting)],
                )
                .expect("tombstone meeting");
            connection
                .execute(
                    "DELETE FROM meeting_sessions WHERE id = ?1",
                    params![id(deleted)],
                )
                .expect("delete meeting");
            connection
                .execute(
                    "INSERT INTO meeting_search_documents (
                        session_id, entity_kind, entity_id, content
                     ) VALUES (?1, 'title', ?2, 'captured only')",
                    params![id(captured), id(captured)],
                )
                .expect("seed a narrowed search document");
        }

        let filtered = store
            .search_evidence(&[captured], "captured", 10)
            .expect("narrowed meeting search");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].citation.session_id, captured);

        let mut connection = store.connection().expect("store connection");
        let trend =
            MeetingStore::trend_projection_with_connection_at(&mut connection, request, now)
                .expect("meeting trend");

        let MeetingTrendProjection::Available {
            range,
            range_total,
            all_time,
            points,
            ..
        } = trend
        else {
            panic!("opened store must have an available trend projection");
        };
        assert_eq!(range, DashboardTrendRange::Days7);
        assert_eq!(points.len(), 7);
        assert_eq!(points[0].meetings, 1);
        assert_eq!(points[0].verified_captured_duration_ms, 3_000);
        assert_eq!(points[0].transcript_segments, 2);
        assert_eq!(points[0].generated_action_items, 2);
        assert_eq!(points[1].meetings, 0);
        assert_eq!(points[2].meetings, 1);
        assert_eq!(points[2].verified_captured_duration_ms, 0);
        assert_eq!(points[2].transcript_segments, 0);
        assert_eq!(points[2].generated_action_items, 0);
        assert_eq!(range_total.meetings, 2);
        assert_eq!(range_total.verified_captured_duration_ms, 3_000);
        assert_eq!(range_total.transcript_segments, 2);
        assert_eq!(range_total.generated_action_items, 2);
        assert_eq!(all_time.meetings, 3);
        assert_eq!(all_time.verified_captured_duration_ms, 3_000);
        assert_eq!(all_time.transcript_segments, 2);
        assert_eq!(all_time.generated_action_items, 2);
        assert_ne!(outside_range, captured);
        assert_ne!(uncaptured, captured);
    }

    #[test]
    fn meeting_trend_uses_local_calendar_boundaries() {
        let (_directory, store) = store();
        let request = DashboardTrendRequest {
            range: DashboardTrendRange::Days7,
        };
        let now = Local::now();
        let calendar =
            LocalCalendarRange::at(now.clone(), request.range).expect("local calendar range");

        let _before_range = trend_session(
            &store,
            calendar
                .start_utc_ms()
                .checked_sub(1)
                .expect("current range has a preceding millisecond"),
        );
        let _first_in_range = trend_session(&store, calendar.start_utc_ms());
        let _last_in_range = trend_session(
            &store,
            calendar
                .end_exclusive_utc_ms()
                .checked_sub(1)
                .expect("range has a final millisecond"),
        );
        let _after_range = trend_session(&store, calendar.end_exclusive_utc_ms());

        let mut connection = store.connection().expect("store connection");
        let trend =
            MeetingStore::trend_projection_with_connection_at(&mut connection, request, now)
                .expect("meeting trend");

        let MeetingTrendProjection::Available {
            range_total,
            all_time,
            points,
            ..
        } = trend
        else {
            panic!("opened store must have an available trend projection");
        };
        assert_eq!(range_total.meetings, 2);
        assert_eq!(all_time.meetings, 4);
        assert_eq!(points.first().map(|point| point.meetings), Some(1));
        assert_eq!(points.last().map(|point| point.meetings), Some(1));
        assert_eq!(
            points.first().map(|point| point.local_date.clone()),
            Some(calendar.start_local_date())
        );
        assert_eq!(
            points.last().map(|point| point.local_date.clone()),
            Some(calendar.end_local_date())
        );
    }

    #[test]
    fn migrations_create_encrypted_store_and_idempotent_preflight_receipt() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        let operation_id = MeetingOperationId::new();
        let receipt = store
            .create_preflight(
                StoreMutation {
                    operation_id,
                    actor: OperationActor::User,
                    requested_at_utc_ms: 1,
                    session_id,
                    expected_revision: 0,
                    command: MeetingCommandKind::PreflightCreate,
                },
                "Design sync".to_string(),
                MeetingOrigin::Manual,
                preflight(session_id),
                MeetingRetentionPolicy::Forever,
            )
            .unwrap();
        let duplicate_session_id = MeetingSessionId::new();
        let duplicate = store
            .create_preflight(
                StoreMutation {
                    operation_id,
                    actor: OperationActor::User,
                    requested_at_utc_ms: 2,
                    session_id: duplicate_session_id,
                    expected_revision: 0,
                    command: MeetingCommandKind::PreflightCreate,
                },
                "Other".to_string(),
                MeetingOrigin::Manual,
                preflight(session_id),
                MeetingRetentionPolicy::Forever,
            )
            .unwrap();
        assert_eq!(receipt, duplicate);
        assert_eq!(
            store
                .list_sessions(None, 10, &MeetingListFilter::default())
                .unwrap()
                .entries
                .len(),
            1
        );
    }

    #[test]
    fn contiguous_untimestamped_prefix_is_durable_without_false_gaps() {
        let (_directory, store) = store();
        let bridge = TimestampBridge {
            native_anchor_value: 0,
            native_timescale: 1_000_000_000,
            host_monotonic_anchor_ns: 0,
            session_offset_ns: 0,
        };
        let (session_id, track_id, storage) = microphone_track(&store, bridge);
        let mut writer = store
            .open_track_writer(session_id, track_id, storage)
            .unwrap();
        let packet_duration_ns = test_packet_duration_ns();
        let samples = vec![0.25; usize::try_from(TEST_PACKET_FRAMES).unwrap()];

        assert_eq!(
            writer
                .accept(captured_packet(track_id, 1, Some(-1)), &samples)
                .unwrap(),
            PacketPushResult::Accepted
        );
        assert_eq!(
            writer
                .accept(captured_packet(track_id, 3, None), &samples)
                .unwrap(),
            PacketPushResult::Accepted
        );
        writer.seal().unwrap();

        let snapshot = store.review_snapshot(session_id).unwrap();
        assert!(snapshot.gaps.is_empty());
        assert_eq!(snapshot.tracks[0].durable_record_count, 2);
        assert_eq!(snapshot.tracks[0].first_offset_ns, Some(0));
        assert_eq!(
            snapshot.tracks[0].last_offset_ns,
            u64::try_from(packet_duration_ns * 2).ok()
        );
    }

    /// Audio a running capture has produced reaches the record index before
    /// anything seals the track, and a reader can ask for only what arrived
    /// after the last one it read.
    ///
    /// Both halves are what a mid-meeting reading of a live capture rests on,
    /// and the first is also what a launch that ends without a stop has to
    /// recover from.
    #[test]
    fn a_writer_commits_records_before_the_track_is_sealed() {
        let (_directory, store) = store();
        let bridge = TimestampBridge {
            native_anchor_value: 0,
            native_timescale: 1_000_000_000,
            host_monotonic_anchor_ns: 0,
            session_offset_ns: 0,
        };
        let (session_id, track_id, storage) = microphone_track(&store, bridge);
        let mut writer = store
            .open_track_writer(session_id, track_id, storage)
            .unwrap();
        let samples = vec![0.25; usize::try_from(TEST_PACKET_FRAMES).unwrap()];

        assert_eq!(
            writer
                .accept(captured_packet(track_id, 0, Some(0)), &samples)
                .unwrap(),
            PacketPushResult::Accepted
        );

        let mut committed = Vec::new();
        store
            .visit_durable_track_records(session_id, track_id, None, |record| {
                committed.push(record.sequence);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            committed,
            vec![0],
            "a packet past the checkpoint interval is committed, not held until the seal"
        );

        assert_eq!(
            writer
                .accept(
                    captured_packet(track_id, 1, Some(test_packet_duration_ns())),
                    &samples
                )
                .unwrap(),
            PacketPushResult::Accepted
        );
        let mut since = Vec::new();
        store
            .visit_durable_track_records(session_id, track_id, Some(0), |record| {
                since.push(record.sequence);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            since,
            vec![1],
            "a mark reads what arrived after it and pays for nothing before it"
        );
    }

    #[test]
    fn capture_clock_stall_records_one_timed_gap_and_keeps_later_audio() {
        let (_directory, store) = store();
        let bridge = TimestampBridge {
            native_anchor_value: 0,
            native_timescale: 1_000_000_000,
            host_monotonic_anchor_ns: 0,
            session_offset_ns: 0,
        };
        let (session_id, track_id, storage) = microphone_track(&store, bridge);
        let mut writer = store
            .open_track_writer(session_id, track_id, storage)
            .unwrap();
        let packet_duration_ns = test_packet_duration_ns();
        let samples = vec![0.25; usize::try_from(TEST_PACKET_FRAMES).unwrap()];

        assert_eq!(
            writer
                .accept(captured_packet(track_id, 0, Some(0)), &samples)
                .unwrap(),
            PacketPushResult::Accepted
        );
        assert_eq!(
            writer
                .accept(
                    captured_packet(track_id, 1, Some(packet_duration_ns * 3)),
                    &samples,
                )
                .unwrap(),
            PacketPushResult::Accepted
        );
        writer.seal().unwrap();

        let snapshot = store.review_snapshot(session_id).unwrap();
        assert_eq!(snapshot.tracks[0].durable_record_count, 2);
        assert_eq!(snapshot.gaps.len(), 1);
        let gap = &snapshot.gaps[0];
        assert_eq!(gap.reason, SourceGapReason::TimestampDiscontinuity);
        assert_eq!(gap.start_offset_ns, u64::try_from(packet_duration_ns).ok());
        assert_eq!(
            gap.end_offset_ns,
            u64::try_from(packet_duration_ns * 3).ok()
        );
        assert_eq!(
            gap.dropped_frames,
            u64::try_from(packet_duration_ns * 2)
                .ok()
                .map(|duration_ns| { duration_ns * u64::from(TEST_PACKET_RATE) / 1_000_000_000 })
        );
    }

    #[test]
    fn encrypted_track_repair_truncates_only_unauthenticated_tail() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        let operation_id = MeetingOperationId::new();
        store
            .create_preflight(
                StoreMutation {
                    operation_id,
                    actor: OperationActor::User,
                    requested_at_utc_ms: 1,
                    session_id,
                    expected_revision: 0,
                    command: MeetingCommandKind::PreflightCreate,
                },
                "Design sync".to_string(),
                MeetingOrigin::Manual,
                preflight(session_id),
                MeetingRetentionPolicy::Forever,
            )
            .unwrap();
        let plan = MeetingRunPlan {
            plan_id: MeetingPlanId::new(),
            session_id,
            consent_id: ConsentId::new(),
            attempt_number: 1,
            schema_version: 1,
            app_build: "test".to_string(),
            preflight_revision: 0,
            requested_sources: vec![SourceKind::Microphone],
            required_sources: vec![SourceKind::Microphone],
            accepted_known_missing_sources: Vec::new(),
            degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
            microphone_device_uid: None,
            frozen_system_audio_application_bundle_ids: Vec::new(),
            session_clock_anchor: SessionClockAnchor {
                host_monotonic_anchor_ns: 0,
                wall_start_utc_ms: 0,
                clock_policy_version: 1,
            },
            storage: MeetingStoragePlan {
                format_version: 1,
                record_max_payload_bytes: 1024,
                checkpoint_interval_ms: 1,
                source_lane_sample_capacity: 4,
                source_lane_descriptor_capacity: 2,
            },
            language: "en".to_string(),
            asr_model_id: None,
            asr_model_version: None,
            diarization_model_id: None,
            diarization_model_version: None,
            destination: ProcessingDestination::Local,
            remote_acknowledgement: None,
            retention_policy: MeetingRetentionPolicy::Forever,
        };
        let consent = MeetingConsent {
            consent_id: plan.consent_id,
            session_id,
            attempt_number: 1,
            preflight_revision: 0,
            policy_version: 1,
            acknowledged_at_utc_ms: 0,
            provenance: MeetingConsentProvenance::Direct,
            microphone_acknowledged: true,
            system_audio_acknowledged: false,
            known_missing_sources_acknowledged: Vec::new(),
            degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
            destination: ProcessingDestination::Local,
            remote_acknowledgement: None,
        };
        store
            .start_with_plan_and_consent(MeetingOperationId::new(), 1, &plan, &consent, 0)
            .unwrap();
        let track_id = SourceTrackId::new();
        store
            .create_track(TrackCreation {
                session_id,
                plan_id: plan.plan_id,
                source_kind: SourceKind::Microphone,
                required: true,
                requested: true,
                descriptor_json: "{}",
                report: SourceStartReport {
                    track_id,
                    source_kind: SourceKind::Microphone,
                    format: AudioFormat {
                        sample_rate_hz: 48_000,
                        channels: 1,
                    },
                    epoch: SourceEpoch::new(1),
                    format_epoch: 1,
                    timestamp_bridge: TimestampBridge {
                        native_anchor_value: 0,
                        native_timescale: 1_000_000_000,
                        host_monotonic_anchor_ns: 0,
                        session_offset_ns: 0,
                    },
                },
            })
            .unwrap();
        let mut writer = store
            .open_track_writer(session_id, track_id, plan.storage.clone())
            .unwrap();
        writer
            .accept(
                CapturedPacket {
                    track_id,
                    source_epoch: SourceEpoch::new(1),
                    format_epoch: 1,
                    sequence: 0,
                    native_timestamp_value: Some(0),
                    native_timestamp_timescale: Some(1_000_000_000),
                    host_monotonic_anchor_ns: Some(0),
                    sample_rate_hz: 48_000,
                    channels: 1,
                    frame_count: 2,
                    discontinuity_flags: PacketDiscontinuityFlags::default(),
                },
                &[0.1, -0.1],
            )
            .unwrap();
        writer.seal().unwrap();
        let files = store.track_files(session_id, track_id);
        let before = fs::metadata(&files.records).unwrap().len();
        OpenOptions::new()
            .append(true)
            .open(&files.records)
            .unwrap()
            .write_all(&[1, 2, 3])
            .unwrap();
        store.repair_session_tracks(session_id).unwrap();
        assert_eq!(fs::metadata(&files.records).unwrap().len(), before);
    }
    #[test]
    fn retention_policy_receipts_are_idempotent_and_reject_stale_writes() {
        let (_directory, store) = store();
        let operation_id = MeetingOperationId::new();
        let policy = MeetingRetentionPolicy::DeleteAfterDays { days: 14 };
        let (receipt, revision) = store
            .set_default_retention_policy(operation_id, 1, 0, &policy)
            .unwrap();
        assert_eq!(receipt.result, OperationResult::Committed);
        assert_eq!(revision, 1);

        let (duplicate, duplicate_revision) = store
            .set_default_retention_policy(operation_id, 2, 99, &MeetingRetentionPolicy::Forever)
            .unwrap();
        assert_eq!(duplicate, receipt);
        assert_eq!(duplicate_revision, revision);

        let stale_operation = MeetingOperationId::new();
        let (stale, actual_revision) = store
            .set_default_retention_policy(stale_operation, 3, 0, &MeetingRetentionPolicy::Forever)
            .unwrap();
        assert_eq!(stale.result, OperationResult::Rejected);
        assert_eq!(stale.reason_codes, vec![MeetingReasonCode::StaleRevision]);
        assert_eq!(actual_revision, revision);
        assert_eq!(
            store.default_retention_policy().unwrap(),
            (policy, revision)
        );
    }
    #[test]
    fn export_receipt_is_idempotent_for_an_immutable_review_revision() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        store
            .create_preflight(
                StoreMutation {
                    operation_id: MeetingOperationId::new(),
                    actor: OperationActor::User,
                    requested_at_utc_ms: 1,
                    session_id,
                    expected_revision: 0,
                    command: MeetingCommandKind::PreflightCreate,
                },
                "Design sync".to_string(),
                MeetingOrigin::Manual,
                preflight(session_id),
                MeetingRetentionPolicy::Forever,
            )
            .unwrap();
        store
            .transition(StoreTransition {
                operation_id: None,
                actor: OperationActor::System,
                command: MeetingCommandKind::Start,
                requested_at_utc_ms: 2,
                session_id,
                expected_revision: 0,
                allowed_from: &[MeetingPhase::Preflight],
                next_phase: MeetingPhase::Starting,
                event_kind: "test_start",
                reason_codes: Vec::new(),
            })
            .unwrap();
        store
            .transition(StoreTransition {
                operation_id: None,
                actor: OperationActor::System,
                command: MeetingCommandKind::Stop,
                requested_at_utc_ms: 3,
                session_id,
                expected_revision: 1,
                allowed_from: &[MeetingPhase::Starting],
                next_phase: MeetingPhase::ReviewReady,
                event_kind: "test_review_ready",
                reason_codes: Vec::new(),
            })
            .unwrap();

        let operation_id = MeetingOperationId::new();
        let result = store
            .record_export(operation_id, 4, session_id, 2, MeetingExportFormat::Json)
            .unwrap();
        assert_eq!(result.receipt.result, OperationResult::Committed);
        assert_eq!(result.export_receipt.snapshot_revision, 2);
        assert_eq!(
            result.export_receipt.capture_completeness,
            CaptureCompleteness::NotStarted
        );
        assert_eq!(
            store
                .record_export(
                    operation_id,
                    5,
                    session_id,
                    99,
                    MeetingExportFormat::Markdown
                )
                .unwrap(),
            result
        );
    }
    #[test]
    fn cloud_migration_creates_non_cascading_durable_tables() {
        let (_directory, store) = store();
        let connection = store.connection().expect("store connection");
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN (
                    'meeting_cloud_state', 'meeting_cloud_capabilities', 'meeting_cloud_heads',
                    'meeting_cloud_outbox', 'meeting_cloud_outbox_chunks',
                    'meeting_cloud_conflicts', 'meeting_cloud_shares'
                 )",
                [],
                |row| row.get(0),
            )
            .expect("cloud migration tables");
        assert_eq!(count, 7);
        for table in [
            "meeting_cloud_heads",
            "meeting_cloud_outbox",
            "meeting_cloud_conflicts",
            "meeting_cloud_shares",
        ] {
            let mut statement = connection
                .prepare(&format!("PRAGMA foreign_key_list({table})"))
                .expect("foreign key query");
            let targets = statement
                .query_map([], |row| row.get::<_, String>(2))
                .expect("foreign key rows")
                .collect::<Result<Vec<_>, _>>()
                .expect("foreign key targets");
            assert!(!targets.iter().any(|target| target == "meeting_sessions"));
        }
    }

    #[test]
    fn cloud_tombstone_outbox_survives_finished_local_deletion() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        let revision = review_ready_session(&store, session_id);
        let (receipt, job_id) = store
            .reserve_deletion(
                MeetingOperationId::new(),
                10,
                session_id,
                revision,
                DeletionCause::User,
            )
            .expect("reserve deletion");
        let outbox = store
            .enqueue_cloud_outbox(CloudOutboxInput {
                kind: CloudOutboxKind::Tombstone,
                object_id: "object_123456789".to_string(),
                source_session_id: Some(session_id),
                source_revision: receipt.new_revision,
                base_remote_revision_id: Some("revision_1234567".to_string()),
                share_content_kind: None,
                remote_revision_id: None,
                idempotency_key: "delete-intent".to_string(),
                next_attempt_utc_ms: 10,
            })
            .expect("queue tombstone");
        store.finish_deletion(job_id).expect("finish deletion");
        assert!(store.session_snapshot(session_id).is_err());
        assert_eq!(
            store
                .cloud_outbox(&outbox.outbox_id)
                .expect("outbox lookup"),
            Some(outbox)
        );
    }

    #[test]
    fn cloud_claimed_outbox_preserves_immutable_intent() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        let revision = review_ready_session(&store, session_id);
        let outbox = store
            .enqueue_cloud_outbox(CloudOutboxInput {
                kind: CloudOutboxKind::Object,
                object_id: "object_123456789".to_string(),
                source_session_id: Some(session_id),
                source_revision: Some(revision),
                base_remote_revision_id: None,
                share_content_kind: None,
                remote_revision_id: None,
                idempotency_key: "object-intent".to_string(),
                next_attempt_utc_ms: 1,
            })
            .expect("queue object");
        let claimed = store
            .claim_cloud_outbox(&outbox.outbox_id, "claim-token", 1)
            .expect("claim outbox")
            .expect("claimed outbox");
        let connection = store.connection().expect("store connection");
        assert!(connection
            .execute(
                "UPDATE meeting_cloud_outbox SET object_id = 'other_1234567890' WHERE outbox_id = ?1",
                params![claimed.outbox_id],
            )
            .is_err());
        drop(connection);
        let with_upload = store
            .set_cloud_outbox_upload_id(
                &claimed.outbox_id,
                "claim-token",
                "upload_123456789".to_string(),
            )
            .expect("persist upload id");
        assert_eq!(with_upload.object_id, outbox.object_id);
        assert_eq!(with_upload.upload_id.as_deref(), Some("upload_123456789"));
    }

    #[test]
    fn cloud_restart_requeues_claimed_intent_without_mutating_it() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        let revision = review_ready_session(&store, session_id);
        let outbox = store
            .enqueue_cloud_outbox(CloudOutboxInput {
                kind: CloudOutboxKind::Object,
                object_id: "object_123456789".to_string(),
                source_session_id: Some(session_id),
                source_revision: Some(revision),
                base_remote_revision_id: None,
                share_content_kind: None,
                remote_revision_id: Some("revision_1234567".to_string()),
                idempotency_key: "restart-intent".to_string(),
                next_attempt_utc_ms: 50,
            })
            .expect("queue object");
        let claimed = store
            .claim_cloud_outbox(&outbox.outbox_id, "restart-claim", 50)
            .expect("claim object")
            .expect("claimed object");

        assert_eq!(
            store
                .recover_claimed_cloud_outbox(75)
                .expect("recover claims"),
            1
        );
        let recovered = store
            .cloud_outbox(&claimed.outbox_id)
            .expect("recovered outbox")
            .expect("outbox exists");
        assert_eq!(recovered.state, CloudOutboxState::Pending);
        assert_eq!(recovered.claim_token, None);
        assert_eq!(recovered.object_id, claimed.object_id);
        assert_eq!(recovered.remote_revision_id, claimed.remote_revision_id);
        assert_eq!(recovered.next_attempt_utc_ms, 50);
        assert_eq!(
            store
                .cloud_outboxes_for_session(session_id)
                .expect("session outboxes"),
            vec![recovered]
        );
    }
    #[test]
    fn cloud_bundle_imports_valid_review_data_and_rejects_invalid_data() {
        let (_directory, source) = store();
        let session_id = MeetingSessionId::new();
        review_ready_session(&source, session_id);
        let bundle = cloud_bundle::CloudMeetingBundleV1::export_from_store(&source, session_id)
            .expect("export bundle");
        assert!(!bundle.audio_included);
        let (_destination_directory, destination) = store();
        let imported = bundle
            .clone()
            .import_into_store(&destination)
            .expect("import bundle");
        assert_eq!(imported, session_id);
        assert_eq!(
            destination
                .session_snapshot(imported)
                .expect("imported review")
                .phase,
            MeetingPhase::ReviewReady
        );
        let mut invalid = bundle;
        invalid.audio_included = true;
        assert_eq!(
            invalid.import_into_store(&destination),
            Err(StoreError::Invalid)
        );
    }

    #[test]
    fn cloud_conflict_choice_keeps_local_or_acknowledges_installed_remote() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        let revision = review_ready_session(&store, session_id);
        let object_id = "object_123456789";
        let remote_revision_id = "revision_1234567";
        let path = store
            .cloud_conflict_staging_path(object_id)
            .expect("conflict staging path");
        fs::write(&path, b"validated remote bundle").expect("cache remote bundle");
        let conflict = CloudConflict {
            object_id: object_id.to_string(),
            source_session_id: Some(session_id),
            source_revision: Some(revision),
            remote_revision_id: remote_revision_id.to_string(),
            remote_sequence: 7,
            remote_bundle_relative_path: format!(".cloud-conflicts/{object_id}.bundle"),
        };
        store
            .cache_cloud_conflict(&conflict)
            .expect("cache conflict");
        let local = store
            .resolve_cloud_conflict_keep_local(object_id, "keep-local".to_string(), 2)
            .expect("keep local");
        assert_eq!(
            local.base_remote_revision_id.as_deref(),
            Some(remote_revision_id)
        );
        assert!(store
            .cloud_conflict(object_id)
            .expect("conflict lookup")
            .is_none());
        store
            .cache_cloud_conflict(&conflict)
            .expect("recache conflict");
        let head = store
            .resolve_cloud_conflict_use_remote(object_id, session_id)
            .expect("use remote");
        assert_eq!(
            head.acknowledged_revision_id.as_deref(),
            Some(remote_revision_id)
        );
        assert!(store
            .cloud_conflict(object_id)
            .expect("conflict lookup")
            .is_none());
    }

    /// Seed one listable session: created at `created_at_utc_ms`, in `phase`,
    /// with `processing_status` as the stored JSON tag, one closed capture
    /// window of `capture_ms`, both source tracks, and the named speakers.
    fn list_session(
        store: &MeetingStore,
        created_at_utc_ms: i64,
        phase: MeetingPhase,
        processing_status: &str,
        capture_ms: i64,
        speakers: &[&str],
    ) -> MeetingSessionId {
        let session_id = MeetingSessionId::new();
        review_ready_session(store, session_id);
        let plan_id = MeetingPlanId::new();
        let connection = store.connection().expect("store connection");
        connection
            .execute(
                "UPDATE meeting_sessions
                 SET created_at_utc_ms = ?1, phase = ?2, processing_status = ?3
                 WHERE id = ?4",
                params![
                    created_at_utc_ms,
                    phase_db(phase),
                    processing_status,
                    id(session_id)
                ],
            )
            .expect("set listed session state");
        connection
            .execute(
                "INSERT INTO meeting_run_plans (
                    plan_id, session_id, attempt_number, schema_version, consent_id,
                    canonical_plan_json, created_at_utc_ms
                 ) VALUES (?1, ?2, 1, 1, ?3, '{}', 1)",
                params![id(plan_id), id(session_id), id(ConsentId::new())],
            )
            .expect("insert listed plan");
        for source_kind in ["microphone", "system_audio"] {
            connection
                .execute(
                    "INSERT INTO meeting_source_tracks (
                        track_id, session_id, plan_id, source_kind, required, requested,
                        descriptor_json, timestamp_bridge_json, health
                     ) VALUES (?1, ?2, ?3, ?4, 1, 1, '{}', '{}', '\"healthy\"')",
                    params![
                        id(SourceTrackId::new()),
                        id(session_id),
                        id(plan_id),
                        source_kind
                    ],
                )
                .expect("insert listed track");
        }
        // Two windows, one still open: only the closed one is recorded time,
        // which is what a paused-then-abandoned session really has.
        connection
            .execute(
                "INSERT INTO meeting_capture_windows (
                    session_id, sequence, start_offset_ns, end_offset_ns, close_reason
                 ) VALUES (?1, 0, 0, ?2, 'paused'), (?1, 1, ?2, NULL, NULL)",
                params![id(session_id), capture_ms * 1_000_000],
            )
            .expect("insert listed capture windows");
        for name in speakers {
            connection
                .execute(
                    "INSERT INTO meeting_speakers (
                        speaker_id, session_id, source_kind, display_name, revision
                     ) VALUES (?1, ?2, 'microphone', ?3, 0)",
                    params![id(SpeakerId::new()), id(session_id), name],
                )
                .expect("insert listed speaker");
        }
        session_id
    }

    /// Attach one current artifact revision carrying `summary` and `ledger`.
    fn list_artifact(
        store: &MeetingStore,
        session_id: MeetingSessionId,
        summary: &str,
        ledger_headline: Option<&str>,
    ) {
        let mut artifacts = trend_artifacts(0);
        artifacts.summary = CitedArtifactText {
            text: summary.to_string(),
            citations: Vec::new(),
        };
        artifacts.ledger = ledger_headline.map(|headline| crate::meeting::ledger::MeetingLedger {
            headline: headline.to_string(),
            threads: Vec::new(),
            open_loops: Vec::new(),
            commitments: Vec::new(),
            stances: Vec::new(),
            caveats: Vec::new(),
            receipts: crate::meeting::ledger::LedgerReceiptState::Verified,
        });
        /* A meeting with generated prose always has a transcript behind it, so
         * the fixture builds one and makes it current. That also proves the
         * artifact path short-circuits before the word count. */
        let transcript_revision_id = TranscriptRevisionId::new();
        let connection = store.connection().expect("store connection");
        connection
            .execute(
                "INSERT INTO meeting_transcript_revisions (
                    transcript_revision_id, session_id, engine_id, destination_json,
                    source_set_json, language, state, created_at_utc_ms, completed_at_utc_ms
                 ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'completed', 1, 1)",
                params![id(transcript_revision_id), id(session_id)],
            )
            .expect("insert artifact transcript revision");
        connection
            .execute(
                "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
                params![id(transcript_revision_id), id(session_id)],
            )
            .expect("set artifact current transcript");
        connection
            .execute(
                "INSERT INTO meeting_artifact_revisions (
                    artifact_id, session_id, transcript_revision_id, input_revision, template_id,
                    template_version, generation_key, state, content_json, generated_at_utc_ms
                 ) VALUES (?1, ?2, ?3, 0, 'test', 1, ?4, 'current', ?5, 1)",
                params![
                    id(MeetingArtifactId::new()),
                    id(session_id),
                    id(transcript_revision_id),
                    format!("key-{}", id(session_id)),
                    serde_json::to_string(&artifacts).expect("encode listed artifacts"),
                ],
            )
            .expect("insert listed artifact");
    }

    /// Give `session_id` a current transcript of `texts` and no artifacts, so
    /// its row has to fall back to counting words.
    fn list_transcript(store: &MeetingStore, session_id: MeetingSessionId, texts: &[&str]) {
        let transcript_revision_id = TranscriptRevisionId::new();
        let speaker_id = SpeakerId::new();
        let connection = store.connection().expect("store connection");
        let track_id: String = connection
            .query_row(
                "SELECT track_id FROM meeting_source_tracks WHERE session_id = ?1 LIMIT 1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .expect("listed track");
        connection
            .execute(
                "INSERT INTO meeting_transcript_revisions (
                    transcript_revision_id, session_id, engine_id, destination_json,
                    source_set_json, language, state, created_at_utc_ms, completed_at_utc_ms
                 ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'completed', 1, 1)",
                params![id(transcript_revision_id), id(session_id)],
            )
            .expect("insert listed transcript revision");
        connection
            .execute(
                "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
                params![id(transcript_revision_id), id(session_id)],
            )
            .expect("set listed current transcript");
        connection
            .execute(
                "INSERT INTO meeting_speakers (
                    speaker_id, session_id, source_kind, display_name, revision
                 ) VALUES (?1, ?2, 'microphone', 'Transcript speaker', 0)",
                params![id(speaker_id), id(session_id)],
            )
            .expect("insert listed transcript speaker");
        for (ordinal, text) in texts.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO meeting_transcript_segments (
                        segment_id, transcript_revision_id, track_id, ordinal, start_offset_ns,
                        end_offset_ns, speaker_id, base_text, confidence_milli
                     ) VALUES (?1, ?2, ?3, ?4, 0, 1000000000, ?5, ?6, NULL)",
                    params![
                        id(TranscriptSegmentId::new()),
                        id(transcript_revision_id),
                        track_id,
                        i64::try_from(ordinal).expect("segment ordinal"),
                        id(speaker_id),
                        text
                    ],
                )
                .expect("insert listed transcript segment");
        }
    }

    fn listed(store: &MeetingStore, filter: &MeetingListFilter) -> Vec<MeetingHistorySummary> {
        store
            .list_sessions(None, 50, filter)
            .expect("meeting list page")
            .entries
    }

    #[test]
    fn listed_rows_carry_recorded_capture_sources_and_speaker_labels() {
        let (_directory, store) = store();
        let session_id = list_session(
            &store,
            local_noon_ms(Local::now().date_naive()),
            MeetingPhase::ReviewReady,
            r#"{"kind":"succeeded"}"#,
            192_000,
            &["Ada", "Grace"],
        );
        list_artifact(&store, session_id, "Shipped the parser.", None);

        let entries = listed(&store, &MeetingListFilter::default());
        assert_eq!(entries.len(), 1);
        let row = &entries[0];
        // The open second window contributes nothing: only closed capture is
        // time this meeting can prove it recorded.
        assert_eq!(row.recorded_duration_ms, Some(192_000));
        assert_eq!(
            row.sources,
            vec![SourceKind::Microphone, SourceKind::SystemAudio]
        );
        assert_eq!(row.speaker_labels, vec!["Ada", "Grace"]);
    }

    #[test]
    fn listed_headline_prefers_the_ledger_then_one_summary_sentence_then_words() {
        let (_directory, store) = store();
        let today = local_noon_ms(Local::now().date_naive());
        let ledger = list_session(
            &store,
            today,
            MeetingPhase::ReviewReady,
            r#"{"kind":"succeeded"}"#,
            1_000,
            &[],
        );
        list_artifact(
            &store,
            ledger,
            "Summary that must lose to the ledger.",
            Some("Pricing came back at the end and is open again."),
        );
        let summary = list_session(
            &store,
            today - 1,
            MeetingPhase::ReviewReady,
            r#"{"kind":"succeeded"}"#,
            1_000,
            &[],
        );
        list_artifact(
            &store,
            summary,
            "First sentence stands alone. Second sentence must not reach the row.",
            None,
        );
        let words = list_session(
            &store,
            today - 2,
            MeetingPhase::ReviewReady,
            r#"{"kind":"succeeded"}"#,
            1_000,
            &[],
        );
        list_transcript(&store, words, &["one two three", "four five"]);
        let silent = list_session(
            &store,
            today - 3,
            MeetingPhase::Processing,
            r#"{"kind":"running"}"#,
            1_000,
            &[],
        );

        let entries = listed(&store, &MeetingListFilter::default());
        let headline = |session_id: MeetingSessionId| {
            entries
                .iter()
                .find(|row| row.session_id == session_id)
                .map(|row| row.headline.clone())
                .expect("listed session")
        };
        assert_eq!(
            headline(ledger),
            MeetingHistoryHeadline::Ledger {
                text: "Pricing came back at the end and is open again.".to_string()
            }
        );
        assert_eq!(
            headline(summary),
            MeetingHistoryHeadline::Summary {
                text: "First sentence stands alone.".to_string()
            }
        );
        assert_eq!(headline(words), MeetingHistoryHeadline::Words { words: 5 });
        // Nothing generated and nothing transcribed is not a zero word count.
        assert_eq!(headline(silent), MeetingHistoryHeadline::None);
    }

    #[test]
    fn listed_status_filter_reads_stored_phase_and_processing_status() {
        let (_directory, store) = store();
        let today = local_noon_ms(Local::now().date_naive());
        let ready = list_session(
            &store,
            today,
            MeetingPhase::ReviewReady,
            r#"{"kind":"succeeded"}"#,
            1_000,
            &[],
        );
        let processing = list_session(
            &store,
            today - 1,
            MeetingPhase::Processing,
            r#"{"kind":"running"}"#,
            1_000,
            &[],
        );
        let failed = list_session(
            &store,
            today - 2,
            MeetingPhase::ReviewReady,
            r#"{"kind":"failed","reason":"engine_failure"}"#,
            1_000,
            &[],
        );
        let recovery = list_session(
            &store,
            today - 3,
            MeetingPhase::RecoveryRequired,
            r#"{"kind":"pending"}"#,
            1_000,
            &[],
        );

        let ids = |status| {
            listed(
                &store,
                &MeetingListFilter {
                    status,
                    ..MeetingListFilter::default()
                },
            )
            .into_iter()
            .map(|row| row.session_id)
            .collect::<Vec<_>>()
        };
        assert_eq!(ids(MeetingStatusFilter::Any).len(), 4);
        assert_eq!(ids(MeetingStatusFilter::Ready), vec![ready]);
        // Recovery is pending processing, so it answers both questions a
        // person can ask about it: it is not finished, and it needs a hand.
        assert_eq!(
            ids(MeetingStatusFilter::Processing),
            vec![processing, recovery]
        );
        assert_eq!(ids(MeetingStatusFilter::Failed), vec![failed, recovery]);
    }

    #[test]
    fn listed_time_window_counts_local_calendar_days_including_today() {
        let (_directory, store) = store();
        let today = Local::now().date_naive();
        let day = |back: u64| {
            local_noon_ms(
                today
                    .checked_sub_days(chrono::Days::new(back))
                    .expect("representable past day"),
            )
        };
        for back in [0_u64, 3, 12, 40] {
            list_session(
                &store,
                day(back),
                MeetingPhase::ReviewReady,
                r#"{"kind":"succeeded"}"#,
                1_000,
                &[],
            );
        }

        let count = |window| {
            listed(
                &store,
                &MeetingListFilter {
                    window,
                    ..MeetingListFilter::default()
                },
            )
            .len()
        };
        assert_eq!(count(MeetingTimeWindow::Any), 4);
        assert_eq!(count(MeetingTimeWindow::Today), 1);
        assert_eq!(count(MeetingTimeWindow::Last7Days), 2);
        assert_eq!(count(MeetingTimeWindow::Last30Days), 3);
    }

    #[test]
    fn listed_title_query_matches_a_substring_and_treats_wildcards_literally() {
        let (_directory, store) = store();
        let today = local_noon_ms(Local::now().date_naive());
        let plain = list_session(
            &store,
            today,
            MeetingPhase::ReviewReady,
            r#"{"kind":"succeeded"}"#,
            1_000,
            &[],
        );
        let percent = list_session(
            &store,
            today - 1,
            MeetingPhase::ReviewReady,
            r#"{"kind":"succeeded"}"#,
            1_000,
            &[],
        );
        let connection = store.connection().expect("store connection");
        connection
            .execute(
                "UPDATE meeting_sessions SET title = 'Pricing review' WHERE id = ?1",
                params![id(plain)],
            )
            .expect("set plain title");
        connection
            .execute(
                "UPDATE meeting_sessions SET title = '100% done' WHERE id = ?1",
                params![id(percent)],
            )
            .expect("set wildcard title");
        drop(connection);

        let ids = |query: &str| {
            listed(
                &store,
                &MeetingListFilter {
                    title_query: query.to_string(),
                    ..MeetingListFilter::default()
                },
            )
            .into_iter()
            .map(|row| row.session_id)
            .collect::<Vec<_>>()
        };
        // A substring, case-folded, and blank means no constraint.
        assert_eq!(ids("ricing"), vec![plain]);
        assert_eq!(ids("PRICING"), vec![plain]);
        assert_eq!(ids("   ").len(), 2);
        // A typed `%` is a per-cent sign, not "match anything".
        assert_eq!(ids("100%"), vec![percent]);
        assert!(ids("%done%").is_empty());
    }

    #[test]
    fn listed_pages_apply_the_filter_to_every_page_and_report_more() {
        let (_directory, store) = store();
        let today = local_noon_ms(Local::now().date_naive());
        for offset in 0..3_i64 {
            list_session(
                &store,
                today - offset,
                MeetingPhase::ReviewReady,
                r#"{"kind":"succeeded"}"#,
                1_000,
                &[],
            );
            list_session(
                &store,
                today - offset,
                MeetingPhase::Processing,
                r#"{"kind":"running"}"#,
                1_000,
                &[],
            );
        }
        let filter = MeetingListFilter {
            status: MeetingStatusFilter::Ready,
            ..MeetingListFilter::default()
        };

        let first = store
            .list_sessions(None, 2, &filter)
            .expect("first filtered page");
        assert_eq!(first.entries.len(), 2);
        assert!(first.has_more);
        let cursor = first
            .entries
            .last()
            .expect("first page has a last row")
            .created_at_utc_ms;
        let second = store
            .list_sessions(Some(cursor), 2, &filter)
            .expect("second filtered page");
        assert_eq!(second.entries.len(), 1);
        assert!(!second.has_more);
        // Page two obeys the same filter, so a Processing row can never appear
        // in a Ready list just because the cursor moved.
        assert!(second
            .entries
            .iter()
            .all(|row| row.phase == MeetingPhase::ReviewReady));
    }

    /* ------------------------------------------- startup recovery invariant */

    /// A meeting the way an unannounced end of a launch leaves it: a phase
    /// nothing is working on any more and whatever status was last written.
    fn stranded_session(
        store: &Arc<MeetingStore>,
        phase: &str,
        status: ProcessingStatus,
    ) -> MeetingSessionId {
        let session_id = MeetingSessionId::new();
        store
            .create_preflight(
                StoreMutation {
                    operation_id: MeetingOperationId::new(),
                    actor: OperationActor::User,
                    requested_at_utc_ms: 1,
                    session_id,
                    expected_revision: 0,
                    command: MeetingCommandKind::PreflightCreate,
                },
                "Design sync".to_string(),
                MeetingOrigin::Manual,
                preflight(session_id),
                MeetingRetentionPolicy::Forever,
            )
            .unwrap();
        let connection = store.connection().unwrap();
        connection
            .execute(
                "UPDATE meeting_sessions SET phase = ?1, processing_status = ?2 WHERE id = ?3",
                params![phase, encode_json(&status).unwrap(), id(session_id)],
            )
            .unwrap();
        session_id
    }

    fn latest_event(store: &Arc<MeetingStore>, session_id: MeetingSessionId) -> (String, String) {
        let connection = store.connection().unwrap();
        connection
            .query_row(
                "SELECT event_kind, details_json FROM meeting_session_events
                  WHERE session_id = ?1 ORDER BY sequence DESC LIMIT 1",
                params![id(session_id)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    fn event_count(store: &Arc<MeetingStore>, session_id: MeetingSessionId) -> i64 {
        let connection = store.connection().unwrap();
        connection
            .query_row(
                "SELECT COUNT(*) FROM meeting_session_events WHERE session_id = ?1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn listed_ids(store: &Arc<MeetingStore>, status: MeetingStatusFilter) -> Vec<MeetingSessionId> {
        store
            .list_sessions(
                None,
                100,
                &MeetingListFilter {
                    status,
                    ..MeetingListFilter::default()
                },
            )
            .expect("list sessions")
            .entries
            .into_iter()
            .map(|entry| entry.session_id)
            .collect()
    }

    /// A start gate the way the app writes it, and the way seven of them were
    /// found sitting in a real store: `create_preflight` and nothing after it.
    /// No consent, no run plan, no track, no capture window, no duration, and
    /// the `pending` status every meeting is born with.
    fn open_start_gate(store: &Arc<MeetingStore>) -> MeetingSessionId {
        let session_id = MeetingSessionId::new();
        store
            .create_preflight(
                StoreMutation {
                    operation_id: MeetingOperationId::new(),
                    actor: OperationActor::User,
                    requested_at_utc_ms: 1,
                    session_id,
                    expected_revision: 0,
                    command: MeetingCommandKind::PreflightCreate,
                },
                "Local notes".to_string(),
                MeetingOrigin::Manual,
                preflight(session_id),
                MeetingRetentionPolicy::Forever,
            )
            .expect("open the start gate");
        session_id
    }

    /// A meeting that made it all the way through, which is what shares the
    /// list with the abandoned gates and what a sweep that deletes rows has to
    /// leave standing.
    fn finished_session(store: &Arc<MeetingStore>) -> MeetingSessionId {
        let session_id = MeetingSessionId::new();
        review_ready_session(store, session_id);
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE meeting_sessions SET processing_status = ?1 WHERE id = ?2",
                params![
                    encode_json(&ProcessingStatus::Succeeded).unwrap(),
                    id(session_id)
                ],
            )
            .unwrap();
        session_id
    }

    /// Matrix 1: the shape that showed "Processing" for days. Recovery has to
    /// leave a terminal status behind, or the row keeps advertising work that
    /// no job is doing.
    #[test]
    fn recovery_gives_an_interrupted_meeting_a_terminal_status() {
        let (_directory, store) = store();
        let session_id = stranded_session(&store, "processing", ProcessingStatus::Pending);
        let before = store.session_snapshot(session_id).unwrap().revision;

        let recovery = store.recover_interrupted().expect("recovery sweep");

        let snapshot = store.session_snapshot(session_id).unwrap();
        assert_eq!(snapshot.phase, MeetingPhase::RecoveryRequired);
        assert_eq!(
            snapshot.processing_status,
            ProcessingStatus::Failed {
                reason: ProcessingFailure::Interrupted,
                cause: None
            }
        );
        assert_eq!(snapshot.revision, before + 1);
        assert_eq!(
            recovery.recovered,
            vec![RecoveredMeeting {
                session_id,
                prior_phase: MeetingPhase::Processing,
            }],
            "the phase the launch died in is the only eligibility discriminator"
        );
        let (event_kind, details) = latest_event(&store, session_id);
        assert_eq!(event_kind, "recovery_required");
        assert_eq!(details, r#"{"prior_phase":"processing"}"#);
        assert_eq!(
            listed_ids(&store, MeetingStatusFilter::Failed),
            vec![session_id]
        );
        assert!(
            listed_ids(&store, MeetingStatusFilter::Processing).is_empty(),
            "an abandoned meeting must leave the Processing filter"
        );
    }

    /// Matrix 2: a launch that died mid-recording ends in the same terminal
    /// shape, and says so — the prior phase is what later refuses to reprocess
    /// it unasked.
    #[test]
    fn recovery_records_the_capture_phase_it_interrupted() {
        let (_directory, store) = store();
        let session_id = stranded_session(&store, "capturing_recording", ProcessingStatus::Pending);

        let recovery = store.recover_interrupted().expect("recovery sweep");

        let snapshot = store.session_snapshot(session_id).unwrap();
        assert_eq!(snapshot.phase, MeetingPhase::RecoveryRequired);
        assert_eq!(
            snapshot.processing_status,
            ProcessingStatus::Failed {
                reason: ProcessingFailure::Interrupted,
                cause: None
            }
        );
        assert_eq!(
            recovery
                .recovered
                .first()
                .map(|meeting| meeting.prior_phase),
            Some(MeetingPhase::CapturingRecording)
        );
        assert_eq!(
            latest_event(&store, session_id).1,
            r#"{"prior_phase":"capturing_recording"}"#
        );
    }

    /// Matrix 3: the rows already on disk from launches that flipped the phase
    /// alone. They heal without a new revision or a new event, because nothing
    /// about the meeting changed — only what was always true is written down.
    #[test]
    fn recovery_heals_a_meeting_already_parked_with_an_unfinished_status() {
        let (_directory, store) = store();
        let session_id = stranded_session(&store, "recovery_required", ProcessingStatus::Pending);
        let before = store.session_snapshot(session_id).unwrap();
        let events = event_count(&store, session_id);

        let recovery = store.recover_interrupted().expect("recovery sweep");

        let after = store.session_snapshot(session_id).unwrap();
        assert_eq!(
            after.processing_status,
            ProcessingStatus::Failed {
                reason: ProcessingFailure::Interrupted,
                cause: None
            }
        );
        assert_eq!(after.phase, MeetingPhase::RecoveryRequired);
        assert_eq!(after.revision, before.revision);
        assert_eq!(event_count(&store, session_id), events);
        assert!(recovery.recovered.is_empty());
        assert_eq!(recovery.status_resolved, vec![session_id]);
        assert!(
            listed_ids(&store, MeetingStatusFilter::Processing).is_empty(),
            "the row the person saw as Processing must stop matching that filter"
        );
        assert_eq!(
            listed_ids(&store, MeetingStatusFilter::Failed),
            vec![session_id]
        );
    }

    /// Matrix 4: a meeting whose failure is already recorded keeps the reason
    /// it has. A terminal status is a fixpoint, not something to overwrite.
    #[test]
    fn recovery_leaves_a_recorded_failure_reason_alone() {
        let (_directory, store) = store();
        let session_id = stranded_session(
            &store,
            "recovery_required",
            ProcessingStatus::Failed {
                reason: ProcessingFailure::EngineFailure,
                cause: Some(EngineFailureCause::ModelRefused),
            },
        );
        let before = store.session_snapshot(session_id).unwrap();

        let recovery = store.recover_interrupted().expect("recovery sweep");

        assert_eq!(store.session_snapshot(session_id).unwrap(), before);
        assert!(recovery.recovered.is_empty());
        assert!(recovery.status_resolved.is_empty());
    }

    /// Matrix 5: the sweep is a fixpoint as a whole, so running it twice in one
    /// launch cannot walk a meeting further away from where it belongs.
    #[test]
    fn recovery_run_twice_changes_nothing_the_second_time() {
        let (_directory, store) = store();
        let interrupted = stranded_session(&store, "stopping", ProcessingStatus::Pending);
        let parked = stranded_session(&store, "recovery_required", ProcessingStatus::Pending);

        store.recover_interrupted().expect("first sweep");
        let after_first = (
            store.session_snapshot(interrupted).unwrap(),
            store.session_snapshot(parked).unwrap(),
            event_count(&store, interrupted),
        );

        let second = store.recover_interrupted().expect("second sweep");

        assert!(second.recovered.is_empty());
        assert!(second.status_resolved.is_empty());
        assert!(second.discarded.is_empty());
        assert_eq!(
            (
                store.session_snapshot(interrupted).unwrap(),
                store.session_snapshot(parked).unwrap(),
                event_count(&store, interrupted),
            ),
            after_first
        );
    }

    /// Matrix 6: the shape that walked past every row above and was found in a
    /// real store seven times over. A start gate is born `pending`, and both
    /// the Processing filter and the row chip read `pending` as work in
    /// flight, so a launch that ended with the gate open left a meeting that
    /// never happened advertising a job that never existed. There is nothing
    /// to park for review — no consent, no plan, no audio — so the sweep sends
    /// the cancel the closing window never sent.
    #[test]
    fn recovery_discards_a_start_gate_an_earlier_launch_left_open() {
        let (_directory, store) = store();
        let finished = finished_session(&store);
        let gate = open_start_gate(&store);
        assert_eq!(
            listed_ids(&store, MeetingStatusFilter::Processing),
            vec![gate],
            "an open gate is exactly what the person was shown as Processing"
        );

        let recovery = store.recover_interrupted().expect("recovery sweep");

        assert_eq!(recovery.discarded, vec![gate]);
        assert!(recovery.recovered.is_empty());
        assert!(recovery.status_resolved.is_empty());
        assert!(matches!(
            store.session_snapshot(gate),
            Err(StoreError::NotFound)
        ));
        assert_eq!(
            event_count(&store, gate),
            0,
            "the gate's own history goes with it"
        );
        assert!(listed_ids(&store, MeetingStatusFilter::Processing).is_empty());
        assert_eq!(
            listed_ids(&store, MeetingStatusFilter::Any),
            vec![finished],
            "the meetings that did happen are not what this deletes"
        );
    }

    /// Matrix 7: pressing Refresh before walking away bumps the revision and
    /// writes a second event, which is the one live gate that was not still at
    /// revision zero. It is the same draft, so it goes the same way, and the
    /// deletion leaves the second sweep of the launch nothing to find.
    #[test]
    fn recovery_discards_a_refreshed_start_gate_and_stays_a_fixpoint() {
        let (_directory, store) = store();
        let finished = finished_session(&store);
        let gate = open_start_gate(&store);
        store
            .refresh_preflight(MeetingOperationId::new(), 2, gate, 0, preflight(gate))
            .expect("refresh the gate");
        assert_eq!(store.session_snapshot(gate).unwrap().revision, 1);

        let first = store.recover_interrupted().expect("first sweep");
        let second = store.recover_interrupted().expect("second sweep");

        assert_eq!(first.discarded, vec![gate]);
        assert!(second.discarded.is_empty());
        assert_eq!(listed_ids(&store, MeetingStatusFilter::Any), vec![finished]);
    }

    /// A meeting that lost its records on disk is exactly what the repair pass
    /// writes a `MissingRecord` gap for, and reading that back is how automatic
    /// reprocessing knows to keep its hands off.
    #[test]
    fn missing_records_are_readable_as_a_gap_after_recovery() {
        let (_directory, store) = store();
        let (session_id, _track_id, _storage) = microphone_track(
            &store,
            TimestampBridge {
                native_anchor_value: 0,
                native_timescale: 1_000_000_000,
                host_monotonic_anchor_ns: 0,
                session_offset_ns: 0,
            },
        );
        assert!(!store.has_missing_record_gap(session_id).unwrap());

        store.recover_interrupted().expect("recovery sweep");

        assert!(
            store.has_missing_record_gap(session_id).unwrap(),
            "a track whose record file was never written reads as missing audio"
        );
    }

    /// The undo bin, end to end: a deletion leaves the audio in `.trash/` and a
    /// receipt that can put the meeting back, a restore does put it back, and the
    /// sweep is the only thing that makes a deletion final.
    #[test]
    fn a_deleted_meeting_stays_restorable_for_its_thirty_days() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        let revision = review_ready_session(&store, session_id);
        let live = store.root.join(session_id.uuid().to_string());
        fs::create_dir_all(live.join("tracks")).expect("session directory");
        fs::write(live.join("tracks").join("audio"), b"records").expect("session audio");

        let (_receipt, job_id) = store
            .reserve_deletion(
                MeetingOperationId::new(),
                10,
                session_id,
                revision,
                DeletionCause::User,
            )
            .expect("reserve deletion");
        store.finish_deletion(job_id).expect("finish deletion");

        assert!(
            store.session_snapshot(session_id).is_err(),
            "a deleted meeting is gone from every surface that reads rows"
        );
        assert!(!live.exists(), "the audio moved out of the live directory");
        let trashed = store.meeting_trash(20).expect("trash list");
        assert_eq!(trashed.len(), 1);
        assert_eq!(trashed[0].job_id, job_id);
        assert_eq!(trashed[0].title, "Design sync");
        assert_eq!(
            trashed[0].deleted_at_utc_ms,
            trashed[0].expires_at_utc_ms - TRASH_RETENTION_MS
        );

        let restored = store
            .restore_trashed_meeting(job_id, 20)
            .expect("restore the deletion");

        assert_eq!(restored, session_id);
        assert_eq!(
            store
                .session_snapshot(session_id)
                .expect("restored snapshot")
                .title,
            "Design sync"
        );
        assert!(
            live.join("tracks").join("audio").exists(),
            "the audio came back with the rows"
        );
        assert!(
            store.meeting_trash(20).expect("trash list").is_empty(),
            "a restored meeting is no longer in the bin"
        );
        assert_eq!(
            store.restore_trashed_meeting(job_id, 20),
            Err(StoreError::NotFound),
            "the undo is spent once it has been used"
        );
    }

    /// After thirty days the sweep purges the bin: the directory goes, the undo
    /// goes, and the receipt that records the deletion stays.
    #[test]
    fn the_sweep_makes_a_deletion_final_after_its_thirty_days() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        let revision = review_ready_session(&store, session_id);
        let live = store.root.join(session_id.uuid().to_string());
        fs::create_dir_all(&live).expect("session directory");
        let (_receipt, job_id) = store
            .reserve_deletion(
                MeetingOperationId::new(),
                10,
                session_id,
                revision,
                DeletionCause::User,
            )
            .expect("reserve deletion");
        store.finish_deletion(job_id).expect("finish deletion");
        let deleted_at = store.meeting_trash(20).expect("trash list")[0].deleted_at_utc_ms;

        assert_eq!(
            store
                .purge_expired_trash(deleted_at + TRASH_RETENTION_MS - 1)
                .expect("sweep inside the horizon"),
            0,
            "an instant short of the horizon is still inside it"
        );
        assert_eq!(
            store
                .purge_expired_trash(deleted_at + TRASH_RETENTION_MS)
                .expect("sweep at the horizon"),
            1,
            "the expiry instant is the moment the undo runs out, and the list, \
             the restore and the sweep all read it that way"
        );

        assert!(store
            .meeting_trash(deleted_at)
            .expect("trash list")
            .is_empty());
        assert_eq!(
            store.restore_trashed_meeting(job_id, deleted_at),
            Err(StoreError::NotFound)
        );
        let connection = store.connection().expect("store connection");
        let receipts: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM meeting_deletion_receipts WHERE job_id = ?1",
                params![id(job_id)],
                |row| row.get(0),
            )
            .expect("deletion receipt");
        assert_eq!(
            receipts, 1,
            "the record that this meeting was deleted has no expiry"
        );
    }

    /// An expired entry never appears in the bin even before the sweep reaches
    /// it: the list and the sweep read one horizon, so a stale window cannot
    /// offer an undo the restore would refuse.
    #[test]
    fn an_expired_entry_is_not_offered_before_the_sweep_runs() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        let revision = review_ready_session(&store, session_id);
        fs::create_dir_all(store.root.join(session_id.uuid().to_string()))
            .expect("session directory");
        let (_receipt, job_id) = store
            .reserve_deletion(
                MeetingOperationId::new(),
                10,
                session_id,
                revision,
                DeletionCause::User,
            )
            .expect("reserve deletion");
        store.finish_deletion(job_id).expect("finish deletion");
        let deleted_at = store.meeting_trash(20).expect("trash list")[0].deleted_at_utc_ms;
        assert!(store
            .meeting_trash(deleted_at + TRASH_RETENTION_MS)
            .expect("trash list")
            .is_empty());
    }

    /// A recording that asked to announce itself records what the paste did,
    /// including — and especially — a refusal: a target that cannot accept an
    /// insertion is the ordinary case, and the live surface says so quietly
    /// because this row says so first.
    #[test]
    fn a_disclosure_records_its_one_attempt_even_when_the_target_refused() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        review_ready_session(&store, session_id);

        assert_eq!(
            store.session_disclosure(session_id).expect("disclosure"),
            MeetingSessionDisclosure::NotAsked,
            "a meeting nobody asked to announce itself has nothing to post"
        );

        store
            .request_session_disclosure(session_id, "Aktan Azat")
            .expect("arm the disclosure");

        assert_eq!(
            store.session_disclosure(session_id).expect("disclosure"),
            MeetingSessionDisclosure::Pending {
                notetaker: "Aktan Azat".to_string()
            }
        );

        let refused = crate::delivery::DeliveryReceipt::not_dispatched();
        let recorded = store
            .record_session_disclosure(session_id, &refused)
            .expect("record the attempt");

        assert_eq!(
            recorded,
            MeetingSessionDisclosure::Attempted {
                receipt: refused.clone()
            }
        );
        assert_eq!(
            store.session_disclosure(session_id).expect("disclosure"),
            recorded,
            "the record is the store's, not the return value's"
        );

        // One line per recording. A second attempt reads the first back rather
        // than putting another sentence in somebody's chat.
        let delivered = crate::delivery::DeliveryReceipt {
            method: crate::delivery::DeliveryMethod::AccessibilityInsertion,
            outcome: crate::delivery::DeliveryOutcome::Delivered,
            dispatched_at_ms: refused.dispatched_at_ms + 1_000,
        };
        assert_eq!(
            store
                .record_session_disclosure(session_id, &delivered)
                .expect("second attempt"),
            recorded
        );
        store
            .request_session_disclosure(session_id, "Somebody Else")
            .expect("arming again is a no-op");
        assert_eq!(
            store.session_disclosure(session_id).expect("disclosure"),
            recorded
        );
    }

    /// A second lane on the session `microphone_track` opened, so a test can
    /// ask what a whole meeting looks like rather than what one track does.
    fn system_audio_track(
        store: &Arc<MeetingStore>,
        session_id: MeetingSessionId,
        timestamp_bridge: TimestampBridge,
    ) -> SourceTrackId {
        let plan_id: String = store
            .connection()
            .expect("store connection")
            .query_row(
                "SELECT plan_id FROM meeting_source_tracks WHERE session_id = ?1 LIMIT 1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .expect("existing lane plan");
        let track_id = SourceTrackId::new();
        store
            .create_track(TrackCreation {
                session_id,
                plan_id: MeetingPlanId::from_uuid(parse_uuid(&plan_id).expect("plan uuid")),
                source_kind: SourceKind::SystemAudio,
                required: true,
                requested: true,
                descriptor_json: "{}",
                report: SourceStartReport {
                    track_id,
                    source_kind: SourceKind::SystemAudio,
                    format: AudioFormat {
                        sample_rate_hz: 48_000,
                        channels: 1,
                    },
                    epoch: SourceEpoch::new(0),
                    format_epoch: 1,
                    timestamp_bridge,
                },
            })
            .expect("system audio lane");
        track_id
    }

    fn test_bridge() -> TimestampBridge {
        TimestampBridge {
            native_anchor_value: 0,
            native_timescale: 1_000_000_000,
            host_monotonic_anchor_ns: 0,
            session_offset_ns: 0,
        }
    }

    fn lane(
        store: &MeetingStore,
        session_id: MeetingSessionId,
        source_kind: SourceKind,
    ) -> MeetingSourceSnapshot {
        store
            .session_snapshot(session_id)
            .expect("session snapshot")
            .sources
            .into_iter()
            .find(|source| source.source_kind == source_kind)
            .expect("lane snapshot")
    }

    /// Commits one packet starting at `start_offset_ns` down the writer path a
    /// live capture uses, which is the only way a lane earns its health.
    fn deliver_audio(
        store: &Arc<MeetingStore>,
        session_id: MeetingSessionId,
        track_id: SourceTrackId,
        storage: MeetingStoragePlan,
        start_offset_ns: i64,
    ) {
        let mut writer = store
            .open_track_writer(session_id, track_id, storage)
            .expect("track writer");
        let samples = vec![0.25; usize::try_from(TEST_PACKET_FRAMES).unwrap()];
        assert_eq!(
            writer
                .accept(
                    captured_packet(track_id, 0, Some(start_offset_ns)),
                    &samples
                )
                .expect("accept packet"),
            PacketPushResult::Accepted
        );
        writer.seal().expect("seal track");
    }

    fn captured_meeting(store: &Arc<MeetingStore>, microphone_start_ns: i64) -> MeetingSessionId {
        let (session_id, microphone_id, storage) = microphone_track(store, test_bridge());
        let system_audio_id = system_audio_track(store, session_id, test_bridge());
        store
            .open_capture_window(session_id, 0)
            .expect("open capture window");
        deliver_audio(
            store,
            session_id,
            microphone_id,
            storage.clone(),
            microphone_start_ns,
        );
        deliver_audio(store, session_id, system_audio_id, storage, 0);
        store
            .close_open_capture_window(session_id, 1_000_000_000, "stopped")
            .expect("close capture window");
        session_id
    }

    /// Every system-audio track in the owner's store held `starting` forever,
    /// because `create_track` wrote it and only a failure ever overwrote it.
    /// The packet that reaches the disk is what makes a lane healthy, and it is
    /// the same transaction, so the column cannot disagree with the records.
    #[test]
    fn a_committed_packet_promotes_its_lane_to_healthy() {
        let (_directory, store) = store();
        let (session_id, track_id, storage) = microphone_track(&store, test_bridge());

        assert_eq!(
            lane(&store, session_id, SourceKind::Microphone).health,
            SourceHealth::Starting,
            "a lane that has not written a record yet is still provisional"
        );

        deliver_audio(&store, session_id, track_id, storage, 0);

        assert_eq!(
            lane(&store, session_id, SourceKind::Microphone).health,
            SourceHealth::Healthy
        );
    }

    /// `starting` at the seal is not a lane that is still coming up: it is a
    /// lane whose audio never existed, however cleanly its source shut down.
    #[test]
    fn the_seal_fails_only_the_lane_that_never_delivered_audio() {
        let (_directory, store) = store();
        let (session_id, microphone_id, storage) = microphone_track(&store, test_bridge());
        system_audio_track(&store, session_id, test_bridge());
        deliver_audio(&store, session_id, microphone_id, storage, 0);

        store
            .seal_source_health(session_id)
            .expect("seal source health");

        assert_eq!(
            lane(&store, session_id, SourceKind::Microphone).health,
            SourceHealth::Healthy,
            "the seal must not overwrite a lane that earned its health"
        );
        assert_eq!(
            lane(&store, session_id, SourceKind::SystemAudio).health,
            SourceHealth::Failed
        );
    }

    /// No meeting in the owner's store had ever been `Complete`: the predicate
    /// demanded `Healthy`, which nothing granted, and zero gaps, which the
    /// unavoidable startup interval denies.
    #[test]
    fn a_meeting_whose_lanes_both_delivered_audio_is_complete() {
        let (_directory, store) = store();
        let session_id = captured_meeting(&store, 0);

        assert_eq!(
            store
                .session_snapshot(session_id)
                .expect("session snapshot")
                .capture_completeness,
            CaptureCompleteness::Complete
        );
    }

    /// A pause is the one gap nobody lost: the operator asked for that
    /// silence. It stays on the ledger for the timeline, and the lane that
    /// honoured it is as healthy as it was, so the meeting is still complete.
    #[test]
    fn a_pause_is_a_gap_that_lost_nothing() {
        let (_directory, store) = store();
        let session_id = captured_meeting(&store, 0);
        let track_id = lane(&store, session_id, SourceKind::Microphone)
            .track_id
            .expect("microphone lane");

        store
            .record_gap(&SourceGap {
                track_id,
                epoch: SourceEpoch::new(0),
                start_offset_ns: Some(5_000_000_000),
                end_offset_ns: Some(9_000_000_000),
                reason: SourceGapReason::Paused,
                dropped_frames: None,
            })
            .expect("record pause");

        let microphone = lane(&store, session_id, SourceKind::Microphone);
        assert_eq!(microphone.gap_count, 1, "the pause is still on the ledger");
        assert_eq!(microphone.health, SourceHealth::Healthy);
        assert_eq!(
            store
                .session_snapshot(session_id)
                .expect("session snapshot")
                .capture_completeness,
            CaptureCompleteness::Complete
        );
    }

    /// Two devices cannot be handed the same instant. The interval between the
    /// session clock and a lane's first buffer is audio the meeting does not
    /// have, so the ledger says so once, where that offset is first known - and
    /// it is startup latency, not a lane losing audio it was recording.
    #[test]
    fn the_first_committed_record_records_the_interval_before_it() {
        let (_directory, store) = store();
        let (session_id, track_id, storage) = microphone_track(&store, test_bridge());

        deliver_audio(&store, session_id, track_id, storage, 300_000_000);

        let gaps = store
            .review_snapshot(session_id)
            .expect("review snapshot")
            .gaps;
        assert_eq!(gaps.len(), 1, "one startup interval, not one per record");
        assert_eq!(gaps[0].start_offset_ns, Some(0));
        assert_eq!(gaps[0].end_offset_ns, Some(300_000_000));
        assert_eq!(gaps[0].reason, SourceGapReason::SourceUnavailable);
        assert_eq!(
            lane(&store, session_id, SourceKind::Microphone).health,
            SourceHealth::Healthy,
            "starting late is not a lane in trouble"
        );
    }

    /// If startup latency counted as lost audio, `Complete` would be
    /// unreachable for every meeting with two sources, and a reader would learn
    /// to ignore the word.
    #[test]
    fn startup_latency_leaves_the_meeting_complete() {
        let (_directory, store) = store();
        let session_id = captured_meeting(&store, 300_000_000);

        let snapshot = store
            .session_snapshot(session_id)
            .expect("session snapshot");
        assert_eq!(
            lane(&store, session_id, SourceKind::Microphone).gap_count,
            1,
            "the startup interval is still on the ledger"
        );
        assert_eq!(snapshot.capture_completeness, CaptureCompleteness::Complete);
    }

    /// Audio lost after a lane was running is the case the word `Partial` is
    /// for, and the lane that lost it is no longer healthy.
    #[test]
    fn audio_lost_mid_meeting_makes_the_meeting_partial() {
        let (_directory, store) = store();
        let session_id = captured_meeting(&store, 0);
        let track_id = lane(&store, session_id, SourceKind::Microphone)
            .track_id
            .expect("microphone lane");

        store
            .record_gap(&SourceGap {
                track_id,
                epoch: SourceEpoch::new(0),
                start_offset_ns: Some(500_000_000),
                end_offset_ns: Some(600_000_000),
                reason: SourceGapReason::PacketDropped,
                dropped_frames: Some(512),
            })
            .expect("record gap");

        assert_eq!(
            lane(&store, session_id, SourceKind::Microphone).health,
            SourceHealth::Degraded
        );
        assert_eq!(
            store
                .session_snapshot(session_id)
                .expect("session snapshot")
                .capture_completeness,
            CaptureCompleteness::Partial
        );
    }

    /// A condition that persists reports itself once per buffer: 568,941 rows
    /// for one unreadable format, two fsync'd autocommits each, on the
    /// connection both lanes share. The interval and the frame total are worth
    /// keeping; one row per buffer is not.
    #[test]
    fn consecutive_identical_gaps_extend_one_interval() {
        let (_directory, store) = store();
        let (session_id, track_id, _storage) = microphone_track(&store, test_bridge());

        for step in 0..3 {
            store
                .record_gap(&SourceGap {
                    track_id,
                    epoch: SourceEpoch::new(0),
                    start_offset_ns: Some(step * 20_000_000),
                    end_offset_ns: Some((step + 1) * 20_000_000),
                    reason: SourceGapReason::InvalidFormat,
                    dropped_frames: Some(960),
                })
                .expect("record gap");
        }

        let gaps = store
            .review_snapshot(session_id)
            .expect("review snapshot")
            .gaps;
        assert_eq!(gaps.len(), 1, "one condition, one interval");
        assert_eq!(gaps[0].start_offset_ns, Some(0));
        assert_eq!(gaps[0].end_offset_ns, Some(60_000_000));
        assert_eq!(gaps[0].dropped_frames, Some(2_880));
    }

    /// Coalescing is for a run of one condition. A different reason is a
    /// different fault and owns its own interval, including when the first one
    /// comes back.
    #[test]
    fn a_gap_with_another_reason_opens_its_own_interval() {
        let (_directory, store) = store();
        let (session_id, track_id, _storage) = microphone_track(&store, test_bridge());
        let reasons = [
            SourceGapReason::InvalidFormat,
            SourceGapReason::PacketDropped,
            SourceGapReason::InvalidFormat,
        ];

        for (step, reason) in reasons.into_iter().enumerate() {
            let step = u64::try_from(step).expect("step offset");
            store
                .record_gap(&SourceGap {
                    track_id,
                    epoch: SourceEpoch::new(0),
                    start_offset_ns: Some(step * 20_000_000),
                    end_offset_ns: Some((step + 1) * 20_000_000),
                    reason,
                    dropped_frames: None,
                })
                .expect("record gap");
        }

        assert_eq!(
            store
                .review_snapshot(session_id)
                .expect("review snapshot")
                .gaps
                .len(),
            3
        );
    }

    fn current_transcript(
        store: &MeetingStore,
        session_id: MeetingSessionId,
    ) -> TranscriptRevisionId {
        let raw: String = store
            .connection()
            .expect("store connection")
            .query_row(
                "SELECT current_transcript_revision_id FROM meeting_sessions WHERE id = ?1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .expect("current transcript revision");
        TranscriptRevisionId::from_uuid(parse_uuid(&raw).expect("transcript revision uuid"))
    }

    fn transcript_segments(
        store: &MeetingStore,
        revision: TranscriptRevisionId,
    ) -> Vec<TranscriptSegmentId> {
        let connection = store.connection().expect("store connection");
        let mut statement = connection
            .prepare(
                "SELECT segment_id FROM meeting_transcript_segments
                  WHERE transcript_revision_id = ?1 ORDER BY ordinal",
            )
            .expect("segment query");
        let ids = statement
            .query_map(params![id(revision)], |row| row.get::<_, String>(0))
            .expect("segment rows")
            .map(|raw| {
                TranscriptSegmentId::from_uuid(
                    parse_uuid(&raw.expect("segment id")).expect("segment uuid"),
                )
            })
            .collect();
        ids
    }

    fn generation_state(
        store: &MeetingStore,
        generation_id: MeetingDiarizationGenerationId,
    ) -> String {
        store
            .connection()
            .expect("store connection")
            .query_row(
                "SELECT state FROM meeting_diarization_generations WHERE generation_id = ?1",
                params![id(generation_id)],
                |row| row.get(0),
            )
            .expect("generation state")
    }

    /// All four diarization generations in the owner's store completed having
    /// assigned nobody, because they walked a system-audio track that held no
    /// records. Publishing that is what took the speaker off every segment: it
    /// is the absence of a result, not a result.
    #[test]
    fn a_generation_that_assigned_nobody_is_refused() {
        let (_directory, store) = store();
        let (session_id, _track_id, _storage) = microphone_track(&store, test_bridge());
        list_transcript(&store, session_id, &["one", "two"]);
        let transcript_revision_id = current_transcript(&store, session_id);
        let generation_id = store
            .begin_diarization_generation(session_id, transcript_revision_id, 0, "test", "1")
            .expect("begin generation");

        assert!(matches!(
            store.publish_diarization_generation(session_id, generation_id),
            Err(StoreError::Invalid)
        ));
        assert_eq!(
            generation_state(&store, generation_id),
            "failed",
            "the refusal is on the record for the caller to report"
        );
    }

    #[test]
    fn a_generation_that_assigned_a_speaker_publishes() {
        let (_directory, store) = store();
        let (session_id, _track_id, _storage) = microphone_track(&store, test_bridge());
        list_transcript(&store, session_id, &["one", "two"]);
        let transcript_revision_id = current_transcript(&store, session_id);
        let generation_id = store
            .begin_diarization_generation(session_id, transcript_revision_id, 0, "test", "1")
            .expect("begin generation");
        let speaker_id = store
            .diarization_speaker(session_id, 0)
            .expect("diarized speaker");
        let assignments = transcript_segments(&store, transcript_revision_id)
            .into_iter()
            .map(|segment_id| DiarizationAssignmentInput {
                segment_id,
                speaker_id,
                assignment: SpeakerAssignmentKind::SystemSpeaker,
            })
            .collect::<Vec<_>>();
        store
            .write_diarization_assignments(generation_id, &assignments)
            .expect("write assignments");

        store
            .publish_diarization_generation(session_id, generation_id)
            .expect("publish generation");

        assert_eq!(generation_state(&store, generation_id), "completed");
    }

    #[test]
    fn merged_speakers_are_canonical_in_stored_transcript_and_names() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        let revision = review_ready_session(&store, session_id);
        let plan_id = MeetingPlanId::new();
        let track_id = SourceTrackId::new();
        {
            let connection = store.connection().expect("store connection");
            connection
                .execute(
                    "INSERT INTO meeting_run_plans (
                        plan_id, session_id, attempt_number, schema_version, consent_id,
                        canonical_plan_json, created_at_utc_ms
                     ) VALUES (?1, ?2, 1, 1, ?3, '{}', 1)",
                    params![id(plan_id), id(session_id), id(ConsentId::new())],
                )
                .expect("insert transcript plan");
            connection
                .execute(
                    r#"INSERT INTO meeting_source_tracks (
                        track_id, session_id, plan_id, source_kind, required, requested,
                        descriptor_json, timestamp_bridge_json, health
                     ) VALUES (?1, ?2, ?3, 'microphone', 1, 1, '{}', '{}', '"healthy"')"#,
                    params![id(track_id), id(session_id), id(plan_id)],
                )
                .expect("insert transcript track");
        }
        list_transcript(&store, session_id, &["one", "two"]);
        let source = store
            .analytics_segments(session_id)
            .expect("stored transcript")
            .first()
            .expect("transcript segment")
            .speaker_id;
        let target = SpeakerId::new();
        {
            let connection = store.connection().expect("store connection");
            connection
                .execute(
                    "INSERT INTO meeting_speakers (
                        speaker_id, session_id, source_kind, display_name, revision
                     ) VALUES (?1, ?2, 'microphone', 'Canonical speaker', 0)",
                    params![id(target), id(session_id)],
                )
                .expect("insert canonical speaker");
        }
        store
            .merge_speaker(
                MeetingOperationId::new(),
                3,
                session_id,
                revision,
                source,
                target,
            )
            .expect("merge transcript speaker");

        let segments = store
            .analytics_segments(session_id)
            .expect("canonical transcript");
        assert!(!segments.is_empty());
        assert!(segments.iter().all(|segment| segment.speaker_id == target));
        let names = store
            .speaker_display_names(session_id)
            .expect("canonical speaker names");
        assert_eq!(
            names.get(&target).map(String::as_str),
            Some("Canonical speaker")
        );
        assert!(!names.contains_key(&source));

        let transcript = segments
            .iter()
            .map(|segment| MeetingEvidence {
                citation: MeetingCitation {
                    kind: CitationKind::Transcript,
                    session_id,
                    entity_id: segment.segment_id.uuid().to_string(),
                    start_offset_ns: Some(segment.start_offset_ns),
                    end_offset_ns: Some(segment.end_offset_ns),
                },
                text: segment.text.clone(),
            })
            .collect();
        let evidence = ArtifactEvidence {
            transcript,
            manual_notes: Vec::new(),
            user_notes: String::new(),
            template: MeetingNotesTemplate::General,
        };
        let captured = Arc::new(Mutex::new(None));
        let generator = CapturingLedgerGenerator {
            citation: segments[0].segment_id.uuid().to_string(),
            evidence: Arc::clone(&captured),
        };
        let ledger = crate::meeting::processing::generate_ledger(
            &generator, &evidence, &segments, &names, session_id,
        )
        .expect("regenerated ledger");
        let prompt = captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("captured ledger prompt");
        assert!(prompt.contains("Canonical speaker"));
        assert!(prompt.contains(segments[0].segment_id.uuid().to_string().as_str()));
        assert_eq!(
            ledger.threads[0].receipt.speaker.as_deref(),
            Some("Canonical speaker")
        );
    }
}
