//! The application dimension of the decision table.
//!
//! Deliberately a **pull query**, not a watcher. The brief sketches an
//! `NSWorkspace` notification observer with a KVO fallback, because
//! `didLaunchApplicationNotification` is not posted for background and
//! `LSUIElement` processes. But every decision-table row that reads the app
//! dimension is triggered by something else — a microphone transition or a
//! calendar instant — and case 4 (app open, microphone idle) is a suppression,
//! so no row is driven by an app edge alone. Asking `runningApplications` at
//! decision time is strictly stronger than either observer: it sees background
//! and agent processes that the launch notification drops, and it keeps no
//! duplicated "is Zoom running" state to fall out of sync.
//!
//! The activation-edge stream the brief describes already exists in
//! `meeting_macos::MacosMeetingSuggestionObserver`, which feeds
//! `MeetingSuggestionService`. This module consumes that service's live offers
//! for the browser evidence in case 7 rather than building a second window-title
//! reader, and pulls a fresh read through the same observer
//! (`BrowserTitleReader`) on the ticks where the answer matters.

use std::collections::HashSet;

use super::machine::{AppSignal, BrowserTitleEvidence, CallSignal};
use crate::meeting::suggestions::{MeetingProvider, MeetingSuggestion};

/// The brief's §5.2 table. Community-sourced, so it seeds a settings-editable
/// list rather than being the final word — Microsoft has already renamed Teams's
/// bundle ID once, and the operator can add whatever their organization uses.
pub const DEFAULT_MEETING_APP_BUNDLE_IDS: &[&str] = &[
    // Zoom.
    "us.zoom.xos",
    // Microsoft Teams, current "work or school" build.
    "com.microsoft.teams2",
    // Microsoft Teams, classic installs.
    "com.microsoft.teams",
    // Slack, which is also where huddles live: a huddle is a mode inside the
    // app, not a separate process.
    "com.tinyspeck.slackmacgap",
    // Cisco Webex. Other Webex components ship under separate IDs.
    "com.webex.meetingmanager",
    // FaceTime. A call app: see `CALL_APP_BUNDLE_IDS`.
    "com.apple.facetime",
    // Phone, which is where an iPhone call relayed to the Mac lands on
    // macOS 26. Also a call app.
    "com.apple.mobilephone",
];

/// The subset of the allowlist whose meetings are calls rather than scheduled
/// meetings, and which therefore read a second audio signal.
///
/// Three things separate these from Zoom and Teams, and all three follow from
/// what they are:
///
/// * **No calendar event ever names them.** Nobody schedules a FaceTime call
///   into a shared invitation, so the calendar path has nothing to contribute
///   and the call path runs ahead of it.
/// * **Their microphone is usually Bluetooth.** AirPods-class headsets are the
///   default answer for a call, and they under-report through
///   `kAudioDevicePropertyDeviceIsRunningSomewhere` — the known false negative
///   named in `input_device`'s module doc. A call app that only had the input
///   signal would be detected on the built-in microphone and nowhere else.
/// * **They play the other side out loud.** That gives a second, independent
///   signal on the default output device, which `machine::call_is_live` reads.
///
/// Both identifiers were read off `/System/Applications` on macOS 26; the
/// registry stores them lowercased, as `WorkspaceApps` reports them.
pub const CALL_APP_BUNDLE_IDS: &[&str] = &["com.apple.facetime", "com.apple.mobilephone"];

/// Browsers whose frontmost tab may be a meeting. Google Meet has no native
/// macOS app at all, so the browser path is the only way to see it.
const BROWSER_BUNDLE_IDS: &[&str] = &[
    "com.apple.safari",
    "com.google.chrome",
    "com.google.chrome.canary",
    "com.microsoft.edgemac",
    "org.mozilla.firefox",
    "company.thebrowser.browser",
];

/// One running application, reduced to what the decision table and prompt copy
/// need. No window titles, URLs, or process arguments cross this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunningApp {
    pub bundle_id: String,
    pub display_name: String,
    pub frontmost: bool,
}

/// The platform's answer to "what is running right now". Behind a trait so the
/// composition below is testable without a window server.
pub trait RunningAppsSource: Send + Sync {
    /// Every running application, with `frontmost` set on the one receiving key
    /// events. Bundle IDs are lowercased.
    fn running_apps(&self) -> Vec<RunningApp>;
}

/// Reports nothing running. Used on non-macOS targets, where detection has no
/// application dimension.
pub struct NoRunningApps;

