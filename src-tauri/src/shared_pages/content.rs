//! What a shared page says: the draft built from a meeting, and the exact
//! `sona-meeting-page-v1` page a draft filters into. The viewer
//! (`share-site/viewer.js`) refuses anything past the bounds below, so each
//! bound has a twin there. Text bounds count UTF-8 bytes and the viewer
//! counts UTF-16 units; a string is never fewer bytes than units, so what
//! passes here passes there.

use chrono::{DateTime, Duration, Local, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use specta::Type;

use super::{Code, SharedPagesError};
use crate::meeting::review_text::{self, readable_artifact};
use crate::meeting::types::MeetingReviewSnapshot;

pub(crate) const PAGE_FORMAT: &str = "sona-meeting-page-v1";
pub(crate) const DRAFT_VERSION: u32 = 1;
const MAX_TITLE_CHARS: usize = 240;
pub(crate) const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_TURNS: usize = 20_000;
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
const UNKNOWN_SPEAKER: &str = "Unknown speaker";
const NS_PER_SECOND: u64 = 1_000_000_000;

/// Which sections go out. The sheet's defaults: the notes Sona wrote go,
/// his own notes, the transcript and the audio stay until he turns them on.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct Include {
    pub title: bool,
    pub summary: bool,
    pub action_items: bool,
    pub decisions: bool,
    pub notes: bool,
    pub transcript: bool,
    pub audio: bool,
}

impl Default for Include {
    fn default() -> Self {
        Self {
            title: true,
            summary: true,
            action_items: true,
            decisions: true,
            notes: false,
            transcript: false,
            audio: false,
        }
    }
}

pub(crate) const SECTION_NAMES: [&str; 7] = [
    "title",
    "summary",
    "action_items",
    "decisions",
    "notes",
    "transcript",
    "audio",
];

impl Include {
    /// Exactly the named sections; any other is off.
    pub(crate) fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<Self, String> {
        let mut include = Self {
            title: false,
            summary: false,
            action_items: false,
            decisions: false,
            notes: false,
            transcript: false,
            audio: false,
        };
        for name in names
            .into_iter()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let flag = match name {
                "title" => &mut include.title,
                "summary" => &mut include.summary,
                "action_items" => &mut include.action_items,
                "decisions" => &mut include.decisions,
                "notes" => &mut include.notes,
                "transcript" => &mut include.transcript,
                "audio" => &mut include.audio,
                other => {
                    return Err(format!(
                        "Unknown section {other}. Sections: {}.",
                        SECTION_NAMES.join(", ")
                    ))
                }
            };
            *flag = true;
        }
        Ok(include)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerMode {
    #[default]
    Names,
    Numbered,
}

/// How long a share lives: one day, a week or thirty days.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(try_from = "u32", into = "u32")]
pub struct ExpiresInDays(u32);

impl TryFrom<u32> for ExpiresInDays {
    type Error = &'static str;

