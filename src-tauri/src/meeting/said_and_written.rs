//! What was said in a meeting against what its document says.
//!
//! The document's amounts of money, spans of time and percentages are each
//! about something, named by the words around them: "sixty (60) days'
//! written notice". A quantity said in the same unit, sharing a word that
//! few of the document's quantities have, is about the same thing. It
//! agrees when any such quantity has its value; otherwise it differs from
//! the one it shares most with, unless two of those disagree with each
//! other. The last thing said about a passage stands, so a slip corrected
//! later in the meeting is no difference.

use super::words::{SPELLED_TENS, SPELLED_UNITS};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Words either side of a quantity, within its sentence, that can say what
/// it is about.
const CONTEXT_WORDS: usize = 8;
/// Words either side of a quantity kept in a passage or a quote.
const EXCERPT_WORDS: usize = 16;
/// What a spoken quantity has to share with a written one to be about the
/// same thing: a word only one or two of the document's quantities have,
/// or several words that a few have. A word near every quantity, like
/// "tenant" in a lease, counts for little.
const MATCH_WEIGHT: f64 = 0.5;

const STOPWORDS: &[&str] = &[
    "about",
    "above",
    "actually",
    "after",
    "again",
    "against",
    "all",
    "also",
    "and",
    "any",
    "are",
    "around",
    "back",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "can",
    "could",
    "did",
    "does",
    "doing",
    "done",
    "down",
    "during",
    "each",
    "either",
    "else",
    "even",
    "every",
    "few",
    "for",
    "from",
    "further",
    "get",
    "gets",
    "getting",
    "give",
    "given",
    "going",
    "gonna",
    "good",
    "got",
    "had",
    "has",
    "have",
    "having",
    "her",
    "here",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "how",
    "into",
    "its",
    "itself",
    "just",
    "know",
    "least",
    "less",
    "let",
    "like",
    "made",
    "make",
    "many",
    "may",
    "mean",
    "might",
    "more",
    "most",
    "much",
    "must",
    "need",
    "never",
    "next",
    "nor",
    "not",
    "now",
    "off",
    "okay",
    "once",
    "one",
    "only",
    "other",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "per",
    "prior",
    "really",
    "right",
    "said",
    "same",
    "say",
    "says",
    "shall",
    "she",
    "should",
    "since",
    "some",
    "such",
    "sure",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "thing",
    "things",
    "think",
    "this",
    "those",
    "through",
    "under",
    "until",
    "upon",
    "very",
    "want",
    "was",
    "way",
    "well",
    "were",
    "what",
    "when",
    "where",
    "whether",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "within",
    "would",
    "yeah",
    "yes",
    "yet",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Unit {
    Days,
    BusinessDays,
    Months,
    Minutes,
    Dollars,
    Euros,
    Pounds,
    Percent,
}

/// A spoken quantity that differs from the document, as byte ranges into
/// the page it differs from and the transcript line it was said in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Difference {
    pub page: usize,
    /// The words around the written quantity.
    pub passage: Range<usize>,
    pub written: Range<usize>,
    pub line: usize,
    /// The words around the spoken quantity.
    pub quote: Range<usize>,
    pub said: Range<usize>,
}

/// Every passage of `pages` that what was said, in `lines` in the order
/// they were said, last differs from. In page order.
pub(crate) fn differences(pages: &[&str], lines: &[&str]) -> Vec<Difference> {
    let written: Vec<(usize, Quantity)> = pages
        .iter()
        .enumerate()
        .flat_map(|(page, text)| {
            quantities(text)
                .into_iter()
                .map(move |quantity| (page, quantity))
        })
        .collect();
    let mut sharing: HashMap<&str, u32> = HashMap::new();
    for (_, quantity) in &written {
        for term in &quantity.terms {
            *sharing.entry(term.as_str()).or_default() += 1;
        }
    }
    let mut last_said: Vec<Option<(usize, Quantity)>> = written.iter().map(|_| None).collect();
    let mut before: HashSet<String> = HashSet::new();
    for (line, text) in lines.iter().enumerate() {
        for mut said in quantities(text) {
            if said.sentence == 0 {
                said.terms.extend(before.iter().cloned());
            }
            let candidates: Vec<(usize, f64)> = written
                .iter()
                .enumerate()
                .filter(|(_, (_, quantity))| quantity.unit == said.unit)
                .map(|(index, (_, quantity))| {
                    let weight = said
                        .terms
                        .intersection(&quantity.terms)
                        .map(|term| 1.0 / f64::from(sharing[term.as_str()]))
                        .sum::<f64>();
                    (index, weight)
                })
                .filter(|(_, weight)| *weight >= MATCH_WEIGHT)
                .collect();
            if let Some(&(index, _)) = candidates
                .iter()
                .find(|(index, _)| same(written[*index].1.value, said.value))
            {
                last_said[index] = None;
                continue;
            }
            let Some(best) = candidates
                .iter()
                .map(|(_, weight)| *weight)
                .reduce(f64::max)
            else {
                continue;
            };
            let closest: Vec<usize> = candidates
                .iter()
                .filter(|(_, weight)| *weight >= best)
                .map(|(index, _)| *index)
                .collect();
            let value = written[closest[0]].1.value;
            if closest
                .iter()
                .all(|index| same(written[*index].1.value, value))
            {
                last_said[closest[0]] = Some((line, said));
            }
        }
        before = closing_terms(text);
    }
    written
        .iter()
        .zip(last_said)
        .filter_map(|((page, quantity), said)| {
            let (line, said) = said?;
            Some(Difference {
                page: *page,
                passage: quantity.excerpt.clone(),
                written: quantity.span.clone(),
                line,
                quote: said.excerpt,
                said: said.span,
            })
        })
        .collect()
}

