//! A hotel stay read out of a booking email already in Mail, offered as a
//! maybe for a recording that names no place. It reads only with email
//! context on, stays on this Mac, and writes nothing: the person chooses the
//! place in Maps and saves it.

use super::context::{ConversationContext, ConversationKind};
use super::mail_context::{MailContext, MailContextState, MailThread};
use super::types::MeetingOrigin;
use chrono::{DateTime, Datelike, Duration, NaiveDate};
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::LazyLock;

/// A stay whose dates cover the recording's day, and the email it came from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MailHotelStay {
    pub hotel: String,
    /// `YYYY-MM-DD`, as the email gave it.
    pub check_in: String,
    pub check_out: String,
    pub subject: String,
    pub sender: String,
    pub received_at: String,
}

/// `conversation_place_from_mail`: what Mail said, and the stay when one
/// covers the recording's day.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MailHotelLookup {
    pub state: MailContextState,
    pub stay: Option<MailHotelStay>,
}

impl MailHotelLookup {
    pub(crate) fn without_reading(state: MailContextState) -> Self {
        Self { state, stay: None }
    }

    pub(crate) fn from_mail(mail: &MailContext, day: NaiveDate) -> Self {
        let stay = stay_covering(&mail.threads, day);
        let state = match (&stay, &mail.state) {
            (Some(_), _) => MailContextState::Ready,
            (None, MailContextState::Ready) => MailContextState::Empty,
            (None, state) => state.clone(),
        };
        Self { state, stay }
    }
}

/// A recording made in a room that names no place: no place chosen, none
/// on its calendar event, and no call audio.
pub(crate) fn wants_place(context: &ConversationContext) -> bool {
    context.location.is_none()
        && context.calendar_location.is_none()
        && !context.captured_system_audio
        && context.kind != ConversationKind::OnlineCall
        && context.origin != MeetingOrigin::Import
}

static CHECK_IN: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: the literal pattern is fixed; Regex has no compile-time constructor.
    Regex::new(r"(?i)\b(?:check[\s-]?in|arrival\s+date)\b").expect("check-in")
});
static CHECK_OUT: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: the literal pattern is fixed; Regex has no compile-time constructor.
    Regex::new(r"(?i)\b(?:check[\s-]?out|departure\s+date)\b").expect("check-out")
});
static DATE: LazyLock<Regex> = LazyLock::new(|| {
    let month = "jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec";
    // PANIC: the pattern is fixed apart from the constant month list.
    Regex::new(&format!(
        r"(?i)\b(?P<iy>\d{{4}})-(?P<im>\d{{1,2}})-(?P<id>\d{{1,2}})\b|\b(?P<mm>{month})[a-z]*\.?\s+(?P<md>\d{{1,2}})(?:st|nd|rd|th)?\b(?:,?\s+(?P<my>\d{{4}})\b)?|\b(?P<dd>\d{{1,2}})(?:st|nd|rd|th)?\s+(?P<dm>{month})[a-z]*\.?(?:,?\s+(?P<dy>\d{{4}})\b)?|\b(?P<um>\d{{1,2}})/(?P<ud>\d{{1,2}})/(?P<uy>\d{{4}}|\d{{2}})\b"
    ))
    .expect("date")
});
static LODGING: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: the literal pattern is fixed; Regex has no compile-time constructor.
    Regex::new(
        r"(?i)\b(?:hotel|inn|suites|resort|lodge|hostel|motel|hilton|hyatt|marriott|sheraton|westin|courtyard|hampton|doubletree|kimpton|radisson|wyndham|fairmont|intercontinental|novotel|ibis|moxy|aloft|four seasons|ritz-carlton|waldorf|conrad|andaz|loews|omni|citizenm|yotel|staybridge|renaissance|sofitel|pullman|mercure|best western|holiday inn|crowne plaza)\b",
    )
    .expect("lodging")
});
/// Words that make a line about the booking, not the hotel's name.
static NOT_A_NAME: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: the literal pattern is fixed; Regex has no compile-time constructor.
    Regex::new(
        r"(?i)\b(?:reservations?|booking|booked|confirm\w*|check|points|rewards?|honors|bonvoy|members?|app|download|privacy|unsubscribe|policy|cancel\w*|thank\w*|dear|your|you|book|deals?|offers?|save|stay|nights?|rooms?|guests?|itinerary)\b|https?:|www\.|@|©",
    )
    .expect("not a name")
});
static SUBJECT_PLACE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: the literal pattern is fixed; Regex has no compile-time constructor.
    Regex::new(
        r"(?i)\b(?:at|to|for)\s+(?P<name>.+?)\s*(?:$|[!|(:,–—]|\s-\s|\s+(?:is|has|was|on|from|in)\s)",
    )
    .expect("subject place")
});

