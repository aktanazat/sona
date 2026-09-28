//! `sona-agent-hook`: the Tauri-free bridge between a coding agent's hook
//! system and the Sona desktop app.
//!
//! One invocation reads exactly one bounded JSON event from stdin. Fixture-backed
//! Claude events can wait for one app response only after the interactive app
//! lease, heartbeat, and policy projection authorize their exact session.

mod codec;
mod response;
mod runtime;
#[path = "../../agent_hook_wire.rs"]
mod wire;

#[cfg(test)]
mod tests;

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use codec::{decode_event, DecodeError};
use runtime::{protocol_session_generation, Clock, Request, RuntimePaths, SessionPaths};
pub(crate) use wire::{
    Agent, CanonicalEvent, CanonicalEventKind, HookAck, SessionBinding, ASK_USER_QUESTION_TOOL,
    EXIT_PLAN_MODE_TOOL, MAX_RESPONSE_BYTES, PROTOCOL_GENERATION, SCHEMA_VERSION,
};

const MAX_INPUT_BYTES: usize = wire::MAX_REQUEST_BYTES;
const USAGE_EXIT_CODE: i32 = 64;

/// How long the hook holds its agent open for an unanswered turn end.
///
/// The app answers a `Stop` only from a reply the user confirmed before the
/// event arrived: `respond_to_stop` returns without writing when no confirmed
/// reply is held, and nothing answers that request afterwards. So the wait is
/// worth only the bridge worker's own scan, which this hook's wake starts at
/// once, plus room for the write. Waiting the full request lifetime instead
/// would stall every turn end for 30 seconds and never produce an answer.
const STOP_POLL_TIMEOUT: Duration = Duration::from_millis(2_000);

/// Fixed, content-free stderr lines. Event and response payloads are private and
/// never appear in a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Diagnostic {
    Usage,
    InputTooLarge,
    InputUnreadable,
    InvalidEvent,
    UnsupportedEvent,
    RuntimeUnavailable,
    RequestNotPersisted,
    ResponseNotClaimed,
    ResponseMalformed,
    ResponseNotBound,
    ResponseStale,
    ResponseOutcomeUnknown,
    ResponseOutcomeUnsupported,
}

impl Diagnostic {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Usage => "usage: sona-agent-hook <claude|codex|grok|omp>",
            Self::InputTooLarge => "sona-agent-hook: input exceeds the size limit; passing through",
            Self::InputUnreadable => "sona-agent-hook: input could not be read; passing through",
            Self::InvalidEvent => "sona-agent-hook: invalid event; passing through",
            Self::UnsupportedEvent => "sona-agent-hook: unsupported event; passing through",
            Self::RuntimeUnavailable => {
                "sona-agent-hook: private runtime is unavailable; passing through"
            }
            Self::RequestNotPersisted => {
                "sona-agent-hook: request was not persisted; passing through"
            }
            Self::ResponseNotClaimed => {
                "sona-agent-hook: response could not be claimed; passing through"
            }
            Self::ResponseMalformed => "sona-agent-hook: malformed response; passing through",
            Self::ResponseNotBound => {
                "sona-agent-hook: response is not bound to this invocation; passing through"
            }
            Self::ResponseStale => "sona-agent-hook: response has expired; passing through",
            Self::ResponseOutcomeUnknown => {
                "sona-agent-hook: unknown response outcome; passing through"
            }
            Self::ResponseOutcomeUnsupported => {
                "sona-agent-hook: unsupported response outcome; passing through"
            }
        }
    }
}

/// How long the hook is willing to hold the agent while the app answers.
///
/// A permission gate is worth the whole request lifetime, because the console
/// can answer it for as long as the request is valid and the person deciding
/// is the slow part. A turn end is not; see `STOP_POLL_TIMEOUT`.
pub(crate) struct PollBudget {
    gate_timeout: Duration,
    stop_timeout: Duration,
    interval: Duration,
}

impl PollBudget {
    pub(crate) fn production() -> Self {
        Self {
            gate_timeout: Duration::from_millis(wire::REQUEST_TTL_MS),
            stop_timeout: STOP_POLL_TIMEOUT,
            interval: Duration::from_millis(100),
        }
    }

    #[cfg(test)]
    pub(crate) fn immediate() -> Self {
        Self {
            gate_timeout: Duration::ZERO,
            stop_timeout: Duration::ZERO,
            interval: Duration::ZERO,
        }
    }

    fn timeout(&self, kind: CanonicalEventKind) -> Duration {
        match kind {
            CanonicalEventKind::Stop => self.stop_timeout,
            _ => self.gate_timeout,
        }
    }
}

pub(crate) struct HookRuntime {
    paths: RuntimePaths,
    workspace: PathBuf,
    clock: Clock,
    poll: PollBudget,
}

impl HookRuntime {
    pub(crate) fn new(
        paths: RuntimePaths,
        workspace: PathBuf,
        clock: Clock,
        poll: PollBudget,
    ) -> Self {
        Self {
            paths,
            workspace,
            clock,
            poll,
        }
    }

    #[cfg(test)]
    pub(crate) fn paths(&self) -> &RuntimePaths {
        &self.paths
    }

