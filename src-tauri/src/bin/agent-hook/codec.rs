//! Strict per-agent wire decoding.
//!
//! Each agent ships its own key convention, so every event is decoded through a
//! structure that accepts exactly the fields that agent is known to send. The
//! canonical event is the only shape the rest of the hook consumes.

use serde::Deserialize;
use serde_json::{Map, Value};

pub(crate) use super::wire::{Agent, CanonicalEvent, CanonicalEventKind, CanonicalTool};
use super::SCHEMA_VERSION;

mod opencode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecodeError {
    Invalid,
    Unsupported,
}

/// Claude's envelope is snake_case: a common core on every event plus the
/// fields that event adds. The common core is `session_id`, `prompt_id`,
/// `transcript_path`, `cwd`, `permission_mode`, `effort`, and
/// `hook_event_name`, and the event list is longer than the set Sona bridges
/// and still growing — `prompt_id` arrived in a point release.
///
/// So this decoder does not deny unknown fields, for the reason [`GrokInput`]
/// does not: refusing an event over a sibling field Sona never reads takes the
/// bridge down on a Claude release rather than on a Sona bug. That mattered
/// here more than it does for Grok, because the fields at stake are not
/// exotic. `cwd` and `permission_mode` ride every event, `prompt` rides
/// `UserPromptSubmit`, and `last_assistant_message` and `stop_hook_active`
/// ride only `Stop`. Each was required or forbidden in a way Claude
/// contradicts, and any one of them on its own was enough to make every real
/// Claude event pass through unseen.
///
/// `turn_id` is the exception, declared so it can be refused: it is Codex's
/// mandatory turn key and Claude documents `prompt_id` in its place, so it is
/// what keeps a Codex payload handed to this decoder from being half-read as a
/// Claude one now that unknown fields are tolerated.
#[derive(Deserialize)]
struct ClaudeInput {
    session_id: String,
    #[serde(default)]
    transcript_path: Option<String>,
    #[serde(default)]
    stop_hook_active: bool,
    hook_event_name: String,
    #[serde(default)]
    last_assistant_message: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    tool_input: Option<Map<String, Value>>,
    #[serde(default)]
    permission_suggestions: Option<Vec<Value>>,
    #[serde(default)]
    permission_mode: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    notification_type: Option<String>,
    #[serde(default)]
    turn_id: Option<Value>,
}

/// Codex sends snake_case keys and fully specifies each event's payload, so
/// this rejects anything outside the documented set.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexInput {
    session_id: String,
    #[serde(default)]
    transcript_path: Option<String>,
    cwd: String,
    hook_event_name: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    permission_mode: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    tool_input: Option<Value>,
    #[serde(default)]
    tool_response: Option<Value>,
    #[serde(default)]
    stop_hook_active: Option<bool>,
    #[serde(default)]
    last_assistant_message: Option<String>,
}

/// Grok's envelope is camelCase and carries the event name twice: `hookEventName`
/// holds Grok's own snake_case spelling and `hook_event_name` holds the Claude
/// PascalCase spelling of the same event.
///
/// This is the one decoder without `deny_unknown_fields`. Grok documents a
/// common envelope plus per-event fields it does not enumerate in full, and
/// refusing an event because of a sibling field Sona never reads would take the
/// whole bridge down on a Grok release rather than on a Sona bug. The fields
/// Sona does read stay typed, and [`validate_grok`] still requires the ones each
/// event must carry.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrokInput {
    hook_event_name: String,
    #[serde(rename = "hook_event_name")]
    claude_event_name: String,
    session_id: String,
    workspace_root: String,
    #[serde(default)]
    prompt_id: Option<String>,
    #[serde(default)]
    permission_mode: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    stop_hook_active: Option<bool>,
    #[serde(default)]
    last_assistant_message: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    tool_input: Option<Value>,
    #[serde(default)]
    notification_type: Option<String>,
    #[serde(default)]
    message: Option<String>,
}
const OMP_EXTENSION_SCHEMA_VERSION: u8 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OmpInput {
    schema_version: u8,
    event: String,
    session_id: String,
    workspace_root: String,
    stop_hook_active: bool,
    sequence: u64,
    #[serde(default)]
    turn_id: Option<u64>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    tool_input: Option<Map<String, Value>>,
    #[serde(default)]
    approval_mode: Option<String>,
}

