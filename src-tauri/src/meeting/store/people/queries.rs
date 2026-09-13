mod artifacts;

use self::artifacts::{
    all_open_loops_in, document_summaries_in, facts_for_person_in, meeting_summary_in,
    person_meeting_headline_from_json, talk_share_average_in, PersonFacts,
};
use super::{
    all_people_in, normalized_email, people_revision_in, person_by_id_in, person_from_row,
    SCHEMA_VERSION,
};
use crate::meeting::detection::machine::{CalendarAttendee, CalendarEventSummary};
use crate::meeting::people_types::{
    organization_slug, MeetingPeopleContextResult, MeetingPersonContextRow, OpenLoopsInboxResult,
    OrganizationDetail, OrganizationDetailResult, PeopleListResult, PersonBriefingLastMeeting,
    PersonBriefingRow, PersonContextResult, PersonDetail, PersonDetailResult, PersonId,
    PersonLinkConfidence, PersonLinkSource, PersonListEntry, PersonListLastMeeting,
    PersonMeetingLink,
};
use crate::meeting::store::workflows::{
    workflow_succeeded_for_calendar_event_in, workflow_succeeded_for_session_in,
};
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::MeetingSessionId;
use crate::meeting::workflow_types::WorkflowId;
use rusqlite::{Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

impl MeetingStore {
    pub(crate) fn people_list(&self) -> Result<PeopleListResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let result = people_list_in(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn person_detail(
        &self,
        person_id: PersonId,
    ) -> Result<PersonDetailResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let result = person_detail_in(&transaction, person_id)?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn person_context(
        &self,
        person_ids: &[PersonId],
    ) -> Result<PersonContextResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let result = context_for_ids_in(&transaction, person_ids)?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn meeting_people_context(
        &self,
        meeting_id: MeetingSessionId,
    ) -> Result<MeetingPeopleContextResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let result = meeting_people_context_in(&transaction, meeting_id)?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn open_loops_inbox(
        &self,
        limit: usize,
    ) -> Result<OpenLoopsInboxResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let revision = people_revision_in(&transaction)?;
        let mut entries = Vec::new();
        for entry in all_open_loops_in(&transaction)? {
            if workflow_succeeded_for_session_in(
                &transaction,
                WorkflowId::Continuity,
                entry.meeting_id,
            )? {
                entries.push(entry);
            }
        }
        entries.truncate(limit);
        let result = OpenLoopsInboxResult {
            schema_version: SCHEMA_VERSION,
            revision,
            entries,
        };
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn calendar_person_context(
        &self,
        event: &CalendarEventSummary,
    ) -> Result<PersonContextResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let result = if workflow_succeeded_for_calendar_event_in(&transaction, &event.event_key)? {
            calendar_context_in(&transaction, &event.attendees)?
        } else {
            PersonContextResult {
                schema_version: SCHEMA_VERSION,
                revision: people_revision_in(&transaction)?,
                rows: Vec::new(),
            }
        };
        transaction.commit()?;
        Ok(result)
    }

    /// Which of these calendar addresses Sona already knows a person by.
    ///
    /// Keyed by the normalized address rather than by person, because the
    /// caller holds attendees and needs to answer "is this chip a link?" one
    /// attendee at a time. Addresses nobody is known by are simply absent, so
    /// an unlinked attendee is a missing key rather than a null.
    ///
    /// Unlike `calendar_person_context` this is not gated on the briefing
    /// workflow: a name that is already a person page is a fact about the
    /// address book, not an inference drawn from the meeting, and hiding the
    /// link until a workflow has run would make the same chip navigable on one
    /// row and dead on the next.
    pub(crate) fn person_ids_for_calendar_emails(
        &self,
        emails: &[String],
    ) -> Result<HashMap<String, PersonId>, StoreError> {
        let connection = self.connection()?;
        let wanted = emails
            .iter()
            .map(|email| normalized_email(email))
            .collect::<HashSet<_>>();
        if wanted.is_empty() {
            return Ok(HashMap::new());
        }
        let mut links = HashMap::new();
        for person in all_people_in(&connection)? {
            for email in &person.calendar_emails {
                let email = normalized_email(email);
                if wanted.contains(&email) {
                    links.insert(email, person.id);
                }
            }
        }
        Ok(links)
    }

    /// Cached organizations for the requested people; unknown and public-mail
    /// domains are absent.
    pub(crate) fn organizations_for_person_ids(
        &self,
        person_ids: &[PersonId],
    ) -> Result<HashMap<PersonId, String>, StoreError> {
        if person_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT organization
               FROM persons
              WHERE id = ?1 AND organization IS NOT NULL",
        )?;
        let mut organizations = HashMap::with_capacity(person_ids.len());
        for person_id in person_ids {
            let organization = statement
                .query_row([person_id.uuid().to_string()], |row| row.get(0))
                .optional()?;
            if let Some(organization) = organization {
                organizations.insert(*person_id, organization);
            }
        }
        Ok(organizations)
    }

    /// One organization page: its people, and what they collectively left open.
    ///
    /// `slug` is matched through [`organization_slug`] on both sides, so the
    /// label a person's header shows resolves here as readily as the slug a
    /// `sona://organization/<slug>` link carries. A slug nobody carries is
    /// [`StoreError::NotFound`] rather than an empty page: an organization
    /// exists only as the people on it.
    pub(crate) fn organization_detail(
        &self,
        slug: &str,
    ) -> Result<OrganizationDetailResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let result = organization_detail_in(&transaction, slug)?;
        transaction.commit()?;
        Ok(result)
    }

    /// The people confirmed to have been in one meeting.
    ///
    /// The artifact pass reads this to know whose relationship paragraph the
    /// meeting that just landed changed. Confirmed only: a suggested link is a
    /// guess, and writing a summary from one would put a guess under a name.
    pub(crate) fn person_ids_for_meeting(
        &self,
        meeting_id: MeetingSessionId,
    ) -> Result<Vec<PersonId>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT p.id
               FROM meeting_person_links l
               JOIN persons p ON p.id = l.person_id
              WHERE l.meeting_id = ?1 AND l.confidence = 'confirmed'
              ORDER BY p.display_name COLLATE NOCASE, p.id",
        )?;
        let ids = statement
            .query_map([meeting_id.uuid().to_string()], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                Uuid::parse_str(&id)
                    .map(PersonId)
                    .map_err(|_| StoreError::Corrupt)
            })
            .collect()
    }
}