    /// Rechecks every app-authored record before creating any session file.
    fn prepare(&self, event: &CanonicalEvent, raw: &[u8]) -> Option<(SessionPaths, Request)> {
        if !self.paths.interactive_supported() {
            return None;
        }

        let now_ms = self.clock.now_ms();
        let lease = self.paths.read_lease().ok()?;
        if !lease.is_valid_at(now_ms) {
            return None;
        }

        let heartbeat = self.paths.read_heartbeat().ok()?;
        if !heartbeat.is_valid_for(&lease, now_ms) {
            return None;
        }

        let policy = self.paths.read_policy().ok()?;
        if policy.generation != lease.policy_generation {
            return None;
        }

        let workspace = event
            .workspace_root
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.workspace.clone());
        let binding = SessionBinding::new(
            event.agent,
            &event.session_id,
            &workspace,
            protocol_session_generation(),
            policy.generation,
        )
        .ok()?;
        if !policy.allows(&binding, &lease.app_instance_id, now_ms) {
            return None;
        }

        let session = self.paths.session(&binding).ok()?;
        let request = Request::new(lease.app_instance_id, binding, event, raw, now_ms).ok()?;
        Some((session, request))
    }

    fn claim(
        &self,
        session: &SessionPaths,
        request: &Request,
    ) -> Result<Option<PathBuf>, Diagnostic> {
        let started = Instant::now();
        loop {
            if self.clock.now_ms() > request.expires_at_ms {
                return Ok(None);
            }

            if let Some(claimed) = response::try_claim(session, &request.invocation_id)? {
                return Ok(Some(claimed));
            }

            let remaining = self
                .poll
                .timeout(request.event.event)
                .saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(None);
            }
            thread::sleep(self.poll.interval.min(remaining));
        }
    }
}

pub(super) fn run_cli() -> i32 {
    let Some(agent) = parse_agent() else {
        write_stderr(Diagnostic::Usage);
        return USAGE_EXIT_CODE;
    };

    let Ok(paths) = RuntimePaths::for_current_user() else {
        write_stderr(Diagnostic::RuntimeUnavailable);
        return 0;
    };
    let Ok(workspace) = env::current_dir() else {
        return 0;
    };

    let hook = HookRuntime::new(paths, workspace, Clock::system(), PollBudget::production());
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let _ = run_with_runtime(
        agent,
        &mut stdin.lock(),
        &mut stdout.lock(),
        &mut stderr.lock(),
        &hook,
    );
    0
}

fn parse_agent() -> Option<Agent> {
    let mut arguments = env::args_os().skip(1);
    let selector = arguments.next()?;
    if arguments.next().is_some() {
        return None;
    }
    Agent::parse(selector.to_str()?)
}

fn run_with_runtime<R: Read, W: Write, D: Write>(
    agent: Agent,
    input: &mut R,
    stdout: &mut W,
    stderr: &mut D,
    hook: &HookRuntime,
) -> io::Result<()> {
    let raw = match read_bounded_json(input) {
        Ok(raw) => raw,
        Err(diagnostic) => return write_diagnostic(stderr, diagnostic),
    };

    let event = match decode_event(agent, &raw) {
        Ok(event) => event,
        Err(DecodeError::Invalid) => return write_diagnostic(stderr, Diagnostic::InvalidEvent),
        Err(DecodeError::Unsupported) => {
            return write_diagnostic(stderr, Diagnostic::UnsupportedEvent)
        }
    };

    if event.event == CanonicalEventKind::Stop && event.stop_hook_active {
        return Ok(());
    }

    let Some((session, request)) = hook.prepare(&event, &raw) else {
        return Ok(());
    };
    if session.persist_request(&request).is_err() {
        return write_diagnostic(stderr, Diagnostic::RequestNotPersisted);
    }

    // Nonblocking events publish session observations for the app bridge but
    // never hold the provider process open for a response.
    if !event.awaits_response() {
        return Ok(());
    }

    let claimed = match hook.claim(&session, &request) {
        Ok(Some(claimed)) => claimed,
        Ok(None) => return Ok(()),
        Err(diagnostic) => return write_diagnostic(stderr, diagnostic),
    };
    let encoded = match response::encode_claimed(&claimed, &hook.clock, &request, &event) {
        Ok(Some(encoded)) => encoded,
        Ok(None) => return Ok(()),
        Err(diagnostic) => return write_diagnostic(stderr, diagnostic),
    };

    stdout.write_all(&encoded)?;
    stdout.flush()?;
    session.persist_ack(&HookAck::response_emitted(&request, hook.clock.now_ms()))?;
    fs::remove_file(claimed)?;
    Ok(())
}

fn read_bounded_json<R: Read>(input: &mut R) -> Result<Vec<u8>, Diagnostic> {
    let mut raw = Vec::with_capacity(MAX_INPUT_BYTES.min(4096));
    let read_limit =
        u64::try_from(MAX_INPUT_BYTES.saturating_add(1)).map_err(|_| Diagnostic::InputTooLarge)?;
    input
        .take(read_limit)
        .read_to_end(&mut raw)
        .map_err(|_| Diagnostic::InputUnreadable)?;

    if raw.len() > MAX_INPUT_BYTES {
        return Err(Diagnostic::InputTooLarge);
    }

    Ok(raw)
}

fn write_stderr(diagnostic: Diagnostic) {
    let stderr = io::stderr();
    let _ = write_diagnostic(&mut stderr.lock(), diagnostic);
}

fn write_diagnostic<W: Write>(stderr: &mut W, diagnostic: Diagnostic) -> io::Result<()> {
    stderr.write_all(diagnostic.message().as_bytes())?;
    stderr.write_all(b"\n")
}
