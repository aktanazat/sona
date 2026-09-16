//! The single owner of target-application context: what a mode is allowed to
//! see, how it is read, and what is recorded about the reading.
//!
//! Three rules shape this module:
//!
//! 1. **One policy owner.** [`ContextPolicy`] is the only thing that decides
//!    which sources a run may use, and [`ContextSnapshot::assemble`] is the only
//!    place that applies it. Nothing downstream re-derives the decision.
//! 2. **One immutable snapshot per run, read in two stages.** The values that
//!    are only true at record-start — what the user had selected, and a
//!    clipboard copy inside the pre-roll window — are read the instant the run
//!    starts. The application context is read when the run first needs it,
//!    immediately before the step that consumes it, so it describes where the
//!    text is about to land rather than where the user began speaking. Both
//!    stages fold into exactly one [`ContextSnapshot`], frozen on first
//!    observation; later application switches or settings edits cannot change
//!    a run in flight.
//! 3. **Absence is never silent.** Every source reports a
//!    [`ContextSourceStatus`], so "the platform cannot do this", "permission was
//!    refused", "the user turned it off", "it was a password field" and "there
//!    was nothing there" stay distinguishable in receipts and diagnostics.
//!
//! Reading another application's window is an out-of-process request that can
//! block for as long as that application is wedged, so neither stage runs on
//! the keypress path: [`start_capture`] hands the record-start read to a worker
//! and the run joins the result under a bounded wait, then pays the same
//! bounded wait for the application stage on the thread that renders the
//! prompt.

use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod clipboard_recency;
/// Every platform reader has cross-process reads to bound, and the budget
/// arithmetic is the part worth testing, so this compiles everywhere.
mod deadline;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
mod project;
#[cfg(target_os = "windows")]
mod windows;

pub use project::ProjectContext;

// One name for "the reader this build talks to". Sona ships on these three
// desktops and nowhere else, so the list is exhaustive rather than a fallback
// chain: a fourth target has to add a reader instead of silently capturing
// nothing.
#[cfg(target_os = "linux")]
use self::linux as platform;
#[cfg(target_os = "macos")]
use self::macos as platform;
#[cfg(target_os = "windows")]
use self::windows as platform;

pub(crate) use clipboard_recency::set_clipboard_watch_enabled;
pub use clipboard_recency::DEFAULT_CLIPBOARD_PREROLL_MS;

/// Reads only the stable identity of the frontmost application. Mode activation
/// calls this before context capture, so it never depends on Accessibility or
/// URL/field/selection consent.
pub(crate) fn frontmost_application_identifier() -> Option<String> {
    platform::frontmost_application_identifier()
}

/// The result of a one-shot browser-host read for mode automation. It contains
/// only a normalized host, never the captured URL or page content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum WebsiteHostCapture {
    Captured(String),
    SecureField,
    #[default]
    Unavailable,
}

impl WebsiteHostCapture {
    pub(crate) fn host(&self) -> Option<&str> {
        match self {
            Self::Captured(host) => Some(host),
            Self::SecureField | Self::Unavailable => None,
        }
    }
}

/// Normalizes a user-entered website host. Rules intentionally accept hosts,
/// not URLs, paths, ports, or credentials.
pub(crate) fn normalize_website_host(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.contains(['/', '?', '#', '@', ':'])
        || value.chars().any(char::is_whitespace)
    {
        return None;
    }

    let url = url::Url::parse(&format!("https://{value}")).ok()?;
    if url.host_str().is_none()
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }

    let host = url.host_str()?.trim_end_matches('.');
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Parses a browser URL into the only value mode automation needs. The raw URL
/// is dropped before this function returns.
pub(crate) fn website_host_from_url(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    normalize_website_host(url.host_str()?)
}

/// Reads the frontmost browser host on a short-lived worker. A wedged
/// application falls back to app rules or the active mode instead of delaying a
/// recording start indefinitely.
pub(crate) fn frontmost_website_host() -> WebsiteHostCapture {
    #[cfg(target_os = "macos")]
    {
        const MODE_AUTOMATION_JOIN_TIMEOUT: Duration = Duration::from_millis(120);
        let (sender, receiver) = mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("sona-mode-host-capture".to_string())
            .spawn(move || {
                let _ = sender.send(macos::frontmost_website_host());
            });
        if spawned.is_err() {
            return WebsiteHostCapture::Unavailable;
        }
        receiver
            .recv_timeout(MODE_AUTOMATION_JOIN_TIMEOUT)
            .unwrap_or(WebsiteHostCapture::Unavailable)
    }

    #[cfg(not(target_os = "macos"))]
    {
        WebsiteHostCapture::Unavailable
    }
}

/// Performs the same bounded host capture for an explicit settings action. The
/// user has requested this read directly, so it may use the reader's complete
/// cross-process deadline.
pub(crate) fn capture_frontmost_website_host() -> WebsiteHostCapture {
    #[cfg(target_os = "macos")]
    {
        macos::frontmost_website_host()
    }

    #[cfg(not(target_os = "macos"))]
    {
        WebsiteHostCapture::Unavailable
    }
}

/// The outcome of an explicit one-shot selection read.
///
/// This is deliberately not a [`ContextPolicy`] source: ambient context is what
/// a mode is *allowed to glance at* while dictating, whereas voice command mode
/// asks the user to press a shortcut whose entire meaning is "operate on the
/// text I have selected". That per-invocation request is its own consent, so it
/// is read directly — exactly like [`capture_frontmost_website_host`] — and the
/// captured text becomes the run's operand rather than part of its context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectionCapture {
    Captured(String),
    /// Nothing was selected, the focused control is secure, Accessibility is
    /// not granted, or the platform cannot answer. The reason is retained so
    /// the caller can refuse with something a user can act on.
    Unavailable(ContextSourceStatus),
}

