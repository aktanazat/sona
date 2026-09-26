//! Running one offered corpus change, and putting it back.
//!
//! The corpus half of what `config.rs` is for settings, and built the same
//! way: every change goes through the mutation the app already has, and the
//! inverse is captured from the same read that supplied the write's fence, so
//! Undo is the same mutation run backwards rather than a second way to reach
//! the store.
//!
//! Nothing here decides *whether* a change happens. The response validator has
//! already refused any id the turn's pack did not name, and a card is only
//! reachable by a press, so by the time a function in this file runs there is
//! a reader who asked for exactly this.
//!
//! Each apply reads before it writes, because every mutation in the meeting
//! store is fenced on a revision and the assistant has none: the pack it was
//! given carries quotes and ids, never a revision. That read is also where the
//! prior value comes from, so the fence and the undo cost one query between
//! them.

use super::protocol::SonaChatActionV1;
use crate::commands::vocabulary::{add_vocabulary_correction, VocabularyScope};
use crate::meeting::analytics::MeetingNotesTemplate;
use crate::meeting::loop_types::{
    MeetingLoopAssignRequest, MeetingLoopId, MeetingLoopMutationResult, MeetingLoopReopenRequest,
    MeetingLoopResolution, MeetingLoopResolveRequest,
};
use crate::meeting::people_types::PersonId;
use crate::meeting::series_types::MeetingSeriesTemplateSetRequest;
use crate::meeting::session::{MeetingSessionManager, MeetingSpeakerRenameRequest};
use crate::meeting::template_types::MeetingTemplateId;
use crate::meeting::types::{MeetingOperationId, MeetingSessionId, OperationReceipt, SpeakerId};
use crate::settings::VocabularyEntry;
use tauri::AppHandle;

/// How to put one applied change back.
///
/// Every variant is an argument list for a mutation this app already exposes,
/// so undoing is applying in the other direction and earns its own receipt —
/// which is the honest record: the corpus was changed twice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ActionUndo {
    ReopenLoop {
        loop_id: MeetingLoopId,
    },
    AssignLoop {
        loop_id: MeetingLoopId,
        owner_person_id: Option<PersonId>,
    },
    SeriesTemplate {
        series_key: String,
        template: Option<MeetingNotesTemplate>,
        custom_template_id: Option<MeetingTemplateId>,
    },
    /// The entry that held this spoken form before, or `None` when the term
    /// was new and putting it back means removing it.
    Vocabulary {
        spoken: String,
        prior: Option<VocabularyEntry>,
    },
    SpeakerName {
        session_id: MeetingSessionId,
        speaker_id: SpeakerId,
        display_name: String,
    },
    ConnectionSend { receipt_id: String },
    CalendarEvent { event_id: String },
    Unavailable,
}

/// What a committed action left behind.
pub(crate) struct AppliedAction {
    /// The receipt the store recorded, when the mutation records one. A
    /// vocabulary term is written to settings, which keeps no ledger.
    pub(crate) operation_id: Option<String>,
    pub(crate) undo: ActionUndo,
}

/// Why a change did not happen. One variant, because every caller does the
/// same thing with it: the card stays where it was and the sheet says the
/// change was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActionError;

type Outcome<T> = Result<T, ActionError>;

/// The receipt id a mutation recorded, as the card reports it.
fn receipt_id(receipt: &OperationReceipt) -> String {
    receipt.operation_id.uuid().to_string()
}

