//! Spoken editing commands (`scratch that`, `quote that`, ...) and the
//! `Sona,` cue that turns a trailing sentence into a rewrite instruction.
//! Both rewrite one transcript once, before delivery; see
//! [`apply_spoken_edits`] and [`split_spoken_instruction`].

use super::text::OutputLanguageEvidence;
use once_cell::sync::Lazy;
use regex::Regex;
use std::ops::Range;

/// Punctuation that ends a spoken *segment*: every mark a transcript renders
/// for a pause. This is boundary evidence, not a sentence model — a comma is a
/// pause, so `scratch that,` is as much a standalone utterance as
/// `Scratch that.` is.
const SEGMENT_BOUNDARY_MARKS: &[char] = &['.', '!', '?', '…', ',', ';', ':', '\n'];

/// The subset of [`SEGMENT_BOUNDARY_MARKS`] that closes a sentence, which is
/// how far back `scratch that` reaches. Shared with the query plane, which
/// splits a transcript chunk into sentences to quote the one that matched:
/// which marks close a sentence is a fact about the language, not about either
/// caller.
pub(crate) const SENTENCE_END_MARKS: &[char] = &['.', '!', '?', '…', '\n'];

/// Padding that is not a line break. Vertical whitespace is load-bearing here
/// (an earlier stage emits it deliberately), so it is never trimmed away.
const HORIZONTAL_WHITESPACE: &[char] = &[' ', '\t'];

/// Whitespace between two words. A line break is padding too: the word before
/// it is still the previous word, so a word-scoped edit reaches across it.
const WORD_PADDING: &[char] = &[' ', '\t', '\n', '\r'];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpokenEdit {
    /// Drop the clause the speaker was in the middle of.
    ScratchClause,
    /// Drop exactly the word before the command.
    DeleteLastWord,
    /// Raise the first letter of the word before the command.
    CapitalizeLastWord,
    /// Lower every letter of the word before the command.
    LowercaseLastWord,
    /// Wrap the sentence before the command in quotes.
    QuoteLastSentence,
    /// Rewrite the sentence before the command as one bullet per item.
    ListLastSentence,
    /// Continue on a new line, as a bullet.
    NewBullet,
}

/// The complete English spoken-edit table.
///
/// Deliberately absent:
///
/// * `new line` / `new paragraph`. The literal-punctuation table is the one
///   owner of spoken punctuation (see `spoken_punctuation_at` in [`super::text`], which already
///   writes `\n` and `\n\n`, and the same note on
///   [`crate::settings::default_replacement_rules`]). Shipping them here too
///   would duplicate that responsibility and quietly override a user who
///   turned the per-mode `literal_punctuation` choice off. `new bullet` is not
///   that case: a list marker is not punctuation, nothing in that table writes
///   one, so there is no owner here to duplicate.
/// * `delete last sentence`. Evaluated and rejected as an alias: because
///   [`scratch_clause`] first drops the mark that closed the scratched clause
///   and then reaches back to the previous sentence end, `scratch that` after a
///   punctuated sentence already deletes exactly that sentence. Two names for
///   one behaviour, disagreeing only about whether the terminator survives, is
///   a worse contract than one name.
/// * `undo that`. Evaluated and rejected because there is nothing to undo: this
///   stage rewrites one transcript once, left to right, and keeps no earlier
///   version of it to restore. Text already delivered elsewhere is out of reach
///   for the same reason [`apply_spoken_edits`] is not a mid-stream editor. An
///   `undo that` that quietly did nothing would be worse than its absence,
///   because the phrase would stop being typed without becoming an edit.
/// * `redo that sentence`. Evaluated and rejected because it names a
///   re-dictation, not an edit: nothing in the transcript says what the
///   replacement should be. The honest implementation deletes the sentence and
///   waits for the speaker to say it again, which is `scratch that` plus
///   speaking — a second name for a behaviour that already has one.
const SPOKEN_EDIT_COMMANDS: &[(&str, SpokenEdit)] = &[
    ("scratch that", SpokenEdit::ScratchClause),
    ("delete last word", SpokenEdit::DeleteLastWord),
    ("delete the last word", SpokenEdit::DeleteLastWord),
    ("capitalize that", SpokenEdit::CapitalizeLastWord),
    ("lowercase that", SpokenEdit::LowercaseLastWord),
    ("quote that", SpokenEdit::QuoteLastSentence),
    ("make that a list", SpokenEdit::ListLastSentence),
    ("new bullet", SpokenEdit::NewBullet),
];

