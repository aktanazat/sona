pub use suggestion_observer::{MacosMeetingSuggestionObserver, MacosSuggestionObserverError};
pub use system_audio::MacosSystemAudioCapture;

mod suggestion_observer {
    use crate::meeting::detection::apps::{BrowserTitleRead, BrowserTitleReader};
    use crate::meeting::suggestions::{
        MeetingEvidenceFlags, MeetingProvider, MeetingSuggestionSignal, MeetingSuggestionSink,
    };
    use std::collections::BTreeSet;
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::ptr;
    use std::sync::Arc;

    const BRIDGE_OK: c_int = 0;
    const MAXIMUM_CONFIGURED_APPLICATIONS: usize = 64;
    const SUGGESTION_AX_UNAVAILABLE: u32 = 1;

    type SuggestionCallback = unsafe extern "C" fn(
        context: *mut c_void,
        app_bundle_id: *const c_char,
        title: *const c_char,
        url_host: *const c_char,
        evidence_flags: u32,
        observed_at_ns: u64,
    );

    extern "C" {
        fn sona_meeting_suggestions_start(
            configured_bundle_ids: *const c_void,
            configured_bundle_id_count: usize,
            callback: Option<SuggestionCallback>,
            callback_context: *mut c_void,
            out_handle: *mut *mut c_void,
        ) -> c_int;
        fn sona_meeting_suggestions_refresh(handle: *mut c_void, bundle_id: *const c_char) -> u32;
        fn sona_meeting_suggestions_stop(handle: *mut c_void);
    }

