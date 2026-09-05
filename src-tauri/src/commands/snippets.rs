use crate::settings;
use crate::snippets::Snippet;
use tauri::AppHandle;
use uuid::Uuid;

fn now_utc_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn normalize_snippet(snippet: Snippet) -> Result<Snippet, String> {
    let snippet = snippet.trim_outer_whitespace();
    if !snippet.is_usable() {
        return Err("Snippets need both a trigger and an expansion".to_string());
    }
    Ok(snippet)
}

fn trigger_key(trigger: &str) -> String {
    trigger.to_lowercase()
}

/// Insert or replace one snippet, keeping triggers unique after case folding.
/// An empty id asks for a new snippet; any other id is the caller's key, so a
/// repeated call with the same id updates in place instead of duplicating.
fn upsert_snippet_into(
    snippets: &mut Vec<Snippet>,
    snippet: Snippet,
    now: i64,
) -> Result<(), String> {
    let key = trigger_key(&snippet.trigger);
    if snippets
        .iter()
        .any(|existing| existing.id != snippet.id && trigger_key(&existing.trigger) == key)
    {
        return Err(format!(
            "Another snippet already uses the trigger '{}'",
            snippet.trigger
        ));
    }

    match snippets
        .iter_mut()
        .find(|existing| existing.id == snippet.id)
    {
        Some(existing) => {
            existing.trigger = snippet.trigger;
            existing.expansion = snippet.expansion;
            existing.enabled = snippet.enabled;
            existing.updated_at = now;
        }
        None => {
            let id = if snippet.id.is_empty() {
                Uuid::new_v4().to_string()
            } else {
                snippet.id
            };
            snippets.push(Snippet {
                id,
                created_at: now,
                updated_at: now,
                trigger: snippet.trigger,
                expansion: snippet.expansion,
                enabled: snippet.enabled,
            });
        }
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn list_snippets(app: AppHandle) -> Result<Vec<Snippet>, String> {
    Ok(settings::get_settings(&app).snippets)
}

#[tauri::command]
#[specta::specta]
pub fn upsert_snippet(app: AppHandle, snippet: Snippet) -> Result<Vec<Snippet>, String> {
    let snippet = normalize_snippet(snippet)?;
    let now = now_utc_ms();
    settings::try_update_settings(&app, |settings| {
        upsert_snippet_into(&mut settings.snippets, snippet, now)?;
        Ok::<_, String>(settings.snippets.clone())
    })
}

#[tauri::command]
#[specta::specta]
pub fn delete_snippet(app: AppHandle, snippet_id: String) -> Result<Vec<Snippet>, String> {
    settings::try_update_settings(&app, |settings| {
        let before = settings.snippets.len();
        settings.snippets.retain(|snippet| snippet.id != snippet_id);
        if settings.snippets.len() == before {
            return Err(format!("Snippet with id '{snippet_id}' not found"));
        }
        Ok(settings.snippets.clone())
    })
}

/// Toggle one snippet. The master switch is `set_snippets_enabled`.
#[tauri::command]
#[specta::specta]
pub fn set_snippet_enabled(
    app: AppHandle,
    snippet_id: String,
    enabled: bool,
) -> Result<Vec<Snippet>, String> {
    let now = now_utc_ms();
    settings::try_update_settings(&app, |settings| {
        let snippet = settings
            .snippets
            .iter_mut()
            .find(|snippet| snippet.id == snippet_id)
            .ok_or_else(|| format!("Snippet with id '{snippet_id}' not found"))?;
        if snippet.enabled != enabled {
            snippet.enabled = enabled;
            snippet.updated_at = now;
        }
        Ok(settings.snippets.clone())
    })
}

/// The master switch: stops snippet expansion without editing the list.
#[tauri::command]
#[specta::specta]
pub fn set_snippets_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.snippets_enabled = enabled;
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AppSettings;

    fn snippet(id: &str, trigger: &str, expansion: &str) -> Snippet {
        Snippet {
            id: id.to_string(),
            trigger: trigger.to_string(),
            expansion: expansion.to_string(),
            enabled: true,
            created_at: 10,
            updated_at: 10,
        }
    }

    #[test]
    fn blank_snippets_are_rejected() {
        assert!(normalize_snippet(snippet("", " ", "text")).is_err());
        assert!(normalize_snippet(snippet("", "trigger", "  ")).is_err());
    }

    #[test]
    fn normalization_trims_both_sides() {
        let normalized =
            normalize_snippet(snippet("", "  addr  ", "  1 Long Street  ")).expect("usable");

        assert_eq!(normalized.trigger, "addr");
        assert_eq!(normalized.expansion, "1 Long Street");
    }

    #[test]
    fn empty_id_creates_a_snippet_with_timestamps() {
        let mut snippets = Vec::new();

        upsert_snippet_into(&mut snippets, snippet("", "addr", "1 Long Street"), 99)
            .expect("create succeeds");

        assert_eq!(snippets.len(), 1);
        assert!(!snippets[0].id.is_empty());
        assert_eq!(snippets[0].created_at, 99);
        assert_eq!(snippets[0].updated_at, 99);
    }

    #[test]
    fn known_id_updates_in_place_and_keeps_created_at() {
        let mut snippets = vec![snippet("one", "addr", "1 Long Street")];

        upsert_snippet_into(&mut snippets, snippet("one", "addr", "2 Short Street"), 99)
            .expect("update succeeds");

        assert_eq!(snippets.len(), 1);
        assert_eq!(snippets[0].expansion, "2 Short Street");
        assert_eq!(snippets[0].created_at, 10);
        assert_eq!(snippets[0].updated_at, 99);
    }

    #[test]
    fn an_unknown_id_is_inserted_under_that_id() {
        let mut snippets = vec![snippet("one", "addr", "1 Long Street")];

        upsert_snippet_into(&mut snippets, snippet("two", "sig", "Best, Aktan"), 99)
            .expect("insert succeeds");
        upsert_snippet_into(&mut snippets, snippet("two", "sig", "Regards, Aktan"), 120)
            .expect("repeat updates");

        assert_eq!(snippets.len(), 2);
        assert_eq!(snippets[1].id, "two");
        assert_eq!(snippets[1].expansion, "Regards, Aktan");
        assert_eq!(snippets[1].created_at, 99);
        assert_eq!(snippets[1].updated_at, 120);
    }

    #[test]
    fn duplicate_triggers_are_rejected_case_insensitively() {
        let mut snippets = vec![snippet("one", "addr", "1 Long Street")];

        let error = upsert_snippet_into(&mut snippets, snippet("", "ADDR", "other"), 99)
            .expect_err("duplicate trigger");

        assert!(error.contains("ADDR"));
        assert_eq!(snippets.len(), 1);
    }

    #[test]
    fn settings_round_trip_keeps_snippets_and_the_master_switch() {
        let settings = AppSettings {
            snippets: vec![Snippet {
                enabled: false,
                ..snippet("one", "addr", "1 Long Street")
            }],
            snippets_enabled: false,
            ..Default::default()
        };

        let stored = serde_json::to_value(&settings).expect("settings serialize");
        assert_eq!(stored["snippets"][0]["trigger"], "addr");
        assert_eq!(stored["snippets"][0]["enabled"], false);
        assert_eq!(stored["snippets_enabled"], false);

        let restored: AppSettings = serde_json::from_value(stored).expect("settings deserialize");
        assert_eq!(restored.snippets, settings.snippets);
        assert!(!restored.snippets_enabled);
    }

    #[test]
    fn stores_without_snippets_default_to_an_enabled_empty_list() {
        let restored: AppSettings =
            serde_json::from_value(serde_json::json!({})).expect("empty store deserialize");

        assert!(restored.snippets.is_empty());
        assert!(restored.snippets_enabled);
    }
}
