//! Shared wire contract for `sona-agent-hook` and the Sona app.
//!
//! This module may import only `std`, `serde`, `serde_json`, `sha2`, and Unix
//! `libc`. It must not depend on Tauri, Specta, UI code, app state, or platform
//! UI frameworks.
//!
//! Both crates compile this file, so it carries only what both sides need: the
//! record formats, path derivation, hashing, and the atomic private-file
//! primitives. Publisher-side lifetimes (the lease, heartbeat, and policy TTLs)
//! and the acknowledgement read bound belong to the app, which is their only
//! writer and reader; they live with `agent_bridge`.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
#[cfg(unix)]
use std::os::unix::net::UnixDatagram;

pub const SCHEMA_VERSION: u8 = 3;
pub const PROTOCOL_GENERATION: u32 = 3;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;
pub const MAX_POLICY_BYTES: usize = 64 * 1024;
pub const REQUEST_TTL_MS: u64 = 30_000;
pub const ASK_USER_QUESTION_TOOL: &str = "AskUserQuestion";
pub const EXIT_PLAN_MODE_TOOL: &str = "ExitPlanMode";
/// Grok's `Stop` also fires once at session teardown, where its decision output
/// is parsed and ignored. Only a genuine turn end carries this reason, and only
/// a genuine turn end can be continued.
pub const GROK_TURN_END_REASON: &str = "end_turn";

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Deserialize, Hash, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
    Grok,
    Omp,
}