pub(crate) fn decode_event(agent: Agent, raw: &[u8]) -> Result<CanonicalEvent, DecodeError> {
    match agent {
        Agent::Claude => decode_claude(raw),
        Agent::Codex => decode_codex(raw),
        Agent::Grok => decode_grok(raw),
        Agent::Omp => decode_omp(raw),
        Agent::OpenCode => opencode::decode(raw),
    }
}

fn decode_claude(raw: &[u8]) -> Result<CanonicalEvent, DecodeError> {
    let input: ClaudeInput = serde_json::from_slice(raw).map_err(|_| DecodeError::Invalid)?;
    let kind = match input.hook_event_name.as_str() {
        "UserPromptSubmit" => CanonicalEventKind::UserPromptSubmit,
        "PermissionRequest" => CanonicalEventKind::PermissionRequest,
        "PreToolUse" => CanonicalEventKind::PreToolUse,
        "Stop" => CanonicalEventKind::Stop,
        "Notification" => CanonicalEventKind::Notification,
        _ => return Err(DecodeError::Unsupported),
    };
    validate_claude(&input, kind)?;
    Ok(canonical_from_claude(&input, kind))
}

/// What each event must carry, and only what Claude documents as belonging to
/// exactly one of them.
///
/// `permission_mode` used to be part of this decision — required on
/// `PermissionRequest`, forbidden everywhere else. It rides every Claude
/// event, so it discriminated nothing and refused everything. The event name
/// is the authority on which event this is; this checks that the payload
/// carries the fields that event is answered from.
fn validate_claude(input: &ClaudeInput, kind: CanonicalEventKind) -> Result<(), DecodeError> {
    // Codex's turn key in a payload handed to the Claude decoder means the
    // hook was wired to the wrong agent, and half-reading it would file the
    // session under Claude in the console.
    if input.session_id.is_empty() || input.turn_id.is_some() {
        return Err(DecodeError::Invalid);
    }

    let no_tool = input.tool_name.is_none() && input.tool_input.is_none();
    let tool_call = input.tool_name.is_some() && input.tool_input.is_some();
    let no_notification = input.message.is_none() && input.notification_type.is_none();
    let no_suggestions = input.permission_suggestions.is_none();

    let expected = match kind {
        CanonicalEventKind::UserPromptSubmit | CanonicalEventKind::Stop => {
            no_tool && no_suggestions && no_notification
        }
        // A permission request names the tool but not the call: Claude has not
        // dispatched one yet, and `permission_suggestions` — the dialog's own
        // "always allow" options — is the only field unique to this event.
        CanonicalEventKind::PermissionRequest => {
            tool_call && input.tool_use_id.is_none() && no_notification
        }
        CanonicalEventKind::PreToolUse => tool_call && no_suggestions && no_notification,
        CanonicalEventKind::Notification => no_tool && no_suggestions && input.message.is_some(),
        CanonicalEventKind::SessionStart
        | CanonicalEventKind::PostToolUse
        | CanonicalEventKind::InputRequest => return Err(DecodeError::Unsupported),
    };
    expected.then_some(()).ok_or(DecodeError::Invalid)
}

