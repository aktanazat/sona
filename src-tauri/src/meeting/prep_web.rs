//! Opt-in Brave Search context. Only calendar attendee names and companies
//! cross this boundary; meeting notes, transcripts and email bodies never do.
//! API: https://api-dashboard.search.brave.com/api-reference/web/search/get
use super::detection::machine::{CalendarAttendee, ParticipationStatus};
use super::session::MeetingSessionManager;
use super::store::MeetingStore;
use crate::secrets::{SecretAccount, SecretManager, SecretResolveError, SecretState};
use reqwest::{header::HeaderValue, Client};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tauri::{AppHandle, State};
use zeroize::Zeroizing;

const ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const MAX_PEOPLE: usize = 6;
const RESULTS_PER_PERSON: usize = 3;
const MAX_RESPONSE_BYTES: usize = 1_048_576;
const MISSING_KEY: &str = "Configure Brave Search in Meeting settings: save an API key with Web Search access, then test the connection.";

static CLIENT: LazyLock<Result<Client, reqwest::Error>> = LazyLock::new(|| {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
});

#[derive(Debug, Deserialize)]
pub(crate) struct WebResult {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(rename = "type")]
    kind: String,
    web: Option<WebResults>,
}

#[derive(Deserialize)]
struct WebResults {
    results: Vec<WebResult>,
}

#[derive(Serialize)]
struct SearchRequest<'a> {
    q: &'a str,
    count: u8,
    result_filter: &'static str,
    text_decorations: bool,
    spellcheck: bool,
    operators: bool,
}

impl<'a> SearchRequest<'a> {
    fn new(query: &'a str) -> Self {
        Self {
            q: query,
            count: 3,
            result_filter: "web",
            text_decorations: false,
            spellcheck: false,
            operators: false,
        }
    }
}

pub(crate) struct WebResearch {
    pub results: Vec<WebResult>,
    pub status: String,
}

impl WebResearch {
    fn without_results(status: impl Into<String>) -> Self {
        Self {
            results: Vec::new(),
            status: status.into(),
        }
    }
}