fn people_list_in(connection: &Connection) -> Result<PeopleListResult, StoreError> {
    let revision = people_revision_in(connection)?;
    let mut statement = connection.prepare(
        "SELECT p.id, p.display_name, p.aliases_json, p.calendar_emails_json,
                p.organization, p.created_at_utc_ms, p.updated_at_utc_ms,
                p.summary, p.summary_generated_at_utc_ms, p.summary_model_id,
                SUM(CASE WHEN l.confidence = 'confirmed' THEN 1 ELSE 0 END),
                lm.at_utc_ms,
                SUM(CASE WHEN l.confidence = 'suggested' THEN 1 ELSE 0 END),
                COALESCE(GROUP_CONCAT(DISTINCT l.source), ''),
                lm.id,
                lm.title,
                (SELECT a.content_json
                   FROM meeting_artifact_revisions a
                  WHERE a.session_id = lm.id AND a.state = 'current'
                    AND a.content_json IS NOT NULL
                  ORDER BY a.generated_at_utc_ms DESC LIMIT 1)
           FROM persons p
           LEFT JOIN meeting_person_links l ON l.person_id = p.id
           LEFT JOIN (
                SELECT m.id, m.title,
                       COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) AS at_utc_ms
                  FROM meeting_sessions m
           ) lm ON lm.id = (
                SELECT l2.meeting_id
                  FROM meeting_person_links l2
                  JOIN meeting_sessions m2 ON m2.id = l2.meeting_id
                 WHERE l2.person_id = p.id AND l2.confidence = 'confirmed'
                 ORDER BY COALESCE(m2.started_at_utc_ms, m2.created_at_utc_ms) DESC,
                          m2.id DESC
                 LIMIT 1
           )
          GROUP BY p.id
          ORDER BY p.display_name COLLATE NOCASE, p.id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                person_from_row(row)?,
                row.get::<_, i64>(10)?,
                row.get::<_, Option<i64>>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, Option<String>>(16)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let entries = rows
        .into_iter()
        .map(
            |(
                person,
                confirmed_count,
                last_meeting_at_utc_ms,
                suggested_count,
                source_csv,
                meeting_id,
                meeting_title,
                content_json,
            )| {
                let confirmed_count =
                    u64::try_from(confirmed_count).map_err(|_| StoreError::Corrupt)?;
                let suggested_count =
                    u64::try_from(suggested_count).map_err(|_| StoreError::Corrupt)?;
                let evidence_sources = evidence_sources_from_csv(&source_csv)?;
                let last_meeting = match (meeting_id, meeting_title, last_meeting_at_utc_ms) {
                    (Some(meeting_id), Some(title), Some(at_ms)) => {
                        let session_id = Uuid::parse_str(&meeting_id)
                            .map(MeetingSessionId::from_uuid)
                            .map_err(|_| StoreError::Corrupt)?;
                        Some(PersonListLastMeeting {
                            session_id,
                            title,
                            at_ms,
                            headline: person_meeting_headline_from_json(content_json.as_deref())?,
                        })
                    }
                    (None, None, None) => None,
                    _ => return Err(StoreError::Corrupt),
                };
                Ok(PersonListEntry {
                    person,
                    meetings_count: confirmed_count,
                    last_meeting_at_utc_ms,
                    suggested_count,
                    evidence_sources,
                    confirmed_count,
                    last_meeting,
                })
            },
        )
        .collect::<Result<Vec<_>, StoreError>>()?;
    Ok(PeopleListResult {
        schema_version: SCHEMA_VERSION,
        revision,
        entries,
    })
}

