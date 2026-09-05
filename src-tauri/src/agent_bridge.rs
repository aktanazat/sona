use crate::agent_hook_wire::{
    self as wire, Agent, AppHeartbeat, AppLease, CanonicalEventKind, HookAck, HookRequest,
    HookResponse, PolicyProjection, RuntimePaths, SessionBinding, WriteMode, MAX_REQUEST_BYTES,
    PROTOCOL_GENERATION, REQUEST_TTL_MS, SCHEMA_VERSION,
};
use crate::settings::{
    AgentBridgeAgent, AgentBridgePermissionDecision, AgentBridgePermissionRule, AgentBridgeSettings,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Manager as _;

/// The app is the only publisher of the lease, heartbeat, and policy records,
/// so it owns their lifetimes. Republication must beat every one of them.
const HEARTBEAT_INTERVAL_MS: u64 = 15_000;
const APP_LEASE_TTL_MS: u64 = 30_000;
const HEARTBEAT_TTL_MS: u64 = 30_000;
const POLICY_TTL_MS: u64 = 30_000;
/// The worker sleeps on the wake socket; this bounds how long the heartbeat,
/// lease check, and expiry sweeps wait when no hook has written anything.
/// It must stay well inside `HEARTBEAT_TTL_MS - HEARTBEAT_INTERVAL_MS`.
const IDLE_FALLBACK: std::time::Duration = std::time::Duration::from_secs(1);
/// The app is the only reader of hook acknowledgements.
const MAX_ACK_BYTES: usize = 8 * 1024;
/// The only reply lifetime comes from the existing hook request boundary.
const DEFAULT_PENDING_TTL_MS: u64 = REQUEST_TTL_MS;
/// The OMP extension's path *as `tauri.conf.json` ships it*, which is the
/// repo-relative path under `src-tauri` and not the name of the file.
///
/// `resources: ["resources/**/*"]` preserves the whole relative path inside the
/// bundle, so the extension lands at `Resources/resources/agent-bridge/…` and
/// not at `Resources/agent-bridge/…`. Resolving it by joining the leaf onto
/// `resource_dir()` therefore missed on every installed build, and because the
/// snippet is one all-or-nothing document, one absent file for the agent
/// nobody enables took the setup instructions for the other three down with
/// it: the console read `Setup code unavailable: packaged OMP extension
/// unavailable`. `BaseDirectory::Resource` is how `tray.rs` reads its own
/// bundled files, and it takes the same repo-relative path.
const OMP_EXTENSION_RESOURCE: &str = "resources/agent-bridge/sona-omp-agent-bridge.mjs";

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentBridgeDiagnostic {
    Disabled,
    RuntimeUnavailable,
    InteractiveUnsupported,
    AppLockHeld,
    Active,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentBridgePendingState {
    Held,
    ResponseWritten,
    Emitted,
    CopyOnly,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentBridgeRequestState {
    Observed,
    Responded,
    Dismissed,
    Expired,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentBridgeRequestKind {
    SessionStart,
    UserPromptSubmit,
    PermissionRequest,
    PreToolUse,
    PostToolUse,
    Stop,
    Notification,
}

impl AgentBridgeRequestKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::UserPromptSubmit => "user_prompt_submit",
            Self::PermissionRequest => "permission_request",
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
            Self::Stop => "stop",
            Self::Notification => "notification",
        }
    }
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct AgentBridgeStatus {
    pub running: bool,
    pub diagnostic: AgentBridgeDiagnostic,
    pub policy_generation: u64,
    pub observed_sessions: usize,
    pub pending_messages: usize,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct AgentBridgeObservedSession {
    pub id: String,
    pub agent: AgentBridgeAgent,
    pub canonical_project_hash: String,
    pub session_generation: u64,
    pub policy_generation: u64,
    pub last_seen_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct AgentBridgeObservedRequest {
    pub id: String,
    pub session_id: String,
    pub agent: AgentBridgeAgent,
    pub kind: AgentBridgeRequestKind,
    pub tool_name: Option<String>,
    pub permission_mode: Option<String>,
    pub expires_at_ms: u64,
    pub state: AgentBridgeRequestState,
    /// Whether the hook invocation behind this row is holding its agent open
    /// for Sona's answer. Derived once from
    /// [`crate::agent_hook_wire::CanonicalEvent::awaits_response`], so the
    /// console and the responder read the same fact instead of each deciding
    /// which agents and events can be answered.
    pub awaiting_response: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct AgentBridgePendingMessage {
    pub id: String,
    pub agent: AgentBridgeAgent,
    pub session_id: String,
    pub text: String,
    pub expires_at_ms: u64,
    pub state: AgentBridgePendingState,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentBridgeError {
    Disabled,
    RuntimeUnavailable,
    InteractiveUnsupported,
    AppLockHeld,
    UnknownSession,
    UnknownRequest,
    WrongAgent,
    WrongDestination,
    Expired,
    DuplicatePending,
    EmptyMessage,
    ConfirmationMismatch,
    RuleRequired,
    RuleMismatch,
    PermissionResponseUnsupported,
    AlreadyHandled,
    PersistenceFailed,
}

impl std::fmt::Display for AgentBridgeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for AgentBridgeError {}

#[derive(Debug, Clone)]
struct ObservedRequestRecord {
    public: AgentBridgeObservedRequest,
    request: HookRequest,
    tool_input_hash: String,
}

struct PendingMessageRecord {
    public: AgentBridgePendingMessage,
    binding: SessionBinding,
    response_invocation_id: Option<String>,
}

pub struct AgentBridgeCore {
    paths: RuntimePaths,
    app_instance_id: String,
    running: bool,
    diagnostic: AgentBridgeDiagnostic,
    last_heartbeat_ms: u64,
    observed_sessions: BTreeMap<String, AgentBridgeObservedSession>,
    observed_requests: BTreeMap<String, ObservedRequestRecord>,
    pending_messages: BTreeMap<String, PendingMessageRecord>,
    prepared_sessions: BTreeSet<String>,
    next_id: u64,
}

impl AgentBridgeCore {
    pub fn new(paths: RuntimePaths, app_instance_id: String) -> Result<Self, AgentBridgeError> {
        if !is_wire_id(&app_instance_id) {
            return Err(AgentBridgeError::RuntimeUnavailable);
        }
        Ok(Self {
            paths,
            app_instance_id,
            running: false,
            diagnostic: AgentBridgeDiagnostic::Disabled,
            last_heartbeat_ms: 0,
            observed_sessions: BTreeMap::new(),
            observed_requests: BTreeMap::new(),
            pending_messages: BTreeMap::new(),
            prepared_sessions: BTreeSet::new(),
            next_id: 0,
        })
    }

    pub fn start(
        &mut self,
        settings: &AgentBridgeSettings,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        if !settings.master_enabled {
            self.diagnostic = AgentBridgeDiagnostic::Disabled;
            return Err(AgentBridgeError::Disabled);
        }
        if !self.paths.interactive_supported() {
            self.diagnostic = AgentBridgeDiagnostic::InteractiveUnsupported;
            return Err(AgentBridgeError::InteractiveUnsupported);
        }

        if let Ok(existing) = self.paths.read_lease() {
            // Another instance's lease blocks this one only while that
            // publisher is still running. An app that was killed leaves an
            // unexpired record behind, and refusing on it would keep the bridge
            // dark for the whole session: reconcile runs at startup and on a
            // settings change, never on a timer.
            if existing.is_valid_at(now_ms)
                && existing.app_instance_id != self.app_instance_id
                && publisher_alive(existing.pid)
            {
                self.diagnostic = AgentBridgeDiagnostic::AppLockHeld;
                return Err(AgentBridgeError::AppLockHeld);
            }
            remove_private_file(&self.paths.app_lock_path());
        }

        let lease = lease(&self.app_instance_id, settings.policy_generation, now_ms);
        if wire::atomic_write_json(&self.paths.app_lock_path(), &lease, WriteMode::CreateNew)
            .is_err()
        {
            // The release above and this create are not one operation, so
            // another instance can win the gap. When it has, the lock is held —
            // saying the runtime is unavailable would send a reader looking for
            // a broken directory instead of a second Sona.
            let contended = self
                .paths
                .read_lease()
                .is_ok_and(|held| held.is_valid_at(now_ms));
            self.diagnostic = if contended {
                AgentBridgeDiagnostic::AppLockHeld
            } else {
                AgentBridgeDiagnostic::RuntimeUnavailable
            };
            return Err(if contended {
                AgentBridgeError::AppLockHeld
            } else {
                AgentBridgeError::RuntimeUnavailable
            });
        }
        self.write_control_state(settings, now_ms)?;
        self.running = true;
        self.diagnostic = AgentBridgeDiagnostic::Active;
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Ok(lease) = self.paths.read_lease() {
            if lease.app_instance_id == self.app_instance_id {
                remove_private_file(&self.paths.app_lock_path());
                remove_private_file(&self.paths.heartbeat_path());
                remove_private_file(&self.paths.policy_path());
                let _ = fs::remove_file(self.paths.wake_path());
            }
        }
        self.running = false;
        self.diagnostic = AgentBridgeDiagnostic::Disabled;
    }

    pub fn tick(
        &mut self,
        settings: &AgentBridgeSettings,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        if !self.running || !settings.master_enabled {
            return Err(AgentBridgeError::Disabled);
        }
        let lease = self
            .paths
            .read_lease()
            .map_err(|_| AgentBridgeError::RuntimeUnavailable)?;
        if lease.app_instance_id != self.app_instance_id {
            self.running = false;
            self.diagnostic = AgentBridgeDiagnostic::AppLockHeld;
            return Err(AgentBridgeError::AppLockHeld);
        }
        // The lease is ours (checked above), so a generation it does not
        // carry can only mean the settings moved under a running worker.
        // Hooks read the projection with that generation, so it is republished
        // at once rather than on the heartbeat.
        if lease.policy_generation != settings.policy_generation
            || now_ms.saturating_sub(self.last_heartbeat_ms) >= HEARTBEAT_INTERVAL_MS
        {
            self.write_control_state(settings, now_ms)?;
        }
        self.expire_pending(now_ms);
        self.scan_sessions(settings, now_ms)?;
        Ok(())
    }

    pub fn status(&self, settings: &AgentBridgeSettings) -> AgentBridgeStatus {
        AgentBridgeStatus {
            running: self.running,
            diagnostic: self.diagnostic,
            policy_generation: settings.policy_generation,
            observed_sessions: self.observed_sessions.len(),
            pending_messages: self
                .pending_messages
                .values()
                .filter(|pending| {
                    matches!(
                        pending.public.state,
                        AgentBridgePendingState::Held | AgentBridgePendingState::ResponseWritten
                    )
                })
                .count(),
        }
    }

    pub fn sessions(&self) -> Vec<AgentBridgeObservedSession> {
        self.observed_sessions.values().cloned().collect()
    }

    pub fn requests(&self) -> Vec<AgentBridgeObservedRequest> {
        self.observed_requests
            .values()
            .map(|record| record.public.clone())
            .collect()
    }

    pub fn pending_messages(&self) -> Vec<AgentBridgePendingMessage> {
        self.pending_messages
            .values()
            .map(|record| record.public.clone())
            .collect()
    }

    pub fn create_reply_preview(
        &mut self,
        session_id: &str,
        text: String,
        now_ms: u64,
        ttl_ms: Option<u64>,
    ) -> Result<AgentBridgePendingMessage, AgentBridgeError> {
        if text.trim().is_empty() {
            return Err(AgentBridgeError::EmptyMessage);
        }
        let session = self
            .observed_sessions
            .get(session_id)
            .cloned()
            .ok_or(AgentBridgeError::UnknownSession)?;
        // Every agent Sona bridges continues a stopped turn the same way, but
        // only a session that reached a prompt has a turn to reply to.
        if !self.prepared_sessions.contains(session_id) {
            return Err(AgentBridgeError::UnknownSession);
        }
        if self.pending_messages.values().any(|pending| {
            pending.public.session_id == session_id
                && matches!(pending.public.state, AgentBridgePendingState::Held)
        }) {
            return Err(AgentBridgeError::DuplicatePending);
        }
        let binding = SessionBinding {
            agent: wire_agent(session.agent),
            session_handle: session.id.clone(),
            project_hash: session.canonical_project_hash,
            session_generation: session.session_generation,
            policy_generation: session.policy_generation,
        };
        let id = self.next_opaque_id(b"pending", session_id.as_bytes(), now_ms);
        let pending = PendingMessageRecord {
            public: AgentBridgePendingMessage {
                id: id.clone(),
                agent: session.agent,
                session_id: session_id.to_string(),
                text,
                expires_at_ms: now_ms.saturating_add(ttl_ms.unwrap_or(DEFAULT_PENDING_TTL_MS)),
                state: AgentBridgePendingState::Held,
                confirmed: false,
            },
            binding,
            response_invocation_id: None,
        };
        let public = pending.public.clone();
        self.pending_messages.insert(id, pending);
        Ok(public)
    }

    pub fn confirm_reply_preview(
        &mut self,
        pending_id: &str,
        session_id: &str,
        text: &str,
        now_ms: u64,
    ) -> Result<AgentBridgePendingMessage, AgentBridgeError> {
        let pending = self
            .pending_messages
            .get_mut(pending_id)
            .ok_or(AgentBridgeError::UnknownRequest)?;
        if pending.public.state != AgentBridgePendingState::Held {
            return Err(AgentBridgeError::AlreadyHandled);
        }
        if pending.public.expires_at_ms < now_ms {
            pending.public.state = AgentBridgePendingState::CopyOnly;
            return Err(AgentBridgeError::Expired);
        }
        if pending.public.session_id != session_id {
            return Err(AgentBridgeError::WrongDestination);
        }
        if pending.public.text != text {
            return Err(AgentBridgeError::ConfirmationMismatch);
        }
        if pending.public.confirmed {
            return Err(AgentBridgeError::AlreadyHandled);
        }
        pending.public.confirmed = true;
        Ok(pending.public.clone())
    }
    pub fn cancel_pending(&mut self, pending_id: &str) -> Result<(), AgentBridgeError> {
        let pending = self
            .pending_messages
            .get_mut(pending_id)
            .ok_or(AgentBridgeError::UnknownRequest)?;
        match pending.public.state {
            AgentBridgePendingState::Held | AgentBridgePendingState::CopyOnly => {
                pending.public.state = AgentBridgePendingState::Cancelled;
                Ok(())
            }
            _ => Err(AgentBridgeError::AlreadyHandled),
        }
    }

    pub fn dismiss_request(&mut self, request_id: &str) -> Result<(), AgentBridgeError> {
        let request = self
            .observed_requests
            .get_mut(request_id)
            .ok_or(AgentBridgeError::UnknownRequest)?;
        if request.public.state != AgentBridgeRequestState::Observed {
            return Err(AgentBridgeError::AlreadyHandled);
        }
        request.public.state = AgentBridgeRequestState::Dismissed;
        Ok(())
    }

    pub fn exact_rule_for_request(
        &self,
        request_id: &str,
        rule_id: String,
        decision: AgentBridgePermissionDecision,
    ) -> Result<AgentBridgePermissionRule, AgentBridgeError> {
        let record = self
            .observed_requests
            .get(request_id)
            .ok_or(AgentBridgeError::UnknownRequest)?;
        if !record.public.awaiting_response {
            return Err(AgentBridgeError::PermissionResponseUnsupported);
        }

        if !matches!(
            record.public.kind,
            AgentBridgeRequestKind::PermissionRequest | AgentBridgeRequestKind::PreToolUse
        ) {
            return Err(AgentBridgeError::RuleMismatch);
        }
        let tool_name = record
            .public
            .tool_name
            .clone()
            .ok_or(AgentBridgeError::RuleMismatch)?;
        Ok(AgentBridgePermissionRule {
            id: rule_id,
            agent: record.public.agent,
            canonical_project_hash: record.request.binding.project_hash.clone(),
            tool_name,
            permission_mode: record.public.permission_mode.clone(),
            tool_input_hash: record.tool_input_hash.clone(),
            decision,
            user_created: true,
        })
    }

    pub fn respond_permission(
        &mut self,
        request_id: &str,
        rule_id: &str,
        decision: AgentBridgePermissionDecision,
        settings: &AgentBridgeSettings,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let record = self
            .observed_requests
            .get(request_id)
            .cloned()
            .ok_or(AgentBridgeError::UnknownRequest)?;
        if !record.public.awaiting_response {
            return Err(AgentBridgeError::PermissionResponseUnsupported);
        }

        if record.public.state != AgentBridgeRequestState::Observed {
            return Err(AgentBridgeError::AlreadyHandled);
        }
        if record.request.expires_at_ms < now_ms {
            return Err(AgentBridgeError::Expired);
        }
        let rule = settings
            .permission_rules
            .iter()
            .find(|rule| rule.id == rule_id && rule.user_created)
            .ok_or(AgentBridgeError::RuleRequired)?;
        if !rule_matches(rule, &record, decision) {
            return Err(AgentBridgeError::RuleMismatch);
        }
        let outcome = match decision {
            AgentBridgePermissionDecision::Allow => "approve",
            AgentBridgePermissionDecision::Deny => "reject",
        };
        self.persist_response(&record.request, outcome, None, now_ms)?;
        if let Some(request) = self.observed_requests.get_mut(request_id) {
            request.public.state = AgentBridgeRequestState::Responded;
        }
        Ok(())
    }

    fn write_control_state(
        &mut self,
        settings: &AgentBridgeSettings,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let lease = lease(&self.app_instance_id, settings.policy_generation, now_ms);
        wire::atomic_write_json(&self.paths.app_lock_path(), &lease, WriteMode::Replace)
            .map_err(|_| AgentBridgeError::PersistenceFailed)?;
        let heartbeat = AppHeartbeat {
            schema_version: SCHEMA_VERSION,
            protocol_generation: PROTOCOL_GENERATION,
            app_instance_id: self.app_instance_id.clone(),
            policy_generation: settings.policy_generation,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(HEARTBEAT_TTL_MS),
        };
        wire::atomic_write_json(&self.paths.heartbeat_path(), &heartbeat, WriteMode::Replace)
            .map_err(|_| AgentBridgeError::PersistenceFailed)?;
        let policy = policy_projection(settings, &self.app_instance_id, now_ms);
        wire::atomic_write_json(&self.paths.policy_path(), &policy, WriteMode::Replace)
            .map_err(|_| AgentBridgeError::PersistenceFailed)?;
        self.last_heartbeat_ms = now_ms;
        Ok(())
    }

    fn scan_sessions(
        &mut self,
        settings: &AgentBridgeSettings,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let files = collect_wire_files(&self.paths.sessions_root())
            .map_err(|_| AgentBridgeError::RuntimeUnavailable)?;
        for path in files.requests {
            let request: HookRequest = match wire::read_json_bounded(&path, MAX_REQUEST_BYTES) {
                Ok(request) => request,
                Err(_) => continue,
            };
            if request.expires_at_ms < now_ms {
                remove_private_file(&path);
                if let Ok(session) = self.paths.session(&request.binding) {
                    if let Ok(ack_path) = session.ack_path(&request.invocation_id) {
                        remove_private_file(&ack_path);
                    }
                }
                continue;
            }
            if !self.request_allowed(&request, settings, now_ms)
                || self.observed_requests.contains_key(&request.invocation_id)
            {
                continue;
            }
            let awaits_response = request.event.awaits_response();
            self.observe_request(request, now_ms)?;
            if !awaits_response {
                remove_private_file(&path);
            }
        }
        for path in files.acks {
            let ack: HookAck = match wire::read_json_bounded(&path, MAX_ACK_BYTES) {
                Ok(ack) => ack,
                Err(_) => continue,
            };
            if ack.app_instance_id != self.app_instance_id || ack.outcome != "response_emitted" {
                continue;
            }
            let binding = match self.observed_requests.get(&ack.invocation_id) {
                Some(record) if record.request.binding == ack.binding => {
                    record.request.binding.clone()
                }
                _ => continue,
            };
            self.apply_ack(&ack);
            remove_private_file(&path);
            if let Ok(session) = self.paths.session(&binding) {
                if let Ok(request_path) = session.request_path(&ack.invocation_id) {
                    remove_private_file(&request_path);
                }
            }
        }
        Ok(())
    }

    fn request_allowed(
        &self,
        request: &HookRequest,
        settings: &AgentBridgeSettings,
        now_ms: u64,
    ) -> bool {
        request.is_valid_at(now_ms)
            && request.app_instance_id == self.app_instance_id
            && request.binding.policy_generation == settings.policy_generation
            && settings.master_enabled
            && setting_agent_enabled(settings, request.binding.agent)
            && settings.allows_project_hash(&request.binding.project_hash)
    }

    fn observe_request(
        &mut self,
        request: HookRequest,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let agent = setting_agent(request.binding.agent);
        let session_id = request.binding.session_handle.clone();
        self.observed_sessions
            .entry(session_id.clone())
            .and_modify(|session| session.last_seen_at_ms = now_ms)
            .or_insert_with(|| AgentBridgeObservedSession {
                id: session_id.clone(),
                agent,
                canonical_project_hash: request.binding.project_hash.clone(),
                session_generation: request.binding.session_generation,
                policy_generation: request.binding.policy_generation,
                last_seen_at_ms: now_ms,
            });
        let kind = request_kind(request.event.event);
        match kind {
            AgentBridgeRequestKind::UserPromptSubmit => {
                self.prepared_sessions.insert(session_id.clone());
            }
            AgentBridgeRequestKind::Stop => {
                self.prepared_sessions.remove(&session_id);
            }
            _ => {}
        }
        let tool_input_hash = request
            .event
            .tool
            .as_ref()
            .and_then(|tool| tool.input.as_ref())
            .and_then(|input| serde_json::to_vec(input).ok())
            .map(|bytes| opaque_hash(&[b"tool-input", &bytes]))
            .unwrap_or_else(|| opaque_hash(&[b"tool-input-none"]));
        let awaiting_response = request.event.awaits_response();
        let public = AgentBridgeObservedRequest {
            id: request.invocation_id.clone(),
            session_id: session_id.clone(),
            agent,
            kind,
            tool_name: request.event.tool_name().map(ToOwned::to_owned),
            permission_mode: request.event.permission_mode.clone(),
            expires_at_ms: request.expires_at_ms,
            state: AgentBridgeRequestState::Observed,
            awaiting_response,
        };
        self.observed_requests.insert(
            request.invocation_id.clone(),
            ObservedRequestRecord {
                public,
                request: request.clone(),
                tool_input_hash,
            },
        );
        if kind == AgentBridgeRequestKind::Stop && awaiting_response {
            self.respond_to_stop(&request, now_ms)?;
        }
        Ok(())
    }

    fn respond_to_stop(
        &mut self,
        request: &HookRequest,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let pending_id = self.pending_messages.iter().find_map(|(id, pending)| {
            (pending.public.confirmed
                && pending.public.state == AgentBridgePendingState::Held
                && pending.binding == request.binding
                && pending.public.expires_at_ms >= now_ms)
                .then(|| id.clone())
        });
        let Some(pending_id) = pending_id else {
            return Ok(());
        };
        let text = self
            .pending_messages
            .get(&pending_id)
            .map(|pending| pending.public.text.clone())
            .ok_or(AgentBridgeError::UnknownRequest)?;
        self.persist_response(request, "block", Some(text), now_ms)?;
        if let Some(pending) = self.pending_messages.get_mut(&pending_id) {
            pending.public.state = AgentBridgePendingState::ResponseWritten;
            pending.response_invocation_id = Some(request.invocation_id.clone());
        }
        if let Some(observed) = self.observed_requests.get_mut(&request.invocation_id) {
            observed.public.state = AgentBridgeRequestState::Responded;
        }
        Ok(())
    }

    fn persist_response(
        &self,
        request: &HookRequest,
        outcome: &str,
        reason: Option<String>,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let response = HookResponse {
            schema_version: SCHEMA_VERSION,
            protocol_generation: PROTOCOL_GENERATION,
            app_instance_id: self.app_instance_id.clone(),
            binding: request.binding.clone(),
            invocation_id: request.invocation_id.clone(),
            outcome: outcome.to_string(),
            issued_at_ms: now_ms,
            expires_at_ms: request.expires_at_ms,
            reason,
            answers: None,
        };
        let session = self
            .paths
            .session(&request.binding)
            .map_err(|_| AgentBridgeError::PersistenceFailed)?;
        wire::atomic_write_json(
            &session
                .response_path(&response.invocation_id)
                .map_err(|_| AgentBridgeError::PersistenceFailed)?,
            &response,
            WriteMode::CreateNew,
        )
        .map_err(|_| AgentBridgeError::PersistenceFailed)
    }

    fn apply_ack(&mut self, ack: &HookAck) {
        for pending in self.pending_messages.values_mut() {
            if pending.response_invocation_id.as_deref() == Some(&ack.invocation_id)
                && pending.binding == ack.binding
                && pending.public.state == AgentBridgePendingState::ResponseWritten
            {
                pending.public.state = AgentBridgePendingState::Emitted;
            }
        }
    }

    fn expire_pending(&mut self, now_ms: u64) {
        for pending in self.pending_messages.values_mut() {
            if pending.public.state == AgentBridgePendingState::Held
                && pending.public.expires_at_ms < now_ms
            {
                pending.public.state = AgentBridgePendingState::CopyOnly;
            }
        }
        for observed in self.observed_requests.values_mut() {
            if observed.public.state == AgentBridgeRequestState::Observed
                && observed.public.expires_at_ms < now_ms
            {
                observed.public.state = AgentBridgeRequestState::Expired;
            }
        }
    }

    fn next_opaque_id(&mut self, kind: &[u8], binding: &[u8], now_ms: u64) -> String {
        self.next_id = self.next_id.saturating_add(1);
        opaque_hash(&[
            kind,
            self.app_instance_id.as_bytes(),
            binding,
            &now_ms.to_be_bytes(),
            &self.next_id.to_be_bytes(),
        ])
    }
}

impl Drop for AgentBridgeCore {
    fn drop(&mut self) {
        self.stop();
    }
}

struct WireFiles {
    requests: Vec<PathBuf>,
    acks: Vec<PathBuf>,
}

fn collect_wire_files(root: &Path) -> io::Result<WireFiles> {
    wire::verify_private_directory(root)?;
    let mut files = WireFiles {
        requests: Vec::new(),
        acks: Vec::new(),
    };
    collect_at_depth(root, 0, &mut files)?;
    Ok(files)
}

fn collect_at_depth(path: &Path, depth: usize, files: &mut WireFiles) -> io::Result<()> {
    if depth > 5 {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        let metadata = fs::symlink_metadata(&child)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.file_type().is_dir() {
            if wire::verify_private_directory(&child).is_err() {
                continue;
            }
            collect_at_depth(&child, depth + 1, files)?;
        } else if metadata.file_type().is_file() {
            if wire::verify_private_file(&child).is_err()
                || child.extension().and_then(|ext| ext.to_str()) != Some("json")
            {
                continue;
            }
            match child
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
            {
                Some("requests") => files.requests.push(child),
                Some("acks") => files.acks.push(child),
                _ => {}
            }
        }
    }
    Ok(())
}

fn lease(app_instance_id: &str, policy_generation: u64, now_ms: u64) -> AppLease {
    AppLease {
        schema_version: SCHEMA_VERSION,
        protocol_generation: PROTOCOL_GENERATION,
        app_instance_id: app_instance_id.to_string(),
        pid: process::id(),
        policy_generation,
        issued_at_ms: now_ms,
        expires_at_ms: now_ms.saturating_add(APP_LEASE_TTL_MS),
    }
}

/// Whether the process that published a lease is still running.
///
/// Signal 0 performs the permission check and returns without delivering
/// anything, so it reports liveness without disturbing the holder. Only "no
/// such process" clears a lease; a live process this app may not signal keeps
/// it. A recycled pid reads as alive, which only defers the takeover to the
/// next launch.
#[cfg(unix)]
fn publisher_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return true;
    };
    // SAFETY: kill with signal 0 delivers nothing and touches no caller memory.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn publisher_alive(_pid: u32) -> bool {
    true
}

/// The bridge reads the wall clock only at its own boundaries; every wire
/// record carries the instant it was published.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

fn policy_projection(
    settings: &AgentBridgeSettings,
    app_instance_id: &str,
    now_ms: u64,
) -> PolicyProjection {
    let enabled_agents = [
        (AgentBridgeAgent::Claude, Agent::Claude),
        (AgentBridgeAgent::Codex, Agent::Codex),
        (AgentBridgeAgent::Grok, Agent::Grok),
        (AgentBridgeAgent::Omp, Agent::Omp),
    ]
    .into_iter()
    .filter_map(|(setting, wire)| settings.agent_enabled(setting).then_some(wire))
    .collect();
    PolicyProjection {
        schema_version: SCHEMA_VERSION,
        protocol_generation: PROTOCOL_GENERATION,
        app_instance_id: app_instance_id.to_string(),
        generation: settings.policy_generation,
        master_enabled: settings.master_enabled,
        enabled_agents,
        allowed_project_hashes: settings
            .allowed_projects
            .iter()
            .map(|scope| scope.canonical_project_hash.clone())
            .collect(),
        issued_at_ms: now_ms,
        expires_at_ms: now_ms.saturating_add(POLICY_TTL_MS),
    }
}

fn wire_agent(agent: AgentBridgeAgent) -> Agent {
    match agent {
        AgentBridgeAgent::Claude => Agent::Claude,
        AgentBridgeAgent::Codex => Agent::Codex,
        AgentBridgeAgent::Grok => Agent::Grok,
        AgentBridgeAgent::Omp => Agent::Omp,
    }
}

fn setting_agent(agent: Agent) -> AgentBridgeAgent {
    match agent {
        Agent::Claude => AgentBridgeAgent::Claude,
        Agent::Codex => AgentBridgeAgent::Codex,
        Agent::Grok => AgentBridgeAgent::Grok,
        Agent::Omp => AgentBridgeAgent::Omp,
    }
}

fn setting_agent_enabled(settings: &AgentBridgeSettings, agent: Agent) -> bool {
    settings.agent_enabled(setting_agent(agent))
}

fn request_kind(kind: CanonicalEventKind) -> AgentBridgeRequestKind {
    match kind {
        CanonicalEventKind::SessionStart => AgentBridgeRequestKind::SessionStart,
        CanonicalEventKind::UserPromptSubmit => AgentBridgeRequestKind::UserPromptSubmit,
        CanonicalEventKind::PermissionRequest => AgentBridgeRequestKind::PermissionRequest,
        CanonicalEventKind::PreToolUse => AgentBridgeRequestKind::PreToolUse,
        CanonicalEventKind::PostToolUse => AgentBridgeRequestKind::PostToolUse,
        CanonicalEventKind::Stop => AgentBridgeRequestKind::Stop,
        CanonicalEventKind::Notification => AgentBridgeRequestKind::Notification,
    }
}

fn rule_matches(
    rule: &AgentBridgePermissionRule,
    record: &ObservedRequestRecord,
    decision: AgentBridgePermissionDecision,
) -> bool {
    record.public.awaiting_response
        && rule.user_created
        && rule.agent == record.public.agent
        && rule.canonical_project_hash == record.request.binding.project_hash
        && record.public.tool_name.as_deref() == Some(rule.tool_name.as_str())
        && rule.permission_mode == record.public.permission_mode
        && rule.tool_input_hash == record.tool_input_hash
        && rule.decision == decision
        && matches!(
            record.public.kind,
            AgentBridgeRequestKind::PermissionRequest | AgentBridgeRequestKind::PreToolUse
        )
}

fn opaque_hash(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part);
        digest.update([0]);
    }
    let bytes = digest.finalize();
    let mut output = String::with_capacity(32);
    for byte in bytes.iter().take(16) {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn is_wire_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn remove_private_file(path: &Path) {
    if wire::verify_private_file(path).is_ok() {
        let _ = fs::remove_file(path);
    }
}

#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct AgentBridgeUpdateEvent {
    pub status: AgentBridgeStatus,
}

struct BridgeWorker {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    join: std::thread::JoinHandle<()>,
}

/// The app's end of the wake socket. A hook that lands a request or an ack
/// sends one datagram here, so the worker sleeps until there is something to
/// read instead of walking the session tree on a timer. The hook's half is
/// [`wire::send_wake`]; only the app binds.
struct WakeListener {
    #[cfg(unix)]
    socket: std::os::unix::net::UnixDatagram,
    #[cfg(not(unix))]
    fallback: std::time::Duration,
}

impl WakeListener {
    #[cfg(unix)]
    fn bind(path: &Path, fallback: std::time::Duration) -> io::Result<Self> {
        // A killed app leaves its socket file behind; the lease already
        // proves nobody else is listening, so the stale entry is replaced.
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let socket = std::os::unix::net::UnixDatagram::bind(path)?;
        socket.set_read_timeout(Some(fallback))?;
        Ok(Self { socket })
    }

    #[cfg(not(unix))]
    fn bind(_path: &Path, fallback: std::time::Duration) -> io::Result<Self> {
        Ok(Self { fallback })
    }

    /// Blocks until a writer wakes the listener or the fallback interval
    /// elapses, and says which one happened.
    fn wait(&self) -> bool {
        #[cfg(unix)]
        {
            let mut byte = [0u8; 1];
            self.socket.recv(&mut byte).is_ok()
        }
        #[cfg(not(unix))]
        {
            std::thread::sleep(self.fallback);
            false
        }
    }
}

pub struct AgentBridgeManager {
    app: tauri::AppHandle,
    core: std::sync::Arc<std::sync::Mutex<AgentBridgeCore>>,
    worker: std::sync::Mutex<Option<BridgeWorker>>,
}

impl AgentBridgeManager {
    pub fn new(app: &tauri::AppHandle) -> Result<Self, String> {
        let paths = RuntimePaths::for_current_user().map_err(|_| "agent runtime unavailable")?;
        let instance_id = opaque_hash(&[
            b"app-instance",
            &process::id().to_be_bytes(),
            &now_ms().to_be_bytes(),
        ]);
        let core = AgentBridgeCore::new(paths, instance_id).map_err(|error| error.to_string())?;
        Ok(Self {
            app: app.clone(),
            core: std::sync::Arc::new(std::sync::Mutex::new(core)),
            worker: std::sync::Mutex::new(None),
        })
    }

    pub fn reconcile(&self) -> Result<(), String> {
        let settings = crate::settings::get_settings(&self.app).agent_bridge;
        if !settings.master_enabled {
            self.stop_worker();
            lock_recover(&self.core).stop();
            return Ok(());
        }

        if lock_recover(&self.worker).is_some() {
            return Ok(());
        }
        let listener = {
            let mut core = lock_recover(&self.core);
            core.start(&settings, now_ms())
                .map_err(|error| error.to_string())?;
            match WakeListener::bind(&core.paths.wake_path(), IDLE_FALLBACK) {
                Ok(listener) => listener,
                Err(error) => {
                    core.stop();
                    return Err(format!("agent wake socket unavailable: {error}"));
                }
            }
        };

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_stop = stop.clone();
        let app = self.app.clone();
        let core = self.core.clone();
        let meeting_manager = self
            .app
            .try_state::<std::sync::Arc<crate::meeting::session::MeetingSessionManager>>()
            .map(|manager| std::sync::Arc::clone(&manager));
        let join = std::thread::spawn(move || {
            use std::sync::atomic::Ordering;
            use tauri::Emitter as _;
            use tauri_specta::Event as _;
            let mut previous: Option<(bool, usize, usize, u64)> = None;
            let mut recorded_workflow_requests = BTreeSet::new();
            while !worker_stop.load(Ordering::Acquire) {
                let settings = crate::settings::get_settings(&app).agent_bridge;
                if !settings.master_enabled {
                    break;
                }
                let (status, requests) = {
                    let mut core = lock_recover(&core);
                    let _ = core.tick(&settings, now_ms());
                    (core.status(&settings), core.requests())
                };
                let active_request_ids = requests
                    .iter()
                    .map(|request| request.id.clone())
                    .collect::<BTreeSet<_>>();
                recorded_workflow_requests
                    .retain(|request_id| active_request_ids.contains(request_id));
                if let Some(manager) = &meeting_manager {
                    for request in requests {
                        if recorded_workflow_requests.contains(&request.id) {
                            continue;
                        }
                        let request_id = request.id;
                        let kind = request.kind.as_str().to_string();
                        if tauri::async_runtime::block_on(
                            manager.record_agent_hook_event(request_id.clone(), kind),
                        ) {
                            recorded_workflow_requests.insert(request_id);
                        }
                    }
                }
                let signature = (
                    status.running,
                    status.observed_sessions,
                    status.pending_messages,
                    status.policy_generation,
                );
                if previous != Some(signature) {
                    let _ = app.emit(
                        AgentBridgeUpdateEvent::NAME,
                        AgentBridgeUpdateEvent {
                            status: status.clone(),
                        },
                    );
                    previous = Some(signature);
                }
                listener.wait();
            }
        });
        *lock_recover(&self.worker) = Some(BridgeWorker { stop, join });
        Ok(())
    }

    pub fn shutdown(&self) {
        self.stop_worker();
        lock_recover(&self.core).stop();
    }

    fn stop_worker(&self) {
        use std::sync::atomic::Ordering;
        let worker = lock_recover(&self.worker).take();
        if let Some(worker) = worker {
            worker.stop.store(true, Ordering::Release);
            wire::send_wake(&lock_recover(&self.core).paths.wake_path());
            let _ = worker.join.join();
        }
    }

    fn status(&self) -> AgentBridgeStatus {
        let settings = crate::settings::get_settings(&self.app).agent_bridge;
        lock_recover(&self.core).status(&settings)
    }
}

impl Drop for AgentBridgeManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn lock_recover<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn mutate_bridge_settings<F>(
    app: &tauri::AppHandle,
    manager: &AgentBridgeManager,
    mutation: F,
) -> Result<AgentBridgeSettings, String>
where
    F: FnOnce(&mut AgentBridgeSettings) -> Result<(), String>,
{
    let bridge = crate::settings::try_update_settings(app, |settings| {
        mutation(&mut settings.agent_bridge)?;
        settings.agent_bridge.advance_policy_generation();
        Ok::<AgentBridgeSettings, String>(settings.agent_bridge.clone())
    })?;
    manager.reconcile()?;
    Ok(bridge)
}

#[tauri::command]
#[specta::specta]
pub fn get_agent_bridge_status(manager: tauri::State<'_, AgentBridgeManager>) -> AgentBridgeStatus {
    manager.status()
}

#[tauri::command]
#[specta::specta]
pub fn get_agent_bridge_sessions(
    manager: tauri::State<'_, AgentBridgeManager>,
) -> Vec<AgentBridgeObservedSession> {
    lock_recover(&manager.core).sessions()
}

#[tauri::command]
#[specta::specta]
pub fn get_agent_bridge_requests(
    manager: tauri::State<'_, AgentBridgeManager>,
) -> Vec<AgentBridgeObservedRequest> {
    lock_recover(&manager.core).requests()
}

#[tauri::command]
#[specta::specta]
pub fn get_agent_bridge_pending_messages(
    manager: tauri::State<'_, AgentBridgeManager>,
) -> Vec<AgentBridgePendingMessage> {
    lock_recover(&manager.core).pending_messages()
}

#[tauri::command]
#[specta::specta]
pub fn set_agent_bridge_master(
    app: tauri::AppHandle,
    manager: tauri::State<'_, AgentBridgeManager>,
    enabled: bool,
) -> Result<AgentBridgeSettings, String> {
    mutate_bridge_settings(&app, &manager, |bridge| {
        bridge.master_enabled = enabled;
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn set_agent_bridge_agent_enabled(
    app: tauri::AppHandle,
    manager: tauri::State<'_, AgentBridgeManager>,
    agent: AgentBridgeAgent,
    enabled: bool,
) -> Result<AgentBridgeSettings, String> {
    mutate_bridge_settings(&app, &manager, |bridge| {
        bridge.set_agent_enabled(agent, enabled);
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn authorize_agent_bridge_project(
    app: tauri::AppHandle,
    manager: tauri::State<'_, AgentBridgeManager>,
    selected_path: String,
) -> Result<AgentBridgeSettings, String> {
    let canonical_project_hash = wire::project_hash(Path::new(&selected_path))
        .map_err(|_| "selected project is unavailable".to_string())?;
    mutate_bridge_settings(&app, &manager, |bridge| {
        if !bridge.allows_project_hash(&canonical_project_hash) {
            bridge
                .allowed_projects
                .push(crate::settings::AgentBridgeProjectScope {
                    canonical_project_hash,
                });
        }
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn remove_agent_bridge_project(
    app: tauri::AppHandle,
    manager: tauri::State<'_, AgentBridgeManager>,
    canonical_project_hash: String,
) -> Result<AgentBridgeSettings, String> {
    mutate_bridge_settings(&app, &manager, |bridge| {
        bridge
            .allowed_projects
            .retain(|scope| scope.canonical_project_hash != canonical_project_hash);
        bridge
            .permission_rules
            .retain(|rule| rule.canonical_project_hash != canonical_project_hash);
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn create_agent_bridge_reply_preview(
    manager: tauri::State<'_, AgentBridgeManager>,
    session_id: String,
    text: String,
) -> Result<AgentBridgePendingMessage, String> {
    lock_recover(&manager.core)
        .create_reply_preview(&session_id, text, now_ms(), None)
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn confirm_agent_bridge_reply(
    manager: tauri::State<'_, AgentBridgeManager>,
    pending_id: String,
    session_id: String,
    text: String,
) -> Result<AgentBridgePendingMessage, String> {
    lock_recover(&manager.core)
        .confirm_reply_preview(&pending_id, &session_id, &text, now_ms())
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn cancel_agent_bridge_message(
    manager: tauri::State<'_, AgentBridgeManager>,
    pending_id: String,
) -> Result<(), String> {
    lock_recover(&manager.core)
        .cancel_pending(&pending_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn dismiss_agent_bridge_request(
    manager: tauri::State<'_, AgentBridgeManager>,
    request_id: String,
) -> Result<(), String> {
    lock_recover(&manager.core)
        .dismiss_request(&request_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn create_agent_bridge_permission_rule(
    app: tauri::AppHandle,
    manager: tauri::State<'_, AgentBridgeManager>,
    request_id: String,
    decision: AgentBridgePermissionDecision,
) -> Result<AgentBridgePermissionRule, String> {
    let rule_id = opaque_hash(&[
        b"permission-rule",
        request_id.as_bytes(),
        &now_ms().to_be_bytes(),
    ]);
    let rule = lock_recover(&manager.core)
        .exact_rule_for_request(&request_id, rule_id, decision)
        .map_err(|error| error.to_string())?;
    let saved = rule.clone();
    mutate_bridge_settings(&app, &manager, |bridge| {
        bridge.permission_rules.push(rule);
        Ok(())
    })?;
    Ok(saved)
}

#[tauri::command]
#[specta::specta]
pub fn delete_agent_bridge_permission_rule(
    app: tauri::AppHandle,
    manager: tauri::State<'_, AgentBridgeManager>,
    rule_id: String,
) -> Result<AgentBridgeSettings, String> {
    mutate_bridge_settings(&app, &manager, |bridge| {
        bridge.permission_rules.retain(|rule| rule.id != rule_id);
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn respond_agent_bridge_permission(
    app: tauri::AppHandle,
    manager: tauri::State<'_, AgentBridgeManager>,
    request_id: String,
    rule_id: String,
    decision: AgentBridgePermissionDecision,
) -> Result<(), String> {
    let settings = crate::settings::get_settings(&app).agent_bridge;
    lock_recover(&manager.core)
        .respond_permission(&request_id, &rule_id, decision, &settings, now_ms())
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn get_agent_bridge_hook_snippet(app: tauri::AppHandle) -> Result<String, String> {
    let executable = std::env::current_exe().map_err(|_| "hook path unavailable".to_string())?;
    let directory = executable
        .parent()
        .ok_or_else(|| "hook path unavailable".to_string())?;
    #[cfg(windows)]
    let hook = directory.join("sona-agent-hook.exe");
    #[cfg(not(windows))]
    let hook = directory.join("sona-agent-hook");
    let hook = hook
        .canonicalize()
        .map_err(|_| "packaged hook unavailable".to_string())?;
    if !fs::metadata(&hook)
        .map_err(|_| "packaged hook unavailable".to_string())?
        .is_file()
    {
        return Err("packaged hook unavailable".to_string());
    }
    let omp_extension = app
        .path()
        .resolve(OMP_EXTENSION_RESOURCE, tauri::path::BaseDirectory::Resource)
        .map_err(|_| "packaged OMP extension unavailable".to_string())?
        .canonicalize()
        .map_err(|_| "packaged OMP extension unavailable".to_string())?;
    if !fs::metadata(&omp_extension)
        .map_err(|_| "packaged OMP extension unavailable".to_string())?
        .is_file()
    {
        return Err("packaged OMP extension unavailable".to_string());
    }
    let hook = hook
        .to_str()
        .ok_or_else(|| "hook path is not UTF-8".to_string())?;
    let omp_extension = omp_extension
        .to_str()
        .ok_or_else(|| "OMP extension path is not UTF-8".to_string())?;
    serde_json::to_string_pretty(&serde_json::json!({
        "claude": {
            "command": hook,
            "args": ["claude"]
        },
        "codex": command_hook_config(
            &shell_invocation(hook, "codex"),
            &["SessionStart", "UserPromptSubmit", "PermissionRequest", "Stop"],
        ),
        "grok": command_hook_config(
            &shell_invocation(hook, "grok"),
            &["SessionStart", "UserPromptSubmit", "PreToolUse", "Stop"],
        ),
        "omp": {
            "command": "omp",
            "args": ["--extension", omp_extension],
            "env": {
                "SONA_AGENT_HOOK": hook
            }
        }
    }))
    .map_err(|_| "hook snippet unavailable".to_string())
}

/// Claude's `hooks.json` shape as Codex and Grok read it: an event name to a
/// matcher group, each holding the handlers to run.
#[derive(Serialize)]
struct CommandHookConfig {
    hooks: BTreeMap<String, [HookGroup; 1]>,
}

#[derive(Clone, Serialize)]
struct HookGroup {
    hooks: [CommandHook; 1],
}

#[derive(Clone, Serialize)]
struct CommandHook {
    #[serde(rename = "type")]
    kind: &'static str,
    command: String,
}

/// Codex and Grok both read Claude's `hooks.json` shape but run handlers
/// through a shell, so the hook arrives as one command string rather than an
/// argv pair.
fn command_hook_config(command: &str, events: &[&str]) -> CommandHookConfig {
    let group = HookGroup {
        hooks: [CommandHook {
            kind: "command",
            command: command.to_string(),
        }],
    };
    CommandHookConfig {
        hooks: events
            .iter()
            .map(|event| ((*event).to_string(), [group.clone()]))
            .collect(),
    }
}

/// Quotes the packaged hook so an installation path containing spaces still
/// runs as one command.
fn shell_invocation(hook: &str, agent: &str) -> String {
    format!("\"{hook}\" {agent}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hook_wire::{CanonicalEvent, CanonicalTool};
    use crate::settings::{AgentBridgePermissionRule, AgentBridgeProjectScope};
    use serde_json::json;
    use std::error::Error;
    #[cfg(unix)]
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    fn test_root(name: &str) -> io::Result<PathBuf> {
        let root =
            std::env::temp_dir().join(format!("sona-bridge-{name}-{}-{}", process::id(), now_ms()));
        #[cfg(unix)]
        {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700).create(&root)?;
        }
        #[cfg(not(unix))]
        fs::create_dir(&root)?;
        Ok(root)
    }

    /// The setup snippet's one external dependency, checked against the tree
    /// the bundler copies from.
    ///
    /// `tauri.conf.json` ships `resources/**/*` and preserves each match's
    /// relative path, so the string the app resolves has to name the file from
    /// `src-tauri` — the same repo-relative form `tray.rs` passes for its own
    /// bundled icons. A leaf-only path resolves to nothing in a bundle, which
    /// is not a visible failure anywhere: the command returns one error string
    /// and the console prints "Setup code unavailable", so the hook
    /// configuration for all four agents disappears over the one file that
    /// belongs to the agent shipped disabled.
    #[test]
    fn the_omp_extension_is_named_where_the_bundle_copies_it_from() {
        let declared = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(OMP_EXTENSION_RESOURCE);
        assert!(
            declared.is_file(),
            "{OMP_EXTENSION_RESOURCE} does not name a bundled resource: {} is not a file",
            declared.display()
        );
        let manifest =
            fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json"))
                .expect("tauri.conf.json is readable");
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest).expect("tauri.conf.json is JSON");
        let resources = manifest["bundle"]["resources"]
            .as_array()
            .expect("the bundle declares resources");
        let root = OMP_EXTENSION_RESOURCE
            .split('/')
            .next()
            .expect("the resource path has a first segment");
        assert!(
            resources
                .iter()
                .filter_map(serde_json::Value::as_str)
                .any(|pattern| pattern.starts_with(&format!("{root}/"))),
            "no bundle resource pattern ships {root}/: {resources:?}"
        );
    }

    fn enabled_settings(project_hash: String) -> AgentBridgeSettings {
        AgentBridgeSettings {
            master_enabled: true,
            claude_enabled: true,
            codex_enabled: true,
            grok_enabled: true,
            omp_enabled: true,
            policy_generation: 7,
            allowed_projects: vec![AgentBridgeProjectScope {
                canonical_project_hash: project_hash,
            }],
            permission_rules: Vec::new(),
        }
    }

    fn event(
        kind: CanonicalEventKind,
        project: &Path,
        tool: Option<CanonicalTool>,
    ) -> CanonicalEvent {
        CanonicalEvent {
            schema_version: SCHEMA_VERSION,
            agent: Agent::Claude,
            event: kind,
            session_id: "provider-session".to_string(),
            transcript_path: None,
            stop_hook_active: false,
            workspace_root: project.to_str().map(ToOwned::to_owned),
            request_id: None,
            model: None,
            source: None,
            prompt: None,
            message: None,
            reason: None,
            tool,
            permission_mode: Some("default".to_string()),
            bypass_permissions: false,
        }
    }

    fn binding(project: &Path) -> io::Result<SessionBinding> {
        SessionBinding::new(Agent::Claude, "provider-session", project, 2, 7)
    }
    fn omp_event(
        kind: CanonicalEventKind,
        project: &Path,
        tool: Option<CanonicalTool>,
    ) -> CanonicalEvent {
        let request_id = tool.as_ref().and_then(|tool| tool.use_id.clone());
        CanonicalEvent {
            schema_version: SCHEMA_VERSION,
            agent: Agent::Omp,
            event: kind,
            session_id: "omp-session".to_string(),
            transcript_path: None,
            stop_hook_active: false,
            workspace_root: Some(
                project
                    .to_str()
                    .expect("OMP test project path is UTF-8")
                    .to_string(),
            ),
            request_id,
            model: None,
            source: None,
            prompt: None,
            message: None,
            reason: None,
            tool,
            permission_mode: (kind == CanonicalEventKind::PermissionRequest)
                .then_some("always-ask".to_string()),
            bypass_permissions: false,
        }
    }

    fn omp_binding(project: &Path) -> io::Result<SessionBinding> {
        SessionBinding::new(Agent::Omp, "omp-session", project, 2, 7)
    }

    fn persist_event(
        paths: &RuntimePaths,
        app_id: &str,
        binding: SessionBinding,
        event: CanonicalEvent,
        raw: &[u8],
        now_ms: u64,
    ) -> io::Result<HookRequest> {
        let request = HookRequest::new(app_id.to_string(), binding, &event, raw, now_ms)?;
        paths.session(&request.binding)?.persist_request(&request)?;
        Ok(request)
    }

    #[test]
    fn observed_nonblocking_request_is_removed_from_wire() -> Result<(), Box<dyn Error>> {
        let root = test_root("nonblocking-wire")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"nonblocking-wire-app"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let request = persist_event(
            &paths,
            &app_id,
            binding,
            event(CanonicalEventKind::UserPromptSubmit, &root, None),
            b"nonblocking request",
            1_001,
        )?;
        let session = paths.session(&request.binding)?;
        core.tick(&settings, 1_002)?;
        assert_eq!(core.requests().len(), 1);
        assert!(!session.request_path(&request.invocation_id)?.exists());
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn emitted_ack_removes_its_request_and_ack() -> Result<(), Box<dyn Error>> {
        let root = test_root("ack-wire")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"ack-wire-app"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let request = persist_event(
            &paths,
            &app_id,
            binding,
            event(CanonicalEventKind::Stop, &root, None),
            b"acknowledged response",
            1_001,
        )?;
        let session = paths.session(&request.binding)?;
        core.tick(&settings, 1_002)?;
        session.persist_ack(&HookAck::response_emitted(&request, 1_003))?;
        core.tick(&settings, 1_004)?;
        assert!(!session.request_path(&request.invocation_id)?.exists());
        assert!(!session.ack_path(&request.invocation_id)?.exists());
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn expired_request_removes_its_matching_ack() -> Result<(), Box<dyn Error>> {
        let root = test_root("expired-wire")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"expired-wire-app"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let request = persist_event(
            &paths,
            &app_id,
            binding,
            event(CanonicalEventKind::Stop, &root, None),
            b"expired response",
            1_001,
        )?;
        let session = paths.session(&request.binding)?;
        session.persist_ack(&HookAck::response_emitted(&request, 1_002))?;
        core.tick(&settings, request.expires_at_ms.saturating_add(1))?;
        assert!(!session.request_path(&request.invocation_id)?.exists());
        assert!(!session.ack_path(&request.invocation_id)?.exists());
        assert!(core.requests().is_empty());
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn policy_change_under_a_running_worker_is_republished_at_once() -> Result<(), Box<dyn Error>> {
        let root = test_root("policy-change")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let binding = binding(&root)?;
        let mut settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), opaque_hash(&[b"policy-change"]))?;
        core.start(&settings, 1_000)?;
        settings.advance_policy_generation();
        // Well inside the heartbeat interval: only the generation mismatch
        // can trigger the write, and a hook reading the projection must see
        // the new generation on the next tick.
        core.tick(&settings, 1_001)?;
        assert_eq!(paths.read_policy()?.generation, settings.policy_generation);
        assert_eq!(
            paths.read_lease()?.policy_generation,
            settings.policy_generation
        );
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// A hook that writes a request must wake the worker at once; the timer
    /// is only the fallback for a socket the hook could not reach.
    #[cfg(unix)]
    #[test]
    fn persisting_a_request_wakes_the_worker_before_its_fallback() -> Result<(), Box<dyn Error>> {
        let root = test_root("wake")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let fallback = std::time::Duration::from_secs(30);
        let listener = WakeListener::bind(&paths.wake_path(), fallback)?;
        let started = std::time::Instant::now();
        persist_event(
            &paths,
            &opaque_hash(&[b"wake-app"]),
            binding(&root)?,
            event(CanonicalEventKind::SessionStart, &root, None),
            b"payload",
            1_000,
        )?;
        assert!(
            listener.wait(),
            "the request write did not wake the listener"
        );
        assert!(started.elapsed() < fallback);
        // Nothing else was written, so the next wait is the plain timeout.
        let quiet = WakeListener::bind(&paths.wake_path(), std::time::Duration::from_millis(20))?;
        assert!(!quiet.wait());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn fresh_foreign_lock_blocks_then_stale_owner_can_release() -> Result<(), Box<dyn Error>> {
        let root = test_root("lock")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let project_hash = wire::project_hash(&root)?;
        let settings = enabled_settings(project_hash);
        let mut first = AgentBridgeCore::new(paths.clone(), opaque_hash(&[b"first"]))?;
        let mut second = AgentBridgeCore::new(paths, opaque_hash(&[b"second"]))?;
        first.start(&settings, 1_000)?;
        assert_eq!(
            second.start(&settings, 1_001),
            Err(AgentBridgeError::AppLockHeld)
        );
        first.stop();
        second.start(&settings, 1_002)?;
        second.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// A killed app leaves an unexpired lease behind. The next launch has to
    /// take it over, or its bridge stays dark until someone toggles a setting.
    #[cfg(unix)]
    #[test]
    fn unexpired_lease_from_a_dead_publisher_is_taken_over() -> Result<(), Box<dyn Error>> {
        let root = test_root("dead-lock")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let settings = enabled_settings(wire::project_hash(&root)?);
        let mut ghost = std::process::Command::new("true").spawn()?;
        let ghost_pid = ghost.id();
        ghost.wait()?;
        let abandoned = AppLease {
            schema_version: SCHEMA_VERSION,
            protocol_generation: PROTOCOL_GENERATION,
            app_instance_id: opaque_hash(&[b"ghost"]),
            pid: ghost_pid,
            policy_generation: settings.policy_generation,
            issued_at_ms: 1_000,
            expires_at_ms: 31_000,
        };
        wire::atomic_write_json(&paths.app_lock_path(), &abandoned, WriteMode::CreateNew)?;

        let mut core = AgentBridgeCore::new(paths.clone(), opaque_hash(&[b"survivor"]))?;
        core.start(&settings, 1_001)?;
        assert_eq!(
            core.status(&settings).diagnostic,
            AgentBridgeDiagnostic::Active
        );
        assert_eq!(paths.read_lease()?.pid, process::id());
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn reply_preview_requires_exact_single_confirmation_before_delivery(
    ) -> Result<(), Box<dyn Error>> {
        let root = test_root("pending")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;

        let prompt = event(CanonicalEventKind::UserPromptSubmit, &root, None);
        persist_event(&paths, &app_id, binding.clone(), prompt, b"prompt", 1_001)?;
        core.tick(&settings, 1_002)?;
        let sessions = core.sessions();
        assert_eq!(sessions.len(), 1);
        let session_id = sessions[0].id.clone();
        let preview =
            core.create_reply_preview(&session_id, "continue".to_string(), 1_003, None)?;
        assert_eq!(preview.session_id, session_id);
        assert_eq!(preview.text, "continue");
        assert!(!preview.confirmed);

        let unconfirmed_stop = persist_event(
            &paths,
            &app_id,
            binding.clone(),
            event(CanonicalEventKind::Stop, &root, None),
            b"stop-unconfirmed",
            1_004,
        )?;
        core.tick(&settings, 1_005)?;
        let session_paths = paths.session(&unconfirmed_stop.binding)?;
        assert!(!session_paths
            .response_path(&unconfirmed_stop.invocation_id)?
            .exists());

        assert_eq!(
            core.confirm_reply_preview(&preview.id, "other-session", "continue", 1_006,),
            Err(AgentBridgeError::WrongDestination)
        );
        assert_eq!(
            core.confirm_reply_preview(&preview.id, &session_id, "different", 1_006),
            Err(AgentBridgeError::ConfirmationMismatch)
        );
        assert!(!core.pending_messages()[0].confirmed);

        let confirmed = core.confirm_reply_preview(&preview.id, &session_id, "continue", 1_006)?;
        assert!(confirmed.confirmed);
        assert_eq!(
            core.confirm_reply_preview(&preview.id, &session_id, "continue", 1_006),
            Err(AgentBridgeError::AlreadyHandled)
        );

        let stop_request = persist_event(
            &paths,
            &app_id,
            binding.clone(),
            event(CanonicalEventKind::Stop, &root, None),
            b"stop-confirmed",
            1_007,
        )?;
        core.tick(&settings, 1_008)?;
        let response: HookResponse = wire::read_json_bounded(
            &session_paths.response_path(&stop_request.invocation_id)?,
            wire::MAX_RESPONSE_BYTES,
        )?;
        assert_eq!(response.reason.as_deref(), Some("continue"));
        assert_eq!(
            core.pending_messages()[0].state,
            AgentBridgePendingState::ResponseWritten
        );

        let ack = HookAck::response_emitted(&stop_request, 1_009);
        session_paths.persist_ack(&ack)?;
        core.tick(&settings, 1_010)?;
        assert_eq!(
            core.pending_messages()
                .into_iter()
                .find(|item| item.id == preview.id)
                .map(|item| item.state),
            Some(AgentBridgePendingState::Emitted)
        );

        let replay = persist_event(
            &paths,
            &app_id,
            binding,
            event(CanonicalEventKind::Stop, &root, None),
            b"stop-replay",
            1_011,
        )?;
        core.tick(&settings, 1_012)?;
        assert!(!session_paths.response_path(&replay.invocation_id)?.exists());
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn stop_before_pending_cannot_bind_retroactively() -> Result<(), Box<dyn Error>> {
        let root = test_root("late")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-late"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let stop = event(CanonicalEventKind::Stop, &root, None);
        persist_event(&paths, &app_id, binding, stop, b"stop-first", 1_001)?;
        core.tick(&settings, 1_002)?;
        let session_id = core.sessions()[0].id.clone();
        assert_eq!(
            core.create_reply_preview(&session_id, "late".to_string(), 1_003, None),
            Err(AgentBridgeError::UnknownSession)
        );
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn expired_pending_becomes_copy_only() -> Result<(), Box<dyn Error>> {
        let root = test_root("expiry")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-expiry"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let prompt = event(CanonicalEventKind::UserPromptSubmit, &root, None);
        persist_event(&paths, &app_id, binding, prompt, b"prompt", 1_001)?;
        core.tick(&settings, 1_002)?;
        let session_id = core.sessions()[0].id.clone();
        core.create_reply_preview(&session_id, "copy me".to_string(), 1_003, Some(1))?;
        core.tick(&settings, 1_005)?;
        assert_eq!(
            core.pending_messages()[0].state,
            AgentBridgePendingState::CopyOnly
        );
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn cancelling_reply_preview_prevents_confirmation_and_delivery() -> Result<(), Box<dyn Error>> {
        let root = test_root("cancel")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-cancel"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;

        let prompt = event(CanonicalEventKind::UserPromptSubmit, &root, None);
        persist_event(&paths, &app_id, binding.clone(), prompt, b"prompt", 1_001)?;
        core.tick(&settings, 1_002)?;
        let session_id = core.sessions()[0].id.clone();
        let preview =
            core.create_reply_preview(&session_id, "cancel me".to_string(), 1_003, None)?;
        core.cancel_pending(&preview.id)?;
        assert_eq!(
            core.confirm_reply_preview(&preview.id, &session_id, "cancel me", 1_004),
            Err(AgentBridgeError::AlreadyHandled)
        );

        let stop_request = persist_event(
            &paths,
            &app_id,
            binding,
            event(CanonicalEventKind::Stop, &root, None),
            b"stop-cancelled",
            1_004,
        )?;
        core.tick(&settings, 1_005)?;
        assert!(!paths
            .session(&stop_request.binding)?
            .response_path(&stop_request.invocation_id)?
            .exists());
        assert_eq!(
            core.pending_messages()[0].state,
            AgentBridgePendingState::Cancelled
        );
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn response_write_failure_does_not_retry_confirmed_preview() -> Result<(), Box<dyn Error>> {
        let root = test_root("response-write-failure")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-response-write-failure"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;

        let prompt = event(CanonicalEventKind::UserPromptSubmit, &root, None);
        persist_event(&paths, &app_id, binding.clone(), prompt, b"prompt", 1_001)?;
        core.tick(&settings, 1_002)?;
        let session_id = core.sessions()[0].id.clone();
        let preview =
            core.create_reply_preview(&session_id, "one attempt".to_string(), 1_003, None)?;
        core.confirm_reply_preview(&preview.id, &session_id, "one attempt", 1_004)?;

        let stop_request = persist_event(
            &paths,
            &app_id,
            binding,
            event(CanonicalEventKind::Stop, &root, None),
            b"stop-write-failure",
            1_004,
        )?;
        core.persist_response(
            &stop_request,
            "block",
            Some("occupied response".to_string()),
            1_004,
        )?;
        assert_eq!(
            core.tick(&settings, 1_005),
            Err(AgentBridgeError::PersistenceFailed)
        );
        assert_eq!(
            core.pending_messages()[0].state,
            AgentBridgePendingState::Held
        );
        assert!(core.pending_messages()[0].confirmed);

        core.tick(&settings, 1_006)?;
        let response: HookResponse = wire::read_json_bounded(
            &paths
                .session(&stop_request.binding)?
                .response_path(&stop_request.invocation_id)?,
            wire::MAX_RESPONSE_BYTES,
        )?;
        assert_eq!(response.reason.as_deref(), Some("occupied response"));
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn omp_reply_requires_confirmation_and_stays_bound_to_one_project() -> Result<(), Box<dyn Error>>
    {
        let root = test_root("omp-reply")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"omp-app"]);
        let binding = omp_binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;

        persist_event(
            &paths,
            &app_id,
            binding.clone(),
            omp_event(CanonicalEventKind::SessionStart, &root, None),
            b"omp-session-start",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;
        assert_eq!(core.sessions()[0].agent, AgentBridgeAgent::Omp);

        persist_event(
            &paths,
            &app_id,
            binding.clone(),
            omp_event(CanonicalEventKind::UserPromptSubmit, &root, None),
            b"omp-prompt",
            1_003,
        )?;
        core.tick(&settings, 1_004)?;
        let session_id = core.sessions()[0].id.clone();
        let preview =
            core.create_reply_preview(&session_id, "OMP reply".to_string(), 1_005, None)?;
        assert_eq!(preview.agent, AgentBridgeAgent::Omp);
        assert!(!preview.confirmed);

        let unconfirmed_stop = persist_event(
            &paths,
            &app_id,
            binding.clone(),
            omp_event(CanonicalEventKind::Stop, &root, None),
            b"omp-stop-unconfirmed",
            1_006,
        )?;
        core.tick(&settings, 1_007)?;
        let session_paths = paths.session(&binding)?;
        assert!(!session_paths
            .response_path(&unconfirmed_stop.invocation_id)?
            .exists());

        core.confirm_reply_preview(&preview.id, &session_id, "OMP reply", 1_008)?;
        let other_project = root.join("other-project");
        fs::create_dir(&other_project)?;
        let other_binding = omp_binding(&other_project)?;
        let wrong_project_stop = persist_event(
            &paths,
            &app_id,
            other_binding,
            omp_event(CanonicalEventKind::Stop, &other_project, None),
            b"omp-stop-wrong-project",
            1_009,
        )?;
        core.tick(&settings, 1_010)?;
        assert!(!paths
            .session(&wrong_project_stop.binding)?
            .response_path(&wrong_project_stop.invocation_id)?
            .exists());

        let confirmed_stop = persist_event(
            &paths,
            &app_id,
            binding.clone(),
            omp_event(CanonicalEventKind::Stop, &root, None),
            b"omp-stop-confirmed",
            1_011,
        )?;
        core.tick(&settings, 1_012)?;
        let response: HookResponse = wire::read_json_bounded(
            &session_paths.response_path(&confirmed_stop.invocation_id)?,
            wire::MAX_RESPONSE_BYTES,
        )?;
        assert_eq!(response.reason.as_deref(), Some("OMP reply"));

        let replay = persist_event(
            &paths,
            &app_id,
            binding,
            omp_event(CanonicalEventKind::Stop, &root, None),
            b"omp-stop-replay",
            1_013,
        )?;
        core.tick(&settings, 1_014)?;
        assert!(!session_paths.response_path(&replay.invocation_id)?.exists());
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn omp_permission_requests_are_observe_only() -> Result<(), Box<dyn Error>> {
        let root = test_root("omp-permission")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"omp-permission-app"]);
        let binding = omp_binding(&root)?;
        let mut settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let tool_input = json!({"command": "pwd"});
        let request = persist_event(
            &paths,
            &app_id,
            binding,
            omp_event(
                CanonicalEventKind::PermissionRequest,
                &root,
                Some(CanonicalTool {
                    name: "bash".to_string(),
                    use_id: Some("omp-tool-1".to_string()),
                    input: Some(tool_input),
                }),
            ),
            b"omp-permission",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;
        assert_eq!(
            core.requests()[0].kind,
            AgentBridgeRequestKind::PermissionRequest
        );
        assert_eq!(core.requests()[0].agent, AgentBridgeAgent::Omp);
        assert_eq!(
            core.exact_rule_for_request(
                &request.invocation_id,
                "omp-rule".to_string(),
                AgentBridgePermissionDecision::Allow,
            ),
            Err(AgentBridgeError::PermissionResponseUnsupported)
        );

        settings.permission_rules.push(AgentBridgePermissionRule {
            id: "omp-rule".to_string(),
            agent: AgentBridgeAgent::Omp,
            canonical_project_hash: request.binding.project_hash.clone(),
            tool_name: "bash".to_string(),
            permission_mode: Some("always-ask".to_string()),
            tool_input_hash: "ignored".to_string(),
            decision: AgentBridgePermissionDecision::Allow,
            user_created: true,
        });
        assert_eq!(
            core.respond_permission(
                &request.invocation_id,
                "omp-rule",
                AgentBridgePermissionDecision::Allow,
                &settings,
                1_003,
            ),
            Err(AgentBridgeError::PermissionResponseUnsupported)
        );
        assert_eq!(settings.permission_rules.len(), 1);
        assert!(!paths
            .session(&request.binding)?
            .response_path(&request.invocation_id)?
            .exists());
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn permission_response_requires_exact_user_rule() -> Result<(), Box<dyn Error>> {
        let root = test_root("permission")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-permission"]);
        let binding = binding(&root)?;
        let mut settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let tool_input = json!({"command": "cargo test"});
        let tool_hash = opaque_hash(&[b"tool-input", &serde_json::to_vec(&tool_input)?]);
        let request = persist_event(
            &paths,
            &app_id,
            binding,
            event(
                CanonicalEventKind::PermissionRequest,
                &root,
                Some(CanonicalTool {
                    name: "Bash".to_string(),
                    use_id: None,
                    input: Some(tool_input),
                }),
            ),
            b"permission",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;
        assert_eq!(
            request.expires_at_ms.saturating_sub(request.issued_at_ms),
            REQUEST_TTL_MS
        );
        settings.permission_rules.push(AgentBridgePermissionRule {
            id: "rule-1".to_string(),
            agent: AgentBridgeAgent::Claude,
            canonical_project_hash: request.binding.project_hash.clone(),
            tool_name: "Bash".to_string(),
            permission_mode: Some("default".to_string()),
            tool_input_hash: tool_hash,
            decision: AgentBridgePermissionDecision::Allow,
            user_created: true,
        });
        core.respond_permission(
            &request.invocation_id,
            "rule-1",
            AgentBridgePermissionDecision::Allow,
            &settings,
            request.expires_at_ms,
        )?;
        let response: HookResponse = wire::read_json_bounded(
            &paths
                .session(&request.binding)?
                .response_path(&request.invocation_id)?,
            wire::MAX_RESPONSE_BYTES,
        )?;
        assert_eq!(response.outcome, "approve");
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// The app writes a response only where the hook is holding its agent open
    /// for one. Claude's pre-tool gate answers exactly two tools, so an ordinary
    /// tool call is observed and nothing else — a response for it would sit in
    /// the session directory until it expired.
    #[test]
    fn a_tool_call_with_no_reply_channel_cannot_be_answered() -> Result<(), Box<dyn Error>> {
        let root = test_root("unanswerable")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-unanswerable"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let request = persist_event(
            &paths,
            &app_id,
            binding,
            event(
                CanonicalEventKind::PreToolUse,
                &root,
                Some(CanonicalTool {
                    name: "Bash".to_string(),
                    use_id: Some("tool-1".to_string()),
                    input: Some(json!({"command": "cargo test"})),
                }),
            ),
            b"unanswerable",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;

        let observed = core.requests();
        assert!(!observed.iter().any(|request| request.awaiting_response));
        assert_eq!(
            core.exact_rule_for_request(
                &request.invocation_id,
                "rule-1".to_string(),
                AgentBridgePermissionDecision::Allow,
            )
            .unwrap_err(),
            AgentBridgeError::PermissionResponseUnsupported
        );
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// The setup code is pasted straight into `hooks.json`, so a wrong shape is
    /// a bridge that silently never fires. Codex and Grok both read Claude's
    /// matcher-group schema, and both run the handler through a shell.
    #[test]
    fn the_setup_code_matches_the_matcher_group_schema_both_agents_read() {
        let config = command_hook_config(
            &shell_invocation("/Applications/Sona.app/sona agent hook", "codex"),
            &["SessionStart", "Stop"],
        );
        assert_eq!(
            serde_json::to_value(config).expect("hook config serializes"),
            json!({
                "hooks": {
                    "SessionStart": [{
                        "hooks": [{
                            "type": "command",
                            "command": "\"/Applications/Sona.app/sona agent hook\" codex"
                        }]
                    }],
                    "Stop": [{
                        "hooks": [{
                            "type": "command",
                            "command": "\"/Applications/Sona.app/sona agent hook\" codex"
                        }]
                    }]
                }
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_request_is_ignored() -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::symlink;
        let root = test_root("symlink")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-symlink"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id)?;
        core.start(&settings, 1_000)?;
        let session = paths.session(&binding)?;
        let target = root.join("target.json");
        fs::write(&target, b"{}")?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
        symlink(&target, session.request_path(&opaque_hash(&[b"link"]))?)?;
        core.tick(&settings, 1_001)?;
        assert!(core.requests().is_empty());
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
