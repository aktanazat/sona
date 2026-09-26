use super::{MeetingStore, StoreError};
use crate::meeting::prep::MeetingBrief;
use rusqlite::{params, OptionalExtension};

impl MeetingStore {
    pub(crate) fn cached_brief(&self, event_key: &str, input_key: &str, now: i64) -> Result<Option<MeetingBrief>, StoreError> {
        let json: Option<String> = self.connection()?.query_row(
            "SELECT brief_json FROM meeting_briefs WHERE event_key = ?1 AND input_key = ?2 AND expires_at_utc_ms > ?3 AND generated_at_utc_ms >= ?4",
            params![event_key, input_key, now, now.saturating_sub(6 * 60 * 60_000)],
            |row| row.get(0),
        ).optional()?;
        json.map(|value| serde_json::from_str(&value).map_err(|_| StoreError::Corrupt)).transpose()
    }

    pub(crate) fn keep_brief(&self, input_key: &str, expires_at: i64, brief: &MeetingBrief) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM meeting_briefs WHERE expires_at_utc_ms <= ?1", [brief.generated_at_utc_ms])?;
        transaction.execute(
            "INSERT INTO meeting_briefs (event_key, input_key, generated_at_utc_ms, expires_at_utc_ms, brief_json) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(event_key) DO UPDATE SET input_key = excluded.input_key, generated_at_utc_ms = excluded.generated_at_utc_ms, expires_at_utc_ms = excluded.expires_at_utc_ms, brief_json = excluded.brief_json",
            params![brief.event_key, input_key, brief.generated_at_utc_ms, expires_at, serde_json::to_string(brief).map_err(|_| StoreError::Invalid)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn clear_briefs(&self) -> Result<(), StoreError> {
        self.connection()?.execute("DELETE FROM meeting_briefs", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::{mail_context::MailContext, prep::BriefContent};

    #[test]
    fn calendar_or_privacy_changes_do_not_reuse_an_old_brief() {
        let (_directory, store) = crate::meeting::store::workflow_core_tests::store();
        let brief = MeetingBrief {
            event_key: "event#1000".into(), title: "Review".into(), start_utc_ms: 1000,
            generated_at_utc_ms: 100, content: BriefContent::default(), sources: Vec::new(),
            mail: MailContext::off(), status: "No extra context".into(), web_status: String::new(),
        };
        store.keep_brief("old-input", 1000, &brief).unwrap();
        assert_eq!(store.cached_brief(&brief.event_key, "old-input", 200).unwrap(), Some(brief.clone()));
        assert_eq!(store.cached_brief(&brief.event_key, "changed-input", 200).unwrap(), None);
        assert_eq!(store.cached_brief(&brief.event_key, "old-input", 1000).unwrap(), None);
        store.clear_briefs().unwrap();
        assert_eq!(store.cached_brief(&brief.event_key, "old-input", 200).unwrap(), None);
    }
}
