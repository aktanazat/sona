use super::*;

const COOLDOWN_MS: i64 = 7 * 86_400_000;
const SWEEP_LIMIT: usize = 100;

pub(crate) struct TranscriptPurge {
    pub people_revision: Option<u64>,
}

impl MeetingStore {
    pub(crate) fn transcript_retention_policy(
        &self,
    ) -> Result<MeetingTranscriptRetentionSnapshot, StoreError> {
        let connection = self.connection()?;
        transcript_retention_policy_in(&connection)
    }

    pub(crate) fn set_transcript_retention_policy(
        &self,
        operation_id: MeetingOperationId,
        now_utc_ms: i64,
        expected_revision: u64,
        policy: &MeetingRetentionPolicy,
    ) -> Result<OperationReceipt, StoreError> {
        if let MeetingRetentionPolicy::DeleteAfterDays { days } = policy {
            if ![1, 7, 30, 90, 180, 365].contains(days) {
                return Err(StoreError::Invalid);
            }
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = operation_receipt_in(&transaction, operation_id)? {
            return Ok(receipt);
        }
        let current = transcript_retention_policy_in(&transaction)?;
        if current.revision != expected_revision {
            let receipt = rejected_global_receipt(
                operation_id, MeetingCommandKind::TranscriptRetentionSet,
                expected_revision, current.revision, now_utc_ms, MeetingReasonCode::StaleRevision,
            );
            insert_operation_receipt(&transaction, &receipt, now_utc_ms)?;
            transaction.commit()?;
            return Ok(receipt);
        }
        let changed_at = if shortens_retention(&current.policy, policy) {
            now_utc_ms
        } else {
            current.changed_at_utc_ms
        };
        let next = current.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
        transaction.execute(
            "UPDATE meeting_transcript_retention_policy
             SET policy_json = ?1, revision = ?2, changed_at_utc_ms = ?3 WHERE singleton = 1",
            params![encode_json(policy)?, to_i64(next)?, changed_at],
        )?;
        let receipt = committed_global_receipt(
            operation_id, MeetingCommandKind::TranscriptRetentionSet,
            expected_revision, now_utc_ms, now_utc_ms, next,
        );
        insert_operation_receipt(&transaction, &receipt, now_utc_ms)?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub(crate) fn due_transcript_retention_sessions(
        &self,
        now_utc_ms: i64,
    ) -> Result<Vec<MeetingSessionId>, StoreError> {
        let connection = self.connection()?;
        let policy = transcript_retention_policy_in(&connection)?;
        let cutoff = transcript_cutoff(&policy, now_utc_ms);
        let mut statement = connection.prepare(
            "SELECT id FROM meeting_sessions
             WHERE phase IN ('review_ready', 'recovery_required')
               AND (transcript_audio_purge_pending = 1
                    OR (transcript_purged_at_utc_ms IS NULL AND ended_at_utc_ms <= ?1))
             ORDER BY transcript_audio_purge_pending DESC, ended_at_utc_ms, id LIMIT ?2",
        )?;
        let rows = statement.query_map(params![cutoff, SWEEP_LIMIT as i64], |row| {
            let value: String = row.get(0)?;
            parse_uuid(&value).map(MeetingSessionId::from_uuid).map_err(to_sql_error)
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// The marker is irreversible. Its SQL trigger removes text in this same
    /// transaction; audio removal is resumable after commit and across launches.
    pub(crate) fn purge_transcript_at(
        &self,
        session_id: MeetingSessionId,
        now_utc_ms: i64,
    ) -> Result<Option<TranscriptPurge>, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = session_row(&transaction, session_id)?;
        if !matches!(current.phase, MeetingPhase::ReviewReady | MeetingPhase::RecoveryRequired) {
            return Ok(None);
        }
        let (ended_at, pending): (Option<i64>, bool) = transaction.query_row(
            "SELECT ended_at_utc_ms, transcript_audio_purge_pending FROM meeting_sessions WHERE id = ?1",
            params![id(session_id)], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut people_revision = None;
        if current.transcript_purged_at_utc_ms.is_none() {
            // Recheck under the write lock: the operator may have changed the
            // policy after this meeting was selected by the sweep.
            let policy = transcript_retention_policy_in(&transaction)?;
            if !ended_at.zip(transcript_cutoff(&policy, now_utc_ms))
                .is_some_and(|(ended, cutoff)| ended <= cutoff)
            {
                return Ok(None);
            }
            let cached: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM meeting_conversation_metrics WHERE session_id = ?1)",
                params![id(session_id)], |row| row.get(0),
            )?;
            if !cached {
                let segments = analytics_segments_in(&transaction, session_id)?;
                let metrics = MeetingAnalytics {
                    talk: super::super::analytics::talk_metrics(&segments),
                    trackers: Vec::new(),
                };
                transaction.execute(
                    "INSERT INTO meeting_conversation_metrics
                     (session_id, input_revision, metrics_json, computed_at_utc_ms) VALUES (?1, ?2, ?3, ?4)",
                    params![id(session_id), to_i64(current.revision)?, encode_json(&metrics)?, now_utc_ms],
                )?;
            }
            let voice_change = voice_identity::purge_session_voice_evidence_in(&transaction, session_id)?;
            if voice_change.people_changed() {
                people_revision = Some(people::bump_people_revision_in(&transaction)?);
            }
            transaction.execute(
                "DELETE FROM meeting_snapshots WHERE session_id = ?1",
                params![id(session_id)],
            )?;
            let next = current.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
            transaction.execute(
                "UPDATE meeting_sessions SET transcript_purged_at_utc_ms = ?1,
                    transcript_audio_purge_pending = 1, revision = ?2 WHERE id = ?3",
                params![now_utc_ms, to_i64(next)?, id(session_id)],
            )?;
            append_event(&transaction, session_id, next, current.phase, current.phase,
                "transcript_purged", None)?;
        } else if !pending {
            return Ok(None);
        }
        transaction.commit()?;
        let paths = {
            let mut statement = connection.prepare(
                "SELECT relative_path, directory FROM meeting_transcript_purge_paths WHERE session_id = ?1",
            )?;
            let rows = statement.query_map(params![id(session_id)], |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        drop(connection);
        let path = validated_relative(&self.root, &id(session_id))?;
        match fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        for (relative, directory) in paths {
            let path = validated_relative(&self.root, &relative)?;
            let result = if directory { fs::remove_dir_all(path) } else { fs::remove_file(path) };
            match result {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        // Persist the replacement intent even if sync is paused or its event
        // listener has not started. Failure leaves cleanup pending for retry.
        if self.cloud_state()?.is_some() {
            crate::cloud_sync::queue_session_upload(self, session_id).map_err(|_| StoreError::Unavailable)?;
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM meeting_transcript_purge_paths WHERE session_id = ?1", params![id(session_id)])?;
        transaction.execute(
            "UPDATE meeting_sessions SET transcript_audio_purge_pending = 0 WHERE id = ?1",
            params![id(session_id)],
        )?;
        transaction.commit()?;
        Ok(Some(TranscriptPurge { people_revision }))
    }

    pub(crate) fn require_retained_transcript(&self, session_id: MeetingSessionId) -> Result<(), StoreError> {
        let connection = self.connection()?;
        require_retained_transcript_in(&connection, session_id)
    }

    /// Purged meetings keep their last measured talk times, not a new empty reading.
    pub(crate) fn retained_conversation_metrics(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Option<MeetingAnalytics>, StoreError> {
        let connection = self.connection()?;
        if session_row(&connection, session_id)?.transcript_purged_at_utc_ms.is_none() {
            return Ok(None);
        }
        let json: Option<String> = connection.query_row(
            "SELECT metrics_json FROM meeting_conversation_metrics WHERE session_id = ?1",
            params![id(session_id)], |row| row.get(0),
        ).optional()?;
        json.map(|json| decode_json(&json)).transpose()?.map(Some).ok_or(StoreError::TranscriptDeleted)
    }

    pub(super) fn require_retained_cloud_recording(&self, object_id: &str) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let purged: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM meeting_cloud_heads h JOIN meeting_sessions m ON m.id = h.source_session_id
             WHERE h.object_id = ?1 AND m.transcript_purged_at_utc_ms IS NOT NULL)",
            params![object_id], |row| row.get(0),
        )?;
        if purged { return Err(StoreError::TranscriptDeleted) }
        Ok(())
    }
}

pub(super) fn require_retained_transcript_in(connection: &Connection, session_id: MeetingSessionId) -> Result<(), StoreError> {
    if session_row(connection, session_id)?.transcript_purged_at_utc_ms.is_some() {
        return Err(StoreError::TranscriptDeleted);
    }
    Ok(())
}

fn transcript_retention_policy_in(connection: &Connection) -> Result<MeetingTranscriptRetentionSnapshot, StoreError> {
    let (policy, revision, changed_at): (String, i64, i64) = connection.query_row(
        "SELECT policy_json, revision, changed_at_utc_ms FROM meeting_transcript_retention_policy WHERE singleton = 1",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let policy = decode_json(&policy)?;
    let deletion_begins_at_utc_ms = match policy {
        MeetingRetentionPolicy::Forever => None,
        MeetingRetentionPolicy::DeleteAfterDays { .. } => Some(changed_at.saturating_add(COOLDOWN_MS)),
    };
    Ok(MeetingTranscriptRetentionSnapshot {
        policy, revision: from_i64(revision)?, changed_at_utc_ms: changed_at, deletion_begins_at_utc_ms,
    })
}

fn transcript_cutoff(policy: &MeetingTranscriptRetentionSnapshot, now: i64) -> Option<i64> {
    if now < policy.deletion_begins_at_utc_ms? {
        return None;
    }
    match policy.policy {
        MeetingRetentionPolicy::Forever => None,
        MeetingRetentionPolicy::DeleteAfterDays { days } => now.checked_sub(i64::from(days) * 86_400_000),
    }
}

fn shortens_retention(before: &MeetingRetentionPolicy, after: &MeetingRetentionPolicy) -> bool {
    match (before, after) {
        (MeetingRetentionPolicy::Forever, MeetingRetentionPolicy::DeleteAfterDays { .. }) => true,
        (MeetingRetentionPolicy::DeleteAfterDays { days: old }, MeetingRetentionPolicy::DeleteAfterDays { days: new }) => new < old,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
