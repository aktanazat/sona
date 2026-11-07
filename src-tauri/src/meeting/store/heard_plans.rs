use super::people::{identity_names, normalized, person_by_id_in};
use super::{
    decode_json, encode_json, from_i64, id, parse_uuid, session_row, to_i64, utc_now_ms,
    MeetingStore, StoreError,
};
use crate::meeting::heard_plan_types::{
    HeardPlan, HeardPlanDraft, HeardPlanEdit, HeardPlanStatus, HeardPlanWindow,
};
use crate::meeting::people_types::{Person, PersonId};
use crate::meeting::types::{
    ArtifactCitation, HeardPlanId, MeetingPhase, MeetingSessionId, TranscriptRevisionId,
    TranscriptSegmentId,
};
use rusqlite::{params, Connection, OptionalExtension, Params, TransactionBehavior};
use std::collections::HashSet;

const MAX_TITLE_CHARS: usize = 120;
const MAX_PLACE_CHARS: usize = 200;
const MAX_NAME_CHARS: usize = 80;
const MAX_PEOPLE: usize = 12;

/// Every column a plan is read from. The last is when the meeting it was
/// heard in began: only a recording after that can be the plan happening.
const PLAN_COLUMNS: &str = "p.plan_id, p.session_id, p.title, p.people_json, p.when_text,
    p.start_utc_ms, p.end_utc_ms, p.all_day, p.place, p.quote, p.segment_id,
    p.start_offset_ns, p.end_offset_ns, p.calendar_event_id IS NOT NULL,
    p.declined_at_utc_ms IS NOT NULL, p.edited_at_utc_ms IS NOT NULL, p.created_at_utc_ms,
    p.updated_at_utc_ms, COALESCE(source.started_at_utc_ms, source.created_at_utc_ms)";