impl RunningAppsSource for NoRunningApps {
    fn running_apps(&self) -> Vec<RunningApp> {
        Vec::new()
    }
}

pub fn default_meeting_app_bundle_ids() -> Vec<String> {
    DEFAULT_MEETING_APP_BUNDLE_IDS
        .iter()
        .map(|bundle_id| (*bundle_id).to_string())
        .collect()
}

/// Normalizes an operator-edited allowlist: lowercased, trimmed, deduplicated,
/// and with empties dropped. A settings-editable list is the one place a typo can
/// silently disable detection for an app, so it is cleaned on the way in.
pub fn normalize_allowlist(entries: &[String]) -> Vec<String> {
    let mut normalized = Vec::with_capacity(entries.len());
    for entry in entries {
        let bundle_id = entry.trim().to_ascii_lowercase();
        if bundle_id.is_empty() || normalized.contains(&bundle_id) {
            continue;
        }
        normalized.push(bundle_id);
    }
    normalized
}

/// The runtime validation the brief asks for: an allowlist entry only becomes a
/// signal when a process with that bundle ID is actually running. A stale or
/// renamed ID contributes nothing instead of poisoning the decision.
///
/// Presence is not participation. An app is `Known` only when the operator is
/// using it this input-device episode — frontmost now, or in `apps_used`, the
/// set the runtime fills from `frontmost_allowlisted` on every tick the
/// microphone is active. Zoom left in the menu bar while a voice memo records
/// is `Present`, and prompts for nothing.
///
/// Call apps are excluded here and reported by `call_signal` instead. They are
/// on the same allowlist but answer a different question, and letting a
/// backgrounded FaceTime win `max_by_key` over a running Zoom would trade a
/// meeting Sona can detect for a call that is not happening.
pub fn app_signal(
    running: &[RunningApp],
    allowlist: &[String],
    apps_used: &HashSet<String>,
) -> AppSignal {
    let mut candidates = running
        .iter()
        .filter(|app| !is_call_app_bundle_id(&app.bundle_id) && in_allowlist(allowlist, app));
    // Frontmost wins so the prompt names the app the operator is looking at.
    let known = candidates
        .clone()
        .filter(|app| app.frontmost || apps_used.contains(&app.bundle_id))
        .max_by_key(|app| app.frontmost);
    if let Some(app) = known {
        return AppSignal::Known {
            bundle_id: app.bundle_id.clone(),
            display_name: app.display_name.clone(),
            frontmost: app.frontmost,
        };
    }

    // A browser only counts when it is frontmost: a background browser window is
    // not evidence that the operator is in a call.
    let browser = running
        .iter()
        .find(|app| app.frontmost && is_browser_bundle_id(&app.bundle_id));
    if let Some(app) = browser {
        return AppSignal::Browser {
            bundle_id: app.bundle_id.clone(),
            display_name: app.display_name.clone(),
        };
    }

    if let Some(app) = candidates.next() {
        return AppSignal::Present {
            bundle_id: app.bundle_id.clone(),
            display_name: app.display_name.clone(),
        };
    }

    AppSignal::Absent
}

/// The allowlisted meeting application in front right now, if any. What the
/// runtime adds to `apps_used` while the microphone is active, so that
/// `app_signal` keeps naming an app the operator switched away from mid-call.
pub fn frontmost_allowlisted<'a>(
    running: &'a [RunningApp],
    allowlist: &[String],
) -> Option<&'a str> {
    running
        .iter()
        .find(|app| {
            app.frontmost && !is_call_app_bundle_id(&app.bundle_id) && in_allowlist(allowlist, app)
        })
        .map(|app| app.bundle_id.as_str())
}

pub fn is_browser_bundle_id(bundle_id: &str) -> bool {
    BROWSER_BUNDLE_IDS
        .iter()
        .any(|candidate| bundle_id.eq_ignore_ascii_case(candidate))
}

pub fn is_app_running(running: &[RunningApp], bundle_id: &str) -> bool {
    running
        .iter()
        .any(|app| app.bundle_id.eq_ignore_ascii_case(bundle_id))
}

fn in_allowlist(allowlist: &[String], app: &RunningApp) -> bool {
    allowlist
        .iter()
        .any(|bundle_id| bundle_id == &app.bundle_id)
}