/// Characters looked at after a check-in or check-out label for its date.
const DATE_WINDOW_CHARS: usize = 100;
const LONGEST_STAY_NIGHTS: i64 = 60;

fn stay_covering(threads: &[MailThread], day: NaiveDate) -> Option<MailHotelStay> {
    threads
        .iter()
        .filter_map(|thread| {
            let (hotel, check_in, check_out) = stay_in(thread)?;
            (check_in <= day && day <= check_out).then(|| MailHotelStay {
                hotel,
                check_in: check_in.to_string(),
                check_out: check_out.to_string(),
                subject: thread.subject.clone(),
                sender: thread.sender.clone(),
                received_at: thread.received_at.clone(),
            })
        })
        .max_by(|a, b| a.received_at.cmp(&b.received_at))
}

fn stay_in(thread: &MailThread) -> Option<(String, NaiveDate, NaiveDate)> {
    if thread.subject.to_lowercase().contains("cancel") {
        return None;
    }
    let received = DateTime::parse_from_rfc3339(&thread.received_at)
        .ok()?
        .date_naive();
    let text = &thread.excerpt;
    let (label_at, check_in) = date_after(text, &CHECK_IN, received, None)?;
    let (_, check_out) = date_after(text, &CHECK_OUT, received, Some(check_in))?;
    if !(1..=LONGEST_STAY_NIGHTS).contains(&(check_out - check_in).num_days()) {
        return None;
    }
    let hotel = hotel_in_subject(&thread.subject).or_else(|| hotel_in_text(text, label_at))?;
    Some((hotel, check_in, check_out))
}

/// The first date written just after one of the label's mentions, and where
/// that mention is. With `after`, the first one later than it: a table that
/// lists both labels before both dates puts check-in's date first.
fn date_after(
    text: &str,
    label: &Regex,
    received: NaiveDate,
    after: Option<NaiveDate>,
) -> Option<(usize, NaiveDate)> {
    label.find_iter(text).find_map(|mention| {
        let rest = &text[mention.end()..];
        let end = rest
            .char_indices()
            .nth(DATE_WINDOW_CHARS)
            .map_or(rest.len(), |(index, _)| index);
        DATE.captures_iter(&rest[..end])
            .filter_map(|found| date(&found, received))
            .find(|date| after.is_none_or(|after| *date > after))
            .map(|date| (mention.start(), date))
    })
}

/// A written date, with a missing year taken as the next time that day
/// comes after the email arrived.
fn date(found: &Captures<'_>, received: NaiveDate) -> Option<NaiveDate> {
    let number = |name: &str| found.name(name)?.as_str().parse::<u32>().ok();
    let month_named = |name: &str| {
        let month = found.name(name)?.as_str().to_lowercase();
        [
            "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
        ]
        .iter()
        .position(|candidate| *candidate == month)
        .and_then(|index| u32::try_from(index + 1).ok())
    };
    let (year, month, day) = if found.name("iy").is_some() {
        (number("iy"), number("im")?, number("id")?)
    } else if found.name("mm").is_some() {
        (number("my"), month_named("mm")?, number("md")?)
    } else if found.name("dd").is_some() {
        (number("dy"), month_named("dm")?, number("dd")?)
    } else {
        let year = number("uy").map(|year| if year < 100 { year + 2000 } else { year });
        (year, number("um")?, number("ud")?)
    };
    match year {
        Some(year) => NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day),
        None => {
            let this_year = NaiveDate::from_ymd_opt(received.year(), month, day)?;
            if this_year < received - Duration::days(30) {
                NaiveDate::from_ymd_opt(received.year() + 1, month, day)
            } else {
                Some(this_year)
            }
        }
    }
}

