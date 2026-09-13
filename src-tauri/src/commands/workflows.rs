use crate::analytics::DashboardTrendRequest;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::MeetingCommandError;
use crate::meeting::workflow_types::{
    PaginatedWorkflowRuns, WorkflowRunTrend, WorkflowRunsRequest, WorkflowSetEnabledRequest,
    WorkflowsListResult,
};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn workflows_list(
    manager: State<'_, Arc<MeetingSessionManager>>,
) -> Result<WorkflowsListResult, MeetingCommandError> {
    manager.workflows_list().await
}

#[tauri::command]
#[specta::specta]
pub async fn workflow_set_enabled(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: WorkflowSetEnabledRequest,
) -> Result<WorkflowsListResult, MeetingCommandError> {
    manager.workflow_set_enabled(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn workflow_runs(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: Option<WorkflowRunsRequest>,
) -> Result<PaginatedWorkflowRuns, MeetingCommandError> {
    manager.workflow_runs(request.unwrap_or_default()).await
}

#[tauri::command]
#[specta::specta]
pub async fn workflow_run_trend(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: DashboardTrendRequest,
) -> Result<WorkflowRunTrend, MeetingCommandError> {
    manager.workflow_run_trend(request).await
}