/// Applies spoken editing commands to the transcript about to be delivered.
///
/// This rewrites the *current* transcript before delivery. It is not a
/// mid-stream editor: text Sona has already typed elsewhere is out of reach and
/// stays that way.
///
/// # The boundary rule
///
/// A command fires only when it is **exactly one whole segment**. A segment is
/// a maximal run of text delimited by [`SEGMENT_BOUNDARY_MARKS`] or by either
/// end of the transcript; leading and trailing whitespace does not count, and
/// the marks that closed the command's own segment are consumed with it.
/// Matching is word-wise and case-insensitive, so `Scratch that.` and
/// `scratch  that,` both fire.
///
/// That single rule is the whole false-positive defence, and it is what the
/// roadmap's named risk needs:
///
/// * `scratch that plan` — the segment is `scratch that plan`, which is not a
///   command phrase. No continuation-word list is required; any extra word in
///   the segment is already a rejection.
/// * `we should scratch that.` — the segment starts before the phrase, so the
///   phrase is not the whole segment. Same for `don't scratch that itch.`
/// * `I like turtles scratch that` — no rendered pause, so no segment
///   boundary, so no command. Without punctuation there is no evidence the
///   speaker issued a command rather than said the words, and guessing here is
///   how a dictation tool eats a sentence the user meant to keep.
///
/// The remaining ambiguity is a speaker who genuinely utters a command phrase
/// as a whole clause (`He said, scratch that, and left.`). That is
/// indistinguishable from the command by construction, which is why the setting
/// ships off and why the escape hatch is turning it off rather than a
/// heuristic.
///
/// # Language
///
/// English only, and it fails closed: an unknown output language skips the
/// stage rather than matching English phrases against speech that is not
/// English. Other locales need their own tables, not a translation of this one.
pub fn apply_spoken_edits(text: &str, language: &OutputLanguageEvidence, enabled: bool) -> String {
    if !enabled || !language.is_english() {
        return text.to_string();
    }

    let mut output = String::with_capacity(text.len());
    // Set by a fired command so the following segment closes the gap the
    // deleted words left. It is never set on a text-only pass, which is what
    // keeps this stage byte-exact when no command fires.
    let mut repair_gap = false;
    let mut segment_start = 0;

    while let Some(offset) = text[segment_start..].find(SEGMENT_BOUNDARY_MARKS) {
        let marks_start = segment_start + offset;
        // An adjacent mark run ("...", "?!", ".\n") closes one segment, not
        // several empty ones.
        let marks_end = marks_start
            + text[marks_start..]
                .find(|character: char| !SEGMENT_BOUNDARY_MARKS.contains(&character))
                .unwrap_or(text.len() - marks_start);
        push_spoken_edit_segment(
            &mut output,
            &text[segment_start..marks_start],
            &text[marks_start..marks_end],
            &mut repair_gap,
        );
        segment_start = marks_end;
    }
    if segment_start < text.len() {
        push_spoken_edit_segment(&mut output, &text[segment_start..], "", &mut repair_gap);
    }

    output
}

/// Appends one segment and the mark run that closed it, or applies the command
/// that segment turned out to be.
fn push_spoken_edit_segment(
    output: &mut String,
    segment: &str,
    marks: &str,
    repair_gap: &mut bool,
) {
    if let Some(edit) = spoken_edit_for_segment(segment) {
        match edit {
            SpokenEdit::ScratchClause => scratch_clause(output),
            SpokenEdit::DeleteLastWord => delete_last_word(output),
            SpokenEdit::CapitalizeLastWord => capitalize_last_word(output),
            SpokenEdit::LowercaseLastWord => lowercase_last_word(output),
            SpokenEdit::QuoteLastSentence => quote_last_sentence(output),
            SpokenEdit::ListLastSentence => list_last_sentence(output),
            SpokenEdit::NewBullet => start_new_bullet(output),
        }
        *repair_gap = true;
        return;
    }

    if std::mem::take(repair_gap) {
        let segment = segment.trim_start_matches(HORIZONTAL_WHITESPACE);
        if !segment.is_empty() && !output.is_empty() && !output.ends_with(char::is_whitespace) {
            output.push(' ');
        }
        output.push_str(segment);
    } else {
        output.push_str(segment);
    }
    output.push_str(marks);
}