    fn try_from(days: u32) -> Result<Self, Self::Error> {
        match days {
            1 | 7 | 30 => Ok(Self(days)),
            _ => Err("expires_in_days is 1, 7 or 30"),
        }
    }
}

impl From<ExpiresInDays> for u32 {
    fn from(days: ExpiresInDays) -> Self {
        days.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ActionItem {
    pub text: String,
    pub owner: Option<String>,
    pub due: Option<String>,
}

/// One speaker turn. `s` indexes the speakers list beside it; `t` is seconds
/// from the start of the recording.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
pub struct Turn {
    pub s: u32,
    pub t: f64,
    pub text: String,
}

/// Everything the meeting has, with his edits. Filtering happens only when a
/// page is built, so turning a section off never loses an edit.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
pub struct DraftContent {
    pub title: String,
    pub date: Option<String>,
    pub duration_s: Option<u64>,
    pub summary: Vec<String>,
    pub decisions: Vec<String>,
    pub action_items: Vec<ActionItem>,
    pub notes: Vec<String>,
    pub speakers: Vec<String>,
    pub transcript: Vec<Turn>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
pub struct Draft {
    pub version: u32,
    pub meeting_id: String,
    pub prepared_by: Option<String>,
    pub include: Include,
    pub speakers: SpeakerMode,
    pub expires_in_days: Option<ExpiresInDays>,
    /// The one person the page is for; its action items then keep only his.
    /// `None` shares the page to everyone.
    #[serde(default)]
    pub for_person: Option<String>,
    pub content: DraftContent,
    pub audio_available: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PageAudio {
    pub r#type: String,
    pub parts: u32,
    pub bytes: u64,
    pub duration_s: u64,
}

/// The page as it is encrypted and as the preview renders it. Every content
/// key is present, `null` when excluded or empty.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
pub struct Page {
    pub format: String,
    pub title: Option<String>,
    pub for_person: Option<String>,
    pub date: Option<String>,
    pub duration_s: Option<u64>,
    pub summary: Option<Vec<String>>,
    pub decisions: Option<Vec<String>>,
    pub action_items: Option<Vec<ActionItem>>,
    pub notes: Option<Vec<String>>,
    pub speakers: Option<Vec<String>>,
    pub transcript: Option<Vec<Turn>>,
    pub audio: Option<PageAudio>,
    pub shared_at: String,
    pub expires_at: Option<String>,
}

/// A page and the bytes that get encrypted.
pub(crate) struct BuiltPage {
    pub page: Page,
    pub json: Vec<u8>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// A meeting as a draft: every section it has, the sheet's default
/// selection, speakers by name.
pub(crate) fn draft_from_meeting(
    review: &MeetingReviewSnapshot,
    user_notes: &str,
    audio_available: bool,
) -> Draft {
    let session = &review.session;
    let generated =
        readable_artifact(&review.artifacts).and_then(|artifact| artifact.content.as_ref());
    let date = session
        .started_at_utc_ms
        .and_then(|ms| Local.timestamp_millis_opt(ms).single())
        .map(|start| start.to_rfc3339_opts(SecondsFormat::Secs, false));
    let duration_ns = review
        .session
        .elapsed_offset_ns
        .into_iter()
        .chain(
            review
                .tracks
                .iter()
                .filter_map(|track| track.last_offset_ns),
        )
        .max();
    let names = review_text::speaker_names(review);
    let mut speaker_ids = Vec::new();
    let mut speakers = Vec::new();
    let mut transcript = Vec::new();
    for turn in review_text::transcript_turns(review, MAX_TEXT_BYTES) {
        let index = match speaker_ids.iter().position(|id| *id == turn.speaker_id) {
            Some(index) => index,
            None => {
                speaker_ids.push(turn.speaker_id);
                speakers.push(
                    names
                        .get(&turn.speaker_id)
                        .map(|name| name.trim())
                        .filter(|name| !name.is_empty())
                        .unwrap_or(UNKNOWN_SPEAKER)
                        .to_owned(),
                );
                speaker_ids.len() - 1
            }
        };
        transcript.push(Turn {
            s: u32::try_from(index).unwrap_or(u32::MAX),
            t: seconds(turn.start_offset_ns),
            text: turn.text,
        });
    }
    Draft {
        version: DRAFT_VERSION,
        meeting_id: session.session_id.uuid().to_string(),
        prepared_by: None,
        include: Include::default(),
        speakers: SpeakerMode::Names,
        expires_in_days: None,
        for_person: None,
        content: DraftContent {
            title: single_line(&session.title)
                .chars()
                .take(MAX_TITLE_CHARS)
                .collect::<String>()
                .trim_end()
                .to_owned(),
            date,
            duration_s: duration_ns.map(|ns| (ns + NS_PER_SECOND / 2) / NS_PER_SECOND),
            summary: generated
                .map(|content| review_text::paragraphs(&content.summary.text))
                .unwrap_or_default(),
            decisions: generated
                .map(|content| {
                    content
                        .decisions
                        .iter()
                        .map(|decision| decision.text.trim().to_owned())
                        .filter(|text| !text.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            action_items: generated
                .map(|content| {
                    content
                        .action_items
                        .iter()
                        .map(|item| ActionItem {
                            text: item.text.text.trim().to_owned(),
                            owner: non_empty(item.owner_text.as_deref()),
                            due: non_empty(item.due_text.as_deref()),
                        })
                        .filter(|item| !item.text.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            notes: review_text::paragraphs(user_notes),
            speakers,
            transcript,
        },
        audio_available,
    }
}

/// The exact page `draft` shares, with `audio` when it carries the recording.
pub(crate) fn page_from_draft(
    draft: &Draft,
    audio: Option<PageAudio>,
    shared_at: DateTime<Utc>,
) -> Result<BuiltPage, SharedPagesError> {
    let include = draft.include;
    let content = &draft.content;
    let title =
        Some(single_line(&content.title)).filter(|title| include.title && !title.is_empty());
    if title
        .as_ref()
        .is_some_and(|title| title.chars().count() > MAX_TITLE_CHARS)
    {
        return Err(too_large("The title is longer than 240 characters."));
    }
    let for_person = draft
        .for_person
        .as_deref()
        .map(single_line)
        .filter(|person| !person.is_empty());
    if for_person
        .as_ref()
        .is_some_and(|person| person.chars().count() > MAX_TITLE_CHARS)
    {
        return Err(too_large("The person is longer than 240 characters."));
    }
    if content.date.as_deref().is_some_and(|date| {
        DateTime::parse_from_rfc3339(date).is_err()
            || date.as_bytes().get(10) != Some(&b'T')
            || date.ends_with('z')
    }) {
        return Err(SharedPagesError::new(
            Code::InvalidDraft,
            "The draft's date is not an RFC 3339 time.",
        ));
    }
    let expires_at = draft
        .expires_in_days
        .map(|days| shared_at + Duration::days(i64::from(u32::from(days))));
    let (speakers, transcript) = if include.transcript {
        turns(&content.speakers, &content.transcript, draft.speakers)?.unzip()
    } else {
        (None, None)
    };
    let page = Page {
        format: PAGE_FORMAT.to_owned(),
        date: title.as_ref().and(content.date.clone()),
        duration_s: title.as_ref().and(content.duration_s),
        title,
        for_person: for_person.clone(),
        summary: texts(include.summary, &content.summary)?,
        decisions: texts(include.decisions, &content.decisions)?,
        action_items: action_items(
            include.action_items,
            &content.action_items,
            for_person.as_deref(),
        )?,
        notes: texts(include.notes, &content.notes)?,
        speakers,
        transcript,
        audio: audio.filter(|_| include.audio),
        shared_at: shared_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        expires_at: expires_at.map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true)),
    };
    let json = serde_json::to_vec(&page)
        .map_err(|_| SharedPagesError::new(Code::Failed, "The page could not be encoded."))?;
    if json.len() > MAX_PAGE_BYTES {
        return Err(too_large("The page is larger than 8 MiB."));
    }
    Ok(BuiltPage {
        page,
        json,
        expires_at,
    })
}

/// Numbers that say what a page carries, for a dry run.
#[derive(Clone, Debug, Serialize)]
pub struct PageSummary {
    pub sections: Vec<&'static str>,
    pub summary_paragraphs: usize,
    pub decisions: usize,
    pub action_items: usize,
    pub notes_paragraphs: usize,
    pub speakers: usize,
    pub transcript_turns: usize,
    pub bytes: usize,
    pub audio_parts: u32,
    pub audio_bytes: u64,
}

impl PageSummary {
    pub(crate) fn of(built: &BuiltPage) -> Self {
        let page = &built.page;
        let count = |list: &Option<Vec<String>>| list.as_ref().map_or(0, Vec::len);
        let sections = [
            ("title", page.title.is_some()),
            ("summary", page.summary.is_some()),
            ("action_items", page.action_items.is_some()),
            ("decisions", page.decisions.is_some()),
            ("notes", page.notes.is_some()),
            ("transcript", page.transcript.is_some()),
            ("audio", page.audio.is_some()),
        ]
        .into_iter()
        .filter(|(_, present)| *present)
        .map(|(name, _)| name)
        .collect();
        Self {
            sections,
            summary_paragraphs: count(&page.summary),
            decisions: count(&page.decisions),
            action_items: page.action_items.as_ref().map_or(0, Vec::len),
            notes_paragraphs: count(&page.notes),
            speakers: count(&page.speakers),
            transcript_turns: page.transcript.as_ref().map_or(0, Vec::len),
            bytes: built.json.len(),
            audio_parts: page.audio.as_ref().map_or(0, |audio| audio.parts),
            audio_bytes: page.audio.as_ref().map_or(0, |audio| audio.bytes),
        }
    }
}

type SpeakersAndTurns = (Vec<String>, Vec<Turn>);

/// Speakers in order of first appearance, and the turns pointing at them.
/// Numbered mode replaces every name, so no name reaches `speakers`.
fn turns(
    names: &[String],
    turns: &[Turn],
    mode: SpeakerMode,
) -> Result<Option<SpeakersAndTurns>, SharedPagesError> {
    let mut order: Vec<u32> = Vec::new();
    let mut kept = Vec::new();
    for turn in turns {
        let text = turn.text.trim();
        if text.is_empty() {
            continue;
        }
        bounded_text(text)?;
        if usize::try_from(turn.s).map_or(true, |index| index >= names.len()) {
            return Err(SharedPagesError::new(
                Code::InvalidDraft,
                "A transcript turn names a speaker the draft does not list.",
            ));
        }
        if !turn.t.is_finite() || turn.t < 0.0 {
            return Err(SharedPagesError::new(
                Code::InvalidDraft,
                "A transcript turn has no valid start time.",
            ));
        }
        let index = match order.iter().position(|speaker| *speaker == turn.s) {
            Some(index) => index,
            None => {
                order.push(turn.s);
                order.len() - 1
            }
        };
        kept.push(Turn {
            s: u32::try_from(index).unwrap_or(u32::MAX),
            t: (turn.t * 10.0).round() / 10.0,
            text: text.to_owned(),
        });
    }
    if kept.is_empty() {
        return Ok(None);
    }
    if kept.len() > MAX_TURNS {
        return Err(too_large("The transcript has more than 20,000 turns."));
    }
    let speakers = order
        .iter()
        .enumerate()
        .map(|(position, original)| match mode {
            SpeakerMode::Names => usize::try_from(*original)
                .ok()
                .and_then(|index| names.get(index))
                .map(|name| single_line(name))
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| UNKNOWN_SPEAKER.to_owned()),
            SpeakerMode::Numbered => format!("Speaker {}", position + 1),
        })
        .collect::<Vec<_>>();
    for name in &speakers {
        bounded_text(name)?;
    }
    Ok(Some((speakers, kept)))
}

fn texts(included: bool, values: &[String]) -> Result<Option<Vec<String>>, SharedPagesError> {
    if !included {
        return Ok(None);
    }
    let kept = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| bounded_text(value).map(|()| value.to_owned()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((!kept.is_empty()).then_some(kept))
}

/// A one-person page keeps only that person's items, and is refused when
/// none are left. `person` is the folded `for_person`; the owner is folded
/// the same way and compared without case, so "  jane   doe " and "Jane Doe"
/// are the same owner.
fn action_items(
    included: bool,
    items: &[ActionItem],
    person: Option<&str>,
) -> Result<Option<Vec<ActionItem>>, SharedPagesError> {
    let none_for = |person: &str| {
        SharedPagesError::new(
            Code::NoItemsForPerson,
            format!("A page for {person} needs at least one of their action items on it."),
        )
    };
    if !included {
        return person.map_or(Ok(None), |person| Err(none_for(person)));
    }
    let folded = person.map(str::to_lowercase);
    let mut kept = Vec::new();
    for item in items {
        let text = item.text.trim();
        if text.is_empty() {
            continue;
        }
        let owner = non_empty(item.owner.as_deref());
        if let Some(person) = &folded {
            let owns = owner
                .as_deref()
                .is_some_and(|owner| &single_line(owner).to_lowercase() == person);
            if !owns {
                continue;
            }
        }
        let due = non_empty(item.due.as_deref());
        for value in [Some(text), owner.as_deref(), due.as_deref()]
            .into_iter()
            .flatten()
        {
            bounded_text(value)?;
        }
        kept.push(ActionItem {
            text: text.to_owned(),
            owner,
            due,
        });
    }
    if kept.is_empty() {
        return person.map_or(Ok(None), |person| Err(none_for(person)));
    }
    Ok(Some(kept))
}

fn bounded_text(value: &str) -> Result<(), SharedPagesError> {
    if value.len() > MAX_TEXT_BYTES {
        return Err(too_large("One piece of text is larger than 64 KiB."));
    }
    Ok(())
}

fn too_large(message: &str) -> SharedPagesError {
    SharedPagesError::new(Code::TooLarge, message)
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn seconds(offset_ns: u64) -> f64 {
    u32::try_from(offset_ns / (NS_PER_SECOND / 10)).map_or(f64::MAX, f64::from) / 10.0
}
