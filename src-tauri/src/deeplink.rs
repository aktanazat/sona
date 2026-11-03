//! `sona://` URL routing.
//!
//! Parsing is kept separate from dispatch so the route table can be tested
//! without an `AppHandle`. Dispatch lives in `lib.rs`, next to the CLI and tray
//! handlers it shares plumbing with.

use uuid::Uuid;

/// A resolved `sona://` route.
///
/// Every variant maps onto an action the app already exposes through the tray,
/// the CLI, or a command. A deep link is an alternative trigger, never a new
/// capability, so nothing here can do something a user cannot already do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeepLinkAction {
    /// `sona://record` — same toggle as `--toggle-transcription` and the tray.
    ToggleRecording,
    /// `sona://record?mode=<id>` — start a run under one specific mode without
    /// changing which mode is active afterwards.
    RecordWithMode(String),
    /// `sona://mode/<id>` — switch the active mode, recording nothing.
    SetActiveMode(String),
    /// `sona://meeting/start` — surface the meeting screen. Deliberately does
    /// not begin capture: meetings are prompt-only by product decision, and a
    /// URL is not consent to record a room.
    StartMeeting,
    /// `sona://meeting/<uuid>` opens a conversation; `atMs` and `segmentId` select a retained contribution.
    OpenMeeting(Uuid, Option<u64>, Option<Uuid>),
    /// `sona://person/<uuid>` — open one person's page.
    OpenPerson(Uuid),
    /// `sona://organization/<slug>` — open the page for everybody at one
    /// organization.
    ///
    /// A slug rather than a uuid because an organization is not a stored row in
    /// this corpus: it is the set of people whose calendar addresses landed on
    /// one domain, so its only stable address is its own name.
    OpenOrganization(String),
    /// `sona://loop/<id>` — open the review of the meeting this loop belongs
    /// to.
    ///
    /// The id is a loop's own address, `<meeting uuid>:<kind>:<digest>`, and is
    /// carried whole rather than split apart here: its format belongs to
    /// `meeting::loop_types`, and the route table validates only what it can
    /// see — that the meeting it names is a uuid.
    OpenLoop(String),
    /// `sona://dictation/<id>` — open one dictation's row in History.
    ///
    /// Not a query-plane noun the round-3 brief named, and here for a reason
    /// worth writing down: every row the query plane returns carries a
    /// `sona://` link, dictations are one of the kinds it returns, and History
    /// had no address. A link that resolves to nothing would be worse than the
    /// route.
    OpenDictation(i64),
    OpenNote(Uuid, Option<i64>),
    OpenSubject(Uuid),
    /// `sona://search?q=…` — open the search surface with the question in it.
    /// An empty or absent `q` opens it empty, which is what pressing ⌘K does;
    /// a blank `q=` is a link built wrong and is refused instead.
    Search(String),
}

/// The one scheme this app answers to.
pub const DEEP_LINK_SCHEME: &str = "sona";

/// The route keyword of a URL addressed to this app, route table or not.
///
/// A `sona://` URL nobody can parse is still ours, and every reader of a URL
/// list has to be told so: on macOS the next reader is the audio importer,
/// which answers a mistyped link with a failed-import error for a file that
/// never existed, and on the argv path it is a branch that does nothing at all.
///
/// The host comes back rather than the whole URL because an unroutable link
/// still has to be reported, and an unroutable link can carry private text:
/// `sona://search?q=` is refused when its `q` is blank, and that `q` is
/// whatever the person was searching for. A hostless `sona:` URL answers with an
/// empty name: still ours, and [`parse_deep_link`] refuses it for the same
/// reason, so both agree on what "ours" means.
pub fn deep_link_route(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw.trim()).ok()?;
    if !url.scheme().eq_ignore_ascii_case(DEEP_LINK_SCHEME) {
        return None;
    }
    Some(url.host_str().unwrap_or_default().to_string())
}