fn person_detail_in(
    connection: &Connection,
    person_id: PersonId,
) -> Result<PersonDetailResult, StoreError> {
    let revision = people_revision_in(connection)?;
    let person = person_by_id_in(connection, person_id)?;
    let mut facts = facts_for_person_in(connection, &person)?;
    gate_continuity_facts_in(connection, &mut facts)?;
    let links = links_for_person_in(connection, person_id)?;
    let documents = document_summaries_in(connection, person_id)?;
    let talk_share_avg_permille = talk_share_average_in(connection, &person)?;
    Ok(PersonDetailResult {
        schema_version: SCHEMA_VERSION,
        revision,
        detail: PersonDetail {
            person,
            links,
            open_loops: facts.open_loops,
            commitments: facts.commitments,
            talk_share_avg_permille,
            documents,
        },
    })
}

/// How many meetings and how many open loops an organization page carries.
///
/// An organization is a handful of people, and its page answers "where does
/// this account stand" rather than "list everything". The number is the plane's
/// own page size, because these are the same rows out of the same corpus.
const ORGANIZATION_PAGE_ROWS: usize = 25;

fn organization_detail_in(
    connection: &Connection,
    slug: &str,
) -> Result<OrganizationDetailResult, StoreError> {
    let slug = organization_slug(slug);
    let list = people_list_in(connection)?;
    let people = list
        .entries
        .into_iter()
        .filter(|entry| {
            entry
                .person
                .organization
                .as_deref()
                .is_some_and(|organization| organization_slug(organization) == slug)
        })
        .collect::<Vec<_>>();
    let name = people
        .first()
        .and_then(|entry| entry.person.organization.clone())
        .ok_or(StoreError::NotFound)?;

    let mut recent_meetings = Vec::new();
    let mut seen_meetings = HashSet::new();
    let mut open_loops = Vec::new();
    for entry in &people {
        let mut facts = facts_for_person_in(connection, &entry.person)?;
        gate_continuity_facts_in(connection, &mut facts)?;
        for meeting in facts.meetings {
            // Two people from one organization in one meeting is the ordinary
            // case, and the meeting happened once.
            if seen_meetings.insert(meeting.id) {
                recent_meetings.push(meeting);
            }
        }
        // Already open-only: `facts_for_person_in` keeps a loop row on a
        // person's page only while it is open.
        open_loops.extend(facts.open_loops);
    }
    recent_meetings.sort_by(|left, right| {
        right
            .at_utc_ms
            .cmp(&left.at_utc_ms)
            .then_with(|| left.id.uuid().cmp(&right.id.uuid()))
    });
    recent_meetings.truncate(ORGANIZATION_PAGE_ROWS);
    open_loops.sort_by(|left, right| {
        right
            .at_utc_ms
            .cmp(&left.at_utc_ms)
            .then_with(|| left.loop_id.as_str().cmp(right.loop_id.as_str()))
    });
    open_loops.truncate(ORGANIZATION_PAGE_ROWS);

    Ok(OrganizationDetailResult {
        schema_version: SCHEMA_VERSION,
        revision: list.revision,
        detail: OrganizationDetail {
            name,
            people,
            recent_meetings,
            open_loops,
        },
    })
}

