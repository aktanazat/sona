//! A line of words read as a task: the title left once the words that name
//! its due date, priority, labels, owner, project and repeat are taken out,
//! and those words with what they name. Typed lines and speech go through
//! the same reading; speech has no sigils, says names whole, and says who
//! owes the work by who it starts with.

use super::sync::People;
use super::types::{WorkLabel, WorkOwner, WorkPersonChoice, WorkProjectChoice};
use super::{items, project_choices, with_meetings};
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::spoken_time::{self, next_weekday, DayNames, SpokenTime};
use anyhow::Result as StorageResult;
use chrono::{DateTime, Datelike, Days, Months, NaiveDate, NaiveTime, TimeZone, Weekday};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::ops::Range;
use std::sync::Arc;
use tauri::State;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkParse {
    /// The text with the parsed words taken out; empty when nothing is left.
    pub title: String,
    /// In text order, never overlapping.
    pub tokens: Vec<WorkToken>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkToken {
    /// The words as typed or said.
    pub text: String,
    /// Where the words sit in the text, in UTF-16 code units, end exclusive.
    pub start: u32,
    pub end: u32,
    pub field: WorkField,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkField {
    /// Epoch ms. A day said with no time is due at 17:00 local.
    Due {
        at: i64,
        timed: bool,
    },
    /// 1 urgent, 2 high, 3 medium, 4 low.
    Priority {
        level: u8,
    },
    /// No id: no label has this name yet.
    Label {
        id: Option<String>,
        name: String,
    },
    /// A spoken subject names the owner and stays in the title.
    Owner {
        owner: WorkOwner,
        name: String,
    },
    Project {
        id: String,
        title: String,
    },
    /// Kept as words; nothing makes the next one. With no due date said,
    /// `next_at` is the next time it falls due, epoch ms, when the words
    /// name its days.
    #[serde(rename_all = "camelCase")]
    Repeat {
        every: String,
        next_at: Option<i64>,
    },
}

/// Whether a line was typed or said.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Said {
    Typed,
    Spoken,
}

/// The labels, projects and people a line can name.
#[derive(Default)]
pub(crate) struct Lexicon {
    pub labels: Vec<WorkLabel>,
    pub projects: Vec<WorkProjectChoice>,
    pub people: Vec<WorkPersonChoice>,
}

impl Lexicon {
    /// The tracker's labels and projects, and the people in the
    /// conversations when they are open.
    pub(crate) fn read(history: &Connection, meetings: Option<&Connection>) -> StorageResult<Self> {
        Ok(Self {
            labels: items::labels(history)?,
            projects: project_choices(history)?,
            people: meetings
                .map(People::read)
                .transpose()?
                .map(|people| people.choices)
                .unwrap_or_default(),
        })
    }
}

/// Reads `text` as a task. Words matching one of `plain`, case aside, were
/// turned back into plain words and are read as nothing.
#[tauri::command]
#[specta::specta]
pub async fn work_parse(
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    text: String,
    plain: Vec<String>,
    spoken: bool,
) -> Result<WorkParse, String> {
    let now = chrono::Local::now();
    let said = if spoken { Said::Spoken } else { Said::Typed };
    with_meetings(&history_state, &meetings, |history, meeting_conn| {
        let lexicon = Lexicon::read(history, meeting_conn)?;
        Ok(parse(&text, said, &plain, &lexicon, &now))
    })
    .await
}

pub(crate) fn parse<Tz: TimeZone>(
    text: &str,
    said: Said,
    plain: &[String],
    lexicon: &Lexicon,
    now: &DateTime<Tz>,
) -> WorkParse {
    let mut line = Line::new(text, plain);
    line.read(said, lexicon, now);
    line.finish(said)
}

/// `words` read as one repeat and nothing else: how often, as the parser
/// says it, and when it next falls, when the words name the days.
pub(crate) fn repeat<Tz: TimeZone>(
    words: &str,
    now: &DateTime<Tz>,
) -> Option<(String, Option<i64>)> {
    let parse = parse(words, Said::Typed, &[], &Lexicon::default(), now);
    match parse.tokens.as_slice() {
        [WorkToken {
            field: WorkField::Repeat { every, next_at },
            ..
        }] if parse.title.is_empty() => Some((every.clone(), *next_at)),
        _ => None,
    }
}

/// One task a spoken passage says: the bytes of the passage that say it,
/// and what they say.
#[derive(Debug, PartialEq)]
pub(crate) struct SpokenTask {
    pub said: Range<usize>,
    pub title: String,
    pub fields: Vec<WorkField>,
    /// It starts with someone who is not in the people list, "Dana's
    /// sending…", so it is not the speaker's to do.
    pub someone_else: bool,
}

/// The tasks a spoken passage says, one per sentence, or more where a
/// sentence goes on with "and" into a task of its own. A sentence that only
/// adds a date or a priority adds it to the task before it; one that says no
/// task is dropped.
pub(crate) fn spoken_tasks<Tz: TimeZone>(
    passage: &str,
    lexicon: &Lexicon,
    now: &DateTime<Tz>,
) -> Vec<SpokenTask> {
    let mut tasks: Vec<SpokenTask> = Vec::new();
    let mut owner_before = None;
    for (said, goes_on) in clauses(passage, lexicon) {
        let mut line = Line::new(&passage[said.clone()], &[]);
        line.lead_in();
        line.read(Said::Spoken, lexicon, now);
        let someone_else = line.someone_else();
        let parse = line.finish(Said::Spoken);
        let mut fields: Vec<WorkField> =
            parse.tokens.into_iter().map(|token| token.field).collect();
        if parse.title.is_empty() || only_filler(&parse.title) {
            if let Some(task) = tasks.last_mut().filter(|_| !fields.is_empty()) {
                for field in fields {
                    task.fields.retain(|kept| !same_kind(kept, &field));
                    task.fields.push(field);
                }
                task.said.end = said.end;
            }
            continue;
        }
        match fields
            .iter()
            .find(|field| matches!(field, WorkField::Owner { .. }))
            .cloned()
        {
            Some(owner) => owner_before = Some(owner),
            None if goes_on && !someone_else => fields.extend(owner_before.clone()),
            None => owner_before = None,
        }
        tasks.push(SpokenTask {
            said,
            title: parse.title,
            fields,
            someone_else,
        });
    }
    tasks
}

fn same_kind(one: &WorkField, other: &WorkField) -> bool {
    std::mem::discriminant(one) == std::mem::discriminant(other)
        && !matches!(one, WorkField::Label { .. })
}

/// Words that open a spoken passage or fill it, and say no task.
const FILLER: &[&str] = &[
    "a", "alright", "all", "also", "and", "are", "couple", "do", "few", "for", "here", "here's",
    "hmm", "i", "is", "just", "let", "let's", "list", "me", "my", "need", "of", "oh", "ok", "okay",
    "quick", "right", "see", "so", "some", "stuff", "task", "tasks", "the", "then", "thing",
    "things", "to", "to-do", "today", "todo", "uh", "um", "well", "what", "yeah", "yes",
];

fn only_filler(title: &str) -> bool {
    split_words(title)
        .iter()
        .all(|word| FILLER.contains(&word.key.as_str()))
}

/// Where a spoken passage turns to a new task: at the end of a sentence,
/// and inside one at "and", "then", "also" or "plus", or a comma, when what
/// follows starts a task of its own. Beside each part, whether it goes on
/// with the doer of the part before it: one inside a sentence that opens
/// with a verb does.
fn clauses(passage: &str, lexicon: &Lexicon) -> Vec<(Range<usize>, bool)> {
    let words = split_words(passage);
    let mut clauses = Vec::new();
    let mut start = 0;
    let mut goes_on = false;
    let mut index = 0;
    while index < words.len() {
        let word = &words[index];
        let next = words.get(index + 1);
        let ends_sentence = next.is_none_or(|next| {
            passage[word.range.end..next.range.start].contains('\n')
                || sentence_ends(passage, word, next)
        });
        if ends_sentence || passage[word.range.clone()].ends_with(';') {
            clauses.push((words[start].range.start..word.range.end, goes_on));
            start = index + 1;
            goes_on = !ends_sentence;
            index += 1;
            continue;
        }
        let comma = passage[word.range.clone()].ends_with(',');
        let joiners = joiner_length(&words, index + 1);
        let opens = if joiners > 0 || comma {
            opening(passage, &words, index + 1 + joiners, lexicon)
        } else {
            None
        };
        if let Some(opens) = opens {
            let end = word.range.end - usize::from(comma);
            clauses.push((words[start].range.start..end, goes_on));
            start = index + 1 + joiners;
            goes_on = opens == Opening::Verb;
            index = start;
            continue;
        }
        index += 1;
    }
    clauses.retain(|(range, _)| !range.is_empty());
    clauses
}

/// How many words at `index` only join two tasks: "and", "and then",
/// "then", "also", "plus".
fn joiner_length(words: &[Word], index: usize) -> usize {
    let key = |at: usize| words.get(at).map(|word| word.key.as_str());
    match (key(index), key(index + 1)) {
        (Some("and"), Some("then" | "also")) => 2,
        (Some("and" | "then" | "also" | "plus"), _) => 1,
        _ => 0,
    }
}

/// Whether a '.', '!' or '?' ends the sentence at `word`. A '.' inside a
/// word, "p.m." or "U.S.", ends one only when a capital follows; a title
/// such as "Dr." never does.
fn sentence_ends(passage: &str, word: &Word, next: &Word) -> bool {
    let text = passage[word.range.clone()].trim_end_matches(['"', '\'', ')', '\u{201d}']);
    if !text.ends_with(['.', '!', '?']) {
        return false;
    }
    if matches!(word.key.as_str(), "mr" | "mrs" | "ms" | "dr" | "st" | "vs") {
        return false;
    }
    !word.key.contains('.')
        || passage[next.core.clone()]
            .chars()
            .next()
            .is_some_and(char::is_uppercase)
}

/// Verbs a spoken task starts with when nobody is named.
const TASK_VERBS: &[&str] = &[
    "add", "ask", "book", "buy", "call", "cancel", "check", "clean", "confirm", "draft", "email",
    "file", "finish", "fix", "follow", "get", "invite", "look", "make", "merge", "message", "move",
    "order", "pay", "ping", "plan", "post", "prepare", "print", "publish", "read", "remind",
    "renew", "reply", "review", "schedule", "send", "share", "ship", "sign", "submit", "take",
    "tell", "test", "text", "update", "write",
];

/// How a part inside a sentence opens when it starts a task of its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Opening {
    /// "call the bank": whoever does the part before does this one.
    Verb,
    /// "I need to", "Dana's sending", "Sam will": it names who does it.
    Doer,
    /// "high priority on the pricing page".
    Priority,
}