fn same(left: f64, right: f64) -> bool {
    (left - right).abs() <= 1e-9 * left.abs().max(right.abs()).max(1.0)
}

/// One quantity in a text: its unit, its value in the unit's base, where it
/// is, and the words around it.
struct Quantity {
    unit: Unit,
    value: f64,
    span: Range<usize>,
    excerpt: Range<usize>,
    sentence: usize,
    terms: HashSet<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Word(String),
    Number(f64),
    Mark(char),
    /// A blank line, a page break: the end of a sentence with no stop.
    Break,
}

struct Token {
    kind: Kind,
    span: Range<usize>,
}

impl Token {
    fn word(&self) -> Option<&str> {
        match &self.kind {
            Kind::Word(word) => Some(word),
            _ => None,
        }
    }

    fn ends_sentence(&self) -> bool {
        matches!(self.kind, Kind::Mark('.' | '!' | '?' | ';') | Kind::Break)
    }
}

fn quantities(text: &str) -> Vec<Quantity> {
    let tokens = tokens(text);
    let sentences = sentences(&tokens);
    let mut found = Vec::new();
    for (sentence, range) in sentences.iter().enumerate() {
        let mut at = range.start;
        while at < range.end {
            let Some((unit, value, end)) = quantity_at(&tokens, at) else {
                at += 1;
                continue;
            };
            found.push(Quantity {
                unit,
                value,
                span: tokens[at].span.start..tokens[end - 1].span.end,
                excerpt: excerpt(&tokens, range, at..end),
                sentence,
                terms: terms(&tokens, range, at..end),
            });
            at = end;
        }
    }
    found
}

/// The words that name what the last sentence of `text` is about: what a
/// short answer on the next line is answering.
fn closing_terms(text: &str) -> HashSet<String> {
    let tokens = tokens(text);
    sentences(&tokens)
        .last()
        .map_or_else(HashSet::new, |range| {
            let from = range.end.saturating_sub(2 * CONTEXT_WORDS).max(range.start);
            (from..range.end)
                .filter_map(|index| tokens[index].word().and_then(term))
                .collect()
        })
}

fn tokens(text: &str) -> Vec<Token> {
    let characters: Vec<(usize, char)> = text.char_indices().collect();
    let offset = |index: usize| characters.get(index).map_or(text.len(), |(at, _)| *at);
    let digit = |index: usize| {
        characters
            .get(index)
            .is_some_and(|(_, character)| character.is_ascii_digit())
    };
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        let (start, character) = characters[index];
        if character.is_whitespace() {
            let mut newlines = 0;
            while let Some((_, space)) = characters.get(index).filter(|(_, c)| c.is_whitespace()) {
                newlines += usize::from(*space == '\n') + 2 * usize::from(*space == '\u{c}');
                index += 1;
            }
            if newlines >= 2 {
                tokens.push(Token {
                    kind: Kind::Break,
                    span: start..offset(index),
                });
            }
        } else if character.is_ascii_digit() {
            let mut number = String::new();
            let mut point = false;
            while index < characters.len() {
                let character = characters[index].1;
                let thousands = character == ','
                    && !point
                    && (1..=3).all(|ahead| digit(index + ahead))
                    && !digit(index + 4);
                let decimal = character == '.' && !point && digit(index + 1);
                if character.is_ascii_digit() || decimal {
                    number.push(character);
                    point |= decimal;
                } else if !thousands {
                    break;
                }
                index += 1;
            }
            if let Ok(value) = number.parse() {
                tokens.push(Token {
                    kind: Kind::Number(value),
                    span: start..offset(index),
                });
            }
        } else if character.is_alphabetic() {
            let mut word = String::new();
            while index < characters.len() {
                let character = characters[index].1;
                let letter_follows = characters
                    .get(index + 1)
                    .is_some_and(|(_, next)| next.is_alphabetic());
                if character.is_alphabetic() {
                    word.extend(character.to_lowercase());
                } else if matches!(character, '\'' | '’') && letter_follows && !word.is_empty() {
                    word.push('\'');
                } else {
                    break;
                }
                index += 1;
            }
            tokens.push(Token {
                kind: Kind::Word(word),
                span: start..offset(index),
            });
        } else {
            index += 1;
            tokens.push(Token {
                kind: Kind::Mark(character),
                span: start..offset(index),
            });
        }
    }
    tokens
}