fn hotel_in_subject(subject: &str) -> Option<String> {
    SUBJECT_PLACE
        .captures_iter(subject)
        .filter_map(|found| found.name("name").map(|name| name.as_str()))
        .chain(
            subject
                .split(['|', ':', '–', '—'])
                .flat_map(|part| part.split(" - ")),
        )
        .find_map(name)
}

/// The hotel-like line nearest above the check-in label, else the first one
/// below it.
fn hotel_in_text(text: &str, label_at: usize) -> Option<String> {
    let mut offset = 0;
    let mut before = None;
    let mut after = None;
    for line in text.split('\n') {
        let start = offset;
        offset += line.len() + 1;
        let Some(found) = name(line) else { continue };
        if start < label_at {
            before = Some(found);
        } else if after.is_none() {
            after = Some(found);
        }
    }
    before.or(after)
}

fn name(candidate: &str) -> Option<String> {
    let candidate = candidate
        .trim()
        .trim_matches(|c: char| matches!(c, '.' | ',' | ';' | '!' | '"' | '“' | '”' | '#'))
        .trim();
    let length = candidate.chars().count();
    ((4..=80).contains(&length)
        && LODGING.is_match(candidate)
        && !NOT_A_NAME.is_match(candidate)
        && candidate.chars().next().is_some_and(char::is_uppercase))
    .then(|| candidate.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email(subject: &str, received_at: &str, excerpt: &str) -> MailThread {
        MailThread {
            subject: subject.into(),
            sender: "Hotels <noreply@example.com>".into(),
            received_at: received_at.into(),
            excerpt: excerpt.into(),
        }
    }

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("day")
    }

    #[test]
    fn a_booking_email_names_the_hotel_for_a_day_inside_the_stay() {
        let threads = [
            email(
                "Your reservation at Hilton San Francisco Union Square is confirmed",
                "2026-09-30T17:02:00.000Z",
                "Thanks for booking.\nCheck-in: Tuesday, November 3, 2026 (after 3 PM)\nCheck-out: Friday, November 6, 2026\nConfirmation 3301",
            ),
            email(
                "Booking confirmation",
                "2026-10-12T09:00:00.000Z",
                "Booking.com\nHotel Zephyr\n250 Beach St, San Francisco\nCheck-in Check-out\nThu 19 Nov Sun 22 Nov",
            ),
        ];
        let stay = stay_covering(&threads, day("2026-11-05")).expect("stay");
        assert_eq!(stay.hotel, "Hilton San Francisco Union Square");
        assert_eq!(
            (stay.check_in.as_str(), stay.check_out.as_str()),
            ("2026-11-03", "2026-11-06")
        );
        let table = stay_covering(&threads, day("2026-11-20")).expect("stay");
        assert_eq!(table.hotel, "Hotel Zephyr");
        assert_eq!(
            (table.check_in.as_str(), table.check_out.as_str()),
            ("2026-11-19", "2026-11-22")
        );
        assert_eq!(stay_covering(&threads, day("2026-11-10")), None);
    }

    #[test]
    fn a_cancelled_stay_a_flight_and_a_nameless_stay_suggest_nothing() {
        let threads = [
            email(
                "Your reservation at Hotel Zetta has been cancelled",
                "2026-10-01T10:00:00.000Z",
                "Check-in: Nov 4, 2026\nCheck-out: Nov 7, 2026",
            ),
            email(
                "Your itinerary to New York",
                "2026-10-01T10:00:00.000Z",
                "Check in online 24 hours before. Departs Nov 4, 2026. Arrives Nov 4, 2026.",
            ),
            email(
                "Reservation confirmed for Mission District",
                "2026-10-01T10:00:00.000Z",
                "Check-in Wed, Nov 4\nCheckout Sat, Nov 7\nEntire home hosted by Sam",
            ),
        ];
        assert_eq!(stay_covering(&threads, day("2026-11-05")), None);
    }
}
