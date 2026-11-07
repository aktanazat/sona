//! The counts Home's Today strip leads with, read in one round trip.
//!
//! Each count is read on its own and answers on its own: a source that could
//! not be read is `Unavailable`, never a zero, and one failing leaves the
//! others standing.

use super::loops::ledger_rows_in;
use super::workflows::workflow_succeeded_for_session_in;
use super::{status_predicate, MeetingStore, StoreError};
use crate::meeting::automations::{due_day_for, local_day};
use crate::meeting::loop_types::MeetingLoopKind;
use crate::meeting::people_types::PersonId;
use crate::meeting::types::{MeetingSessionId, MeetingStatusFilter};
use crate::meeting::workflow_types::WorkflowId;
use chrono::NaiveDate;
use serde::Serialize;
use specta::Type;
use uuid::Uuid;

/// One count, or the fact that its source could not be read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TodayCount<T> {
    Available { value: T },
    Unavailable,
}

impl<T> TodayCount<T> {
    /// The value a read returned, or `Unavailable` when it failed. The one
    /// place a failed read becomes a state, so none of them becomes a zero.
    pub fn read<E: std::fmt::Debug>(source: &str, result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Self::Available { value },
            Err(error) => {
                log::warn!("home_today could not read {source}: {error:?}");
                Self::Unavailable
            }
        }
    }
}

/// Somebody on today's calendar the address book already knows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct TodayPerson {
    pub person_id: PersonId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct TodayMeetings {
    pub count: u32,
    /// Up to four confirmed people from today's events, in calendar order.
    pub people: Vec<TodayPerson>,
}

/// How many of something finished meetings hold, and the newest meeting
/// holding one, which is where the count opens.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Type)]
pub struct TodayTally {
    pub count: u32,
    pub meeting_id: Option<MeetingSessionId>,
}

/// The open loops the user owes, as Home's needs-you list reads them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Type)]
pub struct TodayOwed {
    pub count: u32,
    /// When the oldest of them was first raised.
    pub oldest_at_utc_ms: Option<i64>,
    /// How many are past a day their meeting's notes named.
    pub late: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct HomeToday {
    /// Absent when no calendar is connected.
    pub meetings: Option<TodayCount<TodayMeetings>>,
    pub processing: TodayCount<u32>,
    pub unknown_speakers: TodayCount<TodayTally>,
    pub untracked: TodayCount<TodayTally>,
    pub owed: TodayCount<TodayOwed>,
}

/// The counts the meeting store answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodayStoreCounts {
    pub processing: TodayCount<u32>,
    pub unknown_speakers: TodayCount<TodayTally>,
    pub owed: TodayCount<TodayOwed>,
}

impl TodayStoreCounts {
    pub const UNAVAILABLE: Self = Self {
        processing: TodayCount::Unavailable,
        unknown_speakers: TodayCount::Unavailable,
        owed: TodayCount::Unavailable,
    };
}

impl MeetingStore {
    pub(crate) fn today_counts(
        &self,
        today: NaiveDate,
        user_name: Option<&str>,
    ) -> TodayStoreCounts {
        TodayStoreCounts {
            processing: TodayCount::read("processing", self.processing_count()),
            unknown_speakers: TodayCount::read(
                "unknown speakers",
                self.unknown_speakers(user_name),
            ),
            owed: TodayCount::read("owed", self.owed(today)),
        }
    }