    /// Failure while registering the macOS activation observer.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum MacosSuggestionObserverError {
        InvalidConfiguredApplication,
        ObserverUnavailable,
    }

    /// Owns suggestion-only NSWorkspace activation observation on macOS.
    ///
    /// The observer can submit only content-free normalized suggestion signals.
    pub struct MacosMeetingSuggestionObserver {
        handle: Option<usize>,
        _callback_state: Arc<SuggestionCallbackState>,
    }

    struct SuggestionCallbackState {
        configured_application_ids: BTreeSet<String>,
        sink: Arc<dyn MeetingSuggestionSink>,
    }

    struct CBundleIds {
        _owned: Vec<CString>,
        pointers: Vec<*const c_char>,
    }

    impl CBundleIds {
        fn from_bundle_ids(bundle_ids: &[String]) -> Result<Self, MacosSuggestionObserverError> {
            if bundle_ids.len() > MAXIMUM_CONFIGURED_APPLICATIONS {
                return Err(MacosSuggestionObserverError::InvalidConfiguredApplication);
            }

            let mut owned = Vec::with_capacity(bundle_ids.len());
            for bundle_id in bundle_ids {
                if bundle_id.is_empty() {
                    return Err(MacosSuggestionObserverError::InvalidConfiguredApplication);
                }
                owned.push(
                    CString::new(bundle_id.as_str())
                        .map_err(|_| MacosSuggestionObserverError::InvalidConfiguredApplication)?,
                );
            }
            let pointers = owned.iter().map(|bundle_id| bundle_id.as_ptr()).collect();
            Ok(Self {
                _owned: owned,
                pointers,
            })
        }

        fn as_argument(&self) -> *const c_void {
            if self.pointers.is_empty() {
                ptr::null()
            } else {
                self.pointers.as_ptr().cast()
            }
        }
    }

    impl MacosMeetingSuggestionObserver {
        pub fn start(
            configured_application_ids: &[String],
            sink: Arc<dyn MeetingSuggestionSink>,
        ) -> Result<Self, MacosSuggestionObserverError> {
            let configured_ids = CBundleIds::from_bundle_ids(configured_application_ids)?;
            let callback_state = Arc::new(SuggestionCallbackState {
                configured_application_ids: configured_application_ids
                    .iter()
                    .map(|bundle_id| bundle_id.to_ascii_lowercase())
                    .collect(),
                sink,
            });
            let callback_context = Arc::as_ptr(&callback_state).cast_mut().cast::<c_void>();
            let mut raw_handle = ptr::null_mut();
            // Swift copies configured IDs before this call returns.
            // SAFETY: callback_state remains live until Swift removes and drains the observer.
            let result = unsafe {
                sona_meeting_suggestions_start(
                    configured_ids.as_argument(),
                    configured_ids.pointers.len(),
                    Some(meeting_suggestion_callback),
                    callback_context,
                    &mut raw_handle,
                )
            };
            if result != BRIDGE_OK || raw_handle.is_null() {
                return Err(MacosSuggestionObserverError::ObserverUnavailable);
            }

            Ok(Self {
                handle: Some(raw_handle.addr()),
                _callback_state: callback_state,
            })
        }

        pub fn stop(&mut self) {
            let Some(handle) = self.handle.take() else {
                return;
            };
            // This observer owns the retained Swift handle at this address.
            // SAFETY: Swift marks its queue inactive and drains it before returning.
            unsafe { sona_meeting_suggestions_stop(ptr::with_exposed_provenance_mut(handle)) };
        }
    }

    impl Drop for MacosMeetingSuggestionObserver {
        fn drop(&mut self) {
            self.stop();
        }
    }

    impl BrowserTitleReader for MacosMeetingSuggestionObserver {
        /// The read goes through the same Swift observer and the same callback
        /// the activation edge uses, so what it produces is the same
        /// content-free signal, normalized by the same rules.
        fn refresh_frontmost(&self, bundle_id: &str) -> BrowserTitleRead {
            let Some(handle) = self.handle else {
                return BrowserTitleRead::Unreadable;
            };
            let Ok(bundle_id) = CString::new(bundle_id) else {
                return BrowserTitleRead::Unreadable;
            };
            // SAFETY: the handle is retained until `stop`; the C string outlives the call.
            let flags = unsafe {
                sona_meeting_suggestions_refresh(
                    ptr::with_exposed_provenance_mut(handle),
                    bundle_id.as_ptr(),
                )
            };
            if flags & SUGGESTION_AX_UNAVAILABLE != 0 {
                BrowserTitleRead::Unreadable
            } else {
                BrowserTitleRead::Read
            }
        }
    }

    unsafe extern "C" fn meeting_suggestion_callback(
        context: *mut c_void,
        app_bundle_id: *const c_char,
        title: *const c_char,
        url_host: *const c_char,
        evidence_flags: u32,
        observed_at_ns: u64,
    ) {
        // SAFETY: Swift invokes this while MacosMeetingSuggestionObserver retains
        // callback_state. stop drains Swift's evidence queue before it is dropped.
        let Some(state) = (unsafe { callback_state(context) }) else {
            return;
        };
        // SAFETY: Swift owns these C strings for this synchronous callback only.
        let Some(app_bundle_id) = (unsafe { callback_text(app_bundle_id) }) else {
            return;
        };
        // SAFETY: null represents absent bounded AX evidence.
        let title = unsafe { callback_text(title) };
        // SAFETY: null represents absent sanitized AX host evidence.
        let url_host = unsafe { callback_text(url_host) };

        let Some(signal) = normalize_meeting_signal(
            app_bundle_id,
            title,
            url_host,
            evidence_flags & SUGGESTION_AX_UNAVAILABLE != 0,
            observed_at_ns,
            &state.configured_application_ids,
        ) else {
            return;
        };
        state.sink.submit(signal);
    }

    unsafe fn callback_state<'a>(context: *mut c_void) -> Option<&'a SuggestionCallbackState> {
        if context.is_null() {
            return None;
        }
        // SAFETY: caller documents the retained Arc lifetime for this raw pointer.
        Some(unsafe { &*context.cast::<SuggestionCallbackState>() })
    }

    unsafe fn callback_text<'a>(text: *const c_char) -> Option<&'a str> {
        if text.is_null() {
            return None;
        }
        // SAFETY: caller documents that Swift owns a valid C string for this callback.
        unsafe { CStr::from_ptr(text) }.to_str().ok()
    }

    fn normalize_meeting_signal(
        app_bundle_id: &str,
        title: Option<&str>,
        url_host: Option<&str>,
        ax_unavailable: bool,
        observed_at_ns: u64,
        configured_application_ids: &BTreeSet<String>,
    ) -> Option<MeetingSuggestionSignal> {
        let app_bundle_id = app_bundle_id.trim().to_ascii_lowercase();
        if app_bundle_id.is_empty() {
            return None;
        }

        let provider =
            meeting_provider(&app_bundle_id, title, url_host, configured_application_ids)?;
        let mut evidence_flags = MeetingEvidenceFlags::app_only();
        if title.is_some() {
            evidence_flags = evidence_flags.with_ax_title();
        }
        if url_host.is_some() {
            evidence_flags = evidence_flags.with_ax_host();
        }
        if ax_unavailable {
            evidence_flags = evidence_flags.with_ax_unavailable();
        }

        Some(MeetingSuggestionSignal {
            provider,
            app_bundle_id,
            observed_at_ns,
            evidence_flags,
        })
    }

    fn meeting_provider(
        app_bundle_id: &str,
        title: Option<&str>,
        url_host: Option<&str>,
        configured_application_ids: &BTreeSet<String>,
    ) -> Option<MeetingProvider> {
        match app_bundle_id {
            "us.zoom.xos" => return Some(MeetingProvider::Zoom),
            "com.microsoft.teams" | "com.microsoft.teams2" => {
                return Some(MeetingProvider::MicrosoftTeams);
            }
            "com.cisco.webex" | "com.cisco.webexmeetingsapp" => {
                return Some(MeetingProvider::Webex);
            }
            "com.apple.facetime" => return Some(MeetingProvider::FaceTime),
            "com.tinyspeck.slackmacgap" => return Some(MeetingProvider::SlackHuddle),
            _ => {}
        }

        let title = title.map(str::to_ascii_lowercase);
        let url_host = url_host.map(str::to_ascii_lowercase);
        if is_browser_bundle_id(app_bundle_id) && matches_browser_meeting(&title, &url_host) {
            return provider_from_browser_evidence(title.as_deref(), url_host.as_deref());
        }
        configured_application_ids
            .contains(app_bundle_id)
            .then_some(MeetingProvider::ConfiguredApp)
    }

    /// Mirrors `detection::apps::BROWSER_BUNDLE_IDS` and the Swift observer's
    /// `supportedMeetingBundleIDs`: a browser missing from any of the three
    /// never produces a match.
    fn is_browser_bundle_id(app_bundle_id: &str) -> bool {
        matches!(
            app_bundle_id,
            "com.apple.safari"
                | "com.google.chrome"
                | "com.google.chrome.canary"
                | "com.microsoft.edgemac"
                | "org.mozilla.firefox"
                | "company.thebrowser.browser"
        )
    }

    fn matches_browser_meeting(title: &Option<String>, url_host: &Option<String>) -> bool {
        url_host.as_deref().is_some_and(is_meeting_host)
            || title.as_deref().is_some_and(|value| {
                value.contains("google meet")
                    || value.contains("microsoft teams")
                    || value.contains("webex")
                    || value.contains("zoom")
                    || value.contains("slack huddle")
            })
    }

    fn provider_from_browser_evidence(
        title: Option<&str>,
        url_host: Option<&str>,
    ) -> Option<MeetingProvider> {
        if url_host.is_some_and(|host| host == "meet.google.com")
            || title.is_some_and(|value| value.contains("google meet"))
        {
            return Some(MeetingProvider::GoogleMeet);
        }
        if url_host.is_some_and(|host| {
            host == "teams.microsoft.com" || host.ends_with(".teams.microsoft.com")
        }) || title.is_some_and(|value| value.contains("microsoft teams"))
        {
            return Some(MeetingProvider::MicrosoftTeams);
        }
        if url_host.is_some_and(|host| host == "webex.com" || host.ends_with(".webex.com"))
            || title.is_some_and(|value| value.contains("webex"))
        {
            return Some(MeetingProvider::Webex);
        }
        if url_host.is_some_and(|host| host == "zoom.us" || host.ends_with(".zoom.us"))
            || title.is_some_and(|value| value.contains("zoom"))
        {
            return Some(MeetingProvider::Zoom);
        }
        if title.is_some_and(|value| value.contains("slack huddle")) {
            return Some(MeetingProvider::SlackHuddle);
        }
        None
    }

    fn is_meeting_host(host: &str) -> bool {
        host == "meet.google.com"
            || host == "teams.microsoft.com"
            || host.ends_with(".teams.microsoft.com")
            || host == "webex.com"
            || host.ends_with(".webex.com")
            || host == "zoom.us"
            || host.ends_with(".zoom.us")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn google_meet_host_normalizes_to_content_free_signal() {
            let signal = normalize_meeting_signal(
                "com.google.Chrome",
                Some("Quarterly planning"),
                Some("meet.google.com"),
                false,
                42,
                &BTreeSet::new(),
            )
            .expect("Google Meet host should produce a suggestion");

            assert_eq!(signal.provider, MeetingProvider::GoogleMeet);
            assert_eq!(signal.app_bundle_id, "com.google.chrome");
            assert_eq!(signal.observed_at_ns, 42);
            assert!(signal.evidence_flags.app_only);
            assert!(signal.evidence_flags.ax_title);
            assert!(signal.evidence_flags.ax_host);
            assert!(!signal.evidence_flags.ax_unavailable);
        }

        #[test]
        fn direct_provider_signal_marks_ax_unavailable_without_content() {
            let signal =
                normalize_meeting_signal("us.zoom.xos", None, None, true, 7, &BTreeSet::new())
                    .expect("Zoom activation should produce a suggestion");

            assert_eq!(signal.provider, MeetingProvider::Zoom);
            assert!(signal.evidence_flags.app_only);
            assert!(signal.evidence_flags.ax_unavailable);
            assert!(!signal.evidence_flags.ax_title);
            assert!(!signal.evidence_flags.ax_host);
        }

        #[test]
        fn configured_application_uses_configured_provider() {
            let configured_application_ids = BTreeSet::from(["com.example.call".to_string()]);
            let signal = normalize_meeting_signal(
                "com.example.call",
                None,
                None,
                false,
                11,
                &configured_application_ids,
            )
            .expect("configured app should produce a suggestion");

            assert_eq!(signal.provider, MeetingProvider::ConfiguredApp);
            assert_eq!(signal.app_bundle_id, "com.example.call");
        }

        #[test]
        fn unrelated_browser_activation_has_no_suggestion() {
            assert!(normalize_meeting_signal(
                "com.google.chrome",
                Some("Documentation"),
                Some("example.com"),
                false,
                1,
                &BTreeSet::new(),
            )
            .is_none());
        }

        /* FN5 in the detection map: Arc was in `apps::BROWSER_BUNDLE_IDS` but
         * in neither of the two lists a browser match actually reads. */
        #[test]
        fn arc_normalizes_like_chrome() {
            let signal = normalize_meeting_signal(
                "company.thebrowser.Browser",
                Some("Meet – Weekly sync"),
                Some("meet.google.com"),
                false,
                3,
                &BTreeSet::new(),
            )
            .expect("a Meet tab in Arc should produce a suggestion");

            assert_eq!(signal.provider, MeetingProvider::GoogleMeet);
            assert_eq!(signal.app_bundle_id, "company.thebrowser.browser");
            assert!(signal.evidence_flags.ax_host);
        }
    }
}