/// The call dimension: the allowlisted call application to attribute a call to,
/// preferring the frontmost one so the card names the app the operator is
/// looking at. Whether it is *in* a call is `machine::call_is_live`'s decision,
/// not this layer's — this only reports what is running.
pub fn call_signal(running: &[RunningApp], allowlist: &[String]) -> CallSignal {
    let call = running
        .iter()
        .filter(|app| is_call_app_bundle_id(&app.bundle_id) && in_allowlist(allowlist, app))
        .max_by_key(|app| app.frontmost);
    match call {
        Some(app) => CallSignal::Running {
            bundle_id: app.bundle_id.clone(),
            display_name: app.display_name.clone(),
            frontmost: app.frontmost,
        },
        None => CallSignal::Absent,
    }
}

pub fn is_call_app_bundle_id(bundle_id: &str) -> bool {
    CALL_APP_BUNDLE_IDS
        .iter()
        .any(|candidate| bundle_id.eq_ignore_ascii_case(candidate))
}

/// Normalizes an operator-edited auto-record list, and drops every entry the
/// decision table can never read.
///
/// A standing grant is only ever consulted for a `CallSignal`, which
/// `call_signal` raises for `CALL_APP_BUNDLE_IDS` alone. A grant stored for
/// Zoom therefore round-trips, reads back as held, and records nothing — and
/// the picker knows it, so it draws no switch on that row: the entry is a
/// standing consent the operator can neither see nor withdraw where grants
/// are managed. Two of them are in the wild on this machine.
///
/// The predicate is `is_call_app_bundle_id` rather than a second list, so
/// widening the set of apps that may hold a grant widens the reader and the
/// writer in one edit instead of leaving the store ahead of the table.
pub fn normalize_auto_record(entries: &[String]) -> Vec<String> {
    let mut normalized = normalize_allowlist(entries);
    normalized.retain(|bundle_id| is_call_app_bundle_id(bundle_id));
    normalized
}

/// Whether the operator's auto-record list names `bundle_id`. The one owner of
/// the case rule: `write_settings` normalizes the list on the way in, and this
/// normalizes again on the way out so a hand-edited store cannot differ.
pub fn grants_auto_record(settings: &crate::settings::AppSettings, bundle_id: &str) -> bool {
    normalize_allowlist(&settings.detection_auto_record_apps)
        .iter()
        .any(|granted| granted.eq_ignore_ascii_case(bundle_id))
}

/// Takes `bundle_id` off the auto-record list, under the same case rule.
pub fn revoke_auto_record(settings: &mut crate::settings::AppSettings, bundle_id: &str) {
    settings
        .detection_auto_record_apps
        .retain(|granted| !granted.trim().eq_ignore_ascii_case(bundle_id));
}

/// What one pull of the frontmost browser's window produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserTitleRead {
    /// The focused window was read. Whatever it said that names a meeting is
    /// in the offer store now, with a fresh TTL.
    Read,
    /// No window can be read: Accessibility is not trusted for Sona, or this
    /// platform has no reader at all.
    Unreadable,
}

/// The on-demand half of the browser-title reader.
///
/// The activation observer fires only on app switches, and its offer expires
/// after two minutes. A call joined after the switch, or in a browser that has
/// been in front longer than that, is invisible to it. The tick asks this
/// instead, while a browser is in front and the microphone is live, and the
/// answer lands in the same offer store through the same normalization.
pub trait BrowserTitleReader: Send + Sync {
    /// Reads the frontmost application's focused window now, when that
    /// application is `bundle_id`.
    fn refresh_frontmost(&self, bundle_id: &str) -> BrowserTitleRead;
}

/// Used where no reader exists: non-macOS targets, or a macOS observer that
/// failed to start.
pub struct NoBrowserTitles;

impl BrowserTitleReader for NoBrowserTitles {
    fn refresh_frontmost(&self, _bundle_id: &str) -> BrowserTitleRead {
        BrowserTitleRead::Unreadable
    }
}