    /// The meetings the list's processing filter shows.
    fn processing_count(&self) -> Result<u32, StoreError> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            &format!(
                "SELECT COUNT(*) FROM meeting_sessions m
                  WHERE m.phase != 'deleting' AND ({})",
                status_predicate(MeetingStatusFilter::Processing)
            ),
            [],
            |row| row.get(0),
        )?;
        u32::try_from(count).map_err(|_| StoreError::Corrupt)
    }

    /// Speakers no person, "unknown" answer or the user's own name covers
    /// yet, across finished meetings.
    fn unknown_speakers(&self, user_name: Option<&str>) -> Result<TodayTally, StoreError> {
        let finished = finished_meeting_ids(&*self.connection()?)?;
        let mut tally = TodayTally::default();
        for session_id in finished {
            let unresolved = self.unresolved_active_voice_speaker_ids(session_id, user_name)?;
            if unresolved.is_empty() {
                continue;
            }
            tally.count = tally
                .count
                .saturating_add(u32::try_from(unresolved.len()).unwrap_or(u32::MAX));
            tally.meeting_id.get_or_insert(session_id);
        }
        Ok(tally)
    }

    /// The open loops the needs-you list holds as the user's own: open loop
    /// rows, theirs, from meetings whose continuity pass finished.
    fn owed(&self, today: NaiveDate) -> Result<TodayOwed, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let mut owed = TodayOwed::default();
        for meeting in ledger_rows_in(&transaction)? {
            let mine = meeting
                .rows
                .iter()
                .filter(|row| row.kind == MeetingLoopKind::Loop && row.is_open() && row.is_mine())
                .collect::<Vec<_>>();
            if mine.is_empty()
                || !workflow_succeeded_for_session_in(
                    &transaction,
                    WorkflowId::Continuity,
                    meeting.session_id,
                )?
            {
                continue;
            }
            let meeting_day = local_day(meeting.at_utc_ms);
            for row in mine {
                owed.count = owed.count.saturating_add(1);
                let since = row.outstanding_since(meeting.at_utc_ms);
                owed.oldest_at_utc_ms =
                    Some(owed.oldest_at_utc_ms.map_or(since, |at| at.min(since)));
                if due_day_for(row, &meeting.action_items, meeting_day)
                    .is_some_and(|due| due < today)
                {
                    owed.late = owed.late.saturating_add(1);
                }
            }
        }
        transaction.commit()?;
        Ok(owed)
    }
}

/// Every meeting whose review is ready, newest first.
pub(crate) fn finished_meeting_ids(
    connection: &rusqlite::Connection,
) -> Result<Vec<MeetingSessionId>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id FROM meeting_sessions
          WHERE phase = 'review_ready'
          ORDER BY COALESCE(started_at_utc_ms, created_at_utc_ms) DESC, id DESC",
    )?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|row| -> Result<MeetingSessionId, StoreError> {
            Uuid::parse_str(&row?)
                .map(MeetingSessionId::from_uuid)
                .map_err(|_| StoreError::Corrupt)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::store::id;
    use crate::meeting::store::workflow_core_tests::{reviewable_meeting, store};
    use rusqlite::params;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 4).unwrap()
    }

    /* A source that cannot be read says so instead of reading as an empty
     * one: the strip hides a zero, so a failure shown as zero would be a
     * count that silently vanished. The others still answer. */
    #[test]
    fn a_failed_source_is_unavailable_and_an_empty_one_is_zero() {
        let (_directory, store) = store();
        reviewable_meeting(&store, "Finished", 1_000);
        let running = reviewable_meeting(&store, "Still processing", 2_000);
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE meeting_sessions
                    SET phase = 'processing', processing_status = '{\"kind\":\"running\"}'
                  WHERE id = ?1",
                params![id(running)],
            )
            .unwrap();

        assert_eq!(
            store.today_counts(today(), None),
            TodayStoreCounts {
                processing: TodayCount::Available { value: 1 },
                unknown_speakers: TodayCount::Available {
                    value: TodayTally::default()
                },
                owed: TodayCount::Available {
                    value: TodayOwed::default()
                },
            }
        );

        store
            .connection()
            .unwrap()
            .execute_batch("DROP TABLE meeting_voice_speaker_resolutions")
            .unwrap();

        assert_eq!(
            store.today_counts(today(), None),
            TodayStoreCounts {
                processing: TodayCount::Available { value: 1 },
                unknown_speakers: TodayCount::Unavailable,
                owed: TodayCount::Available {
                    value: TodayOwed::default()
                },
            }
        );
    }
}
