use super::types::{MeetingSuggestionId, SourceAvailability};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingProvider {
    Zoom,
    GoogleMeet,
    MicrosoftTeams,
    Webex,
    SlackHuddle,
    FaceTime,
    ConfiguredApp,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MeetingEvidenceFlags {
    pub app_only: bool,
    pub ax_title: bool,
    pub ax_host: bool,
    pub ax_unavailable: bool,
}

impl MeetingEvidenceFlags {
    pub const fn app_only() -> Self {
        Self {
            app_only: true,
            ax_title: false,
            ax_host: false,
            ax_unavailable: false,
        }
    }

    pub const fn with_ax_title(mut self) -> Self {
        self.ax_title = true;
        self
    }

    pub const fn with_ax_host(mut self) -> Self {
        self.ax_host = true;
        self
    }

    pub const fn with_ax_unavailable(mut self) -> Self {
        self.ax_unavailable = true;
        self
    }
}

/// Content-free normalized detector output. The detector may inspect transient
/// platform metadata while normalizing a signal, but no title, URL, attendee,
/// calendar, or transcript data enters this type. The windows in front while a
/// meeting records are a separate record, kept behind their own setting
/// (`meeting::open_windows`), and never feed detection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSuggestionSignal {
    pub provider: MeetingProvider,
    pub app_bundle_id: String,
    pub observed_at_ns: u64,
    pub evidence_flags: MeetingEvidenceFlags,
}

pub trait MeetingSuggestionSink: Send + Sync {
    fn submit(&self, signal: MeetingSuggestionSignal);
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSuggestion {
    pub offer_id: MeetingSuggestionId,
    pub provider: MeetingProvider,
    pub app_bundle_id: String,
    pub evidence_flags: MeetingEvidenceFlags,
    pub observed_at_ns: u64,
    pub expires_at_ns: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SuggestionServiceStatus {
    pub availability: SourceAvailability,
    pub active_offers: usize,
}

#[derive(Clone)]
pub struct MeetingSuggestionService {
    inner: Arc<Mutex<SuggestionState>>,
}

struct SuggestionState {
    ttl_ns: u64,
    configured_app_bundle_ids: Vec<String>,
    offers: HashMap<(MeetingProvider, String), MeetingSuggestion>,
    availability: SourceAvailability,
}

impl MeetingSuggestionService {
    pub fn new(configured_app_bundle_ids: Vec<String>, ttl_ns: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SuggestionState {
                ttl_ns,
                configured_app_bundle_ids,
                offers: HashMap::new(),
                availability: SourceAvailability::Available,
            })),
        }
    }

    pub fn status(&self, now_ns: u64) -> SuggestionServiceStatus {
        let mut state = self.lock();
        state.purge_expired(now_ns);
        SuggestionServiceStatus {
            availability: state.availability,
            active_offers: state.offers.len(),
        }
    }

    pub fn list(&self, now_ns: u64) -> Vec<MeetingSuggestion> {
        let mut state = self.lock();
        state.purge_expired(now_ns);
        let mut offers = state.offers.values().cloned().collect::<Vec<_>>();
        offers.sort_by_key(|offer| offer.observed_at_ns);
        offers
    }

    pub fn dismiss(&self, offer_id: MeetingSuggestionId) -> bool {
        let mut state = self.lock();
        let key = state
            .offers
            .iter()
            .find_map(|(key, offer)| (offer.offer_id == offer_id).then_some(key.clone()));
        key.is_some_and(|key| state.offers.remove(&key).is_some())
    }

    /// Removes a current offer for an explicit preflight request. This is not a
    /// capture grant and has no edge to a capture source or persistence writer.
    pub fn take_for_preflight(
        &self,
        offer_id: MeetingSuggestionId,
        now_ns: u64,
    ) -> Option<MeetingSuggestion> {
        let mut state = self.lock();
        state.purge_expired(now_ns);
        let key = state
            .offers
            .iter()
            .find_map(|(key, offer)| (offer.offer_id == offer_id).then_some(key.clone()))?;
        state.offers.remove(&key)
    }

    pub fn set_availability(&self, availability: SourceAvailability) {
        self.lock().availability = availability;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SuggestionState> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl MeetingSuggestionSink for MeetingSuggestionService {
    fn submit(&self, signal: MeetingSuggestionSignal) {
        let mut state = self.lock();
        if !state.configured_app_bundle_ids.is_empty()
            && !state
                .configured_app_bundle_ids
                .iter()
                .any(|bundle_id| bundle_id == &signal.app_bundle_id)
        {
            return;
        }

        let expires_at_ns = signal.observed_at_ns.saturating_add(state.ttl_ns);
        let key = (signal.provider, signal.app_bundle_id.clone());
        state.offers.insert(
            key,
            MeetingSuggestion {
                offer_id: MeetingSuggestionId::new(),
                provider: signal.provider,
                app_bundle_id: signal.app_bundle_id,
                evidence_flags: signal.evidence_flags,
                observed_at_ns: signal.observed_at_ns,
                expires_at_ns,
            },
        );
    }
}

impl SuggestionState {
    fn purge_expired(&mut self, now_ns: u64) {
        self.offers
            .retain(|_, suggestion| suggestion.expires_at_ns > now_ns);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detector_signals_only_create_expiring_offers() {
        let service = MeetingSuggestionService::new(vec!["us.zoom.xos".to_string()], 10);
        service.submit(MeetingSuggestionSignal {
            provider: MeetingProvider::Zoom,
            app_bundle_id: "us.zoom.xos".to_string(),
            observed_at_ns: 5,
            evidence_flags: MeetingEvidenceFlags::app_only(),
        });

        assert_eq!(service.list(5).len(), 1);
        assert!(service.list(15).is_empty());
    }

    #[test]
    fn configured_allowlist_rejects_unrelated_apps() {
        let service = MeetingSuggestionService::new(vec!["us.zoom.xos".to_string()], 10);
        service.submit(MeetingSuggestionSignal {
            provider: MeetingProvider::SlackHuddle,
            app_bundle_id: "com.tinyspeck.slackmacgap".to_string(),
            observed_at_ns: 5,
            evidence_flags: MeetingEvidenceFlags::app_only(),
        });

        assert!(service.list(5).is_empty());
    }
}