fn opening(passage: &str, words: &[Word], index: usize, lexicon: &Lexicon) -> Option<Opening> {
    let word = words.get(index)?;
    let key = word.key.as_str();
    if TASK_VERBS.contains(&key) {
        return Some(Opening::Verb);
    }
    let reads = |phrase: &[&str]| {
        phrase.iter().enumerate().all(|(offset, part)| {
            words
                .get(index + offset)
                .is_some_and(|word| word.key == *part)
        })
    };
    let doer = pronoun(key)
        || name_like(passage, word)
        || person_named(&lexicon.people, base_name(key), false).is_some();
    if SELF_PHRASES.iter().any(|phrase| reads(phrase)) || doer && acts(words, index) {
        return Some(Opening::Doer);
    }
    PRIORITY_PHRASES
        .iter()
        .find(|(phrase, _)| reads(phrase))
        .filter(|(phrase, _)| {
            words
                .get(index + phrase.len())
                .is_some_and(|next| AFTER_LEAD.contains(&next.key.as_str()))
                && words.get(index + phrase.len() + 1).is_some()
        })
        .map(|_| Opening::Priority)
}

/// "he", "she" or "they", with a word joined to it or not.
fn pronoun(key: &str) -> bool {
    matches!(base_name(key), "he" | "she" | "they")
}

/// Whether `word` reads as someone's name: a capital, letters only, and
/// not a word a sentence opens with, or a day.
fn name_like(text: &str, word: &Word) -> bool {
    let name = base_name(&word.key);
    text[word.core.clone()]
        .chars()
        .next()
        .is_some_and(char::is_uppercase)
        && name.len() > 1
        && name.chars().all(char::is_alphabetic)
        && !NOT_NAMES.contains(&name)
        && weekday_named(name).is_none()
}

/// Whether the word at `index` has a verb joined to it, "Dana's" or
/// "she'll", or one after it that says it does the work.
fn acts(words: &[Word], index: usize) -> bool {
    words
        .get(index)
        .is_some_and(|word| word.key.len() > base_name(&word.key).len())
        || words
            .get(index + 1)
            .is_some_and(|next| THIRD_PARTY_VERBS.contains(&next.key.as_str()))
}

/// What starts a sentence the speaker owes, longest first.
const SELF_PHRASES: &[&[&str]] = &[
    &["i", "am", "going", "to"],
    &["i've", "got", "to"],
    &["i", "need", "to"],
    &["i", "have", "to"],
    &["i", "got", "to"],
    &["i", "want", "to"],
    &["i'm", "going", "to"],
    &["i'd", "like", "to"],
    &["we", "need", "to"],
    &["we", "have", "to"],
    &["remind", "me", "to"],
    &["don't", "forget", "to"],
    &["i", "gotta"],
    &["i", "should"],
    &["i", "must"],
    &["i", "will"],
    &["i'm", "gonna"],
    &["we", "should"],
    &["we", "will"],
    &["let", "me"],
    &["i'll"],
    &["i'm"],
    &["we'll"],
    &["let's"],
    &["i"],
];

/// Words that open a spoken part and say nothing: "Also, …", "And then …".
const LEAD_INS: &[&str] = &[
    "also", "alright", "and", "oh", "ok", "okay", "plus", "so", "then", "uh", "um", "well",
];

/// Words that open a part and say nothing when a comma follows them.
const ORDINAL_LEAD_INS: &[&str] = &[
    "first", "firstly", "second", "secondly", "third", "thirdly", "next", "finally", "lastly",
];