/// Reads the current selection for an action the user asked for explicitly.
/// Bounded by the reader's own cross-process deadline, and never reads a secure
/// text field.
pub fn capture_selected_text() -> SelectionCapture {
    platform::capture_selected_text()
}

/// Longest a run will wait at prompt-render time for its own capture. A wedged
/// target application must cost the user a poorer prompt, never a lost
/// dictation.
const CAPTURE_JOIN_TIMEOUT: Duration = Duration::from_millis(400);

/// Per-source cap on captured text. The prompt renderer trims again against its
/// own budget; this exists so a multi-megabyte editor buffer is never copied
/// into the process in the first place.
const MAX_SOURCE_BYTES: usize = 8 * 1024;

/// Whether this build's reader can answer the frontmost page's URL. Only macOS
/// Accessibility publishes one: UI Automation and AT-SPI2 expose a browser's
/// address bar as an ordinary control, and guessing per browser is not a
/// platform answer.
const SUPPORTS_BROWSER_URL: bool = cfg!(target_os = "macos");

/// Whether this build can prove a clipboard copy happened inside a run's
/// pre-roll window. [`clipboard_recency`] needs a change counter to sample, and
/// only macOS's pasteboard publishes one.
const SUPPORTS_CLIPBOARD_RECENCY: bool = cfg!(target_os = "macos");

/// How much target-app information a mode may use for one rendering run.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ContextPolicy {
    #[default]
    None,
    Target,
    TargetAndSelection,
    Full,
}

impl ContextPolicy {
    /// The effective per-run policy. The global ceiling is a privacy boundary,
    /// not a feature default: a mode can never collect more than it permits.
    pub fn clamp_to(self, ceiling: Self) -> Self {
        use ContextPolicy::*;
        match (self, ceiling) {
            (None, _) | (_, None) => None,
            (Target, _) | (_, Target) => Target,
            (TargetAndSelection, TargetAndSelection) => TargetAndSelection,
            (Full, TargetAndSelection) | (TargetAndSelection, Full) => TargetAndSelection,
            (Full, Full) => Full,
        }
    }

    pub(crate) fn wants_selection(self) -> bool {
        matches!(self, Self::TargetAndSelection | Self::Full)
    }

    pub(crate) fn wants_target(self) -> bool {
        !matches!(self, Self::None)
    }

    pub(crate) fn wants_focused_field(self) -> bool {
        matches!(self, Self::Full)
    }

    pub(crate) fn wants_clipboard(self) -> bool {
        matches!(self, Self::Full)
    }

    pub(crate) fn wants_project(self) -> bool {
        matches!(self, Self::Full)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct TargetMetadata {
    pub application_name: Option<String>,
    pub application_identifier: Option<String>,
    pub url: Option<String>,
    pub input_format: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ContextName {
    pub display_name: String,
    pub username: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ContextPacket {
    pub target: TargetMetadata,
    pub focused_element_name: Option<String>,
    pub focused_element_content: Option<String>,
    pub selected_text: Option<String>,
    pub clipboard_content: Option<String>,
    pub names_and_usernames: Vec<ContextName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectContext>,
}

impl ContextPacket {
    /// Applies the mode request and the frozen global privacy ceiling in one
    /// place. The caller supplies the original request so a higher-capability
    /// mode cannot bypass the ceiling by passing a pre-expanded packet.
    pub fn for_policy(&self, requested: ContextPolicy, ceiling: ContextPolicy) -> Self {
        match requested.clamp_to(ceiling) {
            ContextPolicy::None => Self::default(),
            ContextPolicy::Target => Self {
                target: self.target.clone(),
                ..Self::default()
            },
            ContextPolicy::TargetAndSelection => Self {
                target: self.target.clone(),
                selected_text: self.selected_text.clone(),
                ..Self::default()
            },
            ContextPolicy::Full => self.clone(),
        }
    }
}

/// Why a context source did or did not contribute to a run. Distinguishing
/// these is the point: "not supported here", "you have not granted
/// Accessibility", "you turned this off" and "there was nothing selected" are
/// four different things a user can act on, and an empty string is none of them.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ContextSourceStatus {
    /// The active mode's policy does not ask for this source.
    #[default]
    NotRequested,
    /// Read, and it had a value.
    Captured,
    /// Read successfully; the source currently holds nothing.
    Empty,
    /// This platform offers no way to read the source.
    Unsupported,
    /// The operating system refused: Accessibility access is not granted.
    PermissionDenied,
    /// A user setting keeps this source off.
    Disabled,
    /// The mode requested this source, but the frozen global privacy ceiling
    /// disallowed it. No value was read or retained.
    DisabledByCeiling,
    /// Deliberately not read: the focused control is a secure text field.
    SecureField,
    /// Deliberately not read: the clipboard's last change is not provably
    /// inside the recency window.
    Stale,
    /// The platform query failed or did not answer in time.
    Failed,
}

/// Per-source outcome of one context capture.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ContextSources {
    pub target: ContextSourceStatus,
    pub focused_field: ContextSourceStatus,
    pub selected_text: ContextSourceStatus,
    pub browser_url: ContextSourceStatus,
    pub clipboard: ContextSourceStatus,
    #[serde(default)]
    pub project: ContextSourceStatus,
}

/// Whether the platform's accessibility API is usable right now. Determined by
/// a non-prompting check — Sona never raises a permission dialog on its own.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AccessibilityAccess {
    Granted,
    Denied,
    #[default]
    Unsupported,
}

/// Content-free record of one capture: which sources participated and why the
/// others did not. Carries no target values and no user text, so it is safe to
/// persist next to a history entry and to log.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ContextReceipt {
    /// The policy requested by the selected mode.
    pub requested_policy: ContextPolicy,
    /// The effective requested-policy/global-ceiling minimum, frozen at start.
    pub policy: ContextPolicy,
    pub accessibility: AccessibilityAccess,
    pub sources: ContextSources,
    /// When the record-start sources were read: the selection the user had and
    /// a clipboard copy inside the pre-roll window.
    pub captured_at_ms: u64,
    /// When the application context was read. It is deliberately later than
    /// `captured_at_ms`: the frontmost application and its focused control are
    /// read immediately before the step that consumes them, so they describe
    /// where the text is about to land. `None` means the effective policy asked
    /// for no application context at all.
    #[serde(default)]
    pub application_captured_at_ms: Option<u64>,
}