fn spoken_edit_for_segment(segment: &str) -> Option<SpokenEdit> {
    let segment = segment.trim();
    if segment.is_empty() {
        return None;
    }
    SPOKEN_EDIT_COMMANDS
        .iter()
        .find(|(phrase, _)| segment_is_phrase(segment, phrase))
        .map(|(_, edit)| *edit)
}

/// Whole-segment, word-wise, case-insensitive equality. Word-wise rather than
/// string equality so a doubled space between two words still matches; whole
/// segment so an extra word never does.
fn segment_is_phrase(segment: &str, phrase: &str) -> bool {
    let mut words = segment.split_whitespace();
    phrase.split(' ').all(|word| {
        words
            .next()
            .is_some_and(|spoken| spoken.eq_ignore_ascii_case(word))
    }) && words.next().is_none()
}

/// Length of `text` without its trailing run of characters from `marks`.
fn trimmed_len(text: &str, marks: &[&[char]]) -> usize {
    text.trim_end_matches(|character: char| marks.iter().any(|set| set.contains(&character)))
        .len()
}

/// Drops the trailing run of `marks` from `output`.
fn truncate_trailing(output: &mut String, marks: &[char]) {
    let keep = trimmed_len(output, &[marks]);
    output.truncate(keep);
}

/// Byte offset just past the last `marks` character in `output`, or 0.
fn end_of_last_mark(output: &str, marks: &[char]) -> usize {
    match output.rfind(marks) {
        Some(index) => index + output[index..].chars().next().map_or(0, char::len_utf8),
        None => 0,
    }
}

/// Deletes back to the previous sentence end, or to the start of the
/// transcript when there is none.
///
/// The pause before the command is already punctuation by the time this stage
/// runs, so the mark that closed the scratched clause is deleted with it —
/// otherwise `I want to go home. Scratch that.` would find an empty clause
/// between the period and the command and delete nothing, which is the shape
/// Whisper actually emits. A surviving line break is kept: the speaker asked to
/// drop a clause, not to rejoin two lines.
fn scratch_clause(output: &mut String) {
    truncate_trailing(output, HORIZONTAL_WHITESPACE);
    truncate_trailing(output, SEGMENT_BOUNDARY_MARKS);
    truncate_trailing(output, HORIZONTAL_WHITESPACE);
    let keep = end_of_last_mark(output, SENTENCE_END_MARKS);
    output.truncate(keep);
    truncate_trailing(output, HORIZONTAL_WHITESPACE);
}

/// Deletes the last word, together with any punctuation hanging off it: the
/// mark belongs to the position the deleted word occupied, and stranding it
/// ("the quick brown." for a deleted "fox") is never what was asked for.
fn delete_last_word(output: &mut String) {
    truncate_trailing(output, HORIZONTAL_WHITESPACE);
    truncate_trailing(output, SEGMENT_BOUNDARY_MARKS);
    let keep = end_of_last_mark(output, WORD_PADDING);
    output.truncate(keep);
    truncate_trailing(output, HORIZONTAL_WHITESPACE);
}

/// The span of the last word in `output`, ignoring the padding and punctuation
/// behind it. Empty when there is no word to name.
fn last_word_span(output: &str) -> Range<usize> {
    let end = trimmed_len(output, &[HORIZONTAL_WHITESPACE, SEGMENT_BOUNDARY_MARKS]);
    end_of_last_mark(&output[..end], WORD_PADDING)..end
}

/// Replaces the last word with `recase` of itself, leaving the punctuation
/// after it where it is: the speaker named a word, not the mark behind it.
fn recase_last_word(output: &mut String, recase: impl FnOnce(&str) -> String) {
    let span = last_word_span(output);
    let recased = recase(&output[span.clone()]);
    output.replace_range(span, &recased);
}