mod system_audio {
    use crate::meeting::capture::{MeetingCaptureSource, PacketSink};
    use crate::meeting::types::{
        AudioFormat, CapturedPacket, MeetingCaptureError, PacketDiscontinuityFlags,
        SessionClockAnchor, SourceAvailability, SourceClockEpoch, SourceEpoch, SourceGap,
        SourceGapReason, SourceHealth, SourceKind, SourceProbe, SourceProbeDetail, SourceStartPlan,
        SourceStartReport, SourceStopReport, SourceTrackId, TimestampBridge,
    };
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::ptr;
    use std::slice;

    const BRIDGE_OK: c_int = 0;
    const BRIDGE_INVALID_ARGUMENT: c_int = 1;
    const BRIDGE_UNSUPPORTED: c_int = 2;
    const BRIDGE_PERMISSION_DENIED: c_int = 3;
    const BRIDGE_SOURCE_UNAVAILABLE: c_int = 4;
    const BRIDGE_ROUTE_UNAVAILABLE: c_int = 5;
    const BRIDGE_STREAM_FAILURE: c_int = 6;

    const STATUS_PERMISSION: c_int = 1;
    const STATUS_SOURCE: c_int = 2;
    const STATUS_ROUTE: c_int = 3;
    const STATUS_STREAM: c_int = 4;

    const FAILURE_AUDIO_FORMAT_CHANGED: c_int = 7;
    const FAILURE_AUDIO_BUFFER_NOT_CONTIGUOUS: c_int = 8;
    const FAILURE_INVALID_TIMESTAMP: c_int = 9;
    const FAILURE_TIMESTAMP_DISCONTINUITY: c_int = 13;

    const PACKET_TIMESTAMP_RESET: u32 = 1;
    const PACKET_SOURCE_RESTARTED: u32 = 1 << 2;
    const PACKET_FORMAT_CHANGED: u32 = 1 << 3;
    const MAXIMUM_FILTER_APPLICATIONS: usize = 64;
    const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

    type PacketCallback = unsafe extern "C" fn(
        context: *mut c_void,
        samples: *const f32,
        frame_count: usize,
        sample_rate_hz: u32,
        channels: u32,
        native_timestamp_value: i64,
        native_timestamp_timescale: i32,
        host_monotonic_anchor_ns: u64,
        source_epoch: u64,
        format_epoch: u64,
        discontinuity_flags: u32,
    );
    type StatusCallback = unsafe extern "C" fn(
        context: *mut c_void,
        category: c_int,
        code: c_int,
        source_epoch: u64,
        format_epoch: u64,
        native_timestamp_value: i64,
        native_timestamp_timescale: i32,
        host_monotonic_anchor_ns: u64,
        frames: u32,
    );

