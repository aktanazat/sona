use super::voice_identity::{embedding_from_blob, speaker_is_user};
use super::{id, phase_from_db, source_kind_from_db, MeetingStore, StoreError};
use crate::meeting::types::{MeetingOrigin, MeetingSessionId, SourceKind};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// Who every speaker of one meeting is, as the voice code sees it, with the
/// stored vectors an offline analysis compares.
#[derive(Debug, Serialize)]
pub(crate) struct SpeakerAnalysis {
    pub meeting_id: String,
    pub imported: bool,
    /// The About-me name `speaker_is_user` compares against.
    pub user_name: Option<String>,
    pub speakers: Vec<AnalysisSpeaker>,
    pub people: Vec<AnalysisPerson>,
    pub user_voiceprint: Option<AnalysisUserVoiceprint>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AnalysisSpeaker {
    pub speaker_id: String,
    pub display_name: String,
    pub source_kind: SourceKind,
    pub revision: u64,
    pub merged_into_speaker_id: Option<String>,
    pub resolution: SpeakerResolution,
    /// What `speaker_is_user` answers; a merged-away speaker is nobody.
    pub counts_as_user: bool,
    pub voiceprint: Option<Vec<f32>>,
}

/// How the speaker got the name it carries. The store keeps a label and a
/// correction as the same `identified` row; a correction also drops the
/// speaker's voice, so `voiceprint_kept` is the only trace of which it was.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum SpeakerResolution {
    Identified {
        person_id: String,
        person_name: Option<String>,
        voiceprint_kept: bool,
    },
    Unknown,
    VoiceMatch {
        person_id: String,
        person_name: Option<String>,
        profile_revision: u64,
    },
    /// A voice suggestion waiting for confirmation; no person means the user.
    Suggestion {
        person_id: Option<String>,
        display_name: String,
    },
    None,
}

#[derive(Debug, Serialize)]
pub(crate) struct AnalysisPerson {
    pub person_id: String,
    pub display_name: String,
    pub voice_profile: Option<AnalysisVoiceProfile>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AnalysisVoiceProfile {
    pub sample_count: u64,
    pub centroid: Vec<f32>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AnalysisUserVoiceprint {
    pub dictation_count: u64,
    pub speech_ms: u64,
    pub embedding: Vec<f32>,
}

struct SpeakerRow {
    speaker_id: String,
    display_name: String,
    source_kind: String,
    revision: i64,
    merged_into_speaker_id: Option<String>,
    named_by_voice_match: bool,
    manual_kind: Option<String>,
    manual_person_id: Option<String>,
    manual_person_name: Option<String>,
    match_person_id: Option<String>,
    match_person_name: Option<String>,
    match_profile_revision: Option<i64>,
    voiceprint: Option<Vec<u8>>,
}

impl MeetingStore {
    /// A session still holding the capture: recording, paused, starting or
    /// stopping. Its track files are still being written.
    pub(crate) fn capturing_session(&self) -> Result<Option<MeetingSessionId>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT id, phase FROM meeting_sessions")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (session, phase) = row?;
            if phase_from_db(&phase)?.retains_capture_lease() {
                return Ok(Some(MeetingSessionId::from_uuid(super::parse_uuid(
                    &session,
                )?)));
            }
        }
        Ok(None)
    }

