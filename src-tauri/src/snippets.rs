use crate::audio_toolkit::text::{has_token_boundaries, lowercase_prefix_len};
use serde::{Deserialize, Serialize};
use specta::Type;

/// A user-authored text expansion. The trigger is matched on whole words after
/// vocabulary correction; the expansion is inserted verbatim.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct Snippet {
    pub id: String,
    pub trigger: String,
    pub expansion: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Snippet {
    pub fn trim_outer_whitespace(mut self) -> Self {
        self.trigger = self.trigger.trim().to_string();
        self.expansion = self.expansion.trim().to_string();
        self
    }

    pub fn is_usable(&self) -> bool {
        !self.trigger.is_empty() && !self.expansion.is_empty()
    }
}

/// One trigger prepared for matching. Triggers are compared through simple
/// lowercase folding rather than a regex, so trigger text can never be read as
/// pattern syntax.
struct PreparedSnippet<'a> {
    lowercase_trigger: String,
    expansion: &'a str,
}

/// Replaces enabled snippet triggers with their expansions. Matching is
/// case-insensitive, respects the same Unicode token boundaries as emoji
/// replacement, and prefers the longest trigger when several match at one
/// position.
pub fn apply_snippets(text: &str, snippets: &[Snippet]) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    let prepared: Vec<PreparedSnippet<'_>> = snippets
        .iter()
        .filter(|snippet| snippet.enabled && snippet.is_usable())
        .map(|snippet| PreparedSnippet {
            lowercase_trigger: snippet.trigger.to_lowercase(),
            expansion: snippet.expansion.as_str(),
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
        for snippet in &prepared {
            let Some(length) = lowercase_prefix_len(&text[start..], &snippet.lowercase_trigger)
            else {
                continue;
            };
            let end = start + length;
            if has_token_boundaries(text, start, end)
                && selected.is_none_or(|(current, _)| length > current)
            {
                selected = Some((length, snippet.expansion));
            }
        }

        if let Some((length, expansion)) = selected {
            result.push_str(&text[cursor..start]);
            result.push_str(expansion);
            cursor = start + length;
        }
    }

    if cursor == 0 {
        return text.to_string();
    }
    result.push_str(&text[cursor..]);
    result
}

/// An expansion with its placeholders filled in.
#[derive(Debug, PartialEq, Eq)]
pub struct Filled {
    pub text: String,
    /// Characters after `{cursor}`: how far the caret moves back once the
    /// text is in. Zero without one.
    pub caret_back: usize,
}

/// Fills `{date}`, `{time}`, `{clipboard}` and `{cursor}`. Any other braces
/// stay as written. The clipboard is read only when the expansion uses it.
pub fn fill_placeholders(
    expansion: &str,
    now: &chrono::DateTime<chrono::Local>,
    clipboard: impl FnOnce() -> Option<String>,
) -> Filled {
    let mut read_clipboard = Some(clipboard);
    let mut clipboard_text: Option<String> = None;
    let mut text = String::with_capacity(expansion.len());
    let mut cursor_at = None;
    let mut rest = expansion;
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        let tail = &rest[open..];
        rest = if let Some(after) = tail.strip_prefix("{date}") {
            text.push_str(&now.format("%B %-d, %Y").to_string());
            after
        } else if let Some(after) = tail.strip_prefix("{time}") {
            text.push_str(&now.format("%-I:%M %p").to_string());
            after
        } else if let Some(after) = tail.strip_prefix("{clipboard}") {
            let pasted = clipboard_text.get_or_insert_with(|| {
                read_clipboard
                    .take()
                    .and_then(|read| read())
                    .unwrap_or_default()
            });
            text.push_str(pasted);
            after
        } else if let Some(after) = tail.strip_prefix("{cursor}") {
            cursor_at.get_or_insert(text.len());
            after
        } else {
            text.push('{');
            &tail[1..]
        };
    }
    text.push_str(rest);
    let caret_back = cursor_at.map_or(0, |at| text[at..].chars().count());
    Filled { text, caret_back }
}

/// One key as the typed-shortcut matcher sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keystroke {
    Text(char),
    Erase,
    /// The caret moved or the text changed some other way, so what came
    /// before is no longer known.
    Reset,
}

