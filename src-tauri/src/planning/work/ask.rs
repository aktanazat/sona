//! A question over every conversation, answered by the lines that answer
//! it: what was said in meetings, what they left open, and what was
//! dictated, each with where it was said. The question is read for the
//! words to look for, a person, and a stretch of time; the lines come from
//! the same transcript index and query plane the search box reads.

use super::capture::WorkCaptureSource;
use super::parse::{person_named, Lexicon};
use super::types::WorkPersonChoice;
use super::with_meetings;
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::{SaidWords, Within};
use crate::meeting::types::SaidMoment;
use crate::query::{self, QueryAudience, QueryRow, QueryScope};
use chrono::{DateTime, Datelike, Days, Local, Months, NaiveDate, TimeZone};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

const MOST_LINES: usize = 12;
const LINES_READ: usize = 40;
const PER_WORD: usize = 6;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkAnswer {
    pub question: String,
    /// What the question was read as, in the order it says them: the words
    /// looked for, the person, the stretch of time.
    pub read_as: Vec<String>,
    /// Best first.
    pub lines: Vec<WorkAnswerLine>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkAnswerLine {
    pub id: String,
    pub quote: String,
    /// The meeting or dictation it is from.
    pub title: String,
    pub speaker: Option<String>,
    pub person_id: Option<String>,
    pub when_utc_ms: i64,
    pub link: String,
    pub source: WorkCaptureSource,
    /// Where a meeting line plays from.
    pub play: Option<WorkAnswerPlay>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkAnswerPlay {
    pub session_id: String,
    pub segment_id: String,
    pub start_offset_ns: u64,
}

/// What a question asks about.
#[derive(Debug, PartialEq)]
pub(crate) struct Asked {
    pub words: Vec<String>,
    pub person: Option<WorkPersonChoice>,
    pub window: Option<Window>,
    /// It asks what was promised or left open, so those come first.
    pub open_first: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Window {
    pub said: &'static str,
    pub from_ms: i64,
    pub until_ms: i64,
}

impl Window {
    fn holds(self, at_ms: i64) -> bool {
        (self.from_ms..self.until_ms).contains(&at_ms)
    }
}

/// Words that ask rather than name what is asked about.
const ASKING: &[&str] = &[
    "a",
    "about",
    "after",
    "again",
    "all",
    "already",
    "am",
    "an",
    "and",
    "any",
    "anyone",
    "anything",
    "are",
    "around",
    "as",
    "ask",
    "asked",
    "at",
    "be",
    "been",
    "before",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "doing",
    "done",
    "else",
    "ever",
    "every",
    "everything",
    "for",
    "from",
    "get",
    "got",
    "had",
    "has",
    "have",
    "he",
    "her",
    "him",
    "his",
    "how",
    "i",
    "i'm",
    "if",
    "in",
    "into",
    "is",
    "it",
    "it's",
    "its",
    "just",
    "know",
    "let",
    "lets",
    "let's",
    "me",
    "mention",
    "mentioned",
    "mentions",
    "my",
    "of",
    "on",
    "or",
    "our",
    "ours",
    "say",
    "said",
    "says",
    "she",
    "should",
    "so",
    "some",
    "someone",
    "something",
    "talk",
    "talked",
    "talking",
    "tell",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "to",
    "told",
    "up",
    "us",
    "was",
    "we",
    "we've",
    "were",
    "what",
    "what's",
    "whats",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
    "anybody",
    "discuss",
    "discussed",
    "bring",
    "brought",
    "come",
    "came",
    "decide",
    "decided",
    "agree",
    "agreed",
];

/// Words that ask for what was promised or left open.
const OPEN: &[&str] = &[
    "promise",
    "promised",
    "promises",
    "commit",
    "committed",
    "commitment",
    "commitments",
    "owe",
    "owed",
    "owes",
    "follow",
    "followups",
    "follow-ups",
    "todo",
    "todos",
    "open",
    "pending",
];

/// The stretches of time a question can name, each as days back from the
/// start of today, or a calendar week or month.
const WINDOWS: &[(&[&str], &str)] = &[
    (&["this", "week"], "this week"),
    (&["last", "week"], "last week"),
    (&["past", "week"], "the past week"),
    (&["this", "month"], "this month"),
    (&["last", "month"], "last month"),
    (&["past", "month"], "the past month"),
    (&["today"], "today"),
    (&["yesterday"], "yesterday"),
    (&["recently"], "the past two weeks"),
    (&["lately"], "the past two weeks"),
];

fn key(word: &str) -> String {
    word.trim_matches(|character: char| !character.is_alphanumeric())
        .replace('\u{2019}', "'")
        .to_lowercase()
}

/// Reads `question` against the people the conversations know.
pub(crate) fn read_question<Tz: TimeZone>(
    question: &str,
    people: &[WorkPersonChoice],
    now: &DateTime<Tz>,
) -> Asked {
    let keys: Vec<String> = question
        .split_whitespace()
        .map(key)
        .filter(|key| !key.is_empty())
        .collect();
    let mut taken = vec![false; keys.len()];
    let today = now.naive_local().date();
    let mut window = None;
    for (index, _) in keys.iter().enumerate() {
        if window.is_some() {
            break;
        }
        for (phrase, said) in WINDOWS {
            let fits = phrase
                .iter()
                .enumerate()
                .all(|(offset, word)| keys.get(index + offset).is_some_and(|key| key == word));
            if fits {
                window = window_named(said, today);
                taken[index..index + phrase.len()].fill(true);
                break;
            }
        }
    }
    let mut person = None;
    for (index, word) in keys.iter().enumerate() {
        let name = word.split('\'').next().unwrap_or(word);
        if taken[index] || ASKING.contains(&name) {
            continue;
        }
        let two = keys
            .get(index + 1)
            .map(|next| format!("{name} {}", next.split('\'').next().unwrap_or(next)));
        if let Some(found) = two
            .as_deref()
            .and_then(|two| person_named(people, two, false))
        {
            person = Some(found.clone());
            taken[index] = true;
            taken[index + 1] = true;
            break;
        }
        if let Some(found) = person_named(people, name, false) {
            person = Some(found.clone());
            taken[index] = true;
            break;
        }
    }
    let open_first = keys.iter().any(|key| OPEN.contains(&key.as_str()));
    let mut seen = HashSet::new();
    let words = keys
        .iter()
        .zip(&taken)
        .filter(|(key, taken)| {
            !**taken
                && !ASKING.contains(&key.as_str())
                && !OPEN.contains(&key.as_str())
                && key.chars().any(char::is_alphanumeric)
        })
        .map(|(key, _)| key.split('\'').next().unwrap_or(key).to_owned())
        .filter(|word| word.chars().count() > 1 && seen.insert(word.clone()))
        .collect();
    Asked {
        words,
        person,
        window,
        open_first,
    }
}

fn window_named(said: &'static str, today: NaiveDate) -> Option<Window> {
    let monday =
        today.checked_sub_days(Days::new(u64::from(today.weekday().num_days_from_monday())))?;
    let first = today.with_day(1)?;
    let (from, until) = match said {
        "today" => (today, today.succ_opt()?),
        "yesterday" => (today.pred_opt()?, today),
        "this week" => (monday, today.succ_opt()?),
        "last week" => (monday.checked_sub_days(Days::new(7))?, monday),
        "the past week" => (today.checked_sub_days(Days::new(7))?, today.succ_opt()?),
        "this month" => (first, today.succ_opt()?),
        "last month" => (first.checked_sub_months(Months::new(1))?, first),
        "the past month" => (today.checked_sub_months(Months::new(1))?, today.succ_opt()?),
        _ => (today.checked_sub_days(Days::new(14))?, today.succ_opt()?),
    };
    let midnight = |day: NaiveDate| {
        Local
            .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
            .earliest()
            .map(|at| at.timestamp_millis())
    };
    Some(Window {
        said,
        from_ms: midnight(from)?,
        until_ms: midnight(until)?,
    })
}

/// The question answered by the lines that answer it.
#[tauri::command]
#[specta::specta]
pub async fn work_ask(app: AppHandle, question: String) -> Result<WorkAnswer, String> {
    answer(&app, QueryAudience::Local, &question).await
}

pub(crate) async fn answer(
    app: &AppHandle,
    audience: QueryAudience,
    question: &str,
) -> Result<WorkAnswer, String> {
    let history = app.state::<Arc<HistoryManager>>();
    let meetings = app.state::<Arc<MeetingSessionManager>>();
    let lexicon = with_meetings(&history, &meetings, |history, meetings| {
        Lexicon::read(history, meetings)
    })
    .await?;
    let asked = read_question(question, &lexicon.people, &Local::now());
    let mut read_as = asked.words.clone();
    if let Some(person) = &asked.person {
        read_as.push(person.name.clone());
    }
    if let Some(window) = asked.window {
        read_as.push(window.said.to_owned());
    }
    let mut looked_for = asked.words.clone();
    if looked_for.is_empty() {
        if let Some(person) = &asked.person {
            looked_for.extend(person.name.split_whitespace().next().map(str::to_lowercase));
        }
    }
    if looked_for.is_empty() {
        return Ok(WorkAnswer {
            question: question.to_owned(),
            read_as,
            lines: Vec::new(),
        });
    }

    let store = meetings
        .store()
        .await
        .map_err(|_| crate::planning::conversations_closed())?;
    let user_name = meetings.user_name();
    let hidden = audience
        .kept_local(&store)
        .map_err(|_| conversations_unread())?;
    let said = store
        .said_matching(
            &SaidWords::Any(&looked_for),
            Within::Everywhere,
            LINES_READ,
            user_name.as_deref(),
        )
        .map_err(|_| conversations_unread())?;
    let person_id = asked.person.as_ref().map(|person| person.id.as_str());
    let spoken: Vec<WorkAnswerLine> = said
        .into_iter()
        .filter(|moment| {
            !hidden
                .as_ref()
                .is_some_and(|hidden| hidden.hides(moment.session_id))
        })
        .filter(|moment| {
            person_id.is_none_or(|wanted| {
                moment
                    .person
                    .as_ref()
                    .is_some_and(|person| person.person_id.uuid().to_string() == wanted)
            }) || asked.words.is_empty()
        })
        .map(said_line)
        .collect();

    let mut open = Vec::new();
    let mut dictated = Vec::new();
    for word in &looked_for {
        for scope in [QueryScope::Loops, QueryScope::Dictations] {
            let page = query::search(
                &meetings,
                &history,
                audience,
                scope,
                word,
                Some(PER_WORD),
                None,
            )
            .await
            .map_err(|_| conversations_unread())?;
            for row in page.entries {
                match scope {
                    QueryScope::Loops => open.push(loop_line(row)),
                    _ => dictated.extend(dictation_line(&history, row, &looked_for)),
                }
            }
        }
    }
    let ordered = if asked.open_first {
        [open, spoken, dictated]
    } else {
        [spoken, open, dictated]
    };
    let mut seen = HashSet::new();
    let lines = ordered
        .into_iter()
        .flatten()
        .filter(|line| {
            asked
                .window
                .is_none_or(|window| window.holds(line.when_utc_ms))
        })
        .filter(|line| seen.insert(line.id.clone()) && seen.insert(line.quote.to_lowercase()))
        .take(MOST_LINES)
        .collect();
    Ok(WorkAnswer {
        question: question.to_owned(),
        read_as,
        lines,
    })
}

fn conversations_unread() -> String {
    "Conversations could not be read.".to_owned()
}

pub(super) fn said_line(moment: SaidMoment) -> WorkAnswerLine {
    let at_ms = i64::try_from(moment.start_offset_ns / 1_000_000).unwrap_or(i64::MAX);
    let session_id = moment.session_id.uuid().to_string();
    WorkAnswerLine {
        id: format!("said:{}", moment.segment_id.uuid()),
        quote: moment.text.clone(),
        title: moment.meeting_title,
        speaker: moment
            .person
            .as_ref()
            .map(|person| person.display_name.clone())
            .or(moment.speaker_label),
        person_id: moment
            .person
            .map(|person| person.person_id.uuid().to_string()),
        when_utc_ms: moment.meeting_started_at_utc_ms.saturating_add(at_ms),
        link: query::meeting_link(moment.session_id),
        source: WorkCaptureSource::Meeting {
            session_id: session_id.clone(),
            at_ms,
            quote: moment.text,
        },
        play: moment.audio_retained.then(|| WorkAnswerPlay {
            session_id,
            segment_id: moment.segment_id.uuid().to_string(),
            start_offset_ns: moment.start_offset_ns,
        }),
    }
}

fn loop_line(row: QueryRow) -> WorkAnswerLine {
    WorkAnswerLine {
        id: format!("loop:{}", row.id),
        quote: row.title,
        title: row.snippet,
        speaker: None,
        person_id: None,
        when_utc_ms: row.when_utc_ms,
        link: row.link,
        source: WorkCaptureSource::Loop { loop_id: row.id },
        play: None,
    }
}

/// A dictation as the sentence of it that holds a word looked for, kept
/// with the whole recording.
fn dictation_line(
    history: &HistoryManager,
    row: QueryRow,
    words: &[String],
) -> Option<WorkAnswerLine> {
    let history_id: i64 = row.id.parse().ok()?;
    let duration_ms = history
        .with_connection(|conn| {
            Ok(conn
                .query_row(
                    "SELECT duration_ms FROM transcription_history WHERE id = ?1",
                    [history_id],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .optional()?
                .flatten())
        })
        .ok()
        .flatten()
        .unwrap_or(0);
    let quote = sentence_with(&row.snippet, words).to_owned();
    Some(WorkAnswerLine {
        id: format!("dictation:{history_id}"),
        title: row.title,
        speaker: None,
        person_id: None,
        when_utc_ms: row.when_utc_ms,
        link: row.link,
        source: WorkCaptureSource::Dictation {
            history_id,
            start_ms: 0,
            end_ms: duration_ms.max(0),
            quote: quote.clone(),
        },
        quote,
        play: None,
    })
}

/// The first sentence of `text` that holds one of `words`, else the text.
fn sentence_with<'t>(text: &'t str, words: &[String]) -> &'t str {
    text.split_inclusive(['.', '!', '?', '\n'])
        .map(str::trim)
        .find(|sentence| {
            let lower = sentence.to_lowercase();
            words.iter().any(|word| lower.contains(word.as_str()))
        })
        .unwrap_or(text.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wednesday 11 November 2026, ten in the morning, here.
    fn wednesday() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 11, 11, 10, 0, 0)
            .earliest()
            .expect("the test's clock")
    }

    fn people() -> Vec<WorkPersonChoice> {
        ["Dana Reyes", "Priya Shah"]
            .iter()
            .map(|name| WorkPersonChoice {
                id: name.to_lowercase(),
                name: (*name).to_owned(),
            })
            .collect()
    }

    #[test]
    fn a_question_reads_as_its_topic_person_and_stretch_of_time() {
        let asked = read_question(
            "What did Dana say about pricing last week?",
            &people(),
            &wednesday(),
        );

        assert_eq!(asked.words, ["pricing"]);
        assert_eq!(
            asked.person.map(|person| person.id).as_deref(),
            Some("dana reyes")
        );
        assert_eq!(asked.window.map(|window| window.said), Some("last week"));
        assert!(!asked.open_first);
    }

    #[test]
    fn a_question_about_promises_reads_open_work_first() {
        let asked = read_question("what did I promise Priya?", &people(), &wednesday());

        assert!(asked.open_first);
        assert!(asked.words.is_empty(), "words: {:?}", asked.words);
        assert_eq!(
            asked.person.map(|person| person.id).as_deref(),
            Some("priya shah")
        );
    }
}
