//! Opt-in names from one explicitly selected call, independent of voice matching.

pub(crate) mod mapping;

use super::clock::host_monotonic_now_ns;
use super::store::{MeetingStore, StoreError};
use super::types::{MeetingPhase, MeetingRunPlan, SourceHealth, SourceKind, SpeakerId};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub(crate) const MAX_CALL_SAMPLES: usize = 4096;
pub(crate) const MAX_OBSERVATION_GAP_NS: u64 = 2_000_000_000;

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct CallNameTarget {
    pub id: String,
    pub label: String,
    pub bundle_id: String,
    pub provider: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct CallNameTargets {
    pub state: String,
    pub detail: String,
    pub targets: Vec<CallNameTarget>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CallParticipant {
    pub id: String,
    pub name: String,
    pub is_local: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CallObservation {
    pub state: String,
    pub detail: String,
    pub participants: Vec<CallParticipant>,
    pub active_ids: Vec<String>,
}

impl CallObservation {
    fn unavailable(detail: &str) -> Self {
        Self {
            state: "unavailable".into(),
            detail: detail.into(),
            participants: Vec::new(),
            active_ids: Vec::new(),
        }
    }

    pub(crate) fn valid(&self) -> bool {
        matches!(
            self.state.as_str(),
            "reading" | "roster_only" | "unavailable"
        ) && self.participants.len() <= 128
            && self.active_ids.len() <= 128
            && self.detail.len() <= 1024
            && self.participants.iter().all(|person| {
                !person.id.is_empty()
                    && person.id.len() <= 64
                    && !person.name.is_empty()
                    && person.name.len() <= 256
            })
            && self
                .active_ids
                .iter()
                .all(|id| self.participants.iter().any(|person| person.id == *id))
    }
}

pub(crate) struct CallSample {
    pub start_ns: u64,
    pub end_ns: u64,
    pub observation: CallObservation,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct CallNameSuggestion {
    pub speaker_id: SpeakerId,
    pub display_name: String,
    pub overlap_ns: u64,
    pub speech_ns: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct CallNameStatus {
    pub enabled: bool,
    pub automatically_use: bool,
    pub state: String,
    pub detail: String,
    pub target: Option<CallNameTarget>,
    pub roster: Vec<String>,
    pub suggestions: Vec<CallNameSuggestion>,
}

impl Default for CallNameStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            automatically_use: false,
            state: "off".into(),
            detail: "Call names are off. Nothing is read until you choose a call.".into(),
            target: None,
            roster: Vec::new(),
            suggestions: Vec::new(),
        }
    }
}

pub(crate) fn targets(plan: &MeetingRunPlan) -> CallNameTargets {
    if !plan.requested_sources.contains(&SourceKind::SystemAudio) {
        return CallNameTargets {
            state: "unavailable".into(),
            detail: "This recording does not capture call audio.".into(),
            targets: Vec::new(),
        };
    }
    let mut result = platform::targets();
    let allowed = &plan.frozen_system_audio_application_bundle_ids;
    if !allowed.is_empty() {
        result.targets.retain(|target| {
            allowed
                .iter()
                .any(|bundle| bundle.eq_ignore_ascii_case(&target.bundle_id))
        });
        if result.targets.is_empty() {
            result.state = "unavailable".into();
            result.detail =
                "No readable joined call belongs to this recording's selected audio app.".into();
        }
    }
    result
}

pub(crate) fn selected_target(plan: &MeetingRunPlan, id: &str) -> Option<CallNameTarget> {
    if !plan.requested_sources.contains(&SourceKind::SystemAudio) {
        return None;
    }
    platform::target(id).filter(|target| {
        plan.frozen_system_audio_application_bundle_ids.is_empty()
            || plan
                .frozen_system_audio_application_bundle_ids
                .iter()
                .any(|bundle| bundle.eq_ignore_ascii_case(&target.bundle_id))
    })
}

/// Owned by ActiveCapture. Its drop joins the only reader, including failed
/// stops and discards. It never changes the audio clock or capture windows.
pub(crate) struct CallNameSampler {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl CallNameSampler {
    pub(crate) fn start(
        store: Arc<MeetingStore>,
        plan: MeetingRunPlan,
        target: CallNameTarget,
        automatically_use: bool,
    ) -> Result<Self, StoreError> {
        store.begin_call_names(&plan, &target, automatically_use)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let session_id = plan.session_id;
        let failed_store = Arc::clone(&store);
        let handle = thread::Builder::new()
            .name("call-names".into())
            .spawn(move || {
                let result = sample_call(&store, &plan, &target.id, &stopping);
                platform::release(&target.id);
                if let Err(error) = result {
                    log::warn!("Call-name sampling stopped: {error:?}");
                    let _ = store.finish_call_names(
                        session_id,
                        "unavailable",
                        "Call names stopped because the recording could not be read or saved.",
                    );
                }
            })
            .map_err(|_| {
                let _ = failed_store.finish_call_names(
                    session_id,
                    "unavailable",
                    "The call-name reader could not start.",
                );
                StoreError::Unavailable
            })?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for CallNameSampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            if handle.join().is_err() {
                log::warn!("Call-name reader ended unexpectedly");
            }
        }
    }
}

fn sample_call(
    store: &MeetingStore,
    plan: &MeetingRunPlan,
    target: &str,
    stop: &AtomicBool,
) -> Result<(), StoreError> {
    let mut last_observation: Option<CallObservation> = None;
    let mut last_ns: Option<u64> = None;
    while !stop.load(Ordering::Acquire) {
        let snapshot = store.session_snapshot(plan.session_id)?;
        match snapshot.phase {
            MeetingPhase::CapturingPaused
            | MeetingPhase::CapturingPausing
            | MeetingPhase::CapturingResuming => {
                store.call_names_state(
                    plan.session_id,
                    "paused",
                    "Call names are paused with the recording.",
                )?;
                last_ns = None;
                last_observation = None;
                thread::park_timeout(Duration::from_secs(1));
                continue;
            }
            MeetingPhase::CapturingRecording => {}
            _ => break,
        }
        if !snapshot.sources.iter().any(|source| {
            source.source_kind == SourceKind::SystemAudio
                && matches!(
                    source.health,
                    SourceHealth::Healthy | SourceHealth::Degraded
                )
        }) {
            return store.finish_call_names(
                plan.session_id,
                "unavailable",
                "Call audio is unavailable. Reading names has stopped.",
            );
        }
        let before = host_monotonic_now_ns();
        let observation = platform::sample(target);
        let now = host_monotonic_now_ns();
        if stop.load(Ordering::Acquire) {
            break;
        }
        if !observation.valid() || now.saturating_sub(before) > MAX_OBSERVATION_GAP_NS {
            return store.finish_call_names(
                plan.session_id,
                "unavailable",
                "The call's participant tree did not answer reliably.",
            );
        }
        let offset_ns = now
            .checked_sub(plan.session_clock_anchor.host_monotonic_anchor_ns)
            .ok_or(StoreError::Invalid)?;
        let continuous = last_ns
            .is_some_and(|last| offset_ns > last && offset_ns - last <= MAX_OBSERVATION_GAP_NS);
        let unchanged = continuous && last_observation.as_ref() == Some(&observation);
        match store.record_call_observation(
            plan.session_id,
            offset_ns,
            &observation,
            continuous,
            unchanged,
        ) {
            Ok(true) => {}
            Ok(false) => return store.finish_call_names(
                plan.session_id,
                "limit_reached",
                "This recording has reached its call-name history limit. Earlier names are kept.",
            ),
            Err(StoreError::Conflict) => {
                // Pause or stop can win while AX is answering. That late
                // observation belongs to no recorded interval.
                last_ns = None;
                last_observation = None;
                continue;
            }
            Err(error) => return Err(error),
        }
        if observation.state == "unavailable" {
            return store.finish_call_names(
                plan.session_id,
                &observation.state,
                &observation.detail,
            );
        }
        last_ns = Some(offset_ns);
        last_observation = Some(observation);
        thread::park_timeout(Duration::from_secs(1));
    }
    store.finish_call_names(
        plan.session_id,
        "stopped",
        "Call-name reading has stopped. Recorded names are available in review.",
    )
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::os::raw::c_char;

    extern "C" {
        fn sona_call_roster_targets_json() -> *mut c_char;
        fn sona_call_roster_target_json(target: *const c_char) -> *mut c_char;
        fn sona_call_roster_sample_json(target: *const c_char) -> *mut c_char;
        fn sona_call_roster_release(target: *const c_char);
        fn sona_call_roster_free_string(value: *mut c_char);
    }

    // SAFETY: callers pass only the bridge's nullable, owned strdup result.
    // The bridge's matching free consumes it exactly once after JSON decoding.
    unsafe fn decode<T: serde::de::DeserializeOwned>(pointer: *mut c_char) -> Option<T> {
        if pointer.is_null() {
            return None;
        }
        let result = serde_json::from_slice(CStr::from_ptr(pointer).to_bytes()).ok();
        sona_call_roster_free_string(pointer);
        result
    }

    pub(super) fn targets() -> CallNameTargets {
        // SAFETY: the linked Swift bridge returns the owned string decode expects.
        unsafe { decode(sona_call_roster_targets_json()) }.unwrap_or_else(|| CallNameTargets {
            state: "unavailable".into(),
            detail: "The call-name reader did not answer.".into(),
            targets: Vec::new(),
        })
    }

    pub(super) fn target(id: &str) -> Option<CallNameTarget> {
        let id = CString::new(id).ok()?;
        // SAFETY: id lives through the call; decode owns the returned bridge string.
        unsafe { decode(sona_call_roster_target_json(id.as_ptr())) }
    }

    pub(super) fn sample(target: &str) -> CallObservation {
        let Ok(target) = CString::new(target) else {
            return CallObservation::unavailable("Invalid call selection.");
        };
        // SAFETY: target lives through the call; decode owns the returned bridge string.
        unsafe { decode(sona_call_roster_sample_json(target.as_ptr())) }
            .unwrap_or_else(|| CallObservation::unavailable("The call-name reader did not answer."))
    }

    pub(super) fn release(target: &str) {
        if let Ok(target) = CString::new(target) {
            // SAFETY: Swift copies the live C string and removes only its registry entry.
            unsafe { sona_call_roster_release(target.as_ptr()) };
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;
    pub(super) fn targets() -> CallNameTargets {
        CallNameTargets {
            state: "unavailable".into(),
            detail: "Reading call names requires macOS Accessibility.".into(),
            targets: Vec::new(),
        }
    }
    pub(super) fn target(_: &str) -> Option<CallNameTarget> {
        None
    }
    pub(super) fn sample(_: &str) -> CallObservation {
        CallObservation::unavailable("Reading call names requires macOS Accessibility.")
    }
    pub(super) fn release(_: &str) {}
}