/// Run one card. Everything that touches the corpus goes through `manager`,
/// the handle the command resolved; `app` carries only the vocabulary term,
/// because a spelling is a setting rather than a row in the meeting store.
pub(crate) async fn apply(
    app: &AppHandle,
    manager: &MeetingSessionManager,
    action: &SonaChatActionV1,
    operation_id: &str,
) -> Result<AppliedAction, String> {
    let result = match action {
        SonaChatActionV1::ResolveLoop { loop_id, .. } => resolve_loop(manager, loop_id).await,
        SonaChatActionV1::AssignLoop {
            loop_id, person_id, ..
        } => assign_loop(manager, loop_id, Some(*person_id)).await,
        SonaChatActionV1::SetSeriesTemplate {
            series_key,
            template_id,
            ..
        } => set_series_template(manager, series_key, Some(*template_id), None).await,
        SonaChatActionV1::AddVocabularyTerm {
            term, replacement, ..
        } => add_vocabulary_term(app, term, replacement.as_deref()),
        SonaChatActionV1::RenameSpeaker {
            session_id,
            speaker_id,
            name,
            ..
        } => rename_speaker(manager, *session_id, *speaker_id, name).await,
        SonaChatActionV1::DraftEmail { recipients, subject, body, .. } => {
            let store = manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
            if !store.connection_preferences().map_err(|_| "Could not read connection permissions.")?.email_drafts_enabled {
                return Err("Enable email drafts in Settings > Connections first.".into());
            }
            let result = crate::meeting::mail_context::compose(recipients, subject, body).await;
            if result.state != crate::meeting::mail_context::MailContextState::Ready {
                return Err(result.status_text().into());
            }
            return Ok(AppliedAction { operation_id: None, undo: ActionUndo::Unavailable });
        }
        SonaChatActionV1::CreateCalendarEvent { title, start_utc_ms, end_utc_ms, notes, location, .. } => {
            let store = manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
            if !store.connection_preferences().map_err(|_| "Could not read connection permissions.")?.calendar_actions_enabled {
                return Err("Enable calendar actions in Settings > Connections first.".into());
            }
            let event_id = crate::integrations::calendar::create(title, *start_utc_ms, *end_utc_ms, notes, location).await?;
            return Ok(AppliedAction { operation_id: Some(event_id.clone()), undo: ActionUndo::CalendarEvent { event_id } });
        }
        SonaChatActionV1::PostSlack { connection_id, text, .. } => {
            let store = manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
            return applied_send(crate::integrations::send_slack(app, &store, connection_id, text, operation_id).await?);
        }
        SonaChatActionV1::SendNotes { connection_id, session_id, .. } => {
            let store = manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
            let request = crate::integrations::types::SendNotesRequest {
                connection_id: connection_id.clone(), session_id: *session_id, operation_id: operation_id.into(),
            };
            return applied_send(crate::integrations::send_notes(app, &store, &request, None).await?);
        }
    };
    result.map_err(|_| "This change could not be applied. The meeting may have changed.".into())
}

pub(crate) async fn undo(
    app: &AppHandle,
    manager: &MeetingSessionManager,
    undo: &ActionUndo,
) -> Result<(), String> {
    let result = match undo {
        ActionUndo::ReopenLoop { loop_id } => reopen_loop(manager, loop_id).await.map(drop),
        ActionUndo::AssignLoop {
            loop_id,
            owner_person_id,
        } => assign_loop(manager, loop_id, *owner_person_id)
            .await
            .map(drop),
        ActionUndo::SeriesTemplate {
            series_key,
            template,
            custom_template_id,
        } => set_series_template(manager, series_key, *template, *custom_template_id)
            .await
            .map(drop),
        ActionUndo::Vocabulary { spoken, prior } => restore_vocabulary(app, spoken, prior.as_ref()),
        ActionUndo::SpeakerName {
            session_id,
            speaker_id,
            display_name,
        } => rename_speaker(manager, *session_id, *speaker_id, display_name)
            .await
            .map(drop),
        ActionUndo::ConnectionSend { receipt_id } => {
            let store = manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
            return crate::integrations::undo_send(app, &store, receipt_id).await;
        }
        ActionUndo::CalendarEvent { event_id } => return crate::integrations::calendar::undo(event_id).await,
        ActionUndo::Unavailable => return Err("This action cannot be undone in Sona. Close the draft in Mail or remove the message at its destination.".into()),
    };
    result.map_err(|_| "This change could not be undone. The meeting may have changed.".into())
}

