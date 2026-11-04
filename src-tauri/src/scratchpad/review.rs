use super::links::{
    describe, resolve, resolve_note, unique_targets, ScratchSource, ScratchSourceKind,
    ScratchSourceRef,
};
use super::synthesis::{require_idle, ScratchReviewSource};
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::{MeetingListFilter, MeetingTimeWindow};
use chrono::{Days, Local, TimeZone};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, State};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ScratchReviewPeriod {
    Day,
    Week,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ScratchReviewCandidates {
    pub sources: Vec<ScratchSource>,
    pub has_more: bool,
    pub conversations_unavailable: bool,
}

#[tauri::command]
#[specta::specta]
pub async fn scratchpad_review_candidates(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    period: ScratchReviewPeriod,
) -> Result<ScratchReviewCandidates, String> {
    require_idle(&app)?;
    let today = Local::now().date_naive();
    let date = match period {
        ScratchReviewPeriod::Day => Some(today),
        ScratchReviewPeriod::Week => today.checked_sub_days(Days::new(6)),
    }
    .ok_or_else(|| "The review dates could not be read.".to_string())?;
    let start = date
        .and_hms_opt(0, 0, 0)
        .and_then(|date| Local.from_local_datetime(&date).earliest())
        .ok_or_else(|| "The review dates could not be read.".to_string())?
        .timestamp_millis();
    let mut sources = history.with_connection(|conn| {
        let mut statement = conn.prepare("SELECT id, title, updated_at_ms FROM scratch_notes WHERE updated_at_ms >= ?1 ORDER BY updated_at_ms DESC, id LIMIT 101")?;
        let rows = statement.query_map(params![start], |row| {
            let id: String = row.get(0)?;
            Ok(ScratchSource { kind: ScratchSourceKind::Note, link: format!("sona://note/{id}"), id, title: row.get(1)?, revision: Some(row.get(2)?), available: true })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }).map_err(|_| "The review could not be read.".to_string())?;
    let mut has_more = sources.len() > 100;
    sources.truncate(100);
    let store = meetings.store().await.ok();
    if let Some(store) = &store {
        let filter = MeetingListFilter {
            window: match period {
                ScratchReviewPeriod::Day => MeetingTimeWindow::Today,
                ScratchReviewPeriod::Week => MeetingTimeWindow::Last7Days,
            },
            ..MeetingListFilter::default()
        };
        let page = store
            .list_sessions(None, 100, &filter)
            .map_err(|_| "The review could not be read.".to_string())?;
        has_more |= page.has_more;
        for entry in page.entries {
            let target = ScratchSourceRef {
                kind: ScratchSourceKind::Meeting,
                id: entry.session_id.uuid().to_string(),
            };
            if let Ok(source) = describe(&history, store, &target) {
                sources.push(source);
            }
        }
    }
    Ok(ScratchReviewCandidates {
        sources,
        has_more,
        conversations_unavailable: store.is_none(),
    })
}

#[tauri::command]
#[specta::specta]
pub async fn scratchpad_review_sources(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    sources: Vec<ScratchSourceRef>,
) -> Result<Vec<ScratchReviewSource>, String> {
    require_idle(&app)?;
    let targets = unique_targets(sources);
    if !(2..=12).contains(&targets.len())
        || targets
            .iter()
            .any(|source| source.kind == ScratchSourceKind::Person)
    {
        return Err("Select between 2 and 12 notes or conversations.".to_string());
    }
    let store = if targets
        .iter()
        .all(|target| target.kind == ScratchSourceKind::Note)
    {
        None
    } else {
        Some(
            meetings
                .store()
                .await
                .map_err(|_| "Conversations are still unlocking.".to_string())?,
        )
    };
    targets
        .iter()
        .map(|target| {
            let resolved = if target.kind == ScratchSourceKind::Note {
                resolve_note(&history, target)?
            } else {
                resolve(
                    &history,
                    store
                        .as_deref()
                        .ok_or_else(|| "Conversations are still unlocking.".to_string())?,
                    target,
                )?
            };
            let text = resolved
                .evidence
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| "One selected record has no text to review.".to_string())?;
            Ok(ScratchReviewSource {
                source: resolved.source,
                text,
            })
        })
        .collect()
}
