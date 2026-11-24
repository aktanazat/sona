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

mod questions;
pub use questions::{AgentBridgeQuestion, AgentBridgeQuestionOption, AgentBridgeQuestionResponse};

/// The app is the only publisher of the lease, heartbeat, and policy records,
/// so it owns their lifetimes. Republication must beat every one of them.
const HEARTBEAT_INTERVAL_MS: u64 = 15_000;
const APP_LEASE_TTL_MS: u64 = 30_000;
const HEARTBEAT_TTL_MS: u64 = 30_000;
const POLICY_TTL_MS: u64 = 30_000;
/// The worker sleeps on the wake socket until the core next has work of its
/// own ([`AgentBridgeCore::idle_wait`]); hooks and settings changes wake it
/// sooner. The socket's timeout runs only while the Mac is awake, so a wait
/// that spans a system sleep ends that much later in wall-clock time, with
/// the heartbeat already expired and hooks passing through until the next
/// tick republishes it. This cap bounds that window after every wake.
const MAX_IDLE_WAIT_MS: u64 = 5_000;
/// How soon a worker whose tick failed tries again. A lease it could not
/// read or a heartbeat it could not write has no deadline to wait for.
const TICK_RETRY: std::time::Duration = std::time::Duration::from_secs(1);
/// Held with an OS lock by the app that serves the bridge, beside the lease
/// hooks read. The kernel releases it when that process exits, however it
/// exits. Hooks never open it.
const OWNER_LOCK_FILE: &str = "app-owner.lock";
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
    Withdrawing,
    WithdrawalFailed,
    Responded,
    Delivered,
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
    InputRequest,
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
            Self::InputRequest => "input_request",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct AgentBridgeStatus {
    pub running: bool,
    pub diagnostic: AgentBridgeDiagnostic,
    pub policy_generation: u64,
    pub observed_sessions: usize,
    pub pending_messages: usize,
    /// Advances whenever anything the console lists changes: a request
    /// arriving, expiring or being answered, a session's turn opening or
    /// closing, a reply changing state. The worker announces a status only
    /// when it differs from the last one it announced, so this is what makes
    /// a second request in a session the console already knows reach it.
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct AgentBridgeObservedSession {
    pub id: String,
    pub agent: AgentBridgeAgent,
    pub canonical_project_hash: String,
    pub session_generation: u64,
    pub policy_generation: u64,
    pub last_seen_at_ms: u64,
    pub voice_enabled: bool,
    /// Legacy agents take a queued reply at their next turn end. OpenCode
    /// accepts replies only while its plugin holds an explicit input request.
    pub accepts_reply: bool,
    /// The task the session works on, as it named it: `S-12` or the task's
    /// id. Read from a prompt Sona wrote for it, a `sona work claim`
    /// command, or Sona's claim tool.
    pub task: Option<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct AgentBridgeObservedRequest {
    pub id: String,
    pub session_id: String,
    pub agent: AgentBridgeAgent,
    pub kind: AgentBridgeRequestKind,
    pub tool_name: Option<String>,
    /// The exact action an answer approves, bounded to one screenful and
    /// stripped of control characters. A shell tool shows its command line;
    /// any other tool shows its input as compact JSON. The full input stays
    /// bound to the rule through its hash.
    pub tool_input_preview: Option<String>,
    pub permission_mode: Option<String>,
    pub message: Option<String>,
    pub questions: Option<Vec<AgentBridgeQuestion>>,
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
    VoiceDisabled,
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
    InvalidAnswers,
    AlreadyHandled,
    Unauthorized,
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

impl ObservedRequestRecord {
    fn new(request: HookRequest, session_id: String) -> Self {
        let tool_input = request
            .event
            .tool
            .as_ref()
            .and_then(|tool| tool.input.as_ref());
        let tool_input_hash = tool_input
            .and_then(|input| serde_json::to_vec(input).ok())
            .map(|bytes| opaque_hash(&[b"tool-input", &bytes]))
            .unwrap_or_else(|| opaque_hash(&[b"tool-input-none"]));
        let public = AgentBridgeObservedRequest {
            id: request.invocation_id.clone(),
            session_id,
            agent: setting_agent(request.binding.agent),
            kind: request_kind(request.event.event),
            tool_name: request.event.tool_name().map(ToOwned::to_owned),
            tool_input_preview: tool_input.map(tool_input_preview),
            permission_mode: request.event.permission_mode.clone(),
            message: request.event.message.as_deref().map(visible_preview),
            questions: questions::from_event(&request.event),
            expires_at_ms: request.expires_at_ms,
            state: AgentBridgeRequestState::Observed,
            awaiting_response: request.event.awaits_response(),
        };
        Self {
            public,
            request,
            tool_input_hash,
        }
    }
}

enum ReplyTarget {
    Unavailable,
    NextStop,
    InputRequest(String),
}

struct ObservedSessionRecord {
    id: String,
    agent: AgentBridgeAgent,
    canonical_project_hash: String,
    session_generation: u64,
    policy_generation: u64,
    last_seen_at_ms: u64,
    voice_enabled: bool,
    task: Option<String>,
    reply_target: ReplyTarget,
    last_order: Option<wire::EventOrder>,
    last_reply_sequence: Option<u64>,
}

impl ObservedSessionRecord {
    fn close_input(&mut self, request_id: &str) -> bool {
        if matches!(&self.reply_target, ReplyTarget::InputRequest(id) if id == request_id) {
            self.reply_target = ReplyTarget::Unavailable;
            true
        } else {
            false
        }
    }
}

struct PendingMessageRecord {
    public: AgentBridgePendingMessage,
    binding: SessionBinding,
    response_invocation_id: Option<String>,
    target_invocation_id: Option<String>,
}

/// Whether this core serves the bridge. One app on the Mac serves at a time;
/// another app with the bridge on waits in `Standby` and takes over on its
/// next tick once the owner lock frees.
enum Stage {
    Off,
    Standby,
    Serving { _owner: fs::File },
}

pub struct AgentBridgeCore {
    paths: RuntimePaths,
    app_instance_id: String,
    stage: Stage,
    diagnostic: AgentBridgeDiagnostic,
    last_heartbeat_ms: u64,
    observed_sessions: BTreeMap<String, ObservedSessionRecord>,
    observed_requests: BTreeMap<String, ObservedRequestRecord>,
    pending_messages: BTreeMap<String, PendingMessageRecord>,
    /// See [`AgentBridgeStatus::revision`].
    revision: u64,
    next_id: u64,
    /// Sessions a prompt started on a task since the worker last asked: the
    /// agent, the session and the task the prompt named.
    started: Vec<(AgentBridgeAgent, String, String)>,
}

impl AgentBridgeCore {
    pub fn new(paths: RuntimePaths, app_instance_id: String) -> Result<Self, AgentBridgeError> {
        if !is_wire_id(&app_instance_id) {
            return Err(AgentBridgeError::RuntimeUnavailable);
        }
        Ok(Self {
            paths,
            app_instance_id,
            stage: Stage::Off,
            diagnostic: AgentBridgeDiagnostic::Disabled,
            last_heartbeat_ms: 0,
            observed_sessions: BTreeMap::new(),
            observed_requests: BTreeMap::new(),
            pending_messages: BTreeMap::new(),
            revision: 0,
            next_id: 0,
            started: Vec::new(),
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
        self.stage = Stage::Standby;
        self.serve(settings, now_ms)
    }

    /// Takes the owner lock and publishes this core's lease. While another
    /// app holds either, this core stays in standby.
    fn serve(
        &mut self,
        settings: &AgentBridgeSettings,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let owner = self.claim_owner_lock().inspect_err(|error| {
            self.diagnostic = match error {
                AgentBridgeError::AppLockHeld => AgentBridgeDiagnostic::AppLockHeld,
                _ => AgentBridgeDiagnostic::RuntimeUnavailable,
            };
        })?;
        // An app built before the owner lock publishes only the lease file.
        // Its lease blocks this one while that publisher is still running; a
        // killed app's unexpired record does not.
        if let Ok(existing) = self.paths.read_lease() {
            if existing.is_valid_at(now_ms)
                && existing.app_instance_id != self.app_instance_id
                && publisher_alive(existing.pid)
            {
                self.diagnostic = AgentBridgeDiagnostic::AppLockHeld;
                return Err(AgentBridgeError::AppLockHeld);
            }
        }
        self.write_control_state(settings, now_ms)?;
        self.stage = Stage::Serving { _owner: owner };
        self.diagnostic = AgentBridgeDiagnostic::Active;
        Ok(())
    }

    fn claim_owner_lock(&self) -> Result<fs::File, AgentBridgeError> {
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options
            .open(self.paths.app_lock_path().with_file_name(OWNER_LOCK_FILE))
            .map_err(|_| AgentBridgeError::RuntimeUnavailable)?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(fs::TryLockError::WouldBlock) => Err(AgentBridgeError::AppLockHeld),
            Err(fs::TryLockError::Error(_)) => Err(AgentBridgeError::RuntimeUnavailable),
        }
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
        let mut changed = false;
        for session in self.observed_sessions.values_mut() {
            if session.agent == AgentBridgeAgent::OpenCode
                && !matches!(session.reply_target, ReplyTarget::Unavailable)
            {
                session.reply_target = ReplyTarget::Unavailable;
                changed = true;
            }
        }
        for pending in self.pending_messages.values_mut() {
            if pending.public.agent == AgentBridgeAgent::OpenCode
                && matches!(
                    pending.public.state,
                    AgentBridgePendingState::Held | AgentBridgePendingState::ResponseWritten
                )
            {
                pending.public.state = AgentBridgePendingState::CopyOnly;
                changed = true;
            }
        }
        for record in self.observed_requests.values_mut() {
            if record.public.agent == AgentBridgeAgent::OpenCode
                && record.public.state == AgentBridgeRequestState::Observed
            {
                record.public.state = AgentBridgeRequestState::Dismissed;
                changed = true;
            }
        }
        if changed {
            self.touch();
        }
        self.stage = Stage::Off;
        self.diagnostic = AgentBridgeDiagnostic::Disabled;
    }

    pub fn tick(
        &mut self,
        settings: &AgentBridgeSettings,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        if !settings.master_enabled {
            return Err(AgentBridgeError::Disabled);
        }
        match self.stage {
            Stage::Off => return Err(AgentBridgeError::Disabled),
            Stage::Standby => self.serve(settings, now_ms)?,
            Stage::Serving { .. } => {}
        }
        let republish = match self.paths.read_lease() {
            Ok(lease) if lease.app_instance_id != self.app_instance_id => {
                self.stage = Stage::Standby;
                self.diagnostic = AgentBridgeDiagnostic::AppLockHeld;
                return Err(AgentBridgeError::AppLockHeld);
            }
            // The lease is ours, so a generation it does not carry can only
            // mean the settings moved under a running worker. Hooks read the
            // projection with that generation, so it is republished at once
            // rather than on the heartbeat.
            Ok(lease) => lease.policy_generation != settings.policy_generation,
            // This core holds the owner lock, so a lease that went missing
            // or unreadable is still its own to write.
            Err(_) => true,
        };
        if republish || now_ms.saturating_sub(self.last_heartbeat_ms) >= HEARTBEAT_INTERVAL_MS {
            self.write_control_state(settings, now_ms)?;
        }
        self.expire_pending(now_ms);
        self.scan_sessions(settings, now_ms)?;
        let withdrawal = self.withdraw_ungranted(settings, now_ms);
        self.deliver_input_replies(settings, now_ms)?;
        withdrawal
    }

    /// How long the worker can sleep before this core has work of its own:
    /// the heartbeat falling due, or a held reply or an unanswered request
    /// passing its deadline (each expires once `now` is past it). Meant for
    /// the moment after a successful tick, when all of those lie ahead.
    fn idle_wait(&self, now_ms: u64) -> std::time::Duration {
        let held = self
            .pending_messages
            .values()
            .filter(|pending| pending.public.state == AgentBridgePendingState::Held)
            .map(|pending| pending.public.expires_at_ms);
        let unanswered = self
            .observed_requests
            .values()
            .filter(|record| record.public.state == AgentBridgeRequestState::Observed)
            .map(|record| record.public.expires_at_ms);
        let due_ms = held
            .chain(unanswered)
            .map(|expires_at_ms| expires_at_ms.saturating_add(1))
            .fold(
                self.last_heartbeat_ms.saturating_add(HEARTBEAT_INTERVAL_MS),
                u64::min,
            );
        // A socket refuses a zero timeout, so an overdue deadline gets the
        // shortest one it takes.
        std::time::Duration::from_millis(due_ms.saturating_sub(now_ms).clamp(1, MAX_IDLE_WAIT_MS))
    }

    pub fn status(&self, settings: &AgentBridgeSettings) -> AgentBridgeStatus {
        AgentBridgeStatus {
            running: matches!(self.stage, Stage::Serving { .. }),
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
            revision: self.revision,
        }
    }

    pub fn sessions(&self) -> Vec<AgentBridgeObservedSession> {
        self.observed_sessions
            .values()
            .map(|session| AgentBridgeObservedSession {
                id: session.id.clone(),
                agent: session.agent,
                canonical_project_hash: session.canonical_project_hash.clone(),
                session_generation: session.session_generation,
                policy_generation: session.policy_generation,
                last_seen_at_ms: session.last_seen_at_ms,
                voice_enabled: session.voice_enabled,
                accepts_reply: session.voice_enabled
                    && !matches!(session.reply_target, ReplyTarget::Unavailable),
                task: session.task.clone(),
            })
            .collect()
    }

    /// See [`AgentBridgeCore::started`].
    pub fn take_started(&mut self) -> Vec<(AgentBridgeAgent, String, String)> {
        std::mem::take(&mut self.started)
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

    pub fn set_session_voice(
        &mut self,
        session_id: &str,
        enabled: bool,
    ) -> Result<(), AgentBridgeError> {
        let session = self
            .observed_sessions
            .get_mut(session_id)
            .ok_or(AgentBridgeError::UnknownSession)?;
        if session.voice_enabled == enabled {
            return Ok(());
        }
        session.voice_enabled = enabled;
        if !enabled {
            session.reply_target = ReplyTarget::Unavailable;
            for pending in self.pending_messages.values_mut() {
                if pending.public.session_id == session_id
                    && pending.public.state == AgentBridgePendingState::Held
                {
                    pending.public.state = AgentBridgePendingState::CopyOnly;
                }
            }
            for record in self.observed_requests.values_mut() {
                if record.public.session_id == session_id
                    && record.public.state == AgentBridgeRequestState::Observed
                {
                    record.public.state = if record.public.awaiting_response {
                        AgentBridgeRequestState::Withdrawing
                    } else {
                        AgentBridgeRequestState::Dismissed
                    };
                }
            }
        }
        self.touch();
        Ok(())
    }

    fn ensure_request_open(&self, record: &ObservedRequestRecord) -> Result<(), AgentBridgeError> {
        if record.public.state == AgentBridgeRequestState::Observed {
            return Ok(());
        }
        if matches!(
            record.public.state,
            AgentBridgeRequestState::Withdrawing | AgentBridgeRequestState::WithdrawalFailed
        ) && self
            .observed_sessions
            .get(&record.public.session_id)
            .is_some_and(|session| !session.voice_enabled)
        {
            Err(AgentBridgeError::VoiceDisabled)
        } else {
            Err(AgentBridgeError::AlreadyHandled)
        }
    }

    pub fn create_reply_preview(
        &mut self,
        session_id: &str,
        text: String,
        now_ms: u64,
        ttl_ms: Option<u64>,
    ) -> Result<AgentBridgePendingMessage, AgentBridgeError> {
        self.expire_pending(now_ms);
        if text.trim().is_empty() {
            return Err(AgentBridgeError::EmptyMessage);
        }
        let session = self
            .observed_sessions
            .get(session_id)
            .ok_or(AgentBridgeError::UnknownSession)?;
        if !session.voice_enabled {
            return Err(AgentBridgeError::VoiceDisabled);
        }
        let target_invocation_id = match &session.reply_target {
            ReplyTarget::Unavailable => return Err(AgentBridgeError::UnknownSession),
            ReplyTarget::NextStop => None,
            ReplyTarget::InputRequest(id) => Some(id.clone()),
        };
        let agent = session.agent;
        let mut binding = session_binding(session);
        if self.pending_messages.values().any(|pending| {
            pending.public.session_id == session_id
                && matches!(pending.public.state, AgentBridgePendingState::Held)
        }) {
            return Err(AgentBridgeError::DuplicatePending);
        }
        if let Some(target) = &target_invocation_id {
            let request = self
                .observed_requests
                .get(target)
                .ok_or(AgentBridgeError::UnknownSession)?;
            if request.public.state != AgentBridgeRequestState::Observed
                || request.request.expires_at_ms < now_ms
            {
                return Err(AgentBridgeError::Expired);
            }
            binding = request.request.binding.clone();
        }
        let id = self.next_opaque_id(b"pending", session_id.as_bytes(), now_ms);
        let pending = PendingMessageRecord {
            public: AgentBridgePendingMessage {
                id: id.clone(),
                agent,
                session_id: session_id.to_string(),
                text,
                expires_at_ms: now_ms.saturating_add(ttl_ms.unwrap_or(DEFAULT_PENDING_TTL_MS)),
                state: AgentBridgePendingState::Held,
                confirmed: false,
            },
            binding,
            response_invocation_id: None,
            target_invocation_id,
        };
        let public = pending.public.clone();
        self.pending_messages.insert(id, pending);
        self.touch();
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
            self.touch();
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
        let public = pending.public.clone();
        self.touch();
        Ok(public)
    }

    pub fn cancel_pending(&mut self, pending_id: &str) -> Result<(), AgentBridgeError> {
        let pending = self
            .pending_messages
            .get_mut(pending_id)
            .ok_or(AgentBridgeError::UnknownRequest)?;
        match pending.public.state {
            AgentBridgePendingState::Held | AgentBridgePendingState::CopyOnly => {
                pending.public.state = AgentBridgePendingState::Cancelled;
                self.touch();
                Ok(())
            }
            _ => Err(AgentBridgeError::AlreadyHandled),
        }
    }

    pub fn dismiss_request(
        &mut self,
        request_id: &str,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let record = self
            .observed_requests
            .get(request_id)
            .ok_or(AgentBridgeError::UnknownRequest)?;
        if record.public.state != AgentBridgeRequestState::Observed {
            return Err(AgentBridgeError::AlreadyHandled);
        }
        if record.request.binding.agent == Agent::OpenCode && record.public.awaiting_response {
            self.persist_response(&record.request, "pass_through", None, None, now_ms)?;
        }
        self.close_input_request(request_id);
        if let Some(record) = self.observed_requests.get_mut(request_id) {
            record.public.state = AgentBridgeRequestState::Dismissed;
        }
        self.touch();
        Ok(())
    }

    pub fn exact_rule_for_request(
        &self,
        request_id: &str,
        rule_id: String,
        decision: AgentBridgePermissionDecision,
        settings: &AgentBridgeSettings,
    ) -> Result<AgentBridgePermissionRule, AgentBridgeError> {
        let record = self
            .observed_requests
            .get(request_id)
            .ok_or(AgentBridgeError::UnknownRequest)?;
        if !record.public.awaiting_response || record.public.agent == AgentBridgeAgent::OpenCode {
            return Err(AgentBridgeError::PermissionResponseUnsupported);
        }
        if !policy_grants(settings, &record.request.binding) {
            return Err(AgentBridgeError::Unauthorized);
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
        rule_id: Option<&str>,
        decision: AgentBridgePermissionDecision,
        settings: &AgentBridgeSettings,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let record = self
            .observed_requests
            .get(request_id)
            .ok_or(AgentBridgeError::UnknownRequest)?;
        if !record.public.awaiting_response
            || record.request.event.is_opencode_question()
            || !matches!(
                record.public.kind,
                AgentBridgeRequestKind::PermissionRequest | AgentBridgeRequestKind::PreToolUse
            )
        {
            return Err(AgentBridgeError::PermissionResponseUnsupported);
        }

        self.ensure_request_open(record)?;
        if record.request.expires_at_ms < now_ms {
            return Err(AgentBridgeError::Expired);
        }
        if !policy_grants(settings, &record.request.binding) {
            return Err(AgentBridgeError::Unauthorized);
        }
        if let Some(rule_id) = rule_id {
            let rule = settings
                .permission_rules
                .iter()
                .find(|rule| rule.id == rule_id && rule.user_created)
                .ok_or(AgentBridgeError::RuleRequired)?;
            if !rule_matches(rule, record, decision) {
                return Err(AgentBridgeError::RuleMismatch);
            }
        } else if record.public.agent != AgentBridgeAgent::OpenCode {
            return Err(AgentBridgeError::RuleRequired);
        }
        let outcome = match decision {
            AgentBridgePermissionDecision::Allow => "approve",
            AgentBridgePermissionDecision::Deny => "reject",
        };
        self.persist_response(&record.request, outcome, None, None, now_ms)?;
        if let Some(request) = self.observed_requests.get_mut(request_id) {
            request.public.state = AgentBridgeRequestState::Responded;
        }
        self.touch();
        Ok(())
    }

    pub fn respond_questions(
        &mut self,
        request_id: &str,
        response: AgentBridgeQuestionResponse,
        settings: &AgentBridgeSettings,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let record = self
            .observed_requests
            .get(request_id)
            .ok_or(AgentBridgeError::UnknownRequest)?;
        if !record.public.awaiting_response || !record.request.event.is_opencode_question() {
            return Err(AgentBridgeError::PermissionResponseUnsupported);
        }
        self.ensure_request_open(record)?;
        if record.request.expires_at_ms < now_ms {
            return Err(AgentBridgeError::Expired);
        }
        if !policy_grants(settings, &record.request.binding) {
            return Err(AgentBridgeError::Unauthorized);
        }
        let (decision, answers) = match response {
            AgentBridgeQuestionResponse::Answer(answers) => {
                let questions = record
                    .public
                    .questions
                    .as_deref()
                    .ok_or(AgentBridgeError::InvalidAnswers)?;
                if !questions::answers_match(questions, &answers) {
                    return Err(AgentBridgeError::InvalidAnswers);
                }
                let answers = questions
                    .iter()
                    .zip(answers)
                    .map(|(question, selected)| wire::HookAnswer {
                        header: question.header.clone(),
                        question: question.question.clone(),
                        selected,
                    })
                    .collect();
                ("approve", Some(answers))
            }
            AgentBridgeQuestionResponse::Reject => ("reject", None),
        };
        self.persist_response(&record.request, decision, None, answers, now_ms)?;
        if let Some(record) = self.observed_requests.get_mut(request_id) {
            record.public.state = AgentBridgeRequestState::Responded;
        }
        self.touch();
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
            if (request.expires_at_ms < now_ms
                && (request.binding.agent != Agent::OpenCode
                    || !request_has_receipt(&self.paths, &request)))
                || (!self.observed_requests.contains_key(&request.invocation_id)
                    && !request_peer_alive(&request))
            {
                remove_private_file(&path);
                if let Ok(session) = self.paths.session(&request.binding) {
                    if let Ok(ack_path) = session.ack_path(&request.invocation_id) {
                        remove_private_file(&ack_path);
                    }
                    if let Ok(response) = session.response_path(&request.invocation_id) {
                        remove_private_file(&response);
                    }
                    if let Ok(claimed) = session.claimed_path(&request.invocation_id) {
                        remove_private_file(&claimed);
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
            if ack.app_instance_id != self.app_instance_id {
                continue;
            }
            let binding = match self.observed_requests.get(&ack.invocation_id) {
                Some(record) if ack_matches_request(&ack, &record.request) => {
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
                if ack.outcome == "request_cancelled" {
                    if let Ok(response) = session.response_path(&ack.invocation_id) {
                        remove_private_file(&response);
                    }
                    if let Ok(claimed) = session.claimed_path(&ack.invocation_id) {
                        remove_private_file(&claimed);
                    }
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
            && (request.binding.policy_generation == settings.policy_generation
                || (request.binding.agent == Agent::OpenCode
                    && request.binding.policy_generation < settings.policy_generation))
            && policy_grants(settings, &request.binding)
            && request_peer_alive(request)
    }

    fn withdraw_ungranted(
        &mut self,
        settings: &AgentBridgeSettings,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let before = self.observed_requests.len();
        self.observed_requests.retain(|_, record| {
            record.public.agent == AgentBridgeAgent::OpenCode
                || record.public.state != AgentBridgeRequestState::Observed
                || policy_grants(settings, &record.request.binding)
        });
        let mut changed = self.observed_requests.len() != before;
        let mut result = Ok(());
        for record in self.observed_requests.values_mut() {
            if matches!(
                record.public.state,
                AgentBridgeRequestState::Withdrawing | AgentBridgeRequestState::WithdrawalFailed
            ) {
                let state = match persist_hook_response(
                    &self.paths,
                    &self.app_instance_id,
                    &record.request,
                    "pass_through",
                    None,
                    None,
                    now_ms,
                ) {
                    Ok(()) => AgentBridgeRequestState::Dismissed,
                    Err(error) => {
                        result = Err(error);
                        AgentBridgeRequestState::WithdrawalFailed
                    }
                };
                if record.public.state != state {
                    record.public.state = state;
                    changed = true;
                }
            } else if record.public.state == AgentBridgeRequestState::Observed
                && record.public.agent == AgentBridgeAgent::OpenCode
                && !policy_grants(settings, &record.request.binding)
            {
                record.public.state = AgentBridgeRequestState::Dismissed;
                changed = true;
            }
        }
        for session in self.observed_sessions.values_mut() {
            if session.agent == AgentBridgeAgent::OpenCode
                && !matches!(session.reply_target, ReplyTarget::Unavailable)
                && !(settings.master_enabled
                    && settings.agent_enabled(session.agent)
                    && settings.allows_project_hash(&session.canonical_project_hash))
            {
                session.reply_target = ReplyTarget::Unavailable;
                changed = true;
            }
        }
        for pending in self.pending_messages.values_mut() {
            if pending.public.agent == AgentBridgeAgent::OpenCode
                && matches!(
                    pending.public.state,
                    AgentBridgePendingState::Held | AgentBridgePendingState::ResponseWritten
                )
                && !policy_grants(settings, &pending.binding)
            {
                pending.public.state = AgentBridgePendingState::CopyOnly;
                changed = true;
            }
        }
        if changed {
            self.touch();
        }
        result
    }

    fn observe_request(
        &mut self,
        request: HookRequest,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let agent = setting_agent(request.binding.agent);
        let session_id = request.binding.session_handle.clone();
        let kind = request_kind(request.event.event);
        let previous = self.observed_sessions.get(&session_id);
        let previous_order = previous.and_then(|session| session.last_order.as_ref());
        let order = request.event.order.as_ref();
        let same_instance = order.zip(previous_order).is_some_and(|(order, previous)| {
            order.instance_id == previous.instance_id
                && order.instance_started_at_ms == previous.instance_started_at_ms
        });
        let begins_instance = order.is_some() && !same_instance;
        let reply_transition = matches!(
            kind,
            AgentBridgeRequestKind::UserPromptSubmit
                | AgentBridgeRequestKind::InputRequest
                | AgentBridgeRequestKind::Stop
        );
        let advances_order = match (order, previous_order) {
            (Some(order), Some(previous)) => event_follows(order, previous),
            _ => true,
        };
        let admitted = match (order, previous_order) {
            (Some(order), Some(_)) if same_instance => {
                !reply_transition
                    || previous
                        .and_then(|session| session.last_reply_sequence)
                        .is_none_or(|sequence| order.sequence > sequence)
            }
            (Some(_), Some(_)) => advances_order,
            _ => true,
        };
        if !admitted {
            if request.event.awaits_response() {
                self.persist_response(&request, "pass_through", None, None, now_ms)?;
            }
            let mut record = ObservedRequestRecord::new(request, session_id);
            record.public.state = AgentBridgeRequestState::Dismissed;
            self.observed_requests
                .insert(record.public.id.clone(), record);
            self.touch();
            return Ok(());
        }
        if agent == AgentBridgeAgent::OpenCode
            && matches!(
                kind,
                AgentBridgeRequestKind::UserPromptSubmit | AgentBridgeRequestKind::InputRequest
            )
        {
            let previous =
                self.observed_sessions.get(&session_id).and_then(|session| {
                    match &session.reply_target {
                        ReplyTarget::InputRequest(id) => Some(id.clone()),
                        _ => None,
                    }
                });
            if let Some(id) = previous {
                if self
                    .observed_requests
                    .get(&id)
                    .is_some_and(|record| record.public.state == AgentBridgeRequestState::Observed)
                {
                    self.dismiss_request(&id, now_ms)?;
                }
            }
        }
        let session = self
            .observed_sessions
            .entry(session_id.clone())
            .or_insert_with(|| ObservedSessionRecord {
                id: session_id.clone(),
                agent,
                canonical_project_hash: request.binding.project_hash.clone(),
                session_generation: request.binding.session_generation,
                policy_generation: request.binding.policy_generation,
                last_seen_at_ms: now_ms,
                voice_enabled: true,
                task: None,
                reply_target: ReplyTarget::Unavailable,
                last_order: None,
                last_reply_sequence: None,
            });
        session.last_seen_at_ms = now_ms;
        if begins_instance {
            session.reply_target = ReplyTarget::Unavailable;
            session.last_reply_sequence = None;
        }
        if reply_transition {
            session.last_reply_sequence = order.map(|order| order.sequence);
        }
        if advances_order {
            session.session_generation = request.binding.session_generation;
            session.policy_generation = request.binding.policy_generation;
            session.last_order = request.event.order.clone();
        }
        if session.voice_enabled {
            match kind {
                AgentBridgeRequestKind::UserPromptSubmit => {
                    session.reply_target = if agent == AgentBridgeAgent::OpenCode {
                        ReplyTarget::Unavailable
                    } else {
                        ReplyTarget::NextStop
                    };
                }
                AgentBridgeRequestKind::Stop => session.reply_target = ReplyTarget::Unavailable,
                AgentBridgeRequestKind::InputRequest => {
                    session.reply_target = ReplyTarget::InputRequest(request.invocation_id.clone());
                }
                _ => {}
            }
        }
        if advances_order {
            let started = request.event.prompt.as_deref().and_then(prompt_task);
            if let Some(task) = started
                .clone()
                .or_else(|| request.event.tool.as_ref().and_then(claimed_task))
            {
                session.task = Some(task);
            }
            if let Some(task) = started {
                self.started.push((agent, session_id.clone(), task));
            }
        }
        for pending in self.pending_messages.values_mut() {
            if pending.public.session_id == session_id
                && pending.public.state == AgentBridgePendingState::Held
            {
                if agent != AgentBridgeAgent::OpenCode {
                    pending.binding = session_binding(session);
                } else if reply_transition || begins_instance {
                    pending.public.state = AgentBridgePendingState::CopyOnly;
                }
            }
        }
        let voice_enabled = session.voice_enabled;
        let awaiting_response = request.event.awaits_response();
        let request_id = request.invocation_id.clone();
        let mut record = ObservedRequestRecord::new(request, session_id);
        if !voice_enabled {
            record.public.state = if awaiting_response {
                AgentBridgeRequestState::Withdrawing
            } else {
                AgentBridgeRequestState::Dismissed
            };
        }
        self.observed_requests.insert(request_id.clone(), record);
        self.touch();
        if voice_enabled && kind == AgentBridgeRequestKind::Stop && awaiting_response {
            self.respond_to_reply_request(&request_id, now_ms)?;
        }
        Ok(())
    }

    fn respond_to_reply_request(
        &mut self,
        request_id: &str,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        let record = self
            .observed_requests
            .get(request_id)
            .ok_or(AgentBridgeError::UnknownRequest)?;
        let request = &record.request;
        let pending_id = self.pending_messages.iter().find_map(|(id, pending)| {
            (pending.public.confirmed
                && pending.public.state == AgentBridgePendingState::Held
                && pending.binding == request.binding
                && pending.public.expires_at_ms >= now_ms
                && pending
                    .target_invocation_id
                    .as_deref()
                    .is_none_or(|id| id == request_id))
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
        self.persist_response(request, "block", Some(text), None, now_ms)?;
        if let Some(pending) = self.pending_messages.get_mut(&pending_id) {
            pending.public.state = AgentBridgePendingState::ResponseWritten;
            pending.response_invocation_id = Some(request_id.to_string());
            if request.binding.agent == Agent::OpenCode {
                pending.public.expires_at_ms = request.expires_at_ms;
            }
        }
        if let Some(session) = self.observed_sessions.get_mut(&record.public.session_id) {
            session.close_input(request_id);
        }
        if let Some(observed) = self.observed_requests.get_mut(request_id) {
            observed.public.state = AgentBridgeRequestState::Responded;
        }
        self.touch();
        Ok(())
    }

    fn deliver_input_replies(
        &mut self,
        settings: &AgentBridgeSettings,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        while let Some(request_id) = self.pending_messages.values().find_map(|pending| {
            if pending.public.state != AgentBridgePendingState::Held || !pending.public.confirmed {
                return None;
            }
            let id = pending.target_invocation_id.as_ref()?;
            let record = self.observed_requests.get(id)?;
            let session = self.observed_sessions.get(&pending.public.session_id)?;
            (record.public.state == AgentBridgeRequestState::Observed
                && record.request.expires_at_ms >= now_ms
                && pending.public.expires_at_ms >= now_ms
                && pending.binding == record.request.binding
                && policy_grants(settings, &record.request.binding)
                && matches!(&session.reply_target, ReplyTarget::InputRequest(target) if target == id))
                .then(|| id.clone())
        }) {
            self.respond_to_reply_request(&request_id, now_ms)?;
        }
        Ok(())
    }

    fn close_input_request(&mut self, request_id: &str) -> bool {
        let mut changed = false;
        if let Some(record) = self.observed_requests.get(request_id) {
            if let Some(session) = self.observed_sessions.get_mut(&record.public.session_id) {
                changed |= session.close_input(request_id);
            }
        }
        for pending in self.pending_messages.values_mut() {
            if (pending.target_invocation_id.as_deref() == Some(request_id)
                || pending.response_invocation_id.as_deref() == Some(request_id))
                && matches!(
                    pending.public.state,
                    AgentBridgePendingState::Held | AgentBridgePendingState::ResponseWritten
                )
            {
                pending.public.state = AgentBridgePendingState::CopyOnly;
                changed = true;
            }
        }
        changed
    }

    fn persist_response(
        &self,
        request: &HookRequest,
        outcome: &str,
        reason: Option<String>,
        answers: Option<Vec<wire::HookAnswer>>,
        now_ms: u64,
    ) -> Result<(), AgentBridgeError> {
        if outcome != "pass_through"
            && self
                .observed_sessions
                .get(&request.binding.session_handle)
                .is_some_and(|session| !session.voice_enabled)
        {
            return Err(AgentBridgeError::VoiceDisabled);
        }
        persist_hook_response(
            &self.paths,
            &self.app_instance_id,
            request,
            outcome,
            reason,
            answers,
            now_ms,
        )
    }

    fn apply_ack(&mut self, ack: &HookAck) {
        let mut changed = false;
        if ack.outcome == "request_cancelled" {
            changed |= self.close_input_request(&ack.invocation_id);
            if let Some(record) = self.observed_requests.get_mut(&ack.invocation_id) {
                record.public.state = AgentBridgeRequestState::Dismissed;
                changed = true;
            }
        }
        if ack.binding.agent == Agent::OpenCode && ack.outcome == "response_emitted" {
            if let Some(record) = self.observed_requests.get_mut(&ack.invocation_id) {
                record.public.state = AgentBridgeRequestState::Delivered;
                changed = true;
            }
        }
        for pending in self.pending_messages.values_mut() {
            if pending.response_invocation_id.as_deref() == Some(&ack.invocation_id)
                && pending.binding == ack.binding
                && (pending.public.state == AgentBridgePendingState::ResponseWritten
                    || (ack.binding.agent == Agent::OpenCode
                        && ack.outcome == "response_emitted"
                        && matches!(
                            pending.public.state,
                            AgentBridgePendingState::CopyOnly | AgentBridgePendingState::Cancelled
                        )))
            {
                pending.public.state = AgentBridgePendingState::Emitted;
                changed = true;
            }
        }
        if changed {
            self.touch();
        }
    }

    fn expire_pending(&mut self, now_ms: u64) {
        let mut changed = false;
        for observed in self.observed_requests.values_mut() {
            let open_code = observed.public.agent == AgentBridgeAgent::OpenCode;
            if !(matches!(
                observed.public.state,
                AgentBridgeRequestState::Observed
                    | AgentBridgeRequestState::Withdrawing
                    | AgentBridgeRequestState::WithdrawalFailed
            ) || (open_code && observed.public.state == AgentBridgeRequestState::Responded))
            {
                continue;
            }
            let abandoned = open_code && !request_peer_alive(&observed.request);
            if observed.public.expires_at_ms >= now_ms && !abandoned {
                continue;
            }
            if open_code && request_has_receipt(&self.paths, &observed.request) {
                continue;
            }
            observed.public.state = AgentBridgeRequestState::Expired;
            if let Some(session) = self.observed_sessions.get_mut(&observed.public.session_id) {
                session.close_input(&observed.public.id);
            }
            changed = true;
        }
        for pending in self.pending_messages.values_mut() {
            let open_code = pending.public.agent == AgentBridgeAgent::OpenCode;
            let written =
                open_code && pending.public.state == AgentBridgePendingState::ResponseWritten;
            if pending.public.state != AgentBridgePendingState::Held && !written {
                continue;
            }
            let target_closed = open_code
                && pending
                    .target_invocation_id
                    .as_ref()
                    .and_then(|id| self.observed_requests.get(id))
                    .is_none_or(|record| {
                        matches!(
                            record.public.state,
                            AgentBridgeRequestState::Dismissed
                                | AgentBridgeRequestState::Expired
                                | AgentBridgeRequestState::Withdrawing
                                | AgentBridgeRequestState::WithdrawalFailed
                        )
                    });
            if pending.public.expires_at_ms >= now_ms && !target_closed {
                continue;
            }
            if written
                && pending
                    .response_invocation_id
                    .as_ref()
                    .and_then(|id| self.observed_requests.get(id))
                    .is_some_and(|record| request_has_receipt(&self.paths, &record.request))
            {
                continue;
            }
            pending.public.state = AgentBridgePendingState::CopyOnly;
            changed = true;
        }
        if changed {
            self.touch();
        }
    }

    /// Marks a change the console can see. See [`AgentBridgeStatus::revision`].
    fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
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

fn persist_hook_response(
    paths: &RuntimePaths,
    app_instance_id: &str,
    request: &HookRequest,
    outcome: &str,
    reason: Option<String>,
    answers: Option<Vec<wire::HookAnswer>>,
    now_ms: u64,
) -> Result<(), AgentBridgeError> {
    let response = HookResponse {
        schema_version: SCHEMA_VERSION,
        protocol_generation: PROTOCOL_GENERATION,
        app_instance_id: app_instance_id.to_owned(),
        binding: request.binding.clone(),
        invocation_id: request.invocation_id.clone(),
        outcome: outcome.to_owned(),
        issued_at_ms: now_ms,
        expires_at_ms: request.expires_at_ms,
        reason,
        answers,
    };
    let session = paths
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

fn request_peer_alive(request: &HookRequest) -> bool {
    request.binding.agent != Agent::OpenCode
        || !request.event.awaits_response()
        || (request.publisher_pid > 0
            && i32::try_from(request.publisher_pid).is_ok()
            && publisher_alive(request.publisher_pid))
}

fn event_follows(order: &wire::EventOrder, previous: &wire::EventOrder) -> bool {
    if order.instance_id == previous.instance_id
        && order.instance_started_at_ms == previous.instance_started_at_ms
    {
        order.sequence > previous.sequence
    } else {
        (order.instance_started_at_ms, &order.instance_id)
            > (previous.instance_started_at_ms, &previous.instance_id)
    }
}

fn ack_matches_request(ack: &HookAck, request: &HookRequest) -> bool {
    ack.schema_version == SCHEMA_VERSION
        && ack.protocol_generation == PROTOCOL_GENERATION
        && ack.app_instance_id == request.app_instance_id
        && ack.binding == request.binding
        && ack.invocation_id == request.invocation_id
        && request.issued_at_ms <= ack.emitted_at_ms
        && ack.emitted_at_ms <= request.expires_at_ms
        && (ack.outcome == "response_emitted"
            || (ack.binding.agent == Agent::OpenCode && ack.outcome == "request_cancelled"))
}

fn request_has_receipt(paths: &RuntimePaths, request: &HookRequest) -> bool {
    let Ok(session) = paths.session(&request.binding) else {
        return false;
    };
    let Ok(path) = session.ack_path(&request.invocation_id) else {
        return false;
    };
    wire::read_json_bounded::<HookAck>(&path, MAX_ACK_BYTES)
        .is_ok_and(|ack| ack_matches_request(&ack, request))
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
        (AgentBridgeAgent::OpenCode, Agent::OpenCode),
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
        AgentBridgeAgent::OpenCode => Agent::OpenCode,
    }
}

fn setting_agent(agent: Agent) -> AgentBridgeAgent {
    match agent {
        Agent::Claude => AgentBridgeAgent::Claude,
        Agent::Codex => AgentBridgeAgent::Codex,
        Agent::Grok => AgentBridgeAgent::Grok,
        Agent::Omp => AgentBridgeAgent::Omp,
        Agent::OpenCode => AgentBridgeAgent::OpenCode,
    }
}

fn setting_agent_enabled(settings: &AgentBridgeSettings, agent: Agent) -> bool {
    settings.agent_enabled(setting_agent(agent))
}

/// The standing grants a request needs: the master switch, its agent, and
/// its project. Checked when a request is observed and again when it is
/// answered, because a grant removed in between withdraws the answer. The
/// policy generation is not part of this on purpose: saving the exact rule
/// an answer needs advances it.
fn policy_grants(settings: &AgentBridgeSettings, binding: &SessionBinding) -> bool {
    settings.master_enabled
        && setting_agent_enabled(settings, binding.agent)
        && settings.allows_project_hash(&binding.project_hash)
}

/// The wire binding a reply for this session must match at its stop.
fn session_binding(session: &ObservedSessionRecord) -> SessionBinding {
    SessionBinding {
        agent: wire_agent(session.agent),
        session_handle: session.id.clone(),
        project_hash: session.canonical_project_hash.clone(),
        session_generation: session.session_generation,
        policy_generation: session.policy_generation,
    }
}

/// One screenful: enough to read a command or an edit, not a transcript.
const TOOL_INPUT_PREVIEW_CHARS: usize = 400;

/// See [`AgentBridgeObservedRequest::tool_input_preview`].
fn tool_input_preview(input: &serde_json::Value) -> String {
    match input {
        serde_json::Value::Object(fields) => match fields.get("command") {
            Some(serde_json::Value::String(command)) => visible_preview(command),
            _ => visible_preview(&input.to_string()),
        },
        serde_json::Value::String(text) => visible_preview(text),
        other => visible_preview(&other.to_string()),
    }
}

fn visible_preview(text: &str) -> String {
    let mut visible = text.chars().map(|ch| {
        if ch == '\n' || !ch.is_control() {
            ch
        } else {
            ' '
        }
    });
    let mut preview: String = visible.by_ref().take(TOOL_INPUT_PREVIEW_CHARS).collect();
    if visible.next().is_some() {
        preview.push('…');
    }
    let leading = preview.len() - preview.trim_start().len();
    preview.drain(..leading);
    preview.truncate(preview.trim_end().len());
    preview
}

/// The task a prompt Sona wrote names after its mark.
fn prompt_task(prompt: &str) -> Option<String> {
    let (_, rest) = prompt.split_once(crate::planning::work::agents::PROMPT_MARK)?;
    rest.split(|character: char| character == ':' || character.is_whitespace())
        .find(|token| !token.is_empty())
        .and_then(task_ref)
}

/// The task a `sona work claim` command or Sona's claim tool claims.
fn claimed_task(tool: &wire::CanonicalTool) -> Option<String> {
    let input = tool.input.as_ref()?;
    if tool.name.ends_with("sona_work_claim") {
        return input.get("task")?.as_str().and_then(task_ref);
    }
    let command = input.get("command")?.as_str()?;
    let (program, rest) = command.split_once(" work claim ")?;
    if !program.trim_end_matches(['"', '\'']).ends_with("sona") {
        return None;
    }
    let mut tokens = rest.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "--operation-id" {
            tokens.next();
        } else if let Some(task) = task_ref(token) {
            return Some(task);
        }
    }
    None
}

/// `S-12`, `#12` and `12` as `S-12`; an item id as itself.
fn task_ref(token: &str) -> Option<String> {
    let token = token.trim_matches(['"', '\'']);
    let number = token
        .strip_prefix("S-")
        .or_else(|| token.strip_prefix("s-"))
        .or_else(|| token.strip_prefix('#'))
        .unwrap_or(token);
    if !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()) {
        return number
            .parse::<i64>()
            .ok()
            .map(|number| format!("S-{number}"));
    }
    uuid::Uuid::parse_str(token).ok().map(|id| id.to_string())
}

/// The name a bridged agent goes by on the tasks it works.
fn doer_name(agent: AgentBridgeAgent) -> &'static str {
    match agent {
        AgentBridgeAgent::Claude => "Claude Code",
        AgentBridgeAgent::Codex => "Codex",
        AgentBridgeAgent::Grok => "Grok",
        AgentBridgeAgent::Omp => "OMP",
        AgentBridgeAgent::OpenCode => "OpenCode",
    }
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
        CanonicalEventKind::InputRequest => AgentBridgeRequestKind::InputRequest,
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
}

impl WakeListener {
    #[cfg(unix)]
    fn bind(path: &Path) -> io::Result<Self> {
        // A killed app leaves its socket file behind; the lease already
        // proves nobody else is listening, so the stale entry is replaced.
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let socket = std::os::unix::net::UnixDatagram::bind(path)?;
        Ok(Self { socket })
    }

    #[cfg(not(unix))]
    fn bind(_path: &Path) -> io::Result<Self> {
        Ok(Self {})
    }

    /// Blocks until a writer wakes the listener or `timeout` elapses, and
    /// says which one happened. The timeout is set per wait because the
    /// worker's next deadline moves with every tick.
    fn wait(&self, timeout: std::time::Duration) -> bool {
        #[cfg(unix)]
        {
            // Receiving without the timeout could block past the heartbeat,
            // so a socket that refuses it still gets the wait, just not the
            // early wake.
            if self.socket.set_read_timeout(Some(timeout)).is_err() {
                std::thread::sleep(timeout);
                return false;
            }
            let mut byte = [0u8; 1];
            self.socket.recv(&mut byte).is_ok()
        }
        #[cfg(not(unix))]
        {
            std::thread::sleep(timeout);
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
            // The running worker reads the settings on every tick; waking it
            // puts a new policy generation in front of hooks at once instead
            // of at its next deadline.
            wire::send_wake(&lock_recover(&self.core).paths.wake_path());
            return Ok(());
        }
        let wake_path = {
            let mut core = lock_recover(&self.core);
            match core.start(&settings, now_ms()) {
                Ok(()) | Err(AgentBridgeError::AppLockHeld) => {}
                Err(error) => return Err(error.to_string()),
            }
            core.paths.wake_path()
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
            let mut previous: Option<AgentBridgeStatus> = None;
            let mut recorded_workflow_requests = BTreeSet::new();
            // The wake socket belongs to whichever app serves, so a core in
            // standby leaves it alone and polls.
            let mut listener: Option<WakeListener> = None;
            while !worker_stop.load(Ordering::Acquire) {
                let settings = crate::settings::get_settings(&app).agent_bridge;
                if !settings.master_enabled {
                    break;
                }
                let (status, requests, started, wait) = {
                    let mut core = lock_recover(&core);
                    let now = now_ms();
                    let wait = match core.tick(&settings, now) {
                        Ok(()) => core.idle_wait(now),
                        Err(_) => TICK_RETRY,
                    };
                    (
                        core.status(&settings),
                        core.requests(),
                        core.take_started(),
                        wait,
                    )
                };
                let was_serving = previous.as_ref().is_some_and(|status| status.running);
                if !status.running {
                    listener = None;
                } else if listener.is_none() {
                    listener = WakeListener::bind(&wake_path)
                        .inspect_err(|error| {
                            if !was_serving {
                                log::warn!("Agent wake socket unavailable: {error}");
                            }
                        })
                        .ok();
                }
                for (agent, session_id, task) in started {
                    crate::planning::work::agents::follow_session(
                        &app,
                        doer_name(agent),
                        &session_id,
                        &task,
                    );
                }
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
                // The status carries the revision, so a request landing in a
                // session the console already lists, or a row expiring, is a
                // difference here and not a change nobody announces.
                if previous.as_ref() != Some(&status) {
                    let _ = app.emit(
                        AgentBridgeUpdateEvent::NAME,
                        AgentBridgeUpdateEvent {
                            status: status.clone(),
                        },
                    );
                    previous = Some(status);
                }
                match &listener {
                    Some(listener) => {
                        listener.wait(wait);
                    }
                    None => std::thread::sleep(wait),
                }
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
pub fn set_agent_bridge_session_voice(
    manager: tauri::State<'_, AgentBridgeManager>,
    session_id: String,
    enabled: bool,
) -> Result<(), String> {
    let mut core = lock_recover(&manager.core);
    core.set_session_voice(&session_id, enabled)
        .map_err(|error| error.to_string())?;
    wire::send_wake(&core.paths.wake_path());
    Ok(())
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
    let mut core = lock_recover(&manager.core);
    let pending = core
        .confirm_reply_preview(&pending_id, &session_id, &text, now_ms())
        .map_err(|error| error.to_string())?;
    wire::send_wake(&core.paths.wake_path());
    Ok(pending)
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
        .dismiss_request(&request_id, now_ms())
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
    let settings = crate::settings::get_settings(&app).agent_bridge;
    let rule = lock_recover(&manager.core)
        .exact_rule_for_request(&request_id, rule_id, decision, &settings)
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
    rule_id: Option<String>,
    decision: AgentBridgePermissionDecision,
) -> Result<(), String> {
    let settings = crate::settings::get_settings(&app).agent_bridge;
    lock_recover(&manager.core)
        .respond_permission(
            &request_id,
            rule_id.as_deref(),
            decision,
            &settings,
            now_ms(),
        )
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn respond_agent_bridge_questions(
    app: tauri::AppHandle,
    manager: tauri::State<'_, AgentBridgeManager>,
    request_id: String,
    response: AgentBridgeQuestionResponse,
) -> Result<(), String> {
    let settings = crate::settings::get_settings(&app).agent_bridge;
    lock_recover(&manager.core)
        .respond_questions(&request_id, response, &settings, now_ms())
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
    let opencode_package = app
        .path()
        .resolve(
            "resources/agent-bridge/opencode",
            tauri::path::BaseDirectory::Resource,
        )
        .map_err(|_| "OpenCode extension path unavailable".to_string())?;
    let opencode_package = opencode_package
        .to_str()
        .ok_or_else(|| "OpenCode extension path is not UTF-8".to_string())?;
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
        },
        "opencode": {
            "plugins": [{"package": opencode_package}]
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

    fn enabled_settings(project_hash: String) -> AgentBridgeSettings {
        AgentBridgeSettings {
            master_enabled: true,
            claude_enabled: true,
            codex_enabled: true,
            grok_enabled: true,
            omp_enabled: true,
            opencode_enabled: true,
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
            order: None,
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
            order: None,
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

    fn opencode_event(
        kind: CanonicalEventKind,
        project: &Path,
        sequence: u64,
        tool: Option<CanonicalTool>,
    ) -> CanonicalEvent {
        let mut event = event(kind, project, tool);
        event.agent = Agent::OpenCode;
        event.permission_mode = None;
        event.order = Some(wire::EventOrder {
            instance_id: "opencode-test-instance".to_string(),
            instance_started_at_ms: 999,
            sequence,
        });
        event.request_id = event
            .awaits_response()
            .then(|| format!("opencode-request-{sequence}"));
        event
    }

    struct VoiceTestRoot(PathBuf);

    impl Drop for VoiceTestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct VoiceFixture {
        core: AgentBridgeCore,
        paths: RuntimePaths,
        settings: AgentBridgeSettings,
        app_id: String,
        root: VoiceTestRoot,
        now: u64,
    }

    impl VoiceFixture {
        fn new(name: &str) -> Result<Self, Box<dyn Error>> {
            let root = VoiceTestRoot(test_root(name)?);
            let paths = RuntimePaths::from_root(root.0.clone(), true)?;
            let app_id = opaque_hash(&[name.as_bytes()]);
            let binding = SessionBinding::new(Agent::OpenCode, "first", &root.0, 4, 7)?;
            let settings = enabled_settings(binding.project_hash);
            let mut fixture = Self {
                core: AgentBridgeCore::new(paths.clone(), app_id.clone())?,
                paths,
                settings,
                app_id,
                root,
                now: 1_000,
            };
            fixture.core.start(&fixture.settings, fixture.now)?;
            Ok(fixture)
        }

        fn tick(&mut self) -> Result<(), AgentBridgeError> {
            self.now += 1;
            self.core.tick(&self.settings, self.now)
        }

        fn publish(
            &mut self,
            agent: Agent,
            session_id: &str,
            kind: CanonicalEventKind,
            tool: Option<CanonicalTool>,
        ) -> Result<HookRequest, Box<dyn Error>> {
            self.now += 1;
            let mut event = if agent == Agent::OpenCode {
                opencode_event(kind, &self.root.0, self.now, tool)
            } else {
                event(kind, &self.root.0, tool)
            };
            event.agent = agent;
            event.session_id = session_id.to_owned();
            let raw = serde_json::to_vec(&event)?;
            let binding = SessionBinding::new(agent, session_id, &self.root.0, 4, 7)?;
            let request = persist_event(&self.paths, &self.app_id, binding, event, &raw, self.now)?;
            self.tick()?;
            Ok(request)
        }

        fn question(&mut self, session_id: &str) -> Result<HookRequest, Box<dyn Error>> {
            self.publish(
                Agent::OpenCode,
                session_id,
                CanonicalEventKind::PreToolUse,
                Some(CanonicalTool {
                    name: wire::ASK_USER_QUESTION_TOOL.to_owned(),
                    use_id: None,
                    input: Some(json!({"questions": [{
                        "header": "Reply", "question": "What should be sent?",
                        "options": [], "multi_select": false,
                        "custom": true, "required": true,
                    }]})),
                }),
            )
        }

        fn response(&self, request: &HookRequest) -> io::Result<HookResponse> {
            wire::read_json_bounded(
                &self
                    .paths
                    .session(&request.binding)?
                    .response_path(&request.invocation_id)?,
                wire::MAX_RESPONSE_BYTES,
            )
        }
    }

    #[test]
    fn failed_cancellation_does_not_stop_a_reply_in_another_conversation(
    ) -> Result<(), Box<dyn Error>> {
        let mut fixture = VoiceFixture::new("voice-failure-isolation")?;
        let question = fixture.question("first")?;
        let input = fixture.publish(
            Agent::OpenCode,
            "second",
            CanonicalEventKind::InputRequest,
            None,
        )?;
        let preview = fixture.core.create_reply_preview(
            &input.binding.session_handle,
            "The other conversation still works.".to_owned(),
            fixture.now,
            None,
        )?;
        fixture.core.confirm_reply_preview(
            &preview.id,
            &preview.session_id,
            &preview.text,
            fixture.now,
        )?;
        let obstacle = fixture
            .paths
            .session(&question.binding)?
            .response_path(&question.invocation_id)?;
        fs::create_dir(obstacle)?;
        fixture
            .core
            .set_session_voice(&question.binding.session_handle, false)?;
        assert_eq!(fixture.tick(), Err(AgentBridgeError::PersistenceFailed));
        let response = fixture.response(&input);
        assert!(
            response.is_ok(),
            "Another conversation's confirmed reply was stalled: {response:?}"
        );
        assert_eq!(
            response?.reason.as_deref(),
            Some("The other conversation still works.")
        );
        Ok(())
    }

    #[test]
    fn failed_cancellation_cannot_revive_an_old_question_after_reenabling(
    ) -> Result<(), Box<dyn Error>> {
        let mut fixture = VoiceFixture::new("voice-failure-resume")?;
        let question = fixture.question("first")?;
        let obstacle = fixture
            .paths
            .session(&question.binding)?
            .response_path(&question.invocation_id)?;
        fs::create_dir(&obstacle)?;
        fixture
            .core
            .set_session_voice(&question.binding.session_handle, false)?;
        assert_eq!(fixture.tick(), Err(AgentBridgeError::PersistenceFailed));
        fs::remove_dir(obstacle)?;
        fixture
            .core
            .set_session_voice(&question.binding.session_handle, true)?;
        assert_eq!(
            fixture.core.respond_questions(
                &question.invocation_id,
                AgentBridgeQuestionResponse::Answer(vec![vec!["Do not revive this.".to_owned()]]),
                &fixture.settings,
                fixture.now,
            ),
            Err(AgentBridgeError::AlreadyHandled),
        );
        Ok(())
    }

    #[test]
    fn failed_cancellation_is_visible_on_its_own_request() -> Result<(), Box<dyn Error>> {
        let mut fixture = VoiceFixture::new("voice-failure-visible")?;
        let question = fixture.question("first")?;
        let obstacle = fixture
            .paths
            .session(&question.binding)?
            .response_path(&question.invocation_id)?;
        fs::create_dir(obstacle)?;
        fixture
            .core
            .set_session_voice(&question.binding.session_handle, false)?;
        assert_eq!(fixture.tick(), Err(AgentBridgeError::PersistenceFailed));
        let request = fixture
            .core
            .requests()
            .into_iter()
            .find(|request| request.id == question.invocation_id)
            .ok_or("The failed request disappeared")?;
        assert_eq!(
            serde_json::to_value(request)?["state"],
            json!("withdrawal_failed")
        );
        Ok(())
    }

    #[test]
    fn paused_conversation_does_not_send_its_held_reply_or_pause_another(
    ) -> Result<(), Box<dyn Error>> {
        let mut fixture = VoiceFixture::new("voice-isolation")?;
        let first = fixture.publish(
            Agent::OpenCode,
            "first",
            CanonicalEventKind::InputRequest,
            None,
        )?;
        let second = fixture.publish(
            Agent::OpenCode,
            "second",
            CanonicalEventKind::InputRequest,
            None,
        )?;
        let held = fixture.core.create_reply_preview(
            &first.binding.session_handle,
            "Do not send after pausing.".to_owned(),
            fixture.now,
            None,
        )?;
        fixture
            .core
            .confirm_reply_preview(&held.id, &held.session_id, &held.text, fixture.now)?;
        let other = fixture.core.create_reply_preview(
            &second.binding.session_handle,
            "Keep the other conversation running.".to_owned(),
            fixture.now,
            None,
        )?;
        fixture.core.confirm_reply_preview(
            &other.id,
            &other.session_id,
            &other.text,
            fixture.now,
        )?;
        fixture
            .core
            .set_session_voice(&first.binding.session_handle, false)?;
        assert_eq!(
            fixture
                .core
                .confirm_reply_preview(&held.id, &held.session_id, &held.text, fixture.now),
            Err(AgentBridgeError::AlreadyHandled),
        );
        assert!(matches!(
            fixture.core.create_reply_preview(
                &first.binding.session_handle,
                "A new reply must not escape.".to_owned(),
                fixture.now,
                None,
            ),
            Err(AgentBridgeError::VoiceDisabled),
        ));
        fixture.tick()?;
        assert_eq!(fixture.response(&first)?.outcome, "pass_through");
        let response = fixture.response(&second)?;
        assert_eq!(response.outcome, "block");
        assert_eq!(
            response.reason.as_deref(),
            Some("Keep the other conversation running.")
        );
        assert_eq!(response.binding, second.binding);
        Ok(())
    }

    #[test]
    fn paused_questions_pass_through_and_resume_requires_a_fresh_question(
    ) -> Result<(), Box<dyn Error>> {
        let mut fixture = VoiceFixture::new("voice-question")?;
        let first = fixture.question("first")?;
        fixture
            .core
            .set_session_voice(&first.binding.session_handle, false)?;
        assert_eq!(
            fixture.core.respond_questions(
                &first.invocation_id,
                AgentBridgeQuestionResponse::Answer(vec![vec!["Do not send.".to_owned()]]),
                &fixture.settings,
                fixture.now,
            ),
            Err(AgentBridgeError::VoiceDisabled),
        );
        fixture.tick()?;
        assert_eq!(fixture.response(&first)?.outcome, "pass_through");
        let muted = fixture.question("first")?;
        assert_eq!(fixture.response(&muted)?.outcome, "pass_through");
        fixture
            .core
            .set_session_voice(&first.binding.session_handle, true)?;
        assert_eq!(
            fixture.core.respond_questions(
                &first.invocation_id,
                AgentBridgeQuestionResponse::Reject,
                &fixture.settings,
                fixture.now,
            ),
            Err(AgentBridgeError::AlreadyHandled),
        );
        let fresh = fixture.question("first")?;
        fixture.core.respond_questions(
            &fresh.invocation_id,
            AgentBridgeQuestionResponse::Answer(vec![vec!["Keep this exact answer.".to_owned()]]),
            &fixture.settings,
            fixture.now,
        )?;
        let response = fixture.response(&fresh)?;
        assert_eq!(response.outcome, "approve");
        assert_eq!(
            response
                .answers
                .as_deref()
                .map(|answers| answers[0].selected.as_slice()),
            Some(["Keep this exact answer.".to_owned()].as_slice()),
        );
        Ok(())
    }

    #[test]
    fn pausing_keeps_an_already_written_reply_until_its_receipt() -> Result<(), Box<dyn Error>> {
        let mut fixture = VoiceFixture::new("voice-written")?;
        let input = fixture.publish(
            Agent::OpenCode,
            "first",
            CanonicalEventKind::InputRequest,
            None,
        )?;
        let preview = fixture.core.create_reply_preview(
            &input.binding.session_handle,
            "Already handed off.".to_owned(),
            fixture.now,
            None,
        )?;
        fixture.core.confirm_reply_preview(
            &preview.id,
            &preview.session_id,
            &preview.text,
            fixture.now,
        )?;
        fixture.tick()?;
        fixture
            .core
            .set_session_voice(&input.binding.session_handle, false)?;
        fixture.tick()?;
        assert_eq!(
            fixture
                .core
                .pending_messages()
                .iter()
                .find(|pending| pending.id == preview.id)
                .map(|pending| pending.state),
            Some(AgentBridgePendingState::ResponseWritten),
        );
        fixture
            .paths
            .session(&input.binding)?
            .persist_ack(&HookAck::response_emitted(&input, fixture.now))?;
        fixture.tick()?;
        assert_eq!(
            fixture
                .core
                .pending_messages()
                .iter()
                .find(|pending| pending.id == preview.id)
                .map(|pending| pending.state),
            Some(AgentBridgePendingState::Emitted),
        );
        assert_eq!(
            fixture.response(&input)?.reason.as_deref(),
            Some("Already handed off.")
        );
        Ok(())
    }

    #[test]
    fn cancellation_write_failure_keeps_voice_paused_until_pending_work_passes_through(
    ) -> Result<(), Box<dyn Error>> {
        let mut fixture = VoiceFixture::new("voice-write-failure")?;
        let question = fixture.question("first")?;
        let response_path = fixture
            .paths
            .session(&question.binding)?
            .response_path(&question.invocation_id)?;
        fs::create_dir(&response_path)?;
        fixture
            .core
            .set_session_voice(&question.binding.session_handle, false)?;
        assert_eq!(fixture.tick(), Err(AgentBridgeError::PersistenceFailed));
        assert_eq!(
            fixture.core.respond_questions(
                &question.invocation_id,
                AgentBridgeQuestionResponse::Reject,
                &fixture.settings,
                fixture.now,
            ),
            Err(AgentBridgeError::VoiceDisabled),
        );
        fs::remove_dir(response_path)?;
        fixture.tick()?;
        let response = fixture.response(&question)?;
        assert_eq!(response.outcome, "pass_through");
        assert!(response.reason.is_none());
        assert!(response.answers.is_none());
        Ok(())
    }

    #[test]
    fn paused_legacy_conversation_passes_through_without_losing_another_reply(
    ) -> Result<(), Box<dyn Error>> {
        let mut fixture = VoiceFixture::new("voice-legacy")?;
        let first = fixture.publish(
            Agent::Omp,
            "first",
            CanonicalEventKind::UserPromptSubmit,
            None,
        )?;
        let second = fixture.publish(
            Agent::Omp,
            "second",
            CanonicalEventKind::UserPromptSubmit,
            None,
        )?;
        let preview = fixture.core.create_reply_preview(
            &second.binding.session_handle,
            "Only the second conversation.".to_owned(),
            fixture.now,
            None,
        )?;
        fixture.core.confirm_reply_preview(
            &preview.id,
            &preview.session_id,
            &preview.text,
            fixture.now,
        )?;
        fixture
            .core
            .set_session_voice(&first.binding.session_handle, false)?;
        let first_stop = fixture.publish(Agent::Omp, "first", CanonicalEventKind::Stop, None)?;
        let second_stop = fixture.publish(Agent::Omp, "second", CanonicalEventKind::Stop, None)?;
        assert_eq!(fixture.response(&first_stop)?.outcome, "pass_through");
        let response = fixture.response(&second_stop)?;
        assert_eq!(response.outcome, "block");
        assert_eq!(
            response.reason.as_deref(),
            Some("Only the second conversation.")
        );
        assert_eq!(response.binding, second_stop.binding);
        Ok(())
    }

    #[test]
    fn opencode_independent_permission_survives_a_later_notification() -> Result<(), Box<dyn Error>>
    {
        let root = test_root("opencode-independent-permission")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"opencode-independent-permission"]);
        let binding = SessionBinding::new(Agent::OpenCode, "provider-session", &root, 4, 7)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let mut notice = opencode_event(CanonicalEventKind::Notification, &root, 2, None);
        notice.message = Some("Another status update".to_string());
        persist_event(
            &paths,
            &app_id,
            binding.clone(),
            notice,
            b"newer notice",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;
        let permission = persist_event(
            &paths,
            &app_id,
            binding,
            opencode_event(
                CanonicalEventKind::PermissionRequest,
                &root,
                1,
                Some(CanonicalTool {
                    name: "shell".to_string(),
                    use_id: None,
                    input: Some(json!({"resources": ["fixture"]})),
                }),
            ),
            b"older independent permission",
            1_003,
        )?;
        core.tick(&settings, 1_004)?;
        let requests = core.requests();
        let observed = requests
            .iter()
            .find(|request| request.id == permission.invocation_id)
            .ok_or("permission was not observed")?;
        assert_eq!(observed.state, AgentBridgeRequestState::Observed);
        assert!(observed.awaiting_response);
        assert!(!paths
            .session(&permission.binding)?
            .response_path(&permission.invocation_id)?
            .exists());
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn opencode_permission_can_be_answered_once_without_a_legacy_rule() -> Result<(), Box<dyn Error>>
    {
        let root = test_root("opencode-once")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"opencode-once"]);
        let binding = SessionBinding::new(Agent::OpenCode, "provider-session", &root, 4, 7)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let tool = CanonicalTool {
            name: "shell".to_string(),
            use_id: None,
            input: Some(json!({"resources": ["fixture"]})),
        };
        let first = persist_event(
            &paths,
            &app_id,
            binding.clone(),
            opencode_event(
                CanonicalEventKind::PermissionRequest,
                &root,
                1,
                Some(tool.clone()),
            ),
            b"first permission",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;
        core.respond_permission(
            &first.invocation_id,
            None,
            AgentBridgePermissionDecision::Allow,
            &settings,
            1_003,
        )?;
        let response: HookResponse = wire::read_json_bounded(
            &paths
                .session(&first.binding)?
                .response_path(&first.invocation_id)?,
            wire::MAX_RESPONSE_BYTES,
        )?;
        assert_eq!(response.outcome, "approve");
        let second = persist_event(
            &paths,
            &app_id,
            binding,
            opencode_event(
                CanonicalEventKind::PermissionRequest,
                &root,
                2,
                Some(tool.clone()),
            ),
            b"second identical permission",
            1_004,
        )?;
        core.tick(&settings, 1_005)?;
        assert!(!paths
            .session(&second.binding)?
            .response_path(&second.invocation_id)?
            .exists());
        let legacy = persist_event(
            &paths,
            &app_id,
            self::binding(&root)?,
            event(CanonicalEventKind::PermissionRequest, &root, Some(tool)),
            b"legacy permission still needs a rule",
            1_006,
        )?;
        core.tick(&settings, 1_007)?;
        assert_eq!(
            core.respond_permission(
                &legacy.invocation_id,
                None,
                AgentBridgePermissionDecision::Allow,
                &settings,
                1_008
            ),
            Err(AgentBridgeError::RuleRequired),
        );
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn opencode_permission_cannot_create_a_persistent_rule() -> Result<(), Box<dyn Error>> {
        let root = test_root("opencode-no-saved-rule")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"opencode-no-saved-rule"]);
        let binding = SessionBinding::new(Agent::OpenCode, "provider-session", &root, 4, 7)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let permission = persist_event(
            &paths,
            &app_id,
            binding,
            opencode_event(
                CanonicalEventKind::PermissionRequest,
                &root,
                1,
                Some(CanonicalTool {
                    name: "shell".to_string(),
                    use_id: None,
                    input: Some(json!({"resources": ["fixture"]})),
                }),
            ),
            b"OpenCode permission must not become a saved rule",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;
        let rule = core.exact_rule_for_request(
            &permission.invocation_id,
            "not-saved".to_string(),
            AgentBridgePermissionDecision::Allow,
            &settings,
        );
        core.stop();
        fs::remove_dir_all(root)?;
        assert!(
            matches!(rule, Err(AgentBridgeError::PermissionResponseUnsupported)),
            "OpenCode produced a persistent permission rule: {rule:?}",
        );
        Ok(())
    }

    #[test]
    fn opencode_input_request_cannot_be_answered_as_a_permission() -> Result<(), Box<dyn Error>> {
        let root = test_root("opencode-input-not-permission")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"opencode-input-not-permission"]);
        let binding = SessionBinding::new(Agent::OpenCode, "provider-session", &root, 4, 7)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let input = persist_event(
            &paths,
            &app_id,
            binding,
            opencode_event(CanonicalEventKind::InputRequest, &root, 1, None),
            b"input is not permission",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;
        assert_eq!(
            core.respond_permission(
                &input.invocation_id,
                None,
                AgentBridgePermissionDecision::Allow,
                &settings,
                1_003
            ),
            Err(AgentBridgeError::PermissionResponseUnsupported),
        );
        assert!(!paths
            .session(&input.binding)?
            .response_path(&input.invocation_id)?
            .exists());
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn opencode_question_is_delivered_only_after_its_matching_receipt() -> Result<(), Box<dyn Error>>
    {
        let root = test_root("opencode-question-receipt")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"opencode-question-receipt"]);
        let binding = SessionBinding::new(Agent::OpenCode, "provider-session", &root, 4, 7)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let question = persist_event(
            &paths,
            &app_id,
            binding,
            opencode_event(
                CanonicalEventKind::PreToolUse,
                &root,
                1,
                Some(CanonicalTool {
                    name: wire::ASK_USER_QUESTION_TOOL.to_string(),
                    use_id: None,
                    input: Some(json!({"questions": [{
                        "header": "Route", "question": "Which route?",
                        "options": [{"label": "Keep", "description": null}],
                        "multi_select": false, "custom": false, "required": true,
                    }]})),
                }),
            ),
            b"question awaiting receipt",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;
        core.respond_questions(
            &question.invocation_id,
            AgentBridgeQuestionResponse::Reject,
            &settings,
            1_003,
        )?;
        let request_state =
            |core: &AgentBridgeCore| -> Result<AgentBridgeRequestState, Box<dyn Error>> {
                core.requests()
                    .iter()
                    .find(|request| request.id == question.invocation_id)
                    .map(|request| request.state)
                    .ok_or_else(|| "question disappeared".into())
            };
        assert_eq!(request_state(&core)?, AgentBridgeRequestState::Responded);
        let session = paths.session(&question.binding)?;
        let mut wrong = HookAck::response_emitted(&question, 1_004);
        wrong.binding.policy_generation += 1;
        session.persist_ack(&wrong)?;
        core.tick(&settings, 1_005)?;
        assert_eq!(request_state(&core)?, AgentBridgeRequestState::Responded);
        fs::remove_file(session.ack_path(&question.invocation_id)?)?;
        session.persist_ack(&HookAck::response_emitted(&question, 1_006))?;
        core.tick(&settings, 1_007)?;
        assert_eq!(request_state(&core)?, AgentBridgeRequestState::Delivered);
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn opencode_reply_after_a_newer_notification_reaches_its_input() -> Result<(), Box<dyn Error>> {
        let root = test_root("opencode-reordered-input")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"opencode-reordered-input"]);
        let binding = SessionBinding::new(Agent::OpenCode, "provider-session", &root, 4, 7)?;
        let session_id = binding.session_handle.clone();
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let mut notice = opencode_event(CanonicalEventKind::Notification, &root, 2, None);
        notice.message = Some("An independent status".to_string());
        persist_event(
            &paths,
            &app_id,
            binding.clone(),
            notice,
            b"newer input status",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;
        let input = persist_event(
            &paths,
            &app_id,
            binding,
            opencode_event(CanonicalEventKind::InputRequest, &root, 1, None),
            b"earlier live input",
            1_003,
        )?;
        core.tick(&settings, 1_004)?;
        let preview = core.create_reply_preview(
            &session_id,
            "Keep this destination".to_string(),
            1_005,
            None,
        )?;
        core.confirm_reply_preview(&preview.id, &session_id, &preview.text, 1_006)?;
        core.tick(&settings, 1_007)?;
        let response: HookResponse = wire::read_json_bounded(
            &paths
                .session(&input.binding)?
                .response_path(&input.invocation_id)?,
            wire::MAX_RESPONSE_BYTES,
        )?;
        assert_eq!(response.binding, input.binding);
        assert_eq!(response.outcome, "block");
        assert_eq!(response.reason.as_deref(), Some("Keep this destination"));
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn opencode_reply_preview_survives_an_independent_request() -> Result<(), Box<dyn Error>> {
        let root = test_root("opencode-preview-independent-request")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"opencode-preview-independent-request"]);
        let binding = SessionBinding::new(Agent::OpenCode, "provider-session", &root, 4, 7)?;
        let session_id = binding.session_handle.clone();
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let input = persist_event(
            &paths,
            &app_id,
            binding.clone(),
            opencode_event(CanonicalEventKind::InputRequest, &root, 1, None),
            b"preview destination",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;
        let preview = core.create_reply_preview(
            &session_id,
            "Preserve these words".to_string(),
            1_003,
            None,
        )?;
        persist_event(
            &paths,
            &app_id,
            binding,
            opencode_event(
                CanonicalEventKind::PermissionRequest,
                &root,
                2,
                Some(CanonicalTool {
                    name: "shell".to_string(),
                    use_id: None,
                    input: Some(json!({"resources": ["separate action"]})),
                }),
            ),
            b"independent preview permission",
            1_004,
        )?;
        core.tick(&settings, 1_005)?;
        let pending = core.pending_messages();
        assert_eq!(
            pending
                .iter()
                .find(|message| message.id == preview.id)
                .ok_or("reply preview disappeared")?
                .state,
            AgentBridgePendingState::Held,
        );
        core.confirm_reply_preview(&preview.id, &session_id, &preview.text, 1_006)?;
        core.tick(&settings, 1_007)?;
        let response: HookResponse = wire::read_json_bounded(
            &paths
                .session(&input.binding)?
                .response_path(&input.invocation_id)?,
            wire::MAX_RESPONSE_BYTES,
        )?;
        assert_eq!(response.reason.as_deref(), Some("Preserve these words"));
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
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

    /// The worker republishes when the status differs, so the status has to
    /// differ for every change the console shows. A second request landing
    /// in a session already listed, and a listed request passing its
    /// deadline, both leave the session and pending counts where they were.
    #[test]
    fn status_changes_when_a_listed_session_gains_or_loses_a_request() -> Result<(), Box<dyn Error>>
    {
        let root = test_root("status-revision")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-status-revision"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let permission = |command: &str| {
            event(
                CanonicalEventKind::PermissionRequest,
                &root,
                Some(CanonicalTool {
                    name: "Bash".to_string(),
                    use_id: None,
                    input: Some(json!({ "command": command })),
                }),
            )
        };
        persist_event(
            &paths,
            &app_id,
            binding.clone(),
            permission("cargo build"),
            b"first",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;
        let listed = core.status(&settings);

        let second = persist_event(
            &paths,
            &app_id,
            binding,
            permission("cargo test"),
            b"second",
            1_003,
        )?;
        core.tick(&settings, 1_004)?;
        let with_second = core.status(&settings);
        assert_eq!(with_second.observed_sessions, listed.observed_sessions);
        assert_ne!(with_second, listed);

        core.tick(&settings, second.expires_at_ms.saturating_add(1))?;
        assert_ne!(core.status(&settings), with_second);
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

    /// Idle, the worker's only work is keeping the heartbeat alive, so it
    /// wakes as often as the cap on one wait allows and no more, and a hook
    /// arriving just before any of those wakes still finds the app alive.
    #[test]
    fn an_idle_worker_wakes_twelve_times_a_minute_and_keeps_its_heartbeat(
    ) -> Result<(), Box<dyn Error>> {
        let root = test_root("idle-wait")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let settings = enabled_settings(wire::project_hash(&root)?);
        let mut core = AgentBridgeCore::new(paths.clone(), opaque_hash(&[b"idle-wait"]))?;
        core.start(&settings, 1_000)?;
        let mut now = 1_000;
        let mut wakes = 0;
        while now < 61_000 {
            now += u64::try_from(core.idle_wait(now).as_millis())?;
            // Validity only runs out, so the moment before this wake's tick
            // is when a hook finds the app's records stalest.
            let lease = paths.read_lease()?;
            assert!(lease.is_valid_at(now), "the lease lapsed before {now}");
            assert!(
                paths.read_heartbeat()?.is_valid_for(&lease, now),
                "the heartbeat lapsed before {now}"
            );
            core.tick(&settings, now)?;
            wakes += 1;
        }
        assert_eq!(wakes, 12);
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// A held reply and an unanswered request each change state the moment
    /// their deadline passes, however far off the heartbeat is: the worker's
    /// first wake after either deadline is the millisecond after it.
    #[test]
    fn the_worker_wakes_as_each_reply_or_request_expires() -> Result<(), Box<dyn Error>> {
        let root = test_root("expiry-wake")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-expiry-wake"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let prompt = event(CanonicalEventKind::UserPromptSubmit, &root, None);
        persist_event(&paths, &app_id, binding, prompt, b"prompt", 1_001)?;
        core.tick(&settings, 1_002)?;
        let session_id = core.sessions()[0].id.clone();
        core.create_reply_preview(&session_id, "hold me".to_string(), 1_003, Some(2_000))?;
        let reply_deadline = core.pending_messages()[0].expires_at_ms;
        let request_deadline = core.requests()[0].expires_at_ms;

        let mut now = 1_003;
        let mut wakes = Vec::new();
        while now <= request_deadline {
            now += u64::try_from(core.idle_wait(now).as_millis())?;
            core.tick(&settings, now)?;
            wakes.push(now);
        }
        let first_wake_after = |deadline: u64| wakes.iter().copied().find(|&wake| wake > deadline);
        assert_eq!(first_wake_after(reply_deadline), Some(reply_deadline + 1));
        assert_eq!(
            first_wake_after(request_deadline),
            Some(request_deadline + 1)
        );
        assert_eq!(
            core.pending_messages()[0].state,
            AgentBridgePendingState::CopyOnly
        );
        assert_eq!(core.requests()[0].state, AgentBridgeRequestState::Expired);
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// A hook that writes a request must wake the worker at once, long before
    /// the wait it is in would have run out.
    #[cfg(unix)]
    #[test]
    fn persisting_a_request_wakes_the_worker_before_its_wait_runs_out() -> Result<(), Box<dyn Error>>
    {
        let root = test_root("wake")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let listener = WakeListener::bind(&paths.wake_path())?;
        let long_wait = std::time::Duration::from_secs(30);
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
            listener.wait(long_wait),
            "the request write did not wake the listener"
        );
        assert!(started.elapsed() < long_wait);
        // Nothing else was written, so a short wait runs out on its own
        // timeout rather than the one set before it.
        let quiet = std::time::Instant::now();
        assert!(!listener.wait(std::time::Duration::from_millis(20)));
        assert!(quiet.elapsed() < long_wait);
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

    /// A second app refused the bridge takes it over once the first quits,
    /// without a relaunch or a settings change.
    #[test]
    fn a_refused_app_serves_once_the_holder_quits() -> Result<(), Box<dyn Error>> {
        let root = test_root("standby")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let settings = enabled_settings(wire::project_hash(&root)?);
        let mut first = AgentBridgeCore::new(paths.clone(), opaque_hash(&[b"first"]))?;
        let mut second = AgentBridgeCore::new(paths.clone(), opaque_hash(&[b"second"]))?;
        first.start(&settings, 1_000)?;
        assert_eq!(
            second.start(&settings, 1_001),
            Err(AgentBridgeError::AppLockHeld)
        );

        first.stop();
        second.tick(&settings, 2_000)?;

        assert_eq!(
            second.status(&settings).diagnostic,
            AgentBridgeDiagnostic::Active
        );
        assert_eq!(
            paths.read_lease()?.app_instance_id,
            opaque_hash(&[b"second"])
        );
        second.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// The serving app holds the owner lock, so a lease that disappears
    /// under it is written again instead of leaving hooks without one.
    #[test]
    fn a_serving_app_republishes_a_lease_that_went_missing() -> Result<(), Box<dyn Error>> {
        let root = test_root("missing-lease")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let settings = enabled_settings(wire::project_hash(&root)?);
        let mut core = AgentBridgeCore::new(paths.clone(), opaque_hash(&[b"serving"]))?;
        core.start(&settings, 1_000)?;
        fs::remove_file(paths.app_lock_path())?;

        core.tick(&settings, 1_001)?;

        assert_eq!(
            paths.read_lease()?.app_instance_id,
            opaque_hash(&[b"serving"])
        );
        core.stop();
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
            None,
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

    /// A settings change advances the policy generation, and every hook
    /// event after it carries the new one. A reply confirmed before the
    /// change is still for the same authorized session, so the stop that
    /// arrives under the new generation must deliver it rather than leave
    /// it held until it expires.
    #[test]
    fn a_confirmed_reply_follows_its_session_across_a_policy_change() -> Result<(), Box<dyn Error>>
    {
        let root = test_root("policy-move")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-policy-move"]);
        let binding = binding(&root)?;
        let mut settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let prompt = event(CanonicalEventKind::UserPromptSubmit, &root, None);
        persist_event(&paths, &app_id, binding, prompt, b"prompt", 1_001)?;
        core.tick(&settings, 1_002)?;
        let session_id = core.sessions()[0].id.clone();
        let preview =
            core.create_reply_preview(&session_id, "carry on".to_string(), 1_003, None)?;
        core.confirm_reply_preview(&preview.id, &session_id, "carry on", 1_004)?;

        settings.advance_policy_generation();
        core.tick(&settings, 1_005)?;
        let moved = SessionBinding::new(
            Agent::Claude,
            "provider-session",
            &root,
            2,
            settings.policy_generation,
        )?;
        let stop = persist_event(
            &paths,
            &app_id,
            moved,
            event(CanonicalEventKind::Stop, &root, None),
            b"stop-after-move",
            1_006,
        )?;
        core.tick(&settings, 1_007)?;
        let response: HookResponse = wire::read_json_bounded(
            &paths
                .session(&stop.binding)?
                .response_path(&stop.invocation_id)?,
            wire::MAX_RESPONSE_BYTES,
        )?;
        assert_eq!(response.reason.as_deref(), Some("carry on"));
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
                &settings,
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
                Some("omp-rule"),
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
            Some("rule-1"),
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

    /// Removing a project withdraws the answer to a request seen while it
    /// was authorized: the request leaves the console on the next tick, and
    /// neither a rule nor a response can be made for it, even though the
    /// hook is still holding the agent open inside its deadline.
    #[test]
    fn revoking_a_project_withdraws_its_outstanding_request() -> Result<(), Box<dyn Error>> {
        let root = test_root("revoked")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-revoked"]);
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
        assert_eq!(core.requests().len(), 1);

        settings.allowed_projects.clear();
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
        assert_eq!(
            core.exact_rule_for_request(
                &request.invocation_id,
                "rule-2".to_string(),
                AgentBridgePermissionDecision::Allow,
                &settings,
            ),
            Err(AgentBridgeError::Unauthorized)
        );
        assert_eq!(
            core.respond_permission(
                &request.invocation_id,
                Some("rule-1"),
                AgentBridgePermissionDecision::Allow,
                &settings,
                1_003,
            ),
            Err(AgentBridgeError::Unauthorized)
        );
        assert!(!paths
            .session(&request.binding)?
            .response_path(&request.invocation_id)?
            .exists());

        settings.policy_generation += 1;
        core.tick(&settings, 1_004)?;
        assert!(core.requests().is_empty());

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
                &settings,
            )
            .unwrap_err(),
            AgentBridgeError::PermissionResponseUnsupported
        );
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// The list shows what the agent is about to do. For a shell tool that
    /// is the command line; the description and timeout beside it are not
    /// what is being approved.
    #[test]
    fn a_permission_request_is_listed_with_its_command_line() -> Result<(), Box<dyn Error>> {
        let root = test_root("preview")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-preview"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        persist_event(
            &paths,
            &app_id,
            binding,
            event(
                CanonicalEventKind::PermissionRequest,
                &root,
                Some(CanonicalTool {
                    name: "Bash".to_string(),
                    use_id: None,
                    input: Some(json!({
                        "command": "cargo test --lib agent_bridge",
                        "description": "Run the bridge tests",
                        "timeout": 120_000
                    })),
                }),
            ),
            b"preview",
            1_001,
        )?;
        core.tick(&settings, 1_002)?;
        assert_eq!(
            core.requests()[0].tool_input_preview.as_deref(),
            Some("cargo test --lib agent_bridge")
        );
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// "Delegate to…" hands an agent a prompt that names the task. The
    /// session that runs it is tied to the task, and the task is started
    /// once, however often the bridge looks again.
    #[test]
    fn a_prompt_sona_wrote_ties_its_session_to_the_task_once() -> Result<(), Box<dyn Error>> {
        let root = test_root("delegated-prompt")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-delegated-prompt"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let mut prompt = event(CanonicalEventKind::UserPromptSubmit, &root, None);
        prompt.prompt = Some(format!(
            "{} S-12: Book the offsite venue\n\nReport back to Sona as you work.",
            crate::planning::work::agents::PROMPT_MARK
        ));
        persist_event(&paths, &app_id, binding, prompt, b"delegated", 1_001)?;
        core.tick(&settings, 1_002)?;
        let session = core.sessions().pop().ok_or("the prompt's session")?;
        assert_eq!(session.task.as_deref(), Some("S-12"));
        assert_eq!(
            core.take_started(),
            [(AgentBridgeAgent::Claude, session.id, "S-12".to_owned())]
        );
        core.tick(&settings, 1_003)?;
        assert!(core.take_started().is_empty());
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// An agent that claims a task itself, by command or with Sona's tool,
    /// is followed on it. Its own claim started the task, so Sona does not
    /// start it again; another program's `work claim` is not a claim.
    #[test]
    fn a_sessions_own_claim_ties_it_to_the_task_without_starting_it() -> Result<(), Box<dyn Error>>
    {
        let root = test_root("own-claim")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let app_id = opaque_hash(&[b"app-own-claim"]);
        let binding = binding(&root)?;
        let settings = enabled_settings(binding.project_hash.clone());
        let mut core = AgentBridgeCore::new(paths.clone(), app_id.clone())?;
        core.start(&settings, 1_000)?;
        let ran = |name: &str, field: &str, value: &str| {
            event(
                CanonicalEventKind::PostToolUse,
                &root,
                Some(CanonicalTool {
                    name: name.to_string(),
                    use_id: None,
                    input: Some(json!({ field: value })),
                }),
            )
        };
        let steps = [
            (ran("Bash", "command", "cargo work claim S-3"), None),
            (
                ran(
                    "Bash",
                    "command",
                    "~/bin/sona work claim --operation-id 0b5c2d9e-4f7a-4c1e-9a3b-6d8e2f1a7c40 S-7 --agent \"Claude Code\"",
                ),
                Some("S-7"),
            ),
            (ran("mcp__sona__sona_work_claim", "task", "#9"), Some("S-9")),
        ];
        for (at, (step, task)) in (1_001..).step_by(2).zip(steps) {
            let raw = format!("ran at {at}");
            persist_event(&paths, &app_id, binding.clone(), step, raw.as_bytes(), at)?;
            core.tick(&settings, at + 1)?;
            let session = core.sessions().pop().ok_or("the session")?;
            assert_eq!(session.task.as_deref(), task);
        }
        assert!(core.take_started().is_empty());
        core.stop();
        fs::remove_dir_all(root)?;
        Ok(())
    }

    macro_rules! preview_cases {
        ($($name:ident: $input:expr => $want:expr;)*) => { $(
            #[test]
            fn $name() {
                assert_eq!(tool_input_preview(&$input), $want);
            }
        )* };
    }
    preview_cases! {
        preview_of_a_tool_without_a_command_is_its_compact_json:
            json!({"file_path": "/tmp/a.rs"}) => r#"{"file_path":"/tmp/a.rs"}"#;
        preview_keeps_one_screenful_whole:
            json!("x".repeat(TOOL_INPUT_PREVIEW_CHARS)) => "x".repeat(TOOL_INPUT_PREVIEW_CHARS);
        preview_marks_what_it_cut:
            json!("x".repeat(TOOL_INPUT_PREVIEW_CHARS + 1))
            => format!("{}…", "x".repeat(TOOL_INPUT_PREVIEW_CHARS));
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