fn canonical_from_claude(input: &ClaudeInput, event: CanonicalEventKind) -> CanonicalEvent {
    CanonicalEvent {
        schema_version: SCHEMA_VERSION,
        agent: Agent::Claude,
        event,
        session_id: input.session_id.clone(),
        transcript_path: input.transcript_path.clone(),
        stop_hook_active: input.stop_hook_active,
        // Claude's `cwd` follows Claude — into a worktree, and after a `cd` —
        // so it is not the project the session is bound to. The hook's own
        // working directory is, which is what `prepare` falls back to.
        workspace_root: None,
        request_id: None,
        model: None,
        source: None,
        order: None,
        prompt: input.prompt.clone(),
        message: match event {
            CanonicalEventKind::Stop => input.last_assistant_message.clone(),
            CanonicalEventKind::Notification => input.message.clone(),
            _ => None,
        },
        reason: None,
        tool: input.tool_name.as_ref().map(|name| CanonicalTool {
            name: name.clone(),
            use_id: input.tool_use_id.clone(),
            input: input.tool_input.clone().map(Value::Object),
        }),
        permission_mode: input.permission_mode.clone(),
        bypass_permissions: false,
    }
}

fn decode_codex(raw: &[u8]) -> Result<CanonicalEvent, DecodeError> {
    let input: CodexInput = serde_json::from_slice(raw).map_err(|_| DecodeError::Invalid)?;
    let kind = match input.hook_event_name.as_str() {
        "SessionStart" => CanonicalEventKind::SessionStart,
        "UserPromptSubmit" => CanonicalEventKind::UserPromptSubmit,
        "PermissionRequest" => CanonicalEventKind::PermissionRequest,
        "PreToolUse" => CanonicalEventKind::PreToolUse,
        "PostToolUse" => CanonicalEventKind::PostToolUse,
        "Stop" => CanonicalEventKind::Stop,
        _ => return Err(DecodeError::Unsupported),
    };
    validate_codex(&input, kind)?;
    Ok(canonical_from_codex(&input, kind))
}

fn validate_codex(input: &CodexInput, kind: CanonicalEventKind) -> Result<(), DecodeError> {
    if input.session_id.is_empty() || input.cwd.is_empty() {
        return Err(DecodeError::Invalid);
    }

    let no_tool = input.tool_name.is_none()
        && input.tool_use_id.is_none()
        && input.tool_input.is_none()
        && input.tool_response.is_none();
    let tool_call = input.tool_name.is_some() && input.tool_input.is_some();

    let expected = match kind {
        CanonicalEventKind::SessionStart => {
            input.source.is_some() && input.turn_id.is_none() && input.prompt.is_none() && no_tool
        }
        CanonicalEventKind::UserPromptSubmit => {
            input.turn_id.is_some() && input.prompt.is_some() && no_tool
        }
        // A permission request names the tool but not the call: Codex has not
        // dispatched one yet.
        CanonicalEventKind::PermissionRequest => {
            input.turn_id.is_some()
                && tool_call
                && input.tool_use_id.is_none()
                && input.tool_response.is_none()
        }
        CanonicalEventKind::PreToolUse => {
            input.turn_id.is_some()
                && tool_call
                && input.tool_use_id.is_some()
                && input.tool_response.is_none()
        }
        CanonicalEventKind::PostToolUse => {
            input.turn_id.is_some()
                && tool_call
                && input.tool_use_id.is_some()
                && input.tool_response.is_some()
        }
        CanonicalEventKind::Stop => {
            input.turn_id.is_some() && input.stop_hook_active.is_some() && no_tool
        }
        CanonicalEventKind::Notification | CanonicalEventKind::InputRequest => {
            return Err(DecodeError::Unsupported)
        }
    };
    expected.then_some(()).ok_or(DecodeError::Invalid)
}

fn canonical_from_codex(input: &CodexInput, event: CanonicalEventKind) -> CanonicalEvent {
    CanonicalEvent {
        schema_version: SCHEMA_VERSION,
        agent: Agent::Codex,
        event,
        session_id: input.session_id.clone(),
        transcript_path: input.transcript_path.clone(),
        stop_hook_active: input.stop_hook_active.unwrap_or(false),
        // Codex runs a hook with the session's working directory, and that
        // directory is the project the bridge binds the session to.
        workspace_root: Some(input.cwd.clone()),
        request_id: input.turn_id.clone(),
        model: input.model.clone(),
        source: input.source.clone(),
        order: None,
        prompt: input.prompt.clone(),
        message: input.last_assistant_message.clone(),
        reason: None,
        tool: input.tool_name.as_ref().map(|name| CanonicalTool {
            name: name.clone(),
            use_id: input.tool_use_id.clone(),
            input: input.tool_input.clone(),
        }),
        permission_mode: input.permission_mode.clone(),
        bypass_permissions: bypasses_permissions(input.permission_mode.as_deref()),
    }
}