    extern "C" {
        fn sona_meeting_capture_probe() -> c_int;
        fn sona_meeting_capture_start(
            application_bundle_ids: *const c_void,
            application_bundle_id_count: usize,
            epoch: u64,
            session_host_anchor_ns: u64,
            packet_callback: Option<PacketCallback>,
            status_callback: Option<StatusCallback>,
            callback_context: *mut c_void,
            out_native_anchor_value: *mut i64,
            out_native_anchor_timescale: *mut i32,
            out_host_monotonic_anchor_ns: *mut u64,
            out_session_offset_ns: *mut u64,
            out_format_epoch: *mut u64,
            out_handle: *mut *mut c_void,
        ) -> c_int;
        fn sona_meeting_capture_pause(handle: *mut c_void) -> c_int;
        fn sona_meeting_capture_resume(
            handle: *mut c_void,
            epoch: u64,
            session_host_anchor_ns: u64,
            out_native_anchor_value: *mut i64,
            out_native_anchor_timescale: *mut i32,
            out_host_monotonic_anchor_ns: *mut u64,
            out_session_offset_ns: *mut u64,
            out_format_epoch: *mut u64,
        ) -> c_int;
        fn sona_meeting_capture_stop(handle: *mut c_void) -> c_int;
        fn sona_meeting_capture_abort(handle: *mut c_void) -> c_int;
        fn sona_meeting_capture_destroy(handle: *mut c_void);
    }

    /// Owns the one frozen-filter ScreenCaptureKit system-audio stream.
    pub struct MacosSystemAudioCapture {
        handle: Option<usize>,
        callback_state: Option<Box<CaptureCallbackState>>,
        anchor: Option<SessionClockAnchor>,
        track_id: Option<SourceTrackId>,
        format: Option<AudioFormat>,
    }

    // Swift serializes packet and status callbacks on its output queue. Rust
    // reads this state only after stop/destroy drains that queue.
    struct CaptureCallbackState {
        sink: PacketSink,
        track_id: SourceTrackId,
        session_host_anchor_ns: u64,
        has_failure: bool,
        has_offset: bool,
        sequence: u64,
        last_reported_source_epoch: u64,
        last_reported_format_epoch: u64,
        last_offset_ns: u64,
    }

    struct CBundleIds {
        _owned: Vec<CString>,
        pointers: Vec<*const c_char>,
    }

    impl CBundleIds {
        fn from_bundle_ids(bundle_ids: &[String]) -> Result<Self, MeetingCaptureError> {
            if bundle_ids.len() > MAXIMUM_FILTER_APPLICATIONS {
                return Err(MeetingCaptureError::InvalidState);
            }

            let mut owned = Vec::with_capacity(bundle_ids.len());
            for bundle_id in bundle_ids {
                if bundle_id.is_empty() {
                    return Err(MeetingCaptureError::InvalidState);
                }
                owned.push(
                    CString::new(bundle_id.as_str())
                        .map_err(|_| MeetingCaptureError::InvalidState)?,
                );
            }
            let pointers = owned.iter().map(|bundle_id| bundle_id.as_ptr()).collect();
            Ok(Self {
                _owned: owned,
                pointers,
            })
        }

        fn as_argument(&self) -> *const c_void {
            if self.pointers.is_empty() {
                ptr::null()
            } else {
                self.pointers.as_ptr().cast()
            }
        }
    }

    impl CaptureCallbackState {
        fn new(sink: PacketSink, track_id: SourceTrackId, session_host_anchor_ns: u64) -> Self {
            Self {
                sink,
                track_id,
                session_host_anchor_ns,
                has_failure: false,
                has_offset: false,
                sequence: 0,
                last_reported_source_epoch: u64::MAX,
                last_reported_format_epoch: u64::MAX,
                last_offset_ns: 0,
            }
        }

        fn bridge_for_observation(
            &self,
            native_timestamp_value: i64,
            native_timestamp_timescale: i32,
            host_monotonic_anchor_ns: u64,
        ) -> Option<TimestampBridge> {
            let native_timescale = u32::try_from(native_timestamp_timescale).ok()?;
            if native_timescale == 0 {
                return None;
            }
            let session_offset_ns =
                host_monotonic_anchor_ns.checked_sub(self.session_host_anchor_ns)?;
            Some(TimestampBridge {
                native_anchor_value: native_timestamp_value,
                native_timescale,
                host_monotonic_anchor_ns,
                session_offset_ns,
            })
        }

        fn report_clock_epoch(
            &mut self,
            source_epoch: SourceEpoch,
            format_epoch: u64,
            bridge: TimestampBridge,
        ) -> bool {
            if self.last_reported_source_epoch == source_epoch.get()
                && self.last_reported_format_epoch == format_epoch
            {
                return true;
            }
            if !self.sink.report_clock_epoch(SourceClockEpoch {
                track_id: self.track_id,
                epoch: source_epoch,
                format_epoch,
                bridge,
            }) {
                self.has_failure = true;
                return false;
            }
            self.last_reported_source_epoch = source_epoch.get();
            self.last_reported_format_epoch = format_epoch;
            true
        }

        fn record_packet_end(
            &mut self,
            bridge: TimestampBridge,
            frame_count: u32,
            sample_rate_hz: u32,
        ) {
            let Some(duration_ns) = u64::from(frame_count)
                .checked_mul(NANOSECONDS_PER_SECOND)
                .and_then(|frames| frames.checked_div(u64::from(sample_rate_hz)))
            else {
                return;
            };
            let Some(end_offset_ns) = bridge.session_offset_ns.checked_add(duration_ns) else {
                return;
            };
            self.last_offset_ns = self.last_offset_ns.max(end_offset_ns);
            self.has_offset = true;
        }

        fn report_gap(
            &mut self,
            epoch: SourceEpoch,
            reason: SourceGapReason,
            bridge: Option<TimestampBridge>,
            dropped_frames: Option<u64>,
        ) {
            self.has_failure = true;
            self.sink.report_gap(SourceGap {
                track_id: self.track_id,
                epoch,
                start_offset_ns: bridge.map(|bridge| bridge.session_offset_ns),
                end_offset_ns: None,
                reason,
                dropped_frames,
            });
        }
    }

