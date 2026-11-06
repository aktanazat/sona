//! The corpus card: what this Mac holds, in numbers, at the top of every pack.
//!
//! A keyword pack answers the question it was searched for. The card answers
//! the questions a reader asks before that one — how much is here, how recent
//! is it, what is coming up — so the model can orient itself and answer an
//! aggregate question ("how many meetings did I have this week") without a
//! lookup, and can pick the right tool for the ones that need one.
//!
//! One fact per line, plain text, `sona corpus card 1` first. Bounded at
//! [`CARD_MAX_BYTES`] by dropping whole trailing lines: the lines are ordered
//! so the ones that matter most come first, and a line that was cut in the
//! middle would be a number the model cannot trust.
//!
//! The card leaves the machine with the pack, so it sees what the pack sees:
//! the last meeting and the events ahead go through the same series exclusion
//! (D14) every row of a pack does, and a series kept on this Mac is neither
//! named nor counted here.

use super::pack::without_excluded_series;
use super::tools::{allowed_upcoming, top_words, when, WORD_STATS_MAX_ENTRIES};
use super::{meeting_link, QueryRow, QueryRowKind};
use crate::analytics::{local_days_start_utc_ms, DashboardTrendRange, DashboardTrendRequest};
use crate::managers::history::HistoryManager;
use crate::meeting::detection::calendar::CalendarSource;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::MeetingStore;
use crate::meeting::types::{
    MeetingHistoryHeadline, MeetingListFilter, MeetingSessionId, MeetingTrendProjection,
};
use crate::meeting::upcoming::{upcoming_window, UPCOMING_DEFAULT_DAYS};
use crate::meeting::upcoming_types::MeetingUpcomingRow;
use chrono::{DateTime, Local, SecondsFormat};
use std::sync::Arc;

/// The most a card may weigh. Two kilobytes is a screen of facts; the pack it
/// heads has its own ceiling, and a card that grew past this would be
/// crowding out the evidence it introduces.
pub const CARD_MAX_BYTES: usize = 2048;

/// How many of the most used words the card names.
const TOP_WORDS: usize = 20;

/// The window the top words are counted over, in days.
const TOP_WORDS_DAYS: u32 = 90;

/// How many upcoming events the `next:` line names.
const NEXT_EVENTS: usize = 2;

/// How deep the open-loop count looks, as the inbox does.
const LOOP_SCAN_DEPTH: usize = 200;

/// How much of a meeting title the card carries.
const MAX_TITLE_CHARS: usize = 80;

const HOUR_MS: u64 = 3_600_000;

/// Everything the card says, before it is rendered.
///
/// Split from the reads so the format is provable without a running app: the
/// history manager cannot be built in a test, the store can, and the card's
/// shape depends on neither.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct CorpusFacts {
    pub dictations: u64,
    pub first_dictation_utc_ms: Option<i64>,
    pub last_dictation_utc_ms: Option<i64>,
    pub words: u64,
    pub meetings: u64,
    /// The newest meeting the card may name: its id, title and time.
    pub last_meeting: Option<(MeetingSessionId, String, i64)>,
    pub meeting_ms: u64,
    pub people: usize,
    pub open_loops: usize,
    pub series: usize,
    pub upcoming_72h: usize,
    pub week_dictations: u64,
    pub week_words: u64,
    pub week_meetings: u64,
    pub week_meeting_ms: u64,
    pub top_words: Vec<(String, u64)>,
    /// The next events, soonest first, already less the excluded series.
    pub next: Vec<MeetingUpcomingRow>,
}

