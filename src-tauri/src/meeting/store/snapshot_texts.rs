//! The words read in a meeting's pictures. A picture's words go when the
//! picture goes, and every picture's words go when the Settings switch turns
//! off. Only this app's own screens read them: the query plane, the chat
//! tools and the command line never do.

use super::{from_i64, id, meeting_fts_match_query, parse_uuid, MeetingStore, StoreError};
use crate::meeting::screen_text::ScreenText;
use crate::meeting::snapshots::{
    MeetingSnapshotId, MeetingSnapshotTextMark, MeetingSnapshotTextMoment,
};
use crate::meeting::types::{MeetingSessionId, TranscriptSegmentId};
use rusqlite::{params, Connection};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

const SEARCH_RESULTS_MAX: usize = 8;

struct Picture {
    snapshot_id: MeetingSnapshotId,
    offset_ns: u64,
    title: String,
}

struct Line {
    segment_id: TranscriptSegmentId,
    start_offset_ns: u64,
    end_offset_ns: u64,
}

/// One meeting's pictures in the order they were taken, and its transcript
/// lines in the order they were said.
struct Timeline {
    pictures: Vec<Picture>,
    lines: Vec<Line>,
}

impl Timeline {
    fn read_in(connection: &Connection, session_id: MeetingSessionId) -> Result<Self, StoreError> {
        let mut statement = connection.prepare(
            "SELECT s.snapshot_id, s.offset_ns, COALESCE(t.title, '')
               FROM meeting_snapshots s
               LEFT JOIN meeting_snapshot_texts t ON t.snapshot_id = s.snapshot_id
              WHERE s.session_id = ?1
              ORDER BY s.offset_ns, s.captured_at_utc_ms, s.snapshot_id",
        )?;
        let mut rows = statement.query(params![id(session_id)])?;
        let mut pictures = Vec::new();
        while let Some(row) = rows.next()? {
            pictures.push(Picture {
                snapshot_id: MeetingSnapshotId::from_uuid(parse_uuid(&row.get::<_, String>(0)?)?),
                offset_ns: from_i64(row.get(1)?)?,
                title: row.get(2)?,
            });
        }
        let mut statement = connection.prepare(
            "SELECT s.segment_id, s.start_offset_ns, s.end_offset_ns
               FROM meeting_sessions m
               JOIN meeting_transcript_segments s
                 ON s.transcript_revision_id = m.current_transcript_revision_id
              WHERE m.id = ?1
                AND COALESCE((SELECT e.removed FROM meeting_segment_edits e
                               WHERE e.segment_id = s.segment_id
                               ORDER BY e.edit_sequence DESC LIMIT 1), 0) = 0
              ORDER BY s.start_offset_ns, s.ordinal",
        )?;
        let mut rows = statement.query(params![id(session_id)])?;
        let mut lines = Vec::new();
        while let Some(row) = rows.next()? {
            lines.push(Line {
                segment_id: TranscriptSegmentId::from_uuid(parse_uuid(&row.get::<_, String>(0)?)?),
                start_offset_ns: from_i64(row.get(1)?)?,
                end_offset_ns: from_i64(row.get(2)?)?,
            });
        }
        Ok(Self { pictures, lines })
    }

    /// The first line said while a picture was up: the one still going when
    /// it went up, else the first to start before the next picture did.
    fn landing_line(&self, index: usize) -> Option<&Line> {
        let from_ns = self.pictures.get(index)?.offset_ns;
        let until_ns = self.pictures.get(index + 1).map(|next| next.offset_ns);
        self.lines
            .iter()
            .find(|line| line.end_offset_ns > from_ns)
            .filter(|line| until_ns.is_none_or(|until_ns| line.start_offset_ns < until_ns))
    }
}

