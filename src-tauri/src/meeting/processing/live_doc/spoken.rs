//! Changes to the doc said out loud: "Sona, add coffee", "Sona, check off
//! the bread", "Sona, group these by aisle". An ask is one model request
//! for its changes, and one or two more for its sections when it asks to
//! group or sort. Both name points by their words, which the model copies far more
//! reliably than labels; Apple's on-device model, given labels, answered
//! with the words anyway. The changes land in place, in the order the asks
//! were heard, and the ask keeps what it did so the screen can confirm it in
//! a word. The words of a point someone typed are never changed or removed
//! this way; checking it off or moving it keeps them.

use super::live_changes::Author;
use super::{
    first_json_value, json_response_schema, related, words, Edited, LiveTranscript,
    MeetingProcessingService, MeetingTextGenerator, MAX_POINTS, MAX_POINT_CHARS,
};
use crate::meeting::ledger::fold;
use crate::meeting::live_doc_types::{
    MeetingLiveChangeKind, MeetingLiveDocPoint, MeetingVoiceOutcome,
};
use crate::meeting::store::MeetingStore;
use crate::meeting::types::MeetingSessionId;
use schemars::{schema::RootSchema, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;
use uuid::Uuid;

const CHANGES_PROMPT: &str = include_str!("../../../../resources/prompts/live_doc_edit.txt");
const GROUPS_PROMPT: &str = include_str!("../../../../resources/prompts/live_doc_group.txt");
const OUTPUT_TOKENS: i32 = 900;
const MAX_SECTION_CHARS: usize = 60;
/// Words that ask to group or sort the doc rather than change its points.
const GROUP_WORDS: &[&str] = &["group", "sort", "separate", "split", "organize", "organise"];
/// Words that ask to change points. An ask with none of them and a group
/// word only groups.
const CHANGE_WORDS: &[&str] = &[
    "add", "remove", "delete", "drop", "rename", "change", "replace", "reword", "check", "tick",
    "cross", "mark", "uncheck", "untick", "make", "create", "write", "list", "have", "got",
];
/// Words that name no item: "the oat milk" is the oat milk point.
const PLAIN: &[&str] = &["the", "a", "an", "my", "our", "some", "any", "of", "and"];
static CHANGES_SCHEMA: LazyLock<RootSchema> = LazyLock::new(json_response_schema::<Changes>);
static GROUPS_SCHEMA: LazyLock<RootSchema> = LazyLock::new(json_response_schema::<Groups>);

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Changes {
    #[schemars(length(max = "MAX_POINTS"))]
    changes: Vec<Change>,
}

/// One change to one point: `item` names it by its words, or holds the
/// words to add; `to` holds a rename's new words.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Change {
    action: Action,
    item: String,
    to: String,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Action {
    Add,
    Check,
    Uncheck,
    Remove,
    Rename,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Groups {
    #[schemars(length(max = "MAX_POINTS"))]
    sections: Vec<Group>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Group {
    title: String,
    #[schemars(length(max = "MAX_POINTS"))]
    items: Vec<String>,
}

#[derive(Serialize)]
struct Item<'a> {
    item: &'a str,
    done: bool,
    typed: bool,
}

#[derive(Serialize)]
struct ChangesInput<'a> {
    list: Vec<Item<'a>>,
    ask: &'a str,
}

#[derive(Serialize)]
struct GroupsInput<'a> {
    items: &'a [String],
    /// The sections the doc has now, which an item may join.
    sections: &'a [String],
    ask: &'a str,
}

/// What one ask did to the doc.
#[derive(Default)]
struct Applied {
    changed: Vec<Edited>,
    /// Points moved or changed heading. A move changes no words, so it is
    /// not a change anyone is marked for.
    moved: bool,
    /// The ask would have changed or removed words someone typed.
    kept_typed: bool,
    /// A request the ask needed got no usable answer, or the call stopped
    /// before its answer landed.
    failed: bool,
}

impl Applied {
    fn landed(&self) -> bool {
        !self.changed.is_empty() || self.moved
    }