    pub(crate) fn speaker_analysis(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<SpeakerAnalysis, StoreError> {
        let user_name = self.user_name();
        let suggestions = self.voice_speaker_suggestions(session_id, user_name.as_deref())?;
        let connection = self.connection()?;
        let imported: bool = connection.query_row(
            "SELECT origin_kind = ?2 FROM meeting_sessions WHERE id = ?1",
            params![id(session_id), super::encode_json(&MeetingOrigin::Import)?],
            |row| row.get(0),
        )?;
        let speakers = speaker_rows_in(&connection, session_id)?
            .into_iter()
            .map(|row| {
                let source_kind = source_kind_from_db(&row.source_kind)?;
                let counts_as_user = row.merged_into_speaker_id.is_none()
                    && speaker_is_user(
                        &row.display_name,
                        source_kind,
                        imported,
                        row.named_by_voice_match,
                        user_name.as_deref(),
                    );
                let suggestion = suggestions
                    .iter()
                    .find(|suggestion| suggestion.speaker_id.uuid().to_string() == row.speaker_id);
                let voiceprint = row
                    .voiceprint
                    .as_deref()
                    .map(|blob| embedding_from_blob(blob).map(|print| print.as_slice().to_vec()))
                    .transpose()?;
                let resolution = match (row.manual_kind.as_deref(), row.manual_person_id) {
                    (Some("identified"), Some(person_id)) => SpeakerResolution::Identified {
                        person_id,
                        person_name: row.manual_person_name,
                        voiceprint_kept: voiceprint.is_some(),
                    },
                    (Some("unknown"), _) => SpeakerResolution::Unknown,
                    (Some(_), _) => return Err(StoreError::Corrupt),
                    (None, _) => match (row.match_person_id, row.match_profile_revision) {
                        (Some(person_id), Some(profile_revision)) => {
                            SpeakerResolution::VoiceMatch {
                                person_id,
                                person_name: row.match_person_name,
                                profile_revision: super::from_i64(profile_revision)?,
                            }
                        }
                        _ => suggestion.map_or(SpeakerResolution::None, |suggestion| {
                            SpeakerResolution::Suggestion {
                                person_id: suggestion
                                    .person_id
                                    .map(|person| person.uuid().to_string()),
                                display_name: suggestion.display_name.clone(),
                            }
                        }),
                    },
                };
                Ok(AnalysisSpeaker {
                    speaker_id: row.speaker_id,
                    display_name: row.display_name,
                    source_kind,
                    revision: super::from_i64(row.revision)?,
                    merged_into_speaker_id: row.merged_into_speaker_id,
                    resolution,
                    counts_as_user,
                    voiceprint,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(SpeakerAnalysis {
            meeting_id: session_id.uuid().to_string(),
            imported,
            user_name,
            speakers,
            people: people_in(&connection)?,
            user_voiceprint: user_voiceprint_in(&connection)?,
        })
    }
}

/// `named_by_voice_match` is the expression `user_speaker_labels_in` uses, so
/// `counts_as_user` here answers what the loops and tasks read.
fn speaker_rows_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<SpeakerRow>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT s.speaker_id, s.display_name, s.source_kind, s.revision, s.merged_into_speaker_id,
                EXISTS (SELECT 1 FROM voice_speaker_matches v JOIN persons p ON p.id = v.person_id
                         WHERE v.speaker_id = s.speaker_id AND v.speaker_revision = s.revision
                           AND p.display_name = s.display_name),
                r.resolution_kind, r.resolved_person_id, rp.display_name,
                m.person_id, mp.display_name, m.profile_revision,
                vp.embedding
           FROM meeting_speakers s
           LEFT JOIN meeting_voice_speaker_resolutions r
                  ON r.speaker_id = s.speaker_id AND r.is_current = 1
           LEFT JOIN persons rp ON rp.id = r.resolved_person_id
           LEFT JOIN voice_speaker_matches m
                  ON m.speaker_id = s.speaker_id AND m.speaker_revision = s.revision
           LEFT JOIN persons mp ON mp.id = m.person_id
           LEFT JOIN meeting_speaker_voiceprints vp ON vp.speaker_id = s.speaker_id
          WHERE s.session_id = ?1
          ORDER BY s.source_kind, s.display_name, s.speaker_id",
    )?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        Ok(SpeakerRow {
            speaker_id: row.get(0)?,
            display_name: row.get(1)?,
            source_kind: row.get(2)?,
            revision: row.get(3)?,
            merged_into_speaker_id: row.get(4)?,
            named_by_voice_match: row.get(5)?,
            manual_kind: row.get(6)?,
            manual_person_id: row.get(7)?,
            manual_person_name: row.get(8)?,
            match_person_id: row.get(9)?,
            match_person_name: row.get(10)?,
            match_profile_revision: row.get(11)?,
            voiceprint: row.get(12)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn people_in(connection: &Connection) -> Result<Vec<AnalysisPerson>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT p.id, p.display_name, v.sample_count, v.centroid
           FROM persons p LEFT JOIN voice_profiles v ON v.person_id = p.id
          ORDER BY p.display_name, p.id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<Vec<u8>>>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (person_id, display_name, sample_count, centroid) = row?;
        let voice_profile = match (sample_count, centroid) {
            (Some(sample_count), Some(centroid)) => Some(AnalysisVoiceProfile {
                sample_count: super::from_i64(sample_count)?,
                centroid: embedding_from_blob(&centroid)?.as_slice().to_vec(),
            }),
            _ => None,
        };
        Ok(AnalysisPerson {
            person_id,
            display_name,
            voice_profile,
        })
    })
    .collect()
}

fn user_voiceprint_in(
    connection: &Connection,
) -> Result<Option<AnalysisUserVoiceprint>, StoreError> {
    let row = connection
        .query_row(
            "SELECT dictation_count, speech_ms, embedding FROM user_voiceprint",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .optional()?;
    row.map(|(dictation_count, speech_ms, embedding)| {
        Ok(AnalysisUserVoiceprint {
            dictation_count: super::from_i64(dictation_count)?,
            speech_ms: super::from_i64(speech_ms)?,
            embedding: embedding_from_blob(&embedding)?.as_slice().to_vec(),
        })
    })
    .transpose()
}