impl Agent {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "grok" => Some(Self::Grok),
            "omp" => Some(Self::Omp),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::Omp => "omp",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalEventKind {
    SessionStart,
    UserPromptSubmit,
    PermissionRequest,
    PreToolUse,
    PostToolUse,
    Stop,
    Notification,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalEvent {
    pub schema_version: u8,
    pub agent: Agent,
    pub event: CanonicalEventKind,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub stop_hook_active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<CanonicalTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bypass_permissions: bool,
}

impl CanonicalEvent {
    pub fn tool_name(&self) -> Option<&str> {
        self.tool.as_ref().map(|tool| tool.name.as_str())
    }

    /// Whether this invocation is holding its agent open for Sona's answer.
    ///
    /// One table, compiled by both sides: the hook binary waits exactly when
    /// this is true, and the app writes a response exactly when it is true. A
    /// disagreement would leave either an agent blocked on an answer nobody
    /// writes or a response file nobody claims.
    ///
    /// Every entry is a reply channel that agent documents:
    ///
    /// | Agent  | Event               | Channel                                     |
    /// | ------ | ------------------- | ------------------------------------------- |
    /// | all    | `Stop`              | `{"decision":"block","reason":…}`           |
    /// | Claude | `PermissionRequest` | `decision.behavior` allow/deny              |
    /// | Claude | `PreToolUse`        | `permissionDecision` for the two ask tools  |
    /// | Codex  | `PermissionRequest` | `decision.behavior` allow/deny              |
    /// | Grok   | `PreToolUse`        | top-level `decision` allow/deny             |
    pub fn awaits_response(&self) -> bool {
        match (self.agent, self.event) {
            // Grok also fires `Stop` once at session teardown, where the
            // decision is parsed and ignored. Only a genuine turn end can be
            // continued, and only that one carries the turn-end reason.
            (Agent::Grok, CanonicalEventKind::Stop) => {
                !self.stop_hook_active && self.reason.as_deref() == Some(GROK_TURN_END_REASON)
            }
            (_, CanonicalEventKind::Stop) => !self.stop_hook_active,
            (Agent::Claude, CanonicalEventKind::PreToolUse) => self
                .tool_name()
                .is_some_and(|tool| tool == ASK_USER_QUESTION_TOOL || tool == EXIT_PLAN_MODE_TOOL),
            // Claude and Codex ask only when a prompt exists; Grok's only
            // gate is the pre-tool one. None can be answered while permissions
            // are bypassed, because no prompt appears to answer.
            (Agent::Claude | Agent::Codex, CanonicalEventKind::PermissionRequest)
            | (Agent::Grok, CanonicalEventKind::PreToolUse) => !self.bypass_permissions,
            _ => false,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionBinding {
    pub agent: Agent,
    pub session_handle: String,
    pub project_hash: String,
    pub session_generation: u64,
    pub policy_generation: u64,
}

impl SessionBinding {
    pub fn new(
        agent: Agent,
        provider_session_id: &str,
        canonical_project: &Path,
        session_generation: u64,
        policy_generation: u64,
    ) -> io::Result<Self> {
        if provider_session_id.is_empty() || session_generation == 0 || policy_generation == 0 {
            return Err(invalid_input("invalid session binding"));
        }
        let project_hash = project_hash(canonical_project)?;
        let session_handle = digest_parts(&[
            b"session",
            agent.as_str().as_bytes(),
            provider_session_id.as_bytes(),
            project_hash.as_bytes(),
        ]);
        Ok(Self {
            agent,
            session_handle,
            project_hash,
            session_generation,
            policy_generation,
        })
    }

    pub fn is_well_formed(&self) -> bool {
        is_hex_32(&self.session_handle)
            && is_hex_32(&self.project_hash)
            && self.session_generation > 0
            && self.policy_generation > 0
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookRequest {
    pub schema_version: u8,
    pub protocol_generation: u32,
    pub app_instance_id: String,
    pub binding: SessionBinding,
    pub invocation_id: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub event: CanonicalEvent,
}

impl HookRequest {
    pub fn new(
        app_instance_id: String,
        binding: SessionBinding,
        event: &CanonicalEvent,
        raw: &[u8],
        issued_at_ms: u64,
    ) -> io::Result<Self> {
        if event.session_id.is_empty() {
            return Err(invalid_input("request binding mismatch"));
        }
        let invocation_id = invocation_id(&app_instance_id, &binding, raw);
        let request = Self {
            schema_version: SCHEMA_VERSION,
            protocol_generation: PROTOCOL_GENERATION,
            app_instance_id,
            binding,
            invocation_id,
            issued_at_ms,
            expires_at_ms: issued_at_ms.saturating_add(REQUEST_TTL_MS),
            event: event.clone(),
        };
        // The reader's admission rule is the only authority on validity, so a
        // request that cannot pass it never reaches a session directory.
        if !request.is_valid_at(issued_at_ms) {
            return Err(invalid_input("request binding mismatch"));
        }
        Ok(request)
    }

    pub fn is_valid_at(&self, now_ms: u64) -> bool {
        self.schema_version == SCHEMA_VERSION
            && self.protocol_generation == PROTOCOL_GENERATION
            && self.event.schema_version == SCHEMA_VERSION
            && is_hex_32(&self.app_instance_id)
            && self.binding.is_well_formed()
            && self.binding.agent == self.event.agent
            && is_hex_32(&self.invocation_id)
            && self.issued_at_ms <= now_ms
            && now_ms <= self.expires_at_ms
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookAnswer {
    pub header: String,
    pub question: String,
    pub selected: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookResponse {
    pub schema_version: u8,
    pub protocol_generation: u32,
    pub app_instance_id: String,
    pub binding: SessionBinding,
    pub invocation_id: String,
    pub outcome: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<Vec<HookAnswer>>,
}

impl HookResponse {
    pub fn matches(&self, request: &HookRequest, now_ms: u64) -> bool {
        self.schema_version == SCHEMA_VERSION
            && self.protocol_generation == PROTOCOL_GENERATION
            && self.app_instance_id == request.app_instance_id
            && self.binding == request.binding
            && self.invocation_id == request.invocation_id
            && self.issued_at_ms <= now_ms
            && now_ms <= self.expires_at_ms
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookAck {
    pub schema_version: u8,
    pub protocol_generation: u32,
    pub app_instance_id: String,
    pub binding: SessionBinding,
    pub invocation_id: String,
    pub outcome: String,
    pub emitted_at_ms: u64,
}

impl HookAck {
    pub fn response_emitted(request: &HookRequest, emitted_at_ms: u64) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            protocol_generation: PROTOCOL_GENERATION,
            app_instance_id: request.app_instance_id.clone(),
            binding: request.binding.clone(),
            invocation_id: request.invocation_id.clone(),
            outcome: "response_emitted".to_string(),
            emitted_at_ms,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppLease {
    pub schema_version: u8,
    pub protocol_generation: u32,
    pub app_instance_id: String,
    pub pid: u32,
    pub policy_generation: u64,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

impl AppLease {
    pub fn is_valid_at(&self, now_ms: u64) -> bool {
        self.schema_version == SCHEMA_VERSION
            && self.protocol_generation == PROTOCOL_GENERATION
            && is_hex_32(&self.app_instance_id)
            && self.pid > 0
            && self.policy_generation > 0
            && self.issued_at_ms <= now_ms
            && now_ms <= self.expires_at_ms
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppHeartbeat {
    pub schema_version: u8,
    pub protocol_generation: u32,
    pub app_instance_id: String,
    pub policy_generation: u64,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

impl AppHeartbeat {
    pub fn is_valid_for(&self, lease: &AppLease, now_ms: u64) -> bool {
        self.schema_version == SCHEMA_VERSION
            && self.protocol_generation == PROTOCOL_GENERATION
            && self.app_instance_id == lease.app_instance_id
            && self.policy_generation == lease.policy_generation
            && self.issued_at_ms <= now_ms
            && now_ms <= self.expires_at_ms
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyProjection {
    pub schema_version: u8,
    pub protocol_generation: u32,
    pub app_instance_id: String,
    pub generation: u64,
    pub master_enabled: bool,
    pub enabled_agents: Vec<Agent>,
    pub allowed_project_hashes: Vec<String>,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

impl PolicyProjection {
    pub fn allows(&self, binding: &SessionBinding, app_instance_id: &str, now_ms: u64) -> bool {
        self.schema_version == SCHEMA_VERSION
            && self.protocol_generation == PROTOCOL_GENERATION
            && self.app_instance_id == app_instance_id
            && self.generation == binding.policy_generation
            && self.master_enabled
            && self.enabled_agents.contains(&binding.agent)
            && self
                .allowed_project_hashes
                .iter()
                .all(|hash| is_hex_32(hash))
            && self.allowed_project_hashes.contains(&binding.project_hash)
            && self.issued_at_ms <= now_ms
            && now_ms <= self.expires_at_ms
    }
}

#[derive(Clone, Debug)]
pub struct RuntimePaths {
    root: PathBuf,
    interactive_supported: bool,
}

#[derive(Clone, Debug)]
pub struct SessionPaths {
    requests: PathBuf,
    responses: PathBuf,
    claimed: PathBuf,
    acks: PathBuf,
    wake: PathBuf,
}

/// One datagram to the app's wake socket. Silent by design: a hook that
/// cannot reach the socket still persisted its file, and the app's fallback
/// tick reads the file instead. The app binds the socket and calls this on
/// itself to unblock a worker it is stopping.
pub fn send_wake(path: &Path) {
    #[cfg(unix)]
    if let Ok(socket) = UnixDatagram::unbound() {
        let _ = socket.send_to(&[1], path);
    }
    #[cfg(not(unix))]
    let _ = path;
}

impl RuntimePaths {
    pub fn for_current_user() -> io::Result<Self> {
        Self::from_root(runtime_root(), cfg!(unix))
    }

    pub fn from_root(root: PathBuf, interactive_supported: bool) -> io::Result<Self> {
        ensure_private_directory(&root)?;
        let paths = Self {
            root,
            interactive_supported,
        };
        ensure_private_directory(&paths.sessions_root())?;
        Ok(paths)
    }

    pub fn interactive_supported(&self) -> bool {
        self.interactive_supported
    }

    pub fn sessions_root(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn app_lock_path(&self) -> PathBuf {
        self.root.join("app-lock.json")
    }

    pub fn heartbeat_path(&self) -> PathBuf {
        self.root.join("heartbeat.json")
    }

    pub fn policy_path(&self) -> PathBuf {
        self.root.join("policy.json")
    }

    pub fn wake_path(&self) -> PathBuf {
        self.root.join("wake.sock")
    }

    pub fn session(&self, binding: &SessionBinding) -> io::Result<SessionPaths> {
        if !binding.is_well_formed() {
            return Err(invalid_input("invalid session binding"));
        }
        let agent_root = self.sessions_root().join(binding.agent.as_str());
        ensure_private_directory(&agent_root)?;
        let session_root = agent_root.join(&binding.session_handle);
        ensure_private_directory(&session_root)?;
        let root = session_root.join(binding.session_generation.to_string());
        SessionPaths::from_root(root, self.wake_path())
    }

    pub fn read_lease(&self) -> io::Result<AppLease> {
        read_json_bounded(&self.app_lock_path(), MAX_POLICY_BYTES)
    }

    pub fn read_heartbeat(&self) -> io::Result<AppHeartbeat> {
        read_json_bounded(&self.heartbeat_path(), MAX_POLICY_BYTES)
    }

    pub fn read_policy(&self) -> io::Result<PolicyProjection> {
        read_json_bounded(&self.policy_path(), MAX_POLICY_BYTES)
    }
}

impl SessionPaths {
    fn from_root(root: PathBuf, wake: PathBuf) -> io::Result<Self> {
        ensure_private_directory(&root)?;
        let requests = root.join("requests");
        let responses = root.join("responses");
        let claimed = root.join("claimed");
        let acks = root.join("acks");
        for directory in [&requests, &responses, &claimed, &acks] {
            ensure_private_directory(directory)?;
        }
        Ok(Self {
            requests,
            responses,
            claimed,
            acks,
            wake,
        })
    }

    pub fn request_path(&self, invocation_id: &str) -> io::Result<PathBuf> {
        json_path(&self.requests, invocation_id)
    }

    pub fn response_path(&self, invocation_id: &str) -> io::Result<PathBuf> {
        json_path(&self.responses, invocation_id)
    }

    pub fn claimed_path(&self, invocation_id: &str) -> io::Result<PathBuf> {
        json_path(&self.claimed, invocation_id)
    }

    pub fn ack_path(&self, invocation_id: &str) -> io::Result<PathBuf> {
        json_path(&self.acks, invocation_id)
    }

    pub fn persist_request(&self, request: &HookRequest) -> io::Result<()> {
        atomic_write_json(
            &self.request_path(&request.invocation_id)?,
            request,
            WriteMode::CreateNew,
        )?;
        send_wake(&self.wake);
        Ok(())
    }

    pub fn persist_ack(&self, ack: &HookAck) -> io::Result<()> {
        atomic_write_json(
            &self.ack_path(&ack.invocation_id)?,
            ack,
            WriteMode::CreateNew,
        )?;
        send_wake(&self.wake);
        Ok(())
    }

    pub fn claim_response(&self, invocation_id: &str) -> io::Result<PathBuf> {
        let source = self.response_path(invocation_id)?;
        let claimed = self.claimed_path(invocation_id)?;
        claim_file(&source, &claimed)?;
        Ok(claimed)
    }
}

pub fn project_hash(canonical_project: &Path) -> io::Result<String> {
    let canonical = canonical_project.canonicalize()?;
    let value = canonical
        .to_str()
        .ok_or_else(|| invalid_input("project path is not UTF-8"))?;
    Ok(digest_parts(&[b"project", value.as_bytes()]))
}

pub fn invocation_id(app_instance_id: &str, binding: &SessionBinding, raw: &[u8]) -> String {
    digest_parts(&[
        b"invocation",
        &PROTOCOL_GENERATION.to_be_bytes(),
        app_instance_id.as_bytes(),
        binding.agent.as_str().as_bytes(),
        binding.session_handle.as_bytes(),
        binding.project_hash.as_bytes(),
        &binding.session_generation.to_be_bytes(),
        &binding.policy_generation.to_be_bytes(),
        raw,
    ])
}

pub fn read_json_bounded<T: DeserializeOwned>(path: &Path, limit: usize) -> io::Result<T> {
    let bytes = read_regular_bounded(path, limit)?;
    serde_json::from_slice(&bytes).map_err(|_| invalid_data("invalid wire JSON"))
}

/// Whether an atomic write may take over a record that already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMode {
    /// Refuses an occupied path: the first writer of a record wins.
    CreateNew,
    /// Replaces the current private record with one rename.
    Replace,
}

pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T, mode: WriteMode) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid_data("wire serialization failed"))?;
    atomic_write(path, &bytes, mode)
}

pub fn atomic_write(path: &Path, bytes: &[u8], mode: WriteMode) -> io::Result<()> {
    if mode == WriteMode::Replace && path.exists() {
        verify_private_file(path)?;
    }
    let temporary = write_temporary(path, bytes)?;
    let result = match mode {
        WriteMode::CreateNew => fs::hard_link(&temporary, path),
        WriteMode::Replace => fs::rename(&temporary, path),
    };
    // A successful link leaves the temporary's own name behind; a rename
    // consumes it. Either failure must not leave a partial record either.
    if mode == WriteMode::CreateNew || result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    verify_private_file(path)
}

pub fn claim_file(source: &Path, destination: &Path) -> io::Result<()> {
    verify_private_file(source)?;
    let parent = destination
        .parent()
        .ok_or_else(|| invalid_input("claim destination has no parent"))?;
    verify_private_directory(parent)?;
    fs::hard_link(source, destination)?;
    if let Err(error) = fs::remove_file(source) {
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    verify_private_file(destination)
}

pub fn read_regular_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    verify_private_file(path)?;
    let metadata = fs::symlink_metadata(path)?;
    let limit_u64 = u64::try_from(limit).unwrap_or(u64::MAX);
    if metadata.len() > limit_u64 {
        return Err(invalid_data("file exceeds limit"));
    }
    let file = fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(limit.min(4096));
    file.take(limit_u64.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid_data("file exceeds limit"));
    }
    Ok(bytes)
}

pub fn ensure_private_directory(path: &Path) -> io::Result<()> {
    if path.exists() {
        return verify_private_directory(path);
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid_input("directory has no parent"))?;
    if !parent.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "private directory parent is missing",
        ));
    }
    #[cfg(unix)]
    {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(path)?;
    }
    #[cfg(not(unix))]
    fs::create_dir(path)?;
    verify_private_directory(path)
}

pub fn verify_private_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid_data("private path is not a directory"));
    }
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and touches no caller memory.
        let uid = unsafe { libc::geteuid() };
        if metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private directory ownership or mode mismatch",
            ));
        }
    }
    Ok(())
}

pub fn verify_private_file(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(invalid_data("private path is not a regular file"));
    }
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and touches no caller memory.
        let uid = unsafe { libc::geteuid() };
        if metadata.uid() != uid || metadata.mode() & 0o777 != 0o600 || metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private file ownership, mode, or link count mismatch",
            ));
        }
    }
    Ok(())
}

fn write_temporary(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_input("wire path has no parent"))?;
    verify_private_directory(parent)?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{}.{}.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("wire"),
        process::id(),
        sequence
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    verify_private_file(&temporary)?;
    Ok(temporary)
}