    fn outcome(&self) -> MeetingVoiceOutcome {
        match (self.failed, self.landed()) {
            (true, true) => MeetingVoiceOutcome::Partial,
            (true, false) => MeetingVoiceOutcome::Failed,
            (false, _) if self.kept_typed => MeetingVoiceOutcome::KeptTyped,
            (false, false) => MeetingVoiceOutcome::Unchanged,
            (false, true) => MeetingVoiceOutcome::Done,
        }
    }
}

impl MeetingProcessingService {
    /// Make the spoken edits waiting, oldest first, until none are left,
    /// then let the making of them go. The caller holds it; see
    /// [`super::Heard`].
    pub(crate) fn edit_live_doc_by_voice(
        &self,
        store: &MeetingStore,
        session_id: MeetingSessionId,
        live: &LiveTranscript,
    ) {
        let generator = self.text_generator_for_session(store, session_id);
        while let Some((sequence, words, said_ns)) = next(live) {
            let applied = generator.as_deref().map_or_else(
                || Applied {
                    failed: true,
                    ..Applied::default()
                },
                |generator| make(generator, live, &words, said_ns),
            );
            if let Some(ask) = live
                .doc()
                .asks
                .iter_mut()
                .find(|ask| ask.sequence == sequence)
            {
                ask.outcome = Some(applied.outcome());
            }
            if applied.landed() {
                super::save(store, session_id, live);
            }
            self.emit_current(store, "meeting:live-doc-changed", session_id);
        }
    }
}

/// The oldest edit not made yet: its ask's sequence, words, and when it
/// was said. `None` lets the making go, under the same lock that queues
/// them, so an edit heard now finds nobody making them and claims it. Once
/// the call is stopping, the edits still waiting fail rather than land
/// after the meeting's page took the doc over.
fn next(live: &LiveTranscript) -> Option<(u32, String, u64)> {
    let mut doc = live.doc();
    if doc.stopping {
        let waiting: Vec<u32> = doc.edits.drain(..).collect();
        for ask in doc
            .asks
            .iter_mut()
            .filter(|ask| waiting.contains(&ask.sequence))
        {
            ask.outcome = Some(MeetingVoiceOutcome::Failed);
        }
    }
    let Some(ask) = doc
        .edits
        .pop_front()
        .and_then(|sequence| doc.asks.iter().find(|ask| ask.sequence == sequence))
    else {
        doc.editing = false;
        return None;
    };
    Some((ask.sequence, ask.words.clone(), ask.start_offset_ns))
}

/// Ask the model what `ask` changes and how it groups the doc, and make
/// what it answered. A request that gets no usable answer marks the ask
/// failed and leaves what the other made.
fn make(
    generator: &dyn MeetingTextGenerator,
    live: &LiveTranscript,
    ask: &str,
    said_ns: u64,
) -> Applied {
    let said: Vec<String> = words(ask).map(|(word, _)| word).collect();
    let groups = said.iter().any(|word| GROUP_WORDS.contains(&word.as_str()));
    let changes = !groups
        || said
            .iter()
            .any(|word| CHANGE_WORDS.contains(&word.as_str()));
    let mut applied = Applied::default();
    if changes {
        let list: Vec<(String, bool, bool)> = live
            .doc()
            .points
            .iter()
            .map(|point| (point.text.clone(), point.done, typed(point)))
            .collect();
        let list = list
            .iter()
            .map(|(item, done, typed)| Item {
                item,
                done: *done,
                typed: *typed,
            })
            .collect();
        let answer = serde_json::to_string(&ChangesInput { list, ask })
            .ok()
            .and_then(|input| {
                request::<Changes>(generator, live, CHANGES_PROMPT, &input, || &CHANGES_SCHEMA)
            });
        let mut doc = live.doc();
        match answer {
            Some(Changes { changes }) if !doc.stopping => {
                change(&mut doc.points, &changes, said_ns, &mut applied);
            }
            _ => applied.failed = true,
        }
    }
    if groups {
        match group(generator, live, ask) {
            Some(moved) => applied.moved = moved,
            None => applied.failed = true,
        }
    }
    for edited in &applied.changed {
        edited.record(live, &Author::Owner);
    }
    applied
}

