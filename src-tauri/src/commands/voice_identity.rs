use crate::meeting::people_types::PersonId;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::voice_identity::{
    MeetingSpeakerIdentifyDisposition, MeetingSpeakerIdentifyRequest, MeetingSpeakerIdentifyTarget,
    VoiceProfileStatus,
};
use crate::meeting::types::{
    MeetingCommandError, MeetingOperationId, MeetingSessionId, OperationReceipt, SpeakerId,
};
use crate::meeting::workflow_engine::voice_identity::VoiceEnrollmentCommand;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::State;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum VoiceIdentityTarget {
    Existing { person_id: PersonId },
    Create { display_name: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum VoiceSpeakerIdentityAction {
    Label { target: VoiceIdentityTarget },
    CorrectTo { target: VoiceIdentityTarget },
    MarkUnknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct VoiceSpeakerIdentityRequest {
    pub operation_id: MeetingOperationId,
    pub requested_at_utc_ms: i64,
    pub session_id: MeetingSessionId,
    pub expected_meeting_revision: u64,
    pub expected_people_revision: u64,
    pub speaker_id: SpeakerId,
    pub action: VoiceSpeakerIdentityAction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct VoiceProfileEnrollmentRequest {
    pub person_id: PersonId,
    pub session_id: MeetingSessionId,
    pub speaker_id: SpeakerId,
    pub expected_meeting_revision: u64,
    pub expected_speaker_revision: u64,
    pub expected_people_revision: u64,
    pub consent_version: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct VoiceProfileRemovalRequest {
    pub person_id: PersonId,
    pub expected_people_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct VoiceIdentityStatus {
    pub unresolved_active_speaker_ids: Vec<SpeakerId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct VoiceSpeakerIdentityResult {
    pub receipt: OperationReceipt,
    pub resolved_person_id: Option<PersonId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct VoiceProfileEnrollmentStatus {
    pub enrolled: bool,
    pub sample_count: u64,
}

#[tauri::command]
#[specta::specta]
pub async fn voice_identity_status(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<VoiceIdentityStatus, MeetingCommandError> {
    Ok(VoiceIdentityStatus {
        unresolved_active_speaker_ids: manager
            .unresolved_active_voice_speaker_ids(session_id)
            .await?,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn voice_identify_speaker(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: VoiceSpeakerIdentityRequest,
) -> Result<VoiceSpeakerIdentityResult, MeetingCommandError> {
    let result = manager
        .identify_voice_speaker(MeetingSpeakerIdentifyRequest {
            operation_id: request.operation_id,
            requested_at_utc_ms: request.requested_at_utc_ms,
            session_id: request.session_id,
            expected_meeting_revision: request.expected_meeting_revision,
            expected_people_revision: request.expected_people_revision,
            speaker_id: request.speaker_id,
            disposition: identity_disposition(request.action),
        })
        .await?;
    Ok(VoiceSpeakerIdentityResult {
        receipt: result.receipt,
        resolved_person_id: result.resolved_person_id,
    })
}
#[tauri::command]
#[specta::specta]
pub async fn voice_enroll_profile(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: VoiceProfileEnrollmentRequest,
) -> Result<VoiceProfileEnrollmentStatus, MeetingCommandError> {
    let status = manager
        .enroll_voice_profile(VoiceEnrollmentCommand {
            person_id: request.person_id,
            session_id: request.session_id,
            speaker_id: request.speaker_id,
            expected_meeting_revision: request.expected_meeting_revision,
            expected_speaker_revision: request.expected_speaker_revision,
            expected_people_revision: request.expected_people_revision,
            consent_version: request.consent_version,
        })
        .await?;
    Ok(public_profile_status(status))
}

#[tauri::command]
#[specta::specta]
pub async fn voice_remove_profile(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: VoiceProfileRemovalRequest,
) -> Result<VoiceProfileEnrollmentStatus, MeetingCommandError> {
    Ok(public_profile_status(
        manager
            .remove_voice_profile(request.person_id, request.expected_people_revision)
            .await?,
    ))
}

fn identity_disposition(action: VoiceSpeakerIdentityAction) -> MeetingSpeakerIdentifyDisposition {
    match action {
        VoiceSpeakerIdentityAction::Label { target } => MeetingSpeakerIdentifyDisposition::Label {
            target: identity_target(target),
        },
        VoiceSpeakerIdentityAction::CorrectTo { target } => {
            MeetingSpeakerIdentifyDisposition::CorrectTo {
                target: identity_target(target),
            }
        }
        VoiceSpeakerIdentityAction::MarkUnknown => MeetingSpeakerIdentifyDisposition::MarkUnknown,
    }
}

fn identity_target(target: VoiceIdentityTarget) -> MeetingSpeakerIdentifyTarget {
    match target {
        VoiceIdentityTarget::Existing { person_id } => {
            MeetingSpeakerIdentifyTarget::Existing(person_id)
        }
        VoiceIdentityTarget::Create { display_name } => {
            MeetingSpeakerIdentifyTarget::Create { display_name }
        }
    }
}

fn public_profile_status(status: VoiceProfileStatus) -> VoiceProfileEnrollmentStatus {
    match status {
        VoiceProfileStatus::Unenrolled => VoiceProfileEnrollmentStatus {
            enrolled: false,
            sample_count: 0,
        },
        VoiceProfileStatus::Enrolled { sample_count, .. } => VoiceProfileEnrollmentStatus {
            enrolled: true,
            sample_count,
        },
    }
}