/// Each sentence as the tokens between its stops.
fn sentences(tokens: &[Token]) -> Vec<Range<usize>> {
    let mut sentences = Vec::new();
    let mut start = 0;
    for (index, token) in tokens.iter().enumerate() {
        if token.ends_sentence() {
            if index > start {
                sentences.push(start..index);
            }
            start = index + 1;
        }
    }
    if tokens.len() > start {
        sentences.push(start..tokens.len());
    }
    sentences
}

/// A quantity starting at `at`: its unit, its value in the unit's base and
/// the token after it. "sixty (60) days", "$2,500.00", "Two Thousand
/// Dollars ($2,000)", "a month", "5%".
fn quantity_at(tokens: &[Token], at: usize) -> Option<(Unit, f64, usize)> {
    let currency = match tokens[at].kind {
        Kind::Mark('$') => Some(Unit::Dollars),
        Kind::Mark('€') => Some(Unit::Euros),
        Kind::Mark('£') => Some(Unit::Pounds),
        _ => None,
    };
    let start = at + usize::from(currency.is_some());
    let (value, mut next) = number_at(tokens, start)?;
    next = past_restated_number(tokens, next, value);
    if matches!(
        tokens.get(next).map(|token| &token.kind),
        Some(Kind::Mark('-'))
    ) {
        next += 1;
    }
    let (unit, value, end) = match (unit_at(tokens, next), currency) {
        (Some((unit, factor, end)), None) => (unit, value * factor, end),
        (Some((unit, _, end)), Some(currency)) if unit == currency => (unit, value, end),
        (_, Some(currency)) => (currency, value, next),
        (None, None) => return None,
    };
    Some((
        unit,
        value,
        past_restated_quantity(tokens, end, unit, value),
    ))
}

/// The token after "(60)" at `at` when it restates `value`, else `at`.
fn past_restated_number(tokens: &[Token], at: usize, value: f64) -> usize {
    match tokens.get(at..at + 3) {
        Some([open, number, close])
            if open.kind == Kind::Mark('(')
                && close.kind == Kind::Mark(')')
                && matches!(number.kind, Kind::Number(restated) if same(restated, value)) =>
        {
            at + 3
        }
        _ => at,
    }
}

/// The token after "($2,000)" at `at` when it restates the quantity just
/// read, else `at`.
fn past_restated_quantity(tokens: &[Token], at: usize, unit: Unit, value: f64) -> usize {
    if tokens.get(at).map(|token| &token.kind) != Some(&Kind::Mark('(')) {
        return at;
    }
    match quantity_at(tokens, at + 1) {
        Some((restated_unit, restated, end))
            if restated_unit == unit
                && same(restated, value)
                && tokens.get(end).map(|token| &token.kind) == Some(&Kind::Mark(')')) =>
        {
            end + 1
        }
        _ => at,
    }
}

/// A number in digits or in words at `at`, and the token after it.
fn number_at(tokens: &[Token], at: usize) -> Option<(f64, usize)> {
    let token = tokens.get(at)?;
    match &token.kind {
        Kind::Number(value) => {
            let next = tokens.get(at + 1);
            let thousands = next
                .is_some_and(|next| next.word() == Some("k") && next.span.start == token.span.end);
            match next.and_then(Token::word).and_then(scale) {
                Some(scale) => Some((value * scale, at + 2)),
                None if thousands => Some((value * 1_000.0, at + 2)),
                None => Some((*value, at + 1)),
            }
        }
        Kind::Word(_) => spelled_at(tokens, at),
        Kind::Mark(_) | Kind::Break => None,
    }
}

