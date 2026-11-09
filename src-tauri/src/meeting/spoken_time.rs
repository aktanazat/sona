//! The day, and the time of day, a phrase names, read against the moment it
//! was said.
//!
//! English words and numerals only: "today", "tonight", "tomorrow", "the day
//! after tomorrow", "yesterday", a weekday, "last Tuesday", "next week
//! Tuesday", "March 4th", "4 March 2027", "2026-04-01", and a time such as "at
//! 2", "2:30 pm", "noon", "ten o'clock" or "from 2 to 3". A phrase in another
//! language, or one this reader cannot place, names nothing rather than a
//! guessed day: a reminder or a calendar entry on the wrong day is worse than
//! one with no day at all.

use chrono::{Datelike, Days, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Weekday};
use std::ops::Range;

/// When a phrase says something happens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SpokenTime {
    /// A day, with no time of day.
    Day(NaiveDate),
    /// An hour from the time named, unless the phrase says when it ends.
    At {
        start: NaiveDateTime,
        end: NaiveDateTime,
    },
}

/// Which shortened day names a reader takes. A meeting's phrases take all of
/// them; a typed task leaves out "sat" and "sun", which are words of their
/// own; speech says day names whole.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DayNames {
    All,
    Typed,
    Spoken,
}

/// What `text` says happens, read as said at `said_at`, local time. A time
/// with no day falls on the day it was said, or on the next day when that
/// time had already passed.
pub(crate) fn resolve(text: &str, said_at: NaiveDateTime) -> Option<SpokenTime> {
    find(text, said_at, DayNames::All).map(|(when, _)| when)
}

/// [`resolve`], with the byte ranges of `text` that say it, in order.
pub(crate) fn find(
    text: &str,
    said_at: NaiveDateTime,
    names: DayNames,
) -> Option<(SpokenTime, Vec<Range<usize>>)> {
    let (normalized, origin) = normalized(text);
    let (tokens, spans) = lexed(&normalized);
    let in_text = |taken: Range<usize>| {
        origin[spans[taken.start].start].start..origin[spans[taken.end - 1].end - 1].end
    };
    let day = day_in(&tokens, said_at.date(), names);
    let Some((start, end, said)) = time_in(&tokens) else {
        let (day, taken) = day?;
        return Some((SpokenTime::Day(day), vec![in_text(taken)]));
    };
    let mut ranges = vec![in_text(said)];
    let date = match day {
        Some((day, taken)) => {
            ranges.push(in_text(taken));
            day
        }
        None if start < said_at.time() => said_at.date().succ_opt()?,
        None => said_at.date(),
    };
    ranges.sort_by_key(|range| range.start);
    let start = date.and_time(start);
    let end = match end {
        Some(end) => date.and_time(end),
        None => start.checked_add_signed(TimeDelta::hours(1))?,
    };
    Some((SpokenTime::At { start, end }, ranges))
}

/// The day `text` names, read against `today`. A time of day in it is not
/// read.
pub(crate) fn day(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    let (normalized, _) = normalized(text);
    day_in(&lexed(&normalized).0, today, DayNames::All).map(|(day, _)| day)
}