    impl Default for MacosSystemAudioCapture {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MacosSystemAudioCapture {
        pub const fn new() -> Self {
            Self {
                handle: None,
                callback_state: None,
                anchor: None,
                track_id: None,
                format: None,
            }
        }

        fn handle_pointer(&self) -> Result<*mut c_void, MeetingCaptureError> {
            self.handle
                .map(ptr::with_exposed_provenance_mut)
                .ok_or(MeetingCaptureError::InvalidState)
        }

        fn source_report(
            &self,
            epoch: SourceEpoch,
            format_epoch: u64,
            timestamp_bridge: TimestampBridge,
        ) -> Result<SourceStartReport, MeetingCaptureError> {
            Ok(SourceStartReport {
                track_id: self.track_id.ok_or(MeetingCaptureError::InvalidState)?,
                source_kind: SourceKind::SystemAudio,
                format: self.format.ok_or(MeetingCaptureError::InvalidState)?,
                epoch,
                format_epoch,
                timestamp_bridge,
            })
        }

        fn end_stream(&mut self, abort: bool) -> Result<SourceStopReport, MeetingCaptureError> {
            let handle = self.handle_pointer()?;
            self.callback_state
                .as_ref()
                .ok_or(MeetingCaptureError::InvalidState)?;

            // SAFETY: the callback Box remains allocated until Swift disables callbacks and drains its output queue during destroy.
            let result = unsafe {
                if abort {
                    sona_meeting_capture_abort(handle)
                } else {
                    sona_meeting_capture_stop(handle)
                }
            };
            // SAFETY: destroy disables future packet/status callbacks and drains the output queue before the Box is taken below.
            unsafe { sona_meeting_capture_destroy(handle) };

            let state = self
                .callback_state
                .take()
                .ok_or(MeetingCaptureError::InvalidState)?;
            self.handle = None;
            self.anchor = None;
            self.track_id = None;
            self.format = None;

            let health = if result == BRIDGE_OK && !state.has_failure {
                SourceHealth::Healthy
            } else if result == BRIDGE_OK {
                SourceHealth::Degraded
            } else {
                SourceHealth::Failed
            };
            Ok(SourceStopReport {
                track_id: state.track_id,
                final_offset_ns: state.has_offset.then_some(state.last_offset_ns),
                health,
                observed_gaps: Vec::new(),
            })
        }
    }

    impl MeetingCaptureSource for MacosSystemAudioCapture {
        fn probe(&self) -> SourceProbe {
            // SAFETY: the Swift probe reads only platform availability and permission state.
            let result = unsafe { sona_meeting_capture_probe() };
            match result {
                BRIDGE_OK => SourceProbe {
                    source_kind: SourceKind::SystemAudio,
                    availability: SourceAvailability::Available,
                    health: SourceHealth::NotStarted,
                    detail: None,
                    negotiated_format: Some(AudioFormat {
                        sample_rate_hz: 48_000,
                        channels: 2,
                    }),
                },
                BRIDGE_PERMISSION_DENIED => SourceProbe {
                    source_kind: SourceKind::SystemAudio,
                    availability: SourceAvailability::PermissionDenied,
                    health: SourceHealth::NotStarted,
                    detail: Some(SourceProbeDetail::Permission),
                    negotiated_format: None,
                },
                BRIDGE_UNSUPPORTED => SourceProbe {
                    source_kind: SourceKind::SystemAudio,
                    availability: SourceAvailability::UnsupportedPlatform,
                    health: SourceHealth::NotStarted,
                    detail: Some(SourceProbeDetail::Platform),
                    negotiated_format: None,
                },
                _ => SourceProbe {
                    source_kind: SourceKind::SystemAudio,
                    availability: SourceAvailability::DeviceUnavailable,
                    health: SourceHealth::Failed,
                    detail: Some(SourceProbeDetail::Stream),
                    negotiated_format: None,
                },
            }
        }

        fn start(
            &mut self,
            plan: SourceStartPlan,
            anchor: SessionClockAnchor,
            sink: PacketSink,
        ) -> Result<SourceStartReport, MeetingCaptureError> {
            if self.handle.is_some() || plan.source_kind != SourceKind::SystemAudio {
                return Err(MeetingCaptureError::InvalidState);
            }

            let bundle_ids = CBundleIds::from_bundle_ids(&plan.frozen_application_bundle_ids)?;
            let mut callback_state = Box::new(CaptureCallbackState::new(
                sink,
                plan.track_id,
                anchor.host_monotonic_anchor_ns,
            ));
            let callback_context = ptr::from_mut(callback_state.as_mut()).cast::<c_void>();
            let mut native_anchor_value = 0;
            let mut native_anchor_timescale = 0;
            let mut host_monotonic_anchor_ns = 0;
            let mut session_offset_ns = 0;
            let mut format_epoch = 0;
            let mut raw_handle = ptr::null_mut();
            // C strings stay valid through this synchronous call.
            // SAFETY: callback_state remains retained until the Swift queue is stopped and drained.
            let result = unsafe {
                sona_meeting_capture_start(
                    bundle_ids.as_argument(),
                    bundle_ids.pointers.len(),
                    plan.source_epoch.get(),
                    anchor.host_monotonic_anchor_ns,
                    Some(meeting_packet_callback),
                    Some(meeting_status_callback),
                    callback_context,
                    &mut native_anchor_value,
                    &mut native_anchor_timescale,
                    &mut host_monotonic_anchor_ns,
                    &mut session_offset_ns,
                    &mut format_epoch,
                    &mut raw_handle,
                )
            };
            if result != BRIDGE_OK || raw_handle.is_null() || format_epoch == 0 {
                if !raw_handle.is_null() {
                    // This start call returned a retained Swift handle.
                    // SAFETY: stop and destroy it before callback_state leaves this stack frame.
                    unsafe {
                        let _ = sona_meeting_capture_abort(raw_handle);
                        sona_meeting_capture_destroy(raw_handle);
                    }
                }
                return Err(if result == BRIDGE_OK {
                    MeetingCaptureError::StreamFailure
                } else {
                    meeting_capture_error(result)
                });
            }

            let timestamp_bridge = match timestamp_bridge(
                native_anchor_value,
                native_anchor_timescale,
                host_monotonic_anchor_ns,
                session_offset_ns,
            ) {
                Ok(timestamp_bridge) => timestamp_bridge,
                Err(error) => {
                    // The successful start returned a retained Swift handle.
                    // SAFETY: a source without a valid clock bridge must stop before returning.
                    unsafe {
                        let _ = sona_meeting_capture_abort(raw_handle);
                        sona_meeting_capture_destroy(raw_handle);
                    }
                    return Err(error);
                }
            };
            self.handle = Some(raw_handle.addr());
            self.callback_state = Some(callback_state);
            self.anchor = Some(anchor);
            self.track_id = Some(plan.track_id);
            self.format = Some(AudioFormat {
                sample_rate_hz: 48_000,
                channels: 2,
            });
            self.source_report(plan.source_epoch, format_epoch, timestamp_bridge)
        }