impl MeetingStore {
    /// The newest picture whose words have not been read, other than
    /// `skipped`.
    pub(crate) fn snapshot_awaiting_text(
        &self,
        skipped: &HashSet<MeetingSnapshotId>,
    ) -> Result<Option<(MeetingSessionId, MeetingSnapshotId)>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT s.session_id, s.snapshot_id
               FROM meeting_snapshots s
               JOIN meeting_sessions m ON m.id = s.session_id
              WHERE m.phase != 'deleting'
                AND NOT EXISTS (SELECT 1 FROM meeting_snapshot_texts t
                                 WHERE t.snapshot_id = s.snapshot_id)
              ORDER BY s.captured_at_utc_ms DESC, s.snapshot_id
              LIMIT ?1",
        )?;
        let limit = i64::try_from(skipped.len() + 1).map_err(|_| StoreError::Invalid)?;
        let mut rows = statement.query(params![limit])?;
        while let Some(row) = rows.next()? {
            let snapshot_id = MeetingSnapshotId::from_uuid(parse_uuid(&row.get::<_, String>(1)?)?);
            if !skipped.contains(&snapshot_id) {
                let session_id =
                    MeetingSessionId::from_uuid(parse_uuid(&row.get::<_, String>(0)?)?);
                return Ok(Some((session_id, snapshot_id)));
            }
        }
        Ok(None)
    }

    /// Keeps the words one picture shows. A picture deleted while it was read
    /// keeps nothing, and one read already keeps its first reading.
    pub(crate) fn insert_snapshot_text(
        &self,
        snapshot_id: MeetingSnapshotId,
        reading: &ScreenText,
    ) -> Result<(), StoreError> {
        self.connection()?.execute(
            "INSERT INTO meeting_snapshot_texts (snapshot_id, title, text)
             SELECT snapshot_id, ?2, ?3 FROM meeting_snapshots WHERE snapshot_id = ?1
             ON CONFLICT (snapshot_id) DO NOTHING",
            params![snapshot_id.uuid().to_string(), reading.title, reading.text],
        )?;
        Ok(())
    }

    /// Forgets the words of every picture in every meeting.
    pub(crate) fn delete_snapshot_texts(&self) -> Result<(), StoreError> {
        self.connection()?
            .execute("DELETE FROM meeting_snapshot_texts", [])?;
        Ok(())
    }

    /// One meeting's "On screen" marks: each picture's title, on the first
    /// line said while it was up. A picture titled like the one before it,
    /// such as the same slide taken twice, adds no mark.
    pub(crate) fn snapshot_text_marks(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<MeetingSnapshotTextMark>, StoreError> {
        let connection = self.connection()?;
        let timeline = Timeline::read_in(&connection, session_id)?;
        let mut marks = Vec::new();
        let mut previous_title = "";
        for (index, picture) in timeline.pictures.iter().enumerate() {
            let repeated = picture.title == previous_title;
            previous_title = &picture.title;
            if picture.title.is_empty() || repeated {
                continue;
            }
            if let Some(line) = timeline.landing_line(index) {
                marks.push(MeetingSnapshotTextMark {
                    snapshot_id: picture.snapshot_id,
                    offset_ns: picture.offset_ns,
                    segment_id: line.segment_id,
                    title: picture.title.clone(),
                });
            }
        }
        Ok(marks)
    }

    /// The pictures whose words match `query`, best match first, each with
    /// the first line said while it was up.
    pub(crate) fn snapshot_text_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MeetingSnapshotTextMoment>, StoreError> {
        if query.trim().chars().count() < 2 {
            return Ok(Vec::new());
        }
        let Some(match_query) = meeting_fts_match_query(query) else {
            return Ok(Vec::new());
        };
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT s.session_id, s.snapshot_id, m.title,
                    COALESCE(m.started_at_utc_ms, m.created_at_utc_ms), s.offset_ns, t.title,
                    snippet(meeting_snapshot_text_fts, 0, '', '', '…', 24)
               FROM meeting_snapshot_text_fts
               JOIN meeting_snapshot_texts t ON t.id = meeting_snapshot_text_fts.rowid
               JOIN meeting_snapshots s ON s.snapshot_id = t.snapshot_id
               JOIN meeting_sessions m ON m.id = s.session_id
              WHERE meeting_snapshot_text_fts MATCH ?1
                AND m.phase != 'deleting'
              ORDER BY bm25(meeting_snapshot_text_fts), s.captured_at_utc_ms DESC
              LIMIT ?2",
        )?;
        let limit =
            i64::try_from(limit.clamp(1, SEARCH_RESULTS_MAX)).map_err(|_| StoreError::Invalid)?;
        let mut rows = statement.query(params![match_query, limit])?;
        let mut timelines: HashMap<MeetingSessionId, Timeline> = HashMap::new();
        let mut moments = Vec::new();
        while let Some(row) = rows.next()? {
            let session_id = MeetingSessionId::from_uuid(parse_uuid(&row.get::<_, String>(0)?)?);
            let snapshot_id = MeetingSnapshotId::from_uuid(parse_uuid(&row.get::<_, String>(1)?)?);
            let timeline = match timelines.entry(session_id) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => entry.insert(Timeline::read_in(&connection, session_id)?),
            };
            let line = timeline
                .pictures
                .iter()
                .position(|picture| picture.snapshot_id == snapshot_id)
                .and_then(|index| timeline.landing_line(index));
            moments.push(MeetingSnapshotTextMoment {
                session_id,
                snapshot_id,
                meeting_title: row.get(2)?,
                meeting_started_at_utc_ms: row.get(3)?,
                offset_ns: from_i64(row.get(4)?)?,
                title: row.get(5)?,
                excerpt: row.get(6)?,
                segment_id: line.map(|line| line.segment_id),
                segment_start_offset_ns: line.map(|line| line.start_offset_ns),
            });
        }
        Ok(moments)
    }
}

#[cfg(test)]
mod tests {
    use super::super::workflow_core_tests::{
        reviewable_meeting, spaced_transcript_segments, store,
    };
    use super::*;
    use crate::meeting::snapshots::{MeetingSnapshotSummary, MeetingSnapshotTrigger};

    const SECOND_NS: u64 = 1_000_000_000;
    const SLIDE: &str =
        "CAC payback by channel\nBlended CAC fell 18% after the pricing change in August";