/// The first `weekday` strictly after `from`.
///
/// "Next Friday" and "Friday" land on the same day here. Colloquially they can
/// differ by a week, and a reminder a week late is worse than one a week early:
/// the earlier day is the one the operator can still act on.
pub(crate) fn next_weekday(from: NaiveDate, weekday: Weekday) -> Option<NaiveDate> {
    let ahead = (7 + weekday.num_days_from_monday() - from.weekday().num_days_from_monday()) % 7;
    from.checked_add_days(Days::new(u64::from(if ahead == 0 { 7 } else { ahead })))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Token<'a> {
    Word(&'a str),
    Number(u32),
    Ordinal(u32),
    Clock(u32, u32),
    Date(NaiveDate),
    Dash,
}

/// `text` lowercased, with everything but letters, digits, ':' and '-' made
/// a space. A dash of any width becomes '-', and a '.' between two digits
/// becomes ':', so "2.30" reads as a clock time. Beside it, for each byte,
/// the bytes of `text` it came from.
fn normalized(text: &str) -> (String, Vec<Range<usize>>) {
    let mut normalized = String::with_capacity(text.len());
    let mut origin = Vec::with_capacity(text.len());
    let mut characters = text
        .char_indices()
        .flat_map(|(at, character)| {
            let from = at..at + character.len_utf8();
            character
                .to_lowercase()
                .map(move |lower| (lower, from.clone()))
        })
        .peekable();
    let mut previous = ' ';
    while let Some((character, from)) = characters.next() {
        let kept = match character {
            '-' | '\u{2013}' | '\u{2014}' => '-',
            ':' => ':',
            '.' if previous.is_ascii_digit()
                && characters
                    .peek()
                    .is_some_and(|(next, _)| next.is_ascii_digit()) =>
            {
                ':'
            }
            character if character.is_alphanumeric() => character,
            _ => ' ',
        };
        normalized.push(kept);
        origin.extend(std::iter::repeat(from).take(kept.len_utf8()));
        previous = character;
    }
    (normalized, origin)
}

/// The tokens of `normalized`, and beside each the bytes it was read from.
fn lexed(normalized: &str) -> (Vec<Token<'_>>, Vec<Range<usize>>) {
    let mut tokens = Vec::new();
    let mut spans = Vec::new();
    let mut at = 0;
    for piece in normalized.split(' ') {
        let start = at;
        at += piece.len() + 1;
        if piece.is_empty() {
            continue;
        }
        if let Ok(date) = NaiveDate::parse_from_str(piece, "%Y-%m-%d") {
            tokens.push(Token::Date(date));
            spans.push(start..start + piece.len());
            continue;
        }
        let mut part_at = start;
        for (index, part) in piece.split('-').enumerate() {
            if index > 0 {
                tokens.push(Token::Dash);
                spans.push(part_at - 1..part_at);
            }
            let lead = part.len() - part.trim_start_matches(':').len();
            push_part(
                &mut tokens,
                &mut spans,
                part.trim_matches(':'),
                part_at + lead,
            );
            part_at += part.len() + 1;
        }
    }
    (tokens, spans)
}

/// One dash-free part of a word, starting at byte `at`: "4th" is an ordinal,
/// "3pm" a number and a word, "2:30" a clock time.
fn push_part<'a>(
    tokens: &mut Vec<Token<'a>>,
    spans: &mut Vec<Range<usize>>,
    part: &'a str,
    at: usize,
) {
    let split = part
        .find(|character: char| !character.is_ascii_digit() && character != ':')
        .unwrap_or(part.len());
    let (digits, rest) = part.split_at(split);
    match (numeral(digits), rest) {
        (Some(Token::Number(number)), "st" | "nd" | "rd" | "th") => {
            tokens.push(Token::Ordinal(number));
            spans.push(at..at + part.len());
        }
        (Some(token), _) => {
            tokens.push(token);
            spans.push(at..at + split);
            if !rest.is_empty() {
                tokens.push(Token::Word(rest));
                spans.push(at + split..at + part.len());
            }
        }
        (None, _) if !part.is_empty() => {
            tokens.push(Token::Word(part));
            spans.push(at..at + part.len());
        }
        (None, _) => {}
    }
}

fn numeral<'a>(digits: &str) -> Option<Token<'a>> {
    match digits.split_once(':') {
        Some((hour, minute)) if minute.len() == 2 => {
            let (hour, minute) = (hour.parse().ok()?, minute.parse().ok()?);
            (hour < 24 && minute < 60).then_some(Token::Clock(hour, minute))
        }
        Some(_) => None,
        None => digits.parse().ok().map(Token::Number),
    }
}