fn decode_grok(raw: &[u8]) -> Result<CanonicalEvent, DecodeError> {
    let input: GrokInput = serde_json::from_slice(raw).map_err(|_| DecodeError::Invalid)?;
    let kind = match input.hook_event_name.as_str() {
        "session_start" => CanonicalEventKind::SessionStart,
        "user_prompt_submit" => CanonicalEventKind::UserPromptSubmit,
        "pre_tool_use" => CanonicalEventKind::PreToolUse,
        "post_tool_use" => CanonicalEventKind::PostToolUse,
        "stop" => CanonicalEventKind::Stop,
        "notification" => CanonicalEventKind::Notification,
        _ => return Err(DecodeError::Unsupported),
    };
    validate_grok(&input, kind)?;
    Ok(canonical_from_grok(&input, kind))
}

fn validate_grok(input: &GrokInput, kind: CanonicalEventKind) -> Result<(), DecodeError> {
    if input.session_id.is_empty() || input.workspace_root.is_empty() {
        return Err(DecodeError::Invalid);
    }
    // Grok spells the event name twice. Disagreement means the payload is not
    // the event it announces, which no Grok release should ever send.
    if Some(input.claude_event_name.as_str()) != claude_event_name(kind) {
        return Err(DecodeError::Invalid);
    }

    let expected = match kind {
        CanonicalEventKind::PreToolUse | CanonicalEventKind::PostToolUse => {
            input.tool_name.is_some() && input.tool_input.is_some()
        }
        // The session-end fire and a turn end are the same event with different
        // reasons, so the reason has to be there to tell them apart.
        CanonicalEventKind::Stop => input.reason.is_some(),
        CanonicalEventKind::Notification => input.notification_type.is_some(),
        CanonicalEventKind::SessionStart | CanonicalEventKind::UserPromptSubmit => {
            input.tool_name.is_none() && input.tool_input.is_none()
        }
        CanonicalEventKind::PermissionRequest | CanonicalEventKind::InputRequest => {
            return Err(DecodeError::Unsupported)
        }
    };
    expected.then_some(()).ok_or(DecodeError::Invalid)
}

/// The Claude spelling Grok mirrors in its `hook_event_name` key.
fn claude_event_name(kind: CanonicalEventKind) -> Option<&'static str> {
    match kind {
        CanonicalEventKind::SessionStart => Some("SessionStart"),
        CanonicalEventKind::UserPromptSubmit => Some("UserPromptSubmit"),
        CanonicalEventKind::PermissionRequest => Some("PermissionRequest"),
        CanonicalEventKind::PreToolUse => Some("PreToolUse"),
        CanonicalEventKind::PostToolUse => Some("PostToolUse"),
        CanonicalEventKind::Stop => Some("Stop"),
        CanonicalEventKind::Notification => Some("Notification"),
        CanonicalEventKind::InputRequest => None,
    }
}

fn canonical_from_grok(input: &GrokInput, event: CanonicalEventKind) -> CanonicalEvent {
    CanonicalEvent {
        schema_version: SCHEMA_VERSION,
        agent: Agent::Grok,
        event,
        session_id: input.session_id.clone(),
        transcript_path: None,
        stop_hook_active: input.stop_hook_active.unwrap_or(false),
        workspace_root: Some(input.workspace_root.clone()),
        request_id: input.prompt_id.clone(),
        model: None,
        source: input.source.clone(),
        order: None,
        prompt: None,
        message: match event {
            CanonicalEventKind::Notification => input.message.clone(),
            _ => input.last_assistant_message.clone(),
        },
        reason: match event {
            CanonicalEventKind::Notification => input.notification_type.clone(),
            _ => input.reason.clone(),
        },
        tool: input.tool_name.as_ref().map(|name| CanonicalTool {
            name: name.clone(),
            use_id: input.tool_use_id.clone(),
            input: input.tool_input.clone(),
        }),
        permission_mode: input.permission_mode.clone(),
        bypass_permissions: bypasses_permissions(input.permission_mode.as_deref()),
    }
}