/// Parses a `sona://` URL into an action, or `None` when it is not a route this
/// app serves.
///
/// Unknown hosts, unknown paths, a missing mode id, and any other scheme all
/// return `None` rather than a best guess: silently reinterpreting a link the
/// user (or another app) got wrong is worse than ignoring it.
pub fn parse_deep_link(raw: &str) -> Option<DeepLinkAction> {
    let url = url::Url::parse(raw.trim()).ok()?;
    if !url.scheme().eq_ignore_ascii_case(DEEP_LINK_SCHEME) {
        return None;
    }

    // A non-special scheme keeps its authority verbatim, so the route keyword
    // is the host and everything after it is the path.
    let route = url.host_str()?;
    let segments: Vec<&str> = url
        .path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();

    if route.eq_ignore_ascii_case("record") && segments.is_empty() {
        // An absent `mode` means "use whatever is active". A present but blank
        // `mode` means the link was built wrong, which is not the same request,
        // so it is refused rather than quietly downgraded to a plain toggle.
        return match url.query_pairs().find(|(key, _)| key == "mode") {
            Some((_, value)) => non_empty(&value).map(DeepLinkAction::RecordWithMode),
            None => Some(DeepLinkAction::ToggleRecording),
        };
    }
    if route.eq_ignore_ascii_case("mode") {
        let [mode_id] = segments[..] else {
            return None;
        };
        return non_empty(mode_id).map(DeepLinkAction::SetActiveMode);
    }
    if route.eq_ignore_ascii_case("meeting") {
        // `start` is a keyword, not an id: it was this route's only meaning
        // before meetings were addressable, and it keeps that meaning.
        if segments == ["start"] {
            return Some(DeepLinkAction::StartMeeting);
        }
        let [session_id] = segments[..] else {
            return None;
        };
        let session_id = uuid(session_id)?;
        let mut moments = url.query_pairs().filter(|(key, _)| key == "atMs");
        let moment = match moments.next() {
            Some((_, value)) => Some(
                value
                    .parse::<u64>()
                    .ok()
                    .filter(|ms| *ms <= i64::MAX as u64 / 1_000_000)?,
            ),
            None => None,
        };
        if moments.next().is_some() {
            return None;
        }
        let mut segments = url.query_pairs().filter(|(key, _)| key == "segmentId");
        let segment = match segments.next() {
            Some((_, value)) => Some(uuid(&value)?),
            None => None,
        };
        if segments.next().is_some() {
            return None;
        }
        return Some(DeepLinkAction::OpenMeeting(session_id, moment, segment));
    }
    if route.eq_ignore_ascii_case("person") {
        let [person_id] = segments[..] else {
            return None;
        };
        return uuid(person_id).map(DeepLinkAction::OpenPerson);
    }
    if route.eq_ignore_ascii_case("note") {
        let [note_id] = segments[..] else {
            return None;
        };
        let note_id = uuid(note_id)?;
        let mut versions = url.query_pairs().filter(|(key, _)| key == "version");
        let version_id = match versions.next() {
            Some((_, value)) => Some(value.parse::<i64>().ok().filter(|id| *id > 0)?),
            None => None,
        };
        if versions.next().is_some() {
            return None;
        }
        return Some(DeepLinkAction::OpenNote(note_id, version_id));
    }
    if route.eq_ignore_ascii_case("subject") {
        let [subject_id] = segments[..] else {
            return None;
        };
        return uuid(subject_id).map(DeepLinkAction::OpenSubject);
    }
    if route.eq_ignore_ascii_case("organization") {
        let [slug] = segments[..] else {
            return None;
        };
        return non_empty(slug).map(DeepLinkAction::OpenOrganization);
    }
    if route.eq_ignore_ascii_case("loop") {
        let [loop_id] = segments[..] else {
            return None;
        };
        let loop_id = non_empty(loop_id)?;
        // The meeting has to be there for the link to route anywhere. The rest
        // of the id is the ledger's business, not this table's.
        uuid(loop_id.split(':').next()?)?;
        return Some(DeepLinkAction::OpenLoop(loop_id));
    }
    if route.eq_ignore_ascii_case("dictation") {
        // Parsed here rather than downstream: a history row is numbered, so a
        // non-numeric id is a malformed link, not a row that has gone away.
        let [history_id] = segments[..] else {
            return None;
        };
        return history_id
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0)
            .map(DeepLinkAction::OpenDictation);
    }
    if route.eq_ignore_ascii_case("search") && segments.is_empty() {
        return match url.query_pairs().find(|(key, _)| key == "q") {
            Some((_, value)) => non_empty(&value).map(DeepLinkAction::Search),
            None => Some(DeepLinkAction::Search(String::new())),
        };
    }
    None
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// An id that cannot be a uuid cannot name a meeting or a person, so the route
/// is refused rather than dispatched to a lookup that must fail.
fn uuid(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_record_toggles() {
        assert_eq!(
            parse_deep_link("sona://record"),
            Some(DeepLinkAction::ToggleRecording)
        );
        assert_eq!(
            parse_deep_link("sona://record/"),
            Some(DeepLinkAction::ToggleRecording)
        );
    }

    #[test]
    fn record_carries_an_explicit_mode() {
        assert_eq!(
            parse_deep_link("sona://record?mode=mode_1712345678901"),
            Some(DeepLinkAction::RecordWithMode(
                "mode_1712345678901".to_string()
            ))
        );
    }

    #[test]
    fn mode_route_switches_the_active_mode() {
        assert_eq!(
            parse_deep_link("sona://mode/mode_42"),
            Some(DeepLinkAction::SetActiveMode("mode_42".to_string()))
        );
    }

    #[test]
    fn start_stays_a_keyword_beside_addressable_meetings() {
        assert_eq!(
            parse_deep_link("sona://meeting/start"),
            Some(DeepLinkAction::StartMeeting)
        );
        assert_eq!(parse_deep_link("sona://meeting"), None);
        // Not an id and not a verb: nothing this route can do.
        assert_eq!(parse_deep_link("sona://meeting/stop"), None);
    }

    #[test]
    fn every_query_plane_noun_has_a_route() {
        let session = Uuid::from_u128(0x9f2c);
        let person = Uuid::from_u128(0x4d10);
        assert_eq!(
            parse_deep_link(&format!("sona://meeting/{session}")),
            Some(DeepLinkAction::OpenMeeting(session, None, None))
        );
        assert_eq!(
            parse_deep_link(&format!("sona://person/{person}")),
            Some(DeepLinkAction::OpenPerson(person))
        );
        assert_eq!(
            parse_deep_link(&format!("sona://loop/{session}:loop:0123456789abcdef")),
            Some(DeepLinkAction::OpenLoop(format!(
                "{session}:loop:0123456789abcdef"
            )))
        );
        assert_eq!(
            parse_deep_link("sona://dictation/4218"),
            Some(DeepLinkAction::OpenDictation(4218))
        );
        assert_eq!(
            parse_deep_link("sona://search?q=pricing%20tier"),
            Some(DeepLinkAction::Search("pricing tier".to_string()))
        );
        assert_eq!(
            parse_deep_link("sona://organization/acme"),
            Some(DeepLinkAction::OpenOrganization("acme".to_string()))
        );
    }

    #[test]
    fn a_bare_search_route_opens_the_surface_empty() {
        assert_eq!(
            parse_deep_link("sona://search"),
            Some(DeepLinkAction::Search(String::new()))
        );
        // A `q` that was built and left blank is a broken link, not ⌘K.
        assert_eq!(parse_deep_link("sona://search?q="), None);
        assert_eq!(parse_deep_link("sona://search?q=%20"), None);
        assert_eq!(parse_deep_link("sona://search/extra"), None);
    }

    #[test]
    fn an_id_that_cannot_name_a_row_is_refused() {
        let session = Uuid::from_u128(0x9f2c);
        assert_eq!(parse_deep_link("sona://meeting/not-a-uuid"), None);
        assert_eq!(parse_deep_link("sona://person/"), None);
        assert_eq!(parse_deep_link("sona://person/steven"), None);
        assert_eq!(parse_deep_link("sona://loop/"), None);
        assert_eq!(parse_deep_link("sona://loop/loop:0123456789abcdef"), None);
        assert_eq!(
            parse_deep_link(&format!("sona://loop/{session}/extra")),
            None
        );
        assert_eq!(parse_deep_link("sona://dictation/abc"), None);
        assert_eq!(parse_deep_link("sona://dictation/0"), None);
        assert_eq!(parse_deep_link("sona://dictation/-3"), None);
        assert_eq!(parse_deep_link("sona://dictation"), None);
        assert_eq!(parse_deep_link("sona://organization"), None);
        assert_eq!(parse_deep_link("sona://organization/"), None);
        assert_eq!(parse_deep_link("sona://organization/acme/extra"), None);
    }

    /// The plane builds these strings and this table parses them. A round trip
    /// is the only thing that proves the two agree, and a row whose link does
    /// not route is a citation an agent cannot hand to a human.
    #[test]
    fn plane_links_round_trip_through_this_table() {
        use crate::meeting::loop_types::{MeetingLoopId, MeetingLoopKind};
        use crate::meeting::people_types::PersonId;
        use crate::meeting::types::MeetingSessionId;

        let session_id = MeetingSessionId::new();
        let person_id = PersonId::new();
        let loop_id = MeetingLoopId::derive(
            session_id,
            MeetingLoopKind::Loop,
            "Which tier does the trial convert into?",
        );

        assert_eq!(
            parse_deep_link(&crate::query::meeting_link(session_id)),
            Some(DeepLinkAction::OpenMeeting(session_id.uuid(), None, None))
        );
        assert_eq!(
            parse_deep_link(&crate::query::person_link(person_id)),
            Some(DeepLinkAction::OpenPerson(person_id.uuid()))
        );
        assert_eq!(
            parse_deep_link(&crate::query::loop_link(&loop_id)),
            Some(DeepLinkAction::OpenLoop(loop_id.as_str().to_string()))
        );
        assert_eq!(
            parse_deep_link(&crate::query::dictation_link(4218)),
            Some(DeepLinkAction::OpenDictation(4218))
        );
        assert_eq!(
            parse_deep_link(&crate::query::search_link("what did I promise Steven?")),
            Some(DeepLinkAction::Search(
                "what did I promise Steven?".to_string()
            ))
        );
        assert_eq!(
            parse_deep_link(&crate::query::organization_link("Acme")),
            Some(DeepLinkAction::OpenOrganization("acme".to_string()))
        );
    }

    #[test]
    fn the_scheme_is_matched_case_insensitively() {
        assert_eq!(
            parse_deep_link("SONA://record"),
            Some(DeepLinkAction::ToggleRecording)
        );
    }

    #[test]
    fn other_schemes_are_not_ours() {
        assert_eq!(parse_deep_link("file:///tmp/audio.wav"), None);
        assert_eq!(parse_deep_link("https://sona.app/record"), None);
        assert_eq!(parse_deep_link("sonata://record"), None);
    }

    #[test]
    fn malformed_routes_are_ignored_rather_than_guessed() {
        assert_eq!(parse_deep_link("sona://record/extra"), None);
        assert_eq!(parse_deep_link("sona://mode"), None);
        assert_eq!(parse_deep_link("sona://mode/"), None);
        assert_eq!(parse_deep_link("sona://mode/a/b"), None);
        assert_eq!(parse_deep_link("sona://quit"), None);
        assert_eq!(parse_deep_link("not a url"), None);
        assert_eq!(parse_deep_link(""), None);
    }

    #[test]
    fn a_blank_mode_query_is_malformed_not_a_toggle() {
        assert_eq!(parse_deep_link("sona://record?mode="), None);
        assert_eq!(parse_deep_link("sona://record?mode=%20"), None);
    }

    #[test]
    fn unrelated_query_parameters_do_not_select_a_mode() {
        assert_eq!(
            parse_deep_link("sona://record?source=raycast"),
            Some(DeepLinkAction::ToggleRecording)
        );
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        assert_eq!(
            parse_deep_link("  sona://record\n"),
            Some(DeepLinkAction::ToggleRecording)
        );
    }

    /// The scheme claim is deliberately wider than the route table: an address
    /// of ours that names no route must not fall through to another reader, and
    /// the name it reports must never carry the query.
    #[test]
    fn every_sona_url_is_ours_even_without_a_route() {
        for (raw, route) in [
            ("sona://record", "record"),
            ("sona://meeting/not-a-uuid", "meeting"),
            ("sona://quit", "quit"),
            ("SONA://mode/", "mode"),
            ("  sona://dictation/0\n", "dictation"),
            ("sona:record", ""),
        ] {
            assert_eq!(deep_link_route(raw).as_deref(), Some(route), "{raw}");
        }
        // A refused search still names only its route: the question it carries
        // is the user's, and a diagnostic is not the place for it.
        assert_eq!(
            deep_link_route("sona://search?q=my%20private%20question").as_deref(),
            Some("search")
        );
        for raw in ["file:///tmp/audio.wav", "sonata://record", "not a url", ""] {
            assert_eq!(deep_link_route(raw), None, "{raw} should not be claimed");
        }
    }
}