fn word_at<'a>(tokens: &[Token<'a>], index: usize) -> Option<&'a str> {
    match tokens.get(index) {
        Some(&Token::Word(word)) => Some(word),
        _ => None,
    }
}

fn word_before<'a>(tokens: &[Token<'a>], index: usize, distance: usize) -> Option<&'a str> {
    word_at(tokens, index.checked_sub(distance)?)
}

/// The first day `tokens` name, and the tokens that name it.
fn day_in(
    tokens: &[Token<'_>],
    today: NaiveDate,
    names: DayNames,
) -> Option<(NaiveDate, Range<usize>)> {
    (0..tokens.len()).find_map(|index| day_at(tokens, index, today, names))
}

fn day_at(
    tokens: &[Token<'_>],
    index: usize,
    today: NaiveDate,
    names: DayNames,
) -> Option<(NaiveDate, Range<usize>)> {
    let one = index..index + 1;
    let word = match tokens[index] {
        Token::Date(date) => return Some((date, one)),
        Token::Word(word) => word,
        _ => return None,
    };
    match word {
        "today" | "tonight" => return Some((today, one)),
        "tomorrow" => return Some((today.succ_opt()?, one)),
        "tmrw" | "tmr" if names == DayNames::Typed => return Some((today.succ_opt()?, one)),
        "yesterday" => return Some((today.pred_opt()?, one)),
        "day"
            if word_at(tokens, index + 1) == Some("after")
                && word_at(tokens, index + 2) == Some("tomorrow") =>
        {
            return Some((today.checked_add_days(Days::new(2))?, index..index + 3));
        }
        _ => {}
    }
    if let Some(weekday) = weekday(word, names) {
        if matches!(
            word_before(tokens, index, 1),
            Some("last" | "past" | "previous")
        ) {
            return Some((last_weekday(today, weekday)?, index - 1..index + 1));
        }
        if let Some(said) = said_for_next_week(tokens, index) {
            return Some((in_next_week(today, weekday)?, said));
        }
        return Some((next_weekday(today, weekday)?, one));
    }
    month_day(tokens, index, month(word)?, today)
}

/// The tokens that say the weekday at `index` as "next week Tuesday", "next
/// week on Tuesday" or "Tuesday next week".
fn said_for_next_week(tokens: &[Token<'_>], index: usize) -> Option<Range<usize>> {
    let on = usize::from(word_before(tokens, index, 1) == Some("on"));
    if word_before(tokens, index, on + 2) == Some("next")
        && word_before(tokens, index, on + 1) == Some("week")
    {
        return Some(index - on - 2..index + 1);
    }
    (word_at(tokens, index + 1) == Some("next") && word_at(tokens, index + 2) == Some("week"))
        .then_some(index..index + 3)
}

/// `weekday` in the Monday-to-Sunday week after the one `from` falls in.
fn in_next_week(from: NaiveDate, weekday: Weekday) -> Option<NaiveDate> {
    let monday = 7 - from.weekday().num_days_from_monday();
    from.checked_add_days(Days::new(u64::from(
        monday + weekday.num_days_from_monday(),
    )))
}

/// The last `weekday` strictly before `from`: "last Tuesday" said on a
/// Tuesday is a week ago.
fn last_weekday(from: NaiveDate, weekday: Weekday) -> Option<NaiveDate> {
    let back = (7 + from.weekday().num_days_from_monday() - weekday.num_days_from_monday()) % 7;
    from.checked_sub_days(Days::new(u64::from(if back == 0 { 7 } else { back })))
}

/// The date a month name at `index` names with a day number beside it:
/// "March 4", "March 4th 2027", "4 March", "4th of March". With no year, the
/// next such day on or after `today`.
fn month_day(
    tokens: &[Token<'_>],
    index: usize,
    month: u32,
    today: NaiveDate,
) -> Option<(NaiveDate, Range<usize>)> {
    let day_number = |at: usize| match tokens.get(at) {
        Some(&(Token::Number(day) | Token::Ordinal(day))) if (1..=31).contains(&day) => Some(day),
        _ => None,
    };
    let (day, year_at, from) = match day_number(index + 1) {
        Some(day) => (day, index + 2, index),
        None => {
            let of = usize::from(word_before(tokens, index, 1) == Some("of"));
            let at = index.checked_sub(of + 1)?;
            (day_number(at)?, index + 1, at)
        }
    };
    match tokens.get(year_at) {
        Some(&Token::Number(year)) if (1000..=9999).contains(&year) => Some((
            NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)?,
            from..year_at + 1,
        )),
        _ => Some((
            NaiveDate::from_ymd_opt(today.year(), month, day)
                .filter(|date| *date >= today)
                .or_else(|| NaiveDate::from_ymd_opt(today.year() + 1, month, day))?,
            from..year_at,
        )),
    }
}

fn weekday(word: &str, names: DayNames) -> Option<Weekday> {
    let (weekday, whole) = match word {
        "monday" => (Weekday::Mon, true),
        "mon" => (Weekday::Mon, false),
        "tuesday" => (Weekday::Tue, true),
        "tue" | "tues" => (Weekday::Tue, false),
        "wednesday" => (Weekday::Wed, true),
        "wed" => (Weekday::Wed, false),
        "thursday" => (Weekday::Thu, true),
        "thu" | "thurs" => (Weekday::Thu, false),
        "friday" => (Weekday::Fri, true),
        "fri" => (Weekday::Fri, false),
        "saturday" => (Weekday::Sat, true),
        "sat" => (Weekday::Sat, false),
        "sunday" => (Weekday::Sun, true),
        "sun" => (Weekday::Sun, false),
        _ => return None,
    };
    let taken = whole
        || match names {
            DayNames::All => true,
            DayNames::Typed => !matches!(word, "sat" | "sun"),
            DayNames::Spoken => false,
        };
    taken.then_some(weekday)
}

fn month(word: &str) -> Option<u32> {
    match word {
        "january" | "jan" => Some(1),
        "february" | "feb" => Some(2),
        "march" | "mar" => Some(3),
        "april" | "apr" => Some(4),
        "may" => Some(5),
        "june" | "jun" => Some(6),
        "july" | "jul" => Some(7),
        "august" | "aug" => Some(8),
        "september" | "sep" | "sept" => Some(9),
        "october" | "oct" => Some(10),
        "november" | "nov" => Some(11),
        "december" | "dec" => Some(12),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Meridiem {
    Am,
    Pm,
}

impl Meridiem {
    fn other(self) -> Self {
        match self {
            Self::Am => Self::Pm,
            Self::Pm => Self::Am,
        }
    }
}

/// An hour and minute as said, before morning or afternoon is settled.
#[derive(Clone, Copy)]
struct SaidHour {
    hour: u32,
    minute: u32,
    meridiem: Option<Meridiem>,
}

/// The first time of day `tokens` name, when it ends if they say so, and the
/// tokens that say it. "In the morning", "afternoon", "evening" or "tonight"
/// anywhere in the phrase settles a time said without "am" or "pm".
fn time_in(tokens: &[Token<'_>]) -> Option<(NaiveTime, Option<NaiveTime>, Range<usize>)> {
    let part_of_day = tokens.iter().find_map(|token| match token {
        Token::Word("morning") => Some(Meridiem::Am),
        Token::Word("afternoon" | "evening" | "tonight" | "night") => Some(Meridiem::Pm),
        _ => None,
    });
    (0..tokens.len()).find_map(|index| time_at(tokens, index, part_of_day))
}

/// A bare number is a time only when "am" or "pm" or "o'clock" follows it,
/// or "at", "around", "from" or "between" comes before it.
fn time_at(
    tokens: &[Token<'_>],
    index: usize,
    part_of_day: Option<Meridiem>,
) -> Option<(NaiveTime, Option<NaiveTime>, Range<usize>)> {
    let introduced = word_before(tokens, index, 1);
    let introducer = matches!(introduced, Some("at" | "around" | "from" | "between"));
    let from = index - usize::from(introducer);
    if matches!(tokens[index], Token::Word("noon" | "midday")) {
        return Some((NaiveTime::from_hms_opt(12, 0, 0)?, None, from..index + 1));
    }
    let (start, after) = hour_at(tokens, index)?;
    let o_clock = o_clock_at(tokens, after);
    let after = after + o_clock;
    if o_clock == 0
        && start.meridiem.is_none()
        && !matches!(tokens[index], Token::Clock(..))
        && !introducer
    {
        return None;
    }
    let end = match tokens.get(after) {
        Some(Token::Dash | Token::Word("to" | "until" | "till" | "til")) => {
            hour_at(tokens, after + 1)
        }
        Some(Token::Word("and")) if introduced == Some("between") => hour_at(tokens, after + 1),
        _ => None,
    };
    let start_meridiem = start
        .meridiem
        .or_else(|| {
            let (end, _) = end?;
            let meridiem = end.meridiem?;
            Some(if start.hour % 12 > end.hour % 12 {
                meridiem.other()
            } else {
                meridiem
            })
        })
        .or(part_of_day);
    let start_time = clock(start.hour, start.minute, start_meridiem)?;
    let end_time = end.and_then(|(end, _)| {
        let time = clock(end.hour, end.minute, end.meridiem.or(part_of_day))?;
        if time > start_time {
            return Some(time);
        }
        if end.meridiem.is_none() && end.hour < 12 {
            return clock(end.hour + 12, end.minute, None).filter(|time| *time > start_time);
        }
        None
    });
    let through = match (end_time, end) {
        (Some(_), Some((_, next))) => next,
        _ => after,
    };
    Some((start_time, end_time, from..through))
}

/// The hour said at `index`, as a numeral, a clock time or a word, with "am"
/// or "pm" when one follows; and the index after it.
fn hour_at(tokens: &[Token<'_>], index: usize) -> Option<(SaidHour, usize)> {
    let (hour, minute) = match *tokens.get(index)? {
        Token::Clock(hour, minute) => (hour, minute),
        Token::Number(hour) => (hour, 0),
        Token::Word(word) => (hour_word(word)?, 0),
        _ => return None,
    };
    let (meridiem, used) = match meridiem_at(tokens, index + 1) {
        Some((meridiem, used)) => (Some(meridiem), used),
        None => (None, 0),
    };
    Some((
        SaidHour {
            hour,
            minute,
            meridiem,
        },
        index + 1 + used,
    ))
}

fn meridiem_at(tokens: &[Token<'_>], index: usize) -> Option<(Meridiem, usize)> {
    match (word_at(tokens, index)?, word_at(tokens, index + 1)) {
        ("am", _) => Some((Meridiem::Am, 1)),
        ("pm", _) => Some((Meridiem::Pm, 1)),
        ("a", Some("m")) => Some((Meridiem::Am, 2)),
        ("p", Some("m")) => Some((Meridiem::Pm, 2)),
        _ => None,
    }
}

fn o_clock_at(tokens: &[Token<'_>], index: usize) -> usize {
    match (word_at(tokens, index), word_at(tokens, index + 1)) {
        (Some("oclock"), _) => 1,
        (Some("o"), Some("clock")) => 2,
        _ => 0,
    }
}

fn hour_word(word: &str) -> Option<u32> {
    let hour = match word {
        "one" => 1,
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
        _ => return None,
    };
    Some(hour)
}

/// The time `hour:minute` names, morning or afternoon as said. Unsaid, 1 to 7
/// is the afternoon and 8 to 12 the morning or noon, the hours meetings keep;
/// 13 to 23 read as a 24-hour clock.
fn clock(hour: u32, minute: u32, meridiem: Option<Meridiem>) -> Option<NaiveTime> {
    let hour = match (hour, meridiem) {
        (0 | 13..=23, _) => hour,
        (1..=12, Some(Meridiem::Am)) => hour % 12,
        (1..=12, Some(Meridiem::Pm)) => hour % 12 + 12,
        (1..=7, None) => hour + 12,
        (8..=12, None) => hour,
        _ => return None,
    };
    NaiveTime::from_hms_opt(hour, minute, 0)
}

#[cfg(test)]
mod tests {
    use super::{find, resolve, DayNames, SpokenTime};
    use chrono::NaiveDate;

    fn on(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("a real day")
    }

    fn hours(day: NaiveDate, from: (u32, u32), to: (u32, u32)) -> Option<SpokenTime> {
        Some(SpokenTime::At {
            start: day.and_hms_opt(from.0, from.1, 0)?,
            end: day.and_hms_opt(to.0, to.1, 0)?,
        })
    }

    /// Each phrase read as said on Monday 2 November 2026 at 10:00. A wrong
    /// answer here is a plan put on the calendar on the wrong day.
    macro_rules! read_as_said_on_a_monday_at_ten {
        ($($name:ident: $text:expr => $want:expr;)*) => { $(
            #[test]
            fn $name() {
                let said = on(2026, 11, 2).and_hms_opt(10, 0, 0).expect("a real time");
                assert_eq!(resolve($text, said), $want, "{:?}", $text);
            }
        )* };
    }

    read_as_said_on_a_monday_at_ten! {
        a_weekday_and_an_hour_is_that_afternoon_this_week:
            "Thursday at 2" => hours(on(2026, 11, 5), (14, 0), (15, 0));
        tomorrow_at_noon_is_midday_tomorrow:
            "tomorrow at noon" => hours(on(2026, 11, 3), (12, 0), (13, 0));
        an_end_that_is_said_is_kept:
            "from 2 to 3:30" => hours(on(2026, 11, 2), (14, 0), (15, 30));
        an_hour_already_gone_today_is_tomorrow:
            "at 9" => hours(on(2026, 11, 3), (9, 0), (10, 0));
        in_the_morning_settles_the_hour:
            "Friday at 7 in the morning" => hours(on(2026, 11, 6), (7, 0), (8, 0));
        next_week_names_the_week_after_this_one:
            "next week Tuesday" => Some(SpokenTime::Day(on(2026, 11, 10)));
        a_month_day_already_gone_this_year_is_next_year:
            "March 4th" => Some(SpokenTime::Day(on(2027, 3, 4)));
        last_weekday_is_behind_not_ahead:
            "last Tuesday at 3" => hours(on(2026, 10, 27), (15, 0), (16, 0));
        a_number_no_time_word_comes_with_is_not_a_time:
            "the churn sheet has 3 tabs" => None;
        another_language_names_nothing:
            "nächsten Freitag um 3" => None;
    }

    #[test]
    fn found_ranges_are_the_bytes_of_the_text_that_say_it() {
        let said = on(2026, 11, 2).and_hms_opt(10, 0, 0).expect("a real time");
        let text = "“Launch” review Friday at 3pm";

        let (when, ranges) = find(text, said, DayNames::Typed).expect("a time");

        assert_eq!(Some(when), hours(on(2026, 11, 6), (15, 0), (16, 0)));
        let words: Vec<&str> = ranges.iter().map(|range| &text[range.clone()]).collect();
        assert_eq!(words, ["Friday", "at 3pm"]);
    }

    #[test]
    fn typed_sat_and_sun_are_words_not_days() {
        let said = on(2026, 11, 2).and_hms_opt(10, 0, 0).expect("a real time");

        assert_eq!(find("Sat with the new hires", said, DayNames::Typed), None);
        assert_eq!(
            find("Sat with the new hires", said, DayNames::All).map(|(when, _)| when),
            Some(SpokenTime::Day(on(2026, 11, 7)))
        );
    }
}
