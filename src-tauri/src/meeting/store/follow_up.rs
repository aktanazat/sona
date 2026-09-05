//! D26: the receipt half of a follow-up draft.
//!
//! Drafting writes nothing to the meeting, so this records the event rather
//! than a change: the session it was drafted for, when, and which engine wrote
//! it. That is the difference between an agent that can say "I drafted a
//! follow-up for the pricing call on this engine" and one that can only claim
//! it — the same reason every mutation in this store returns a receipt.
//!
//! Idempotency is the operation id's, as everywhere else: pressing the button
//! twice with the same id returns the first receipt and does not record a
//! second event.

use super::{
    committed_receipt, insert_operation_receipt, session_row, utc_now_ms, MeetingStore, StoreError,
    StoreMutation,
};
use crate::meeting::types::{
    MeetingCommandKind, MeetingOperationId, MeetingPhase, MeetingSessionId, OperationActor,
    OperationReceipt,
};
use rusqlite::TransactionBehavior;

impl MeetingStore {
    /// Record that a follow-up was drafted for this meeting.
    ///
    /// `engine` names what wrote it — a `MeetingTextGenerator::model_id`, or
    /// the fallback — and lands in the receipt's `effect_ids`, which is where
    /// a reader looks to find out whether a given draft was written on this
    /// Mac or on a server.
    pub(crate) fn record_follow_up_draft(
        &self,
        operation_id: MeetingOperationId,
        session_id: MeetingSessionId,
        engine: &str,
    ) -> Result<OperationReceipt, StoreError> {
        if let Some(receipt) = self.operation_receipt(operation_id)? {
            return Ok(receipt);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = session_row(&transaction, session_id)?;
        if session.phase == MeetingPhase::Deleting {
            return Err(StoreError::NotFound);
        }
        let now = utc_now_ms();
        // The revision is the one the draft was read at, unchanged: a draft is
        // a reading of the record, so claiming a new revision would say the
        // meeting moved when it did not.
        let receipt = committed_receipt(
            StoreMutation {
                operation_id,
                actor: OperationActor::User,
                requested_at_utc_ms: now,
                session_id,
                expected_revision: session.revision,
                command: MeetingCommandKind::FollowUpDraft,
            },
            session.phase,
            session.phase,
            now,
            session.revision,
            vec![engine.to_string()],
        );
        insert_operation_receipt(&transaction, &receipt, now)?;
        transaction.commit()?;
        Ok(receipt)
    }
}