fn request<T: for<'de> Deserialize<'de>>(
    generator: &dyn MeetingTextGenerator,
    live: &LiveTranscript,
    prompt: &str,
    input: &str,
    schema: fn() -> &'static RootSchema,
) -> Option<T> {
    live.doc().requests += 1;
    generator
        .generate_json(prompt, input, OUTPUT_TOKENS, schema)
        .map_err(|error| log::info!("A spoken edit got no answer: {error:?}"))
        .and_then(|answer| first_json_value::<T>(answer.trim()))
        .ok()
}

/// Typed by a person, by hand or from the meeting's link.
fn typed(point: &MeetingLiveDocPoint) -> bool {
    point.edited && !point.spoken
}

/// The point among those `open` lets through that `item` names: the one
/// with the same words, else the only one that holds every word of it
/// besides "the" and its kin. "the oat milk" names "Oat milk, the barista
/// one" when no point says just that.
fn find(
    points: &[MeetingLiveDocPoint],
    item: &str,
    open: impl Fn(&MeetingLiveDocPoint) -> bool,
) -> Option<usize> {
    let item_words: Vec<String> = words(item)
        .map(|(word, _)| word)
        .filter(|word| !PLAIN.contains(&word.as_str()))
        .collect();
    if item_words.is_empty() {
        return None;
    }
    if let Some(index) = points
        .iter()
        .position(|point| open(point) && fold(&point.text) == fold(item))
    {
        return Some(index);
    }
    let mut holding = points
        .iter()
        .enumerate()
        .filter(|(_, point)| open(point))
        .filter(|(_, point)| {
            let point_words: Vec<String> = words(&point.text).map(|(word, _)| word).collect();
            item_words
                .iter()
                .all(|wanted| point_words.iter().any(|word| related(word, wanted)))
        });
    let (index, _) = holding.next()?;
    holding.next().is_none().then_some(index)
}

/// Make `changes` on `points`. A change that names no point is passed over.
fn change(
    points: &mut Vec<MeetingLiveDocPoint>,
    changes: &[Change],
    said_ns: u64,
    applied: &mut Applied,
) {
    for change in changes {
        if let Action::Add = change.action {
            add(points, &change.item, said_ns, applied);
            continue;
        }
        let Some(index) = find(points, &change.item, |_| true) else {
            continue;
        };
        let point = &mut points[index];
        match change.action {
            Action::Add => {}
            Action::Check | Action::Uncheck => {
                let done = matches!(change.action, Action::Check);
                if point.done == done {
                    continue;
                }
                point.done = done;
                applied.changed.push(Edited {
                    point_id: point.id.clone(),
                    kind: MeetingLiveChangeKind::Edited,
                    text: point.text.clone(),
                });
            }
            Action::Remove | Action::Rename if typed(point) => applied.kept_typed = true,
            Action::Remove => {
                let removed = points.remove(index);
                applied.changed.push(Edited {
                    point_id: removed.id,
                    kind: MeetingLiveChangeKind::Removed,
                    text: removed.text,
                });
            }
            Action::Rename => {
                let Some(text) = line(&change.to).filter(|text| *text != point.text) else {
                    continue;
                };
                point.text.clone_from(&text);
                point.edited = true;
                point.spoken = true;
                applied.changed.push(Edited {
                    point_id: point.id.clone(),
                    kind: MeetingLiveChangeKind::Edited,
                    text,
                });
            }
        }
    }
}

/// A new point from `item` at the end of the doc, under no heading, unless
/// the doc already says it. Grouping again files it.
fn add(points: &mut Vec<MeetingLiveDocPoint>, item: &str, said_ns: u64, applied: &mut Applied) {
    let Some(text) = line(item).map(|text| capitalized(&text)) else {
        return;
    };
    if points.len() >= MAX_POINTS || points.iter().any(|point| fold(&point.text) == fold(&text)) {
        return;
    }
    let id = Uuid::new_v4().to_string();
    applied.changed.push(Edited {
        point_id: id.clone(),
        kind: MeetingLiveChangeKind::Added,
        text: text.clone(),
    });
    points.push(MeetingLiveDocPoint {
        id,
        text,
        edited: true,
        spoken: true,
        done: false,
        section: None,
        start_offset_ns: Some(said_ns),
    });
}