        fn pause(&mut self) -> Result<(), MeetingCaptureError> {
            let handle = self.handle_pointer()?;
            self.callback_state
                .as_ref()
                .ok_or(MeetingCaptureError::InvalidState)?;
            // SAFETY: this source owns the retained Swift stream handle.
            let result = unsafe { sona_meeting_capture_pause(handle) };
            if result == BRIDGE_OK {
                Ok(())
            } else {
                // A failed pause must stop its live stream before returning.
                // SAFETY: the Swift abort path stops the stream and drains its callback queue before this method returns.
                unsafe {
                    let _ = sona_meeting_capture_abort(handle);
                }
                Err(meeting_capture_error(result))
            }
        }

        fn resume(&mut self, epoch: SourceEpoch) -> Result<SourceStartReport, MeetingCaptureError> {
            let handle = self.handle_pointer()?;
            let anchor = self.anchor.ok_or(MeetingCaptureError::InvalidState)?;
            self.callback_state
                .as_ref()
                .ok_or(MeetingCaptureError::InvalidState)?;
            let mut native_anchor_value = 0;
            let mut native_anchor_timescale = 0;
            let mut host_monotonic_anchor_ns = 0;
            let mut session_offset_ns = 0;
            let mut format_epoch = 0;
            // SAFETY: this source owns the retained Swift stream handle, and its callback Box remains live through the call.
            let result = unsafe {
                sona_meeting_capture_resume(
                    handle,
                    epoch.get(),
                    anchor.host_monotonic_anchor_ns,
                    &mut native_anchor_value,
                    &mut native_anchor_timescale,
                    &mut host_monotonic_anchor_ns,
                    &mut session_offset_ns,
                    &mut format_epoch,
                )
            };
            if result != BRIDGE_OK || format_epoch == 0 {
                if result == BRIDGE_OK {
                    // A zero format epoch cannot describe a safe resumed source.
                    // SAFETY: abort the newly resumed stream before returning the failure.
                    unsafe {
                        let _ = sona_meeting_capture_abort(handle);
                    }
                }
                return Err(if result == BRIDGE_OK {
                    MeetingCaptureError::StreamFailure
                } else {
                    meeting_capture_error(result)
                });
            }

            let timestamp_bridge = match timestamp_bridge(
                native_anchor_value,
                native_anchor_timescale,
                host_monotonic_anchor_ns,
                session_offset_ns,
            ) {
                Ok(timestamp_bridge) => timestamp_bridge,
                Err(error) => {
                    // Resume opened the stream but did not yield a valid explicit bridge.
                    // SAFETY: abort it instead of accepting untimeable audio.
                    unsafe {
                        let _ = sona_meeting_capture_abort(handle);
                    }
                    return Err(error);
                }
            };
            self.source_report(epoch, format_epoch, timestamp_bridge)
        }

        fn stop(&mut self) -> Result<SourceStopReport, MeetingCaptureError> {
            self.end_stream(false)
        }

        fn abort(&mut self) -> Result<(), MeetingCaptureError> {
            if self.handle.is_none() {
                return Ok(());
            }
            self.end_stream(true).map(|_| ())
        }
    }

    impl Drop for MacosSystemAudioCapture {
        fn drop(&mut self) {
            if self.handle.is_some() {
                let _ = self.end_stream(true);
            }
        }
    }

