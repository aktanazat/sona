//! D28: today and the next seven days, joined to what each series remembers.
//!
//! Two halves, deliberately separate. `upcoming_window` and `upcoming_rows` are
//! pure: given a clock, a list of occurrences, the series records and the
//! address book's answer about each attendee, they produce exactly the rows the
//! pane draws — no calendar, no database, no platform. The manager method under
//! them is the wiring: it turns a window into occurrences, collects the two
//! lookups those rows need in one read each, and hands both to the pure half.
//!
//! Rows are not grouped into days here. The frontend already owns local-day
//! grouping and its headings (`lib/utils/localDay`), shared by the dictation log
//! and meeting history; a second bucketer on this side would be the same rule
//! written twice, and the two would drift the first time someone changed a
//! heading. What this side does own is where the *window* is cut, because the
//! calendar query needs two instants and only the host knows the timezone.

use super::detection::calendar::{CalendarAccess, CalendarOccurrence};
use super::series_types::MeetingSeriesPreferences;
use super::session::MeetingSessionManager;
use super::types::MeetingCommandError;
use super::upcoming_types::{
    MeetingUpcomingAttendee, MeetingUpcomingEvents, MeetingUpcomingRow, MeetingUpcomingSeries,
};
use super::workflow_engine::map_store_error;
use crate::analytics::local_midnight;
use crate::meeting::people_types::PersonId;
use chrono::{DateTime, Days, Local};
use std::collections::HashMap;

/// The furthest ahead the section will ever look. Seven days is the D28
/// default; the ceiling exists so a caller cannot turn a home-screen read into
/// a year-long calendar scan.
pub const MAX_UPCOMING_DAYS: u32 = 30;

/// How far ahead the section looks by default: today and the next seven days.
/// D28's window, and what `sona --upcoming` reads, so a script and the pane
/// answer the same question.
pub const UPCOMING_DEFAULT_DAYS: u32 = 7;

/// `[start, end)` in UTC milliseconds, covering today plus `days` more local
/// calendar days.
///
/// Cut on local midnights rather than on `now + 7*24h`, because "the next seven
/// days" is a statement about the calendar the rows are grouped by, not about
/// 168 hours. Falls back to a plain arithmetic window only if the host calendar
/// cannot represent one of the two midnights, which `local_midnight` already
/// walks forward past.
pub fn upcoming_window(now: DateTime<Local>, days: u32) -> (i64, i64) {
    let days = days.min(MAX_UPCOMING_DAYS);
    let today = now.date_naive();
    let start = local_midnight(today)
        .map(|midnight| midnight.timestamp_millis())
        .unwrap_or_else(|_| now.timestamp_millis());
    let end = today
        .checked_add_days(Days::new(u64::from(days) + 1))
        .and_then(|date| local_midnight(date).ok())
        .map(|midnight| midnight.timestamp_millis())
        .unwrap_or(start + (i64::from(days) + 1) * 24 * 60 * 60_000);
    (start, end)
}

/// Turns calendar occurrences into rows, joining each recurring one to what its
/// series remembers and each named attendee to the person page behind their
/// address.
///
/// Order is whatever the source gave — `events_between` already sorts by start
/// — so this function never reorders and never regroups. A series record that
/// is missing from `series` yields a row with the series' defaults rather than
/// no series chip: the event still repeats, and hiding the controls because a
/// read came back short would be a lie about the calendar.
pub fn upcoming_rows(
    occurrences: Vec<CalendarOccurrence>,
    series: &HashMap<String, MeetingSeriesPreferences>,
    person_links: &HashMap<String, PersonId>,
) -> Vec<MeetingUpcomingRow> {
    occurrences
        .into_iter()
        .map(|occurrence| {
            let CalendarOccurrence {
                summary,
                is_recurring,
            } = occurrence;
            let series = is_recurring
                .then(|| series_for(&summary.series_key, series))
                .flatten();
            MeetingUpcomingRow {
                event_key: summary.event_key,
                title: summary.title,
                start_utc_ms: summary.start_utc_ms,
                end_utc_ms: summary.end_utc_ms,
                attendees: summary
                    .attendees
                    .into_iter()
                    .map(|attendee| MeetingUpcomingAttendee {
                        person_id: attendee
                            .email
                            .as_deref()
                            .map(str::trim)
                            .map(str::to_lowercase)
                            .and_then(|email| person_links.get(&email).copied()),
                        name: attendee.name,
                        status: attendee.status,
                        is_self: attendee.is_self,
                    })
                    .collect(),
                attendee_count: u32::try_from(summary.attendee_count).unwrap_or(u32::MAX),
                calendar_name: summary.calendar_name,
                join_url: summary.url,
                series,
            }
        })
        .collect()
}