fn applied_send(receipt: crate::integrations::types::SendReceipt) -> Result<AppliedAction, String> {
    if receipt.state != crate::integrations::types::SendState::Sent {
        return Err(receipt.detail);
    }
    let undo = if receipt.undo.is_some() {
        ActionUndo::ConnectionSend { receipt_id: receipt.id.clone() }
    } else { ActionUndo::Unavailable };
    Ok(AppliedAction { operation_id: Some(receipt.id), undo })
}

/// The revision every write against a loop must carry, and its current owner.
/// Both come out of the id and one list read. The store fences each loop on
/// its own row revision, not on the meeting's, so a row that has been resolved
/// or reassigned before still accepts the next write; a loop whose row is not
/// in that list is a loop that no longer exists.
async fn loop_fence(
    manager: &MeetingSessionManager,
    loop_id: &MeetingLoopId,
) -> Outcome<(u64, Option<PersonId>)> {
    let session_id = loop_id.session_id().ok_or(ActionError)?;
    let loops = manager
        .loops_list(session_id)
        .await
        .map_err(|_| ActionError)?;
    let row = loops
        .rows
        .iter()
        .find(|row| &row.loop_id == loop_id)
        .ok_or(ActionError)?;
    Ok((row.revision, row.owner_person_id))
}

pub(super) async fn resolve_loop(
    manager: &MeetingSessionManager,
    loop_id: &MeetingLoopId,
) -> Outcome<AppliedAction> {
    let (expected_revision, _) = loop_fence(manager, loop_id).await?;
    let result = manager
        .loop_resolve(MeetingLoopResolveRequest {
            operation_id: MeetingOperationId::new(),
            loop_id: loop_id.clone(),
            expected_revision,
            /* Done, never Dropped. "I did it" and "this is not happening" are
             * different claims, and only the first is one an assistant reading
             * a transcript can make. */
            resolution: MeetingLoopResolution::Done,
        })
        .await
        .map_err(|_| ActionError)?;
    Ok(applied_loop(
        result,
        ActionUndo::ReopenLoop {
            loop_id: loop_id.clone(),
        },
    ))
}

pub(super) async fn reopen_loop(
    manager: &MeetingSessionManager,
    loop_id: &MeetingLoopId,
) -> Outcome<MeetingLoopMutationResult> {
    let (expected_revision, _) = loop_fence(manager, loop_id).await?;
    manager
        .loop_reopen(MeetingLoopReopenRequest {
            operation_id: MeetingOperationId::new(),
            loop_id: loop_id.clone(),
            expected_revision,
        })
        .await
        .map_err(|_| ActionError)
}

async fn assign_loop(
    manager: &MeetingSessionManager,
    loop_id: &MeetingLoopId,
    owner_person_id: Option<PersonId>,
) -> Outcome<AppliedAction> {
    let (expected_revision, prior_owner) = loop_fence(manager, loop_id).await?;
    let result = manager
        .loop_assign(MeetingLoopAssignRequest {
            operation_id: MeetingOperationId::new(),
            loop_id: loop_id.clone(),
            expected_revision,
            owner_person_id,
        })
        .await
        .map_err(|_| ActionError)?;
    Ok(applied_loop(
        result,
        ActionUndo::AssignLoop {
            loop_id: loop_id.clone(),
            owner_person_id: prior_owner,
        },
    ))
}

fn applied_loop(result: MeetingLoopMutationResult, undo: ActionUndo) -> AppliedAction {
    AppliedAction {
        operation_id: Some(receipt_id(&result.receipt)),
        undo,
    }
}

