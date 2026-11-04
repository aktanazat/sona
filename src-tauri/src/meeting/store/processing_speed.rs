//! How long processing takes on this Mac, stage by stage, and what each
//! finished run cost.
//!
//! A speed sample belongs to the meeting it was measured on: deleting the
//! meeting takes its timing with it, and processing the same meeting again
//! replaces its samples rather than counting that audio twice.

use super::{from_i64, id, to_i64, MeetingStore, StoreError};
use crate::meeting::types::{
    MeetingArtifactId, MeetingSessionId, ProcessingReceipt, ProcessingRoute, ProcessingStage,
};
use rusqlite::{params, TransactionBehavior};
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// How many of a stage's newest runs its speed is read from.
const SPEED_WINDOW: i64 = 9;

/// One measured pace: this much audio took this much wall time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StageSpeed {
    pub(crate) audio_ns: u64,
    pub(crate) wall_ns: u64,
}

impl StageSpeed {
    /// The wall time this pace gives `audio_ns` of audio.
    pub(crate) fn wall_for(self, audio_ns: u64) -> u64 {
        let wall =
            u128::from(audio_ns) * u128::from(self.wall_ns) / u128::from(self.audio_ns.max(1));
        u64::try_from(wall).unwrap_or(u64::MAX)
    }

    fn pace_cmp(self, other: Self) -> Ordering {
        (u128::from(self.wall_ns) * u128::from(other.audio_ns))
            .cmp(&(u128::from(other.wall_ns) * u128::from(self.audio_ns)))
    }
}

/// How much one run has to get through.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ProcessingWork {
    /// The recording's length as the meeting list counts it, or the longest
    /// track when no capture window was closed.
    pub(crate) audio_ns: Option<u64>,
    /// Every track's span end to end, which is what transcription reads.
    pub(crate) track_ns: Option<u64>,
}

impl MeetingStore {
    pub(crate) fn processing_work(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<ProcessingWork, StoreError> {
        let connection = self.connection()?;
        let (recorded, longest, total): (Option<i64>, Option<i64>, Option<i64>) = connection
            .query_row(
                "SELECT (SELECT SUM(w.end_offset_ns - w.start_offset_ns)
                           FROM meeting_capture_windows w
                          WHERE w.session_id = ?1 AND w.end_offset_ns IS NOT NULL),
                        MAX(t.last_offset_ns - t.first_offset_ns),
                        SUM(t.last_offset_ns - t.first_offset_ns)
                   FROM meeting_source_tracks t
                  WHERE t.session_id = ?1
                    AND t.first_offset_ns IS NOT NULL
                    AND t.last_offset_ns IS NOT NULL",
                params![id(session_id)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        let positive = |value: Option<i64>| {
            value
                .and_then(|value| u64::try_from(value).ok())
                .filter(|value| *value > 0)
        };
        Ok(ProcessingWork {
            audio_ns: positive(recorded).or(positive(longest)),
            track_ns: positive(total),
        })
    }

    /// Keep the pace each finished stage of this meeting's run was measured
    /// at, replacing what an earlier run of the same meeting left.
    pub(crate) fn record_processing_speeds(
        &self,
        session_id: MeetingSessionId,
        samples: &[(ProcessingStage, StageSpeed)],
        measured_at_utc_ms: i64,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (stage, speed) in samples {
            transaction.execute(
                "INSERT INTO meeting_processing_speeds (
                    session_id, stage, audio_ns, wall_ns, measured_at_utc_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(session_id, stage) DO UPDATE SET
                    audio_ns = excluded.audio_ns,
                    wall_ns = excluded.wall_ns,
                    measured_at_utc_ms = excluded.measured_at_utc_ms",
                params![
                    id(session_id),
                    stage_text(*stage),
                    to_i64(speed.audio_ns)?,
                    to_i64(speed.wall_ns)?,
                    measured_at_utc_ms,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Each measured stage's median pace over its newest runs. A stage this
    /// Mac has never finished is absent, not zero.
    ///
    /// The median is one of the runs, not an average of two: on an even
    /// count it is the slower of the middle pair.
    pub(crate) fn processing_speeds(
        &self,
    ) -> Result<BTreeMap<ProcessingStage, StageSpeed>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT audio_ns, wall_ns FROM meeting_processing_speeds
              WHERE stage = ?1
              ORDER BY measured_at_utc_ms DESC
              LIMIT ?2",
        )?;
        let mut speeds = BTreeMap::new();
        for stage in ProcessingStage::ALL {
            let mut samples = statement
                .query_map(params![stage_text(stage), SPEED_WINDOW], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                })?
                .map(|row| {
                    let (audio_ns, wall_ns) = row?;
                    Ok(StageSpeed {
                        audio_ns: from_i64(audio_ns)?,
                        wall_ns: from_i64(wall_ns)?,
                    })
                })
                .collect::<Result<Vec<_>, StoreError>>()?;
            samples.sort_by(|left, right| left.pace_cmp(*right));
            if let Some(median) = samples.get(samples.len() / 2) {
                speeds.insert(stage, *median);
            }
        }
        Ok(speeds)
    }

    pub(crate) fn record_processing_receipt(
        &self,
        artifact_id: MeetingArtifactId,
        receipt: ProcessingReceipt,
    ) -> Result<(), StoreError> {
        self.connection()?.execute(
            "INSERT INTO meeting_processing_receipts (artifact_id, wall_ms, audio_ns, route)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(artifact_id) DO UPDATE SET
                wall_ms = excluded.wall_ms,
                audio_ns = excluded.audio_ns,
                route = excluded.route",
            params![
                id(artifact_id),
                to_i64(receipt.wall_ms)?,
                to_i64(receipt.audio_ns)?,
                route_text(receipt.route),
            ],
        )?;
        Ok(())
    }
}

/// A receipt read beside its artifact. The receipt's columns are all set or,
/// for an artifact no measured run wrote, all absent.
pub(super) fn receipt_from_columns(
    wall_ms: Option<i64>,
    audio_ns: Option<i64>,
    route: Option<String>,
) -> Result<Option<ProcessingReceipt>, StoreError> {
    match (wall_ms, audio_ns, route) {
        (None, None, None) => Ok(None),
        (Some(wall_ms), Some(audio_ns), Some(route)) => Ok(Some(ProcessingReceipt {
            wall_ms: from_i64(wall_ms)?,
            audio_ns: from_i64(audio_ns)?,
            route: route_from_text(&route)?,
        })),
        _ => Err(StoreError::Corrupt),
    }
}

fn stage_text(stage: ProcessingStage) -> &'static str {
    match stage {
        ProcessingStage::Audio => "audio",
        ProcessingStage::Speech => "speech",
        ProcessingStage::Speakers => "speakers",
        ProcessingStage::Notes => "notes",
        ProcessingStage::People => "people",
    }
}

fn route_text(route: ProcessingRoute) -> &'static str {
    match route {
        ProcessingRoute::Local => "local",
        ProcessingRoute::Relay => "relay",
    }
}

fn route_from_text(text: &str) -> Result<ProcessingRoute, StoreError> {
    match text {
        "local" => Ok(ProcessingRoute::Local),
        "relay" => Ok(ProcessingRoute::Relay),
        _ => Err(StoreError::Corrupt),
    }
}