/// A shortcut typed and ended with a space: erase that many characters,
/// the space included, and write the expansion in their place.
#[derive(Debug, PartialEq, Eq)]
pub struct TypedExpansion {
    pub erase: usize,
    pub expansion: String,
}

/// The characters typed since the caret last moved, enough to hold the
/// longest trigger.
#[derive(Debug, Default)]
pub struct TypedTriggers {
    typed: String,
}

const TYPED_MEMORY_BYTES: usize = 256;

impl TypedTriggers {
    /// A trigger expands when a space follows it and it starts a word, so
    /// "sig" never fires inside "design". The longest trigger wins.
    pub fn key(&mut self, keystroke: Keystroke, snippets: &[Snippet]) -> Option<TypedExpansion> {
        match keystroke {
            Keystroke::Reset => self.typed.clear(),
            Keystroke::Erase => {
                self.typed.pop();
            }
            Keystroke::Text(' ') => {
                let found = snippets
                    .iter()
                    .filter(|snippet| snippet.enabled && snippet.is_usable())
                    .filter(|snippet| ends_with_word(&self.typed, &snippet.trigger))
                    .max_by_key(|snippet| snippet.trigger.chars().count());
                if let Some(snippet) = found {
                    self.typed.clear();
                    return Some(TypedExpansion {
                        erase: snippet.trigger.chars().count() + 1,
                        expansion: snippet.expansion.clone(),
                    });
                }
                self.typed.push(' ');
            }
            Keystroke::Text(character) => self.typed.push(character),
        }
        if self.typed.len() > TYPED_MEMORY_BYTES {
            let half = self.typed.len() / 2;
            let keep_from = (half..self.typed.len())
                .find(|&index| self.typed.is_char_boundary(index))
                .unwrap_or(half);
            self.typed.drain(..keep_from);
        }
        None
    }
}

