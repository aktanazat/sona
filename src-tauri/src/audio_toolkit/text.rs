use crate::settings::{EmojiReplacement, EnglishSpelling, ReplacementRule, VocabularyEntry};
use natural::phonetics::soundex;
use once_cell::sync::Lazy;
use regex::Regex;
use strsim::levenshtein;

/// Builds an n-gram string by cleaning and concatenating words.
fn build_ngram(words: &[&str]) -> String {
    words
        .iter()
        .map(|word| build_match_key(word))
        .collect::<Vec<_>>()
        .concat()
}

fn build_match_key(word: &str) -> String {
    word.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect()
}

pub(crate) fn vocabulary_spoken_key(spoken: &str) -> String {
    build_match_key(spoken)
}
fn starts_with_spoken_character(candidate: &str, entry: &VocabularyEntry) -> bool {
    let Some(candidate_first) = candidate.chars().next() else {
        return false;
    };
    let Some(spoken_first) = entry
        .spoken
        .chars()
        .find(|character| character.is_alphanumeric())
    else {
        return false;
    };
    candidate_first == spoken_first.to_ascii_lowercase()
}

struct VocabularyMatchKey {
    entry_index: usize,
    key: String,
}

fn build_vocabulary_match_keys(
    entry: &VocabularyEntry,
    entry_index: usize,
) -> Vec<VocabularyMatchKey> {
    let primary_key = build_match_key(&entry.spoken);
    let mut keys = Vec::with_capacity(2);

    // The fallback matcher is intentionally limited to ASCII terms. Its
    // whitespace tokenization and Soundex scoring are not suitable for CJK
    // scripts. Unicode entries still participate in Whisper prompt biasing.
    if is_supported_fuzzy_key(&primary_key) {
        keys.push(VocabularyMatchKey {
            entry_index,
            key: primary_key.clone(),
        });
    }

    if entry.spoken.contains('&') {
        let expanded_key = build_match_key(&entry.spoken.replace('&', " and "));
        if is_supported_fuzzy_key(&expanded_key) && expanded_key != primary_key {
            keys.push(VocabularyMatchKey {
                entry_index,
                key: expanded_key,
            });
        }
    }

    keys
}

fn is_supported_fuzzy_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
}

fn supports_soundex(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|character| character.is_ascii_alphabetic())
}

fn fuzzy_count_as_f64(value: usize) -> Option<f64> {
    u32::try_from(value).ok().map(f64::from)
}

/// Finds the best spoken-form match for a candidate string.
fn find_best_vocabulary_match<'a>(
    candidate: &str,
    entries: &'a [VocabularyEntry],
    match_keys: &[VocabularyMatchKey],
    threshold: f64,
) -> Option<(&'a VocabularyEntry, f64)> {
    if !is_supported_fuzzy_key(candidate) || candidate.chars().count() > 50 {
        return None;
    }

    let mut best_match: Option<&VocabularyEntry> = None;
    let mut best_score = f64::MAX;

    for match_key in match_keys {
        let candidate_len = candidate.chars().count();
        let spoken_len = match_key.key.chars().count();
        let Some(len_diff) = fuzzy_count_as_f64(candidate_len.abs_diff(spoken_len)) else {
            continue;
        };
        let Some(max_len) = fuzzy_count_as_f64(candidate_len.max(spoken_len)) else {
            continue;
        };
        let max_allowed_diff = (max_len * 0.25).max(2.0);
        if len_diff > max_allowed_diff {
            continue;
        }

        let levenshtein_score = if max_len > 0.0 {
            let Some(distance) = fuzzy_count_as_f64(levenshtein(candidate, &match_key.key)) else {
                continue;
            };
            distance / max_len
        } else {
            1.0
        };
        // Admission is decided on spelling alone, because spelling is what
        // `word_correction_threshold` names. Letting the Soundex bonus into
        // this comparison stretched the user's threshold by 3.3x: at 0.18 it
        // admitted the candidate "some" against the entry "Sona" (levenshtein
        // 2 of 4 characters, both Soundex S500, 0.5 * 0.3 = 0.15) and rewrote
        // a common English word in unrelated dictation. A phonetic tie is
        // corroboration for a near-miss spelling, not a licence to replace a
        // different word, so it only ranks the candidates that already passed.
        if levenshtein_score >= threshold {
            continue;
        }
        let phonetic_match = supports_soundex(candidate)
            && supports_soundex(&match_key.key)
            && soundex(candidate, &match_key.key);
        let combined_score = if phonetic_match {
            levenshtein_score * 0.3
        } else {
            levenshtein_score
        };

        if combined_score < best_score {
            best_match = Some(&entries[match_key.entry_index]);
            best_score = combined_score;
        }
    }

    best_match.map(|entry| (entry, best_score))
}

/// Applies deterministic fuzzy corrections from spoken forms to written forms.
/// The existing threshold, three-token n-gram ceiling, punctuation boundary,
/// and length guards remain the sole fuzzy matching policy.
///
/// Correction runs per line. Stages before this one legitimately emit line
/// breaks (spoken punctuation writes `\n` and `\n\n`, and so can a user
/// replacement rule), and this stage rebuilds its output from whitespace-split
/// tokens, so without the split it would silently rejoin those breaks as
/// spaces. Confining a match to one line is also correct on its own terms: a
/// line end is at least as strong a phrase boundary as a space, so an n-gram
/// should never span one.
pub fn apply_vocabulary_entries(text: &str, entries: &[VocabularyEntry], threshold: f64) -> String {
    if entries.is_empty() {
        return text.to_string();
    }

    let match_keys: Vec<VocabularyMatchKey> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.is_usable())
        .flat_map(|(index, entry)| build_vocabulary_match_keys(entry, index))
        .collect();
    if match_keys.is_empty() {
        return text.to_string();
    }

    text.split('\n')
        .map(|line| apply_vocabulary_entries_to_line(line, entries, &match_keys, threshold))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One line of [`apply_vocabulary_entries`]. Split out only so the match keys
/// are built once for the whole text rather than once per line.
fn apply_vocabulary_entries_to_line(
    line: &str,
    entries: &[VocabularyEntry],
    match_keys: &[VocabularyMatchKey],
    threshold: f64,
) -> String {
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut result = Vec::with_capacity(words.len());
    let mut index = 0;

    while index < words.len() {
        let mut best_match: Option<(usize, &VocabularyEntry, f64)> = None;
        for ngram_len in (1..=3).rev() {
            if index + ngram_len > words.len() {
                continue;
            }

            let ngram_words = &words[index..index + ngram_len];
            if ngram_words[..ngram_len.saturating_sub(1)]
                .iter()
                .any(|word| !extract_punctuation(word).1.is_empty())
            {
                continue;
            }
            let candidate = build_ngram(ngram_words);
            if let Some((entry, score)) =
                find_best_vocabulary_match(&candidate, entries, match_keys, threshold)
            {
                if ngram_len > 1 && !starts_with_spoken_character(&candidate, entry) {
                    continue;
                }

                if best_match
                    .as_ref()
                    .is_none_or(|(_, _, best_score)| score < *best_score)
                {
                    best_match = Some((ngram_len, entry, score));
                }
            }
        }

        if let Some((ngram_len, entry, _)) = best_match {
            let ngram_words = &words[index..index + ngram_len];
            let (prefix, _) = extract_punctuation(ngram_words[0]);
            let (_, suffix) = extract_punctuation(ngram_words[ngram_len - 1]);
            result.push(format!("{}{}{}", prefix, entry.written, suffix));
            index += ngram_len;
        } else {
            result.push(words[index].to_string());
            index += 1;
        }
    }

    result.join(" ")
}

/// Applies only normalized exact vocabulary matches after prompt-biased decoding.
pub fn apply_exact_vocabulary_entries(text: &str, entries: &[VocabularyEntry]) -> String {
    apply_vocabulary_entries(text, entries, f64::EPSILON)
}

pub(crate) fn is_token_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

pub(crate) fn has_token_boundaries(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    before.is_none_or(|character| !is_token_character(character))
        && after.is_none_or(|character| !is_token_character(character))
}

/// Byte length of the prefix of `text` whose lowercase folding equals
/// `lowercase_phrase`, or `None` when no whole-character prefix matches.
///
/// Case-insensitive phrase matching is done by folding rather than by regex so
/// user-authored phrase text can never be read as pattern syntax.
pub(crate) fn lowercase_prefix_len(text: &str, lowercase_phrase: &str) -> Option<usize> {
    let mut expected = lowercase_phrase.chars();
    for (offset, character) in text.char_indices() {
        if expected.as_str().is_empty() {
            return Some(offset);
        }
        for lowered in character.to_lowercase() {
            if expected.next() != Some(lowered) {
                return None;
            }
        }
    }
    expected.as_str().is_empty().then_some(text.len())
}