/// Group the doc as `ask` says: one request for every point, and one more
/// for the points it left out, given the sections the first made. The
/// grouping is worked out on a copy and lands under one lock, so a screen
/// never reads it half done. Returns whether any point moved or changed
/// heading; `None` when the first request got no usable answer or the call
/// stopped first.
fn group(generator: &dyn MeetingTextGenerator, live: &LiveTranscript, ask: &str) -> Option<bool> {
    let mut points = live.doc().points.clone();
    let items: Vec<String> = points.iter().map(|point| point.text.clone()).collect();
    let first = sort(generator, live, ask, &items, &[])?;
    for point in &mut points {
        point.section = None;
    }
    let mut order = file(&mut points, &first);
    let left: Vec<String> = points
        .iter()
        .filter(|point| point.section.is_none())
        .map(|point| point.text.clone())
        .collect();
    if !left.is_empty() && !order.is_empty() {
        if let Some(second) = sort(generator, live, ask, &left, &order) {
            for title in file(&mut points, &second) {
                if !order.contains(&title) {
                    order.push(title);
                }
            }
        }
    }
    points.sort_by_key(|point| {
        point
            .section
            .as_ref()
            .and_then(|section| order.iter().position(|title| title == section))
            .unwrap_or(order.len())
    });
    let placed: HashMap<&str, (usize, Option<String>)> = points
        .iter()
        .enumerate()
        .map(|(rank, point)| (point.id.as_str(), (rank, point.section.clone())))
        .collect();
    let mut doc = live.doc();
    if doc.stopping {
        return None;
    }
    let before: Vec<(String, Option<String>)> = doc
        .points
        .iter()
        .map(|point| (point.id.clone(), point.section.clone()))
        .collect();
    for point in &mut doc.points {
        if let Some((_, section)) = placed.get(point.id.as_str()) {
            point.section.clone_from(section);
        }
    }
    doc.points.sort_by_key(|point| {
        placed
            .get(point.id.as_str())
            .map_or(usize::MAX, |(rank, _)| *rank)
    });
    Some(
        doc.points
            .iter()
            .map(|point| (point.id.as_str(), point.section.as_deref()))
            .ne(before
                .iter()
                .map(|(id, section)| (id.as_str(), section.as_deref()))),
    )
}

fn sort(
    generator: &dyn MeetingTextGenerator,
    live: &LiveTranscript,
    ask: &str,
    items: &[String],
    sections: &[String],
) -> Option<Vec<Group>> {
    let input = serde_json::to_string(&GroupsInput {
        items,
        sections,
        ask,
    })
    .ok()?;
    request::<Groups>(generator, live, GROUPS_PROMPT, &input, || &GROUPS_SCHEMA)
        .map(|groups| groups.sections)
}

/// Give each point a group names, and has no heading yet, the group's
/// title; the first group to name a point keeps it. Returns the titles in
/// order.
fn file(points: &mut [MeetingLiveDocPoint], groups: &[Group]) -> Vec<String> {
    let mut titles = Vec::new();
    for group in groups {
        let Some(title) = heading(&group.title) else {
            continue;
        };
        for item in &group.items {
            if let Some(index) = find(points, item, |point| point.section.is_none()) {
                points[index].section = Some(title.clone());
            }
        }
        if !titles.contains(&title) {
            titles.push(title);
        }
    }
    titles
}

/// Words fit for a point: one line, not blank, and not too long.
fn line(text: &str) -> Option<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()
        && text.chars().count() <= MAX_POINT_CHARS
        && !text.chars().any(char::is_control))
    .then_some(text)
}

/// `text` with its first letter a capital, as the doc writes an item.
fn capitalized(text: &str) -> String {
    let mut characters = text.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}

/// A section's title, cut to a heading's length; `None` when blank.
fn heading(title: &str) -> Option<String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = title
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_SECTION_CHARS)
        .collect();
    (!title.is_empty()).then_some(title)
}
