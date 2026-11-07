//! What a recording with nothing on its calendar may be: a plan heard in an
//! earlier recording whose time it starts in, with the people the plan names
//! and the place an earlier recording saved under the plan's words; or, with
//! no such plan, someone whose voice the capture hears. These wait beside the
//! start's other maybes and change nothing until the user keeps one.

use super::context::{context_in, save_context_in};
use super::conversation_review::{distance_meters, SAME_PLACE_METERS};
use super::evening::{
    accept_filing_in, confirmed_people_in, insert_suggestion_in, target_identity,
    FILING_EVENT_SLACK_MS,
};
use super::heard_plans::{answers_to, plan_in, plans_in};
use super::people::{all_people_in, bump_people_revision_in, upsert_link_in};
use super::voice_identity::{known_voices_in, names_user};
use super::{decode_json, encode_json, id, session_row, MeetingStore, StoreError};
use crate::meeting::context::{
    ConversationContext, ConversationContextUpdate, ConversationLocation,
};
use crate::meeting::diarization::SpeakerEmbedding;
use crate::meeting::evening::{FilingPerson, FilingReason, FilingSuggestion, FilingTarget};
use crate::meeting::heard_plan_types::HeardPlan;
use crate::meeting::people_types::{Person, PersonId, PersonLinkConfidence, PersonLinkSource};
use crate::meeting::types::{MeetingOperationId, MeetingOrigin, MeetingSessionId};
use crate::query::tools::STOPWORDS;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashSet;

/// A recording is offered at most this many plans.
const MAX_MEETING_MAYBES: usize = 2;

/// Words that say what sort of place a place is, not which one.
const PLACE_KINDS: &[&str] = &[
    "bar",
    "cafe",
    "café",
    "coffee",
    "desk",
    "downstairs",
    "entrance",
    "floor",
    "front",
    "home",
    "hotel",
    "house",
    "lobby",
    "lounge",
    "office",
    "outside",
    "place",
    "restaurant",
    "room",
    "shop",
    "spot",
    "there",
    "upstairs",
    "usual",
];

/// A voice the running capture has heard, as its live pass groups voices:
/// the mean of its windows so far, and when it was first heard. Never stored.
#[derive(Clone, Debug)]
pub(crate) struct HeardVoice {
    pub(crate) voice: SpeakerEmbedding,
    pub(crate) first_heard_ns: u64,
}

/// A person whose confirmed voice a heard voice matches.
struct HeardPerson {
    person_id: PersonId,
    name: String,
    first_heard_ns: u64,
}

