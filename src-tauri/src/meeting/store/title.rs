//! What a meeting title is when nobody chose it, and the topic a summary
//! headline names once its lead-in is gone.
//!
//! Two questions, both pure. `is_placeholder` decides whether a stored title
//! is one Sona made up to have something to show — "Local notes", "Slack
//! meeting", "Call in Safari", "FaceTime call, 3:15 PM" — and may therefore be
//! replaced by a title read from the notes. `topic` turns the notes model's
//! headline into that title: "The meeting addressed the Q3 pricing rollout."
//! names "Q3 pricing rollout".

use crate::meeting::types::MANUAL_DEFAULT_TITLE;

/// Names detection writes before " meeting" or " call, <time>": the default
/// allowlist's apps as macOS names them, and the services a browser call or
/// an operator-added app is most often named for. Compared case-insensitively.
///
/// A closed list on purpose. "<anything> meeting" would also match "Board
/// meeting" and "Team meeting", which are titles a person typed.
const MEETING_APP_NAMES: &[&str] = &[
    "zoom",
    "zoom.us",
    "zoom workplace",
    "microsoft teams",
    "teams",
    "slack",
    "webex",
    "cisco webex",
    "cisco webex meetings",
    "facetime",
    "phone",
    "google meet",
    "meet",
    "discord",
    "skype",
    "whatsapp",
    "signal",
    "telegram",
    "tuple",
    "around",
    "gather",
    "jitsi meet",
    "bluejeans",
    "goto",
    "goto meeting",
    "ringcentral",
    "huddle",
];

/// Whether `title` is a name Sona gave a recording itself, so a title read
/// from the notes may replace it.
pub(super) fn is_placeholder(title: &str) -> bool {
    let title = title.trim();
    if title == MANUAL_DEFAULT_TITLE {
        return true;
    }
    let lower = title.to_lowercase();
    if let Some(app) = lower.strip_suffix(" meeting") {
        return is_meeting_app(app);
    }
    // `holder_title` for a browser with no tab title: "Call in Safari".
    if let Some(browser) = lower.strip_prefix("call in ") {
        let words = browser.split_whitespace().count();
        return (1..=3).contains(&words);
    }
    // `call_meeting_title`: "FaceTime call, 3:15 PM".
    if let Some((app, time)) = lower.split_once(" call, ") {
        return is_meeting_app(app) && is_clock_time(time);
    }
    false
}

fn is_meeting_app(name: &str) -> bool {
    MEETING_APP_NAMES.contains(&name.trim())
}

/// "3:15 pm", as `%-I:%M %p` writes it, already lowercased.
fn is_clock_time(text: &str) -> bool {
    let Some((clock, meridiem)) = text.trim().split_once(' ') else {
        return false;
    };
    let Some((hour, minute)) = clock.split_once(':') else {
        return false;
    };
    matches!(meridiem, "am" | "pm")
        && matches!(hour.parse::<u8>(), Ok(1..=12))
        && minute.len() == 2
        && matches!(minute.parse::<u8>(), Ok(0..=59))
}

/// Openers that set a scene before the subject: "In this meeting, the team…".
const SCENE_SETTERS: &[&str] = &[
    "in this meeting",
    "in the meeting",
    "in this call",
    "in the call",
    "in this conversation",
    "in this discussion",
    "in this session",
    "in this huddle",
    "during this meeting",
    "during the meeting",
    "during this call",
    "during the call",
];

/// Subjects that name the meeting or everyone in it rather than a topic.
const MEETING_SUBJECTS: &[&str] = &[
    "the meeting",
    "this meeting",
    "the discussion",
    "this discussion",
    "the call",
    "this call",
    "the conversation",
    "this conversation",
    "the session",
    "this session",
    "the huddle",
    "this huddle",
    "the sync",
    "this sync",
    "the standup",
    "the team",
    "the group",
    "the participants",
    "participants",
    "the attendees",
    "attendees",
    "the speakers",
    "speakers",
    "everyone",
    "we",
    "they",
];

const HEDGES: &[&str] = &[
    "primarily",
    "mainly",
    "mostly",
    "largely",
    "briefly",
    "also",
    "first",
];

/// Verbs that only say a topic was talked about. Longest forms first, so
/// "was focused on" is not read as "was" plus a topic.
const TOPIC_VERBS: &[&str] = &[
    "was focused on",
    "was centered on",
    "was centered around",
    "was mostly about",
    "was mainly about",
    "was about",
    "is about",
    "focused on",
    "focuses on",
    "centered on",
    "centered around",
    "revolved around",
    "dealt with",
    "went over",
    "talked about",
    "talked through",
    "touched on",
    "walked through",
    "caught up on",
    "met to discuss",
    "met to review",
    "met about",
    "addressed",
    "addresses",
    "covered",
    "covers",
    "discussed",
    "discusses",
    "reviewed",
    "reviews",
    "explored",
    "examined",
    "concerned",
    "considered",
];

/// Where a topic ends and commentary on it begins.
const CLAUSE_BREAKS: &[&str] = &[
    ", with ",
    ", including ",
    ", as well as ",
    ", while ",
    ", before ",
    ", after ",
    ", but ",
    ", and then ",
    ", which ",
    ", where ",
    "; ",
    " — ",
    " – ",
];

/// `text` without `prefix` when it starts with it as whole words, compared
/// case-insensitively. ASCII prefixes only, so the byte length is the same in
/// either case.
fn strip_words<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let rest = &text[prefix.len()..];
    match rest.chars().next() {
        None => Some(rest),
        Some(next) if next.is_whitespace() || next == ',' => {
            Some(rest.trim_start_matches([',', ' ']))
        }
        Some(_) => None,
    }
}

fn strip_any<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    prefixes.iter().find_map(|prefix| strip_words(text, prefix))
}

/// The topic a headline sentence names, when it opens by saying a meeting
/// talked about something: "The meeting addressed the Q3 pricing rollout,
/// with Dana presenting" is "Q3 pricing rollout".
///
/// A sentence that already opens on its subject — "Pricing stayed open and
/// Dana took the comparison" — comes back unchanged: it says what happened,
/// which is a better title than any part of it.
pub(super) fn topic(sentence: &str) -> &str {
    let sentence = sentence.trim();
    let after_scene = strip_any(sentence, SCENE_SETTERS).unwrap_or(sentence);
    let Some(after_subject) = strip_any(after_scene, MEETING_SUBJECTS) else {
        return sentence;
    };
    let after_hedge = strip_any(after_subject, HEDGES).unwrap_or(after_subject);
    let Some(after_verb) = strip_any(after_hedge, TOPIC_VERBS) else {
        return sentence;
    };
    let lower = after_verb.to_lowercase();
    let end = CLAUSE_BREAKS
        .iter()
        .filter_map(|mark| lower.find(mark))
        // A lowercased string can differ in length from its source; only a
        // cut that lands on a boundary of the source is used.
        .filter(|&index| lower.len() == after_verb.len() && after_verb.is_char_boundary(index))
        .min()
        .unwrap_or(after_verb.len());
    let named = after_verb[..end].trim();
    let named = strip_any(named, &["the", "a", "an", "how to", "how", "about"]).unwrap_or(named);
    if named.is_empty() {
        sentence
    } else {
        named
    }
}