/// What the record-start stage read, before the policy is applied. Values are
/// present only when the platform actually read them; every other case carries
/// the reason instead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StartCapture {
    pub(crate) captured_at_ms: u64,
    /// Accessibility as observed at record start. The application stage reports
    /// a later loss of access per source rather than rewriting this.
    pub(crate) accessibility: AccessibilityAccess,
    pub(crate) selected_text: SourceOutcome,
    pub(crate) clipboard: SourceOutcome,
    pub(crate) project: SourceOutcome<ProjectContext>,
}

/// What the application stage read: the frontmost application and the control
/// the text is about to land in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ApplicationCapture {
    /// `None` when the effective policy asked for no application context, so
    /// the stage never ran.
    pub(crate) captured_at_ms: Option<u64>,
    pub(crate) application_name: Option<String>,
    pub(crate) application_identifier: Option<String>,
    pub(crate) target: SourceOutcome,
    pub(crate) focused_field_name: Option<String>,
    pub(crate) focused_field: SourceOutcome,
    pub(crate) browser_url: SourceOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceOutcome<T = String> {
    Captured(T),
    Unavailable(ContextSourceStatus),
}

impl<T> Default for SourceOutcome<T> {
    fn default() -> Self {
        Self::Unavailable(ContextSourceStatus::NotRequested)
    }
}

impl SourceOutcome {
    /// A read that succeeded: a non-blank value is captured, blank is `Empty`.
    pub(crate) fn read(value: Option<String>) -> Self {
        match value {
            Some(text) if !text.trim().is_empty() => Self::Captured(truncate_on_boundary(text)),
            _ => Self::Unavailable(ContextSourceStatus::Empty),
        }
    }
}

impl<T> SourceOutcome<T> {
    fn split(self) -> (Option<T>, ContextSourceStatus) {
        match self {
            Self::Captured(text) => (Some(text), ContextSourceStatus::Captured),
            Self::Unavailable(status) => (None, status),
        }
    }
}

/// Everything the capture needs from settings, resolved once at run start so a
/// mid-run settings edit cannot change what is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureOptions {
    /// Whether the user has opted in to reading the frontmost browser URL.
    pub url_capture_enabled: bool,
    /// How long before record-start a clipboard copy still counts as part of
    /// the dictation. A copy older than this is never read.
    pub clipboard_preroll_ms: u64,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            url_capture_enabled: false,
            clipboard_preroll_ms: DEFAULT_CLIPBOARD_PREROLL_MS,
        }
    }
}

/// One capture, frozen. Constructed exactly once per run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextSnapshot {
    packet: ContextPacket,
    receipt: ContextReceipt,
}

impl ContextSnapshot {
    /// Applies the mode request and frozen global ceiling to two already-bounded
    /// platform reads. Each stage received only the effective policy, so content
    /// disallowed by the ceiling is never read in the first place.
    pub(crate) fn assemble(
        requested_policy: ContextPolicy,
        policy_ceiling: ContextPolicy,
        start: StartCapture,
        application: ApplicationCapture,
    ) -> Self {
        let policy = requested_policy.clamp_to(policy_ceiling);
        let StartCapture {
            captured_at_ms,
            accessibility,
            selected_text,
            clipboard,
            project,
        } = start;
        let ApplicationCapture {
            captured_at_ms: application_captured_at_ms,
            application_name,
            application_identifier,
            target,
            focused_field_name,
            focused_field,
            browser_url,
        } = application;

        let target_allowed = policy.wants_target();
        let focused_allowed = policy.wants_focused_field();
        let selection_allowed = policy.wants_selection();
        let clipboard_allowed = policy.wants_clipboard();

        let target_status = gate_status(&target, requested_policy.wants_target(), target_allowed);
        let (target_url, browser_url_status) =
            gate_source(browser_url, requested_policy.wants_target(), target_allowed);
        let (selected_text, selected_status) = gate_source(
            selected_text,
            requested_policy.wants_selection(),
            selection_allowed,
        );
        let (focused_content, focused_status) = gate_source(
            focused_field,
            requested_policy.wants_focused_field(),
            focused_allowed,
        );
        let (clipboard, clipboard_status) = gate_source(
            clipboard,
            requested_policy.wants_clipboard(),
            clipboard_allowed,
        );
        let (project, project_status) = gate_source(
            project,
            requested_policy.wants_project(),
            policy.wants_project(),
        );

        let raw_packet = ContextPacket {
            target: TargetMetadata {
                application_name: target_allowed.then_some(application_name).flatten(),
                application_identifier: target_allowed.then_some(application_identifier).flatten(),
                url: target_url,
                input_format: None,
            },
            focused_element_name: focused_allowed.then_some(focused_field_name).flatten(),
            focused_element_content: focused_content,
            selected_text,
            clipboard_content: clipboard,
            names_and_usernames: Vec::new(),
            project,
        };

        Self {
            receipt: ContextReceipt {
                requested_policy,
                policy,
                accessibility,
                sources: ContextSources {
                    target: target_status,
                    focused_field: focused_status,
                    selected_text: selected_status,
                    browser_url: browser_url_status,
                    clipboard: clipboard_status,
                    project: project_status,
                },
                captured_at_ms,
                application_captured_at_ms,
            },
            // One choke point for applying context policy to content. Source
            // statuses above remain explicit even when this strips all values.
            packet: raw_packet.for_policy(requested_policy, policy_ceiling),
        }
    }