impl MeetingStore {
    /// The transcript a plan reading of `session_id` would read, and when the
    /// meeting began. `None` when the last reading read this transcript, or
    /// there is no transcript to read.
    pub(crate) fn heard_plans_unread(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Option<(TranscriptRevisionId, i64)>, StoreError> {
        let connection = self.connection()?;
        let session = session_row(&connection, session_id)?;
        if session.phase == MeetingPhase::Deleting || session.transcript_purged_at_utc_ms.is_some()
        {
            return Ok(None);
        }
        let (revision, began, read): (Option<String>, i64, Option<String>) = connection.query_row(
            "SELECT session.current_transcript_revision_id,
                    COALESCE(session.started_at_utc_ms, session.created_at_utc_ms),
                    reading.transcript_revision_id
               FROM meeting_sessions session
               LEFT JOIN meeting_heard_plan_readings reading ON reading.session_id = session.id
              WHERE session.id = ?1",
            params![id(session_id)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let Some(revision) = revision.filter(|revision| read.as_ref() != Some(revision)) else {
            return Ok(None);
        };
        Ok(Some((
            TranscriptRevisionId::from_uuid(parse_uuid(&revision)?),
            began,
        )))
    }

    /// Keep what one reading of `session_id` found, and mark
    /// `transcript_revision_id` read. The plans the operator has not touched
    /// are replaced; one added to the calendar, answered "Not now" or edited
    /// stays, and a found plan quoting the same words as one that stays is
    /// dropped. Returns whether the meeting's plans changed.
    pub(crate) fn replace_heard_plans(
        &self,
        session_id: MeetingSessionId,
        transcript_revision_id: TranscriptRevisionId,
        found: &[HeardPlanDraft],
        now_utc_ms: i64,
    ) -> Result<bool, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if session_row(&transaction, session_id)?.phase == MeetingPhase::Deleting {
            return Err(StoreError::NotFound);
        }
        super::transcript_retention::require_retained_transcript_in(&transaction, session_id)?;
        let current: Option<String> = transaction.query_row(
            "SELECT current_transcript_revision_id FROM meeting_sessions WHERE id = ?1",
            params![id(session_id)],
            |row| row.get(0),
        )?;
        if current != Some(id(transcript_revision_id)) {
            return Err(StoreError::Conflict);
        }
        let removed = transaction.execute(
            "DELETE FROM meeting_heard_plans
              WHERE session_id = ?1 AND calendar_event_id IS NULL
                AND declined_at_utc_ms IS NULL AND edited_at_utc_ms IS NULL",
            params![id(session_id)],
        )?;
        let mut quoted = {
            let mut statement = transaction.prepare(
                "SELECT quote FROM meeting_heard_plans WHERE session_id = ?1 AND quote IS NOT NULL",
            )?;
            let quotes = statement
                .query_map(params![id(session_id)], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            quotes
                .iter()
                .map(|quote| folded(quote))
                .collect::<HashSet<_>>()
        };
        let mut added = 0_usize;
        for draft in found {
            if !quoted.insert(folded(&draft.quote)) {
                continue;
            }
            let window = draft.window;
            transaction.execute(
                "INSERT INTO meeting_heard_plans (
                    plan_id, session_id, title, people_json, when_text, start_utc_ms,
                    end_utc_ms, all_day, place, quote, segment_id, start_offset_ns,
                    end_offset_ns, created_at_utc_ms, updated_at_utc_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
                params![
                    HeardPlanId::new().uuid().to_string(),
                    id(session_id),
                    draft.title,
                    encode_json(&draft.people)?,
                    draft.when_text,
                    window.map(|window| window.start_utc_ms),
                    window.map(|window| window.end_utc_ms),
                    window.is_some_and(|window| window.all_day),
                    draft.place,
                    draft.quote,
                    id(draft.citation.segment_id),
                    to_i64(draft.citation.start_offset_ns)?,
                    to_i64(draft.citation.end_offset_ns)?,
                    now_utc_ms,
                ],
            )?;
            added += 1;
        }
        transaction.execute(
            "INSERT INTO meeting_heard_plan_readings (session_id, transcript_revision_id, read_at_utc_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET
                transcript_revision_id = excluded.transcript_revision_id,
                read_at_utc_ms = excluded.read_at_utc_ms",
            params![id(session_id), id(transcript_revision_id), now_utc_ms],
        )?;
        transaction.commit()?;
        Ok(removed > 0 || added > 0)
    }

    /// The plans heard in one meeting, in the order they were said.
    pub(crate) fn heard_plans(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<HeardPlan>, StoreError> {
        let connection = self.connection()?;
        if session_row(&connection, session_id)?.phase == MeetingPhase::Deleting {
            return Err(StoreError::NotFound);
        }
        plans_in(
            &connection,
            "p.session_id = ?1",
            params![id(session_id)],
            utc_now_ms(),
            &HashSet::new(),
        )
    }

    /// Every plan heard, newest meeting first. A meeting in `hidden` is
    /// neither where a plan was heard nor where one happened.
    pub(crate) fn all_heard_plans_without(
        &self,
        hidden: &HashSet<MeetingSessionId>,
    ) -> Result<Vec<HeardPlan>, StoreError> {
        let connection = self.connection()?;
        plans_in(
            &connection,
            "source.phase != 'deleting'",
            [],
            utc_now_ms(),
            hidden,
        )
    }

    pub(crate) fn heard_plan(&self, plan_id: HeardPlanId) -> Result<HeardPlan, StoreError> {
        let connection = self.connection()?;
        plan_in(&connection, plan_id)
    }

    /// The operator's "Not now": the plan is kept as heard and stops asking.
    pub(crate) fn heard_plan_not_now(&self, plan_id: HeardPlanId) -> Result<HeardPlan, StoreError> {
        let connection = self.connection()?;
        let now = utc_now_ms();
        let changed = connection.execute(
            "UPDATE meeting_heard_plans
                SET declined_at_utc_ms = COALESCE(declined_at_utc_ms, ?2), updated_at_utc_ms = ?2
              WHERE plan_id = ?1",
            params![plan_id.uuid().to_string(), now],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        plan_in(&connection, plan_id)
    }

    /// The operator's correction of what, when, who and where. A plan on the
    /// calendar is refused, so the entry and the plan never disagree: it is
    /// taken off the calendar first.
    pub(crate) fn heard_plan_edit(&self, edit: HeardPlanEdit) -> Result<HeardPlan, StoreError> {
        let title = squeezed(&edit.title);
        let place = edit
            .place
            .as_deref()
            .map(squeezed)
            .filter(|place| !place.is_empty());
        let names = edit
            .people
            .iter()
            .map(|name| squeezed(name))
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        if title.is_empty()
            || unfit(&title, MAX_TITLE_CHARS)
            || place
                .as_deref()
                .is_some_and(|place| unfit(place, MAX_PLACE_CHARS))
            || names.len() > MAX_PEOPLE
            || names.iter().any(|name| unfit(name, MAX_NAME_CHARS))
            || edit.start_utc_ms >= edit.end_utc_ms
        {
            return Err(StoreError::Invalid);
        }
        let connection = self.connection()?;
        let now = utc_now_ms();
        let changed = connection.execute(
            "UPDATE meeting_heard_plans
                SET title = ?2, people_json = ?3, start_utc_ms = ?4, end_utc_ms = ?5,
                    all_day = 0, place = ?6, edited_at_utc_ms = ?7, updated_at_utc_ms = ?7
              WHERE plan_id = ?1 AND calendar_event_id IS NULL",
            params![
                edit.plan_id.uuid().to_string(),
                title,
                encode_json(&names)?,
                edit.start_utc_ms,
                edit.end_utc_ms,
                place,
                now
            ],
        )?;
        if changed == 0 {
            plan_in(&connection, edit.plan_id)?;
            return Err(StoreError::Conflict);
        }
        plan_in(&connection, edit.plan_id)
    }

    /// The calendar entry made for a plan, for taking it back off.
    pub(crate) fn heard_plan_calendar_event(
        &self,
        plan_id: HeardPlanId,
    ) -> Result<Option<String>, StoreError> {
        self.connection()?
            .query_row(
                "SELECT calendar_event_id FROM meeting_heard_plans WHERE plan_id = ?1",
                params![plan_id.uuid().to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    /// Record the calendar entry made for a plan, or with `None` that it was
    /// taken back off. Refuses a second entry for a plan already on the
    /// calendar.
    pub(crate) fn set_heard_plan_calendar_event(
        &self,
        plan_id: HeardPlanId,
        event_id: Option<&str>,
    ) -> Result<HeardPlan, StoreError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE meeting_heard_plans SET calendar_event_id = ?2, updated_at_utc_ms = ?3
              WHERE plan_id = ?1 AND (?2 IS NULL OR calendar_event_id IS NULL)",
            params![plan_id.uuid().to_string(), event_id, utc_now_ms()],
        )?;
        if changed == 0 {
            plan_in(&connection, plan_id)?;
            return Err(StoreError::Conflict);
        }
        plan_in(&connection, plan_id)
    }
}

/// The first recording after the meeting a plan was heard in that overlaps
/// the plan's window, with someone confirmed there who answers to one of the
/// plan's names.
fn happened_in(
    connection: &Connection,
    session_id: &str,
    began_utc_ms: i64,
    window: HeardPlanWindow,
    names: &[String],
    hidden: &HashSet<MeetingSessionId>,
) -> Result<Option<MeetingSessionId>, StoreError> {
    if names.is_empty() {
        return Ok(None);
    }
    let mut statement = connection.prepare_cached(
        "SELECT later.id, link.person_id
           FROM meeting_sessions later
           JOIN meeting_person_links link
             ON link.meeting_id = later.id AND link.confidence = 'confirmed'
          WHERE later.id != ?1 AND later.phase != 'deleting'
            AND later.started_at_utc_ms > ?2 AND later.started_at_utc_ms < ?4
            AND COALESCE(later.ended_at_utc_ms, later.started_at_utc_ms) >= ?3
          ORDER BY later.started_at_utc_ms, later.id",
    )?;
    let confirmed = statement
        .query_map(
            params![
                session_id,
                began_utc_ms,
                window.start_utc_ms,
                window.end_utc_ms
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    for (later, person_id) in confirmed {
        let later = MeetingSessionId::from_uuid(parse_uuid(&later)?);
        if hidden.contains(&later) {
            continue;
        }
        let person = person_by_id_in(connection, PersonId::from_uuid(parse_uuid(&person_id)?))?;
        if names.iter().any(|name| answers_to(&person, name)) {
            return Ok(Some(later));
        }
    }
    Ok(None)
}

/// Whether `person` answers to `name`: by a full name or an alias, or by the
/// first word of one, so "Stephen" is Stephen Park.
pub(super) fn answers_to(person: &Person, name: &str) -> bool {
    let said = normalized(name);
    !said.is_empty()
        && identity_names(person).any(|known| {
            normalized(known) == said
                || known
                    .split_whitespace()
                    .next()
                    .is_some_and(|first| normalized(first) == said)
        })
}

pub(super) fn plan_in(
    connection: &Connection,
    plan_id: HeardPlanId,
) -> Result<HeardPlan, StoreError> {
    plans_in(
        connection,
        "p.plan_id = ?1",
        params![plan_id.uuid().to_string()],
        utc_now_ms(),
        &HashSet::new(),
    )?
    .pop()
    .ok_or(StoreError::NotFound)
}

pub(super) fn plans_in(
    connection: &Connection,
    filter: &str,
    arguments: impl Params,
    now_utc_ms: i64,
    hidden: &HashSet<MeetingSessionId>,
) -> Result<Vec<HeardPlan>, StoreError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {PLAN_COLUMNS}
           FROM meeting_heard_plans p JOIN meeting_sessions source ON source.id = p.session_id
          WHERE {filter}
          ORDER BY COALESCE(source.started_at_utc_ms, source.created_at_utc_ms) DESC, source.id,
                   p.start_offset_ns, p.plan_id"
    ))?;
    let rows = statement
        .query_map(arguments, |row| {
            Ok(PlanRow {
                plan_id: row.get(0)?,
                session_id: row.get(1)?,
                title: row.get(2)?,
                people_json: row.get(3)?,
                when_text: row.get(4)?,
                start_utc_ms: row.get(5)?,
                end_utc_ms: row.get(6)?,
                all_day: row.get(7)?,
                place: row.get(8)?,
                quote: row.get(9)?,
                segment_id: row.get(10)?,
                start_offset_ns: row.get(11)?,
                end_offset_ns: row.get(12)?,
                on_calendar: row.get(13)?,
                declined: row.get(14)?,
                edited: row.get(15)?,
                created_at_utc_ms: row.get(16)?,
                updated_at_utc_ms: row.get(17)?,
                began_utc_ms: row.get(18)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut plans = Vec::with_capacity(rows.len());
    for row in rows {
        if !hidden.contains(&MeetingSessionId::from_uuid(parse_uuid(&row.session_id)?)) {
            plans.push(row.plan(connection, now_utc_ms, hidden)?);
        }
    }
    Ok(plans)
}

struct PlanRow {
    plan_id: String,
    session_id: String,
    title: String,
    people_json: String,
    when_text: String,
    start_utc_ms: Option<i64>,
    end_utc_ms: Option<i64>,
    all_day: bool,
    place: Option<String>,
    quote: Option<String>,
    segment_id: Option<String>,
    start_offset_ns: Option<i64>,
    end_offset_ns: Option<i64>,
    on_calendar: bool,
    declined: bool,
    edited: bool,
    created_at_utc_ms: i64,
    updated_at_utc_ms: i64,
    began_utc_ms: i64,
}

impl PlanRow {
    fn plan(
        self,
        connection: &Connection,
        now_utc_ms: i64,
        hidden: &HashSet<MeetingSessionId>,
    ) -> Result<HeardPlan, StoreError> {
        let window = match (self.start_utc_ms, self.end_utc_ms) {
            (Some(start_utc_ms), Some(end_utc_ms)) => Some(HeardPlanWindow {
                start_utc_ms,
                end_utc_ms,
                all_day: self.all_day,
            }),
            (None, None) => None,
            _ => return Err(StoreError::Corrupt),
        };
        let citation = match (self.segment_id, self.start_offset_ns, self.end_offset_ns) {
            (Some(segment_id), Some(start), Some(end)) => Some(ArtifactCitation {
                segment_id: TranscriptSegmentId::from_uuid(parse_uuid(&segment_id)?),
                start_offset_ns: from_i64(start)?,
                end_offset_ns: from_i64(end)?,
            }),
            (None, None, None) => None,
            _ => return Err(StoreError::Corrupt),
        };
        let people: Vec<String> = decode_json(&self.people_json)?;
        let happened_session_id = match window {
            Some(window) => happened_in(
                connection,
                &self.session_id,
                self.began_utc_ms,
                window,
                &people,
                hidden,
            )?,
            None => None,
        };
        let status = if happened_session_id.is_some() {
            HeardPlanStatus::Happened
        } else if window.is_some_and(|window| window.end_utc_ms <= now_utc_ms) {
            HeardPlanStatus::NoRecord
        } else if self.on_calendar {
            HeardPlanStatus::OnCalendar
        } else {
            HeardPlanStatus::Heard
        };
        Ok(HeardPlan {
            plan_id: HeardPlanId::from_uuid(parse_uuid(&self.plan_id)?),
            session_id: MeetingSessionId::from_uuid(parse_uuid(&self.session_id)?),
            title: self.title,
            people,
            when_text: self.when_text,
            window,
            place: self.place,
            quote: self.quote,
            citation,
            status,
            on_calendar: self.on_calendar,
            declined: self.declined,
            edited: self.edited,
            happened_session_id,
            created_at_utc_ms: self.created_at_utc_ms,
            updated_at_utc_ms: self.updated_at_utc_ms,
        })
    }
}

fn folded(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn squeezed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn unfit(text: &str, max_chars: usize) -> bool {
    text.chars().count() > max_chars || text.chars().any(char::is_control)
}
