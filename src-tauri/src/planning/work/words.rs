//! Filter words: "high priority bugs due this week that are mine" read as
//! the conditions a list filters by. Every word is read, joins words that
//! are, or is kept as not understood; none is dropped.

use super::filter::{self, WorkCondition, WorkFilterField, WorkFilterOp};
use super::parse::{self, Lexicon, Said, WorkField, WorkToken};
use super::types::{WorkLabel, WorkPersonChoice, WorkProjectChoice};
use super::with_meetings;
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use chrono::{DateTime, Datelike, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::ops::Range;
use std::sync::Arc;
use tauri::State;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkWhere {
    /// What the words were read as, in their order; all of them must hold.
    pub terms: Vec<WorkWhereTerm>,
    /// Words read as nothing, in their order.
    pub unread: Vec<WorkWhereWord>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkWhereTerm {
    /// The words as typed.
    pub text: String,
    /// The condition in words: "Priority is High".
    pub label: String,
    pub condition: WorkCondition,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkWhereWord {
    pub text: String,
    /// What a word naming more than one thing could mean: "label Mobile",
    /// "project Mobile". Empty when it names nothing.
    pub could_be: Vec<String>,
}

/// Reads filter words as conditions, keeping the words it could not read.
#[tauri::command]
#[specta::specta]
pub async fn work_where(
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    text: String,
) -> Result<WorkWhere, String> {
    let now = chrono::Local::now();
    with_meetings(&history_state, &meetings, |history, meeting_conn| {
        let lexicon = Lexicon::read(history, meeting_conn)?;
        Ok(read(&text, &lexicon, &now))
    })
    .await
}

pub(crate) fn read<Tz: TimeZone>(text: &str, lexicon: &Lexicon, now: &DateTime<Tz>) -> WorkWhere {
    Reader {
        text,
        toks: toks(text),
        lexicon,
        now,
        today: now.date_naive(),
    }
    .read()
}

/// One word, or one part of a hyphenated one: "high" and "priority" in
/// "high-priority".
struct Tok {
    core: Range<usize>,
    key: String,
}

fn toks(text: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    for word in parse::split_words(text) {
        let hyphenated = word.key.contains('-')
            && word
                .key
                .split('-')
                .all(|part| !part.is_empty() && part.chars().all(char::is_alphabetic));
        if !hyphenated {
            toks.push(Tok {
                core: word.core,
                key: word.key,
            });
            continue;
        }
        let mut at = word.core.start;
        for part in text[word.core].split('-') {
            toks.push(Tok {
                core: at..at + part.len(),
                key: part.to_lowercase(),
            });
            at += part.len() + 1;
        }
    }
    toks
}

#[derive(Clone, Debug)]
struct Term {
    field: WorkFilterField,
    op: WorkFilterOp,
    values: Vec<String>,
    names: Vec<String>,
}

impl Term {
    fn one(field: WorkFilterField, op: WorkFilterOp, value: &str, name: &str) -> Self {
        Self {
            field,
            op,
            values: vec![value.to_owned()],
            names: vec![name.to_owned()],
        }
    }

    fn of(field: WorkFilterField, pairs: &[(&str, &str)]) -> Self {
        Self {
            field,
            op: if pairs.len() > 1 {
                WorkFilterOp::AnyOf
            } else {
                WorkFilterOp::Is
            },
            values: pairs.iter().map(|(value, _)| (*value).to_owned()).collect(),
            names: pairs.iter().map(|(_, name)| (*name).to_owned()).collect(),
        }
    }

    fn label(&self) -> String {
        let field = match self.field {
            WorkFilterField::Due => "due date",
            WorkFilterField::Created => "created date",
            WorkFilterField::Updated => "updated date",
            other => other.word(),
        };
        if self.values == ["none"] {
            match self.op {
                WorkFilterOp::Is => return format!("No {field}"),
                WorkFilterOp::IsNot => return format!("Has a {field}"),
                _ => {}
            }
        }
        let op = match self.op {
            WorkFilterOp::Is => "is",
            WorkFilterOp::IsNot => "is not",
            WorkFilterOp::AnyOf => "is any of",
            WorkFilterOp::NoneOf => "is none of",
            WorkFilterOp::IncludesAll => "includes all of",
            WorkFilterOp::IncludesAny => "includes any of",
            WorkFilterOp::Before => "before",
            WorkFilterOp::After => "after",
        };
        let word = self.field.word();
        let mut letters = word.chars();
        let field = letters
            .next()
            .map(|first| first.to_uppercase().chain(letters).collect::<String>())
            .unwrap_or_default();
        format!("{field} {op} {}", self.names.join(", "))
    }
}

enum Reading {
    Term(Term),
    Not,
    Or,
    Glue,
    Unclear(Vec<String>),
    Unread,
}

#[derive(Clone, Copy)]
enum Fixed {
    Glue,
    Not,
    Or,
    Status(&'static [(&'static str, &'static str)]),
    Priority(&'static [(&'static str, &'static str)]),
    Owner(&'static str, &'static str),
    Others,
    Due(WorkFilterOp, &'static str),
    Cycle(&'static str, &'static str),
    NoProject,
    NoLabel,
}

const OPEN: &[(&str, &str)] = &[
    ("backlog", "Backlog"),
    ("not_started", "Todo"),
    ("in_progress", "In progress"),
];
const DONE: &[(&str, &str)] = &[("done", "Done")];
const STARTED: &[(&str, &str)] = &[("in_progress", "In progress")];
const TODO: &[(&str, &str)] = &[("not_started", "Todo")];
const CANCELED: &[(&str, &str)] = &[("canceled", "Canceled")];
const DUPLICATE: &[(&str, &str)] = &[("duplicate", "Duplicate")];
const ME: Fixed = Fixed::Owner("me", "me");
const NOBODY: Fixed = Fixed::Owner("none", "no one");
const OVERDUE: Fixed = Fixed::Due(WorkFilterOp::Before, "today");
const UNDATED: Fixed = Fixed::Due(WorkFilterOp::Is, "none");
const DATED: Fixed = Fixed::Due(WorkFilterOp::IsNot, "none");
const NO_PRIORITY: Fixed = Fixed::Priority(&[("0", "No priority")]);
const NO_CYCLE: Fixed = Fixed::Cycle("none", "no cycle");

const LEVELS: [&str; 5] = ["No priority", "Urgent", "High", "Medium", "Low"];

const PHRASES: &[(&[&str], Fixed)] = &[
    (&["show", "me"], Fixed::Glue),
    (&["give", "me"], Fixed::Glue),
    (&["find", "me"], Fixed::Glue),
    (&["get", "me"], Fixed::Glue),
    (&["not"], Fixed::Not),
    (&["isn't"], Fixed::Not),
    (&["isnt"], Fixed::Not),
    (&["aren't"], Fixed::Not),
    (&["arent"], Fixed::Not),
    (&["without"], Fixed::Not),
    (&["except"], Fixed::Not),
    (&["excluding"], Fixed::Not),
    (&["no"], Fixed::Not),
    (&["non"], Fixed::Not),
    (&["or"], Fixed::Or),
    (&["mine"], ME),
    (&["my"], ME),
    (&["me"], ME),
    (&["i", "owe"], ME),
    (&["i", "own"], ME),
    (&["for", "me"], ME),
    (&["unassigned"], NOBODY),
    (&["nobody"], NOBODY),
    (&["no", "owner"], NOBODY),
    (&["no", "one"], NOBODY),
    (&["without", "owner"], NOBODY),
    (&["without", "an", "owner"], NOBODY),
    (&["not", "assigned"], NOBODY),
    (&["waiting"], Fixed::Others),
    (&["waiting", "on", "others"], Fixed::Others),
    (&["waiting", "on", "someone"], Fixed::Others),
    (&["waiting", "on", "them"], Fixed::Others),
    (&["others", "owe"], Fixed::Others),
    (&["owed", "to", "me"], Fixed::Others),
    (&["open"], Fixed::Status(OPEN)),
    (&["active"], Fixed::Status(OPEN)),
    (&["done"], Fixed::Status(DONE)),
    (&["completed"], Fixed::Status(DONE)),
    (&["complete"], Fixed::Status(DONE)),
    (&["finished"], Fixed::Status(DONE)),
    (
        &["closed"],
        Fixed::Status(&[
            ("done", "Done"),
            ("canceled", "Canceled"),
            ("duplicate", "Duplicate"),
        ]),
    ),
    (&["canceled"], Fixed::Status(CANCELED)),
    (&["cancelled"], Fixed::Status(CANCELED)),
    (&["in", "progress"], Fixed::Status(STARTED)),
    (&["started"], Fixed::Status(STARTED)),
    (&["ongoing"], Fixed::Status(STARTED)),
    (&["todo"], Fixed::Status(TODO)),
    (&["to", "do"], Fixed::Status(TODO)),
    (&["not", "started"], Fixed::Status(TODO)),
    (&["unstarted"], Fixed::Status(TODO)),
    (&["backlog"], Fixed::Status(&[("backlog", "Backlog")])),
    (&["triage"], Fixed::Status(&[("triage", "Triage")])),
    (&["duplicate"], Fixed::Status(DUPLICATE)),
    (&["duplicates"], Fixed::Status(DUPLICATE)),
    (&["urgent"], Fixed::Priority(&[("1", "Urgent")])),
    (&["high"], Fixed::Priority(&[("2", "High")])),
    (&["medium"], Fixed::Priority(&[("3", "Medium")])),
    (&["low"], Fixed::Priority(&[("4", "Low")])),
    (
        &["important"],
        Fixed::Priority(&[("1", "Urgent"), ("2", "High")]),
    ),
    (&["no", "priority"], NO_PRIORITY),
    (&["without", "priority"], NO_PRIORITY),
    (&["without", "a", "priority"], NO_PRIORITY),
    (&["overdue"], OVERDUE),
    (&["late"], OVERDUE),
    (&["past", "due"], OVERDUE),
    (&["no", "due", "date"], UNDATED),
    (&["no", "due"], UNDATED),
    (&["no", "date"], UNDATED),
    (&["no", "deadline"], UNDATED),
    (&["undated"], UNDATED),
    (&["without", "due", "date"], UNDATED),
    (&["without", "a", "due", "date"], UNDATED),
    (&["with", "a", "due", "date"], DATED),
    (&["with", "due", "date"], DATED),
    (&["has", "a", "due", "date"], DATED),
    (&["dated"], DATED),
    (&["this", "cycle"], Fixed::Cycle("current", "current")),
    (&["current", "cycle"], Fixed::Cycle("current", "current")),
    (&["next", "cycle"], Fixed::Cycle("next", "next")),
    (&["no", "cycle"], NO_CYCLE),
    (&["without", "cycle"], NO_CYCLE),
    (&["without", "a", "cycle"], NO_CYCLE),
    (&["no", "project"], Fixed::NoProject),
    (&["without", "project"], Fixed::NoProject),
    (&["without", "a", "project"], Fixed::NoProject),
    (&["no", "label"], Fixed::NoLabel),
    (&["no", "labels"], Fixed::NoLabel),
    (&["unlabeled"], Fixed::NoLabel),
    (&["unlabelled"], Fixed::NoLabel),
    (&["without", "labels"], Fixed::NoLabel),
    (&["without", "a", "label"], Fixed::NoLabel),
];

/// Words that join the words that say something, and say nothing.
const GLUE: &[&str] = &[
    "a", "all", "an", "and", "any", "are", "assigned", "at", "be", "by", "every", "everything",
    "find", "for", "from", "get", "give", "has", "have", "i", "in", "is", "issue", "issues", "it",
    "item", "items", "just", "label", "labeled", "labelled", "labels", "list", "of", "on", "ones",
    "only", "owe", "owed", "owes", "owned", "please", "priority", "project", "show", "tagged",
    "task", "tasks", "that", "the", "them", "there", "these", "things", "those", "to", "was",
    "were", "what", "where", "which", "who", "with", "work",
];

/// Days that move with the calendar, in the condition's words.
const WINDOWS: &[(&[&str], &str)] = &[
    (&["today"], "today"),
    (&["tomorrow"], "tomorrow"),
    (&["yesterday"], "yesterday"),
    (&["this", "week"], "this_week"),
    (&["next", "week"], "next_week"),
    (&["last", "week"], "last_week"),
    (&["this", "month"], "this_month"),
    (&["next", "month"], "next_month"),
    (&["last", "month"], "last_month"),
];

#[derive(Clone, Copy)]
enum Relation {
    On,
    Before,
    After,
    By,
    Since,
}

const RELATIONS: &[(&str, Relation)] = &[
    ("on", Relation::On),
    ("before", Relation::Before),
    ("after", Relation::After),
    ("by", Relation::By),
    ("until", Relation::By),
    ("till", Relation::By),
    ("since", Relation::Since),
    ("from", Relation::Since),
];

const DATED_FIELDS: &[(&str, WorkFilterField)] = &[
    ("due", WorkFilterField::Due),
    ("created", WorkFilterField::Created),
    ("added", WorkFilterField::Created),
    ("opened", WorkFilterField::Created),
    ("updated", WorkFilterField::Updated),
    ("changed", WorkFilterField::Updated),
    ("edited", WorkFilterField::Updated),
];

/// A day a condition compares against: one that moves with the calendar,
/// or a fixed one.
#[derive(Clone, Copy)]
enum Day {
    Named(&'static str),
    On(NaiveDate),
}

impl Day {
    fn value(self) -> String {
        match self {
            Self::Named(name) => name.to_owned(),
            Self::On(day) => day.format("%Y-%m-%d").to_string(),
        }
    }

    fn name(self, today: NaiveDate) -> String {
        match self {
            Self::Named(name) => name.replace('_', " "),
            Self::On(day) if day.year() == today.year() => day.format("%b %-d").to_string(),
            Self::On(day) => day.format("%b %-d, %Y").to_string(),
        }
    }

    /// The day that starts where this one ends, so "before" it is "by" this.
    fn following(self, today: NaiveDate) -> Option<Self> {
        Some(match self {
            Self::Named("yesterday") => Self::Named("today"),
            Self::Named("today") => Self::Named("tomorrow"),
            Self::Named("last_week") => Self::Named("this_week"),
            Self::Named("this_week") => Self::Named("next_week"),
            Self::Named("last_month") => Self::Named("this_month"),
            Self::Named("this_month") => Self::Named("next_month"),
            Self::Named(name) => Self::On(filter::span(name, today)?.1),
            Self::On(day) => Self::On(day.succ_opt()?),
        })
    }

    /// The day that ends where this one starts, so "after" it is "since"
    /// this.
    fn preceding(self, today: NaiveDate) -> Option<Self> {
        Some(match self {
            Self::Named("tomorrow") => Self::Named("today"),
            Self::Named("today") => Self::Named("yesterday"),
            Self::Named("next_week") => Self::Named("this_week"),
            Self::Named("this_week") => Self::Named("last_week"),
            Self::Named("next_month") => Self::Named("this_month"),
            Self::Named("this_month") => Self::Named("last_month"),
            Self::Named(name) => Self::On(filter::span(name, today)?.0.pred_opt()?),
            Self::On(day) => Self::On(day.pred_opt()?),
        })
    }
}

enum Named<'l> {
    Label(&'l WorkLabel),
    Project(&'l WorkProjectChoice),
    Person(&'l WorkPersonChoice),
}

impl Named<'_> {
    fn term(&self) -> Term {
        match self {
            Self::Label(label) => Term::one(
                WorkFilterField::Label,
                WorkFilterOp::Is,
                &label.id,
                &label.name,
            ),
            Self::Project(project) => Term::one(
                WorkFilterField::Project,
                WorkFilterOp::Is,
                &project.id,
                &project.title,
            ),
            Self::Person(person) => Term::one(
                WorkFilterField::Owner,
                WorkFilterOp::Is,
                &person.id,
                &person.name,
            ),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Label(label) => format!("label {}", label.name),
            Self::Project(project) => format!("project {}", project.title),
            Self::Person(person) => format!("person {}", person.name),
        }
    }
}

/// `said` is `name`, or a plural of it: "bugs", "fixes", "stories".
fn plural_of(name: &str, said: &str) -> bool {
    said == name
        || said.strip_suffix('s') == Some(name)
        || said.strip_suffix("es") == Some(name)
        || said
            .strip_suffix("ies")
            .is_some_and(|stem| name.strip_suffix('y') == Some(stem))
}

/// The op a negated condition takes; a range has none.
const fn negated(op: WorkFilterOp) -> Option<WorkFilterOp> {
    match op {
        WorkFilterOp::Is => Some(WorkFilterOp::IsNot),
        WorkFilterOp::IsNot => Some(WorkFilterOp::Is),
        WorkFilterOp::AnyOf | WorkFilterOp::IncludesAny => Some(WorkFilterOp::NoneOf),
        WorkFilterOp::NoneOf => Some(WorkFilterOp::AnyOf),
        WorkFilterOp::IncludesAll | WorkFilterOp::Before | WorkFilterOp::After => None,
    }
}

/// The op two conditions on one field joined by "or" take together.
fn joined(kept: WorkFilterOp, next: WorkFilterOp, field: WorkFilterField) -> Option<WorkFilterOp> {
    let either = |op| {
        matches!(
            op,
            WorkFilterOp::Is | WorkFilterOp::AnyOf | WorkFilterOp::IncludesAny
        )
    };
    (either(kept) && either(next)).then_some(if field == WorkFilterField::Label {
        WorkFilterOp::IncludesAny
    } else {
        WorkFilterOp::AnyOf
    })
}

struct Reader<'a, Tz: TimeZone> {
    text: &'a str,
    toks: Vec<Tok>,
    lexicon: &'a Lexicon,
    now: &'a DateTime<Tz>,
    today: NaiveDate,
}

impl<Tz: TimeZone> Reader<'_, Tz> {
    fn read(&self) -> WorkWhere {
        let mut terms: Vec<(Range<usize>, Term)> = Vec::new();
        let mut unread: Vec<(Range<usize>, Vec<String>)> = Vec::new();
        let mut glue = vec![false; self.toks.len()];
        let mut not: Option<Range<usize>> = None;
        let mut or: Option<Range<usize>> = None;
        let mut at = 0;
        while at < self.toks.len() {
            let (len, reading) = self.best(at);
            let span = at..at + len;
            at += len;
            match reading {
                Reading::Glue => glue[span].fill(true),
                Reading::Not => unread.extend(not.replace(span).map(|word| (word, Vec::new()))),
                Reading::Or => unread.extend(or.replace(span).map(|word| (word, Vec::new()))),
                Reading::Term(mut term) => {
                    let mut span = span;
                    if let Some(word) = not.take() {
                        match negated(term.op) {
                            Some(op) => {
                                term.op = op;
                                span.start = word.start;
                            }
                            None => unread.push((word, Vec::new())),
                        }
                    }
                    if let Some(word) = or.take() {
                        if let Some((kept_span, kept)) =
                            terms.last_mut().filter(|(_, kept)| kept.field == term.field)
                        {
                            if let Some(op) = joined(kept.op, term.op, term.field) {
                                kept.op = op;
                                for (value, name) in term.values.into_iter().zip(term.names) {
                                    if !kept.values.contains(&value) {
                                        kept.values.push(value);
                                        kept.names.push(name);
                                    }
                                }
                                kept_span.end = span.end;
                                continue;
                            }
                        }
                        unread.push((word, Vec::new()));
                    }
                    terms.push((span, term));
                }
                Reading::Unclear(could_be) => {
                    unread.extend(not.take().into_iter().chain(or.take()).map(|word| (word, Vec::new())));
                    unread.push((span, could_be));
                }
                Reading::Unread => {
                    unread.extend(not.take().into_iter().chain(or.take()).map(|word| (word, Vec::new())));
                    unread.push((span, Vec::new()));
                }
            }
        }
        unread.extend(not.into_iter().chain(or).map(|word| (word, Vec::new())));
        unread.sort_by_key(|(span, _)| span.start);
        let mut runs: Vec<(Range<usize>, Vec<String>)> = Vec::new();
        for (span, could_be) in unread {
            if let Some((last, kept)) = runs.last_mut() {
                if kept.is_empty()
                    && could_be.is_empty()
                    && glue.get(last.end..span.start).is_some_and(|gap| gap.iter().all(|&joins| joins))
                {
                    last.end = span.end;
                    continue;
                }
            }
            runs.push((span, could_be));
        }
        WorkWhere {
            terms: terms
                .into_iter()
                .map(|(span, term)| WorkWhereTerm {
                    text: self.words(span),
                    label: term.label(),
                    condition: WorkCondition {
                        field: term.field,
                        op: term.op,
                        values: term.values,
                    },
                })
                .collect(),
            unread: runs
                .into_iter()
                .map(|(span, could_be)| WorkWhereWord {
                    text: self.words(span),
                    could_be,
                })
                .collect(),
        }
    }

    fn words(&self, span: Range<usize>) -> String {
        match (self.toks.get(span.start), span.end.checked_sub(1).and_then(|last| self.toks.get(last))) {
            (Some(first), Some(last)) => self.text[first.core.start..last.core.end].to_owned(),
            _ => String::new(),
        }
    }

    fn key(&self, at: usize) -> Option<&str> {
        self.toks.get(at).map(|tok| tok.key.as_str())
    }

    fn starts(&self, at: usize, words: &[&str]) -> bool {
        words
            .iter()
            .enumerate()
            .all(|(offset, word)| self.key(at + offset) == Some(*word))
    }

    /// The longest reading at `at`. On equal length a fixed word beats a
    /// name and a name beats a joining word.
    fn best(&self, at: usize) -> (usize, Reading) {
        let Some(key) = self.key(at) else {
            return (1, Reading::Unread);
        };
        if key.starts_with(['#', '+', '@']) {
            return (1, self.marked(key).map_or(Reading::Unread, Reading::Term));
        }
        let mut best: Option<(usize, Reading)> = None;
        let mut offer = |len: usize, reading: Reading| {
            if best.as_ref().is_none_or(|(kept, _)| len > *kept) {
                best = Some((len, reading));
            }
        };
        if let Some((len, term)) = self.dated(at) {
            offer(len, Reading::Term(term));
        }
        if let Some((len, fixed)) = PHRASES
            .iter()
            .filter(|(words, _)| self.starts(at, words))
            .map(|(words, fixed)| (words.len(), *fixed))
            .max_by_key(|(len, _)| *len)
        {
            offer(len, Self::fixed(fixed));
        }
        if let Some((len, level)) = parse::PRIORITY_PHRASES
            .iter()
            .filter(|(words, _)| self.starts(at, words))
            .map(|(words, level)| (words.len(), *level))
            .max_by_key(|(len, _)| *len)
        {
            offer(len, Self::level(level).map_or(Reading::Unread, Reading::Term));
        }
        if let Some(level) = key
            .strip_prefix(['p', '!'])
            .and_then(|level| level.parse::<u8>().ok())
            .filter(|level| (1..=4).contains(level))
        {
            offer(1, Self::level(level).map_or(Reading::Unread, Reading::Term));
        }
        if key == "cycle" {
            if let Some(number) = self.key(at + 1).and_then(|next| next.parse::<u32>().ok()) {
                let number = number.to_string();
                offer(
                    2,
                    Reading::Term(Term::one(
                        WorkFilterField::Cycle,
                        WorkFilterOp::Is,
                        &number,
                        &number,
                    )),
                );
            }
        }
        if key == "project" && self.names(at + 1).is_none() {
            if let Some(WorkField::Project { id, title }) = self
                .key(at + 1)
                .and_then(|next| parse::project_named(self.lexicon, next))
            {
                offer(
                    2,
                    Reading::Term(Term::one(
                        WorkFilterField::Project,
                        WorkFilterOp::Is,
                        &id,
                        &title,
                    )),
                );
            }
        }
        let lead = usize::from(key == "waiting" && self.key(at + 1) == Some("on")) * 2;
        if let Some((len, named)) = self.names(at + lead) {
            let people = named.iter().all(|one| matches!(one, Named::Person(_)));
            if lead == 0 || people {
                let reading = match named.as_slice() {
                    [one] => Reading::Term(one.term()),
                    many => Reading::Unclear(many.iter().map(Named::describe).collect()),
                };
                offer(lead + len, reading);
            }
        }
        if GLUE.contains(&key) {
            offer(1, Reading::Glue);
        }
        best.unwrap_or((1, Reading::Unread))
    }

    fn fixed(fixed: Fixed) -> Reading {
        let term = match fixed {
            Fixed::Glue => return Reading::Glue,
            Fixed::Not => return Reading::Not,
            Fixed::Or => return Reading::Or,
            Fixed::Status(pairs) => Term::of(WorkFilterField::Status, pairs),
            Fixed::Priority(pairs) => Term::of(WorkFilterField::Priority, pairs),
            Fixed::Owner(value, name) => {
                Term::one(WorkFilterField::Owner, WorkFilterOp::Is, value, name)
            }
            Fixed::Others => Term {
                field: WorkFilterField::Owner,
                op: WorkFilterOp::NoneOf,
                values: vec!["me".to_owned(), "none".to_owned()],
                names: vec!["me".to_owned(), "no one".to_owned()],
            },
            Fixed::Due(op, value) => {
                Term::one(WorkFilterField::Due, op, value, &value.replace('_', " "))
            }
            Fixed::Cycle(value, name) => {
                Term::one(WorkFilterField::Cycle, WorkFilterOp::Is, value, name)
            }
            Fixed::NoProject => {
                Term::one(WorkFilterField::Project, WorkFilterOp::Is, "none", "no project")
            }
            Fixed::NoLabel => {
                Term::one(WorkFilterField::Label, WorkFilterOp::Is, "none", "no label")
            }
        };
        Reading::Term(term)
    }

    fn level(level: u8) -> Option<Term> {
        let name = LEVELS.get(usize::from(level))?;
        Some(Term::one(
            WorkFilterField::Priority,
            WorkFilterOp::Is,
            &level.to_string(),
            name,
        ))
    }

    /// `#label`, `+project` or `@person`.
    fn marked(&self, key: &str) -> Option<Term> {
        let mut letters = key.chars();
        let mark = letters.next()?;
        let name = letters.as_str();
        let wanted = parse::key(name);
        match mark {
            '#' => self
                .lexicon
                .labels
                .iter()
                .find(|label| label.retired_at_ms.is_none() && plural_of(&parse::key(&label.name), &wanted))
                .map(|label| Named::Label(label).term()),
            '+' => match parse::project_named(self.lexicon, name)? {
                WorkField::Project { id, title } => Some(Term::one(
                    WorkFilterField::Project,
                    WorkFilterOp::Is,
                    &id,
                    &title,
                )),
                _ => None,
            },
            '@' if wanted == "me" => Some(Term::one(
                WorkFilterField::Owner,
                WorkFilterOp::Is,
                "me",
                "me",
            )),
            '@' => parse::person_named(&self.lexicon.people, name, true)
                .map(|person| Named::Person(person).term()),
            _ => None,
        }
    }

    /// Labels, projects and people named by the most words from `at`, up to
    /// four.
    fn names(&self, at: usize) -> Option<(usize, Vec<Named<'_>>)> {
        for len in (1..=4).rev() {
            let Some(span) = self.toks.get(at..at + len) else {
                continue;
            };
            let last = len - 1;
            let joined: String = span
                .iter()
                .enumerate()
                .map(|(offset, tok)| {
                    parse::key(if offset == last {
                        parse::base_name(&tok.key)
                    } else {
                        &tok.key
                    })
                })
                .collect();
            if !joined.chars().any(char::is_alphabetic) {
                continue;
            }
            let lexicon = self.lexicon;
            let mut found: Vec<Named<'_>> = lexicon
                .labels
                .iter()
                .filter(|label| {
                    label.retired_at_ms.is_none() && plural_of(&parse::key(&label.name), &joined)
                })
                .map(Named::Label)
                .collect();
            found.extend(
                lexicon
                    .projects
                    .iter()
                    .filter(|project| parse::key(&project.title) == joined)
                    .map(Named::Project),
            );
            let called: Vec<&WorkPersonChoice> = lexicon
                .people
                .iter()
                .filter(|person| {
                    parse::key(&person.name) == joined
                        || person.aliases.iter().any(|alias| parse::key(alias) == joined)
                })
                .collect();
            if called.is_empty() && len == 1 {
                found.extend(
                    lexicon
                        .people
                        .iter()
                        .filter(|person| {
                            person
                                .name
                                .split_whitespace()
                                .next()
                                .is_some_and(|first| parse::key(first) == joined)
                        })
                        .map(Named::Person),
                );
            } else {
                found.extend(called.into_iter().map(Named::Person));
            }
            if !found.is_empty() {
                return Some((len, found));
            }
        }
        None
    }

    /// A day condition from `at`: a field (due, created, updated), how it
    /// compares (on, before, after, by, since) and the day. Without a field
    /// it is the due day; without a word saying when, only a day that moves
    /// with the calendar, a weekday or a day with numbers in it is read.
    fn dated(&self, at: usize) -> Option<(usize, Term)> {
        let mut next = at;
        let field = DATED_FIELDS
            .iter()
            .find(|(word, _)| self.key(next) == Some(*word))
            .map(|(_, field)| *field);
        next += usize::from(field.is_some());
        let relation = RELATIONS
            .iter()
            .find(|(word, _)| self.key(next) == Some(*word))
            .map(|(_, relation)| *relation);
        next += usize::from(relation.is_some());
        let (len, day) = self.day(next, field.is_some() || relation.is_some())?;
        let (op, day) = match relation.unwrap_or(Relation::On) {
            Relation::On => (WorkFilterOp::Is, day),
            Relation::Before => (WorkFilterOp::Before, day),
            Relation::After => (WorkFilterOp::After, day),
            Relation::By => (WorkFilterOp::Before, day.following(self.today)?),
            Relation::Since => (WorkFilterOp::After, day.preceding(self.today)?),
        };
        let term = Term::one(
            field.unwrap_or(WorkFilterField::Due),
            op,
            &day.value(),
            &day.name(self.today),
        );
        Some((next + len - at, term))
    }

    fn day(&self, at: usize, led: bool) -> Option<(usize, Day)> {
        if let Some(&(words, name)) = WINDOWS.iter().find(|(words, _)| self.starts(at, words)) {
            return Some((words.len(), Day::Named(name)));
        }
        for len in (1..=3).rev() {
            let Some(span) = self.toks.get(at..at + len) else {
                continue;
            };
            let dated = span.iter().any(|tok| {
                parse::weekday_named(&tok.key).is_some()
                    || tok.key.chars().any(|character| character.is_ascii_digit())
            });
            if !led && !dated {
                continue;
            }
            let words = &self.text[span[0].core.start..span[len - 1].core.end];
            let read = parse::parse(words, Said::Typed, &[], &Lexicon::default(), self.now);
            if let [WorkToken {
                field: WorkField::Due { at: due_at, .. },
                ..
            }] = read.tokens.as_slice()
            {
                if read.title.is_empty() {
                    let day = self
                        .now
                        .timezone()
                        .timestamp_millis_opt(*due_at)
                        .single()?
                        .date_naive();
                    return Some((len, Day::On(day)));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::types::ProjectStatus;
    use chrono::FixedOffset;
    use super::{WorkFilterField as F, WorkFilterOp as O};

    fn lexicon() -> Lexicon {
        let label = |id: &str, name: &str| WorkLabel {
            id: id.to_owned(),
            name: name.to_owned(),
            hue: 0.0,
            revision: 1,
            group: None,
            retired_at_ms: None,
        };
        let project = |id: &str, title: &str| WorkProjectChoice {
            id: id.to_owned(),
            title: title.to_owned(),
            status: ProjectStatus::Active,
        };
        Lexicon {
            labels: vec![label("bug", "bug"), label("mobile", "Mobile")],
            projects: vec![project("launch", "Launch"), project("mobile-app", "Mobile")],
            people: vec![WorkPersonChoice {
                id: "dana".to_owned(),
                name: "Dana Kim".to_owned(),
                aliases: Vec::new(),
            }],
        }
    }

    fn words(text: &str) -> WorkWhere {
        let wednesday = FixedOffset::east_opt(0)
            .and_then(|zone| zone.with_ymd_and_hms(2026, 11, 11, 10, 0, 0).single())
            .expect("the test's clock");
        read(text, &lexicon(), &wednesday)
    }

    fn conditions(read: &WorkWhere) -> Vec<(F, O, Vec<&str>)> {
        read.terms
            .iter()
            .map(|term| {
                let condition = &term.condition;
                (condition.field, condition.op, condition.values.iter().map(String::as_str).collect())
            })
            .collect()
    }

    fn unread(read: &WorkWhere) -> Vec<(&str, Vec<&str>)> {
        read.unread
            .iter()
            .map(|word| (word.text.as_str(), word.could_be.iter().map(String::as_str).collect()))
            .collect()
    }

    #[test]
    fn plain_phrases_read_as_the_conditions_they_say() {
        let phrases: [(&str, Vec<(F, O, Vec<&str>)>); 6] = [
            (
                "high priority bugs due this week that are mine",
                vec![
                    (F::Priority, O::Is, vec!["2"]),
                    (F::Label, O::Is, vec!["bug"]),
                    (F::Due, O::Is, vec!["this_week"]),
                    (F::Owner, O::Is, vec!["me"]),
                ],
            ),
            (
                "urgent or high not done",
                vec![(F::Priority, O::AnyOf, vec!["1", "2"]), (F::Status, O::IsNot, vec!["done"])],
            ),
            (
                "unassigned overdue tasks in Launch",
                vec![
                    (F::Owner, O::Is, vec!["none"]),
                    (F::Due, O::Before, vec!["today"]),
                    (F::Project, O::Is, vec!["launch"]),
                ],
            ),
            (
                "open tasks for Dana created since last week",
                vec![
                    (F::Status, O::AnyOf, vec!["backlog", "not_started", "in_progress"]),
                    (F::Owner, O::Is, vec!["dana"]),
                    (F::Created, O::After, vec!["2026-11-01"]),
                ],
            ),
            (
                "due before 2026-12-01 without a project",
                vec![(F::Due, O::Before, vec!["2026-12-01"]), (F::Project, O::Is, vec!["none"])],
            ),
            (
                "done or canceled this cycle",
                vec![(F::Status, O::AnyOf, vec!["done", "canceled"]), (F::Cycle, O::Is, vec!["current"])],
            ),
        ];
        for (text, expected) in phrases {
            let read = words(text);
            assert_eq!(conditions(&read), expected, "{text}");
            assert!(read.unread.is_empty(), "{text}: {:?}", read.unread);
        }
    }

    #[test]
    fn a_word_it_cannot_read_is_kept() {
        let read = words("high priority frobnicate");

        assert_eq!(conditions(&read), [(F::Priority, O::Is, vec!["2"])]);
        assert_eq!(unread(&read), [("frobnicate", vec![])]);
    }

    #[test]
    fn a_word_naming_two_things_offers_both_and_applies_neither() {
        let read = words("mobile bugs");

        assert_eq!(conditions(&read), [(F::Label, O::Is, vec!["bug"])]);
        assert_eq!(unread(&read), [("mobile", vec!["label Mobile", "project Mobile"])]);
    }
}