/// What the last word of a spelled number was, which decides what may
/// follow it.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Spelled {
    Nothing,
    Units,
    Tens,
    Hundred,
    Scale,
    Join,
}

/// "twenty-five", "two thousand five hundred", "one hundred and ten", "a
/// hundred", and "a" or "an" before a unit: "a month".
fn spelled_at(tokens: &[Token], at: usize) -> Option<(f64, usize)> {
    let value_at = |index: usize| tokens.get(index).and_then(Token::word).and_then(spelled);
    let digit = |value: f64| (1.0..10.0).contains(&value);
    let mut total = 0.0;
    let mut current: f64 = 0.0;
    let mut last = Spelled::Nothing;
    let mut index = at;
    while let Some(token) = tokens.get(index) {
        let word = token.word();
        let opens = matches!(
            last,
            Spelled::Nothing | Spelled::Hundred | Spelled::Scale | Spelled::Join
        );
        last = match (word, value_at(index), word.and_then(scale)) {
            (_, Some(value), _)
                if value < 20.0 && (opens || last == Spelled::Tens && digit(value)) =>
            {
                current += value;
                Spelled::Units
            }
            (_, Some(value), _) if value >= 20.0 && opens => {
                current += value;
                Spelled::Tens
            }
            (Some("hundred"), _, _) if matches!(last, Spelled::Nothing | Spelled::Units) => {
                current = current.max(1.0) * 100.0;
                Spelled::Hundred
            }
            (_, _, Some(factor)) if !matches!(last, Spelled::Scale | Spelled::Join) => {
                total += current.max(1.0) * factor;
                current = 0.0;
                Spelled::Scale
            }
            (Some("a" | "an"), _, _)
                if last == Spelled::Nothing && leads_quantity(tokens, index + 1) =>
            {
                current = 1.0;
                Spelled::Units
            }
            (Some("and"), _, _)
                if matches!(last, Spelled::Hundred | Spelled::Scale)
                    && value_at(index + 1).is_some() =>
            {
                Spelled::Join
            }
            (None, _, _)
                if token.kind == Kind::Mark('-')
                    && last == Spelled::Tens
                    && value_at(index + 1).is_some_and(digit) =>
            {
                Spelled::Join
            }
            _ => break,
        };
        index += 1;
    }
    (index > at).then_some((total + current, index))
}

fn spelled(word: &str) -> Option<f64> {
    SPELLED_UNITS
        .iter()
        .zip(0_u8..)
        .find(|(names, _)| names[0] == word)
        .map(|(_, units)| f64::from(units))
        .or_else(|| {
            SPELLED_TENS
                .iter()
                .zip(0_u8..)
                .find(|(names, _)| names[0] == word)
                .map(|(_, tens)| 20.0 + 10.0 * f64::from(tens))
        })
}

fn scale(word: &str) -> Option<f64> {
    match word {
        "thousand" => Some(1e3),
        "million" => Some(1e6),
        "billion" => Some(1e9),
        _ => None,
    }
}

/// Whether "a" before `at` is a count of one: "a month", "a business day",
/// "a hundred".
fn leads_quantity(tokens: &[Token], at: usize) -> bool {
    tokens.get(at).and_then(Token::word).is_some_and(|word| {
        word == "hundred"
            || scale(word).is_some()
            || matches!(word, "business" | "working" | "calendar")
            || unit_word(word).is_some()
    })
}

/// The unit at `at`, the factor to its base, and the token after it.
fn unit_at(tokens: &[Token], at: usize) -> Option<(Unit, f64, usize)> {
    let token = tokens.get(at)?;
    if token.kind == Kind::Mark('%') {
        return Some((Unit::Percent, 1.0, at + 1));
    }
    let next = tokens.get(at + 1).and_then(Token::word);
    match token.word()? {
        "business" | "working" if matches!(next, Some("day" | "days" | "day's")) => {
            Some((Unit::BusinessDays, 1.0, at + 2))
        }
        "calendar" => next
            .and_then(unit_word)
            .map(|(unit, factor)| (unit, factor, at + 2)),
        "per" if next == Some("cent") => Some((Unit::Percent, 1.0, at + 2)),
        word => unit_word(word).map(|(unit, factor)| (unit, factor, at + 1)),
    }
}