/// The series record for a recurring event, defaulted when the read did not
/// carry one. `None` only for an event whose calendar gave no identifier at
/// all, which `summarize` already refuses to produce.
fn series_for(
    series_key: &str,
    series: &HashMap<String, MeetingSeriesPreferences>,
) -> Option<MeetingUpcomingSeries> {
    let series_key = series_key.trim();
    if series_key.is_empty() {
        return None;
    }
    let stored = series.get(series_key);
    Some(MeetingUpcomingSeries {
        series_key: series_key.to_string(),
        always_record: stored.is_some_and(|record| record.always_record),
        template: stored.and_then(|record| record.template),
        custom_template_id: stored.and_then(|record| record.custom_template_id),
        digest_included: stored.is_none_or(|record| record.digest_included),
    })
}

impl MeetingSessionManager {
    /// Joins a window of calendar occurrences to the series and person state
    /// Sona already holds.
    ///
    /// Two reads for the whole week, not two per row: the series keys and the
    /// attendee addresses are collected first and looked up in bulk. A store
    /// that cannot answer is not fatal — the rows still describe the calendar,
    /// which is the part the operator can act on — so both lookups fall back to
    /// empty and the controls render at their defaults.
    pub(crate) async fn upcoming_events(
        &self,
        access: CalendarAccess,
        window: (i64, i64),
        occurrences: Vec<CalendarOccurrence>,
    ) -> Result<MeetingUpcomingEvents, MeetingCommandError> {
        let series_keys = occurrences
            .iter()
            .filter(|occurrence| occurrence.is_recurring)
            .map(|occurrence| occurrence.summary.series_key.clone())
            .collect::<Vec<_>>();
        let emails = occurrences
            .iter()
            .flat_map(|occurrence| occurrence.summary.attendees.iter())
            .filter_map(|attendee| attendee.email.clone())
            .collect::<Vec<_>>();
        let store = self.store().await?;
        let series = store
            .series_preferences_many(&series_keys)
            .map_err(map_store_error)?;
        // A failed address-book read costs chips their links, which is a
        // degraded row rather than a wrong one. A failed fence read is not
        // recoverable that way: writing with a made-up revision would either
        // always reject or, worse, race.
        let person_links = store
            .person_ids_for_calendar_emails(&emails)
            .unwrap_or_default();
        let series_revision = store.series_revision().map_err(map_store_error)?;
        Ok(MeetingUpcomingEvents {
            access,
            window_start_utc_ms: window.0,
            window_end_utc_ms: window.1,
            rows: upcoming_rows(occurrences, &series, &person_links),
            series_revision,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::analytics::MeetingNotesTemplate;
    use crate::meeting::detection::machine::{
        CalendarAttendee, CalendarEventSummary, ParticipationStatus,
    };
    use chrono::TimeZone;
    use uuid::Uuid;

    const MINUTE: i64 = 60_000;

    fn attendee(name: &str, email: Option<&str>, is_self: bool) -> CalendarAttendee {
        CalendarAttendee {
            name: name.to_string(),
            status: ParticipationStatus::Accepted,
            email: email.map(str::to_string),
            is_self,
        }
    }

    fn occurrence(
        title: &str,
        series_key: &str,
        start_utc_ms: i64,
        is_recurring: bool,
        attendees: Vec<CalendarAttendee>,
    ) -> CalendarOccurrence {
        CalendarOccurrence {
            summary: CalendarEventSummary {
                event_key: format!("{series_key}#{start_utc_ms}"),
                series_key: series_key.to_string(),
                title: title.to_string(),
                attendee_count: attendees.len() + 1,
                start_utc_ms,
                end_utc_ms: start_utc_ms + 30 * MINUTE,
                attendees,
                notes: None,
                calendar_name: Some("Work".to_string()),
                url: Some("https://meet.example.com/abc".to_string()),
            },
            is_recurring,
        }
    }

    fn preferences(
        series_key: &str,
        always_record: bool,
        template: Option<MeetingNotesTemplate>,
        digest_included: bool,
    ) -> (String, MeetingSeriesPreferences) {
        (
            series_key.to_string(),
            MeetingSeriesPreferences {
                series_key: Some(series_key.to_string()),
                template,
                custom_template_id: None,
                digest_included,
                always_record,
                remote_intelligence_opt_out: false,
                announce_in_chat: false,
                revision: 7,
            },
        )
    }

    /* The join is the whole point of the command: a row that repeats arrives
     * carrying the three decisions its series has made, so the pane draws the
     * controls in the right position without a second read per row. */
    #[test]
    fn a_recurring_row_carries_its_series_three_decisions() {
        let series = HashMap::from([preferences(
            "weekly-sync",
            true,
            Some(MeetingNotesTemplate::OneOnOne),
            false,
        )]);

        let rows = upcoming_rows(
            vec![occurrence(
                "Weekly sync",
                "weekly-sync",
                1_000,
                true,
                Vec::new(),
            )],
            &series,
            &HashMap::new(),
        );

        let joined = rows[0].series.as_ref().expect("a recurring row has series");
        assert_eq!(joined.series_key, "weekly-sync");
        assert!(joined.always_record);
        assert_eq!(joined.template, Some(MeetingNotesTemplate::OneOnOne));
        assert!(!joined.digest_included);
    }

    /* A one-off has nothing to remember it by. Offering "always record this
     * series" on an event that happens once would be a switch with no series
     * behind it, so the row carries no series at all — even though EventKit
     * still handed it a calendar-item identifier. */
    #[test]
    fn a_one_off_carries_no_series_even_though_it_has_an_identifier() {
        let series = HashMap::from([preferences("lunch", true, None, true)]);

        let rows = upcoming_rows(
            vec![occurrence("Lunch", "lunch", 1_000, false, Vec::new())],
            &series,
            &HashMap::new(),
        );

        assert!(rows[0].series.is_none());
    }

    /* A series nobody has decided anything about still gets its chip and its
     * controls, at the defaults: it repeats, and that is what the chip claims. */
    #[test]
    fn a_series_with_no_stored_record_renders_at_the_defaults() {
        let rows = upcoming_rows(
            vec![occurrence("Standup", "standup", 1_000, true, Vec::new())],
            &HashMap::new(),
            &HashMap::new(),
        );

        let joined = rows[0].series.as_ref().expect("a recurring row has series");
        assert!(!joined.always_record);
        assert_eq!(joined.template, None);
        assert!(
            joined.digest_included,
            "a series nobody excluded is in the digest"
        );
    }

    /* An attendee chip is navigable only when the address book actually knows
     * that address. Case and surrounding space come from the calendar, not from
     * the person record, so the match normalizes both sides. */
    #[test]
    fn only_attendees_the_address_book_knows_become_links() {
        let known = PersonId(Uuid::new_v4());
        let links = HashMap::from([("steven@example.com".to_string(), known)]);

        let rows = upcoming_rows(
            vec![occurrence(
                "Pricing",
                "pricing",
                1_000,
                true,
                vec![
                    attendee("Steven", Some("  STEVEN@example.com "), false),
                    attendee("Dana", Some("dana@example.com"), false),
                    attendee("Me", None, true),
                ],
            )],
            &HashMap::new(),
            &links,
        );

        let attendees = &rows[0].attendees;
        assert_eq!(attendees[0].person_id, Some(known));
        assert_eq!(attendees[1].person_id, None);
        assert_eq!(attendees[2].person_id, None);
        assert!(attendees[2].is_self);
    }

    /* `attendee_count` includes the participants EventKit refused to name, so a
     * row can honestly say "3 people" while showing one chip. */
    #[test]
    fn the_participant_count_survives_the_named_attendee_list() {
        let rows = upcoming_rows(
            vec![occurrence(
                "Board",
                "board",
                1_000,
                true,
                vec![attendee("Dana", None, false)],
            )],
            &HashMap::new(),
            &HashMap::new(),
        );

        assert_eq!(rows[0].attendees.len(), 1);
        assert_eq!(rows[0].attendee_count, 2);
    }

    /* Rows come out in the order the calendar gave them. The source sorts by
     * start; re-sorting here would be a second precedence rule for one list. */
    #[test]
    fn rows_keep_the_order_the_calendar_listed_them_in() {
        let rows = upcoming_rows(
            vec![
                occurrence("First", "a", 1_000, false, Vec::new()),
                occurrence("Second", "b", 2_000, false, Vec::new()),
                occurrence("Third", "c", 3_000, false, Vec::new()),
            ],
            &HashMap::new(),
            &HashMap::new(),
        );

        let titles = rows
            .iter()
            .map(|row| row.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(titles, vec!["First", "Second", "Third"]);
    }

    /* "Today and the next seven days" is eight local calendar days, cut at
     * midnight on both ends — not 168 hours from whenever the pane happened to
     * mount. */
    #[test]
    fn the_window_spans_eight_local_days_from_this_midnight() {
        let now = Local
            .with_ymd_and_hms(2026, 8, 31, 14, 37, 12)
            .single()
            .expect("a representable local instant");

        let (start, end) = upcoming_window(now, 7);

        assert!(start <= now.timestamp_millis(), "the window includes today");
        assert!(end > now.timestamp_millis());
        let span_days = (end - start) as f64 / (24.0 * 60.0 * 60_000.0);
        assert!(
            (7.9..=8.1).contains(&span_days),
            "eight local days, allowing one DST hour: {span_days}"
        );
    }

    /* A caller cannot turn a home-screen read into a year-long calendar scan. */
    #[test]
    fn the_window_is_capped_at_the_supported_horizon() {
        let now = Local
            .with_ymd_and_hms(2026, 8, 31, 9, 0, 0)
            .single()
            .expect("a representable local instant");

        let (start, end) = upcoming_window(now, 4_000);
        let span_days = (end - start) as f64 / (24.0 * 60.0 * 60_000.0);

        assert!(span_days <= f64::from(MAX_UPCOMING_DAYS) + 1.1);
    }
}
