use super::{MeetingStore, StoreError};
use crate::integrations::types::{AutoSendRule, Connection, ConnectionPreferences, SendReceipt};
use rusqlite::{params, OptionalExtension, TransactionBehavior};

impl MeetingStore {
    pub(crate) fn connections(&self) -> Result<Vec<Connection>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT record_json FROM integration_connections ORDER BY id")?;
        let records = statement.query_map([], |row| row.get::<_, String>(0))?;
        records.map(|row| serde_json::from_str(&row?).map_err(|_| StoreError::Corrupt)).collect()
    }

    pub(crate) fn save_connection(&self, record: &Connection) -> Result<(), StoreError> {
        self.connection()?.execute(
            "INSERT INTO integration_connections (id, record_json) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET record_json = excluded.record_json",
            params![record.id, serde_json::to_string(record).map_err(|_| StoreError::Invalid)?],
        )?;
        Ok(())
    }

    pub(crate) fn disconnect_connection(&self, id: &str) -> Result<(), StoreError> {
        self.connection()?.execute("DELETE FROM integration_connections WHERE id = ?1", [id])?;
        Ok(())
    }

    pub(crate) fn connection_preferences(&self) -> Result<ConnectionPreferences, StoreError> {
        let json: Option<String> = self.connection()?.query_row(
            "SELECT record_json FROM integration_preferences WHERE singleton = 1", [], |row| row.get(0),
        ).optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(|_| StoreError::Corrupt)).transpose()
            .map(Option::unwrap_or_default)
    }

    pub(crate) fn save_connection_preferences(&self, preferences: &ConnectionPreferences) -> Result<(), StoreError> {
        self.connection()?.execute(
            "INSERT INTO integration_preferences (singleton, record_json) VALUES (1, ?1)
             ON CONFLICT(singleton) DO UPDATE SET record_json = excluded.record_json",
            [serde_json::to_string(preferences).map_err(|_| StoreError::Invalid)?],
        )?;
        Ok(())
    }

    pub(crate) fn connection_rules(&self) -> Result<Vec<AutoSendRule>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT record_json FROM integration_rules ORDER BY id")?;
        let records = statement.query_map([], |row| row.get::<_, String>(0))?;
        records.map(|row| serde_json::from_str(&row?).map_err(|_| StoreError::Corrupt)).collect()
    }

    /// Stamp new consent inside the write transaction, like folder membership commits.
    /// A caller-supplied timestamp must never authorize exporting earlier content.
    pub(crate) fn save_connection_rule(&self, rule: &mut AutoSendRule) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<String> = transaction.query_row(
            "SELECT record_json FROM integration_rules WHERE id = ?1", [&rule.id], |row| row.get(0),
        ).optional()?;
        let previous: Option<AutoSendRule> = previous.map(|json| serde_json::from_str(&json)
            .map_err(|_| StoreError::Corrupt)).transpose()?;
        // Changing a destination or scope is a new grant, never a backfill.
        rule.created_at_utc_ms = previous.filter(|old| old.enabled && rule.enabled
            && old.connection_id == rule.connection_id && old.scope == rule.scope)
            .map_or_else(super::utc_now_ms, |old| old.created_at_utc_ms);
        transaction.execute(
            "INSERT INTO integration_rules (id, connection_id, record_json) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET connection_id = excluded.connection_id, record_json = excluded.record_json",
            params![rule.id, rule.connection_id, serde_json::to_string(&*rule).map_err(|_| StoreError::Invalid)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn delete_connection_rule(&self, id: &str) -> Result<(), StoreError> {
        self.connection()?.execute("DELETE FROM integration_rules WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Claim before egress. A duplicate request returns its original receipt even
    /// when the app exited during a send; an unknown outcome is never retried.
    pub(crate) fn claim_connection_send(&self, receipt: &SendReceipt, dedup: Option<&str>) -> Result<Option<SendReceipt>, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let existing: Option<String> = transaction.query_row(
            "SELECT record_json FROM integration_receipts WHERE id = ?1 OR dedup_key = ?2 LIMIT 1",
            params![receipt.id, dedup], |row| row.get(0),
        ).optional()?;
        if let Some(json) = existing {
            return serde_json::from_str(&json).map(Some).map_err(|_| StoreError::Corrupt);
        }
        transaction.execute(
            "INSERT INTO integration_receipts (id, connection_id, dedup_key, created_at_utc_ms, record_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![receipt.id, receipt.connection_id, dedup, receipt.created_at_utc_ms,
                serde_json::to_string(receipt).map_err(|_| StoreError::Invalid)?],
        )?;
        transaction.commit()?;
        Ok(None)
    }

    pub(crate) fn finish_connection_send(&self, receipt: &SendReceipt) -> Result<(), StoreError> {
        let count = self.connection()?.execute(
            "UPDATE integration_receipts SET record_json = ?1 WHERE id = ?2",
            params![serde_json::to_string(receipt).map_err(|_| StoreError::Invalid)?, receipt.id],
        )?;
        if count == 1 { Ok(()) } else { Err(StoreError::NotFound) }
    }

    pub(crate) fn connection_receipt(&self, id: &str) -> Result<SendReceipt, StoreError> {
        let json: String = self.connection()?.query_row(
            "SELECT record_json FROM integration_receipts WHERE id = ?1", [id], |row| row.get(0),
        ).optional()?.ok_or(StoreError::NotFound)?;
        serde_json::from_str(&json).map_err(|_| StoreError::Corrupt)
    }

    pub(crate) fn connection_receipts(&self) -> Result<Vec<SendReceipt>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT record_json FROM integration_receipts ORDER BY created_at_utc_ms DESC LIMIT 50")?;
        let records = statement.query_map([], |row| row.get::<_, String>(0))?;
        records.map(|row| serde_json::from_str(&row?).map_err(|_| StoreError::Corrupt)).collect()
    }
}