/// Build the card for this moment.
///
/// Every read that fails leaves its line at zero rather than failing the
/// card: a pack with a card that says "meetings: 0" while the store is locked
/// is wrong about one number, and a pack with no card is wrong about all of
/// them. The calendar read blocks on EventKit, so it runs off the async
/// runtime's worker threads.
pub async fn corpus_card(
    meetings: &Arc<MeetingSessionManager>,
    history: &Arc<HistoryManager>,
    calendar: &Arc<dyn CalendarSource>,
    now: DateTime<Local>,
) -> String {
    let mut facts = match meetings.store().await {
        Ok(store) => store_facts(&store, now),
        Err(error) => {
            log::warn!("Corpus card without the meeting store: {error:?}");
            CorpusFacts::default()
        }
    };
    if let Ok(store) = meetings.store().await {
        let window = upcoming_window(now, UPCOMING_DEFAULT_DAYS);
        let window = (now.timestamp_millis(), window.1);
        let source = Arc::clone(calendar);
        let occurrences =
            tauri::async_runtime::spawn_blocking(move || source.events_between(window.0, window.1))
                .await
                .unwrap_or_default();
        if let Ok(events) = meetings
            .upcoming_events(calendar.access(), window, occurrences)
            .await
        {
            let rows = allowed_upcoming(&store, events.rows).unwrap_or_default();
            let horizon = (now + chrono::Duration::hours(72)).timestamp_millis();
            facts.upcoming_72h = rows.iter().filter(|row| row.start_utc_ms < horizon).count();
            facts.next = rows.into_iter().take(NEXT_EVENTS).collect();
        }
    }
    if let Ok(stats) = history.get_history_stats().await {
        facts.dictations = stats.entries;
        facts.words = stats.total_words;
    }
    if let Ok(Some((first, last))) = history.get_history_span().await {
        facts.first_dictation_utc_ms = Some(first * 1000);
        facts.last_dictation_utc_ms = Some(last * 1000);
    }
    if let Ok(trend) = history
        .get_history_trend(DashboardTrendRequest {
            range: DashboardTrendRange::Days7,
        })
        .await
    {
        facts.week_dictations = trend.range_total.recordings;
        facts.week_words = trend.range_total.words;
    }
    if let Ok(since) = local_days_start_utc_ms(now, TOP_WORDS_DAYS) {
        if let Ok(entries) = history
            .get_history_entries_since(since / 1000, WORD_STATS_MAX_ENTRIES)
            .await
        {
            facts.top_words = top_words(&entries, TOP_WORDS).1;
        }
    }
    compose(&facts, now)
}

/// The store's half of the facts. A read that fails leaves its numbers at
/// zero, for the reason the card itself does.
pub(super) fn store_facts(store: &MeetingStore, now: DateTime<Local>) -> CorpusFacts {
    let mut facts = CorpusFacts::default();
    if let Ok(MeetingTrendProjection::Available {
        all_time,
        range_total,
        ..
    }) = store.trend_projection_at(
        DashboardTrendRequest {
            range: DashboardTrendRange::Days7,
        },
        now,
    ) {
        facts.meetings = all_time.meetings;
        facts.meeting_ms = all_time.verified_captured_duration_ms;
        facts.week_meetings = range_total.meetings;
        facts.week_meeting_ms = range_total.verified_captured_duration_ms;
    }
    facts.last_meeting = last_meeting(store);
    if let Ok(people) = store.people_list() {
        facts.people = people.entries.len();
    }
    if let Ok(inbox) = store.open_loops_inbox(LOOP_SCAN_DEPTH) {
        facts.open_loops = without_excluded_series(
            store,
            inbox
                .entries
                .iter()
                .map(|entry| QueryRow {
                    kind: QueryRowKind::Loop,
                    id: entry.loop_id.as_str().to_string(),
                    title: String::new(),
                    snippet: String::new(),
                    when_utc_ms: entry.at_utc_ms,
                    link: String::new(),
                })
                .collect(),
        )
        .len();
    }
    if let Ok(roster) = store.series_remote_roster() {
        facts.series = roster
            .rows
            .iter()
            .filter(|row| !row.remote_intelligence_opt_out)
            .count();
    }
    facts
}