/// One rule prepared for matching, with its spoken form folded once instead of
/// once per candidate position.
struct PreparedReplacement<'a> {
    lowercase_spoken: String,
    written: &'a str,
}

/// Applies the user's deterministic replacement rules.
///
/// Runs before vocabulary correction so a rewritten phrase is never also fuzzy
/// matched. Matching is case-insensitive, respects Unicode token boundaries, and
/// prefers the longest rule when several match at one position, so `dot com`
/// wins over a `dot` rule at the same offset. Written forms are inserted
/// verbatim and are never rescanned, which makes the pass idempotent.
pub fn apply_text_replacements(text: &str, rules: &[ReplacementRule]) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    let prepared: Vec<PreparedReplacement<'_>> = rules
        .iter()
        .filter(|rule| rule.enabled && rule.is_usable())
        .map(|rule| PreparedReplacement {
            lowercase_spoken: rule.spoken.trim().to_lowercase(),
            written: rule.written.as_str(),
        })
        .collect();
    if prepared.is_empty() {
        return text.to_string();
    }

    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;

    for (start, _) in text.char_indices() {
        if start < cursor {
            continue;
        }
        let mut selected: Option<(usize, &str)> = None;
        for rule in &prepared {
            let Some(length) = lowercase_prefix_len(&text[start..], &rule.lowercase_spoken) else {
                continue;
            };
            let end = start + length;
            if has_token_boundaries(text, start, end)
                && selected.is_none_or(|(current, _)| length > current)
            {
                selected = Some((length, rule.written));
            }
        }

        if let Some((length, written)) = selected {
            result.push_str(&text[cursor..start]);
            result.push_str(written);
            cursor = start + length;
        }
    }

    if cursor == 0 {
        return text.to_string();
    }
    result.push_str(&text[cursor..]);
    result
}

/// Applies exact phrase replacements while respecting Unicode token boundaries.
///
/// Matching folds case, exactly as [`apply_text_replacements`] and
/// [`crate::snippets::apply_snippets`] do, and for the same reason: the stored
/// phrase is a *spoken* form, so the recognizer decides its case and the user
/// cannot. A byte-exact match made that decision load-bearing — the settings
/// UI offers `smiley face` as its worked example, and a recognizer writes
/// `Smiley face` at the start of a sentence, so the app's own example fired
/// mid-sentence and stayed silent at the start of one. Measured on
/// `say`-rendered speech through the real pipeline: `Smiley face, that is my
/// answer.` left a `smiley face` rule unfired while the same string as a
/// snippet trigger or a replacement rule fired.
///
/// Fuzzy matching stays deliberately absent. Case folding is not fuzz: it
/// matches the phrase the user wrote and no other. Only the
/// threshold-governed vocabulary stage may match a phrase it was not given.
pub fn apply_emoji_replacements(text: &str, replacements: &[EmojiReplacement]) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    let prepared: Vec<PreparedReplacement<'_>> = replacements
        .iter()
        .filter(|replacement| replacement.is_usable())
        .map(|replacement| PreparedReplacement {
            lowercase_spoken: replacement.spoken.trim().to_lowercase(),
            written: replacement.written.as_str(),
        })
        .collect();
    if prepared.is_empty() {
        return text.to_string();
    }

    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;

    for (start, _) in text.char_indices() {
        if start < cursor {
            continue;
        }
        let mut selected: Option<(usize, &str)> = None;
        for replacement in &prepared {
            let Some(length) = lowercase_prefix_len(&text[start..], &replacement.lowercase_spoken)
            else {
                continue;
            };
            let end = start + length;
            if has_token_boundaries(text, start, end)
                && selected.is_none_or(|(current, _)| length > current)
            {
                selected = Some((length, replacement.written));
            }
        }

        if let Some((length, written)) = selected {
            result.push_str(&text[cursor..start]);
            result.push_str(written);
            cursor = start + length;
        }
    }

    if cursor == 0 {
        return text.to_string();
    }
    result.push_str(&text[cursor..]);
    result
}
/// Extracts punctuation prefix and suffix from a word
fn extract_punctuation(word: &str) -> (&str, &str) {
    // String slices use byte offsets. Derive both boundaries from char_indices
    // so multibyte punctuation such as `。` and `「」` can never be split.
    let prefix_end = word
        .char_indices()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(index, _)| index)
        .unwrap_or(word.len());
    let suffix_start = word
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(index, c)| index + c.len_utf8())
        .unwrap_or(0);

    let prefix = if prefix_end > 0 {
        &word[..prefix_end]
    } else {
        ""
    };

    let suffix = if suffix_start < word.len() {
        &word[suffix_start..]
    } else {
        ""
    };

    (prefix, suffix)
}

/// Evidence for the language of the text being cleaned.
///
/// This intentionally describes the transcription output, not Sona's UI
/// language. Unknown output languages fail closed: built-in filler removal is
/// skipped rather than applying a language profile speculatively.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputLanguageEvidence {
    UserSelected(String),
    ModelConstrained(String),
    /// The transcription model itself identified the language (audio-based
    /// LID, e.g. Whisper in auto mode).
    ModelDetected(String),
    /// Detected from the transcribed text with high confidence, constrained to
    /// the model's supported languages. Weakest accepted evidence.
    TextDetected(String),
    TranslatedToEnglish,
    Unknown,
}

impl OutputLanguageEvidence {
    fn language(&self) -> Option<&str> {
        match self {
            Self::UserSelected(language)
            | Self::ModelConstrained(language)
            | Self::ModelDetected(language)
            | Self::TextDetected(language) => Some(language),
            Self::TranslatedToEnglish => Some("en"),
            Self::Unknown => None,
        }
    }
}

fn is_english_language(language: &str) -> bool {
    language
        .split(&['-', '_'][..])
        .next()
        .is_some_and(|base| base.eq_ignore_ascii_case("en"))
}

impl OutputLanguageEvidence {
    pub(super) fn is_english(&self) -> bool {
        self.language().is_some_and(is_english_language)
    }
}