    /// A snapshot for a run with no live target — a history retry, or a capture
    /// that did not answer in time. Nothing is claimed to have been read.
    pub fn unavailable(
        requested_policy: ContextPolicy,
        policy_ceiling: ContextPolicy,
        reason: ContextSourceStatus,
    ) -> Self {
        let policy = requested_policy.clamp_to(policy_ceiling);
        let source: SourceOutcome = SourceOutcome::Unavailable(reason);
        let sources = ContextSources {
            target: gate_status(
                &source,
                requested_policy.wants_target(),
                policy.wants_target(),
            ),
            focused_field: gate_status(
                &source,
                requested_policy.wants_focused_field(),
                policy.wants_focused_field(),
            ),
            selected_text: gate_status(
                &source,
                requested_policy.wants_selection(),
                policy.wants_selection(),
            ),
            browser_url: gate_status(
                &source,
                requested_policy.wants_target(),
                policy.wants_target(),
            ),
            clipboard: gate_status(
                &source,
                requested_policy.wants_clipboard(),
                policy.wants_clipboard(),
            ),
            project: gate_status(
                &source,
                requested_policy.wants_project(),
                policy.wants_project(),
            ),
        };
        Self {
            packet: ContextPacket::default(),
            receipt: ContextReceipt {
                requested_policy,
                policy,
                accessibility: platform_accessibility(),
                sources,
                captured_at_ms: now_ms(),
                application_captured_at_ms: None,
            },
        }
    }

    pub fn packet(&self) -> &ContextPacket {
        &self.packet
    }

    pub fn target(&self) -> &TargetMetadata {
        &self.packet.target
    }

    pub fn receipt(&self) -> &ContextReceipt {
        &self.receipt
    }
}

fn gate_source<T>(
    source: SourceOutcome<T>,
    requested: bool,
    allowed: bool,
) -> (Option<T>, ContextSourceStatus) {
    if !requested {
        (None, ContextSourceStatus::NotRequested)
    } else if !allowed {
        (None, ContextSourceStatus::DisabledByCeiling)
    } else {
        source.split()
    }
}

fn gate_status<T>(
    source: &SourceOutcome<T>,
    requested: bool,
    allowed: bool,
) -> ContextSourceStatus {
    if !requested {
        ContextSourceStatus::NotRequested
    } else if !allowed {
        ContextSourceStatus::DisabledByCeiling
    } else {
        match source {
            SourceOutcome::Captured(_) => ContextSourceStatus::Captured,
            SourceOutcome::Unavailable(status) => *status,
        }
    }
}

/// A capture in flight. Resolves to exactly one snapshot: the first observation
/// runs the application stage, folds it into the record-start read, and every
/// later reader sees that same value.
#[derive(Debug)]
pub struct PendingContext {
    requested_policy: ContextPolicy,
    policy_ceiling: ContextPolicy,
    options: CaptureOptions,
    resolved: OnceLock<ContextSnapshot>,
    inbox: Mutex<Option<Receiver<StartCapture>>>,
}

impl PendingContext {
    /// Already-resolved context, for a ceiling-disabled run or one with no live
    /// target to read.
    pub fn resolved(snapshot: ContextSnapshot) -> Self {
        let resolved = OnceLock::new();
        let requested_policy = snapshot.receipt.requested_policy;
        // This field is never read when the resolved cell is set. Keeping the
        // effective policy is still conservative if a poisoned cell regresses.
        let policy_ceiling = snapshot.receipt.policy;
        let _ = resolved.set(snapshot);
        Self {
            requested_policy,
            policy_ceiling,
            options: CaptureOptions::default(),
            resolved,
            inbox: Mutex::new(None),
        }
    }

    fn awaiting(
        requested_policy: ContextPolicy,
        policy_ceiling: ContextPolicy,
        options: CaptureOptions,
        inbox: Receiver<StartCapture>,
    ) -> Self {
        Self {
            requested_policy,
            policy_ceiling,
            options,
            resolved: OnceLock::new(),
            inbox: Mutex::new(Some(inbox)),
        }
    }

    /// The frozen snapshot for this run. The first caller joins the record-start
    /// read under a bounded wait, then reads the application context — that
    /// caller is the step about to consume the context, which is exactly when
    /// "which application am I writing into" has to be answered. A record-start
    /// read that misses its window resolves to an explicit failed snapshot and
    /// is never waited on again.
    pub fn snapshot(&self) -> &ContextSnapshot {
        if let Some(snapshot) = self.resolved.get() {
            return snapshot;
        }

        let snapshot = match self.join_start_capture() {
            Some(start) => {
                let policy = self.requested_policy.clamp_to(self.policy_ceiling);
                ContextSnapshot::assemble(
                    self.requested_policy,
                    self.policy_ceiling,
                    start,
                    read_application(policy, self.options),
                )
            }
            None => ContextSnapshot::unavailable(
                self.requested_policy,
                self.policy_ceiling,
                ContextSourceStatus::Failed,
            ),
        };
        let _ = self.resolved.set(snapshot);
        match self.resolved.get() {
            Some(snapshot) => snapshot,
            None => unreachable!("context snapshot is set before it is read"),
        }
    }

    fn join_start_capture(&self) -> Option<StartCapture> {
        let mut inbox = self.inbox.lock().ok()?;
        match inbox.as_ref()?.recv_timeout(CAPTURE_JOIN_TIMEOUT) {
            Ok(start) => Some(start),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                // Stop waiting on a read that missed its window; a later reader
                // must not pay the wait again.
                inbox.take();
                None
            }
        }
    }
}