impl MeetingStore {
    /// Keeps a meeting maybe: the recording takes `title`, a confirmed link
    /// to each of `people` and `location` when it has no place yet, and its
    /// other meeting maybes close. Returns the people revision when a link
    /// changed.
    pub(crate) fn keep_meeting_maybe(
        &self,
        suggestion_id: &str,
        session_id: MeetingSessionId,
        title: &str,
        people: &[PersonId],
        location: Option<ConversationLocation>,
        now_utc_ms: i64,
    ) -> Result<Option<u64>, StoreError> {
        let snapshot = self.session_snapshot(session_id)?;
        if snapshot.title != title {
            self.set_title(
                MeetingOperationId::new(),
                now_utc_ms,
                session_id,
                snapshot.revision,
                title.to_owned(),
            )?;
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut linked = false;
        for person_id in people {
            linked |= upsert_link_in(
                &transaction,
                session_id,
                *person_id,
                PersonLinkSource::Manual,
                PersonLinkConfidence::Confirmed,
                now_utc_ms,
            )?;
        }
        let revision = linked
            .then(|| bump_people_revision_in(&transaction))
            .transpose()?;
        let current = context_in(&transaction, session_id)?;
        if let Some(location) = location.filter(|_| current.location.is_none()) {
            save_context_in(
                &transaction,
                &ConversationContextUpdate {
                    session_id,
                    expected_revision: current.revision,
                    kind: current.kind,
                    location: Some(location),
                },
            )?;
        }
        accept_filing_in(&transaction, suggestion_id, now_utc_ms)?;
        transaction.execute(
            "UPDATE filing_suggestions SET state = 'declined', decided_at_utc_ms = ?2
              WHERE session_id = ?1 AND target_kind = 'meeting' AND state = 'pending'",
            params![id(session_id), now_utc_ms],
        )?;
        transaction.commit()?;
        Ok(revision)
    }
}

/// Offers what a recording with nothing on its calendar may be: the plans
/// its start falls in, best first, and each person whose voice it hears that
/// none of them names.
pub(super) fn offer_meeting_maybes_in(
    connection: &Connection,
    context: &ConversationContext,
    heard: &[HeardVoice],
    user_name: Option<&str>,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    let session_id = context.session_id;
    let Some(started_utc_ms) = session_row(connection, session_id)?
        .started_at_utc_ms
        .filter(|_| context.origin != MeetingOrigin::Import)
    else {
        return Ok(());
    };
    let heard = heard_people_in(connection, session_id, heard, user_name)?;
    let plans = plans_near_in(connection, session_id, started_utc_ms, now_utc_ms)?;
    let people = if plans.is_empty() {
        Vec::new()
    } else {
        all_people_in(connection)?
    };
    let mut offers = Vec::new();
    for plan in plans {
        let Some(window) = plan.window else {
            continue;
        };
        if plan
            .happened_session_id
            .is_some_and(|happened| happened != session_id)
        {
            continue;
        }
        let known = plan_people(&plan.people, &people, &heard, user_name);
        let voices = known
            .iter()
            .filter_map(|person| {
                heard
                    .iter()
                    .find(|voice| voice.person_id == person.person_id)
            })
            .map(|voice| FilingReason::VoiceMatches {
                name: voice.name.clone(),
                start_offset_ns: Some(voice.first_heard_ns),
            })
            .collect::<Vec<_>>();
        if window.all_day && voices.is_empty() {
            continue;
        }
        let location = match &plan.place {
            Some(place) if context.location.is_none() => {
                saved_place_in(connection, session_id, place)?
            }
            _ => None,
        };
        let rank = (
            window.all_day,
            voices.is_empty(),
            location.is_none(),
            (window.start_utc_ms - started_utc_ms).abs(),
            plan.plan_id.0,
        );
        offers.push((rank, plan, known, voices, location));
    }
    offers.sort_by(|left, right| left.0.cmp(&right.0));
    let mut planned = HashSet::new();
    for (_, plan, known, voices, location) in offers.into_iter().take(MAX_MEETING_MAYBES) {
        planned.extend(known.iter().map(|person| person.person_id));
        let said_at_utc_ms = said_at_in(connection, &plan)?;
        let mut reasons = vec![FilingReason::Planned {
            session_id: plan.session_id,
            said_at_utc_ms,
            quote: None,
            citation: None,
        }];
        reasons.extend(voices);
        offer_in(
            connection,
            session_id,
            FilingTarget::Meeting {
                plan_id: plan.plan_id,
                title: plan.title,
                people: known,
                place: plan.place,
                location,
            },
            reasons,
            now_utc_ms,
        )?;
    }
    let linked = confirmed_people_in(connection, session_id)?;
    for person in heard {
        if planned.contains(&person.person_id) || linked.contains(&person.person_id) {
            continue;
        }
        offer_in(
            connection,
            session_id,
            FilingTarget::Person {
                person_id: person.person_id,
                name: person.name.clone(),
            },
            vec![FilingReason::VoiceMatches {
                name: person.name,
                start_offset_ns: Some(person.first_heard_ns),
            }],
            now_utc_ms,
        )?;
    }
    Ok(())
}

/// A meeting maybe with the words that made its plan, read from the plan
/// as it is now. `None` once the plan is gone with the recording it was
/// heard in.
pub(super) fn with_plan_words_in(
    connection: &Connection,
    mut suggestion: FilingSuggestion,
) -> Result<Option<FilingSuggestion>, StoreError> {
    let FilingTarget::Meeting { plan_id, .. } = &suggestion.target else {
        return Ok(Some(suggestion));
    };
    let plan = match plan_in(connection, *plan_id) {
        Ok(plan) => plan,
        Err(StoreError::NotFound) => return Ok(None),
        Err(error) => return Err(error),
    };
    for reason in &mut suggestion.reasons {
        if let FilingReason::Planned {
            quote, citation, ..
        } = reason
        {
            quote.clone_from(&plan.quote);
            citation.clone_from(&plan.citation);
        }
    }
    Ok(Some(suggestion))
}

/// The words of each meeting maybe kept on `session_id` that is not in
/// `seeded`, by suggestion: its title, its place as said and the words that
/// made its plan.
pub(super) fn kept_meeting_words_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    seeded: &HashSet<String>,
) -> Result<Vec<(String, Vec<String>)>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, target_json FROM filing_suggestions
          WHERE session_id = ?1 AND target_kind = 'meeting' AND state = 'accepted'
          ORDER BY rowid",
    )?;
    let rows = statement
        .query_map([id(session_id)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut kept = Vec::new();
    for (suggestion_id, target) in rows {
        if seeded.contains(&suggestion_id) {
            continue;
        }
        let FilingTarget::Meeting {
            plan_id,
            title,
            place,
            ..
        } = decode_json::<FilingTarget>(&target)?
        else {
            return Err(StoreError::Corrupt);
        };
        let quote = match plan_in(connection, plan_id) {
            Ok(plan) => plan.quote,
            Err(StoreError::NotFound) => None,
            Err(error) => return Err(error),
        };
        let words = std::iter::once(title).chain(place).chain(quote).collect();
        kept.push((suggestion_id, words));
    }
    Ok(kept)
}

/// The plans a recording that began at `started_utc_ms` may be: heard in a
/// kept recording before it began, neither turned down nor on the calendar,
/// with a window the start falls in or comes at most the filing slack before.
fn plans_near_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    started_utc_ms: i64,
    now_utc_ms: i64,
) -> Result<Vec<HeardPlan>, StoreError> {
    plans_in(
        connection,
        "source.phase != 'deleting' AND p.session_id != ?1
           AND COALESCE(source.started_at_utc_ms, source.created_at_utc_ms) < ?2
           AND p.start_utc_ms <= ?2 + ?3 AND p.end_utc_ms > ?2
           AND p.declined_at_utc_ms IS NULL AND p.calendar_event_id IS NULL",
        params![id(session_id), started_utc_ms, FILING_EVENT_SLACK_MS],
        now_utc_ms,
        &HashSet::new(),
    )
}