fn meeting_people_context_in(
    connection: &Connection,
    meeting_id: MeetingSessionId,
) -> Result<MeetingPeopleContextResult, StoreError> {
    let revision = people_revision_in(connection)?;
    let mut statement = connection.prepare(
        "SELECT p.id, l.source
           FROM meeting_person_links l
           JOIN persons p ON p.id = l.person_id
          WHERE l.meeting_id = ?1 AND l.confidence = 'confirmed'
          ORDER BY p.display_name COLLATE NOCASE, p.id",
    )?;
    let linked = statement
        .query_map([meeting_id.uuid().to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let mut rows = Vec::with_capacity(linked.len());
    for (person_id, evidence_source) in linked {
        let person_id = Uuid::parse_str(&person_id)
            .map(PersonId)
            .map_err(|_| StoreError::Corrupt)?;
        let person = person_by_id_in(connection, person_id)?;
        let mut facts = facts_for_person_in(connection, &person)?;
        gate_continuity_facts_in(connection, &mut facts)?;
        // The facts list runs newest first with a fixed tie-break, so the
        // meetings before this one are the ones after it in that list. A
        // later meeting must not pass for an earlier one: the oldest of
        // three has nobody before it, whatever came after.
        let prior = facts
            .meetings
            .iter()
            .position(|meeting| meeting.id == meeting_id)
            .map_or(&[][..], |position| &facts.meetings[position + 1..]);
        let prior_meetings = u64::try_from(prior.len()).map_err(|_| StoreError::Corrupt)?;
        let last_prior_meeting = prior.first().map(|meeting| PersonBriefingLastMeeting {
            id: meeting.id,
            title: meeting.title.clone(),
            at_utc_ms: meeting.at_utc_ms,
            headline: meeting.headline.clone(),
        });
        let top_open_loop = facts
            .open_loops
            .iter()
            .find(|open_loop| {
                prior
                    .iter()
                    .any(|meeting| meeting.id == open_loop.meeting_id)
            })
            .cloned();
        rows.push(MeetingPersonContextRow {
            person_id,
            display_name: person.display_name,
            evidence_source: source_from_db(&evidence_source)?,
            prior_meetings,
            last_prior_meeting,
            top_open_loop,
        });
    }
    Ok(MeetingPeopleContextResult {
        schema_version: SCHEMA_VERSION,
        revision,
        rows,
    })
}

fn evidence_sources_from_csv(value: &str) -> Result<Vec<PersonLinkSource>, StoreError> {
    let mut sources = value
        .split(',')
        .filter(|source| !source.is_empty())
        .map(source_from_db)
        .collect::<Result<Vec<_>, _>>()?;
    sources.sort_by_key(|source| match source {
        PersonLinkSource::Calendar => 0,
        PersonLinkSource::Speaker => 1,
        PersonLinkSource::Title => 2,
        PersonLinkSource::Manual => 3,
    });
    sources.dedup();
    Ok(sources)
}

pub(in crate::meeting::store) fn calendar_context_in(
    connection: &Connection,
    attendees: &[CalendarAttendee],
) -> Result<PersonContextResult, StoreError> {
    let people = all_people_in(connection)?;
    let emails = attendees
        .iter()
        .filter(|attendee| !attendee.is_self)
        .filter_map(|attendee| attendee.email.as_deref())
        .map(normalized_email)
        .collect::<HashSet<_>>();
    let person_ids = people
        .into_iter()
        .filter(|person| {
            person
                .calendar_emails
                .iter()
                .map(|email| normalized_email(email))
                .any(|email| emails.contains(&email))
        })
        .map(|person| person.id)
        .collect::<Vec<_>>();
    context_for_ids_in(connection, &person_ids)
}

pub(in crate::meeting::store) fn continuity_summary_in(
    connection: &Connection,
    meeting_id: MeetingSessionId,
) -> Result<(u64, usize), StoreError> {
    let mut statement = connection.prepare(
        "SELECT person_id FROM meeting_person_links
          WHERE meeting_id = ?1 AND confidence = 'confirmed'",
    )?;
    let ids = statement
        .query_map([meeting_id.uuid().to_string()], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut largest_series = 0;
    let mut carried = 0;
    for id in ids {
        let id = Uuid::parse_str(&id).map_err(|_| StoreError::Corrupt)?;
        let person = person_by_id_in(connection, PersonId(id))?;
        let facts = facts_for_person_in(connection, &person)?;
        largest_series = largest_series.max(
            facts
                .meetings
                .first()
                .map_or(0, |meeting| meeting.series_number),
        );
        carried += facts
            .open_loops
            .iter()
            .filter(|open_loop| {
                open_loop.meeting_id == meeting_id && open_loop.carried_since_at_utc_ms.is_some()
            })
            .count();
    }
    Ok((largest_series, carried))
}

fn context_for_ids_in(
    connection: &Connection,
    person_ids: &[PersonId],
) -> Result<PersonContextResult, StoreError> {
    let revision = people_revision_in(connection)?;
    let mut rows = Vec::with_capacity(person_ids.len());
    for person_id in person_ids {
        let person = person_by_id_in(connection, *person_id)?;
        let mut facts = facts_for_person_in(connection, &person)?;
        gate_continuity_facts_in(connection, &mut facts)?;
        let last = facts
            .meetings
            .first()
            .map(|meeting| PersonBriefingLastMeeting {
                id: meeting.id,
                title: meeting.title.clone(),
                at_utc_ms: meeting.at_utc_ms,
                headline: meeting.headline.clone(),
            });
        let meetings_count =
            u64::try_from(facts.meetings.len()).map_err(|_| StoreError::Corrupt)?;
        facts.open_loops.truncate(3);
        facts.commitments.truncate(3);
        rows.push(PersonBriefingRow {
            person_id: person.id,
            display_name: person.display_name,
            meetings_count,
            last,
            open_loops: facts.open_loops,
            commitments: facts.commitments,
        });
    }
    Ok(PersonContextResult {
        schema_version: SCHEMA_VERSION,
        revision,
        rows,
    })
}

fn gate_continuity_facts_in(
    connection: &Connection,
    facts: &mut PersonFacts,
) -> Result<(), StoreError> {
    for meeting in &mut facts.meetings {
        if !workflow_succeeded_for_session_in(connection, WorkflowId::Continuity, meeting.id)? {
            meeting.series_number = 1;
        }
    }
    for open_loop in &mut facts.open_loops {
        if !workflow_succeeded_for_session_in(
            connection,
            WorkflowId::Continuity,
            open_loop.meeting_id,
        )? {
            open_loop.carried_since_at_utc_ms = None;
        }
    }
    Ok(())
}

fn links_for_person_in(
    connection: &Connection,
    person_id: PersonId,
) -> Result<Vec<PersonMeetingLink>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT meeting_id, source, confidence
           FROM meeting_person_links
          WHERE person_id = ?1
          ORDER BY created_at_utc_ms DESC, meeting_id DESC",
    )?;
    let rows = statement
        .query_map([person_id.uuid().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let continuity_enabled =
        super::super::workflows::workflow_enabled_in(connection, WorkflowId::Continuity)?;
    rows.into_iter()
        .map(|(meeting_id, source, confidence)| {
            let meeting_id = Uuid::parse_str(&meeting_id).map_err(|_| StoreError::Corrupt)?;
            let meeting_id = MeetingSessionId::from_uuid(meeting_id);
            let mut meeting = meeting_summary_in(connection, meeting_id, person_id)?;
            if !continuity_enabled
                || !workflow_succeeded_for_session_in(
                    connection,
                    WorkflowId::Continuity,
                    meeting_id,
                )?
            {
                meeting.series_number = 1;
            }
            Ok(PersonMeetingLink {
                meeting,
                source: source_from_db(&source)?,
                confidence: confidence_from_db(&confidence)?,
            })
        })
        .collect()
}

fn source_from_db(value: &str) -> Result<PersonLinkSource, StoreError> {
    match value {
        "calendar" => Ok(PersonLinkSource::Calendar),
        "speaker" => Ok(PersonLinkSource::Speaker),
        "title" => Ok(PersonLinkSource::Title),
        "manual" => Ok(PersonLinkSource::Manual),
        _ => Err(StoreError::Corrupt),
    }
}

fn confidence_from_db(value: &str) -> Result<PersonLinkConfidence, StoreError> {
    match value {
        "confirmed" => Ok(PersonLinkConfidence::Confirmed),
        "suggested" => Ok(PersonLinkConfidence::Suggested),
        _ => Err(StoreError::Corrupt),
    }
}
