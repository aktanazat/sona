use serde::Deserialize;
use serde_json::{Map, Value};

use super::super::wire::EventOrder;
use super::super::ASK_USER_QUESTION_TOOL;
use super::{
    Agent, CanonicalEvent, CanonicalEventKind, CanonicalTool, DecodeError, SCHEMA_VERSION,
};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Event {
    SessionStart,
    UserPromptSubmit,
    InputRequest,
    PermissionRequest,
    QuestionRequest,
    Notification,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    schema_version: u8,
    instance_id: String,
    instance_started_at_ms: u64,
    event: Event,
    session_id: String,
    workspace_root: String,
    sequence: u64,
    request_id: Option<String>,
    prompt: Option<String>,
    message: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<Map<String, Value>>,
}

pub(super) fn decode(raw: &[u8]) -> Result<CanonicalEvent, DecodeError> {
    let input: Input = serde_json::from_slice(raw).map_err(|_| DecodeError::Invalid)?;
    if input.schema_version != 1
        || input.sequence == 0
        || input.instance_id.trim().is_empty()
        || input.instance_started_at_ms == 0
        || input.session_id.trim().is_empty()
        || input.workspace_root.trim().is_empty()
        || (input.prompt.is_some() && !matches!(input.event, Event::UserPromptSubmit))
    {
        return Err(DecodeError::Invalid);
    }
    let has_request = input.request_id.as_deref().is_some_and(|id| !id.is_empty());
    let no_tool = input.tool_name.is_none() && input.tool_input.is_none();
    let event = match input.event {
        Event::SessionStart if input.request_id.is_none() && no_tool => {
            CanonicalEventKind::SessionStart
        }
        Event::UserPromptSubmit if input.request_id.is_none() && no_tool => {
            CanonicalEventKind::UserPromptSubmit
        }
        Event::InputRequest if has_request && no_tool => CanonicalEventKind::InputRequest,
        Event::PermissionRequest
            if has_request
                && input
                    .tool_name
                    .as_deref()
                    .is_some_and(|name| !name.is_empty())
                && input.tool_input.is_some() =>
        {
            CanonicalEventKind::PermissionRequest
        }
        Event::QuestionRequest
            if has_request && input.tool_name.is_none() && input.tool_input.is_some() =>
        {
            CanonicalEventKind::PreToolUse
        }
        Event::Notification
            if input.request_id.is_none()
                && no_tool
                && input
                    .message
                    .as_deref()
                    .is_some_and(|message| !message.is_empty()) =>
        {
            CanonicalEventKind::Notification
        }
        _ => return Err(DecodeError::Invalid),
    };
    let tool = if matches!(input.event, Event::QuestionRequest) {
        Some(CanonicalTool {
            name: ASK_USER_QUESTION_TOOL.to_string(),
            use_id: None,
            input: input.tool_input.map(Value::Object),
        })
    } else {
        input.tool_name.map(|name| CanonicalTool {
            name,
            use_id: None,
            input: input.tool_input.map(Value::Object),
        })
    };
    Ok(CanonicalEvent {
        schema_version: SCHEMA_VERSION,
        agent: Agent::OpenCode,
        event,
        session_id: input.session_id,
        transcript_path: None,
        stop_hook_active: false,
        workspace_root: Some(input.workspace_root),
        request_id: input.request_id,
        model: None,
        source: None,
        order: Some(EventOrder {
            instance_id: input.instance_id,
            instance_started_at_ms: input.instance_started_at_ms,
            sequence: input.sequence,
        }),
        prompt: input.prompt,
        message: input.message,
        reason: None,
        tool,
        permission_mode: None,
        bypass_permissions: false,
    })
}