/// When the plan was said: where its words fall in the recording it was heard
/// in, or when that recording began.
fn said_at_in(connection: &Connection, plan: &HeardPlan) -> Result<i64, StoreError> {
    let began = session_row(connection, plan.session_id)?
        .started_at_utc_ms
        .unwrap_or(plan.created_at_utc_ms);
    let into = plan.citation.as_ref().map_or(0, |citation| {
        i64::try_from(citation.start_offset_ns / 1_000_000).unwrap_or(0)
    });
    Ok(began.saturating_add(into))
}

/// The people whose confirmed voice one of `heard` matches, each with when
/// it was first heard, soonest first. A voice that matches the user's names
/// no one.
fn heard_people_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    heard: &[HeardVoice],
    user_name: Option<&str>,
) -> Result<Vec<HeardPerson>, StoreError> {
    if heard.is_empty() {
        return Ok(Vec::new());
    }
    let known = known_voices_in(connection, session_id, user_name, &[])?;
    let mut people: Vec<HeardPerson> = Vec::new();
    for voice in heard {
        let Some((Some(person_id), name)) = known.owner(&voice.voice) else {
            continue;
        };
        match people
            .iter_mut()
            .find(|person| person.person_id == person_id)
        {
            Some(person) => person.first_heard_ns = person.first_heard_ns.min(voice.first_heard_ns),
            None => people.push(HeardPerson {
                person_id,
                name: name.to_owned(),
                first_heard_ns: voice.first_heard_ns,
            }),
        }
    }
    people.sort_by_key(|person| person.first_heard_ns);
    Ok(people)
}

/// The people Sona knows among a plan's names: the one person a name
/// answers to, or the one a heard voice picks out of several. The user is
/// none of them, and no one is made up.
fn plan_people(
    names: &[String],
    people: &[Person],
    heard: &[HeardPerson],
    user_name: Option<&str>,
) -> Vec<FilingPerson> {
    let mut known: Vec<FilingPerson> = Vec::new();
    for name in names.iter().filter(|name| !names_user(name, user_name)) {
        let answering = people
            .iter()
            .filter(|person| answers_to(person, name))
            .collect::<Vec<_>>();
        let person = match answering.as_slice() {
            [only] => Some(*only),
            several => heard.iter().find_map(|voice| {
                several
                    .iter()
                    .find(|person| person.id == voice.person_id)
                    .copied()
            }),
        };
        if let Some(person) =
            person.filter(|person| !known.iter().any(|known| known.person_id == person.id))
        {
            known.push(FilingPerson {
                person_id: person.id,
                name: person.display_name.clone(),
            });
        }
    }
    known
}