/// Starts a capture for one run. Returns immediately: the record-start read
/// happens on a worker so a wedged target application cannot delay recording.
/// The effective policy is computed before the worker exists, which is the
/// privacy boundary that prevents a later settings edit from widening a live
/// run.
pub fn start_capture(
    requested_policy: ContextPolicy,
    policy_ceiling: ContextPolicy,
    options: CaptureOptions,
    project_root: Option<std::path::PathBuf>,
) -> PendingContext {
    start_capture_with_sources(
        requested_policy,
        policy_ceiling,
        options,
        clipboard_recency::observe_clipboard_generation,
        move |policy, options, generation| {
            let mut start = read_start(policy, options, generation);
            start.project = project::read(policy, project_root.as_deref());
            start
        },
        now_ms,
    )
}

/// Shared capture boundary. Production supplies the platform readers; tests
/// supply deterministic sources without reading a user's frontmost app.
fn start_capture_with_sources<G, R, N>(
    requested_policy: ContextPolicy,
    policy_ceiling: ContextPolicy,
    options: CaptureOptions,
    observe_generation: G,
    read_start: R,
    now_ms: N,
) -> PendingContext
where
    G: FnOnce() -> clipboard_recency::Generation,
    R: FnOnce(ContextPolicy, CaptureOptions, clipboard_recency::Generation) -> StartCapture
        + Send
        + 'static,
    N: FnOnce() -> u64 + Send + 'static,
{
    let policy = requested_policy.clamp_to(policy_ceiling);
    if matches!(policy, ContextPolicy::None) {
        return PendingContext::resolved(ContextSnapshot::assemble(
            requested_policy,
            policy_ceiling,
            StartCapture {
                captured_at_ms: now_ms(),
                ..StartCapture::default()
            },
            ApplicationCapture::default(),
        ));
    }

    let (tx, rx) = mpsc::sync_channel(1);
    let generation = observe_generation();
    let spawned = std::thread::Builder::new()
        .name("sona-context-capture".to_string())
        .spawn(move || {
            let captured_at_ms = now_ms();
            let started = Instant::now();
            let mut start = read_start(policy, options, generation);
            start.captured_at_ms = captured_at_ms;
            // The per-read budget bounds this at 400 ms. Logging it is how a
            // capture that ran long stays visible without a stopwatch.
            log::debug!("Context capture read finished in {:?}", started.elapsed());
            let _ = tx.send(start);
        });

    match spawned {
        Ok(_) => PendingContext::awaiting(requested_policy, policy_ceiling, options, rx),
        Err(error) => {
            log::warn!("Could not start context capture: {error}");
            PendingContext::resolved(ContextSnapshot::unavailable(
                requested_policy,
                policy_ceiling,
                ContextSourceStatus::Failed,
            ))
        }
    }
}

/// Context sources reachable on this build, for settings and diagnostics.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ContextDiagnostics {
    pub accessibility: AccessibilityAccess,
    /// Reading the frontmost application's name and identifier.
    pub target_identity: ContextSourceStatus,
    /// Reading the focused control's contents.
    pub focused_field: ContextSourceStatus,
    /// Reading the current selection.
    pub selected_text: ContextSourceStatus,
    /// Reading the frontmost browser's page URL.
    pub browser_url: ContextSourceStatus,
    /// Reading recently changed clipboard text.
    pub clipboard: ContextSourceStatus,
    pub url_capture_enabled: bool,
}
/// Reports what context capture could do right now. Read-only and
/// non-prompting: opening a settings pane must never raise a system permission
/// dialog.
#[tauri::command]
#[specta::specta]
pub fn get_context_diagnostics(app: tauri::AppHandle) -> ContextDiagnostics {
    let settings = crate::settings::get_settings(&app);
    diagnostics(
        platform_accessibility(),
        settings.context_url_capture_enabled,
        clipboard_recency::watch_enabled(),
    )
}

fn diagnostics(
    accessibility: AccessibilityAccess,
    url_capture_enabled: bool,
    clipboard_watch_enabled: bool,
) -> ContextDiagnostics {
    let accessibility_gated = match accessibility {
        AccessibilityAccess::Granted => ContextSourceStatus::Captured,
        AccessibilityAccess::Denied => ContextSourceStatus::PermissionDenied,
        AccessibilityAccess::Unsupported => ContextSourceStatus::Unsupported,
    };
    let target_identity = ContextSourceStatus::Captured;
    // A source the platform reader cannot answer at all stays unsupported even
    // when accessibility is granted, so the pane never offers a toggle that
    // could not change the outcome.
    let browser_url = match (
        SUPPORTS_BROWSER_URL,
        accessibility_gated,
        url_capture_enabled,
    ) {
        (false, _, _) => ContextSourceStatus::Unsupported,
        (true, ContextSourceStatus::Captured, false) => ContextSourceStatus::Disabled,
        (true, status, _) => status,
    };
    let clipboard = match (
        SUPPORTS_CLIPBOARD_RECENCY,
        accessibility_gated,
        clipboard_watch_enabled,
    ) {
        (false, _, _) | (_, ContextSourceStatus::Unsupported, _) => {
            ContextSourceStatus::Unsupported
        }
        (_, _, false) => ContextSourceStatus::NotRequested,
        (_, _, true) => ContextSourceStatus::Captured,
    };

    ContextDiagnostics {
        accessibility,
        target_identity,
        focused_field: accessibility_gated,
        selected_text: accessibility_gated,
        browser_url,
        clipboard,
        url_capture_enabled,
    }
}