/// The one permission mode in which no prompt can appear, so Sona has nothing
/// to answer. Codex and Grok spell it the same way Claude does.
fn bypasses_permissions(permission_mode: Option<&str>) -> bool {
    permission_mode == Some("bypassPermissions")
}

fn decode_omp(raw: &[u8]) -> Result<CanonicalEvent, DecodeError> {
    let input: OmpInput = serde_json::from_slice(raw).map_err(|_| DecodeError::Invalid)?;
    if input.schema_version != OMP_EXTENSION_SCHEMA_VERSION {
        return Err(DecodeError::Invalid);
    }
    let event = match input.event.as_str() {
        "session_start" => CanonicalEventKind::SessionStart,
        "user_prompt_submit" => CanonicalEventKind::UserPromptSubmit,
        "permission_request" => CanonicalEventKind::PermissionRequest,
        "stop" => CanonicalEventKind::Stop,
        _ => return Err(DecodeError::Unsupported),
    };
    validate_omp(&input, event)?;
    Ok(canonical_from_omp(&input, event))
}

fn validate_omp(input: &OmpInput, event: CanonicalEventKind) -> Result<(), DecodeError> {
    if input.session_id.is_empty() || input.workspace_root.is_empty() || input.sequence == 0 {
        return Err(DecodeError::Invalid);
    }
    let no_tool = input.tool_name.is_none()
        && input.tool_call_id.is_none()
        && input.tool_input.is_none()
        && input.approval_mode.is_none();
    match event {
        CanonicalEventKind::SessionStart | CanonicalEventKind::UserPromptSubmit => {
            (!input.stop_hook_active && input.turn_id.is_none() && no_tool)
                .then_some(())
                .ok_or(DecodeError::Invalid)
        }
        CanonicalEventKind::PermissionRequest => (!input.stop_hook_active
            && input.turn_id.is_none()
            && input
                .tool_name
                .as_deref()
                .is_some_and(|name| !name.is_empty())
            && input
                .tool_call_id
                .as_deref()
                .is_some_and(|call_id| !call_id.is_empty())
            && input.tool_input.is_some()
            && input
                .approval_mode
                .as_deref()
                .is_some_and(|mode| !mode.is_empty()))
        .then_some(())
        .ok_or(DecodeError::Invalid),
        CanonicalEventKind::Stop => (input.turn_id.is_some() && no_tool)
            .then_some(())
            .ok_or(DecodeError::Invalid),
        _ => Err(DecodeError::Unsupported),
    }
}

fn canonical_from_omp(input: &OmpInput, event: CanonicalEventKind) -> CanonicalEvent {
    CanonicalEvent {
        schema_version: SCHEMA_VERSION,
        agent: Agent::Omp,
        event,
        session_id: input.session_id.clone(),
        transcript_path: None,
        stop_hook_active: input.stop_hook_active,
        workspace_root: Some(input.workspace_root.clone()),
        request_id: match event {
            CanonicalEventKind::PermissionRequest => input.tool_call_id.clone(),
            CanonicalEventKind::Stop => input.turn_id.map(|turn_id| turn_id.to_string()),
            _ => None,
        },
        model: None,
        source: None,
        order: None,
        prompt: None,
        message: None,
        reason: None,
        tool: input.tool_name.as_ref().map(|name| CanonicalTool {
            name: name.clone(),
            use_id: input.tool_call_id.clone(),
            input: input.tool_input.clone().map(Value::Object),
        }),
        permission_mode: input.approval_mode.clone(),
        bypass_permissions: false,
    }
}