    /// A meeting whose four lines start 20 s apart and last 15 s each.
    fn meeting(store: &MeetingStore) -> (MeetingSessionId, Vec<TranscriptSegmentId>) {
        let session_id = reviewable_meeting(store, "Q3 board review", 1_000);
        spaced_transcript_segments(
            store,
            session_id,
            &[
                "Let's start with the numbers.",
                "Here is where payback landed.",
                "Partners are our best channel.",
                "Questions?",
            ],
            20 * SECOND_NS,
            15 * SECOND_NS,
        );
        let connection = store.connection().unwrap();
        let mut statement = connection
            .prepare("SELECT segment_id FROM meeting_transcript_segments ORDER BY start_offset_ns")
            .unwrap();
        let lines = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|segment_id| {
                TranscriptSegmentId::from_uuid(parse_uuid(&segment_id.unwrap()).unwrap())
            })
            .collect();
        (session_id, lines)
    }

    /// A picture taken `at_s` seconds in, whose words read `text`.
    fn picture(
        store: &MeetingStore,
        session_id: MeetingSessionId,
        at_s: u32,
        title: &str,
        text: &str,
    ) -> MeetingSnapshotId {
        let summary = MeetingSnapshotSummary {
            snapshot_id: MeetingSnapshotId::new(),
            session_id,
            offset_ns: u64::from(at_s) * SECOND_NS,
            captured_at_utc_ms: 1_000 + i64::from(at_s) * 1_000,
            width: 1920,
            height: 1080,
            trigger: MeetingSnapshotTrigger::Automatic,
            app_bundle_id: Some("us.zoom.xos".to_owned()),
        };
        store
            .insert_meeting_snapshot(&summary, b"full picture", b"small picture")
            .unwrap();
        let reading = ScreenText {
            title: title.to_owned(),
            text: text.to_owned(),
        };
        store
            .insert_snapshot_text(summary.snapshot_id, &reading)
            .unwrap();
        summary.snapshot_id
    }

    fn indexed(store: &MeetingStore, word: &str) -> i64 {
        store
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM meeting_snapshot_text_fts
                  WHERE meeting_snapshot_text_fts MATCH ?1",
                params![word],
                |row| row.get(0),
            )
            .unwrap()
    }

    // Contract: a word shown only on a slide finds that picture, with the line
    // said while it was up as the place to open the meeting at.
    #[test]
    fn a_word_only_on_screen_finds_the_picture_and_the_line_said_under_it() {
        let (_directory, store) = store();
        let (session_id, lines) = meeting(&store);
        let snapshot_id = picture(&store, session_id, 25, "CAC payback by channel", SLIDE);

        let hits = store.snapshot_text_search("cac", 4).unwrap();

        assert_eq!(
            hits.iter()
                .map(|hit| (hit.snapshot_id, hit.segment_id, hit.segment_start_offset_ns))
                .collect::<Vec<_>>(),
            vec![(snapshot_id, Some(lines[1]), Some(20 * SECOND_NS))]
        );
    }

    // Contract: each new title marks the first line said while its picture was
    // up, and the same slide taken again adds no second mark.
    #[test]
    fn a_new_title_marks_the_first_line_said_while_its_picture_was_up() {
        let (_directory, store) = store();
        let (session_id, lines) = meeting(&store);
        let agenda = picture(
            &store,
            session_id,
            17,
            "Agenda",
            "Agenda\nNumbers\nChannels",
        );
        picture(
            &store,
            session_id,
            30,
            "Agenda",
            "Agenda\nNumbers\nChannels",
        );
        let payback = picture(&store, session_id, 41, "CAC payback by channel", SLIDE);

        let marks = store.snapshot_text_marks(session_id).unwrap();

        assert_eq!(
            marks
                .iter()
                .map(|mark| (mark.snapshot_id, mark.segment_id, mark.title.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (agenda, lines[1], "Agenda"),
                (payback, lines[2], "CAC payback by channel"),
            ]
        );
    }

    // Contract: turning reading off forgets every word, in the rows and in the
    // search index alike.
    #[test]
    fn turning_reading_off_forgets_every_word() {
        let (_directory, store) = store();
        let (session_id, _) = meeting(&store);
        picture(&store, session_id, 25, "CAC payback by channel", SLIDE);

        store.delete_snapshot_texts().unwrap();

        assert_eq!(store.snapshot_text_search("CAC", 4).unwrap(), vec![]);
        assert_eq!(store.snapshot_text_marks(session_id).unwrap(), vec![]);
        assert_eq!(indexed(&store, "CAC"), 0);
    }

    // Contract: deleting a picture forgets its words.
    #[test]
    fn deleting_a_picture_forgets_its_words() {
        let (_directory, store) = store();
        let (session_id, _) = meeting(&store);
        let snapshot_id = picture(&store, session_id, 25, "CAC payback by channel", SLIDE);

        store
            .delete_meeting_snapshot(session_id, snapshot_id)
            .unwrap();

        assert_eq!(store.snapshot_text_search("CAC", 4).unwrap(), vec![]);
        assert_eq!(indexed(&store, "CAC"), 0);
    }
}