/// Browser-tab evidence for §5.3 case 7: what this tick's read said it could
/// see, then the live suggestion offers the read and the activation observer
/// both feed.
///
/// `MeetingSuggestion::evidence_flags` carries exactly what is needed:
/// `ax_title` / `ax_host` mean the observer read a window title or URL host and
/// matched it against a meeting host, and `ax_unavailable` means Accessibility is
/// not trusted, so no title was readable at all. Reusing this keeps one owner for
/// "what is in that browser tab" instead of adding a second reader.
pub fn browser_title_evidence(
    read: BrowserTitleRead,
    suggestions: &[MeetingSuggestion],
    browser_bundle_id: &str,
) -> BrowserTitleEvidence {
    if read == BrowserTitleRead::Unreadable {
        return BrowserTitleEvidence::Unreadable;
    }
    let offer = suggestions
        .iter()
        .find(|offer| offer.app_bundle_id.eq_ignore_ascii_case(browser_bundle_id));
    let Some(offer) = offer else {
        return BrowserTitleEvidence::NoMatch;
    };
    if offer.evidence_flags.ax_unavailable {
        return BrowserTitleEvidence::Unreadable;
    }
    let matched_a_meeting = offer.evidence_flags.ax_title || offer.evidence_flags.ax_host;
    let meeting_provider = matches!(
        offer.provider,
        MeetingProvider::GoogleMeet
            | MeetingProvider::Zoom
            | MeetingProvider::MicrosoftTeams
            | MeetingProvider::Webex
            | MeetingProvider::SlackHuddle
    );
    if matched_a_meeting && meeting_provider {
        return BrowserTitleEvidence::MeetingMatch;
    }
    BrowserTitleEvidence::NoMatch
}

/// What a running process said about itself the first time a read saw it.
#[cfg(target_os = "macos")]
struct AppIdentity {
    bundle_id: String,
    display_name: String,
}

/// Each process's identity, remembered by pid for as long as it runs.
///
/// Listing the running applications is cheap. Asking one of them for its
/// bundle ID or its name is a round trip to launchservicesd, and a tick used
/// to ask every one of them for both, every fifteen seconds. Neither answer
/// changes while a process runs, so each process is asked once; a pid missing
/// from a read is forgotten, so a reused pid is asked again.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct KnownProcesses(std::collections::HashMap<i32, Option<AppIdentity>>);

#[cfg(target_os = "macos")]
impl KnownProcesses {
    /// The running apps among `processes`, `(pid, process)` pairs, calling
    /// `identify` only for a pid the previous read did not see.
    fn running_apps<P>(
        &mut self,
        processes: impl IntoIterator<Item = (i32, P)>,
        frontmost_pid: Option<i32>,
        mut identify: impl FnMut(&P) -> Option<AppIdentity>,
    ) -> Vec<RunningApp> {
        let capacity = self.0.len();
        let mut previous = std::mem::replace(
            &mut self.0,
            std::collections::HashMap::with_capacity(capacity),
        );
        let mut running = Vec::with_capacity(capacity);
        for (pid, process) in processes {
            let identity = previous.remove(&pid).unwrap_or_else(|| identify(&process));
            if let Some(identity) = &identity {
                running.push(RunningApp {
                    bundle_id: identity.bundle_id.clone(),
                    display_name: identity.display_name.clone(),
                    frontmost: frontmost_pid == Some(pid),
                });
            }
            // Without a pid of its own, one process cannot be told from the next.
            if pid > 0 {
                self.0.insert(pid, identity);
            }
        }
        running
    }
}

/// `NSWorkspace`-backed implementation. `runningApplications` needs no
/// entitlement and no TCC grant; it is always-available data.
#[cfg(target_os = "macos")]
#[derive(Default)]
pub struct WorkspaceApps {
    known: std::sync::Mutex<KnownProcesses>,
}