    unsafe extern "C" fn meeting_packet_callback(
        context: *mut c_void,
        samples: *const f32,
        frame_count: usize,
        sample_rate_hz: u32,
        channels: u32,
        native_timestamp_value: i64,
        native_timestamp_timescale: i32,
        host_monotonic_anchor_ns: u64,
        source_epoch: u64,
        format_epoch: u64,
        discontinuity_flags: u32,
    ) {
        // SAFETY: Swift invokes packet and status callbacks serially on its output queue while this Box remains retained.
        let Some(state) = (unsafe { callback_state(context) }) else {
            return;
        };
        let Ok(frame_count) = u32::try_from(frame_count) else {
            state.report_gap(
                SourceEpoch::new(source_epoch),
                SourceGapReason::InvalidFormat,
                None,
                None,
            );
            return;
        };
        let Ok(channels) = u16::try_from(channels) else {
            state.report_gap(
                SourceEpoch::new(source_epoch),
                SourceGapReason::InvalidFormat,
                None,
                Some(u64::from(frame_count)),
            );
            return;
        };
        let Some(sample_count) = usize::try_from(frame_count)
            .ok()
            .and_then(|frames| frames.checked_mul(usize::from(channels)))
        else {
            state.report_gap(
                SourceEpoch::new(source_epoch),
                SourceGapReason::InvalidFormat,
                None,
                Some(u64::from(frame_count)),
            );
            return;
        };
        if samples.is_null() || sample_rate_hz == 0 || channels == 0 {
            state.report_gap(
                SourceEpoch::new(source_epoch),
                SourceGapReason::InvalidFormat,
                None,
                Some(u64::from(frame_count)),
            );
            return;
        }

        let source_epoch = SourceEpoch::new(source_epoch);
        let Some(bridge) = state.bridge_for_observation(
            native_timestamp_value,
            native_timestamp_timescale,
            host_monotonic_anchor_ns,
        ) else {
            state.report_gap(
                source_epoch,
                SourceGapReason::TimestampMissing,
                None,
                Some(u64::from(frame_count)),
            );
            return;
        };
        if !state.report_clock_epoch(source_epoch, format_epoch, bridge) {
            state.report_gap(
                source_epoch,
                SourceGapReason::TimestampDiscontinuity,
                Some(bridge),
                Some(u64::from(frame_count)),
            );
            return;
        }

        let packet_discontinuity_flags = PacketDiscontinuityFlags {
            timestamp_reset: discontinuity_flags & PACKET_TIMESTAMP_RESET != 0,
            route_changed: false,
            source_restarted: discontinuity_flags
                & (PACKET_SOURCE_RESTARTED | PACKET_FORMAT_CHANGED)
                != 0,
        };
        if packet_discontinuity_flags.timestamp_reset {
            state.report_gap(
                source_epoch,
                SourceGapReason::TimestampDiscontinuity,
                Some(bridge),
                Some(u64::from(frame_count)),
            );
        }
        if discontinuity_flags & PACKET_FORMAT_CHANGED != 0 {
            state.report_gap(
                source_epoch,
                SourceGapReason::InvalidFormat,
                Some(bridge),
                Some(u64::from(frame_count)),
            );
        }

        let sequence = state.sequence;
        state.sequence = state.sequence.wrapping_add(1);
        let packet = CapturedPacket {
            track_id: state.track_id,
            source_epoch,
            format_epoch,
            sequence,
            native_timestamp_value: Some(native_timestamp_value),
            native_timestamp_timescale: Some(bridge.native_timescale),
            host_monotonic_anchor_ns: Some(host_monotonic_anchor_ns),
            sample_rate_hz,
            channels,
            frame_count,
            discontinuity_flags: packet_discontinuity_flags,
        };
        // Swift supplies sample_count contiguous f32 values and retains the CMSampleBuffer.
        // SAFETY: PacketSink copies the source pointer before this synchronous callback returns.
        let samples = unsafe { slice::from_raw_parts(samples, sample_count) };
        let _ = state.sink.try_push_interleaved(packet, samples);
        state.record_packet_end(bridge, frame_count, sample_rate_hz);
    }

    unsafe extern "C" fn meeting_status_callback(
        context: *mut c_void,
        category: c_int,
        code: c_int,
        source_epoch: u64,
        format_epoch: u64,
        native_timestamp_value: i64,
        native_timestamp_timescale: i32,
        host_monotonic_anchor_ns: u64,
        frames: u32,
    ) {
        // SAFETY: Swift serializes packet and status callbacks on its output queue while this Box remains retained.
        let Some(state) = (unsafe { callback_state(context) }) else {
            return;
        };
        let source_epoch = SourceEpoch::new(source_epoch);
        let bridge = state.bridge_for_observation(
            native_timestamp_value,
            native_timestamp_timescale,
            host_monotonic_anchor_ns,
        );
        if let Some(bridge) = bridge {
            if !state.report_clock_epoch(source_epoch, format_epoch, bridge) {
                state.report_gap(
                    source_epoch,
                    SourceGapReason::TimestampDiscontinuity,
                    Some(bridge),
                    (frames > 0).then_some(u64::from(frames)),
                );
            }
        }
        state.report_gap(
            source_epoch,
            source_gap_reason(category, code),
            bridge,
            (frames > 0).then_some(u64::from(frames)),
        );
    }

    unsafe fn callback_state<'a>(context: *mut c_void) -> Option<&'a mut CaptureCallbackState> {
        if context.is_null() {
            return None;
        }
        // SAFETY: context points to the live callback Box, and Swift invokes its packet/status callbacks serially.
        Some(unsafe { &mut *context.cast::<CaptureCallbackState>() })
    }

    fn timestamp_bridge(
        native_anchor_value: i64,
        native_anchor_timescale: i32,
        host_monotonic_anchor_ns: u64,
        session_offset_ns: u64,
    ) -> Result<TimestampBridge, MeetingCaptureError> {
        let native_timescale = u32::try_from(native_anchor_timescale)
            .ok()
            .filter(|timescale| *timescale > 0)
            .ok_or(MeetingCaptureError::StreamFailure)?;
        Ok(TimestampBridge {
            native_anchor_value,
            native_timescale,
            host_monotonic_anchor_ns,
            session_offset_ns,
        })
    }

    fn source_gap_reason(category: c_int, code: c_int) -> SourceGapReason {
        match code {
            FAILURE_INVALID_TIMESTAMP => SourceGapReason::TimestampMissing,
            FAILURE_TIMESTAMP_DISCONTINUITY => SourceGapReason::TimestampDiscontinuity,
            FAILURE_AUDIO_FORMAT_CHANGED | FAILURE_AUDIO_BUFFER_NOT_CONTIGUOUS => {
                SourceGapReason::InvalidFormat
            }
            _ => match category {
                STATUS_PERMISSION => SourceGapReason::PermissionLost,
                STATUS_SOURCE => SourceGapReason::SourceUnavailable,
                STATUS_ROUTE | STATUS_STREAM => SourceGapReason::SourceStopped,
                _ => SourceGapReason::SourceStartFailed,
            },
        }
    }

    fn meeting_capture_error(result: c_int) -> MeetingCaptureError {
        match result {
            BRIDGE_PERMISSION_DENIED => MeetingCaptureError::PermissionDenied,
            BRIDGE_SOURCE_UNAVAILABLE | BRIDGE_ROUTE_UNAVAILABLE | BRIDGE_UNSUPPORTED => {
                MeetingCaptureError::Unavailable
            }
            BRIDGE_INVALID_ARGUMENT => MeetingCaptureError::InvalidState,
            BRIDGE_STREAM_FAILURE => MeetingCaptureError::StreamFailure,
            _ => MeetingCaptureError::StreamFailure,
        }
    }