/// The place another recording saved under every word of `place` that says
/// which place it is: the latest such recording's, when all of them saved the
/// same place.
fn saved_place_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    place: &str,
) -> Result<Option<ConversationLocation>, StoreError> {
    let naming = words(place)
        .filter(|word| {
            word.chars().count() > 1
                && STOPWORDS.binary_search(&word.as_str()).is_err()
                && !PLACE_KINDS.contains(&word.as_str())
        })
        .collect::<Vec<_>>();
    if naming.is_empty() {
        return Ok(None);
    }
    let mut statement = connection.prepare(
        "SELECT context.location_json
           FROM conversation_context context
           JOIN meeting_sessions session ON session.id = context.session_id
          WHERE session.id != ?1 AND session.phase != 'deleting'
            AND context.location_json IS NOT NULL
          ORDER BY session.started_at_utc_ms DESC, session.id",
    )?;
    let mut rows = statement.query(params![id(session_id)])?;
    let mut found: Option<ConversationLocation> = None;
    while let Some(row) = rows.next()? {
        let saved: ConversationLocation = decode_json(&row.get::<_, String>(0)?)?;
        let name = words(&saved.name).collect::<HashSet<_>>();
        if !saved.is_valid() || !naming.iter().all(|word| name.contains(word)) {
            continue;
        }
        match &found {
            None => found = Some(saved),
            Some(latest) if distance_meters(latest, &saved) <= SAME_PLACE_METERS => {}
            Some(_) => return Ok(None),
        }
    }
    Ok(found)
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

/// Leaves a maybe waiting on a recording, or adds to one still waiting what
/// it lacks: a person a voice has since picked out, a place, a reason. The
/// plan's own title and place words are kept current. A maybe already taken
/// or turned down stays as it is.
fn offer_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    target: FilingTarget,
    reasons: Vec<FilingReason>,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    let (kind, key) = target_identity(&target)?;
    let stored = connection
        .query_row(
            "SELECT id, state, target_json, reasons_json FROM filing_suggestions
              WHERE session_id = ?1 AND target_kind = ?2 AND target_key = ?3",
            params![id(session_id), kind, key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((suggestion_id, state, stored_target, stored_reasons)) = stored else {
        return insert_suggestion_in(connection, session_id, &target, &reasons, now_utc_ms);
    };
    if state != "pending" {
        return Ok(());
    }
    let mut merged: FilingTarget = decode_json(&stored_target)?;
    let mut merged_reasons: Vec<FilingReason> = decode_json(&stored_reasons)?;
    if let (
        FilingTarget::Meeting {
            title,
            people,
            place,
            location,
            ..
        },
        FilingTarget::Meeting {
            title: plan_title,
            people: found,
            place: plan_place,
            location: found_location,
            ..
        },
    ) = (&mut merged, target)
    {
        *title = plan_title;
        *place = plan_place;
        for person in found {
            if !people
                .iter()
                .any(|known| known.person_id == person.person_id)
            {
                people.push(person);
            }
        }
        if location.is_none() {
            *location = found_location;
        }
    }
    for reason in reasons {
        if !merged_reasons
            .iter()
            .any(|known| same_evidence(known, &reason))
        {
            merged_reasons.push(reason);
        }
    }
    let target_json = encode_json(&merged)?;
    let reasons_json = encode_json(&merged_reasons)?;
    if target_json != stored_target || reasons_json != stored_reasons {
        connection.execute(
            "UPDATE filing_suggestions SET target_json = ?2, reasons_json = ?3 WHERE id = ?1",
            params![suggestion_id, target_json, reasons_json],
        )?;
    }
    Ok(())
}

/// Whether two reasons say the same thing: a voice by whose it is, and a
/// maybe's one plan however its words have changed.
fn same_evidence(left: &FilingReason, right: &FilingReason) -> bool {
    match (left, right) {
        (
            FilingReason::VoiceMatches { name: left, .. },
            FilingReason::VoiceMatches { name: right, .. },
        ) => left == right,
        (FilingReason::Planned { .. }, FilingReason::Planned { .. }) => true,
        _ => left == right,
    }
}