pub(super) const PRIORITY_PHRASES: &[(&[&str], u8)] = &[
    (&["top", "priority"], 1),
    (&["highest", "priority"], 1),
    (&["urgent", "priority"], 1),
    (&["priority", "one"], 1),
    (&["priority", "1"], 1),
    (&["high", "priority"], 2),
    (&["priority", "high"], 2),
    (&["priority", "two"], 2),
    (&["priority", "2"], 2),
    (&["medium", "priority"], 3),
    (&["normal", "priority"], 3),
    (&["priority", "medium"], 3),
    (&["priority", "three"], 3),
    (&["priority", "3"], 3),
    (&["low", "priority"], 4),
    (&["lowest", "priority"], 4),
    (&["priority", "low"], 4),
    (&["priority", "four"], 4),
    (&["priority", "4"], 4),
    (&["no", "rush"], 4),
];

/// Words that join a field to the title and go with it.
const BEFORE_DUE: &[&str] = &[
    "around", "at", "before", "by", "due", "for", "from", "next", "on", "starting", "this", "till",
    "until",
];
const BEFORE_PRIORITY: &[&str] = &["as", "at", "is", "it's", "its", "with"];
const BEFORE_PROJECT: &[&str] = &["for", "in", "into", "on", "to", "under"];
const BEFORE_LABEL: &[&str] = &["as", "tagged", "with"];
const ARTICLES: &[&str] = &["a", "an", "the"];

/// Words left hanging at the start of a spoken title once what they led
/// into was taken: "on" in "High priority on the pricing page".
const AFTER_LEAD: &[&str] = &["about", "for", "on", "re", "regarding", "to", "with"];

/// Words left hanging at the end of a title once what followed was taken.
const TRAILING: &[&str] = &[
    "a", "also", "an", "and", "as", "at", "but", "by", "for", "from", "is", "it's", "of", "on",
    "or", "so", "the", "to", "with",
];

/// Words that start a sentence without naming anyone.
const NOT_NAMES: &[&str] = &[
    "a",
    "also",
    "an",
    "and",
    "can",
    "could",
    "everybody",
    "everyone",
    "here",
    "how",
    "i",
    "if",
    "it",
    "just",
    "last",
    "let",
    "maybe",
    "my",
    "next",
    "nobody",
    "our",
    "please",
    "project",
    "should",
    "so",
    "somebody",
    "someone",
    "that",
    "the",
    "their",
    "then",
    "there",
    "this",
    "today",
    "tomorrow",
    "tonight",
    "we",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "will",
    "would",
    "you",
    "your",
];

/// Words after a name that make the name the one doing the work.
const THIRD_PARTY_VERBS: &[&str] = &[
    "agreed", "can", "could", "has", "is", "must", "needs", "owes", "promised", "said", "sending",
    "sends", "sent", "should", "wants", "was", "will", "would",
];

/// One run of text between spaces: `range` all of it, `core` without the
/// punctuation around it, `key` the core lowercased, apostrophes straight.
#[derive(Debug)]
pub(super) struct Word {
    range: Range<usize>,
    pub(super) core: Range<usize>,
    pub(super) key: String,
}

const OPENING: &[char] = &['"', '\'', '(', '[', '{', '\u{201c}', '\u{2018}'];
const CLOSING: &[char] = &[
    '.', ',', ';', ':', '!', '?', '"', '\'', ')', ']', '}', '\u{201d}', '\u{2019}', '\u{2026}',
];

pub(super) fn split_words(text: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut start = None;
    for (at, character) in text.char_indices().chain([(text.len(), ' ')]) {
        match (character.is_whitespace(), start) {
            (false, None) => start = Some(at),
            (true, Some(from)) => {
                words.push(word(text, from..at));
                start = None;
            }
            _ => {}
        }
    }
    words
}

fn word(text: &str, range: Range<usize>) -> Word {
    let slice = &text[range.clone()];
    let leading = slice.len() - slice.trim_start_matches(OPENING).len();
    let trailing = slice.trim_end_matches(CLOSING).len();
    let core = if leading < trailing {
        range.start + leading..range.start + trailing
    } else {
        range.clone()
    };
    let key = text[core.clone()].to_lowercase().replace('\u{2019}', "'");
    Word { range, core, key }
}

/// A name as letters and digits only, lowercased: "Needs review" and
/// "#needs-review" are the same label.
pub(super) fn key(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A spoken name without what is joined to it: "dana" in "dana's".
pub(super) fn base_name(key: &str) -> &str {
    key.split_once('\'').map_or(key, |(name, _)| name)
}

fn only<T>(mut matches: impl Iterator<Item = T>) -> Option<T> {
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// The one person `said` names: by whole name, by first name, or, when
/// `prefix`, by the start of a name.
pub(crate) fn person_named<'p>(
    people: &'p [WorkPersonChoice],
    said: &str,
    prefix: bool,
) -> Option<&'p WorkPersonChoice> {
    let wanted = key(said);
    if wanted.is_empty() {
        return None;
    }
    let first_name = |person: &&WorkPersonChoice| {
        person
            .name
            .split_whitespace()
            .next()
            .is_some_and(|first| key(first) == wanted)
    };
    let called = |person: &&WorkPersonChoice| {
        key(&person.name) == wanted || person.aliases.iter().any(|alias| key(alias) == wanted)
    };
    only(people.iter().filter(called))
        .or_else(|| only(people.iter().filter(first_name)))
        .or_else(|| {
            (prefix && wanted.len() >= 2)
                .then(|| {
                    only(
                        people
                            .iter()
                            .filter(|person| key(&person.name).starts_with(&wanted)),
                    )
                })
                .flatten()
        })
}

fn me() -> WorkField {
    WorkField::Owner {
        owner: WorkOwner::Myself,
        name: "Me".to_owned(),
    }
}

fn person_field(person: &WorkPersonChoice) -> WorkField {
    WorkField::Owner {
        owner: WorkOwner::Person {
            person_id: person.id.clone(),
        },
        name: person.name.clone(),
    }
}

fn priority_number(level: &str) -> Option<WorkField> {
    let level = match level {
        "1" => 1,
        "2" => 2,
        "3" => 3,
        "4" => 4,
        _ => return None,
    };
    Some(WorkField::Priority { level })
}

/// The label `name` names, or, when `create`, one to make with that name.
fn label_named(lexicon: &Lexicon, name: &str, create: bool) -> Option<WorkField> {
    let wanted = key(name);
    if !wanted.chars().any(char::is_alphabetic) {
        return None;
    }
    match lexicon
        .labels
        .iter()
        .find(|label| key(&label.name) == wanted)
    {
        Some(label) if label.retired_at_ms.is_some() => None,
        Some(label) => Some(WorkField::Label {
            id: Some(label.id.clone()),
            name: label.name.clone(),
        }),
        None => create.then(|| WorkField::Label {
            id: None,
            name: name
                .trim_matches(|character: char| !character.is_alphanumeric())
                .to_owned(),
        }),
    }
}

/// The project `name` names, whole or by the one title it starts.
pub(super) fn project_named(lexicon: &Lexicon, name: &str) -> Option<WorkField> {
    let wanted = key(name);
    if wanted.len() < 2 || !wanted.chars().any(char::is_alphabetic) {
        return None;
    }
    let field = |project: &WorkProjectChoice| WorkField::Project {
        id: project.id.clone(),
        title: project.title.clone(),
    };
    lexicon
        .projects
        .iter()
        .find(|project| key(&project.title) == wanted)
        .or_else(|| {
            only(
                lexicon
                    .projects
                    .iter()
                    .filter(|project| key(&project.title).starts_with(&wanted)),
            )
        })
        .map(field)
}