fn json_path(directory: &Path, id: &str) -> io::Result<PathBuf> {
    if !is_hex_32(id) {
        return Err(invalid_input("invalid wire identifier"));
    }
    Ok(directory.join(format!("{id}.json")))
}

fn digest_parts(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part);
        digest.update([0]);
    }
    hex_prefix(&digest.finalize())
}

fn hex_prefix(digest: &[u8]) -> String {
    let mut key = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        key.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        key.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    key
}

fn is_hex_32(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn runtime_root() -> PathBuf {
    #[cfg(unix)]
    {
        if let Some(path) = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            return path.join("sona-agent");
        }
        // SAFETY: geteuid has no preconditions and touches no caller memory.
        let uid = unsafe { libc::geteuid() };
        env::temp_dir().join(format!("sona-agent-{uid}"))
    }
    #[cfg(not(unix))]
    {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(env::temp_dir)
            .join("Sona")
            .join("agent-runtime")
    }
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn test_root(name: &str) -> io::Result<PathBuf> {
        let root = env::temp_dir().join(format!(
            "sona-wire-{name}-{}-{}",
            process::id(),
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        #[cfg(unix)]
        {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700).create(&root)?;
        }
        #[cfg(not(unix))]
        fs::create_dir(&root)?;
        Ok(root)
    }

    fn binding(project: &Path) -> io::Result<SessionBinding> {
        SessionBinding::new(Agent::Claude, "session-a", project, 1, 7)
    }

    fn event() -> CanonicalEvent {
        CanonicalEvent {
            schema_version: SCHEMA_VERSION,
            agent: Agent::Claude,
            event: CanonicalEventKind::Stop,
            session_id: "session-a".to_string(),
            transcript_path: None,
            stop_hook_active: false,
            workspace_root: None,
            request_id: None,
            model: None,
            source: None,
            prompt: None,
            message: None,
            reason: Some("finished".to_string()),
            tool: None,
            permission_mode: None,
            bypass_permissions: false,
        }
    }

    #[test]
    fn binding_and_paths_are_opaque() -> Result<(), Box<dyn Error>> {
        let root = test_root("paths")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let binding = binding(&root)?;
        let session = paths.session(&binding)?;
        let request = session.request_path(&digest_parts(&[b"invocation"]))?;
        let rendered = request.to_string_lossy();
        assert!(rendered.contains(binding.agent.as_str()));
        assert!(rendered.contains(&binding.session_handle));
        assert!(!rendered.contains("session-a"));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn request_response_and_ack_bind_exactly() -> Result<(), Box<dyn Error>> {
        let root = test_root("binding")?;
        let binding = binding(&root)?;
        let app_instance = digest_parts(&[b"app-instance"]);
        let request = HookRequest::new(
            app_instance.clone(),
            binding.clone(),
            &event(),
            b"payload",
            1_000,
        )?;
        assert!(request.is_valid_at(1_001));
        let response = HookResponse {
            schema_version: SCHEMA_VERSION,
            protocol_generation: PROTOCOL_GENERATION,
            app_instance_id: app_instance,
            binding,
            invocation_id: request.invocation_id.clone(),
            outcome: "block".to_string(),
            issued_at_ms: 1_001,
            expires_at_ms: 2_000,
            reason: Some("continue".to_string()),
            answers: None,
        };
        assert!(response.matches(&request, 1_500));
        let ack = HookAck::response_emitted(&request, 1_600);
        assert_eq!(ack.outcome, "response_emitted");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn lease_heartbeat_and_policy_expire() -> Result<(), Box<dyn Error>> {
        let root = test_root("policy")?;
        let binding = binding(&root)?;
        let app_instance_id = digest_parts(&[b"app"]);
        let lease = AppLease {
            schema_version: SCHEMA_VERSION,
            protocol_generation: PROTOCOL_GENERATION,
            app_instance_id: app_instance_id.clone(),
            pid: 42,
            policy_generation: 7,
            issued_at_ms: 1_000,
            expires_at_ms: 2_000,
        };
        let heartbeat = AppHeartbeat {
            schema_version: SCHEMA_VERSION,
            protocol_generation: PROTOCOL_GENERATION,
            app_instance_id: app_instance_id.clone(),
            policy_generation: 7,
            issued_at_ms: 1_000,
            expires_at_ms: 2_000,
        };
        let policy = PolicyProjection {
            schema_version: SCHEMA_VERSION,
            protocol_generation: PROTOCOL_GENERATION,
            app_instance_id: app_instance_id.clone(),
            generation: 7,
            master_enabled: true,
            enabled_agents: vec![Agent::Claude],
            allowed_project_hashes: vec![binding.project_hash.clone()],
            issued_at_ms: 1_000,
            expires_at_ms: 2_000,
        };
        assert!(lease.is_valid_at(1_500));
        assert!(heartbeat.is_valid_for(&lease, 1_500));
        assert!(policy.allows(&binding, &app_instance_id, 1_500));
        assert!(!lease.is_valid_at(2_001));
        assert!(!policy.allows(&binding, &app_instance_id, 2_001));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn atomic_write_modes_reject_duplicates_and_claim_once() -> Result<(), Box<dyn Error>> {
        let root = test_root("atomic")?;
        let paths = RuntimePaths::from_root(root.clone(), true)?;
        let binding = binding(&root)?;
        let session = paths.session(&binding)?;
        let id = digest_parts(&[b"response"]);
        let source = session.response_path(&id)?;
        atomic_write(&source, b"one", WriteMode::CreateNew)?;
        assert!(atomic_write(&source, b"two", WriteMode::CreateNew).is_err());
        atomic_write(&source, b"two", WriteMode::Replace)?;
        assert_eq!(
            read_regular_bounded(&source, MAX_POLICY_BYTES)?,
            b"two".as_slice()
        );
        let claimed = session.claimed_path(&id)?;
        claim_file(&source, &claimed)?;
        assert!(!source.exists());
        assert!(claim_file(&source, &claimed).is_err());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn strict_json_and_size_limit_reject_invalid_input() -> Result<(), Box<dyn Error>> {
        let root = test_root("json")?;
        let file = root.join("value.json");
        atomic_write(
            &file,
            br#"{"schema_version":2,"extra":true}"#,
            WriteMode::CreateNew,
        )?;
        let decoded: io::Result<AppLease> = read_json_bounded(&file, MAX_POLICY_BYTES);
        assert!(decoded.is_err());
        assert!(read_regular_bounded(&file, 4).is_err());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unix_rejects_wrong_modes_and_hardlinks() -> Result<(), Box<dyn Error>> {
        let root = test_root("unix")?;
        let file = root.join("wire.json");
        atomic_write(&file, b"{}", WriteMode::CreateNew)?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644))?;
        assert!(verify_private_file(&file).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        let linked = root.join("linked.json");
        fs::hard_link(&file, &linked)?;
        assert!(verify_private_file(&file).is_err());
        fs::remove_file(linked)?;
        fs::remove_file(file)?;
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