/// The spelling transform is deliberately a fixed dictionary, not a suffix
/// heuristic. This table is the complete supported contract: add a pair only
/// with its intended forms, then extend the table-driven case tests below.
const AMERICAN_TO_BRITISH_SPELLINGS: &[(&str, &str)] = &[
    ("acknowledgment", "acknowledgement"),
    ("acknowledgments", "acknowledgements"),
    ("aging", "ageing"),
    ("analyze", "analyse"),
    ("analyzed", "analysed"),
    ("analyzes", "analyses"),
    ("analyzing", "analysing"),
    ("anemia", "anaemia"),
    ("anemic", "anaemic"),
    ("apologize", "apologise"),
    ("apologized", "apologised"),
    ("apologizes", "apologises"),
    ("apologizing", "apologising"),
    ("armor", "armour"),
    ("armored", "armoured"),
    ("armoring", "armouring"),
    ("armors", "armours"),
    ("artifact", "artefact"),
    ("artifacts", "artefacts"),
    ("authorize", "authorise"),
    ("authorized", "authorised"),
    ("authorizes", "authorises"),
    ("authorizing", "authorising"),
    ("behavior", "behaviour"),
    ("behaviors", "behaviours"),
    ("canceled", "cancelled"),
    ("canceler", "canceller"),
    ("cancelers", "cancellers"),
    ("canceling", "cancelling"),
    ("catalog", "catalogue"),
    ("cataloged", "catalogued"),
    ("cataloging", "cataloguing"),
    ("catalogs", "catalogues"),
    ("categorize", "categorise"),
    ("categorized", "categorised"),
    ("categorizes", "categorises"),
    ("categorizing", "categorising"),
    ("center", "centre"),
    ("centered", "centred"),
    ("centering", "centring"),
    ("centers", "centres"),
    ("centralize", "centralise"),
    ("centralized", "centralised"),
    ("centralizes", "centralises"),
    ("centralizing", "centralising"),
    ("characterize", "characterise"),
    ("characterized", "characterised"),
    ("characterizes", "characterises"),
    ("characterizing", "characterising"),
    ("civilize", "civilise"),
    ("civilized", "civilised"),
    ("civilizes", "civilises"),
    ("civilizing", "civilising"),
    ("colonize", "colonise"),
    ("colonized", "colonised"),
    ("colonizes", "colonises"),
    ("colonizing", "colonising"),
    ("color", "colour"),
    ("colored", "coloured"),
    ("coloring", "colouring"),
    ("colorless", "colourless"),
    ("colors", "colours"),
    ("cozier", "cosier"),
    ("coziest", "cosiest"),
    ("coziness", "cosiness"),
    ("cozy", "cosy"),
    ("criticize", "criticise"),
    ("criticized", "criticised"),
    ("criticizes", "criticises"),
    ("criticizing", "criticising"),
    ("customize", "customise"),
    ("customized", "customised"),
    ("customizes", "customises"),
    ("customizing", "customising"),
    ("defense", "defence"),
    ("defenses", "defences"),
    ("dialog", "dialogue"),
    ("dialogs", "dialogues"),
    ("digitize", "digitise"),
    ("digitized", "digitised"),
    ("digitizes", "digitises"),
    ("digitizing", "digitising"),
    ("dramatize", "dramatise"),
    ("dramatized", "dramatised"),
    ("dramatizes", "dramatises"),
    ("dramatizing", "dramatising"),
    ("emphasize", "emphasise"),
    ("emphasized", "emphasised"),
    ("emphasizes", "emphasises"),
    ("emphasizing", "emphasising"),
    ("endeavor", "endeavour"),
    ("endeavored", "endeavoured"),
    ("endeavoring", "endeavouring"),
    ("endeavors", "endeavours"),
    ("enroll", "enrol"),
    ("enrollment", "enrolment"),
    ("enrollments", "enrolments"),
    ("estrogen", "oestrogen"),
    ("familiarize", "familiarise"),
    ("familiarized", "familiarised"),
    ("familiarizes", "familiarises"),
    ("familiarizing", "familiarising"),
    ("favor", "favour"),
    ("favored", "favoured"),
    ("favoring", "favouring"),
    ("favors", "favours"),
    ("fiber", "fibre"),
    ("fibers", "fibres"),
    ("finalize", "finalise"),
    ("finalized", "finalised"),
    ("finalizes", "finalises"),
    ("finalizing", "finalising"),
    ("flavor", "flavour"),
    ("flavors", "flavours"),
    ("fulfill", "fulfil"),
    ("fulfillment", "fulfilment"),
    ("fulfillments", "fulfilments"),
    ("generalize", "generalise"),
    ("generalized", "generalised"),
    ("generalizes", "generalises"),
    ("generalizing", "generalising"),
    ("gray", "grey"),
    ("grayed", "greyed"),
    ("grayer", "greyer"),
    ("grayest", "greyest"),
    ("graying", "greying"),
    ("grays", "greys"),
    ("harbor", "harbour"),
    ("harbored", "harboured"),
    ("harboring", "harbouring"),
    ("harbors", "harbours"),
    ("harmonize", "harmonise"),
    ("harmonized", "harmonised"),
    ("harmonizes", "harmonises"),
    ("harmonizing", "harmonising"),
    ("honor", "honour"),
    ("honored", "honoured"),
    ("honoring", "honouring"),
    ("honors", "honours"),
    ("humor", "humour"),
    ("humors", "humours"),
    ("initialize", "initialise"),
    ("initialized", "initialised"),
    ("initializes", "initialises"),
    ("initializing", "initialising"),
    ("installment", "instalment"),
    ("installments", "instalments"),
    ("jewelry", "jewellery"),
    ("judgment", "judgement"),
    ("judgments", "judgements"),
    ("labeled", "labelled"),
    ("labeler", "labeller"),
    ("labelers", "labellers"),
    ("labeling", "labelling"),
    ("labor", "labour"),
    ("labored", "laboured"),
    ("laboring", "labouring"),
    ("labors", "labours"),
    ("legalize", "legalise"),
    ("legalized", "legalised"),
    ("legalizes", "legalises"),
    ("legalizing", "legalising"),
    ("maneuver", "manoeuvre"),
    ("maneuvered", "manoeuvred"),
    ("maneuvering", "manoeuvring"),
    ("maneuvers", "manoeuvres"),
    ("maximize", "maximise"),
    ("maximized", "maximised"),
    ("maximizes", "maximises"),
    ("maximizing", "maximising"),
    ("memorize", "memorise"),
    ("memorized", "memorised"),
    ("memorizes", "memorises"),
    ("memorizing", "memorising"),
    ("minimize", "minimise"),
    ("minimized", "minimised"),
    ("minimizes", "minimises"),
    ("minimizing", "minimising"),
    ("mobilize", "mobilise"),
    ("mobilized", "mobilised"),
    ("mobilizes", "mobilises"),
    ("mobilizing", "mobilising"),
    ("modernize", "modernise"),
    ("modernized", "modernised"),
    ("modernizes", "modernises"),
    ("modernizing", "modernising"),
    ("mold", "mould"),
    ("molded", "moulded"),
    ("molding", "moulding"),
    ("molds", "moulds"),
    ("moldy", "mouldy"),
    ("mustache", "moustache"),
    ("mustaches", "moustaches"),
    ("neighbor", "neighbour"),
    ("neighbors", "neighbours"),
    ("normalize", "normalise"),
    ("normalized", "normalised"),
    ("normalizes", "normalises"),
    ("normalizing", "normalising"),
    ("odor", "odour"),
    ("odors", "odours"),
    ("offense", "offence"),
    ("offenses", "offences"),
    ("optimize", "optimise"),
    ("optimized", "optimised"),
    ("optimizes", "optimises"),
    ("optimizing", "optimising"),
    ("organize", "organise"),
    ("organized", "organised"),
    ("organizes", "organises"),
    ("organizing", "organising"),
    ("pajamas", "pyjamas"),
    ("pediatric", "paediatric"),
    ("pediatrician", "paediatrician"),
    ("pediatricians", "paediatricians"),
    ("pediatrics", "paediatrics"),
    ("personalize", "personalise"),
    ("personalized", "personalised"),
    ("personalizes", "personalises"),
    ("personalizing", "personalising"),
    ("plow", "plough"),
    ("plowed", "ploughed"),
    ("plowing", "ploughing"),
    ("plows", "ploughs"),
    ("prioritize", "prioritise"),
    ("prioritized", "prioritised"),
    ("prioritizes", "prioritises"),
    ("prioritizing", "prioritising"),
    ("publicize", "publicise"),
    ("publicized", "publicised"),
    ("publicizes", "publicises"),
    ("publicizing", "publicising"),
    ("realize", "realise"),
    ("realized", "realised"),
    ("realizes", "realises"),
    ("realizing", "realising"),
    ("recognize", "recognise"),
    ("recognized", "recognised"),
    ("recognizes", "recognises"),
    ("recognizing", "recognising"),
    ("rumor", "rumour"),
    ("rumors", "rumours"),
    ("savior", "saviour"),
    ("saviors", "saviours"),
    ("savor", "savour"),
    ("savored", "savoured"),
    ("savoring", "savouring"),
    ("savors", "savours"),
    ("skeptic", "sceptic"),
    ("skeptical", "sceptical"),
    ("skeptics", "sceptics"),
    ("skillful", "skilful"),
    ("skillfully", "skilfully"),
    ("smolder", "smoulder"),
    ("smoldered", "smouldered"),
    ("smoldering", "smouldering"),
    ("smolders", "smoulders"),
    ("socialize", "socialise"),
    ("socialized", "socialised"),
    ("socializes", "socialises"),
    ("socializing", "socialising"),
    ("specialize", "specialise"),
    ("specialized", "specialised"),
    ("specializes", "specialises"),
    ("specializing", "specialising"),
    ("splendor", "splendour"),
    ("stabilize", "stabilise"),
    ("stabilized", "stabilised"),
    ("stabilizes", "stabilises"),
    ("stabilizing", "stabilising"),
    ("standardize", "standardise"),
    ("standardized", "standardised"),
    ("standardizes", "standardises"),
    ("standardizing", "standardising"),
    ("summarize", "summarise"),
    ("summarized", "summarised"),
    ("summarizes", "summarises"),
    ("summarizing", "summarising"),
    ("symbolize", "symbolise"),
    ("symbolized", "symbolised"),
    ("symbolizes", "symbolises"),
    ("symbolizing", "symbolising"),
    ("theater", "theatre"),
    ("theaters", "theatres"),
    ("tire", "tyre"),
    ("tires", "tyres"),
    ("traveled", "travelled"),
    ("traveler", "traveller"),
    ("travelers", "travellers"),
    ("traveling", "travelling"),
    ("utilize", "utilise"),
    ("utilized", "utilised"),
    ("utilizes", "utilises"),
    ("utilizing", "utilising"),
    ("valor", "valour"),
    ("vapor", "vapour"),
    ("vapors", "vapours"),
    ("vigor", "vigour"),
    ("visualize", "visualise"),
    ("visualized", "visualised"),
    ("visualizes", "visualises"),
    ("visualizing", "visualising"),
    ("vocalize", "vocalise"),
    ("vocalized", "vocalised"),
    ("vocalizes", "vocalises"),
    ("vocalizing", "vocalising"),
    ("woolen", "woollen"),
    ("worshiped", "worshipped"),
    ("worshiper", "worshipper"),
    ("worshipers", "worshippers"),
    ("worshiping", "worshipping"),
    ("yogurt", "yoghurt"),
];