fn unit_word(word: &str) -> Option<(Unit, f64)> {
    let word = word.strip_suffix("'s").unwrap_or(word);
    let word = word.strip_suffix('s').unwrap_or(word);
    match word {
        "day" => Some((Unit::Days, 1.0)),
        "week" => Some((Unit::Days, 7.0)),
        "month" => Some((Unit::Months, 1.0)),
        "year" => Some((Unit::Months, 12.0)),
        "hour" => Some((Unit::Minutes, 60.0)),
        "minute" => Some((Unit::Minutes, 1.0)),
        "dollar" | "buck" | "usd" => Some((Unit::Dollars, 1.0)),
        "euro" | "eur" => Some((Unit::Euros, 1.0)),
        "pound" | "gbp" => Some((Unit::Pounds, 1.0)),
        "percent" => Some((Unit::Percent, 1.0)),
        _ => None,
    }
}

/// The words of `sentence` around the quantity at `quantity`, as the text
/// they cover, ending on the sentence's stop when they reach it.
fn excerpt(tokens: &[Token], sentence: &Range<usize>, quantity: Range<usize>) -> Range<usize> {
    let from = quantity
        .start
        .saturating_sub(EXCERPT_WORDS)
        .max(sentence.start);
    let to = (quantity.end + EXCERPT_WORDS).min(sentence.end);
    let end = match tokens.get(to) {
        Some(stop) if to == sentence.end && matches!(stop.kind, Kind::Mark('.' | '!' | '?')) => {
            stop.span.end
        }
        _ => tokens[to - 1].span.end,
    };
    tokens[from].span.start..end
}

/// The words near the quantity at `quantity` that can name what it is
/// about.
fn terms(tokens: &[Token], sentence: &Range<usize>, quantity: Range<usize>) -> HashSet<String> {
    let before = tokens[sentence.start..quantity.start]
        .iter()
        .rev()
        .filter_map(Token::word)
        .take(CONTEXT_WORDS);
    let after = tokens[quantity.end..sentence.end]
        .iter()
        .filter_map(Token::word)
        .take(CONTEXT_WORDS);
    before.chain(after).filter_map(term).collect()
}

/// A word as it is compared: no possessive, no plural, None for a word that
/// names nothing.
fn term(word: &str) -> Option<String> {
    let word = word.strip_suffix("'s").unwrap_or(word);
    if word.chars().count() < 3
        || STOPWORDS.contains(&word)
        || spelled(word).is_some()
        || word == "hundred"
        || scale(word).is_some()
    {
        return None;
    }
    let stem = match word.strip_suffix('s') {
        Some(stem) if word.len() > 3 && !stem.ends_with('s') => stem,
        _ => word,
    };
    Some(stem.to_string())
}

#[cfg(test)]
mod tests {
    use super::differences;

    const LEASE: [&str; 2] = [
        "Residential Lease\nThe rent is $2,500 per month.",
        "Tenant shall give Landlord sixty (60) days’ written notice\nbefore moving out. \
         The security deposit is Two Thousand Five Hundred Dollars ($2,500).",
    ];

    #[test]
    fn a_spoken_amount_the_document_says_otherwise_is_found_on_its_page() {
        let lines = [
            "Okay, the rent looks fine.",
            "So thirty days notice is fine, right?",
        ];

        let found = differences(&LEASE, &lines);

        assert_eq!(found.len(), 1);
        let difference = &found[0];
        assert_eq!(difference.page, 1);
        assert_eq!(&LEASE[1][difference.written.clone()], "sixty (60) days");
        assert_eq!(
            &LEASE[1][difference.passage.clone()],
            "Tenant shall give Landlord sixty (60) days’ written notice\nbefore moving out."
        );
        assert_eq!(difference.line, 1);
        assert_eq!(&lines[1][difference.said.clone()], "thirty days");
        assert_eq!(&lines[1][difference.quote.clone()], lines[1]);
    }

    #[test]
    fn what_agrees_or_is_about_something_else_is_no_difference() {
        let lines = [
            "Sixty days notice works for us.",
            "And the deposit was 2,500 dollars.",
            "I'll be away for thirty days in March.",
            "How much is the rent?",
            "2,500 a month.",
        ];

        assert_eq!(differences(&LEASE, &lines), Vec::new());
    }

    #[test]
    fn a_slip_corrected_later_in_the_meeting_is_no_difference() {
        let corrected = ["So thirty days notice?", "Sorry, sixty days notice."];
        let slipped = ["So sixty days notice?", "Sorry, thirty days notice."];

        assert_eq!(differences(&LEASE, &corrected), Vec::new());
        assert_eq!(differences(&LEASE, &slipped).len(), 1);
    }
}