fn ends_with_word(typed: &str, trigger: &str) -> bool {
    let mut typed = typed.chars().rev();
    for wanted in trigger.chars().rev() {
        match typed.next() {
            Some(got) if got.to_lowercase().eq(wanted.to_lowercase()) => {}
            _ => return false,
        }
    }
    typed.next().is_none_or(|before| !before.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(trigger: &str, expansion: &str) -> Snippet {
        Snippet {
            id: format!("snippet-{trigger}"),
            trigger: trigger.to_string(),
            expansion: expansion.to_string(),
            enabled: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn matches_trigger_case_insensitively() {
        let snippets = vec![snippet("addr", "1 Long Street")];

        assert_eq!(apply_snippets("addr", &snippets), "1 Long Street");
        assert_eq!(apply_snippets("ADDR", &snippets), "1 Long Street");
        assert_eq!(apply_snippets("Addr", &snippets), "1 Long Street");
    }

    #[test]
    fn expansion_case_is_preserved_verbatim() {
        let snippets = vec![snippet("sig", "Best,\nAktan")];

        assert_eq!(apply_snippets("SIG", &snippets), "Best,\nAktan");
    }

    #[test]
    fn only_whole_words_are_replaced() {
        let snippets = vec![snippet("addr", "1 Long Street")];

        assert_eq!(apply_snippets("addressed", &snippets), "addressed");
        assert_eq!(apply_snippets("xaddr", &snippets), "xaddr");
        assert_eq!(apply_snippets("addr_x", &snippets), "addr_x");
        assert_eq!(
            apply_snippets("send addr, please", &snippets),
            "send 1 Long Street, please"
        );
    }

    #[test]
    fn multi_word_triggers_match_as_phrases() {
        let snippets = vec![snippet("my address", "1 Long Street")];

        assert_eq!(
            apply_snippets("My Address is here", &snippets),
            "1 Long Street is here"
        );
        assert_eq!(apply_snippets("my addresses", &snippets), "my addresses");
    }

    #[test]
    fn longest_trigger_wins_at_the_same_position() {
        let snippets = vec![
            snippet("ship", "shipping"),
            snippet("ship it", "ship it now"),
        ];

        assert_eq!(apply_snippets("ship it", &snippets), "ship it now");
        assert_eq!(apply_snippets("ship", &snippets), "shipping");
    }

    #[test]
    fn disabled_snippets_are_skipped() {
        let mut disabled = snippet("addr", "1 Long Street");
        disabled.enabled = false;

        assert_eq!(apply_snippets("addr", &[disabled]), "addr");
    }

    #[test]
    fn blank_snippets_are_skipped() {
        let empty_expansion = snippet("addr", "");
        let empty_trigger = snippet("", "1 Long Street");

        assert_eq!(
            apply_snippets("addr here", &[empty_expansion, empty_trigger]),
            "addr here"
        );
    }

    #[test]
    fn every_occurrence_is_replaced() {
        let snippets = vec![snippet("addr", "1 Long Street")];

        assert_eq!(
            apply_snippets("addr and addr and ADDR", &snippets),
            "1 Long Street and 1 Long Street and 1 Long Street"
        );
    }

    #[test]
    fn expansions_are_never_rescanned() {
        let snippets = vec![snippet("a", "a b"), snippet("b", "loop")];

        assert_eq!(apply_snippets("a", &snippets), "a b");
    }

    #[test]
    fn trigger_text_is_never_read_as_a_pattern() {
        let snippets = vec![snippet("c++", "C plus plus")];

        assert_eq!(apply_snippets("c++", &snippets), "C plus plus");
        assert_eq!(apply_snippets("cxx", &snippets), "cxx");
    }

    #[test]
    fn non_ascii_triggers_fold_case_and_keep_boundaries() {
        let snippets = vec![snippet("straße", "Strasse")];

        assert_eq!(apply_snippets("STRASSE", &snippets), "STRASSE");
        assert_eq!(apply_snippets("Straße hier", &snippets), "Strasse hier");
        assert_eq!(apply_snippets("Straßeneck", &snippets), "Straßeneck");
    }

    #[test]
    fn text_without_matches_is_returned_unchanged() {
        let snippets = vec![snippet("addr", "1 Long Street")];

        assert_eq!(apply_snippets("nothing here", &snippets), "nothing here");
        assert_eq!(apply_snippets("", &snippets), "");
    }

    #[test]
    fn snippets_round_trip_through_serde() {
        let snippets = vec![snippet("addr", "1 Long Street")];
        let encoded = serde_json::to_value(&snippets).expect("snippets serialize");

        assert_eq!(
            encoded,
            serde_json::json!([{
                "id": "snippet-addr",
                "trigger": "addr",
                "expansion": "1 Long Street",
                "enabled": true,
                "created_at": 0,
                "updated_at": 0,
            }])
        );
        assert_eq!(
            serde_json::from_value::<Vec<Snippet>>(encoded).expect("snippets deserialize"),
            snippets
        );
    }

    #[test]
    fn placeholders_fill_and_the_cursor_lands_where_it_was_written() {
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 11, 20, 21, 5, 0)
            .unwrap();
        let filled = fill_placeholders(
            "Hi {cursor}, re {clipboard} on {date} at {time} {x}",
            &now,
            || Some("PR 12".to_string()),
        );
        assert_eq!(
            filled.text,
            "Hi , re PR 12 on November 20, 2026 at 9:05 PM {x}"
        );
        assert_eq!(filled.caret_back, filled.text.chars().count() - 3);

        let plain = fill_placeholders("Best, Aktan", &now, || panic!("clipboard read"));
        assert_eq!(plain.caret_back, 0);
    }

    fn typed(triggers: &mut TypedTriggers, text: &str, snippets: &[Snippet]) -> Option<TypedExpansion> {
        text.chars()
            .map(|character| triggers.key(Keystroke::Text(character), snippets))
            .last()
            .flatten()
    }

    #[test]
    fn a_typed_trigger_expands_on_the_space_after_it_and_only_as_a_word() {
        let snippets = vec![snippet("sig", "Best,\nAktan"), snippet("sig2", "Aktan")];
        let mut triggers = TypedTriggers::default();
        assert_eq!(
            typed(&mut triggers, "thanks SIG ", &snippets),
            Some(TypedExpansion {
                erase: 4,
                expansion: "Best,\nAktan".to_string()
            })
        );
        assert_eq!(typed(&mut triggers, "unsig ", &snippets), None);

        typed(&mut triggers, "sih", &snippets);
        triggers.key(Keystroke::Erase, &snippets);
        assert_eq!(typed(&mut triggers, "g2 ", &snippets).map(|found| found.erase), Some(5));

        typed(&mut triggers, "si", &snippets);
        triggers.key(Keystroke::Reset, &snippets);
        assert_eq!(typed(&mut triggers, "g ", &snippets), None);
    }
}
