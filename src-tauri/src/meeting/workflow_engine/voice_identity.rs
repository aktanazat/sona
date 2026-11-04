use super::{map_store_error, now_utc_ms, MEETING_EVENT_SCHEMA_VERSION};
use crate::meeting::people_types::PersonId;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::voice_identity::{
    ExplicitVoiceConsent, MeetingSpeakerIdentifyRequest, VoiceProfileEnrollmentRequest,
    VoiceProfileStatus, VoiceSpeakerIdentityResult, VoiceSpeakerSuggestion,
};
use crate::meeting::transcript_correction_types::TranscriptCorrectionChangedSession;
use crate::meeting::types::{
    MeetingCommandError, MeetingEventPayload, MeetingSessionId, SpeakerId,
};
use tauri::Emitter;

/// The ids and fences one enrollment needs, as one value. Three adjacent
/// `u64` fences read the same positionally, so callers name them instead.
pub(crate) struct VoiceEnrollmentCommand {
    pub person_id: PersonId,
    pub session_id: MeetingSessionId,
    pub speaker_id: SpeakerId,
    pub expected_meeting_revision: u64,
    pub expected_speaker_revision: u64,
    pub expected_people_revision: u64,
    pub consent_version: u32,
}

impl MeetingSessionManager {
    pub(crate) async fn unresolved_active_voice_speaker_ids(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<SpeakerId>, MeetingCommandError> {
        self.store()
            .await?
            .unresolved_active_voice_speaker_ids(session_id)
            .map_err(map_store_error)
    }

    pub(crate) async fn voice_speaker_suggestions(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<VoiceSpeakerSuggestion>, MeetingCommandError> {
        self.store()
            .await?
            .voice_speaker_suggestions(session_id)
            .map_err(map_store_error)
    }

    pub(crate) async fn identify_voice_speaker(
        &self,
        request: MeetingSpeakerIdentifyRequest,
    ) -> Result<VoiceSpeakerIdentityResult, MeetingCommandError> {
        let store = self.store().await?;
        let before = store
            .transcript_correction_identity_snapshot(request.session_id, &[request.speaker_id])
            .map_err(map_store_error)?;
        let result = store.identify_speaker(request).map_err(map_store_error)?;
        self.emit_transcript_correction_changes(
            store
                .transcript_correction_projection_changes(before)
                .map_err(map_store_error)?,
        );
        if let Some(revision) = result.receipt.new_revision {
            self.emit_artifact_changed(result.receipt.session_id.clone(), revision);
        }
        Ok(result)
    }

    pub(crate) fn emit_transcript_correction_changes(
        &self,
        changed_sessions: impl IntoIterator<Item = TranscriptCorrectionChangedSession>,
    ) {
        if let Some(app) = self.app_handle() {
            for session in changed_sessions {
                let _ = app.emit(
                    "meeting:transcript-changed",
                    MeetingEventPayload {
                        event_schema_version: MEETING_EVENT_SCHEMA_VERSION,
                        session_id: Some(session.session_id),
                        revision: session.revision,
                    },
                );
            }
        }
    }

    pub(crate) async fn enroll_voice_profile(
        &self,
        command: VoiceEnrollmentCommand,
    ) -> Result<VoiceProfileStatus, MeetingCommandError> {
        let VoiceEnrollmentCommand {
            person_id,
            session_id,
            speaker_id,
            expected_meeting_revision,
            expected_speaker_revision,
            expected_people_revision,
            consent_version,
        } = command;
        let consent = ExplicitVoiceConsent::granted(consent_version).map_err(map_store_error)?;
        let store = self.store().await?;
        let evidence = store
            .local_voice_enrollment_evidence(session_id, speaker_id)
            .map_err(map_store_error)?;
        let (model, embedding) = self
            .processing()
            .embed_voice_enrollment_evidence(&store, evidence)
            .await
            .map_err(map_store_error)?;
        let before = store
            .transcript_correction_people_snapshot(&[person_id])
            .map_err(map_store_error)?;
        let status = store
            .commit_voice_profile_enrollment(VoiceProfileEnrollmentRequest {
                person_id,
                expected_meeting_revision,
                expected_people_revision,
                expected_speaker_revision,
                consent,
                evidence,
                model,
                embedding,
                committed_at_utc_ms: now_utc_ms(),
            })
            .map_err(map_store_error)?;
        self.emit_transcript_correction_changes(
            store
                .transcript_correction_projection_changes(before)
                .map_err(map_store_error)?,
        );
        Ok(status)
    }

    pub(crate) async fn remove_voice_profile(
        &self,
        person_id: PersonId,
        expected_people_revision: u64,
    ) -> Result<VoiceProfileStatus, MeetingCommandError> {
        let store = self.store().await?;
        let before = store
            .transcript_correction_people_snapshot(&[person_id])
            .map_err(map_store_error)?;
        let status = store
            .remove_voice_profile(person_id, expected_people_revision)
            .map_err(map_store_error)?;
        self.emit_transcript_correction_changes(
            store
                .transcript_correction_projection_changes(before)
                .map_err(map_store_error)?,
        );
        Ok(status)
    }
}