/// The newest meeting whose series may leave this Mac, from the list the
/// Library shows. One page is read: a corpus whose newest page is entirely
/// kept local names no meeting, which is the honest answer.
fn last_meeting(store: &MeetingStore) -> Option<(MeetingSessionId, String, i64)> {
    let page = store
        .list_sessions(None, 10, &MeetingListFilter::default())
        .ok()?;
    let rows = page
        .entries
        .iter()
        .map(|summary| QueryRow {
            kind: QueryRowKind::Meeting,
            id: summary.session_id.uuid().to_string(),
            title: summary.title.clone(),
            snippet: match &summary.headline {
                MeetingHistoryHeadline::Ledger { text }
                | MeetingHistoryHeadline::Summary { text } => text.clone(),
                _ => String::new(),
            },
            when_utc_ms: summary.created_at_utc_ms,
            link: meeting_link(summary.session_id),
        })
        .collect();
    let allowed = without_excluded_series(store, rows).into_iter().next()?;
    let session_id = MeetingSessionId::from_uuid(uuid::Uuid::parse_str(&allowed.id).ok()?);
    Some((session_id, allowed.title, allowed.when_utc_ms))
}

/// Render the card. Pure: the same facts and clock give the same bytes.
pub(super) fn compose(facts: &CorpusFacts, now: DateTime<Local>) -> String {
    let mut lines = vec![
        "sona corpus card 1".to_string(),
        format!(
            "now: {} {}",
            now.to_rfc3339_opts(SecondsFormat::Secs, false),
            zone_name()
        ),
    ];
    let mut dictations = format!("dictations: {}", facts.dictations);
    if let (Some(first), Some(last)) = (facts.first_dictation_utc_ms, facts.last_dictation_utc_ms) {
        dictations.push_str(&format!(" (first {}, last {})", when(first), when(last)));
    }
    dictations.push_str(&format!(", words: {}", facts.words));
    lines.push(dictations);
    let mut meetings = format!("meetings: {}", facts.meetings);
    if let Some((session_id, title, at_utc_ms)) = &facts.last_meeting {
        meetings.push_str(&format!(
            " (last {} {:?}, {})",
            when(*at_utc_ms),
            super::bounded(&super::pack::one_line(title), MAX_TITLE_CHARS),
            meeting_link(*session_id)
        ));
    }
    meetings.push_str(&format!(", hours: {}", hours(facts.meeting_ms)));
    lines.push(meetings);
    lines.push(format!(
        "people: {}, open loops: {}, series: {}, upcoming 72h: {}",
        facts.people, facts.open_loops, facts.series, facts.upcoming_72h
    ));
    lines.push(format!(
        "last 7 days: {} dictations, {} words, {} meetings ({})",
        facts.week_dictations,
        facts.week_words,
        facts.week_meetings,
        duration(facts.week_meeting_ms)
    ));
    if facts.top_words.is_empty() {
        lines.push(format!("top words {TOP_WORDS_DAYS}d: none"));
    } else {
        lines.push(format!(
            "top words {TOP_WORDS_DAYS}d: {} ({} entries, stopwords removed)",
            facts
                .top_words
                .iter()
                .map(|(word, count)| format!("{word} {count}"))
                .collect::<Vec<_>>()
                .join(", "),
            facts.top_words.len()
        ));
    }
    if !facts.next.is_empty() {
        lines.push(format!(
            "next: {}",
            facts
                .next
                .iter()
                .map(|row| {
                    format!(
                        "{} {:?} ({} attendees)",
                        when(row.start_utc_ms),
                        super::bounded(&super::pack::one_line(&row.title), MAX_TITLE_CHARS),
                        row.attendee_count
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut card = String::new();
    for line in lines {
        let needed = line.len() + usize::from(!card.is_empty());
        if card.len() + needed > CARD_MAX_BYTES {
            break;
        }
        if !card.is_empty() {
            card.push('\n');
        }
        card.push_str(&line);
    }
    card
}

/// The host zone by its IANA name, or the offset alone when the platform will
/// not say. Written once on the `now:` line so every other time on the card,
/// which carries only its offset, can be read against it.
fn zone_name() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "local".to_string())
}

/// Hours to one decimal: "41.5".
fn hours(ms: u64) -> String {
    let tenths = (ms + HOUR_MS / 20) / (HOUR_MS / 10);
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// Hours and minutes: "2h10m", "45m", "0m".
fn duration(ms: u64) -> String {
    let minutes = ms / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes}m"),
        (hours, minutes) => format!("{hours}h{minutes:02}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::store::workflow_core_tests::{reviewable_meeting, store};
    use crate::meeting::types::MeetingOperationId;
    use chrono::TimeZone;

    /// 2026-08-14 09:32 UTC.
    const WHEN: i64 = 1_786_699_920_000;
    const DAY: i64 = 86_400_000;

    fn now() -> DateTime<Local> {
        Local.timestamp_millis_opt(WHEN + 3_600_000).unwrap()
    }

    fn upcoming(title: &str, start: i64, attendees: u32) -> MeetingUpcomingRow {
        MeetingUpcomingRow {
            event_key: format!("{title}@{start}"),
            title: title.to_string(),
            start_utc_ms: start,
            end_utc_ms: start + 1_800_000,
            attendees: Vec::new(),
            attendee_count: attendees,
            calendar_name: None,
            join_url: None,
            location: None,
            series: None,
        }
    }

    fn in_series(store: &MeetingStore, session_id: MeetingSessionId, series_key: &str) {
        store
            .remember_calendar_facts(
                session_id,
                &CalendarEventSummary {
                    event_key: format!("{series_key}#{}", session_id.uuid()),
                    series_key: series_key.to_string(),
                    title: "Weekly".to_string(),
                    attendee_count: 2,
                    start_utc_ms: WHEN,
                    end_utc_ms: WHEN + 1_800_000,
                    attendees: Vec::new(),
                    notes: None,
                    calendar_name: None,
                    location: None,
                    url: None,
                },
            )
            .unwrap();
    }

    #[test]
    fn the_card_is_one_fact_per_line_under_its_header() {
        let session_id = MeetingSessionId::new();
        let facts = CorpusFacts {
            dictations: 1284,
            first_dictation_utc_ms: Some(WHEN - 60 * DAY),
            last_dictation_utc_ms: Some(WHEN),
            words: 96_410,
            meetings: 37,
            last_meeting: Some((session_id, "Planning\twalk".to_string(), WHEN - DAY)),
            meeting_ms: 41 * 3_600_000 + 30 * 60_000,
            people: 22,
            open_loops: 9,
            series: 5,
            upcoming_72h: 2,
            week_dictations: 41,
            week_words: 6_210,
            week_meetings: 3,
            week_meeting_ms: 130 * 60_000,
            top_words: vec![("deck".to_string(), 41), ("friday".to_string(), 33)],
            next: vec![
                upcoming("Standup", WHEN + DAY, 4),
                upcoming("Nolan 1:1", WHEN + DAY + 4 * 3_600_000, 2),
            ],
        };

        let card = compose(&facts, now());

        let lines = card.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "sona corpus card 1");
        assert_eq!(
            lines[1],
            format!(
                "now: {} {}",
                now().to_rfc3339_opts(SecondsFormat::Secs, false),
                zone_name()
            )
        );
        assert_eq!(
            lines[2],
            format!(
                "dictations: 1284 (first {}, last {}), words: 96410",
                when(WHEN - 60 * DAY),
                when(WHEN)
            )
        );
        assert_eq!(
            lines[3],
            format!(
                "meetings: 37 (last {} \"Planning walk\", {}), hours: 41.5",
                when(WHEN - DAY),
                meeting_link(session_id)
            )
        );
        assert_eq!(
            lines[4],
            "people: 22, open loops: 9, series: 5, upcoming 72h: 2"
        );
        assert_eq!(
            lines[5],
            "last 7 days: 41 dictations, 6210 words, 3 meetings (2h10m)"
        );
        assert_eq!(
            lines[6],
            "top words 90d: deck 41, friday 33 (2 entries, stopwords removed)"
        );
        assert_eq!(
            lines[7],
            format!(
                "next: {} \"Standup\" (4 attendees), {} \"Nolan 1:1\" (2 attendees)",
                when(WHEN + DAY),
                when(WHEN + DAY + 4 * 3_600_000)
            )
        );
        assert_eq!(lines.len(), 8);
        assert!(!card.ends_with('\n'));
        assert!(DateTime::parse_from_rfc3339(lines[1].split(' ').nth(1).unwrap()).is_ok());
    }

    #[test]
    fn an_empty_corpus_reports_zero_counts() {
        let (_directory, store) = store();

        let facts = store_facts(&store, now());
        let card = compose(&facts, now());

        assert_eq!(facts, CorpusFacts::default());
        assert!(card.contains("\ndictations: 0, words: 0\n"), "{card}");
        assert!(card.contains("\nmeetings: 0, hours: 0.0\n"), "{card}");
        assert!(card.contains("\npeople: 0, open loops: 0, series: 0, upcoming 72h: 0\n"));
        assert!(card.contains("\nlast 7 days: 0 dictations, 0 words, 0 meetings (0m)\n"));
        assert!(card.ends_with("\ntop words 90d: none"), "{card}");
        assert!(!card.contains("next:"));
    }

    #[test]
    fn the_store_facts_count_meetings_and_name_the_newest_allowed_one() {
        let (_directory, store) = store();
        let now = now();
        for index in 0..40 {
            let session_id = reviewable_meeting(
                &store,
                &format!("Meeting {index} about a topic with a long enough title to matter"),
                WHEN - index * DAY,
            );
            in_series(&store, session_id, &format!("series-{}", index % 6));
        }
        store
            .set_series_remote_opt_out(
                &MeetingSeriesRemoteOptOutSetRequest {
                    operation_id: MeetingOperationId::new(),
                    series_key: "series-0".to_string(),
                    remote_intelligence_opt_out: true,
                    expected_revision: store.series_revision().unwrap(),
                },
                WHEN,
            )
            .unwrap();

        let facts = store_facts(&store, now);
        let card = compose(&facts, now);

        assert_eq!(facts.meetings, 40);
        assert_eq!(facts.series, 5, "the kept series is not counted");
        let (_, title, at) = facts
            .last_meeting
            .clone()
            .expect("a newest allowed meeting");
        assert!(
            title.starts_with("Meeting 1 "),
            "meeting 0 is in the kept series: {title}"
        );
        assert_eq!(at, WHEN - DAY);
        assert!(!card.contains("Meeting 0 "), "{card}");
        assert!(card.len() <= CARD_MAX_BYTES, "{} bytes", card.len());
        assert!(card.contains("\nmeetings: 40 (last "), "{card}");
    }

    #[test]
    fn a_card_over_the_ceiling_loses_whole_trailing_lines() {
        let facts = CorpusFacts {
            top_words: (0..200)
                .map(|index| (format!("word{index:03}"), 200 - index))
                .collect(),
            next: vec![upcoming(&"x".repeat(400), WHEN + DAY, 1)],
            ..CorpusFacts::default()
        };

        let card = compose(&facts, now());

        assert!(card.len() <= CARD_MAX_BYTES, "{} bytes", card.len());
        assert!(card.starts_with("sona corpus card 1\nnow: "));
        assert!(
            card.ends_with("meetings (0m)"),
            "the top words line did not fit and was dropped whole: {card}"
        );
        assert!(!card.contains("next:"));
    }

    #[test]
    fn durations_round_the_way_a_reader_expects() {
        assert_eq!(hours(0), "0.0");
        assert_eq!(hours(41 * HOUR_MS + 30 * 60_000), "41.5");
        assert_eq!(hours(HOUR_MS / 20), "0.1");
        assert_eq!(duration(0), "0m");
        assert_eq!(duration(45 * 60_000), "45m");
        assert_eq!(duration(130 * 60_000), "2h10m");
        assert_eq!(duration(3 * HOUR_MS + 5 * 60_000), "3h05m");
    }
}