pub(super) fn weekday_named(key: &str) -> Option<Weekday> {
    let singular = key
        .strip_suffix('s')
        .filter(|name| name.ends_with("day"))
        .unwrap_or(key);
    Some(match singular {
        "monday" | "mon" => Weekday::Mon,
        "tuesday" | "tue" | "tues" => Weekday::Tue,
        "wednesday" | "wed" => Weekday::Wed,
        "thursday" | "thu" | "thurs" => Weekday::Thu,
        "friday" | "fri" => Weekday::Fri,
        "saturday" | "sat" => Weekday::Sat,
        "sunday" | "sun" => Weekday::Sun,
        _ => return None,
    })
}

fn weekday_word(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "monday",
        Weekday::Tue => "tuesday",
        Weekday::Wed => "wednesday",
        Weekday::Thu => "thursday",
        Weekday::Fri => "friday",
        Weekday::Sat => "saturday",
        Weekday::Sun => "sunday",
    }
}

fn count(key: &str) -> Option<u32> {
    Some(match key {
        "a" | "an" | "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        digits => digits
            .parse()
            .ok()
            .filter(|count| (1..=365).contains(count))?,
    })
}

fn unit(key: &str) -> Option<&'static str> {
    Some(match key {
        "day" | "days" => "day",
        "week" | "weeks" => "week",
        "month" | "months" => "month",
        "quarter" | "quarters" => "quarter",
        "year" | "years" => "year",
        _ => return None,
    })
}

/// The days a repeat falls on, and the hour its part of the day names.
struct Falls {
    days: Vec<Weekday>,
    hour: Option<u32>,
}

impl Falls {
    fn daily(hour: Option<u32>) -> Self {
        Self {
            days: EVERY_DAY.to_vec(),
            hour,
        }
    }

    /// The first day it falls on, `from` or after.
    fn next(&self, from: NaiveDate) -> Option<NaiveDate> {
        self.days
            .iter()
            .filter_map(|weekday| on_or_after(from, *weekday))
            .min()
    }
}

const EVERY_DAY: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];

const WEEKDAYS: [Weekday; 5] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
];

/// The usual hour of a part of the day.
fn part_hour(key: &str) -> Option<u32> {
    Some(match key {
        "morning" => 9,
        "afternoon" => 14,
        "evening" => 18,
        "night" | "tonight" => 20,
        _ => return None,
    })
}

/// The day of the month an ordinal names: "1st", "22nd", "3rd", "15th".
fn day_of_month(key: &str) -> Option<u32> {
    let digits = ["st", "nd", "rd", "th"]
        .iter()
        .find_map(|suffix| key.strip_suffix(suffix))?;
    digits.parse().ok().filter(|day| (1..=31).contains(day))
}

/// The first date, `today` or after, that is the `day` of its month.
fn next_day_of_month(today: NaiveDate, day: u32) -> Option<NaiveDate> {
    let mut month = today.with_day(1)?;
    for _ in 0..12 {
        if let Some(date) = month.with_day(day).filter(|date| *date >= today) {
            return Some(date);
        }
        month = month.checked_add_months(Months::new(1))?;
    }
    None
}

/// `weekday` today or the next time it comes.
fn on_or_after(today: NaiveDate, weekday: Weekday) -> Option<NaiveDate> {
    if today.weekday() == weekday {
        Some(today)
    } else {
        next_weekday(today, weekday)
    }
}

fn end_of_week(today: NaiveDate) -> Option<NaiveDate> {
    on_or_after(today, Weekday::Fri)
}

fn first_of_next_month(today: NaiveDate) -> Option<NaiveDate> {
    today.with_day(1)?.checked_add_months(Months::new(1))
}