/// Reads the sources whose value is only true at record start. Each stage owns
/// its own budget, so a wedged application costs one bounded attempt per stage
/// instead of one shared allowance.
fn read_start(
    policy: ContextPolicy,
    options: CaptureOptions,
    clipboard_generation: clipboard_recency::Generation,
) -> StartCapture {
    platform::read_start(
        policy,
        options,
        clipboard_generation,
        deadline::CaptureDeadline::starting_now(),
    )
}

/// Reads the frontmost application and its focused control. Called immediately
/// before the step that consumes the context, never at record start.
fn read_application(policy: ContextPolicy, options: CaptureOptions) -> ApplicationCapture {
    if !policy.wants_target() {
        return ApplicationCapture::default();
    }
    let started = Instant::now();
    let application = platform::read_application(
        policy,
        options,
        now_ms(),
        deadline::CaptureDeadline::starting_now(),
    );
    log::debug!(
        "Application context read finished in {:?}",
        started.elapsed()
    );
    application
}

/// Non-prompting accessibility check.
pub fn platform_accessibility() -> AccessibilityAccess {
    platform::accessibility_access()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// Caps a captured value without splitting a character.
fn truncate_on_boundary(mut text: String) -> String {
    if text.len() <= MAX_SOURCE_BYTES {
        return text;
    }
    let mut cut = MAX_SOURCE_BYTES;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_start() -> StartCapture {
        StartCapture {
            captured_at_ms: 7,
            accessibility: AccessibilityAccess::Granted,
            selected_text: SourceOutcome::Captured("private selection contents".to_string()),
            clipboard: SourceOutcome::Captured("copied".to_string()),
            project: SourceOutcome::Captured(ProjectContext {
                files: vec!["src/ChatStore.swift".to_string()],
                identifiers: vec!["ChatStore".to_string()],
                truncated: false,
            }),
        }
    }

    fn full_application() -> ApplicationCapture {
        ApplicationCapture {
            captured_at_ms: Some(9),
            application_name: Some("Mail".to_string()),
            application_identifier: Some("com.apple.mail".to_string()),
            target: SourceOutcome::Captured("com.apple.mail".to_string()),
            focused_field_name: Some("Message body".to_string()),
            focused_field: SourceOutcome::Captured("draft body".to_string()),
            browser_url: SourceOutcome::Captured("https://example.test/a".to_string()),
        }
    }

    fn snapshot(requested: ContextPolicy, ceiling: ContextPolicy) -> ContextSnapshot {
        ContextSnapshot::assemble(requested, ceiling, full_start(), full_application())
    }
    #[test]
    fn full_capture_admits_only_the_frozen_fresh_preroll_in_stable_packet_order() {
        let options = CaptureOptions {
            url_capture_enabled: false,
            clipboard_preroll_ms: DEFAULT_CLIPBOARD_PREROLL_MS,
        };
        let generation = clipboard_recency::generation_for_test(9, 2_000);
        let reader_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reader_calls_for_worker = reader_calls.clone();

        let pending = start_capture_with_sources(
            ContextPolicy::Full,
            ContextPolicy::Full,
            options,
            || generation,
            move |policy, frozen_options, _| {
                reader_calls_for_worker.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                assert_eq!(policy, ContextPolicy::Full);
                assert_eq!(
                    frozen_options, options,
                    "the worker receives the run-start options"
                );
                StartCapture {
                    accessibility: AccessibilityAccess::Granted,
                    selected_text: SourceOutcome::Captured("selected at record start".to_string()),
                    clipboard: clipboard_recency::read_if_fresh(
                        generation,
                        5_000,
                        frozen_options.clipboard_preroll_ms,
                        || Some(9),
                        || Some("copied inside the frozen pre-roll".to_string()),
                    ),
                    ..StartCapture::default()
                }
            },
            || 5_000,
        );
        let start = pending
            .join_start_capture()
            .expect("controlled worker returns start capture");
        let snapshot = ContextSnapshot::assemble(
            ContextPolicy::Full,
            ContextPolicy::Full,
            start,
            ApplicationCapture::default(),
        );
        let packet = snapshot.packet();

        assert_eq!(reader_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            packet.selected_text.as_deref(),
            Some("selected at record start")
        );
        assert_eq!(
            packet.clipboard_content.as_deref(),
            Some("copied inside the frozen pre-roll")
        );
        assert_eq!(snapshot.receipt().captured_at_ms, 5_000);
        let serialized = serde_json::to_string(packet).expect("context packet serializes");
        assert!(
            serialized
                .find("\"selected_text\"")
                .expect("selected-text field")
                < serialized
                    .find("\"clipboard_content\"")
                    .expect("clipboard field"),
            "packet ordering stays stable for prompt construction"
        );
    }

    #[test]
    fn a_none_ceiling_skips_generation_observation_and_the_context_worker() {
        let options = CaptureOptions::default();
        let generation_reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reader_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let generation_reads_for_capture = generation_reads.clone();
        let reader_calls_for_capture = reader_calls.clone();

        let pending = start_capture_with_sources(
            ContextPolicy::Full,
            ContextPolicy::None,
            options,
            move || {
                generation_reads_for_capture.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                clipboard_recency::Generation::default()
            },
            move |_, _, _| {
                reader_calls_for_capture.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                StartCapture::default()
            },
            || 7_000,
        );
        let snapshot = pending.snapshot();

        assert_eq!(
            generation_reads.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(reader_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(snapshot.packet(), &ContextPacket::default());
        assert_eq!(snapshot.receipt().captured_at_ms, 7_000);
        assert_eq!(
            snapshot.receipt().sources.clipboard,
            ContextSourceStatus::DisabledByCeiling
        );

        let public_capture = start_capture(ContextPolicy::Full, ContextPolicy::None, options, None);
        let public_snapshot = public_capture.snapshot();
        assert_eq!(public_snapshot.packet(), &ContextPacket::default());
    }

    #[test]
    fn policy_none_captures_nothing_and_claims_nothing() {
        let snapshot = snapshot(ContextPolicy::None, ContextPolicy::Full);
        assert_eq!(snapshot.packet(), &ContextPacket::default());
        assert_eq!(snapshot.receipt().sources, ContextSources::default());
        assert_eq!(snapshot.receipt().captured_at_ms, 7);
    }

    #[test]
    fn target_policy_drops_body_selection_and_clipboard() {
        let snapshot = snapshot(ContextPolicy::Target, ContextPolicy::Full);
        let packet = snapshot.packet();
        assert_eq!(
            packet.target.application_identifier.as_deref(),
            Some("com.apple.mail")
        );
        assert_eq!(packet.target.url.as_deref(), Some("https://example.test/a"));
        assert_eq!(packet.selected_text, None);
        assert_eq!(packet.focused_element_content, None);
        assert_eq!(packet.focused_element_name, None);
        assert_eq!(packet.clipboard_content, None);
        assert_eq!(packet.project, None);

        let sources = snapshot.receipt().sources;
        assert_eq!(sources.target, ContextSourceStatus::Captured);
        assert_eq!(sources.selected_text, ContextSourceStatus::NotRequested);
        assert_eq!(sources.focused_field, ContextSourceStatus::NotRequested);
        assert_eq!(sources.clipboard, ContextSourceStatus::NotRequested);
    }

    #[test]
    fn ceiling_blocks_full_mode_before_disallowed_values_enter_the_packet() {
        let snapshot = snapshot(ContextPolicy::Full, ContextPolicy::Target);
        let packet = snapshot.packet();
        assert_eq!(snapshot.receipt().requested_policy, ContextPolicy::Full);
        assert_eq!(snapshot.receipt().policy, ContextPolicy::Target);
        assert_eq!(
            packet.target.application_identifier.as_deref(),
            Some("com.apple.mail")
        );
        assert_eq!(packet.selected_text, None);
        assert_eq!(packet.focused_element_content, None);
        assert_eq!(packet.clipboard_content, None);
        assert_eq!(packet.project, None);
        assert_eq!(
            snapshot.receipt().sources.project,
            ContextSourceStatus::DisabledByCeiling
        );
        assert_eq!(
            snapshot.receipt().sources.selected_text,
            ContextSourceStatus::DisabledByCeiling
        );
        assert_eq!(
            snapshot.receipt().sources.focused_field,
            ContextSourceStatus::DisabledByCeiling
        );
        assert_eq!(
            snapshot.receipt().sources.clipboard,
            ContextSourceStatus::DisabledByCeiling
        );
    }

    #[test]
    fn refused_sources_are_reported_by_reason_not_as_empty_success() {
        let start = StartCapture {
            captured_at_ms: 7,
            accessibility: AccessibilityAccess::Denied,
            selected_text: SourceOutcome::Unavailable(ContextSourceStatus::PermissionDenied),
            clipboard: SourceOutcome::Unavailable(ContextSourceStatus::Stale),
            project: SourceOutcome::default(),
        };
        let application = ApplicationCapture {
            captured_at_ms: Some(9),
            application_name: Some("Terminal".to_string()),
            target: SourceOutcome::Captured("com.apple.Terminal".to_string()),
            focused_field: SourceOutcome::Unavailable(ContextSourceStatus::SecureField),
            browser_url: SourceOutcome::Unavailable(ContextSourceStatus::Disabled),
            ..ApplicationCapture::default()
        };
        let snapshot =
            ContextSnapshot::assemble(ContextPolicy::Full, ContextPolicy::Full, start, application);
        let sources = snapshot.receipt().sources;
        assert_eq!(sources.focused_field, ContextSourceStatus::SecureField);
        assert_eq!(sources.selected_text, ContextSourceStatus::PermissionDenied);
        assert_eq!(sources.browser_url, ContextSourceStatus::Disabled);
        assert_eq!(sources.clipboard, ContextSourceStatus::Stale);
        assert_eq!(
            snapshot.receipt().accessibility,
            AccessibilityAccess::Denied
        );
        assert_eq!(snapshot.packet().focused_element_content, None);
    }

    #[test]
    fn secure_fields_exclude_browser_urls_with_their_contents() {
        let mut start = full_start();
        start.selected_text = SourceOutcome::Unavailable(ContextSourceStatus::SecureField);
        let mut application = full_application();
        application.focused_field = SourceOutcome::Unavailable(ContextSourceStatus::SecureField);
        application.browser_url = SourceOutcome::Unavailable(ContextSourceStatus::SecureField);

        let snapshot = ContextSnapshot::assemble(
            ContextPolicy::Target,
            ContextPolicy::Full,
            start,
            application,
        );

        assert_eq!(
            snapshot.receipt().sources.browser_url,
            ContextSourceStatus::SecureField
        );
        assert_eq!(snapshot.packet().target.url, None);
    }

    #[test]
    fn blank_reads_are_empty_not_captured() {
        assert_eq!(
            SourceOutcome::read(Some("   ".to_string())),
            SourceOutcome::Unavailable(ContextSourceStatus::Empty)
        );
        assert_eq!(
            SourceOutcome::read(None),
            SourceOutcome::Unavailable(ContextSourceStatus::Empty)
        );
        assert_eq!(
            SourceOutcome::read(Some("x".to_string())),
            SourceOutcome::Captured("x".to_string())
        );
    }

    #[test]
    fn website_hosts_are_normalized_and_rule_inputs_reject_urls() {
        assert_eq!(
            normalize_website_host(" Docs.Example.COM. "),
            Some("docs.example.com".to_string())
        );
        assert_eq!(
            website_host_from_url("https://docs.example.com/path?query=1"),
            Some("docs.example.com".to_string())
        );

        for invalid in [
            "",
            "https://example.com",
            "example.com/path",
            "example.com:443",
            "user@example.com",
        ] {
            assert_eq!(normalize_website_host(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn oversized_reads_are_capped_on_a_character_boundary() {
        let value = "é".repeat(MAX_SOURCE_BYTES);
        let SourceOutcome::Captured(captured) = SourceOutcome::read(Some(value)) else {
            panic!("a long value is captured");
        };
        assert!(captured.len() <= MAX_SOURCE_BYTES);
        assert!(captured.chars().all(|c| c == 'é'));
    }

    #[test]
    fn receipt_carries_no_captured_text() {
        let snapshot = snapshot(ContextPolicy::Full, ContextPolicy::Full);
        let serialized = serde_json::to_string(snapshot.receipt()).unwrap();
        for secret in [
            "draft body",
            "private selection contents",
            "copied",
            "example.test",
            "com.apple.mail",
        ] {
            assert!(
                !serialized.contains(secret),
                "receipt leaked {secret}: {serialized}"
            );
        }
    }

    #[test]
    fn unavailable_snapshot_reports_the_reason_for_every_requested_source() {
        let snapshot = ContextSnapshot::unavailable(
            ContextPolicy::Full,
            ContextPolicy::Full,
            ContextSourceStatus::Failed,
        );
        let sources = snapshot.receipt().sources;
        assert_eq!(sources.target, ContextSourceStatus::Failed);
        assert_eq!(sources.focused_field, ContextSourceStatus::Failed);
        assert_eq!(sources.selected_text, ContextSourceStatus::Failed);
        assert_eq!(sources.clipboard, ContextSourceStatus::Failed);
        assert_eq!(snapshot.packet(), &ContextPacket::default());
    }

    /// The two stages are timestamped separately: the record-start read keeps
    /// the instant the user began speaking, and the application read keeps the
    /// later instant it actually looked at the frontmost window.
    #[test]
    fn the_receipt_dates_each_capture_stage_separately() {
        let receipt = snapshot(ContextPolicy::Full, ContextPolicy::Full)
            .receipt()
            .clone();
        assert_eq!(receipt.captured_at_ms, 7);
        assert_eq!(receipt.application_captured_at_ms, Some(9));
    }

    /// A policy that asks for no application context never runs the second
    /// stage, and the receipt says so instead of dating a read that never
    /// happened.
    #[test]
    fn a_skipped_application_stage_is_undated() {
        let receipt = ContextSnapshot::assemble(
            ContextPolicy::None,
            ContextPolicy::None,
            full_start(),
            ApplicationCapture::default(),
        )
        .receipt()
        .clone();
        assert_eq!(receipt.application_captured_at_ms, None);
    }

    /// The whole point of the split: the application context is read when the
    /// run first needs its snapshot, not when the run started. A delay between
    /// starting the capture and reading it therefore shows up as a gap between
    /// the two stamps — that gap is time the user spent speaking, during which
    /// they may have switched windows.
    #[test]
    fn the_application_stage_is_read_when_the_snapshot_is_first_needed() {
        let pending = start_capture(
            ContextPolicy::Target,
            ContextPolicy::Target,
            CaptureOptions::default(),
            None,
        );
        std::thread::sleep(Duration::from_millis(20));

        let receipt = pending.snapshot().receipt().clone();
        let application_at = receipt
            .application_captured_at_ms
            .expect("a target policy reads the application");
        assert!(
            application_at.saturating_sub(receipt.captured_at_ms) >= 10,
            "the application stage ran at record start: {} vs {}",
            application_at,
            receipt.captured_at_ms
        );
    }

    #[test]
    fn a_pending_capture_resolves_once_and_stays_frozen() {
        let (tx, rx) = mpsc::sync_channel(1);
        let pending = PendingContext::awaiting(
            ContextPolicy::Target,
            ContextPolicy::Full,
            CaptureOptions::default(),
            rx,
        );
        tx.send(StartCapture {
            captured_at_ms: 11,
            ..full_start()
        })
        .unwrap();

        let first = pending.snapshot().clone();
        assert_eq!(first.receipt().captured_at_ms, 11);
        tx.send(StartCapture {
            captured_at_ms: 99,
            ..StartCapture::default()
        })
        .unwrap();
        assert_eq!(pending.snapshot(), &first);
    }

    #[test]
    fn a_capture_that_never_answers_resolves_to_an_explicit_failure() {
        let (tx, rx) = mpsc::sync_channel::<StartCapture>(1);
        drop(tx);
        let pending = PendingContext::awaiting(
            ContextPolicy::Full,
            ContextPolicy::Full,
            CaptureOptions::default(),
            rx,
        );
        let snapshot = pending.snapshot();
        assert_eq!(
            snapshot.receipt().sources.selected_text,
            ContextSourceStatus::Failed
        );
        assert_eq!(snapshot.packet(), &ContextPacket::default());
    }

    #[test]
    fn diagnostics_separate_permission_from_opt_in_and_platform() {
        let granted = diagnostics(AccessibilityAccess::Granted, false, false);
        assert_eq!(granted.selected_text, ContextSourceStatus::Captured);
        assert_eq!(granted.browser_url, ContextSourceStatus::Disabled);
        assert_eq!(granted.clipboard, ContextSourceStatus::NotRequested);

        let denied = diagnostics(AccessibilityAccess::Denied, true, true);
        assert_eq!(denied.focused_field, ContextSourceStatus::PermissionDenied);
        assert_eq!(denied.browser_url, ContextSourceStatus::PermissionDenied);

        let unsupported = diagnostics(AccessibilityAccess::Unsupported, true, true);
        assert_eq!(unsupported.focused_field, ContextSourceStatus::Unsupported);
        assert_eq!(unsupported.clipboard, ContextSourceStatus::Unsupported);
    }
}
