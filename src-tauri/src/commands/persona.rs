//! Writing-sample personalization.
//!
//! Samples are the user's own prose, injected into every rewrite prompt as
//! voice-matching examples (see [`crate::prompt_renderer`]). They are stored
//! globally rather than per mode: a person has one writing voice, and copying
//! it into each mode would create several sources of truth for the same fact.

use crate::settings::{self, PersonaSample, PERSONA_SAMPLES_MAX};
use tauri::AppHandle;

/// Drops blank rows and truncates each sample to the word ceiling, then caps
/// the list.
///
/// The renderer applies the same bounds because it is the layer that decides
/// what leaves the machine; enforcing them here too keeps the persisted file
/// honest, so what the settings screen shows is what a prompt would carry.
fn normalize_persona_samples(samples: Vec<PersonaSample>) -> Vec<PersonaSample> {
    samples
        .iter()
        .filter_map(PersonaSample::normalized)
        .take(PERSONA_SAMPLES_MAX)
        .collect()
}

#[tauri::command]
#[specta::specta]
pub fn get_persona_samples(app: AppHandle) -> Vec<PersonaSample> {
    settings::get_settings(&app).persona_samples
}

#[tauri::command]
#[specta::specta]
pub fn save_persona_samples(
    app: AppHandle,
    samples: Vec<PersonaSample>,
) -> Result<Vec<PersonaSample>, String> {
    let normalized = normalize_persona_samples(samples);
    settings::update_settings(&app, |settings| {
        settings.persona_samples = normalized.clone();
    })?;
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str, text: &str) -> PersonaSample {
        PersonaSample {
            id: id.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn blank_samples_are_not_persisted() {
        let normalized =
            normalize_persona_samples(vec![sample("a", "  \n "), sample("b", "Real voice.")]);
        assert_eq!(normalized, vec![sample("b", "Real voice.")]);
    }

    #[test]
    fn the_list_is_capped_and_each_sample_is_truncated_on_a_word_boundary() {
        let long = (0..600)
            .map(|index| format!("w{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let samples: Vec<PersonaSample> = (0..9)
            .map(|index| sample(&format!("s{index}"), &long))
            .collect();

        let normalized = normalize_persona_samples(samples);

        assert_eq!(normalized.len(), PERSONA_SAMPLES_MAX);
        let words: Vec<&str> = normalized[0].text.split_whitespace().collect();
        assert_eq!(words.len(), 500);
        assert_eq!(words[499], "w499");
    }

    #[test]
    fn saving_an_empty_list_is_how_personalization_is_turned_off() {
        assert!(normalize_persona_samples(Vec::new()).is_empty());
    }
}
