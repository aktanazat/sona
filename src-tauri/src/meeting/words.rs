//! The words of a line the way the notes' checks compare them.

pub(crate) const SPELLED_UNITS: [[&str; 2]; 20] = [
    ["zero", "zeroth"],
    ["one", "first"],
    ["two", "second"],
    ["three", "third"],
    ["four", "fourth"],
    ["five", "fifth"],
    ["six", "sixth"],
    ["seven", "seventh"],
    ["eight", "eighth"],
    ["nine", "ninth"],
    ["ten", "tenth"],
    ["eleven", "eleventh"],
    ["twelve", "twelfth"],
    ["thirteen", "thirteenth"],
    ["fourteen", "fourteenth"],
    ["fifteen", "fifteenth"],
    ["sixteen", "sixteenth"],
    ["seventeen", "seventeenth"],
    ["eighteen", "eighteenth"],
    ["nineteen", "nineteenth"],
];

pub(crate) const SPELLED_TENS: [[&str; 2]; 8] = [
    ["twenty", "twentieth"],
    ["thirty", "thirtieth"],
    ["forty", "fortieth"],
    ["fifty", "fiftieth"],
    ["sixty", "sixtieth"],
    ["seventy", "seventieth"],
    ["eighty", "eightieth"],
    ["ninety", "ninetieth"],
];

/// The words of `text` in lower case without punctuation, every number
/// among them in digits: a model writes the "Thursday the thirteenth" it
/// read as "Thursday the 13th", and both are the same words here.
pub(crate) fn normalized_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut open_tens: Option<usize> = None;
    for word in text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        let word = word.to_lowercase();
        let units = SPELLED_UNITS
            .iter()
            .position(|names| names.contains(&word.as_str()));
        if let (Some(tens), Some(units @ 1..=9)) = (open_tens, units) {
            words.pop();
            words.push((tens + units).to_string());
            open_tens = None;
            continue;
        }
        let tens = SPELLED_TENS
            .iter()
            .position(|names| names.contains(&word.as_str()))
            .map(|index| (20 + 10 * index, SPELLED_TENS[index][0] == word));
        open_tens = tens.and_then(|(value, cardinal)| cardinal.then_some(value));
        words.push(match (units, tens) {
            (Some(units), _) => units.to_string(),
            (None, Some((value, _))) => value.to_string(),
            (None, None) => written_number(&word).unwrap_or(word),
        });
    }
    words
}

/// `13`, `13th` or `013` as `13`; None for a word that is not a number.
fn written_number(word: &str) -> Option<String> {
    let digits = ["st", "nd", "rd", "th"]
        .into_iter()
        .find_map(|suffix| word.strip_suffix(suffix))
        .unwrap_or(word);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok().map(|number| number.to_string())
}

/// "I'll", "I will", "I can", "I'm going to", "I'm gonna", "let me": words
/// that promise the speaker's own work, unless a negation follows. "I'm
/// gonna need", "say" or "guess" asks or remarks instead of taking a task on.
pub(crate) fn commits(words: &[String]) -> bool {
    words.windows(2).enumerate().any(|(index, pair)| {
        let next = words.get(index + 2).map(String::as_str);
        let negated = matches!(next, Some("t" | "not" | "never"));
        match (pair[0].as_str(), pair[1].as_str()) {
            ("i", "ll" | "will" | "can" | "shall") => !negated,
            ("i", "m" | "am") => {
                let verb = match next {
                    Some("gonna") => words.get(index + 3),
                    Some("going") if words.get(index + 3).map(String::as_str) == Some("to") => {
                        words.get(index + 4)
                    }
                    Some("going") => None,
                    _ => return false,
                };
                !matches!(verb.map(String::as_str), Some("need" | "say" | "guess"))
            }
            ("let", "me") => true,
            _ => false,
        }
    })
}

pub(crate) fn says_words(text: &[String], phrase: &[String]) -> bool {
    !phrase.is_empty() && text.windows(phrase.len()).any(|words| words == phrase)
}
