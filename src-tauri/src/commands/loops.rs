use crate::meeting::loop_types::{
    MeetingLoopAssignRequest, MeetingLoopMutationResult, MeetingLoopReopenRequest,
    MeetingLoopResolveRequest, MeetingLoopsResult,
};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::{DecisionDrift, MeetingCommandError, MeetingSessionId};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn meeting_loops(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<MeetingLoopsResult, MeetingCommandError> {
    manager.loops_list(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_decision_drift(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<Vec<DecisionDrift>, MeetingCommandError> {
    manager.decision_drift(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_loop_resolve(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingLoopResolveRequest,
) -> Result<MeetingLoopMutationResult, MeetingCommandError> {
    manager.loop_resolve(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_loop_reopen(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingLoopReopenRequest,
) -> Result<MeetingLoopMutationResult, MeetingCommandError> {
    manager.loop_reopen(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_loop_assign(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingLoopAssignRequest,
) -> Result<MeetingLoopMutationResult, MeetingCommandError> {
    manager.loop_assign(request).await
}