fn after(today: NaiveDate, count: u32, unit: &str) -> Option<NaiveDate> {
    match unit {
        "day" => today.checked_add_days(Days::new(u64::from(count))),
        "week" => today.checked_add_days(Days::new(u64::from(count) * 7)),
        "month" => today.checked_add_months(Months::new(count)),
        "quarter" => today.checked_add_months(Months::new(count * 3)),
        "year" => today.checked_add_months(Months::new(count * 12)),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Use {
    Free,
    /// Turned back into plain words: read as nothing.
    Plain,
    /// One of the words of `Line::found[index]`.
    Field(usize),
    /// A word that only joined a field to the title: "by" in "by Friday".
    Gone,
}

struct Found {
    words: Range<usize>,
    field: WorkField,
    kept: bool,
}

struct Line<'a> {
    text: &'a str,
    words: Vec<Word>,
    uses: Vec<Use>,
    found: Vec<Found>,
}

impl<'a> Line<'a> {
    fn new(text: &'a str, plain: &[String]) -> Self {
        let words = split_words(text);
        let mut uses = vec![Use::Free; words.len()];
        for phrase in plain {
            let keys = split_words(phrase);
            if keys.is_empty() {
                continue;
            }
            for start in 0..words.len().saturating_sub(keys.len() - 1) {
                if keys
                    .iter()
                    .enumerate()
                    .all(|(offset, part)| words[start + offset].key == part.key)
                {
                    uses[start..start + keys.len()].fill(Use::Plain);
                }
            }
        }
        Self {
            text,
            words,
            uses,
            found: Vec::new(),
        }
    }

    fn key(&self, index: usize) -> Option<&str> {
        (self.uses.get(index) == Some(&Use::Free)).then(|| self.words[index].key.as_str())
    }

    /// Whether the free words at `start` read `keys`.
    fn reads(&self, start: usize, keys: &[&str]) -> bool {
        keys.iter()
            .enumerate()
            .all(|(offset, key)| self.key(start + offset) == Some(*key))
    }

    fn take(&mut self, words: Range<usize>, field: WorkField) {
        self.uses[words.clone()].fill(Use::Field(self.found.len()));
        self.found.push(Found {
            words,
            field,
            kept: false,
        });
    }

    fn keep(&mut self, words: Range<usize>, field: WorkField) {
        self.take(words, field);
        if let Some(found) = self.found.last_mut() {
            found.kept = true;
        }
    }

    fn first_free(&self) -> Option<usize> {
        self.uses.iter().position(|used| *used == Use::Free)
    }

    fn read<Tz: TimeZone>(&mut self, said: Said, lexicon: &Lexicon, now: &DateTime<Tz>) {
        if said == Said::Typed {
            self.sigils(lexicon);
        }
        self.priority();
        let falls = self.repeat();
        if said == Said::Spoken {
            self.spoken_label(lexicon);
            self.spoken_project(lexicon);
            self.subject(lexicon);
        }
        self.due(said, now, falls);
        self.connectors(said);
    }

    /// `!2`, `p2`, `#label`, `+project`, `@person` and `@me`. A `#` with no
    /// letter after it, "#1234", is left as words.
    fn sigils(&mut self, lexicon: &Lexicon) {
        let text = self.text;
        for index in 0..self.words.len() {
            let Some(key) = self.key(index) else {
                continue;
            };
            let core = &text[self.words[index].core.clone()];
            let found = if let Some(level) = key.strip_prefix('!') {
                priority_number(level).map(|field| (index + 1, field))
            } else if let Some(name) = core.strip_prefix('#') {
                label_named(lexicon, name, true).map(|field| (index + 1, field))
            } else if let Some(name) = key.strip_prefix('+') {
                self.project_sigil(lexicon, index, name)
            } else if let Some(name) = key.strip_prefix('@') {
                let field = match name {
                    "me" | "myself" => Some(me()),
                    name => person_named(&lexicon.people, name, true).map(person_field),
                };
                field.map(|field| (index + 1, field))
            } else {
                key.strip_prefix('p')
                    .and_then(priority_number)
                    .map(|field| (index + 1, field))
            };
            if let Some((end, field)) = found {
                self.take(index..end, field);
            }
        }
    }

    /// The project a `+` names: the most words from `index` that make one
    /// project's whole title, "+Q4 pricing", else the one title the first
    /// word names or starts.
    fn project_sigil(
        &self,
        lexicon: &Lexicon,
        index: usize,
        name: &str,
    ) -> Option<(usize, WorkField)> {
        let mut said = key(name);
        let mut longest = None;
        let mut end = index + 1;
        while let Some(next) = self.key(end) {
            said.push_str(&key(next));
            let mut titles = lexicon
                .projects
                .iter()
                .filter(|project| key(&project.title).starts_with(&said))
                .peekable();
            if titles.peek().is_none() {
                break;
            }
            end += 1;
            if let Some(project) = titles.find(|project| key(&project.title) == said) {
                longest = Some((
                    end,
                    WorkField::Project {
                        id: project.id.clone(),
                        title: project.title.clone(),
                    },
                ));
            }
        }
        longest.or_else(|| project_named(lexicon, name).map(|field| (index + 1, field)))
    }

    /// The first priority said: "high priority", "priority 2", "no rush", or
    /// "urgent" and "asap" as the first or last word.
    fn priority(&mut self) {
        if self
            .found
            .iter()
            .any(|found| matches!(found.field, WorkField::Priority { .. }))
        {
            return;
        }
        let last = self.words.len().saturating_sub(1);
        for index in 0..self.words.len() {
            let said = PRIORITY_PHRASES
                .iter()
                .find(|(phrase, _)| self.reads(index, phrase))
                .map(|(phrase, level)| (phrase.len(), *level))
                .or_else(|| {
                    ((index == 0 || index == last)
                        && matches!(self.key(index), Some("urgent" | "asap")))
                    .then_some((1, 1))
                });
            if let Some((length, level)) = said {
                self.take(index..index + length, WorkField::Priority { level });
                return;
            }
        }
    }

    /// "every monday", "every other week", "every 2 weeks", "daily", "on
    /// mondays". The days it falls on, when it names them.
    fn repeat(&mut self) -> Option<Falls> {
        for index in 0..self.words.len() {
            if let Some((words, every, falls)) = self.repeat_at(index) {
                self.take(
                    words,
                    WorkField::Repeat {
                        every,
                        next_at: None,
                    },
                );
                return falls;
            }
        }
        None
    }

    fn repeat_at(&self, index: usize) -> Option<(Range<usize>, String, Option<Falls>)> {
        let key = self.key(index)?;
        let one = index..index + 1;
        let every = |what: &str| format!("every {what}");
        match key {
            "daily" => return Some((one, every("day"), Some(Falls::daily(None)))),
            "weekly" => return Some((one, every("week"), None)),
            "biweekly" | "fortnightly" => return Some((one, every("other week"), None)),
            "monthly" => return Some((one, every("month"), None)),
            "quarterly" => return Some((one, every("quarter"), None)),
            "yearly" | "annually" => return Some((one, every("year"), None)),
            "on" => {
                let plural = self.key(index + 1)?;
                let weekday = weekday_named(plural).filter(|_| plural.ends_with("days"))?;
                return Some((
                    index..index + 2,
                    every(weekday_word(weekday)),
                    Some(Falls {
                        days: vec![weekday],
                        hour: None,
                    }),
                ));
            }
            "every" | "each" => {}
            _ => return None,
        }
        let next = self.key(index + 1)?;
        if next == "other" {
            let after = self.key(index + 2)?;
            if let Some(weekday) = weekday_named(after) {
                return Some((
                    index..index + 3,
                    every(&format!("other {}", weekday_word(weekday))),
                    Some(Falls {
                        days: vec![weekday],
                        hour: None,
                    }),
                ));
            }
            let unit = unit(after)?;
            return Some((index..index + 3, every(&format!("other {unit}")), None));
        }
        if let (Some(number), Some(unit)) = (
            count(next).filter(|_| next != "a" && next != "an"),
            self.key(index + 2).and_then(unit),
        ) {
            return Some((index..index + 3, every(&format!("{number} {unit}s")), None));
        }
        let simple = match next {
            "day" => Some(Falls::daily(None)),
            "weekday" => Some(Falls {
                days: WEEKDAYS.to_vec(),
                hour: None,
            }),
            "week" | "month" | "quarter" | "year" => None,
            part => match part_hour(part) {
                Some(hour) => Some(Falls::daily(Some(hour))),
                None => return self.repeat_weekdays(index, next),
            },
        };
        Some((index..index + 2, every(next), simple))
    }

    /// "every monday", "every tuesday and thursday", "every friday
    /// morning", from `index`, its second word `first`.
    fn repeat_weekdays(
        &self,
        index: usize,
        first: &str,
    ) -> Option<(Range<usize>, String, Option<Falls>)> {
        let mut days = vec![weekday_named(first)?];
        let mut end = index + 2;
        loop {
            if let Some(weekday) = self.key(end).and_then(weekday_named) {
                days.push(weekday);
                end += 1;
            } else if let Some(weekday) = self
                .key(end + 1)
                .and_then(weekday_named)
                .filter(|_| self.key(end) == Some("and"))
            {
                days.push(weekday);
                end += 2;
            } else {
                break;
            }
        }
        let mut every = format!(
            "every {}",
            days.iter()
                .map(|weekday| weekday_word(*weekday))
                .collect::<Vec<_>>()
                .join(" and ")
        );
        let part = self.key(end).filter(|part| part_hour(part).is_some());
        let hour = part.and_then(part_hour);
        if let Some(part) = part {
            every.push(' ');
            every.push_str(part);
            end += 1;
        }
        Some((index..end, every, Some(Falls { days, hour })))
    }

    /// "hashtag design": a label that exists already.
    fn spoken_label(&mut self, lexicon: &Lexicon) {
        for index in 0..self.words.len() {
            if self.key(index) != Some("hashtag") {
                continue;
            }
            let Some(name) = self.key(index + 1) else {
                continue;
            };
            if let Some(field) = label_named(lexicon, name, false) {
                self.take(index..index + 2, field);
            }
        }
    }

    /// A project said by its title with "project" before or after it.
    fn spoken_project(&mut self, lexicon: &Lexicon) {
        for project in &lexicon.projects {
            let title: Vec<String> = split_words(&project.title)
                .into_iter()
                .map(|word| word.key)
                .collect();
            let title: Vec<&str> = title.iter().map(String::as_str).collect();
            if title.is_empty() {
                continue;
            }
            for index in 0..self.words.len() {
                let words = if self.reads(index, &title)
                    && self.key(index + title.len()) == Some("project")
                {
                    index..index + title.len() + 1
                } else if self.key(index) == Some("project") && self.reads(index + 1, &title) {
                    index..index + title.len() + 1
                } else {
                    continue;
                };
                self.take(
                    words,
                    WorkField::Project {
                        id: project.id.clone(),
                        title: project.title.clone(),
                    },
                );
                return;
            }
        }
    }

    /// Who a spoken part says owes it: the speaker ("I need to…", "remind
    /// me to…"), taken out of the title, or a person the part starts with,
    /// or is "waiting on", left in it.
    fn subject(&mut self, lexicon: &Lexicon) {
        let Some(first) = self.first_free() else {
            return;
        };
        if let Some(phrase) = SELF_PHRASES.iter().find(|phrase| self.reads(first, phrase)) {
            self.take(first..first + phrase.len(), me());
            return;
        }
        let two = self
            .key(first + 1)
            .and_then(|second| {
                let both = format!("{} {}", self.key(first)?, base_name(second));
                person_named(&lexicon.people, &both, false)
                    .filter(|person| key(&person.name) == key(&both))
            })
            .map(|person| (first..first + 2, person));
        let one = || {
            let name = base_name(self.key(first)?);
            person_named(&lexicon.people, name, false).map(|person| (first..first + 1, person))
        };
        if let Some((words, person)) = two.or_else(one) {
            self.keep(words, person_field(person));
            return;
        }
        for index in 0..self.words.len() {
            if self.reads(index, &["waiting", "on"]) || self.reads(index, &["waiting", "for"]) {
                if let Some(person) = self
                    .key(index + 2)
                    .and_then(|name| person_named(&lexicon.people, base_name(name), false))
                {
                    self.keep(index + 2..index + 3, person_field(person));
                    return;
                }
            }
        }
    }

    /// Whether a spoken part with no owner starts with a name nobody knows
    /// doing the work: "Dana's sending…", "Marketing needs…", "She'll…".
    fn someone_else(&self) -> bool {
        if self
            .found
            .iter()
            .any(|found| matches!(found.field, WorkField::Owner { .. }))
        {
            return false;
        }
        let Some(first) = self.first_free() else {
            return false;
        };
        let word = &self.words[first];
        pronoun(&word.key) || name_like(self.text, word) && acts(&self.words, first)
    }

    /// Drops the words a spoken part opens with that say nothing.
    fn lead_in(&mut self) {
        let mut index = 0;
        while let Some(key) = self.key(index) {
            let comma = self.text[self.words[index].range.clone()].ends_with(',');
            if !(LEAD_INS.contains(&key) || comma && ORDINAL_LEAD_INS.contains(&key)) {
                break;
            }
            self.uses[index] = Use::Gone;
            index += 1;
        }
    }

    /// The due date and time the line names, from the words the meeting
    /// reader understands, the ones only tasks use ("next week", "in 3
    /// days", "eod", "the 1st"), or a part of the day. A repeat's days
    /// date a time said alone; with no due words at all, the next time the
    /// repeat falls due goes on the repeat.
    fn due<Tz: TimeZone>(&mut self, said: Said, now: &DateTime<Tz>, falls: Option<Falls>) {
        let local = now.naive_local();
        let today = local.date();
        let names = match said {
            Said::Typed => DayNames::Typed,
            Said::Spoken => DayNames::Spoken,
        };
        let mut used = Vec::new();
        let mut day = None;
        let mut time = None;
        if let Some((when, ranges)) = spoken_time::find(&self.masked(), local, names) {
            for range in &ranges {
                used.extend(self.covering(range));
            }
            match when {
                SpokenTime::Day(date) => day = Some(date),
                SpokenTime::At { start, .. } => {
                    time = Some(start.time());
                    if ranges.len() > 1 {
                        day = Some(start.date());
                    } else if let Some((date, words)) = self.extra_day(today, &used) {
                        day = Some(date);
                        used.extend(words);
                    } else if falls.is_none() {
                        day = Some(start.date());
                    }
                }
            }
        }
        if day.is_none() {
            if let Some((date, words)) = self.extra_day(today, &used) {
                day = Some(date);
                used.extend(words);
            }
        }
        if let Some((hour, words)) = self.part_of_day(&used) {
            used.extend(words);
            let part = NaiveTime::from_hms_opt(hour, 0, 0);
            time = time.or(part);
            if day.is_none() && falls.is_none() {
                day = match part {
                    Some(part) if part <= local.time() => today.succ_opt(),
                    _ => Some(today),
                };
            }
        }
        let time = time.or_else(|| {
            falls
                .as_ref()?
                .hour
                .and_then(|hour| NaiveTime::from_hms_opt(hour, 0, 0))
        });
        let Some(clock) = time.or_else(|| NaiveTime::from_hms_opt(17, 0, 0)) else {
            return;
        };
        let date = match (day, &falls) {
            (Some(day), _) => Some(day),
            (None, Some(falls)) => falls.next(today).and_then(|first| {
                if first.and_time(clock) > local {
                    Some(first)
                } else {
                    falls.next(first.succ_opt()?)
                }
            }),
            (None, None) => None,
        };
        let Some(at) = date
            .and_then(|date| {
                now.timezone()
                    .from_local_datetime(&date.and_time(clock))
                    .earliest()
            })
            .map(|at| at.timestamp_millis())
        else {
            return;
        };
        if used.is_empty() {
            for found in &mut self.found {
                if let WorkField::Repeat { next_at, .. } = &mut found.field {
                    *next_at = Some(at);
                }
            }
            return;
        }
        let field = WorkField::Due {
            at,
            timed: time.is_some(),
        };
        used.sort_unstable();
        used.dedup();
        let mut runs: Vec<Range<usize>> = Vec::new();
        for index in used {
            match runs.last_mut() {
                Some(run) if run.end == index => run.end += 1,
                _ => runs.push(index..index + 1),
            }
        }
        for run in runs {
            self.take(run, field.clone());
        }
    }

    /// The text with every word that is not free blanked out, byte for byte.
    fn masked(&self) -> String {
        let mut masked = self.text.to_owned();
        for (word, used) in self.words.iter().zip(&self.uses) {
            if *used != Use::Free {
                masked.replace_range(word.range.clone(), &" ".repeat(word.range.len()));
            }
        }
        masked
    }

    /// The free words a byte range of the text touches.
    fn covering(&self, range: &Range<usize>) -> Vec<usize> {
        (0..self.words.len())
            .filter(|index| {
                let core = &self.words[*index].core;
                self.uses[*index] == Use::Free && core.start < range.end && range.start < core.end
            })
            .collect()
    }

    /// Whether the free words at `start`, none of them in `used`, read `keys`.
    fn reads_unused(&self, start: usize, used: &[usize], keys: &[&str]) -> bool {
        self.reads(start, keys) && !(start..start + keys.len()).any(|index| used.contains(&index))
    }

    fn extra_day(&self, today: NaiveDate, used: &[usize]) -> Option<(NaiveDate, Range<usize>)> {
        let phrases: [(&[&str], fn(NaiveDate) -> Option<NaiveDate>); 14] = [
            (&["end", "of", "the", "day"], Some),
            (&["end", "of", "day"], Some),
            (&["close", "of", "business"], Some),
            (&["later", "today"], Some),
            (&["eod"], Some),
            (&["cob"], Some),
            (&["end", "of", "the", "week"], end_of_week),
            (&["end", "of", "week"], end_of_week),
            (&["this", "week"], end_of_week),
            (&["eow"], end_of_week),
            (&["end", "of", "the", "month"], |today| {
                first_of_next_month(today)?.pred_opt()
            }),
            (&["end", "of", "month"], |today| {
                first_of_next_month(today)?.pred_opt()
            }),
            (&["eom"], |today| first_of_next_month(today)?.pred_opt()),
            (&["next", "month"], first_of_next_month),
        ];
        let weeks: [(&[&str], fn(NaiveDate) -> Option<NaiveDate>); 4] = [
            (&["next", "week"], |today| next_weekday(today, Weekday::Mon)),
            (&["this", "weekend"], |today| match today.weekday() {
                Weekday::Sun => Some(today),
                _ => on_or_after(today, Weekday::Sat),
            }),
            (&["the", "weekend"], |today| match today.weekday() {
                Weekday::Sun => Some(today),
                _ => on_or_after(today, Weekday::Sat),
            }),
            (&["next", "weekend"], |today| {
                next_weekday(today, Weekday::Mon)?.checked_add_days(Days::new(5))
            }),
        ];
        for index in 0..self.words.len() {
            for (phrase, day) in phrases.iter().chain(&weeks) {
                if self.reads_unused(index, used, phrase) {
                    return Some((day(today)?, index..index + phrase.len()));
                }
            }
            if self.reads_unused(index, used, &["the"]) && !used.contains(&(index + 1)) {
                if let Some(date) = self
                    .key(index + 1)
                    .and_then(day_of_month)
                    .and_then(|day| next_day_of_month(today, day))
                {
                    return Some((date, index..index + 2));
                }
            }
            if self.reads_unused(index, used, &["in"]) {
                if let Some(found) = self.in_count(index, today, used) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// "in 3 days", "in a week", "in a couple of weeks", "in a few days".
    fn in_count(
        &self,
        index: usize,
        today: NaiveDate,
        used: &[usize],
    ) -> Option<(NaiveDate, Range<usize>)> {
        let (number, unit_at) = if self.reads_unused(index + 1, used, &["a", "couple", "of"]) {
            (2, index + 4)
        } else if self.reads_unused(index + 1, used, &["a", "few"]) {
            (3, index + 3)
        } else {
            (count(self.key(index + 1)?)?, index + 2)
        };
        if used.contains(&unit_at) {
            return None;
        }
        let unit = unit(self.key(unit_at)?)?;
        Some((after(today, number, unit)?, index..unit_at + 1))
    }

    /// A part of the day said beside the due words, or after "this" or "in
    /// the": its usual hour and its words.
    fn part_of_day(&self, used: &[usize]) -> Option<(u32, Range<usize>)> {
        for index in 0..self.words.len() {
            let Some(hour) = part_hour(&self.words[index].key) else {
                continue;
            };
            if used.contains(&index) {
                return Some((hour, index..index + 1));
            }
            if self.uses[index] != Use::Free {
                continue;
            }
            let start = if index >= 2 && self.reads_unused(index - 2, used, &["in", "the"]) {
                index - 2
            } else if index >= 1 && self.reads_unused(index - 1, used, &["this"]) {
                index - 1
            } else {
                index
            };
            let beside = (start > 0 && used.contains(&(start - 1))) || used.contains(&(index + 1));
            if start < index || beside {
                return Some((hour, start..index + 1));
            }
        }
        None
    }

    /// Takes the words that only joined a field to the title, and the ones
    /// left hanging at either end of it.
    fn connectors(&mut self, said: Said) {
        for found in 0..self.found.len() {
            if self.found[found].kept {
                continue;
            }
            let before = match self.found[found].field {
                WorkField::Due { .. } => BEFORE_DUE,
                WorkField::Priority { .. } => BEFORE_PRIORITY,
                WorkField::Project { .. } => BEFORE_PROJECT,
                WorkField::Label { .. } => BEFORE_LABEL,
                WorkField::Owner { .. } | WorkField::Repeat { .. } => continue,
            };
            let mut chain = Vec::new();
            let mut index = self.found[found].words.start;
            while index > 0 {
                index -= 1;
                match self.key(index) {
                    Some(key) if before.contains(&key) || ARTICLES.contains(&key) => {
                        chain.push(index)
                    }
                    None if self.uses[index] == Use::Gone => {}
                    _ => break,
                }
            }
            while chain
                .last()
                .is_some_and(|index| ARTICLES.contains(&self.words[*index].key.as_str()))
            {
                chain.pop();
            }
            for index in chain {
                self.uses[index] = Use::Gone;
            }
        }
        if said == Said::Spoken {
            self.hanging_start();
        }
        self.hanging_end();
    }

    fn in_title(&self, index: usize) -> bool {
        match self.uses[index] {
            Use::Free | Use::Plain => true,
            Use::Field(found) => self.found[found].kept,
            Use::Gone => false,
        }
    }

    fn hanging_start(&mut self) {
        let Some(mut first) = (0..self.words.len()).find(|index| self.in_title(*index)) else {
            return;
        };
        if first == 0 {
            return;
        }
        let mut dropped = false;
        while self.key(first).is_some_and(|key| AFTER_LEAD.contains(&key)) {
            self.uses[first] = Use::Gone;
            dropped = true;
            first += 1;
        }
        while dropped && self.key(first).is_some_and(|key| ARTICLES.contains(&key)) {
            self.uses[first] = Use::Gone;
            first += 1;
        }
    }

    fn hanging_end(&mut self) {
        while let Some(last) = (0..self.words.len())
            .rev()
            .find(|index| self.in_title(*index))
        {
            let followed = last + 1 < self.words.len();
            if !followed || !self.key(last).is_some_and(|key| TRAILING.contains(&key)) {
                return;
            }
            self.uses[last] = Use::Gone;
        }
    }

    fn finish(self, said: Said) -> WorkParse {
        let title_words: Vec<&str> = (0..self.words.len())
            .filter(|index| self.in_title(*index))
            .map(|index| &self.text[self.words[index].range.clone()])
            .collect();
        let joined = title_words.join(" ");
        let tidy = joined
            .trim_start_matches(|character: char| {
                character.is_whitespace() || ",;:-\u{2013}\u{2014}".contains(character)
            })
            .trim_end_matches(|character: char| {
                character.is_whitespace() || ",;:.-\u{2013}\u{2014}".contains(character)
            });
        let capital =
            said == Said::Spoken || self.text.trim_start().starts_with(char::is_uppercase);
        let mut characters = tidy.chars();
        let title = match characters.next() {
            Some(first) if capital => first.to_uppercase().chain(characters).collect(),
            _ => tidy.to_owned(),
        };
        let mut found = self.found;
        found.sort_by_key(|found| found.words.start);
        let units = |byte: usize| {
            u32::try_from(self.text[..byte].encode_utf16().count()).unwrap_or(u32::MAX)
        };
        let tokens = found
            .into_iter()
            .map(|found| {
                let start = self.words[found.words.start].core.start;
                let end = self.words[found.words.end - 1].core.end;
                WorkToken {
                    text: self.text[start..end].to_owned(),
                    start: units(start),
                    end: units(end),
                    field: found.field,
                }
            })
            .collect();
        WorkParse { title, tokens }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::types::ProjectStatus;
    use chrono::FixedOffset;

    /// What the speech model wrote for one held shortcut: commas, no
    /// sentence breaks.
    const RAMBLE: &str = "I owe Sam the Deck Friday, Dana sending the rebate sheet Thursday, high priority on the pricing page.";

    /// Monday 9 November 2026, ten in the morning.
    fn monday() -> DateTime<FixedOffset> {
        FixedOffset::east_opt(0)
            .and_then(|zone| zone.with_ymd_and_hms(2026, 11, 9, 10, 0, 0).single())
            .expect("the test's clock")
    }

    fn at(month: u32, day: u32, hour: u32) -> i64 {
        monday()
            .timezone()
            .with_ymd_and_hms(2026, month, day, hour, 0, 0)
            .single()
            .expect("a time in the test's zone")
            .timestamp_millis()
    }

    fn lexicon(people: &[&str]) -> Lexicon {
        Lexicon {
            labels: vec![WorkLabel {
                id: "bug".to_owned(),
                name: "bug".to_owned(),
                hue: 0.0,
                revision: 1,
                group: None,
                retired_at_ms: None,
                scope: Default::default(),
            }],
            projects: ["Launch", "Q4 pricing"]
                .iter()
                .map(|title| WorkProjectChoice {
                    id: title.to_lowercase(),
                    title: (*title).to_owned(),
                    status: ProjectStatus::Active,
                })
                .collect(),
            people: people
                .iter()
                .map(|name| WorkPersonChoice {
                    id: name.to_lowercase(),
                    name: (*name).to_owned(),
                    aliases: Vec::new(),
                })
                .collect(),
        }
    }

    fn typed(line: &str, plain: &[&str]) -> WorkParse {
        let plain: Vec<String> = plain.iter().map(|word| (*word).to_owned()).collect();
        parse(
            line,
            Said::Typed,
            &plain,
            &lexicon(&["Dana Kim"]),
            &monday(),
        )
    }

    fn fields(parse: &WorkParse) -> Vec<WorkField> {
        parse
            .tokens
            .iter()
            .map(|token| token.field.clone())
            .collect()
    }

    fn repeat(every: &str, next_at: Option<i64>) -> WorkField {
        WorkField::Repeat {
            every: every.to_owned(),
            next_at,
        }
    }

    fn due(at: i64, timed: bool) -> WorkField {
        WorkField::Due { at, timed }
    }

    fn dana() -> WorkField {
        WorkField::Owner {
            owner: WorkOwner::Person {
                person_id: "dana kim".to_owned(),
            },
            name: "Dana Kim".to_owned(),
        }
    }

    #[test]
    fn typed_words_set_their_fields_and_leave_the_title() {
        let parse = typed("Fix login bug @Dana friday p2 #bug +Q4 pricing", &[]);

        assert_eq!(parse.title, "Fix login bug");
        assert_eq!(
            fields(&parse),
            [
                dana(),
                due(at(11, 13, 17), false),
                WorkField::Priority { level: 2 },
                WorkField::Label {
                    id: Some("bug".to_owned()),
                    name: "bug".to_owned()
                },
                WorkField::Project {
                    id: "q4 pricing".to_owned(),
                    title: "Q4 pricing".to_owned()
                },
            ]
        );
    }

    #[test]
    fn a_chip_turned_back_into_text_stays_in_the_title() {
        let parse = typed("Plan offsite friday", &["friday"]);

        assert_eq!(parse.title, "Plan offsite friday");
        assert!(parse.tokens.is_empty());
    }

    #[test]
    fn chips_sit_on_the_text_field_in_utf16_units() {
        let parse = typed("Ship “deck” friday", &[]);

        let token = &parse.tokens[0];
        assert_eq!(
            (token.text.as_str(), token.start, token.end),
            ("friday", 12, 18)
        );
    }

    #[test]
    fn a_repeat_dates_the_task_from_the_days_it_names() {
        assert_eq!(
            fields(&typed("Review metrics every monday", &[])),
            [repeat("every monday", Some(at(11, 9, 17)))]
        );
        assert_eq!(
            fields(&typed("Water plants every other friday", &[])),
            [repeat("every other friday", Some(at(11, 13, 17)))]
        );
        assert_eq!(
            fields(&typed("Standup notes every weekday at 9", &[])),
            [repeat("every weekday", None), due(at(11, 10, 9), true)]
        );
        let rent = typed("Pay rent every month on the 1st", &[]);
        assert_eq!(rent.title, "Pay rent");
        assert_eq!(
            fields(&rent),
            [repeat("every month", None), due(at(12, 1, 17), false)]
        );
    }

    #[test]
    fn a_ramble_splits_where_a_part_names_who_does_it_or_its_priority() {
        let task = |part: &str, title: &str, fields: Vec<WorkField>, someone_else: bool| {
            let start = RAMBLE.find(part).expect("a part of the ramble");
            SpokenTask {
                said: start..start + part.len(),
                title: title.to_owned(),
                fields,
                someone_else,
            }
        };
        let rebate = "Dana sending the rebate sheet Thursday";
        let thursday = due(at(11, 12, 17), false);
        for (people, dana_task) in [
            (
                &["Sam Patel", "Dana Kim"][..],
                task(
                    rebate,
                    "Dana sending the rebate sheet",
                    vec![dana(), thursday.clone()],
                    false,
                ),
            ),
            (
                &[][..],
                task(
                    rebate,
                    "Dana sending the rebate sheet",
                    vec![thursday.clone()],
                    true,
                ),
            ),
        ] {
            assert_eq!(
                spoken_tasks(RAMBLE, &lexicon(people), &monday()),
                [
                    task(
                        "I owe Sam the Deck Friday",
                        "Owe Sam the Deck",
                        vec![me(), due(at(11, 13, 17), false)],
                        false,
                    ),
                    dana_task,
                    task(
                        "high priority on the pricing page.",
                        "Pricing page",
                        vec![WorkField::Priority { level: 2 }],
                        false,
                    ),
                ],
                "people known: {people:?}"
            );
        }
    }

    #[test]
    fn names_listed_with_and_stay_in_one_task() {
        let passage = "Ping Sam and Dana about the offsite, then call the bank tomorrow morning.";

        let titles: Vec<String> =
            spoken_tasks(passage, &lexicon(&["Sam Patel", "Dana Kim"]), &monday())
                .into_iter()
                .map(|task| task.title)
                .collect();

        assert_eq!(
            titles,
            ["Ping Sam and Dana about the offsite", "Call the bank"]
        );
    }

    /// A captured task keeps its repeat in the parser's words, and saving
    /// one refuses words that are not one repeat; so every repeat the
    /// parser writes has to read back as itself, or a capture saying it
    /// would be refused.
    #[test]
    fn every_repeat_the_parser_writes_reads_back_as_itself() {
        let said = [
            "daily",
            "weekly",
            "biweekly",
            "fortnightly",
            "monthly",
            "quarterly",
            "annually",
            "on mondays",
            "every day",
            "every weekday",
            "every morning",
            "every other week",
            "every other friday",
            "every two weeks",
            "each 3 months",
            "every tuesday and thursday",
            "every friday evening",
        ];
        let changed: Vec<(&str, Option<String>, Option<String>)> = said
            .into_iter()
            .map(|words| {
                let written = super::repeat(words, &monday()).map(|(every, _)| every);
                let again = written
                    .as_deref()
                    .and_then(|every| super::repeat(every, &monday()))
                    .map(|(every, _)| every);
                (words, written, again)
            })
            .filter(|(_, written, again)| written.is_none() || written != again)
            .collect();

        assert_eq!(changed, Vec::new(), "these repeats do not read back");
    }
}