static AMERICAN_SPELLING_PATTERN: Lazy<Regex> = Lazy::new(|| {
    let pattern_len = AMERICAN_TO_BRITISH_SPELLINGS
        .iter()
        .map(|(american, _)| american.len() + 1)
        .sum::<usize>()
        + r"(?i)\b(?:)\b".len();
    let mut pattern = String::with_capacity(pattern_len);
    pattern.push_str(r"(?i)\b(?:");
    for (index, (american, _)) in AMERICAN_TO_BRITISH_SPELLINGS.iter().enumerate() {
        if index > 0 {
            pattern.push('|');
        }
        // Every table key is an ASCII word, so it is regex-safe as-is.
        pattern.push_str(american);
    }
    pattern.push_str(r")\b");
    // PANIC: the fixed table is verified by the table-driven spelling tests.
    Regex::new(&pattern).expect("American spelling pattern is valid")
});

fn british_spelling(word: &str) -> Option<&'static str> {
    let index = AMERICAN_TO_BRITISH_SPELLINGS
        .binary_search_by(|(american, _)| american.cmp(&word))
        .ok()?;
    Some(AMERICAN_TO_BRITISH_SPELLINGS[index].1)
}

fn preserve_ascii_case(source: &str, replacement: &str) -> String {
    if source.bytes().all(|byte| byte.is_ascii_lowercase()) {
        return replacement.to_string();
    }
    if source.bytes().all(|byte| !byte.is_ascii_lowercase()) {
        return replacement.to_ascii_uppercase();
    }
    let is_title_case = source
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_uppercase())
        && source.bytes().skip(1).all(|byte| byte.is_ascii_lowercase());
    if !is_title_case {
        return source.to_string();
    }

    let mut chars = replacement.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut result = String::with_capacity(replacement.len());
    result.extend(first.to_uppercase());
    result.push_str(chars.as_str());
    result
}

/// Applies the user's global British spelling choice when trustworthy English
/// output evidence exists. A matching vocabulary spoken form suppresses this
/// transform so the user's written replacement remains the final authority.
pub fn apply_british_spelling(
    text: &str,
    language: &OutputLanguageEvidence,
    spelling: EnglishSpelling,
    vocabulary: &[VocabularyEntry],
) -> String {
    if spelling != EnglishSpelling::British || !language.is_english() {
        return text.to_string();
    }

    AMERICAN_SPELLING_PATTERN
        .replace_all(text, |captures: &regex::Captures<'_>| {
            // PANIC: regex replace callbacks always contain capture group zero.
            let source = captures.get(0).expect("spelling match has text").as_str();
            if vocabulary_reserves_phrase(vocabulary, source) {
                return source.to_string();
            }
            let lower = source.to_ascii_lowercase();
            british_spelling(&lower)
                .map(|replacement| preserve_ascii_case(source, replacement))
                .unwrap_or_else(|| source.to_string())
        })
        .into_owned()
}

fn vocabulary_reserves_phrase(entries: &[VocabularyEntry], phrase: &str) -> bool {
    entries
        .iter()
        .any(|entry| entry.is_usable() && entry.spoken.trim().eq_ignore_ascii_case(phrase))
}

fn append_literal_word(output: &mut String, word: &str) {
    if !output.is_empty() && !output.ends_with('\n') {
        output.push(' ');
    }
    output.push_str(word);
}

fn spoken_punctuation_at(
    words: &[&str],
    index: usize,
) -> Option<(&'static str, usize, &'static str)> {
    let current = words[index];
    if current.eq_ignore_ascii_case("comma") {
        Some(("comma", 1, ","))
    } else if current.eq_ignore_ascii_case("period") {
        Some(("period", 1, "."))
    } else if current.eq_ignore_ascii_case("full")
        && words
            .get(index + 1)
            .is_some_and(|word| word.eq_ignore_ascii_case("stop"))
    {
        Some(("full stop", 2, "."))
    } else if current.eq_ignore_ascii_case("question")
        && words
            .get(index + 1)
            .is_some_and(|word| word.eq_ignore_ascii_case("mark"))
    {
        Some(("question mark", 2, "?"))
    } else if current.eq_ignore_ascii_case("exclamation")
        && words
            .get(index + 1)
            .is_some_and(|word| word.eq_ignore_ascii_case("mark"))
    {
        Some(("exclamation mark", 2, "!"))
    } else if current.eq_ignore_ascii_case("new")
        && words
            .get(index + 1)
            .is_some_and(|word| word.eq_ignore_ascii_case("paragraph"))
    {
        Some(("new paragraph", 2, "\n\n"))
    } else if current.eq_ignore_ascii_case("new")
        && words
            .get(index + 1)
            .is_some_and(|word| word.eq_ignore_ascii_case("line"))
    {
        Some(("new line", 2, "\n"))
    } else {
        None
    }
}

/// Converts a conservative English spoken-punctuation table before vocabulary
/// correction. A user vocabulary phrase reserves its own words, and a literal
/// mention such as "the word comma itself" remains text.
pub fn apply_literal_punctuation(
    text: &str,
    language: &OutputLanguageEvidence,
    enabled: bool,
    vocabulary: &[VocabularyEntry],
) -> String {
    if !enabled || !language.is_english() {
        return text.to_string();
    }

    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.to_string();
    }

    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < words.len() {
        let mention = index > 0 && words[index - 1].eq_ignore_ascii_case("word");
        if let Some((phrase, consumed, replacement)) = spoken_punctuation_at(&words, index) {
            if !mention && !vocabulary_reserves_phrase(vocabulary, phrase) {
                while output.ends_with(' ') {
                    output.pop();
                }
                if replacement.starts_with('\n') {
                    // A break before any text has nothing to separate, and two
                    // adjacent break phrases widen to the larger break rather
                    // than stacking into arbitrary vertical space.
                    let trailing = output.len() - output.trim_end_matches('\n').len();
                    if !output.is_empty() && replacement.len() > trailing {
                        output.truncate(output.len() - trailing);
                        output.push_str(replacement);
                    }
                } else {
                    output.push_str(replacement);
                }
                index += consumed;
                continue;
            }
        }
        append_literal_word(&mut output, words[index]);
        index += 1;
    }
    output
}

/// Filler tokens that are not lexical words in any language Sona's models can
/// output, so removing them cannot corrupt text regardless of the (possibly
/// unknown) output language. Kept deliberately conservative: anything that is a
/// real word somewhere ("um" pt/de, "ah"/"eh" interjections, "mm" millimetres)
/// belongs in the language-gated lists instead - or, if it is also a real word
/// in the language that would remove it, in neither list. See "ha" below.
const UNIVERSAL_FILLER_WORDS: &[&str] = &[
    "uh", "uhm", "umm", "uhh", "uhhh", "ehh", "ehm", "ahm", "hmm", "hm", "mmm", "хм", "ммм",
];

/// Filler words that are only safe to remove with evidence for the output
/// language, because the same token is a real word elsewhere (e.g. Portuguese
/// "um" = "a/an", German "um" = "at/around").
///
/// Language evidence guards a token against *other* languages and cannot guard
/// it against its own. "ha" was here until it was caught deleting a word from
/// English: it is the first syllable of Ha Long, Ha Noi and Hanoi, and it is
/// how laughter transcribes, so `say "Ha Long Bay is beautiful"` came back as
/// "Long Bay is beautiful". Nothing was gained by carrying it - "uh", "uhm",
/// "um", "ah" and "eh" already cover the English hesitation it stood for. A
/// token that collides inside its own language belongs in a custom list the
/// user chose, not in a built-in one they never saw.
///
/// That said, `custom_filler_words` has no editor in the app: it exists at both
/// the root and per mode, is `null` in both, and the only way to set it is to
/// hand-edit `settings_store.json`. So the sentence above describes where such
/// a token *belongs*, not a place a user can put one today. Its sibling
/// `custom_words` has two editors built on `useVocabularyRows`, so the control
/// this list needs already ships for vocabulary.
fn gated_filler_words_for_language(lang: &str) -> &'static [&'static str] {
    let base_lang = lang.split(&['-', '_'][..]).next().unwrap_or(lang);

    match base_lang {
        "en" => &["um", "ah", "eh"],
        "de" => &["äh", "ähm"],
        "fr" => &["euh"],
        _ => &[],
    }
}