#[cfg(target_os = "macos")]
impl RunningAppsSource for WorkspaceApps {
    fn running_apps(&self) -> Vec<RunningApp> {
        use objc2_app_kit::NSWorkspace;

        let mut known = self
            .known
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // An autorelease pool per call: without one, every NSString and
        // NSRunningApplication read here would be held for the polling thread's
        // whole life.
        objc2::rc::autoreleasepool(|_| {
            let workspace = NSWorkspace::sharedWorkspace();
            let frontmost_pid = workspace
                .frontmostApplication()
                .map(|application| application.processIdentifier());
            let applications = workspace.runningApplications();
            known.running_apps(
                applications
                    .iter()
                    .map(|application| (application.processIdentifier(), application)),
                frontmost_pid,
                |application| {
                    let bundle_id = application.bundleIdentifier()?.to_string().to_lowercase();
                    if bundle_id.is_empty() {
                        return None;
                    }
                    let display_name = application
                        .localizedName()
                        .map(|name| name.to_string())
                        .unwrap_or_else(|| bundle_id.clone());
                    Some(AppIdentity {
                        bundle_id,
                        display_name,
                    })
                },
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::suggestions::{
        MeetingEvidenceFlags, MeetingSuggestionService, MeetingSuggestionSignal,
        MeetingSuggestionSink as _,
    };
    use crate::meeting::types::MeetingSuggestionId;

    fn app(bundle_id: &str, display_name: &str, frontmost: bool) -> RunningApp {
        RunningApp {
            bundle_id: bundle_id.to_string(),
            display_name: display_name.to_string(),
            frontmost,
        }
    }

    /// The set the runtime keeps per input-device episode. Empty means the
    /// microphone just went active and nothing has been in front yet.
    fn unused() -> HashSet<String> {
        HashSet::new()
    }

    fn used(bundle_ids: &[&str]) -> HashSet<String> {
        bundle_ids
            .iter()
            .map(|bundle_id| (*bundle_id).to_string())
            .collect()
    }

    fn offer(
        bundle_id: &str,
        provider: MeetingProvider,
        evidence_flags: MeetingEvidenceFlags,
    ) -> MeetingSuggestion {
        MeetingSuggestion {
            offer_id: MeetingSuggestionId::new(),
            provider,
            app_bundle_id: bundle_id.to_string(),
            evidence_flags,
            observed_at_ns: 1,
            expires_at_ns: 2,
        }
    }

    #[test]
    fn the_default_allowlist_is_the_briefs_bundle_id_table() {
        let defaults = default_meeting_app_bundle_ids();

        for expected in [
            "us.zoom.xos",
            "com.microsoft.teams2",
            "com.microsoft.teams",
            "com.tinyspeck.slackmacgap",
            "com.webex.meetingmanager",
        ] {
            assert!(
                defaults.iter().any(|bundle_id| bundle_id == expected),
                "{expected} must seed the allowlist"
            );
        }
    }

    #[test]
    fn an_allowlist_entry_only_counts_when_the_app_is_actually_running() {
        let allowlist = default_meeting_app_bundle_ids();

        assert_eq!(app_signal(&[], &allowlist, &unused()), AppSignal::Absent);
        assert_eq!(
            app_signal(&[app("us.zoom.xos", "Zoom", true)], &allowlist, &unused()),
            AppSignal::Known {
                bundle_id: "us.zoom.xos".to_string(),
                display_name: "Zoom".to_string(),
                frontmost: true,
            }
        );
    }

    /* FP1 and FP2 in the detection map: Zoom left in the menu bar while a
     * voice memo records, or Slack open in a background window while a Discord
     * call holds the microphone. Neither app is in use, and neither prompts. */
    #[test]
    fn a_meeting_app_the_operator_has_not_used_is_present_and_not_known() {
        let signal = app_signal(
            &[app("us.zoom.xos", "Zoom", false)],
            &default_meeting_app_bundle_ids(),
            &unused(),
        );

        assert_eq!(
            signal,
            AppSignal::Present {
                bundle_id: "us.zoom.xos".to_string(),
                display_name: "Zoom".to_string(),
            }
        );
    }

    /* The operator joined in Zoom, then switched to Notes to type. Zoom was in
     * front two ticks ago, and the microphone is still its. */
    #[test]
    fn an_app_used_earlier_in_the_episode_stays_known_after_switching_away() {
        let signal = app_signal(
            &[
                app("us.zoom.xos", "Zoom", false),
                app("com.apple.notes", "Notes", true),
            ],
            &default_meeting_app_bundle_ids(),
            &used(&["us.zoom.xos"]),
        );

        assert_eq!(
            signal,
            AppSignal::Known {
                bundle_id: "us.zoom.xos".to_string(),
                display_name: "Zoom".to_string(),
                frontmost: false,
            }
        );
    }

    #[test]
    fn the_app_in_front_is_what_the_episode_remembers() {
        let allowlist = default_meeting_app_bundle_ids();

        assert_eq!(
            frontmost_allowlisted(&[app("us.zoom.xos", "Zoom", true)], &allowlist),
            Some("us.zoom.xos")
        );
        // A browser is read through its title, and a call app through the call
        // dimension; neither belongs in the app set.
        assert_eq!(
            frontmost_allowlisted(&[app("com.google.chrome", "Chrome", true)], &allowlist),
            None
        );
        assert_eq!(
            frontmost_allowlisted(&[app("com.apple.facetime", "FaceTime", true)], &allowlist),
            None
        );
        assert_eq!(
            frontmost_allowlisted(&[app("us.zoom.xos", "Zoom", false)], &allowlist),
            None
        );
    }

    #[test]
    fn a_renamed_bundle_id_contributes_nothing_instead_of_matching_loosely() {
        let signal = app_signal(
            &[app("com.microsoft.teams3", "Microsoft Teams", true)],
            &default_meeting_app_bundle_ids(),
            &unused(),
        );

        assert_eq!(signal, AppSignal::Absent);
    }

    #[test]
    fn the_frontmost_meeting_app_names_the_prompt() {
        let signal = app_signal(
            &[
                app("us.zoom.xos", "Zoom", false),
                app("com.tinyspeck.slackmacgap", "Slack", true),
            ],
            &default_meeting_app_bundle_ids(),
            &used(&["us.zoom.xos"]),
        );

        assert_eq!(
            signal,
            AppSignal::Known {
                bundle_id: "com.tinyspeck.slackmacgap".to_string(),
                display_name: "Slack".to_string(),
                frontmost: true,
            }
        );
    }

    #[test]
    fn a_meeting_app_in_use_outranks_a_frontmost_browser() {
        let signal = app_signal(
            &[
                app("com.google.chrome", "Chrome", true),
                app("us.zoom.xos", "Zoom", false),
            ],
            &default_meeting_app_bundle_ids(),
            &used(&["us.zoom.xos"]),
        );

        assert_eq!(
            signal,
            AppSignal::Known {
                bundle_id: "us.zoom.xos".to_string(),
                display_name: "Zoom".to_string(),
                frontmost: false,
            }
        );
    }

    /* FN4 in the detection map: a Meet call in Chrome while Slack sits in a
     * background window used to read as a Slack huddle. */
    #[test]
    fn a_frontmost_browser_outranks_a_meeting_app_that_is_only_open() {
        let signal = app_signal(
            &[
                app("com.google.chrome", "Chrome", true),
                app("com.tinyspeck.slackmacgap", "Slack", false),
            ],
            &default_meeting_app_bundle_ids(),
            &unused(),
        );

        assert_eq!(
            signal,
            AppSignal::Browser {
                bundle_id: "com.google.chrome".to_string(),
                display_name: "Chrome".to_string(),
            }
        );
    }

    #[test]
    fn a_background_browser_is_not_evidence() {
        let signal = app_signal(
            &[app("com.google.chrome", "Chrome", false)],
            &default_meeting_app_bundle_ids(),
            &unused(),
        );

        assert_eq!(signal, AppSignal::Absent);
    }

    #[test]
    fn an_operator_added_bundle_id_becomes_a_signal() {
        let signal = app_signal(
            &[app("com.example.call", "Example Call", true)],
            &normalize_allowlist(&["  COM.Example.Call  ".to_string()]),
            &unused(),
        );

        assert_eq!(
            signal,
            AppSignal::Known {
                bundle_id: "com.example.call".to_string(),
                display_name: "Example Call".to_string(),
                frontmost: true,
            }
        );
    }

    #[test]
    fn normalizing_an_allowlist_drops_blanks_and_duplicates() {
        let normalized = normalize_allowlist(&[
            "US.Zoom.XOS".to_string(),
            "   ".to_string(),
            "us.zoom.xos".to_string(),
            String::new(),
        ]);

        assert_eq!(normalized, vec!["us.zoom.xos".to_string()]);
    }

    #[test]
    fn browser_evidence_reads_the_existing_activation_offers() {
        let matched = offer(
            "com.google.chrome",
            MeetingProvider::GoogleMeet,
            MeetingEvidenceFlags::app_only().with_ax_host(),
        );
        assert_eq!(
            browser_title_evidence(BrowserTitleRead::Read, &[matched], "com.google.chrome"),
            BrowserTitleEvidence::MeetingMatch
        );

        let untrusted = offer(
            "com.google.chrome",
            MeetingProvider::GoogleMeet,
            MeetingEvidenceFlags::app_only().with_ax_unavailable(),
        );
        assert_eq!(
            browser_title_evidence(BrowserTitleRead::Read, &[untrusted], "com.google.chrome"),
            BrowserTitleEvidence::Unreadable
        );

        assert_eq!(
            browser_title_evidence(BrowserTitleRead::Read, &[], "com.google.chrome"),
            BrowserTitleEvidence::NoMatch
        );
    }

    #[test]
    fn an_app_only_browser_offer_is_not_a_meeting_match() {
        let app_only = offer(
            "com.google.chrome",
            MeetingProvider::ConfiguredApp,
            MeetingEvidenceFlags::app_only(),
        );

        assert_eq!(
            browser_title_evidence(BrowserTitleRead::Read, &[app_only], "com.google.chrome"),
            BrowserTitleEvidence::NoMatch
        );
    }

    /* FN1 and FN2 in the detection map: Chrome activated on Gmail, then the
     * operator joins a Meet call without switching apps, or stays in a call
     * past the two minutes an activation offer lives. The tick's own read is
     * what sees it, and it lands in the store with a fresh TTL. */
    #[test]
    fn a_title_read_on_a_later_tick_matches_after_the_activation_offer_expired() {
        const TTL_NS: u64 = 120_000_000_000;
        let store = MeetingSuggestionService::new(Vec::new(), TTL_NS);
        let meet = |observed_at_ns: u64| MeetingSuggestionSignal {
            provider: MeetingProvider::GoogleMeet,
            app_bundle_id: "com.google.chrome".to_string(),
            observed_at_ns,
            evidence_flags: MeetingEvidenceFlags::app_only().with_ax_host(),
        };

        // The activation edge, and nothing for the next two minutes.
        store.submit(meet(0));
        let later = TTL_NS + 1;
        assert_eq!(
            browser_title_evidence(
                BrowserTitleRead::Read,
                &store.list(later),
                "com.google.chrome"
            ),
            BrowserTitleEvidence::NoMatch,
            "the activation offer has expired"
        );

        // A tick with Chrome in front and the microphone live reads the window.
        store.submit(meet(later));
        assert_eq!(
            browser_title_evidence(
                BrowserTitleRead::Read,
                &store.list(later),
                "com.google.chrome"
            ),
            BrowserTitleEvidence::MeetingMatch
        );
        assert_eq!(
            browser_title_evidence(
                BrowserTitleRead::Read,
                &store.list(later + TTL_NS - 1),
                "com.google.chrome"
            ),
            BrowserTitleEvidence::MeetingMatch,
            "the read restarted the offer's TTL"
        );
    }

    /* A browser's title that does not match a meeting produces no offer at
     * all, so the reader's own answer is the only way to tell "not a call"
     * from "could not look". */
    #[test]
    fn an_unreadable_window_is_unreadable_whatever_the_store_holds() {
        let matched = offer(
            "com.google.chrome",
            MeetingProvider::GoogleMeet,
            MeetingEvidenceFlags::app_only().with_ax_host(),
        );

        assert_eq!(
            browser_title_evidence(
                BrowserTitleRead::Unreadable,
                &[matched],
                "com.google.chrome"
            ),
            BrowserTitleEvidence::Unreadable
        );
        assert_eq!(
            NoBrowserTitles.refresh_frontmost("com.google.chrome"),
            BrowserTitleRead::Unreadable
        );
    }

    #[test]
    fn the_call_apps_ship_in_the_default_allowlist() {
        let defaults = default_meeting_app_bundle_ids();

        for expected in CALL_APP_BUNDLE_IDS {
            assert!(
                defaults.iter().any(|bundle_id| bundle_id == expected),
                "{expected} must seed the allowlist, or the call path can never run"
            );
            assert_eq!(
                *expected,
                expected.to_ascii_lowercase(),
                "the registry stores what WorkspaceApps reports, which is lowercased"
            );
        }
    }

    #[test]
    fn a_call_app_reports_on_the_call_dimension_and_not_the_app_one() {
        let running = [app("com.apple.facetime", "FaceTime", true)];
        let allowlist = default_meeting_app_bundle_ids();

        assert_eq!(
            app_signal(&running, &allowlist, &unused()),
            AppSignal::Absent
        );
        assert_eq!(
            call_signal(&running, &allowlist),
            CallSignal::Running {
                bundle_id: "com.apple.facetime".to_string(),
                display_name: "FaceTime".to_string(),
                frontmost: true,
            }
        );
    }

    /* A backgrounded FaceTime used to be able to win `max_by_key` over a
     * running Zoom once it joined the shipped allowlist, which would have
     * silently traded a detectable meeting for a call that is not happening.
     * The app dimension names Zoom whether it is in use or merely open. */
    #[test]
    fn an_open_call_app_does_not_shadow_a_running_meeting_app() {
        let running = [
            app("com.apple.facetime", "FaceTime", false),
            app("us.zoom.xos", "Zoom", false),
        ];
        let allowlist = default_meeting_app_bundle_ids();

        assert_eq!(
            app_signal(&running, &allowlist, &used(&["us.zoom.xos"])),
            AppSignal::Known {
                bundle_id: "us.zoom.xos".to_string(),
                display_name: "Zoom".to_string(),
                frontmost: false,
            }
        );
        assert_eq!(
            app_signal(&running, &allowlist, &unused()),
            AppSignal::Present {
                bundle_id: "us.zoom.xos".to_string(),
                display_name: "Zoom".to_string(),
            }
        );
    }

    #[test]
    fn a_call_app_removed_from_the_allowlist_stops_being_a_call_signal() {
        let running = [app("com.apple.mobilephone", "Phone", true)];

        assert_eq!(
            call_signal(&running, &["us.zoom.xos".to_string()]),
            CallSignal::Absent
        );
    }

    #[test]
    fn the_frontmost_call_app_names_the_card() {
        let signal = call_signal(
            &[
                app("com.apple.facetime", "FaceTime", false),
                app("com.apple.mobilephone", "Phone", true),
            ],
            &default_meeting_app_bundle_ids(),
        );

        assert_eq!(
            signal,
            CallSignal::Running {
                bundle_id: "com.apple.mobilephone".to_string(),
                display_name: "Phone".to_string(),
                frontmost: true,
            }
        );
    }

    /* A standing grant is only ever consulted for a `CallSignal`, so a grant
     * the call dimension can never raise is consent nothing reads, the picker
     * draws no switch for, and the operator cannot withdraw where grants are
     * managed. Stated against `call_signal` rather than against the predicate
     * the writer uses, so a reverted filter fails here instead of agreeing
     * with itself. */
    #[test]
    fn a_grant_the_call_dimension_can_never_raise_does_not_survive_the_write() {
        // The two stale entries in the wild, plus the two that are real.
        let requested = [
            "us.zoom.xos".to_string(),
            "com.tinyspeck.slackmacgap".to_string(),
            "COM.APPLE.FACETIME".to_string(),
            "  com.apple.mobilephone  ".to_string(),
        ];
        let allowlist = default_meeting_app_bundle_ids();
        let stored = normalize_auto_record(&requested);

        for bundle_id in normalize_allowlist(&requested) {
            let running = [app(&bundle_id, "granted app", true)];
            let readable = matches!(
                call_signal(&running, &allowlist),
                CallSignal::Running { .. }
            );
            assert_eq!(
                stored.contains(&bundle_id),
                readable,
                "{bundle_id}: a stored grant and a grant the call dimension can \
                 raise must be the same set"
            );
        }
        assert_eq!(stored, ["com.apple.facetime", "com.apple.mobilephone"]);
    }

    /* Every tick used to ask launchservicesd for every running application's
     * bundle ID and name: two round trips per application, about 160 every
     * fifteen seconds. A process's identity does not change while it runs, so
     * a read asks only about a pid the previous read did not see, and asks
     * again about a pid that went away and came back. */
    #[cfg(target_os = "macos")]
    #[test]
    fn a_running_process_is_asked_who_it_is_once() {
        fn identify(pid: i32) -> Option<AppIdentity> {
            let (bundle_id, display_name) = match pid {
                10 => ("us.zoom.xos", "Zoom"),
                11 => ("com.apple.safari", "Safari"),
                // A process without a bundle ID.
                _ => return None,
            };
            Some(AppIdentity {
                bundle_id: bundle_id.to_string(),
                display_name: display_name.to_string(),
            })
        }
        let everything = [(10, 10), (11, 11), (12, 12)];
        let mut known = KnownProcesses::default();
        let mut asked = Vec::new();

        let first = known.running_apps(everything, Some(11), |&pid| {
            asked.push(pid);
            identify(pid)
        });
        assert_eq!(
            asked,
            [10, 11, 12],
            "the first read asks about every process"
        );
        assert_eq!(
            first,
            [
                app("us.zoom.xos", "Zoom", false),
                app("com.apple.safari", "Safari", true)
            ]
        );

        asked.clear();
        let second = known.running_apps(everything, Some(10), |&pid| {
            asked.push(pid);
            identify(pid)
        });
        assert!(asked.is_empty(), "nothing started, so nothing is asked");
        assert_eq!(
            second,
            [
                app("us.zoom.xos", "Zoom", true),
                app("com.apple.safari", "Safari", false)
            ],
            "which process is in front is read fresh every time"
        );

        known.running_apps([(10, 10), (12, 12)], None, |&pid| {
            asked.push(pid);
            identify(pid)
        });
        known.running_apps(everything, None, |&pid| {
            asked.push(pid);
            identify(pid)
        });
        assert_eq!(asked, [11], "a pid that went away is asked about again");
    }
}
