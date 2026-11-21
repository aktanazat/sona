//! Names of the people he meets, his project titles and the words heard
//! across his meetings, added to every dictation's vocabulary so they come
//! out spelled right.
//!
//! The list is read from the conversation store, which is encrypted, so it
//! lives only in memory and is never written to the plain settings file.

use crate::audio_toolkit::text::vocabulary_spoken_key;
use crate::managers::history::HistoryManager;
use crate::meeting::people_types::{PersonListEntry, VocabularyCandidate};
use crate::meeting::store::MeetingStore;
use crate::meeting::types::MeetingSessionId;
use crate::settings::VocabularyEntry;
use std::collections::HashSet;
use std::sync::{Arc, RwLock};

/// The most words a dictation takes from meetings. Whisper's prompt holds
/// fewer; the rest still correct near misses after transcription.
const LIMIT: usize = 60;
/// Shorter words collide with everyday ones ("Al", "Bo") and would be
/// corrected into names.
const MIN_KEY_CHARS: usize = 3;

static WORDS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// The words the next dictation adds, most important first.
pub(crate) fn current() -> Vec<String> {
    WORDS.read().map(|words| words.clone()).unwrap_or_default()
}

/// Re-reads the list when the conversation store is open; never opens it.
pub(crate) fn refresh(store: Option<Arc<MeetingStore>>, history: &HistoryManager, own: &[String]) {
    let Some(store) = store else {
        return;
    };
    match read(&store, history, own, &HashSet::new()) {
        Ok(words) => remember(words),
        Err(error) => log::debug!("Meeting vocabulary not refreshed: {error}"),
    }
}

pub(crate) fn remember(words: Vec<String>) {
    if let Ok(mut current) = WORDS.write() {
        *current = words;
    }
}

/// The list as it stands in the stores: people met most recently first,
/// then projects being worked on, then words heard in the most meetings.
/// Nothing comes from the `hidden` meetings.
pub(crate) fn read(
    store: &MeetingStore,
    history: &HistoryManager,
    own: &[String],
    hidden: &HashSet<MeetingSessionId>,
) -> Result<Vec<String>, String> {
    let people = store
        .people_list_without(hidden)
        .map_err(|error| format!("{error:?}"))?;
    let mined = store
        .vocabulary_candidates_without(own, hidden)
        .map_err(|error| format!("{error:?}"))?;
    let projects: Vec<String> = history
        .with_connection(|connection| crate::planning::store::projects_by_activity(connection))
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|project| project.title)
        .collect();
    Ok(words_from(&people.entries, &projects, &mined.entries, own))
}

/// `vocabulary` with the meeting words in front of it. The user's own entries
/// stay last, where the Whisper prompt keeps them first, and a meeting word
/// he already has is left out.
pub(crate) fn with_meeting_words(vocabulary: Vec<VocabularyEntry>) -> Vec<VocabularyEntry> {
    prepend(current(), vocabulary)
}

fn prepend(words: Vec<String>, vocabulary: Vec<VocabularyEntry>) -> Vec<VocabularyEntry> {
    let own: HashSet<String> = vocabulary
        .iter()
        .map(|entry| vocabulary_spoken_key(&entry.spoken))
        .collect();
    let mut entries: Vec<VocabularyEntry> = words
        .into_iter()
        .filter(|word| !own.contains(&vocabulary_spoken_key(word)))
        .rev()
        .map(|word| VocabularyEntry {
            spoken: word.clone(),
            written: word,
        })
        .collect();
    entries.extend(vocabulary);
    entries
}

fn words_from(
    people: &[PersonListEntry],
    projects: &[String],
    mined: &[VocabularyCandidate],
    own: &[String],
) -> Vec<String> {
    let mut met: Vec<&PersonListEntry> = people
        .iter()
        .filter(|entry| entry.meetings_count > 0)
        .collect();
    met.sort_by_key(|entry| std::cmp::Reverse(entry.last_meeting_at_utc_ms));
    let mut heard: Vec<&VocabularyCandidate> = mined.iter().collect();
    heard.sort_by_key(|candidate| std::cmp::Reverse(candidate.meetings_count));

    let mut seen: HashSet<String> = own.iter().map(|word| vocabulary_spoken_key(word)).collect();
    met.into_iter()
        .map(|entry| entry.person.display_name.trim())
        .chain(projects.iter().map(|title| title.trim()))
        .chain(heard.into_iter().map(|candidate| candidate.text.trim()))
        .filter(|word| !word.contains('@'))
        .filter(|word| {
            let key = vocabulary_spoken_key(word);
            key.chars().count() >= MIN_KEY_CHARS && seen.insert(key)
        })
        .take(LIMIT)
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::people_types::{Person, PersonId};

    fn met(name: &str, meetings: u64, last: i64) -> PersonListEntry {
        PersonListEntry {
            person: Person {
                id: PersonId::new(),
                display_name: name.to_string(),
                aliases: Vec::new(),
                calendar_emails: Vec::new(),
                organization: None,
                summary: None,
                role: None,
                notes: None,
                created_at_utc_ms: 0,
                updated_at_utc_ms: 0,
            },
            meetings_count: meetings,
            last_meeting_at_utc_ms: (meetings > 0).then_some(last),
            suggested_count: 0,
            evidence_sources: Vec::new(),
            confirmed_count: meetings,
            last_meeting: None,
        }
    }

    fn heard(text: &str, meetings: u64) -> VocabularyCandidate {
        VocabularyCandidate {
            text: text.to_string(),
            occurrences: meetings,
            meetings_count: meetings,
        }
    }

    #[test]
    fn people_met_lately_come_first_and_strangers_stay_out() {
        let people = [
            met("Nolan Reyes", 2, 100),
            met("Stephen Okafor", 5, 300),
            met("Never Met", 0, 0),
        ];
        let words = words_from(
            &people,
            &["Sona".to_string()],
            &[heard("Kubernetes", 1), heard("Tauri", 4)],
            &[],
        );
        assert_eq!(
            words,
            [
                "Stephen Okafor",
                "Nolan Reyes",
                "Sona",
                "Tauri",
                "Kubernetes"
            ]
        );
    }

    #[test]
    fn short_words_addresses_and_his_own_words_are_skipped() {
        let people = [
            met("Al", 1, 1),
            met("ann@example.com", 1, 2),
            met("sona", 1, 3),
        ];
        let words = words_from(&people, &["Sona".to_string()], &[], &["SONA".to_string()]);
        assert!(words.is_empty(), "{words:?}");
    }

    #[test]
    fn his_own_vocabulary_stays_last_where_the_prompt_keeps_it() {
        let own = vec![VocabularyEntry {
            spoken: "nolan".to_string(),
            written: "Nolan".to_string(),
        }];
        let entries = prepend(
            vec![
                "Stephen".to_string(),
                "Nolan".to_string(),
                "Tauri".to_string(),
            ],
            own,
        );
        let written: Vec<&str> = entries.iter().map(|entry| entry.written.as_str()).collect();
        assert_eq!(written, ["Tauri", "Stephen", "Nolan"]);
    }
}