async fn set_series_template(
    manager: &MeetingSessionManager,
    series_key: &str,
    template: Option<MeetingNotesTemplate>,
    custom_template_id: Option<MeetingTemplateId>,
) -> Outcome<AppliedAction> {
    let preferences = manager
        .series_preferences(series_key.to_string())
        .await
        .map_err(|_| ActionError)?;
    // A key with no series behind it answers with every field at its default,
    // so the key itself is what says whether the series exists.
    if preferences.series_key.as_deref() != Some(series_key) {
        return Err(ActionError);
    }
    let result = manager
        .set_series_template(MeetingSeriesTemplateSetRequest {
            operation_id: MeetingOperationId::new(),
            series_key: series_key.to_string(),
            template,
            custom_template_id,
            expected_revision: preferences.revision,
        })
        .await
        .map_err(|_| ActionError)?;
    Ok(AppliedAction {
        operation_id: Some(receipt_id(&result.receipt)),
        undo: ActionUndo::SeriesTemplate {
            series_key: series_key.to_string(),
            template: preferences.template,
            custom_template_id: preferences.custom_template_id,
        },
    })
}

async fn rename_speaker(
    manager: &MeetingSessionManager,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
    display_name: &str,
) -> Outcome<AppliedAction> {
    let store = manager.store().await.map_err(|_| ActionError)?;
    let review = store.review_snapshot(session_id).map_err(|_| ActionError)?;
    let prior = review
        .speakers
        .iter()
        .find(|speaker| speaker.speaker_id == speaker_id)
        .ok_or(ActionError)?
        .display_name
        .clone();
    let result = manager
        .speaker_rename(MeetingSpeakerRenameRequest {
            operation_id: MeetingOperationId::new(),
            session_id,
            expected_revision: review.session.revision,
            speaker_id,
            display_name: display_name.to_string(),
        })
        .await
        .map_err(|_| ActionError)?;
    Ok(AppliedAction {
        operation_id: Some(receipt_id(&result.receipt)),
        undo: ActionUndo::SpeakerName {
            session_id,
            speaker_id,
            display_name: prior,
        },
    })
}

/// A term with no written form of its own is a spelling: the word as it should
/// be written is the word itself, which is how the vocabulary screen stores a
/// name Sona keeps mishearing.
fn add_vocabulary_term(
    app: &AppHandle,
    term: &str,
    replacement: Option<&str>,
) -> Outcome<AppliedAction> {
    let written = replacement.unwrap_or(term).to_string();
    let prior = held_entry(app, term);
    let saved = add_vocabulary_correction(
        app.clone(),
        term.to_string(),
        written,
        VocabularyScope::Global,
    )
    .map_err(|_| ActionError)?;
    Ok(AppliedAction {
        operation_id: None,
        undo: ActionUndo::Vocabulary {
            spoken: saved.spoken,
            prior,
        },
    })
}

/// The global entry this spoken form already had, matched the way the
/// vocabulary screen matches it, so an undo restores what an upsert replaced.
fn held_entry(app: &AppHandle, spoken: &str) -> Option<VocabularyEntry> {
    let key = crate::audio_toolkit::text::vocabulary_spoken_key(spoken);
    crate::settings::get_settings(app)
        .custom_words
        .into_iter()
        .find(|entry| crate::audio_toolkit::text::vocabulary_spoken_key(&entry.spoken) == key)
}

fn restore_vocabulary(
    app: &AppHandle,
    spoken: &str,
    prior: Option<&VocabularyEntry>,
) -> Outcome<()> {
    let key = crate::audio_toolkit::text::vocabulary_spoken_key(spoken);
    let mut entries = crate::settings::get_settings(app).custom_words;
    entries.retain(|entry| crate::audio_toolkit::text::vocabulary_spoken_key(&entry.spoken) != key);
    if let Some(prior) = prior {
        entries.push(prior.clone());
    }
    crate::commands::vocabulary::update_vocabulary_entries(
        app.clone(),
        VocabularyScope::Global,
        entries,
    )
    .map(drop)
    .map_err(|_| ActionError)
}