    /// These drive the Swift bridge's own format decision and sample
    /// conversion through `sona_meeting_capture_convert_for_test`, which is the
    /// pair of steps every ScreenCaptureKit buffer takes before it reaches
    /// `meeting_packet_callback`. A live SCStream is not available in a unit
    /// test; the format description and the plane layout are.
    #[cfg(test)]
    mod capture_format_tests {
        use super::*;

        extern "C" {
            fn sona_meeting_capture_convert_for_test(
                sample_rate: f64,
                channels_per_frame: u32,
                bytes_per_frame: u32,
                format_flags: u32,
                bits_per_channel: u32,
                samples: *const f32,
                sample_count: u32,
                frame_count: u32,
                out: *mut f32,
            ) -> c_int;
        }

        const FLAG_FLOAT: u32 = 1 << 0;
        const FLAG_PACKED: u32 = 1 << 3;
        const FLAG_NON_INTERLEAVED: u32 = 1 << 5;

        fn convert(
            channels: u32,
            bytes_per_frame: u32,
            format_flags: u32,
            bits_per_channel: u32,
            payload: &[f32],
            frame_count: u32,
        ) -> (c_int, Vec<f32>) {
            let mut out = vec![f32::NAN; frame_count as usize * channels as usize];
            // `payload` outlives the call and `sample_count` is its real length.
            // SAFETY: `out` holds the `frame_count * channels` samples the bridge fills on success.
            let status = unsafe {
                sona_meeting_capture_convert_for_test(
                    48_000.0,
                    channels,
                    bytes_per_frame,
                    format_flags,
                    bits_per_channel,
                    payload.as_ptr(),
                    u32::try_from(payload.len()).unwrap(),
                    frame_count,
                    out.as_mut_ptr(),
                )
            };
            (status, out)
        }

        /// ScreenCaptureKit delivers one Float32 plane per channel, so its
        /// description sets the non-interleaved flag and counts a single
        /// channel in `mBytesPerFrame`. Rejecting that shape is why no system
        /// audio was ever recorded on this machine.
        #[test]
        fn planar_screen_capture_audio_becomes_interleaved_frames() {
            let (status, samples) = convert(
                2,
                4,
                FLAG_FLOAT | FLAG_PACKED | FLAG_NON_INTERLEAVED,
                32,
                &[1.0, 2.0, 3.0, -1.0, -2.0, -3.0],
                3,
            );

            assert_eq!(status, 0);
            assert_eq!(samples, vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
        }

        #[test]
        fn interleaved_audio_reaches_the_packet_unchanged() {
            let (status, samples) = convert(
                2,
                8,
                FLAG_FLOAT | FLAG_PACKED,
                32,
                &[1.0, -1.0, 2.0, -2.0, 3.0, -3.0],
                3,
            );

            assert_eq!(status, 0);
            assert_eq!(samples, vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
        }

        /// The guard still has a job: this bridge reads 32-bit float and
        /// nothing else, and a format it cannot read must be refused rather
        /// than reinterpreted.
        #[test]
        fn integer_pcm_is_refused() {
            let (status, _) = convert(
                2,
                4,
                FLAG_PACKED | FLAG_NON_INTERLEAVED,
                16,
                &[1.0, 2.0, 3.0, -1.0, -2.0, -3.0],
                3,
            );

            assert_eq!(status, 1);
        }

        /// A buffer list whose byte count disagrees with the frame count its
        /// format promises is a mismatch, not a licence to reinterpret it.
        #[test]
        fn a_payload_that_contradicts_its_format_is_rejected() {
            let (status, _) = convert(
                2,
                8,
                FLAG_FLOAT | FLAG_PACKED,
                32,
                &[1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0],
                3,
            );

            assert_eq!(status, 2);
        }
    }

    #[cfg(test)]
    mod live_capture_probe {
        use super::*;
        use crate::meeting::capture::PacketSink;
        use crate::meeting::types::MeetingSessionId;
        use std::time::{Duration, Instant};

        #[test]
        #[ignore = "needs Screen Recording permission and live system audio"]
        fn live_system_audio_delivers_packets() {
            // SAFETY: the Swift probe reads only platform availability and permission state.
            let permission = unsafe { sona_meeting_capture_probe() };
            println!("probe={permission}");
            assert_eq!(permission, BRIDGE_OK, "screen recording permission");

            let track_id = SourceTrackId::new();
            let (sink, mut reader) = PacketSink::new(track_id, 48_000 * 2 * 4, 512);
            let mut capture = MacosSystemAudioCapture::new();
            let anchor = SessionClockAnchor {
                host_monotonic_anchor_ns: crate::meeting::clock::host_monotonic_now_ns(),
                wall_start_utc_ms: 0,
                clock_policy_version: 1,
            };
            let report = capture
                .start(
                    SourceStartPlan {
                        session_id: MeetingSessionId::new(),
                        track_id,
                        source_kind: SourceKind::SystemAudio,
                        required: true,
                        frozen_application_bundle_ids: Vec::new(),
                        source_epoch: SourceEpoch::new(0),
                    },
                    anchor,
                    sink,
                )
                .expect("start system audio");
            println!("format={:?} epoch={:?}", report.format, report.epoch);

            let mut samples = Vec::new();
            let mut packets = 0_u64;
            let mut frames = 0_u64;
            let mut gaps = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                while let Ok(Some(packet)) = reader.pop_into(&mut samples) {
                    packets += 1;
                    frames += u64::from(packet.frame_count);
                }
                while let Some(gap) = reader.pop_gap() {
                    gaps.push(gap.reason);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let stop = capture.stop().expect("stop system audio");
            println!(
                "packets={packets} frames={frames} gaps={} reasons={:?} health={:?}",
                gaps.len(),
                &gaps[..gaps.len().min(4)],
                stop.health
            );
            assert!(packets > 0, "no system audio packet ever reached Rust");
        }
    }
}