/// Calendar names sometimes contain an address instead of a display name.
/// Refuse those rather than leaking an address as a search term. Bounds keep
/// both phrases together below Brave's 600-character / 75-word query limit.
fn phrase(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.contains('@')
        || value.contains("://")
        || value.chars().any(char::is_control)
        || value.chars().count() > 120
        || value.split_whitespace().count() > 30
    {
        return None;
    }
    Some(value.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn attendee_query(attendee: &CalendarAttendee, company: Option<&str>) -> Option<String> {
    if attendee.is_self || attendee.status == ParticipationStatus::Declined {
        return None;
    }
    let name = phrase(&attendee.name)?;
    match company.and_then(phrase) {
        Some(company) => Some(format!("{name} {company}")),
        None if name.split_whitespace().count() >= 2 => Some(name),
        None => None,
    }
}

fn queries(store: &MeetingStore, attendees: &[CalendarAttendee]) -> Result<Vec<String>, String> {
    let emails = super::follow_up::recipient_addresses(attendees);
    let matched = store
        .person_ids_for_calendar_emails(&emails)
        .map_err(|_| "Attendee companies could not be read. Refresh the brief to try again.")?;
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for attendee in attendees
        .iter()
        .filter(|attendee| !attendee.is_self && attendee.status != ParticipationStatus::Declined)
    {
        let email = attendee
            .email
            .as_deref()
            .map(|value| value.trim().to_lowercase());
        let company = if let Some(person_id) = email.as_ref().and_then(|email| matched.get(email)) {
            store
                .person_detail(*person_id)
                .map_err(|_| {
                    "An attendee's company could not be read. Refresh the brief to try again."
                })?
                .detail
                .person
                .organization
        } else {
            None
        };
        let company = company.or_else(|| {
            email
                .as_deref()
                .and_then(MeetingStore::organization_from_email)
        });
        if let Some(query) = attendee_query(attendee, company.as_deref()) {
            if seen.insert(query.to_lowercase()) {
                result.push(query);
            }
        }
    }
    Ok(result)
}

fn parse_results(bytes: &[u8]) -> Result<Vec<WebResult>, String> {
    let response: SearchResponse = serde_json::from_slice(bytes)
        .map_err(|_| "Brave Search returned an unreadable response.")?;
    if response.kind != "search" {
        return Err("Brave Search returned an unexpected response.".into());
    }
    let Some(web) = response.web else {
        return Ok(Vec::new());
    };
    Ok(web
        .results
        .into_iter()
        .filter(|result| {
            if result.title.trim().is_empty() {
                return false;
            }
            let Ok(url) = url::Url::parse(&result.url) else {
                return false;
            };
            matches!(url.scheme(), "https" | "http")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
        })
        .take(RESULTS_PER_PERSON)
        .map(|result| WebResult {
            title: result.title.chars().take(240).collect(),
            url: result.url,
            description: result.description.chars().take(1_200).collect(),
        })
        .collect())
}

fn http_error(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 | 403 => "Brave Search refused the key. Check that it has Web Search access, then replace or test it in Meeting settings.".into(),
        402 | 429 => "Brave Search reached its request or credit limit. Check your Brave plan, then refresh the brief later.".into(),
        _ => format!("Brave Search returned HTTP {}. Try again later.", status.as_u16()),
    }
}

async fn search(client: &Client, token: &str, query: &str) -> Result<Vec<WebResult>, String> {
    let mut key = HeaderValue::from_str(token)
        .map_err(|_| "The saved Brave key is invalid. Replace it in Meeting settings.")?;
    key.set_sensitive(true);
    let mut response = client
        .get(ENDPOINT)
        .header("X-Subscription-Token", key)
        .header(reqwest::header::ACCEPT, "application/json")
        .query(&SearchRequest::new(query))
        .send()
        .await
        .map_err(|_| "Brave Search could not be reached. Check your connection and try again.")?;
    if !response.status().is_success() {
        return Err(http_error(response.status()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "The Brave Search response was interrupted.")?
    {
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("The Brave Search response was too large.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    parse_results(&bytes)
}

fn credential_error(error: SecretResolveError) -> String {
    match error {
        SecretResolveError::NotFound => MISSING_KEY.into(),
        SecretResolveError::Store(_) => {
            "Keychain could not read the Brave key. Unlock it and try again in Meeting settings."
                .into()
        }
    }
}

pub(crate) async fn research(
    app: Option<&AppHandle>,
    secrets: &SecretManager,
    store: &MeetingStore,
    attendees: &[CalendarAttendee],
) -> WebResearch {
    let Some(app) = app else {
        return WebResearch::without_results("Web research is off.");
    };
    if !crate::settings::get_settings(app)
        .meeting_prep
        .web_research_enabled
    {
        return WebResearch::without_results(
            "Web research is off. You can enable Brave Search in Meeting settings.",
        );
    }
    let token = match secrets.resolve(SecretAccount::meeting_brave_search()).await {
        Ok(token) => token,
        Err(error) => return WebResearch::without_results(credential_error(error)),
    };
    let queries = match queries(store, attendees) {
        Ok(queries) => queries,
        Err(error) => return WebResearch::without_results(error),
    };
    if queries.is_empty() {
        return WebResearch::without_results("No attendee has enough name or company information for web research. No search was sent.");
    }
    let Ok(client) = CLIENT.as_ref() else {
        return WebResearch::without_results(
            "A secure connection to Brave Search could not be prepared.",
        );
    };
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    for (index, query) in queries.iter().take(MAX_PEOPLE).enumerate() {
        // One query per second also works with Brave's entry-level rate limit.
        if index > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if !crate::settings::get_settings(app)
            .meeting_prep
            .web_research_enabled
        {
            return WebResearch::without_results(
                "Web research was turned off. Its results were left out.",
            );
        }
        match search(client, token.expose(), query).await {
            Ok(found) => {
                for result in found {
                    if seen.insert(result.url.clone()) {
                        results.push(result);
                    }
                }
            }
            Err(error) => {
                return WebResearch {
                    status: if results.is_empty() {
                        error
                    } else {
                        format!("Some searches could not finish. {error} The completed results are below.")
                    },
                    results,
                }
            }
        }
    }
    let mut status = if results.is_empty() {
        "Brave Search returned no usable results for these attendees.".to_string()
    } else {
        "External results from Brave Search. Names can match different people; check each source before relying on it.".to_string()
    };
    if queries.len() > MAX_PEOPLE {
        status.push_str(" Research is limited to the first six eligible attendees per brief.");
    }
    WebResearch { results, status }
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_web_research_connection(
    secrets: State<'_, Arc<SecretManager>>,
) -> Result<SecretState, String> {
    Ok(secrets
        .state(SecretAccount::meeting_brave_search(), None)
        .await)
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_web_research_key_set(
    secrets: State<'_, Arc<SecretManager>>,
    manager: State<'_, Arc<MeetingSessionManager>>,
    token: String,
) -> Result<SecretState, String> {
    let token = Zeroizing::new(token);
    if token.is_empty() || token.len() > 4_096 || !token.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err("Paste the Brave Search API key without spaces or line breaks.".into());
    }
    let _turn = super::prep::brief_gate().lock().await;
    let store = manager
        .store()
        .await
        .map_err(|_| "The meeting library is unavailable.")?;
    let state = secrets
        .replace(SecretAccount::meeting_brave_search(), token)
        .await
        .map_err(|_| {
            "The Brave key could not be saved in Keychain. Unlock Keychain and try again."
        })?;
    store.clear_briefs().map_err(|_| {
        "The key was saved, but saved briefs could not be cleared. Refresh the brief."
    })?;
    Ok(state)
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_web_research_key_remove(
    secrets: State<'_, Arc<SecretManager>>,
    manager: State<'_, Arc<MeetingSessionManager>>,
) -> Result<SecretState, String> {
    let _turn = super::prep::brief_gate().lock().await;
    let store = manager
        .store()
        .await
        .map_err(|_| "The meeting library is unavailable.")?;
    let state = secrets
        .remove(SecretAccount::meeting_brave_search())
        .await
        .map_err(|_| {
            "The Brave key could not be removed from Keychain. Unlock it and try again."
        })?;
    store.clear_briefs().map_err(|_| {
        "The key was removed, but saved briefs could not be cleared. Refresh the brief."
    })?;
    Ok(state)
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_web_research_test(
    secrets: State<'_, Arc<SecretManager>>,
) -> Result<String, String> {
    let token = secrets
        .resolve(SecretAccount::meeting_brave_search())
        .await
        .map_err(credential_error)?;
    let client = CLIENT
        .as_ref()
        .map_err(|_| "A secure connection to Brave Search could not be prepared.")?;
    // Explicitly testing the service sends only this fixed public query.
    search(client, token.expose(), "Brave Search").await?;
    Ok("Brave Search accepted the saved key. Enable web research and save your preparation settings to use it in briefs.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attendee(name: &str) -> CalendarAttendee {
        CalendarAttendee {
            name: name.into(),
            email: Some("private-address@acme.com".into()),
            status: ParticipationStatus::Accepted,
            is_self: false,
        }
    }

    #[test]
    fn query_carries_only_name_and_company_not_the_attendees_address() {
        let person = attendee("Dana Lee");
        let query = attendee_query(&person, Some("Acme")).unwrap();
        assert_eq!(query, "Dana Lee Acme");
        let request = serde_json::to_value(SearchRequest::new(&query)).unwrap();
        assert_eq!(
            request,
            serde_json::json!({"q":"Dana Lee Acme","count":3,"result_filter":"web","text_decorations":false,"spellcheck":false,"operators":false})
        );
        assert!(attendee_query(&attendee("private-address@acme.com"), Some("Acme")).is_none());
    }

    #[test]
    fn self_declined_and_ambiguous_first_names_are_not_searched() {
        let mut person = attendee("Dana Lee");
        person.is_self = true;
        assert!(attendee_query(&person, Some("Acme")).is_none());
        person.is_self = false;
        person.status = ParticipationStatus::Declined;
        assert!(attendee_query(&person, Some("Acme")).is_none());
        assert!(attendee_query(&attendee("Dana"), None).is_none());
        assert_eq!(
            attendee_query(&attendee("Dana Lee"), None),
            Some("Dana Lee".into())
        );
    }

    #[test]
    fn valid_no_results_is_distinct_from_a_bad_provider_payload() {
        assert!(parse_results(br#"{"type":"search","web":{"results":[]}}"#)
            .unwrap()
            .is_empty());
        assert!(
            parse_results(br#"{"type":"search","query":{"original":"Dana Lee"}}"#)
                .unwrap()
                .is_empty()
        );
        assert!(parse_results(br#"{"type":"error"}"#).is_err());
        assert!(parse_results(br#"{}"#).is_err());
    }

    #[test]
    fn external_sources_keep_real_titles_urls_and_snippets_and_drop_executable_links() {
        let results = parse_results(br#"{"type":"search","web":{"results":[{"title":"Dana at Acme","url":"https://acme.example/team/dana","description":"Dana leads engineering."},{"title":"Unsafe","url":"javascript:alert(1)","description":"Do not open"}]}}"#).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Dana at Acme");
        assert_eq!(results[0].url, "https://acme.example/team/dana");
        assert_eq!(results[0].description, "Dana leads engineering.");
    }
}