/// Runs of two or more spaces/tabs on one line. Deliberately excludes newlines:
/// collapsing every whitespace run with a single `\s{2,}` pass would erase any
/// line break an earlier stage produced, which is why line breaks get their own
/// pattern below instead of being folded in here.
static MULTI_SPACE_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"[^\S\n]{2,}").unwrap());

/// A whitespace run containing at least one line break, together with the
/// horizontal padding on either side of it.
static LINE_BREAK_RUN_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s*\n\s*").unwrap());

/// Collapses repeated words (3+ repetitions) to a single instance.
/// E.g., "wh wh wh wh" -> "wh", "I I I I" -> "I"
fn collapse_stutters(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.to_string();
    }

    let mut result: Vec<&str> = Vec::new();
    let mut i = 0;

    while i < words.len() {
        let word = words[i];
        let word_lower = word.to_lowercase();

        if word_lower.chars().all(|c| c.is_alphabetic()) {
            // Count consecutive repetitions (case-insensitive)
            let mut count = 1;
            while i + count < words.len() && words[i + count].to_lowercase() == word_lower {
                count += 1;
            }

            // If 3+ repetitions, collapse to single instance
            if count >= 3 {
                result.push(word);
                i += count;
            } else {
                result.push(word);
                i += 1;
            }
        } else {
            result.push(word);
            i += 1;
        }
    }

    result.join(" ")
}

/// Whether the text emitted so far leaves the next word opening a sentence.
///
/// Nothing but whitespace so far means the deleted filler stood at the very
/// beginning, so whatever follows inherits the opening capital.
fn opens_sentence(emitted: &str) -> bool {
    emitted
        .chars()
        .rev()
        .find(|character| !character.is_whitespace())
        .is_none_or(|character| matches!(character, '.' | '!' | '?' | '…'))
}

/// Appends `text`, promoting its first word character to uppercase when a
/// deleted filler owes the sentence its capital.
///
/// Only ASCII letters are promoted, the same Latin scope
/// [`preserve_ascii_case`] keeps: elsewhere an uppercase form is either absent
/// (Chinese, Japanese, Thai) or locale-dependent (Turkish dotless i), and
/// there the recogniser's own casing is the better guess. The first word
/// character settles the debt either way, so a digit or a Cyrillic word is not
/// passed over in search of a letter that could carry the capital.
fn append_restoring_capital(output: &mut String, text: &str, capital_owed: &mut bool) {
    if !*capital_owed {
        output.push_str(text);
        return;
    }
    let Some((index, character)) = text
        .char_indices()
        .find(|(_, character)| character.is_alphanumeric())
    else {
        output.push_str(text);
        return;
    };

    *capital_owed = false;
    output.push_str(&text[..index]);
    output.push(character.to_ascii_uppercase());
    output.push_str(&text[index + character.len_utf8()..]);
}

/// Deletes every match of one filler pattern, carrying a sentence's capital
/// over to the word that takes the deleted filler's place.
///
/// The sentence boundary is read off the text kept so far rather than the
/// input, because the pattern also eats a trailing comma or period: when the
/// deletion swallowed the period that ended the previous sentence, no sentence
/// starts here any more and no capital is owed.
fn remove_filler_matches(text: &str, pattern: &Regex) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut resume = 0;
    let mut capital_owed = false;

    for deleted in pattern.find_iter(text) {
        append_restoring_capital(&mut kept, &text[resume..deleted.start()], &mut capital_owed);
        capital_owed |= opens_sentence(&kept);
        resume = deleted.end();
    }
    append_restoring_capital(&mut kept, &text[resume..], &mut capital_owed);

    kept
}

/// Removes filler words from transcription output when enabled.
///
/// Built-in removal is two-tiered: [`UNIVERSAL_FILLER_WORDS`] apply regardless
/// of language evidence, while [`gated_filler_words_for_language`] tokens are
/// only removed when the output language is known. A custom list is an
/// explicit user override and replaces both tiers without requiring language
/// evidence. `Some(empty vec)` disables removal, preserving the legacy
/// power-user setting. The master toggle takes precedence over both built-in
/// and custom lists.
///
/// A deletion that opens a sentence hands the capital to the word taking the
/// filler's place: `"Um, so I think"` leaves `"So I think"`, because the
/// recogniser capitalized `Um` for standing where it stood and not for being
/// itself.
///
/// # Arguments
/// * `text` - The raw transcription text to filter
/// * `language` - Evidence for the language of the transcription output
/// * `custom_filler_words` - Optional user-provided filler word list. `Some(vec)` overrides
///   language defaults; `Some(empty vec)` disables filtering; `None` uses language defaults.
/// * `enabled` - Whether filler-word removal is enabled
///
/// # Returns
/// The text with configured filler words removed
pub fn remove_filler_words(
    text: &str,
    language: &OutputLanguageEvidence,
    custom_filler_words: &Option<Vec<String>>,
    enabled: bool,
) -> String {
    if !enabled {
        return text.to_string();
    }

    // Build filler patterns from custom list or the built-in tiers
    let patterns: Vec<Regex> = match custom_filler_words {
        Some(words) => words
            .iter()
            .filter_map(|word| Regex::new(&format!(r"(?i)\b{}\b[,.]?", regex::escape(word))).ok())
            .collect(),
        None => UNIVERSAL_FILLER_WORDS
            .iter()
            .chain(
                language
                    .language()
                    .map(gated_filler_words_for_language)
                    .unwrap_or_default(),
            )
            .map(|word| {
                Regex::new(&format!(r"(?i)\b{}\b[,.]?", regex::escape(word))).unwrap_or_else(
                    |error| unreachable!("escaped filler-word pattern is valid: {error}"),
                )
            })
            .collect(),
    };

    let mut filtered = text.to_string();
    for pattern in &patterns {
        filtered = remove_filler_matches(&filtered, pattern);
    }

    filtered
}