/// Raises the first letter of the last word.
fn capitalize_last_word(output: &mut String) {
    recase_last_word(output, |word| {
        let mut characters = word.chars();
        let mut capitalized = String::with_capacity(word.len());
        if let Some(first) = characters.next() {
            capitalized.extend(first.to_uppercase());
            capitalized.push_str(characters.as_str());
        }
        capitalized
    });
}

/// Lowers every letter of the last word. Unlike `capitalize that` this is not a
/// one-letter change: a word with an interior capital is not lowercase.
fn lowercase_last_word(output: &mut String) {
    recase_last_word(output, str::to_lowercase);
}

/// The byte offset where the last sentence of `text` begins: just past the
/// previous sentence's terminator and whatever padding follows it, or 0 when
/// there is no earlier sentence. A trailing terminator belongs to the last
/// sentence, so it is not what this measures back from.
fn last_sentence_start(text: &str) -> usize {
    let body = &text[..trimmed_len(text, &[SENTENCE_END_MARKS, HORIZONTAL_WHITESPACE])];
    let start = end_of_last_mark(body, SENTENCE_END_MARKS);
    let tail = &body[start..];
    start + tail.len() - tail.trim_start_matches(HORIZONTAL_WHITESPACE).len()
}

/// Drops the pause that bounded a spoken command's own segment, so a
/// sentence-scoped edit does not inherit it. A sentence terminator is not a
/// pause and survives, because it belongs to the text being edited.
fn truncate_trailing_pause(output: &mut String) {
    truncate_trailing(output, HORIZONTAL_WHITESPACE);
    let keep = output
        .trim_end_matches(|character: char| {
            SEGMENT_BOUNDARY_MARKS.contains(&character) && !SENTENCE_END_MARKS.contains(&character)
        })
        .len();
    output.truncate(keep);
}

/// The bytes of a list marker opening `line`, or 0. A marker is furniture
/// around the sentence on that line, not part of it, so a sentence-scoped edit
/// reads past it and a rewrite of the whole sentence replaces it.
fn list_marker_len(line: &str) -> usize {
    if line.starts_with("- ") {
        2
    } else {
        0
    }
}

/// Wraps the last sentence in quotes, its terminator included: `"he said it."`
/// is a quotation, `"he said it".` is a quotation plus a stranded period. A
/// line break is the one terminator that stays outside: it ends the sentence
/// without being part of it, and a closing quote on the next line reads as a
/// stray mark.
fn quote_last_sentence(output: &mut String) {
    truncate_trailing_pause(output);
    let line_start = last_sentence_start(output);
    let start = line_start + list_marker_len(&output[line_start..]);
    let close = output.trim_end().len();
    if close <= start {
        return;
    }
    output.insert(close, '"');
    output.insert(start, '"');
}

/// A comma, or the word `and`, between two spoken list items. Both spellings of
/// the last separator ("milk, eggs and bread", "milk, eggs, and bread") reduce
/// to the same items because an empty piece between two separators is dropped.
static LIST_ITEM_SEPARATOR: Lazy<Regex> = Lazy::new(|| {
    // PANIC: a fixed pattern with no user input in it.
    Regex::new(r"(?i),|\band\b").expect("list separator pattern is valid")
});

/// Rewrites the last sentence as one bullet per item. The sentence's terminator
/// goes with it — a list of bullets has no sentence left to end — and the items
/// keep their own wording and case, because the speaker asked for a list and
/// not for a rewrite.
///
/// The split is literal, so a name containing `and` ("Ben and Jerry's") becomes
/// two bullets. Splitting on the word is what makes the common spoken list work
/// at all, and the escape hatch is not saying the command.
fn list_last_sentence(output: &mut String) {
    truncate_trailing_pause(output);
    let start = last_sentence_start(output);
    let line = &output[start + list_marker_len(&output[start..])..];
    let sentence = &line[..trimmed_len(line, &[SENTENCE_END_MARKS, HORIZONTAL_WHITESPACE])];
    let mut list = String::with_capacity(sentence.len());
    for item in LIST_ITEM_SEPARATOR.split(sentence).map(str::trim) {
        if item.is_empty() {
            continue;
        }
        if !list.is_empty() {
            list.push('\n');
        }
        list.push_str("- ");
        list.push_str(item);
    }
    if list.is_empty() {
        return;
    }
    output.truncate(start);
    truncate_trailing(output, HORIZONTAL_WHITESPACE);
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&list);
}

