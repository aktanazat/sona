use super::audio_retention::meetings_purging_by_in;
use super::*;
use specta::Type;

const WEEK_MS: i64 = 7 * 86_400_000;

/// What this store holds, counted from rows and file metadata. Nothing here
/// decrypts audio, reads transcript text or loads an image.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct MeetingHoldings {
    pub recordings: RecordingHoldings,
    pub transcripts: u64,
    pub voiceprints: VoiceprintHoldings,
    pub portraits: PortraitHoldings,
    /// People with contact details attached from the address book.
    pub contact_cards: u64,
    /// Meetings whose transcript or audio a retention sweep removes within
    /// the next seven days under the current policies.
    pub purging_within_week: u64,
    pub last_cloud_upload_at_utc_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct RecordingHoldings {
    pub meetings: u64,
    /// The longest track of each meeting, so a microphone and a system track
    /// recorded side by side count once.
    pub duration_ms: u64,
    /// Encrypted track files on disk, measured.
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct VoiceprintHoldings {
    pub people: u64,
    pub sample_ms: u64,
    /// Meeting speakers whose voice is kept to suggest who they are.
    pub speakers: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct PortraitHoldings {
    pub count: u64,
    pub bytes: u64,
}

impl MeetingStore {
    pub(crate) fn holdings(&self, now_utc_ms: i64) -> Result<MeetingHoldings, StoreError> {
        let rows = {
            let mut connection = self.connection()?;
            let transaction = connection.transaction()?;
            holding_rows_in(&transaction, now_utc_ms)?
        };
        Ok(MeetingHoldings {
            recordings: RecordingHoldings {
                meetings: rows.recorded_meetings,
                duration_ms: rows.recorded_ms,
                bytes: track_bytes(&self.root)?,
            },
            transcripts: rows.transcripts,
            voiceprints: rows.voiceprints,
            portraits: rows.portraits,
            contact_cards: rows.contact_cards,
            purging_within_week: rows.purging_within_week,
            last_cloud_upload_at_utc_ms: rows.last_cloud_upload_at_utc_ms,
        })
    }
}

struct HoldingRows {
    recorded_meetings: u64,
    recorded_ms: u64,
    transcripts: u64,
    voiceprints: VoiceprintHoldings,
    portraits: PortraitHoldings,
    contact_cards: u64,
    purging_within_week: u64,
    last_cloud_upload_at_utc_ms: Option<i64>,
}

fn holding_rows_in(connection: &Connection, now_utc_ms: i64) -> Result<HoldingRows, StoreError> {
    let (recorded_meetings, recorded_ns): (i64, i64) = connection.query_row(
        "SELECT COUNT(*), COALESCE(SUM(longest_ns), 0) FROM (
             SELECT MAX(track_ns) AS longest_ns FROM (
                 SELECT t.session_id, SUM(r.duration_ns) AS track_ns
                   FROM meeting_source_tracks t
                   JOIN meeting_track_records r ON r.track_id = t.track_id
                   JOIN meeting_sessions s ON s.id = t.session_id
                  WHERE s.phase != 'deleting' AND s.transcript_purged_at_utc_ms IS NULL
                    AND s.audio_purged_at_utc_ms IS NULL
                  GROUP BY t.track_id
             ) GROUP BY session_id
         )",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let transcripts: i64 = connection.query_row(
        "SELECT COUNT(*) FROM meeting_sessions
          WHERE phase != 'deleting' AND transcript_purged_at_utc_ms IS NULL
            AND current_transcript_revision_id IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    let (people, sample_ns, speakers): (i64, i64, i64) = connection.query_row(
        "SELECT (SELECT COUNT(*) FROM voice_profiles),
                COALESCE((SELECT SUM(end_offset_ns - start_offset_ns) FROM voice_profile_samples), 0),
                (SELECT COUNT(*) FROM meeting_speaker_voiceprints)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let (portraits, portrait_bytes, contact_cards): (i64, i64, i64) = connection.query_row(
        "SELECT COUNT(*), COALESCE(SUM(length(png) + COALESCE(length(original), 0)), 0),
                (SELECT COUNT(*) FROM person_contact_cards)
           FROM person_avatars",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let last_cloud_upload_at_utc_ms: Option<i64> = connection.query_row(
        "SELECT MAX(updated_at_utc_ms) FROM meeting_cloud_outbox
          WHERE kind = 'object' AND state = 'completed'",
        [],
        |row| row.get(0),
    )?;
    Ok(HoldingRows {
        recorded_meetings: from_i64(recorded_meetings)?,
        recorded_ms: from_i64(recorded_ns)? / 1_000_000,
        transcripts: from_i64(transcripts)?,
        voiceprints: VoiceprintHoldings {
            people: from_i64(people)?,
            sample_ms: from_i64(sample_ns)? / 1_000_000,
            speakers: from_i64(speakers)?,
        },
        portraits: PortraitHoldings {
            count: from_i64(portraits)?,
            bytes: from_i64(portrait_bytes)?,
        },
        contact_cards: from_i64(contact_cards)?,
        purging_within_week: meetings_purging_by_in(
            connection,
            now_utc_ms.saturating_add(WEEK_MS),
        )?,
        last_cloud_upload_at_utc_ms,
    })
}

/// The sizes of every session's `tracks` directory, including sessions whose
/// rows are mid-deletion: this is what the disk holds, not what the list shows.
fn track_bytes(root: &Path) -> Result<u64, StoreError> {
    let mut total = 0_u64;
    for session in fs::read_dir(root)? {
        let session = session?;
        if session.file_name().to_string_lossy().starts_with('.') || !session.file_type()?.is_dir()
        {
            continue;
        }
        let tracks = match fs::read_dir(session.path().join("tracks")) {
            Ok(tracks) => tracks,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for file in tracks {
            let metadata = file?.metadata()?;
            if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests;