/// Applies non-filler transcription cleanup.
///
/// Kept separate from [`remove_filler_words`] so disabling filler deletion
/// does not also disable the existing repeated-word and whitespace cleanup.
///
/// Line breaks survive. Earlier stages legitimately emit them — the spoken
/// punctuation table writes `\n` and `\n\n`, and a user replacement rule or an
/// LLM rewrite can emit either — so this stage normalizes vertical whitespace
/// (one break stays one break, several collapse to a paragraph break) instead
/// of flattening it into a space.
pub fn normalize_transcription_output(text: &str) -> String {
    let normalized = text
        .split('\n')
        .map(collapse_stutters)
        .collect::<Vec<_>>()
        .join("\n");

    let normalized =
        LINE_BREAK_RUN_PATTERN.replace_all(&normalized, |captures: &regex::Captures<'_>| {
            // Group zero is the whole match, so it is always present; treating an
            // absent one as "no extra newlines" keeps this total either way.
            let newlines = captures.get(0).map_or(0, |run| {
                run.as_str().bytes().filter(|byte| *byte == b'\n').count()
            });
            if newlines > 1 {
                "\n\n"
            } else {
                "\n"
            }
        });

    let normalized = MULTI_SPACE_PATTERN.replace_all(&normalized, " ");

    normalized.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercise the complete cleanup sequence with an explicitly selected
    /// language. Individual tests below predate the split between filler
    /// removal and non-filler normalization.
    fn filter_transcription_output(
        text: &str,
        language: &str,
        custom_filler_words: &Option<Vec<String>>,
    ) -> String {
        let language = OutputLanguageEvidence::UserSelected(language.to_string());
        let filtered = remove_filler_words(text, &language, custom_filler_words, true);
        normalize_transcription_output(&filtered)
    }

    fn apply_equal_pairs(text: &str, words: &[String], threshold: f64) -> String {
        let entries: Vec<_> = words
            .iter()
            .map(|word| VocabularyEntry {
                spoken: word.clone(),
                written: word.clone(),
            })
            .collect();
        apply_vocabulary_entries(text, &entries, threshold)
    }

    fn pair(spoken: &str, written: &str) -> VocabularyEntry {
        VocabularyEntry {
            spoken: spoken.to_string(),
            written: written.to_string(),
        }
    }

    fn rule(spoken: &str, written: &str) -> ReplacementRule {
        ReplacementRule {
            spoken: spoken.to_string(),
            written: written.to_string(),
            enabled: true,
        }
    }

    /// The starter library, as a fixture, so these tests exercise what ships.
    fn starter_rules() -> Vec<ReplacementRule> {
        crate::settings::default_replacement_rules()
    }

    #[test]
    fn replacements_rewrite_whole_phrases_case_insensitively() {
        let rules = starter_rules();
        assert_eq!(
            apply_text_replacements("email me at sign example dot com", &rules),
            "email me @ example .com"
        );
        assert_eq!(apply_text_replacements("At Sign", &rules), "@");
        assert_eq!(apply_text_replacements("HASHTAG", &rules), "#");
    }

    #[test]
    fn a_longer_rule_wins_over_a_shorter_one_at_the_same_position() {
        let rules = vec![rule("dot", "."), rule("dot com", ".com")];
        assert_eq!(apply_text_replacements("dot com", &rules), ".com");
        // Order in the list must not decide the outcome.
        let reversed = vec![rule("dot com", ".com"), rule("dot", ".")];
        assert_eq!(apply_text_replacements("dot com", &reversed), ".com");
        // The shorter rule still fires where the longer one cannot match.
        assert_eq!(apply_text_replacements("dot net", &rules), ". net");
    }

    #[test]
    fn replacements_never_fire_inside_a_longer_word() {
        let rules = vec![rule("at sign", "@"), rule("hashtag", "#")];
        assert_eq!(
            apply_text_replacements("hashtagged posts", &rules),
            "hashtagged posts"
        );
        assert_eq!(
            apply_text_replacements("format sign here", &rules),
            "format sign here"
        );
    }

    #[test]
    fn applying_replacements_twice_changes_nothing_the_second_time() {
        let rules = starter_rules();
        let once = apply_text_replacements("say at sign then new paragraph please", &rules);
        assert_eq!(apply_text_replacements(&once, &rules), once);
    }

    #[test]
    fn written_output_is_never_rescanned_as_input() {
        // Without the single-pass cursor this would loop or double-apply.
        let rules = vec![rule("alpha", "alpha beta"), rule("beta", "gamma")];
        assert_eq!(
            apply_text_replacements("alpha", &rules),
            "alpha beta".to_string()
        );
    }

    #[test]
    fn disabled_and_unusable_rules_are_skipped() {
        let mut disabled = rule("at sign", "@");
        disabled.enabled = false;
        assert_eq!(
            apply_text_replacements("at sign", &[disabled]),
            "at sign".to_string()
        );
        assert_eq!(
            apply_text_replacements("at sign", &[rule("at sign", "")]),
            "at sign".to_string()
        );
        assert_eq!(
            apply_text_replacements("at sign", &[rule("   ", "@")]),
            "at sign".to_string()
        );
    }

    #[test]
    fn an_empty_rule_set_returns_the_text_untouched() {
        assert_eq!(apply_text_replacements("unchanged", &[]), "unchanged");
        assert_eq!(apply_text_replacements("", &starter_rules()), "");
    }

    #[test]
    fn the_starter_library_leaves_spoken_line_breaks_to_the_punctuation_table() {
        // Owning "new line" in two places would let the replacements stage
        // override a user who turned literal punctuation off.
        let owns_a_break_phrase = starter_rules()
            .iter()
            .any(|rule| rule.spoken == "new line" || rule.spoken == "new paragraph");
        assert!(!owns_a_break_phrase);
    }

    #[test]
    fn normalization_preserves_a_paragraph_break() {
        assert_eq!(
            normalize_transcription_output("first\n\nsecond"),
            "first\n\nsecond"
        );
        assert_eq!(
            normalize_transcription_output("first\nsecond"),
            "first\nsecond"
        );
    }

    #[test]
    fn normalization_tidies_whitespace_around_a_break_without_erasing_it() {
        assert_eq!(
            normalize_transcription_output("first  \n   second"),
            "first\nsecond"
        );
        assert_eq!(
            normalize_transcription_output("first \n \n \n second"),
            "first\n\nsecond"
        );
    }

    #[test]
    fn normalization_still_collapses_runs_of_spaces_on_one_line() {
        assert_eq!(
            normalize_transcription_output("  too    many   spaces  "),
            "too many spaces"
        );
        assert_eq!(
            normalize_transcription_output("\n\n only line \n\n"),
            "only line"
        );
    }

    #[test]
    fn spoken_punctuation_produces_both_break_widths() {
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());
        assert_eq!(
            apply_literal_punctuation("one new line two", &english, true, &[]),
            "one\ntwo"
        );
        assert_eq!(
            apply_literal_punctuation("one new paragraph two", &english, true, &[]),
            "one\n\ntwo"
        );
        // A leading break has nothing to separate, and adjacent break phrases
        // widen rather than stack.
        assert_eq!(
            apply_literal_punctuation("new paragraph one", &english, true, &[]),
            "one"
        );
        assert_eq!(
            apply_literal_punctuation("one new line new paragraph two", &english, true, &[]),
            "one\n\ntwo"
        );
        assert_eq!(
            apply_literal_punctuation("one new paragraph new line two", &english, true, &[]),
            "one\n\ntwo"
        );
    }

    #[test]
    fn test_apply_custom_words_exact_match() {
        let text = "hello world";
        let custom_words = vec!["Hello".to_string(), "World".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 0.5);
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn test_apply_custom_words_fuzzy_match() {
        let text = "helo wrold";
        let custom_words = vec!["hello".to_string(), "world".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn vocabulary_writes_the_exact_written_form() {
        let entries = vec![pair("charge bee", "ChargeBee")];
        let result = apply_vocabulary_entries("CHARGE BEE", &entries, 0.5);
        assert_eq!(result, "ChargeBee");
    }

    #[test]
    fn test_extract_punctuation() {
        assert_eq!(extract_punctuation("hello"), ("", ""));
        assert_eq!(extract_punctuation("!hello?"), ("!", "?"));
        assert_eq!(extract_punctuation("...hello..."), ("...", "..."));
    }

    #[test]
    fn test_extract_punctuation_uses_unicode_boundaries() {
        assert_eq!(extract_punctuation("你好。"), ("", "。"));
        assert_eq!(extract_punctuation("「你好」"), ("「", "」"));
        assert_eq!(extract_punctuation("你好！"), ("", "！"));
    }

    #[test]
    fn test_empty_custom_words() {
        let text = "hello world";
        let custom_words = vec![];
        let result = apply_equal_pairs(text, &custom_words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_filter_filler_words() {
        let text = "So uhm I was thinking uh about this";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "So I was thinking about this");
    }

    #[test]
    fn test_filter_filler_words_case_insensitive() {
        let text = "UHM this is UH a test";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "This is a test");
    }

    #[test]
    fn test_filter_filler_words_with_punctuation() {
        let text = "Well, uhm, I think, uh. that's right";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Well, I think, that's right");
    }

    #[test]
    fn a_leading_filler_hands_its_capital_to_the_next_word() {
        let result = filter_transcription_output("Um, so I think this works", "en", &None);
        assert_eq!(result, "So I think this works");
    }

    #[test]
    fn a_filler_after_a_full_stop_opens_the_next_sentence() {
        let result = filter_transcription_output("That works. Uh, this one does not", "en", &None);
        assert_eq!(result, "That works. This one does not");
    }

    #[test]
    fn a_filler_inside_a_sentence_leaves_the_next_word_lowercase() {
        let result = filter_transcription_output("I was uh thinking about this", "en", &None);
        assert_eq!(result, "I was thinking about this");
    }

    #[test]
    fn a_filler_that_swallows_the_full_stop_owes_no_capital() {
        // The pattern takes a trailing period with the filler, so the sentence
        // that period ended is gone as well and the two halves merge. A
        // capital here would open a sentence the reader cannot see.
        let result = filter_transcription_output("that works uh. this one does not", "en", &None);
        assert_eq!(result, "that works this one does not");
    }

    #[test]
    fn a_restored_capital_stays_out_of_non_latin_scripts() {
        let filtered = remove_filler_words(
            "Ммм это работает",
            &OutputLanguageEvidence::Unknown,
            &None,
            true,
        );
        assert_eq!(normalize_transcription_output(&filtered), "это работает");
    }

    #[test]
    fn test_filter_cleans_whitespace() {
        let text = "Hello    world   test";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Hello world test");
    }

    #[test]
    fn test_filter_trims() {
        let text = "  Hello world  ";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_filter_combined() {
        let text = "  Uhm, so I was, uh, thinking about this  ";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "So I was, thinking about this");
    }

    #[test]
    fn test_filter_preserves_valid_text() {
        let text = "This is a completely normal sentence.";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "This is a completely normal sentence.");
    }

    #[test]
    fn test_filter_stutter_collapse() {
        let text = "w wh wh wh wh wh wh wh wh wh why";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "w wh why");
    }

    #[test]
    fn test_filter_stutter_short_words() {
        let text = "I I I I think so so so so";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "I think so");
    }

    #[test]
    fn test_filter_stutter_longer_words() {
        let text = "Check data doc doc doc doc documentation.";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Check data doc documentation.");
    }

    #[test]
    fn test_filter_stutter_mixed_case() {
        let text = "No NO no NO no";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "No");
    }

    #[test]
    fn test_filter_stutter_preserves_two_repetitions() {
        let text = "no no is fine";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "no no is fine");
    }

    #[test]
    fn test_filter_english_removes_um() {
        let text = "um I think um this is good";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "I think this is good");
    }

    #[test]
    fn test_filter_portuguese_preserves_um() {
        // "um" means "a/an" in Portuguese
        let text = "um gato bonito";
        let result = filter_transcription_output(text, "pt", &None);
        assert_eq!(result, "um gato bonito");
    }

    #[test]
    fn test_filter_spanish_preserves_ha() {
        // "ha" means "has" in Spanish
        let text = "ha sido un buen día";
        let result = filter_transcription_output(text, "es", &None);
        assert_eq!(result, "ha sido un buen día");
    }

    #[test]
    fn test_filter_english_preserves_ha() {
        // Deleted a real word before it left the English list: "Ha" opens Ha
        // Long, Ha Noi and Hanoi, so a built-in filler cannot claim it.
        let text = "Ha Long Bay is beautiful.";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(
            result, "Ha Long Bay is beautiful.",
            "a built-in filler must not delete the first syllable of a place name"
        );

        let laughter = filter_transcription_output("ha ha very funny", "en", &None);
        assert_eq!(
            laughter, "ha ha very funny",
            "laughter is text the speaker said, not hesitation"
        );

        // The hesitations it stood for still go.
        let hesitation = filter_transcription_output("um so ah I think eh yes", "en", &None);
        assert_eq!(hesitation, "So I think yes");
    }

    #[test]
    fn test_filter_language_code_with_region() {
        // "pt-BR" should normalize to "pt"
        let text = "um gato bonito";
        let result = filter_transcription_output(text, "pt-BR", &None);
        assert_eq!(result, "um gato bonito");
    }

    #[test]
    fn test_filter_custom_filler_words_override() {
        let custom = Some(vec!["okay".to_string(), "right".to_string()]);
        let text = "okay so I think right this works";
        let result = filter_transcription_output(text, "en", &custom);
        assert_eq!(result, "So I think this works");
    }

    #[test]
    fn test_filter_custom_filler_words_empty_disables() {
        let custom = Some(vec![]);
        let text = "So uhm I was thinking uh about this";
        let result = filter_transcription_output(text, "en", &custom);
        // No filler words removed since custom list is empty
        assert_eq!(result, "So uhm I was thinking uh about this");
    }

    #[test]
    fn test_filter_unknown_language_still_removes_universal_fillers() {
        let text = "uh I think uhm this works";
        let result = filter_transcription_output(text, "xx", &None);
        assert_eq!(result, "I think this works");
    }

    #[test]
    fn test_filter_unknown_language_does_not_remove_um() {
        let text = "um I think this works";
        let result = filter_transcription_output(text, "xx", &None);
        assert_eq!(result, "um I think this works");
    }

    #[test]
    fn test_filter_unknown_evidence_removes_universal_keeps_gated() {
        let filtered = remove_filler_words(
            "uhh bueno hmm creo que um ha llegado",
            &OutputLanguageEvidence::Unknown,
            &None,
            true,
        );
        assert_eq!(
            normalize_transcription_output(&filtered),
            "Bueno creo que um ha llegado"
        );

        let cyrillic = remove_filler_words(
            "хм я думаю ммм это работает",
            &OutputLanguageEvidence::Unknown,
            &None,
            true,
        );
        assert_eq!(
            normalize_transcription_output(&cyrillic),
            "я думаю это работает"
        );
    }

    #[test]
    fn test_filter_german_gated_fillers_require_evidence() {
        let text = "äh ich glaube ähm das passt";

        let unknown = remove_filler_words(text, &OutputLanguageEvidence::Unknown, &None, true);
        assert_eq!(normalize_transcription_output(&unknown), text);

        let result = filter_transcription_output(text, "de", &None);
        assert_eq!(result, "Ich glaube das passt");
    }

    #[test]
    fn test_filter_preserves_millimetre_unit() {
        // "mm" was removed from the filler lists because it eats units.
        let text = "the screw is 5 mm long";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "the screw is 5 mm long");
    }

    #[test]
    fn test_filter_detected_evidence_unlocks_gated_fillers() {
        let model = remove_filler_words(
            "um I think this works",
            &OutputLanguageEvidence::ModelDetected("en".to_string()),
            &None,
            true,
        );
        assert_eq!(normalize_transcription_output(&model), "I think this works");

        let text = remove_filler_words(
            "euh je pense que ça marche",
            &OutputLanguageEvidence::TextDetected("fr".to_string()),
            &None,
            true,
        );
        assert_eq!(
            normalize_transcription_output(&text),
            "Je pense que ça marche"
        );
    }

    #[test]
    fn test_filter_master_toggle_disables_custom_and_builtin_removal() {
        let text = "um customword I think";
        let language = OutputLanguageEvidence::UserSelected("en".to_string());
        let custom = Some(vec!["customword".to_string()]);

        let result = remove_filler_words(text, &language, &custom, false);

        assert_eq!(result, text);
    }

    #[test]
    fn test_filter_custom_words_apply_without_language_evidence() {
        let custom = Some(vec!["customword".to_string()]);
        let text = "customword should be removed but um should remain";

        let filtered = remove_filler_words(text, &OutputLanguageEvidence::Unknown, &custom, true);
        let result = normalize_transcription_output(&filtered);

        assert_eq!(result, "Should be removed but um should remain");
    }

    #[test]
    fn test_apply_custom_words_ngram_two_words() {
        let text = "il cui nome è Charge B, che permette";
        let custom_words = vec!["ChargeBee".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 0.5);
        assert!(result.contains("ChargeBee,"), "unexpected result: {result}");
        assert!(!result.contains("Charge B"));
    }

    #[test]
    fn test_apply_custom_words_ngram_three_words() {
        let text = "use Chat G P T for this";
        let custom_words = vec!["ChatGPT".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 0.5);
        assert!(result.contains("ChatGPT"));
    }

    #[test]
    fn test_apply_custom_words_prefers_longer_ngram() {
        let text = "Open AI GPT model";
        let custom_words = vec!["OpenAI".to_string(), "GPT".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 0.5);
        assert_eq!(result, "OpenAI GPT model");
    }

    #[test]
    fn test_apply_custom_words_ngram_preserves_case() {
        let text = "CHARGE B is great";
        let custom_words = vec!["ChargeBee".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 0.5);
        assert!(result.contains("ChargeBee"));
    }

    #[test]
    fn test_apply_custom_words_ngram_with_spaces_in_custom() {
        // Custom word with space should also match against split words
        let text = "using Mac Book Pro";
        let custom_words = vec!["MacBook Pro".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 0.5);
        assert_eq!(result, "using MacBook Pro");
    }

    #[test]
    fn test_apply_custom_words_trailing_number_not_doubled() {
        // Verify that trailing non-alpha chars (like numbers) aren't double-counted
        // between build_ngram stripping them and extract_punctuation capturing them
        let text = "use GPT4 for this";
        let custom_words = vec!["GPT-4".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 0.5);
        // Should NOT produce "GPT-44" (double-counting the trailing 4)
        assert!(
            !result.contains("GPT-44"),
            "got double-counted result: {}",
            result
        );
    }

    #[test]
    fn test_apply_custom_words_matches_ampersand_word() {
        let text = "send it to RD for review";
        let custom_words = vec!["R&D".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 0.18);
        assert_eq!(result, "send it to R&D for review");
    }

    #[test]
    fn test_apply_custom_words_matches_spoken_ampersand_word() {
        let text = "send it to R and D for review";
        let custom_words = vec!["R&D".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 0.18);
        assert_eq!(result, "send it to R&D for review");
    }

    #[test]
    fn test_apply_custom_words_preserves_ampersand_word() {
        let text = "send it to R&D for review";
        let custom_words = vec!["R&D".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 0.18);
        assert_eq!(result, "send it to R&D for review");
    }

    #[test]
    fn test_apply_custom_words_handles_unicode_punctuation() {
        let text = "「Sonaa。」";
        let custom_words = vec!["Sona".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 0.5);
        assert_eq!(result, "「Sona。」");
    }

    #[test]
    fn test_apply_custom_words_skips_cjk_fuzzy_matching() {
        let text = "你好。";
        let custom_words = vec!["你号".to_string()];
        let result = apply_equal_pairs(text, &custom_words, 1.0);
        assert_eq!(result, text);
    }

    #[test]
    fn deterministic_entity_fixture_has_recall_without_false_positives() {
        let entries: Vec<_> = (0..50)
            .map(|id| {
                pair(
                    &format!("northstar entity {id}"),
                    &format!("NorthstarEntity{id}"),
                )
            })
            .collect();

        for id in 0..50 {
            let written = format!("NorthstarEntity{id}");
            for (source, expected) in [
                (format!("northstar entity {id}"), written.clone()),
                (format!("northstarr entity {id}"), written.clone()),
                (format!("Northstarr Entity {id}"), written.clone()),
                (format!("northstarr entity {id},"), format!("{written},")),
            ] {
                assert_eq!(apply_vocabulary_entries(&source, &entries, 0.18), expected);
            }
        }

        let unrelated = "calendar invoice 42";
        assert_eq!(
            apply_vocabulary_entries(unrelated, &entries, 0.18),
            unrelated
        );
    }

    /// Measured on a real dictation: parakeet emitted "if you want some of
    /// the most tender" and the shipped `Sona` entry rewrote "some" as
    /// "Sona", because the two share Soundex S500 and the phonetic bonus was
    /// admitting matches the 0.18 threshold rejects on spelling.
    #[test]
    fn a_rhyming_common_word_survives_a_short_vocabulary_entry() {
        let entries = vec![pair("Sona", "Sona")];
        let dictation = "if you want some of the most tender";
        assert_eq!(
            apply_vocabulary_entries(dictation, &entries, 0.18),
            dictation
        );
        // The transcript the defect was measured on, in full.
        let measured = "If you want some of the most tender, collagen-rich meat \
            on the entire cow, stop buying the cuts everyone else is buying.";
        assert_eq!(apply_vocabulary_entries(measured, &entries, 0.18), measured);
        // Same Soundex code, one edit rather than two, and still not a
        // spelling the threshold admits.
        for rhyme in ["son", "soma", "sonny"] {
            assert_eq!(apply_vocabulary_entries(rhyme, &entries, 0.18), rhyme);
        }
        // The entry still does its job on its own spelling.
        assert_eq!(apply_vocabulary_entries("sona", &entries, 0.18), "Sona");
    }

    #[test]
    fn vocabulary_pairs_preserve_multiword_punctuation_and_unicode_boundaries() {
        let entries = vec![pair("charge bee", "ChargeBee"), pair("open ai", "OpenAI")];
        assert_eq!(
            apply_vocabulary_entries("「charge bee, open ai。」", &entries, 0.18),
            "「ChargeBee, OpenAI。」"
        );
    }

    #[test]
    fn emoji_replacements_are_exact_and_unicode_token_safe() {
        let replacements = vec![EmojiReplacement {
            spoken: "smiley face".to_string(),
            written: "🙂".to_string(),
        }];
        assert_eq!(
            apply_emoji_replacements("smiley face! xsmiley face smiley facey", &replacements,),
            "🙂! xsmiley face smiley facey"
        );
        assert_eq!(
            apply_emoji_replacements("你好 smiley face。你smiley face", &replacements),
            "你好 🙂。你smiley face"
        );
    }

    /// The settings UI prints `smiley face` as its worked example, and a
    /// recognizer capitalizes the first word of a sentence, so a byte-exact
    /// matcher made the shipped example fire mid-sentence and stay silent at
    /// the start of one. Measured on `say`-rendered speech: parakeet writes
    /// `Smiley face, that is my answer.`
    ///
    /// The replacement stage is asserted beside it because the two are copies
    /// of one matcher and this is the copy that drifted. `apply_snippets` is
    /// the third; its own module already pins the same folding.
    #[test]
    fn an_emoji_rule_matches_the_case_the_recognizer_chose() {
        let spoken = "smiley face";
        let heard = "Smiley face, that is my answer.";
        let expected = "🙂, that is my answer.";

        let emoji = vec![EmojiReplacement {
            spoken: spoken.to_string(),
            written: "🙂".to_string(),
        }];
        assert_eq!(apply_emoji_replacements(heard, &emoji), expected);

        assert_eq!(
            apply_text_replacements(heard, &[rule(spoken, "🙂")]),
            expected
        );

        // Folding case is not fuzz: a phrase the user never wrote still misses,
        // and a token boundary still bounds the one they did.
        assert_eq!(
            apply_emoji_replacements("Smiler face and xsmiley face", &emoji),
            "Smiler face and xsmiley face"
        );
    }
    #[test]
    fn literal_punctuation_is_english_only_opt_in_and_respects_vocabulary() {
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());
        let portuguese = OutputLanguageEvidence::UserSelected("pt-BR".to_string());

        assert_eq!(
            apply_literal_punctuation("a comma b", &english, true, &[]),
            "a, b"
        );
        assert_eq!(
            apply_literal_punctuation("the word comma itself", &english, true, &[]),
            "the word comma itself"
        );
        assert_eq!(
            apply_literal_punctuation("a comma b", &english, false, &[]),
            "a comma b"
        );
        assert_eq!(
            apply_literal_punctuation("a comma b", &portuguese, true, &[]),
            "a comma b"
        );

        let vocabulary = vec![pair("comma", "COMMA")];
        let literal = apply_literal_punctuation("a comma b", &english, true, &vocabulary);
        assert_eq!(literal, "a comma b");
        assert_eq!(
            apply_vocabulary_entries(&literal, &vocabulary, 0.18),
            "a COMMA b"
        );
    }

    #[test]
    fn british_spelling_table_is_complete_and_case_preserving() {
        let english = OutputLanguageEvidence::UserSelected("en-GB".to_string());
        assert!(AMERICAN_TO_BRITISH_SPELLINGS
            .windows(2)
            .all(|pair| pair[0].0 < pair[1].0));
        for &(american, british) in AMERICAN_TO_BRITISH_SPELLINGS {
            assert!(!american.is_empty() && american.bytes().all(|byte| byte.is_ascii_lowercase()));
            assert!(!british.is_empty() && british.bytes().all(|byte| byte.is_ascii_lowercase()));
            let title_source = format!("{}{}", american[..1].to_ascii_uppercase(), &american[1..]);
            let title_expected = format!("{}{}", british[..1].to_ascii_uppercase(), &british[1..]);
            assert_eq!(
                apply_british_spelling(american, &english, EnglishSpelling::British, &[]),
                british,
                "lowercase form for {american}",
            );
            assert_eq!(
                apply_british_spelling(
                    &american.to_ascii_uppercase(),
                    &english,
                    EnglishSpelling::British,
                    &[],
                ),
                british.to_ascii_uppercase(),
                "uppercase form for {american}",
            );
            assert_eq!(
                apply_british_spelling(&title_source, &english, EnglishSpelling::British, &[],),
                title_expected,
                "title case form for {american}",
            );
        }
    }

    #[test]
    fn british_spelling_covers_common_missing_forms_without_partial_matches() {
        let english = OutputLanguageEvidence::UserSelected("en".to_string());
        assert_eq!(
            apply_british_spelling(
                "gray traveled canceled labeled mold tire jewelry discoloration",
                &english,
                EnglishSpelling::British,
                &[],
            ),
            "grey travelled cancelled labelled mould tyre jewellery discoloration",
        );
    }

    #[test]
    fn british_spelling_preserves_case_and_yields_to_vocabulary() {
        let english = OutputLanguageEvidence::UserSelected("en".to_string());
        let portuguese = OutputLanguageEvidence::UserSelected("pt".to_string());

        assert_eq!(
            apply_british_spelling(
                "Color colors ORGANIZE",
                &english,
                EnglishSpelling::British,
                &[],
            ),
            "Colour colours ORGANISE"
        );
        assert_eq!(
            apply_british_spelling("color", &english, EnglishSpelling::AsSpoken, &[],),
            "color"
        );
        assert_eq!(
            apply_british_spelling("color", &portuguese, EnglishSpelling::British, &[],),
            "color"
        );

        let vocabulary = vec![pair("color", "BrandColor")];
        let protected =
            apply_british_spelling("color", &english, EnglishSpelling::British, &vocabulary);
        assert_eq!(protected, "color");
        assert_eq!(
            apply_vocabulary_entries(&protected, &vocabulary, 0.18),
            "BrandColor"
        );
    }
}