/// Starts a bullet on its own line. Whatever is dictated next continues inside
/// it, which is why the marker keeps its trailing space.
fn start_new_bullet(output: &mut String) {
    truncate_trailing_pause(output);
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str("- ");
}

/// The cue that makes a trailing sentence an instruction for the rewrite model
/// instead of words to type. Addressing the app by name is the evidence: a
/// dictation that merely mentions Sona carries on, while `Sona,` opening the
/// last sentence is a speaker turning to talk to it.
const SPOKEN_INSTRUCTION_CUE: &str = "sona,";

/// A dictation that ended by asking for an edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpokenInstruction {
    /// The dictation with the cue sentence removed, never empty. This is what
    /// gets delivered when the instruction cannot be applied.
    pub text: String,
    /// What the speaker asked for, cue removed.
    pub instruction: String,
}

/// Splits a trailing `Sona, …` sentence off a transcript.
///
/// # What fires
///
/// The last sentence only, and only when the cue opens it. The reach is a
/// sentence rather than a segment because the cue ends in the very mark that
/// bounds a segment: `Sona, make that a question.` is one spoken sentence and
/// two segments.
///
/// A cue that directs nothing (`Sona,` alone) or has nothing to direct (a
/// dictation that is only the cue sentence) is not an instruction and stays
/// text. The second case is a speaker asking to edit words this run did not
/// dictate, which is out of reach for the same reason [`apply_spoken_edits`]
/// is not a mid-stream editor; typing their words back is the honest answer,
/// and it is the same answer a bare `Sona,` already gets.
///
/// This never consults [`SPOKEN_EDIT_COMMANDS`]. While the literal stage runs,
/// it has already answered: the cue's comma is itself a segment boundary, so
/// words after the cue that spell a command phrase (`Sona, scratch that.`) are
/// a whole segment, and [`apply_spoken_edits`] — which runs first — applies the
/// command and takes the cue with it. That stage is a separate switch, though:
/// it is off by default and English only, while this cue is per mode and
/// language-agnostic. With it off, a command phrase behind the cue is an
/// instruction like any other, and the model reads it (`scratch that.` over
/// `Hello.`), which is the closest this path can come to what was asked.
///
/// # Language
///
/// Not gated on English, unlike [`apply_spoken_edits`]. The cue is the app's
/// name rather than an English phrase, and the instruction after it is read by
/// a model that handles the speaker's language, so an English gate here would
/// refuse `Sona, mach das kürzer.` for no reason.
///
/// The remaining ambiguity is a speaker who dictates a sentence that opens with
/// the app's name ("Sona, the dictation app, shipped today."). That is
/// indistinguishable from the cue by construction, which is why the setting
/// ships off.
pub fn split_spoken_instruction(text: &str) -> Option<SpokenInstruction> {
    let start = last_sentence_start(text);
    let sentence = text[start..].trim();
    if !sentence
        .get(..SPOKEN_INSTRUCTION_CUE.len())
        .is_some_and(|cue| cue.eq_ignore_ascii_case(SPOKEN_INSTRUCTION_CUE))
    {
        return None;
    }
    let instruction = sentence[SPOKEN_INSTRUCTION_CUE.len()..].trim();
    let mut kept = text[..start].to_string();
    truncate_trailing(&mut kept, HORIZONTAL_WHITESPACE);
    if instruction.is_empty() || kept.trim().is_empty() {
        return None;
    }
    Some(SpokenInstruction {
        text: kept,
        instruction: instruction.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_toolkit::apply_literal_punctuation;

    fn spoken_edits(text: &str) -> String {
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());
        apply_spoken_edits(text, &english, true)
    }

    #[test]
    fn scratch_that_drops_the_clause_back_to_the_previous_sentence_end() {
        // The shape Whisper actually emits: the pause before the command is
        // already a period, and the scratched clause is a whole sentence.
        assert_eq!(spoken_edits("I want to go home. Scratch that."), "");
        assert_eq!(spoken_edits("One. Two. Scratch that."), "One.");
        assert_eq!(spoken_edits("One! Two? Scratch that."), "One!");
        assert_eq!(spoken_edits("One… Two… Scratch that."), "One…");
        // A comma is a pause, so it bounds the command's segment, but it does
        // not end a sentence, so the deletion reaches back past it.
        assert_eq!(spoken_edits("Hello. World, scratch that."), "Hello.");
        assert_eq!(spoken_edits("One, two, scratch that."), "");
        // Dictation continues after the command in the same transcript.
        assert_eq!(
            spoken_edits("One. Two. Scratch that. Three."),
            "One. Three."
        );
        assert_eq!(spoken_edits("Scratch that. Hello."), "Hello.");
        // A line break is a segment boundary and a sentence end, and it
        // survives: the speaker asked to drop a clause, not to rejoin lines.
        assert_eq!(
            spoken_edits("One.\nTwo. Scratch that. Three."),
            "One.\nThree."
        );
    }

    #[test]
    fn scratch_that_does_not_fire_inside_ordinary_speech() {
        // The roadmap's named risk.
        assert_eq!(spoken_edits("scratch that plan"), "scratch that plan");
        assert_eq!(
            spoken_edits("We should scratch that plan."),
            "We should scratch that plan."
        );
        // The phrase is not the whole segment.
        assert_eq!(
            spoken_edits("We should scratch that."),
            "We should scratch that."
        );
        assert_eq!(
            spoken_edits("Don't scratch that itch."),
            "Don't scratch that itch."
        );
        assert_eq!(
            spoken_edits("Remember to scratch that, then leave."),
            "Remember to scratch that, then leave."
        );
        // No rendered pause means no evidence of a command, even trailing.
        assert_eq!(
            spoken_edits("I like turtles scratch that"),
            "I like turtles scratch that"
        );
        // A quoted mention is not a bare segment either.
        assert_eq!(
            spoken_edits("He said \"scratch that\"."),
            "He said \"scratch that\"."
        );
    }

    #[test]
    fn delete_last_word_removes_exactly_one_word_with_its_punctuation() {
        assert_eq!(
            spoken_edits("the quick brown fox. Delete last word."),
            "the quick brown"
        );
        assert_eq!(
            spoken_edits("the quick brown fox, delete the last word."),
            "the quick brown"
        );
        assert_eq!(spoken_edits("solo. Delete last word."), "");
        // Vertical whitespace is load-bearing and stays.
        assert_eq!(spoken_edits("One\ntwo. Delete last word."), "One\n");
        // Negatives: not a whole segment, or no boundary at all.
        assert_eq!(
            spoken_edits("Please delete last word from the file."),
            "Please delete last word from the file."
        );
        assert_eq!(
            spoken_edits("the quick brown fox delete last word"),
            "the quick brown fox delete last word"
        );
    }

    #[test]
    fn capitalize_and_lowercase_recase_the_previous_word_only() {
        assert_eq!(
            spoken_edits("the quick brown fox. Capitalize that."),
            "the quick brown Fox."
        );
        assert_eq!(spoken_edits("Hello WORLD. Lowercase that."), "Hello world.");
        assert_eq!(spoken_edits("solo. Capitalize that."), "Solo.");
        // Vertical whitespace is load-bearing, and the word before it is still
        // the previous word.
        assert_eq!(spoken_edits("One\ntwo. Capitalize that."), "One\nTwo.");
        // Dictation continues after the command.
        assert_eq!(
            spoken_edits("hello world. Capitalize that. And more."),
            "hello World. And more."
        );
        // Nothing to recase leaves an empty transcript rather than panicking.
        assert_eq!(spoken_edits("Capitalize that."), "");
        assert_eq!(spoken_edits("Lowercase that."), "");
    }

    #[test]
    fn quote_that_wraps_the_previous_sentence_with_its_terminator() {
        assert_eq!(
            spoken_edits("He said hello. Quote that."),
            "\"He said hello.\""
        );
        // Only the last sentence, and the padding before it stays outside.
        assert_eq!(spoken_edits("One. Two. Quote that."), "One. \"Two.\"");
        // An unterminated sentence is still a sentence.
        assert_eq!(
            spoken_edits("Hello. World, quote that."),
            "Hello. \"World\""
        );
        // A line break ends the sentence without being part of it, so the
        // closing mark goes before it and the break survives after.
        assert_eq!(
            spoken_edits("He said hello\nquote that"),
            "\"He said hello\"\n"
        );
        assert_eq!(spoken_edits("One.\nTwo.\nQuote that."), "One.\n\"Two.\"\n");
        assert_eq!(spoken_edits("Quote that."), "");
    }

    #[test]
    fn make_that_a_list_splits_the_previous_sentence_into_bullets() {
        assert_eq!(
            spoken_edits("Milk, eggs and bread. Make that a list."),
            "- Milk\n- eggs\n- bread"
        );
        // The Oxford comma leaves an empty piece between two separators, which
        // is dropped rather than becoming a bullet.
        assert_eq!(
            spoken_edits("Milk, eggs, and bread. Make that a list."),
            "- Milk\n- eggs\n- bread"
        );
        // Only the last sentence becomes the list, on its own line.
        assert_eq!(
            spoken_edits("Shopping. Milk, eggs and bread. Make that a list."),
            "Shopping.\n- Milk\n- eggs\n- bread"
        );
        assert_eq!(spoken_edits("Milk. Make that a list."), "- Milk");
        assert_eq!(spoken_edits("Make that a list."), "");
    }

    #[test]
    fn new_bullet_continues_the_dictation_inside_a_marker() {
        assert_eq!(
            spoken_edits("Buy milk. New bullet. Buy eggs."),
            "Buy milk.\n- Buy eggs."
        );
        // A bullet at the very start needs no line break before it.
        assert_eq!(spoken_edits("New bullet. Buy milk."), "- Buy milk.");
        assert_eq!(
            spoken_edits("New bullet. Milk. New bullet. Eggs."),
            "- Milk.\n- Eggs."
        );
        // A comma is a pause, so it bounds the command's segment, and it goes
        // with the command rather than dangling before the list.
        assert_eq!(
            spoken_edits("Buy milk, new bullet, buy eggs."),
            "Buy milk\n- buy eggs."
        );
    }

    #[test]
    fn a_bullet_marker_is_furniture_around_the_sentence_not_part_of_it() {
        assert_eq!(
            spoken_edits("Buy milk. New bullet. Eggs, ham. Make that a list."),
            "Buy milk.\n- Eggs\n- ham"
        );
        assert_eq!(spoken_edits("New bullet. Milk. Quote that."), "- \"Milk.\"");
    }

    #[test]
    fn the_new_commands_obey_the_whole_segment_boundary_rule() {
        for source in [
            "Please capitalize that word.",
            "Can you lowercase that for me?",
            "He asked me to quote that, then left.",
            "We should make that a list of names.",
            "Add a new bullet to the deck.",
            // No rendered pause means no evidence of a command at all.
            "the quick brown fox capitalize that",
        ] {
            assert_eq!(spoken_edits(source), source);
        }
    }

    #[test]
    fn new_line_and_new_paragraph_stay_owned_by_the_literal_punctuation_table() {
        // Break phrases are not in SPOKEN_EDIT_COMMANDS, so this stage leaves
        // them alone; the upstream literal-punctuation stage already converts
        // them, gated on the user's own per-mode choice.
        assert_eq!(spoken_edits("one new line two"), "one new line two");
        assert_eq!(
            spoken_edits("one new paragraph two"),
            "one new paragraph two"
        );

        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());
        assert_eq!(
            apply_literal_punctuation("one new line two", &english, true, &[]),
            "one\ntwo"
        );
        assert_eq!(
            apply_literal_punctuation("one new paragraph two", &english, true, &[]),
            "one\n\ntwo"
        );
    }

    #[test]
    fn multiple_commands_apply_left_to_right() {
        assert_eq!(spoken_edits("One. Two. Scratch that. Scratch that."), "");
        assert_eq!(
            spoken_edits("One. Two three. Delete last word. Scratch that. Four."),
            "One. Four."
        );
        assert_eq!(
            spoken_edits("alpha beta gamma. Delete last word. Delete last word."),
            "alpha"
        );
    }

    #[test]
    fn a_command_with_nothing_to_delete_leaves_an_empty_transcript() {
        assert_eq!(spoken_edits("Scratch that."), "");
        assert_eq!(spoken_edits("Scratch that"), "");
        assert_eq!(spoken_edits("Delete last word."), "");
        assert_eq!(spoken_edits(""), "");
    }

    #[test]
    fn spoken_edits_are_opt_in_and_english_only() {
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());
        let british = OutputLanguageEvidence::ModelDetected("en".to_string());
        let portuguese = OutputLanguageEvidence::UserSelected("pt-BR".to_string());
        let source = "One. Two. Scratch that.";

        assert_eq!(apply_spoken_edits(source, &english, true), "One.");
        assert_eq!(apply_spoken_edits(source, &british, true), "One.");
        // Disabled, and unknown language, both fail closed to the input.
        assert_eq!(apply_spoken_edits(source, &english, false), source);
        assert_eq!(apply_spoken_edits(source, &portuguese, true), source);
        assert_eq!(
            apply_spoken_edits(source, &OutputLanguageEvidence::Unknown, true),
            source
        );
    }

    #[test]
    fn text_without_a_command_survives_byte_for_byte() {
        for source in [
            "Hello, world.",
            "  leading and trailing padding\t",
            "One.\n\nTwo… three?! Four; five: six,",
            "「日本語。」",
            "",
        ] {
            assert_eq!(spoken_edits(source), source);
        }
    }

    fn split(text: &str) -> Option<(String, String)> {
        split_spoken_instruction(text).map(|split| (split.text, split.instruction))
    }

    #[test]
    fn a_trailing_cue_sentence_becomes_the_instruction() {
        assert_eq!(
            split("The plan is ready by Friday. Sona, make that a question."),
            Some((
                "The plan is ready by Friday.".to_string(),
                "make that a question.".to_string()
            ))
        );
        // Case and padding around the cue do not matter.
        assert_eq!(
            split("Ready.  sona,   shorten it."),
            Some(("Ready.".to_string(), "shorten it.".to_string()))
        );
        // A line break ends a sentence, and survives in the kept text.
        assert_eq!(
            split("One.\nSona, join those."),
            Some(("One.\n".to_string(), "join those.".to_string()))
        );
    }

    #[test]
    fn only_a_trailing_cue_sentence_is_an_instruction() {
        // No cue at all.
        assert_eq!(split("The plan is ready. Make that a question."), None);
        // The cue is not the last sentence, so it is dictation.
        assert_eq!(split("Sona, make it short. Then send it."), None);
        // The cue mid-sentence is a mention, not an address.
        assert_eq!(split("I love Sona, the dictation app."), None);
        // A cue that directs nothing, or has nothing to direct, is left as
        // text: an instruction with no text to apply to is not an edit.
        assert_eq!(split("Ready. Sona,"), None);
        assert_eq!(split("Sona, make it shorter."), None);
        assert_eq!(split("  Sona, make it shorter."), None);
        assert_eq!(split(""), None);
    }

    #[test]
    fn the_literal_table_answers_a_cue_that_names_a_command() {
        // The cue's own comma bounds a segment, so `scratch that` behind it is a
        // whole segment: the literal stage applies the command and the cue goes
        // with the scratched clause, leaving nothing for the cue path to find.
        assert_eq!(spoken_edits("Hello. Sona, scratch that."), "Hello.");
        assert_eq!(split("Hello."), None);
        // A cue sentence that is not a command survives the literal stage
        // byte-for-byte, so the cue path sees the whole transcript.
        let dictation = "Ready. Sona, make it shorter.";
        assert_eq!(spoken_edits(dictation), dictation);
        assert_eq!(
            split(dictation),
            Some(("Ready.".to_string(), "make it shorter.".to_string()))
        );
    }

    #[test]
    fn with_the_literal_stage_off_a_command_phrase_behind_the_cue_is_an_instruction() {
        // The shipped default: the literal stage is off, so nothing has
        // answered, and the cue path is the only reader left.
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());
        let dictation = "Hello. Sona, scratch that.";
        assert_eq!(apply_spoken_edits(dictation, &english, false), dictation);
        assert_eq!(
            split(dictation),
            Some(("Hello.".to_string(), "scratch that.".to_string()))
        );
    }
}
