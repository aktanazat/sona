//! What a preview flags before a page goes out: phone numbers, email
//! addresses, amounts, links, long digit runs and the names of known people
//! who are not a named speaker. The checks scan exactly the page that would be
//! published — the included sections, after the person filter — so a flag
//! names something the page really says and the share sheet can offer to
//! replace it.
//!
//! A digit run of seven or more digits becomes `phone` or `number`. Dates
//! and clock times are blanked before runs are read, so "2026-11-24 10:30"
//! never joins into one long run.

use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;

use super::content::{Page, MAX_TEXT_BYTES};
use super::{store_error, SharedPagesError};
use crate::meeting::store::MeetingStore;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ShareCheckKind {
    Phone,
    Email,
    Money,
    Link,
    Number,
    Person,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ShareCheckSection {
    Title,
    ForPerson,
    Summary,
    Decisions,
    ActionItems,
    Notes,
    Speakers,
    Transcript,
}
/// One thing on the page worth a second look. `text` is the exact substring
/// the flag covers, so replacing every occurrence of it in the draft removes
/// the flag. `index` only places the flag on screen.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ShareCheck {
    pub kind: ShareCheckKind,
    pub section: ShareCheckSection,
    pub index: u32,
    pub text: String,
}

static DIGIT_RUN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    // PANIC: the literal pattern is fixed; Regex has no compile-time constructor.
    regex::Regex::new(r"\+?\(?\d[\d\s().\-+]*\d|\d").expect("digit run pattern")
});
static DATE_OR_TIME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    // PANIC: the literal pattern is fixed; Regex has no compile-time constructor.
    regex::Regex::new(r"\b\d{4}-\d{1,2}-\d{1,2}\b|\b\d{1,2}:\d{2}(?::\d{2})?\b")
        .expect("date or time pattern")
});
static EMAIL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    // PANIC: the literal pattern is fixed; Regex has no compile-time constructor.
    regex::Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}")
        .expect("email pattern")
});
static MONEY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    // PANIC: the literal pattern is fixed; Regex has no compile-time constructor.
    regex::Regex::new(
        r"(?i)[$€£¥]\s?\d+(?:,\d{3})*(?:\.\d+)?|\b\d+(?:,\d{3})*(?:\.\d+)?\s+(dollars?|cents?|euros?|usd|eur|gbp|pounds?)\b",
    )
    .expect("money pattern")
});
static LINK: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    // PANIC: the literal pattern is fixed; Regex has no compile-time constructor.
    regex::Regex::new(r"(?i)(?:https?://|www\.)[^\s]+").expect("link pattern")
});

/// The names the people check may flag: each person in the store's people
/// directory, with their display name and aliases together.
pub(crate) fn people_names(store: &MeetingStore) -> Result<Vec<Vec<String>>, SharedPagesError> {
    let list = store.people_list().map_err(store_error)?;
    Ok(list
        .entries
        .into_iter()
        .map(|entry| {
            let mut names = entry.person.aliases;
            names.push(entry.person.display_name);
            names
        })
        .collect())
}

/// Every flag on the page, once per distinct text: removing a flag removes
/// every copy of it. `speakers` is the draft's speaker list: a person any of
/// whose names is a speaker, or is the person the page is for, is never
/// flagged under any of their names. `people` holds each directory person's
/// names; empty means the people check is skipped.
pub(crate) fn check_page(
    page: &Page,
    speakers: &[String],
    people: &[Vec<String>],
) -> Vec<ShareCheck> {
    let spoken = speakers
        .iter()
        .chain(&page.for_person)
        .map(|name| single_line_lower(name))
        .collect::<HashSet<_>>();
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for person in people {
        if person
            .iter()
            .any(|name| spoken.contains(&single_line_lower(name)))
        {
            continue;
        }
        for name in person {
            let folded = single_line_lower(name);
            if folded.chars().count() < 3
                || folded.len() > MAX_TEXT_BYTES
                || !seen.insert(folded)
            {
                continue;
            }
            names.push(name.trim());
        }
    }
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let pattern = names
        .iter()
        .map(|name| regex::escape(name))
        .collect::<Vec<_>>()
        .join("|");
    let person = (!pattern.is_empty())
        .then(|| regex::Regex::new(&format!(r"(?i)\b(?:{pattern})\b")).ok())
        .flatten();

    let mut checks = Vec::new();
    let mut flagged = HashSet::new();
    let mut index = 0_u32;
    for (section, text) in items(page) {
        let mut flag = |kind: ShareCheckKind, text: &str| {
            if !flagged.insert((kind, text.to_owned())) {
                return;
            }
            checks.push(ShareCheck {
                kind,
                section,
                index,
                text: text.to_owned(),
            });
            index += 1;
        };
        let numbers = DATE_OR_TIME.replace_all(text, |found: &regex::Captures<'_>| {
            "x".repeat(found[0].len())
        });
        for found in DIGIT_RUN.find_iter(&numbers) {
            let run = &text[found.range()];
            let digits = run.chars().filter(|c| c.is_ascii_digit()).count();
            if digits < 7 {
                continue;
            }
            let is_phone = digits <= 15
                && (run.contains(['+', '(', ')', '-', ' '])
                    || (digits >= 11 && !run.contains('.')));
            flag(
                if is_phone {
                    ShareCheckKind::Phone
                } else {
                    ShareCheckKind::Number
                },
                run,
            );
        }
        for found in EMAIL.find_iter(text) {
            flag(ShareCheckKind::Email, found.as_str());
        }
        for found in MONEY.find_iter(text) {
            flag(ShareCheckKind::Money, found.as_str());
        }
        for found in LINK.find_iter(text) {
            let url = found
                .as_str()
                .trim_end_matches(|c: char| ".,;:!?'\")".contains(c));
            if !url.is_empty() {
                flag(ShareCheckKind::Link, url);
            }
        }
        if let Some(person) = &person {
            for found in person.find_iter(text) {
                flag(ShareCheckKind::Person, found.as_str());
            }
        }
    }
    checks
}

/// Every string the page shows, labelled with the section a flag on it
/// belongs to.
fn items(page: &Page) -> Vec<(ShareCheckSection, &str)> {
    let mut items = Vec::new();
    if let Some(title) = &page.title {
        items.push((ShareCheckSection::Title, title.as_str()));
    }
    if let Some(person) = &page.for_person {
        items.push((ShareCheckSection::ForPerson, person.as_str()));
    }
    for paragraph in page.summary.iter().flatten() {
        items.push((ShareCheckSection::Summary, paragraph.as_str()));
    }
    for decision in page.decisions.iter().flatten() {
        items.push((ShareCheckSection::Decisions, decision.as_str()));
    }
    for item in page.action_items.iter().flatten() {
        items.push((ShareCheckSection::ActionItems, item.text.as_str()));
        if let Some(owner) = &item.owner {
            items.push((ShareCheckSection::ActionItems, owner.as_str()));
        }
        if let Some(due) = &item.due {
            items.push((ShareCheckSection::ActionItems, due.as_str()));
        }
    }
    for paragraph in page.notes.iter().flatten() {
        items.push((ShareCheckSection::Notes, paragraph.as_str()));
    }
    for name in page.speakers.iter().flatten() {
        items.push((ShareCheckSection::Speakers, name.as_str()));
    }
    for turn in page.transcript.iter().flatten() {
        items.push((ShareCheckSection::Transcript, turn.text.as_str()));
    }
    items
}

fn single_line_lower(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
