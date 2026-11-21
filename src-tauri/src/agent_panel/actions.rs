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
use super::wire::{AgentPanelActionContextV1, AgentPanelLongValueV1};
use crate::commands::vocabulary::{add_vocabulary_correction, VocabularyScope};
use crate::managers::history::HistoryManager;
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
    ConnectionSend {
        receipt_id: String,
    },
    CalendarEvent {
        event_id: String,
    },
    RemoveCreatedTask {
        task_id: String,
    },
    RestoreTask {
        task_id: String,
        prior: Box<crate::planning::work::chat::TaskFields>,
        written: Box<crate::planning::work::chat::TaskFields>,
    },
    RestoreProject {
        project_id: String,
        prior: Option<crate::planning::work::chat::ProjectFields>,
        written: crate::planning::work::chat::ProjectFields,
    },
    RestoreNote {
        note_id: String,
        body: String,
        written: String,
    },
    Unavailable,
}

/// What a committed action left behind.
pub(crate) struct AppliedAction {
    /// The receipt the store recorded, when the mutation records one. A
    /// vocabulary term is written to settings, which keeps no ledger.
    pub(crate) operation_id: Option<String>,
    pub(crate) undo: ActionUndo,
    /// A line the card shows under it: an outside tool's answer, or why Undo
    /// was refused.
    pub(crate) detail: Option<String>,
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

/// A string argument longer than this many characters is flagged on the
/// card: longer than a search query, so it may carry text the owner did not
/// mean to send.
const LONG_ARGUMENT_CHARS: usize = 120;

/// What a card acts on, read when it arrives, so the card names it. `None`
/// for a kind whose fields already say everything, or when the row is gone.
pub(crate) fn context(
    history: Option<&HistoryManager>,
    action: &SonaChatActionV1,
) -> Option<AgentPanelActionContextV1> {
    if let SonaChatActionV1::CallOutsideTool { arguments, .. } = action {
        return outside_call(arguments);
    }
    history?
        .with_connection(|conn| context_in(conn, action))
        .ok()
        .flatten()
}

fn context_in(
    conn: &rusqlite::Connection,
    action: &SonaChatActionV1,
) -> anyhow::Result<Option<AgentPanelActionContextV1>> {
    use crate::planning::work::chat::{note_named, project_named, task_named};
    let project = |id: &str| -> anyhow::Result<Option<AgentPanelActionContextV1>> {
        Ok(project_named(conn, id)?.map(|name| AgentPanelActionContextV1::Project { name }))
    };
    match action {
        SonaChatActionV1::UpdateTask {
            task_id,
            project_id,
            ..
        } => {
            let Some((handle, title, current)) = task_named(conn, task_id)? else {
                return Ok(None);
            };
            let to_project = match project_id.as_deref() {
                Some(id) if id != "none" => project_named(conn, id)?,
                _ => None,
            };
            Ok(Some(AgentPanelActionContextV1::Task {
                handle,
                title,
                project: current,
                to_project,
            }))
        }
        SonaChatActionV1::CreateTask {
            project_id: Some(id),
            ..
        }
        | SonaChatActionV1::SaveProject {
            project_id: Some(id),
            ..
        } => project(id),
        SonaChatActionV1::EditNote { note_id, .. } => {
            Ok(note_named(conn, note_id)?.map(|title| AgentPanelActionContextV1::Note { title }))
        }
        _ => Ok(None),
    }
}

/// An outside call's arguments as the owner should read them: escapes
/// decoded, one field per line, and every long string named.
fn outside_call(arguments: &str) -> Option<AgentPanelActionContextV1> {
    fn long_values(value: &serde_json::Value, path: &str, out: &mut Vec<AgentPanelLongValueV1>) {
        match value {
            serde_json::Value::String(text) => {
                let chars = text.chars().count();
                if chars > LONG_ARGUMENT_CHARS {
                    out.push(AgentPanelLongValueV1 {
                        path: path.to_owned(),
                        chars: u32::try_from(chars).unwrap_or(u32::MAX),
                    });
                }
            }
            serde_json::Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    long_values(item, &format!("{path}[{index}]"), out);
                }
            }
            serde_json::Value::Object(fields) => {
                for (key, item) in fields {
                    let path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    long_values(item, &path, out);
                }
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
    }
    let value = serde_json::from_str::<serde_json::Value>(arguments).ok()?;
    let mut long = Vec::new();
    long_values(&value, "", &mut long);
    Some(AgentPanelActionContextV1::OutsideCall {
        arguments: serde_json::to_string_pretty(&value).ok()?,
        long_values: long,
    })
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
        SonaChatActionV1::DraftEmail {
            recipients,
            subject,
            body,
            ..
        } => {
            let store = manager
                .store()
                .await
                .map_err(|_| "The meeting library is unavailable.")?;
            if !store
                .connection_preferences()
                .map_err(|_| "Could not read connection permissions.")?
                .email_drafts_enabled
            {
                return Err("Enable email drafts in Settings > Connections first.".into());
            }
            let result = crate::meeting::mail_context::compose(recipients, subject, body).await;
            if result.state != crate::meeting::mail_context::MailContextState::Ready {
                return Err(result.status_text().into());
            }
            return Ok(AppliedAction {
                operation_id: None,
                detail: None,
                undo: ActionUndo::Unavailable,
            });
        }
        SonaChatActionV1::CreateCalendarEvent {
            title,
            start_utc_ms,
            end_utc_ms,
            notes,
            location,
            ..
        } => {
            let store = manager
                .store()
                .await
                .map_err(|_| "The meeting library is unavailable.")?;
            if !store
                .connection_preferences()
                .map_err(|_| "Could not read connection permissions.")?
                .calendar_actions_enabled
            {
                return Err("Enable calendar actions in Settings > Connections first.".into());
            }
            let event_id = crate::integrations::calendar::create(
                title,
                *start_utc_ms,
                *end_utc_ms,
                notes,
                location,
            )
            .await?;
            return Ok(AppliedAction {
                operation_id: Some(event_id.clone()),
                detail: None,
                undo: ActionUndo::CalendarEvent { event_id },
            });
        }
        SonaChatActionV1::PostSlack {
            connection_id,
            text,
            ..
        } => {
            let store = manager
                .store()
                .await
                .map_err(|_| "The meeting library is unavailable.")?;
            return applied_send(
                crate::integrations::send_slack(app, &store, connection_id, text, operation_id)
                    .await?,
            );
        }
        SonaChatActionV1::SendNotes {
            connection_id,
            session_id,
            ..
        } => {
            let store = manager
                .store()
                .await
                .map_err(|_| "The meeting library is unavailable.")?;
            let request = crate::integrations::types::SendNotesRequest {
                connection_id: connection_id.clone(),
                session_id: *session_id,
                operation_id: operation_id.into(),
            };
            return applied_send(
                crate::integrations::send_notes(app, &store, &request, None).await?,
            );
        }
        SonaChatActionV1::CreateTask {
            title,
            details,
            project_id,
            priority,
            due,
            ..
        } => {
            let due = due
                .as_deref()
                .map(|value| {
                    crate::planning::work::chat::DueEdit::named(value)
                        .ok_or_else(|| "That due day is not valid.".to_owned())
                })
                .transpose()?;
            let item = crate::planning::work::chat::create_task(
                app,
                operation_id,
                title,
                details.as_deref(),
                project_id.as_deref(),
                *priority,
                due,
            )
            .await?;
            return Ok(AppliedAction {
                operation_id: Some(operation_id.to_owned()),
                detail: None,
                undo: ActionUndo::RemoveCreatedTask { task_id: item.id },
            });
        }
        SonaChatActionV1::UpdateTask {
            task_id,
            title,
            details,
            status,
            priority,
            due,
            project_id,
            ..
        } => {
            let status = status
                .as_deref()
                .map(|value| {
                    crate::planning::work::chat::status_named(value)
                        .ok_or_else(|| "That status is not valid.".to_owned())
                })
                .transpose()?;
            let due = due
                .as_deref()
                .map(|value| {
                    crate::planning::work::chat::DueEdit::named(value)
                        .ok_or_else(|| "That due day is not valid.".to_owned())
                })
                .transpose()?;
            let project_id = project_id
                .as_deref()
                .map(|pid| (pid != "none").then(|| pid.to_owned()));
            let edit = crate::planning::work::chat::TaskEdit {
                title: title.clone(),
                details: details.clone(),
                status,
                priority: *priority,
                due,
                project_id,
            };
            let (prior, written) =
                crate::planning::work::chat::update_task(app, operation_id, task_id, &edit).await?;
            return Ok(AppliedAction {
                operation_id: Some(operation_id.to_owned()),
                undo: ActionUndo::RestoreTask {
                    task_id: task_id.clone(),
                    prior: Box::new(prior),
                    written: Box::new(written),
                },
                detail: None,
            });
        }
        SonaChatActionV1::SaveProject {
            project_id,
            name,
            description,
            ..
        } => {
            let (project, prior, written) = crate::planning::work::chat::save_project(
                app,
                operation_id,
                project_id.as_deref(),
                name,
                description.as_deref(),
            )?;
            return Ok(AppliedAction {
                operation_id: Some(operation_id.to_owned()),
                undo: ActionUndo::RestoreProject {
                    project_id: project.id,
                    prior,
                    written,
                },
                detail: None,
            });
        }
        SonaChatActionV1::EditNote { note_id, body, .. } => {
            let prior = crate::planning::work::chat::edit_note(app, note_id, body)?;
            return Ok(AppliedAction {
                operation_id: Some(operation_id.to_owned()),
                undo: ActionUndo::RestoreNote {
                    note_id: note_id.clone(),
                    body: prior,
                    written: body.clone(),
                },
                detail: None,
            });
        }
        SonaChatActionV1::CallOutsideTool {
            server,
            tool,
            arguments,
            ..
        } => {
            let arguments = serde_json::from_str(arguments)
                .map_err(|_| "The tool's arguments are not a JSON object.".to_owned())?;
            let answer =
                crate::agent_panel::mcp_servers::call_read_only(app, server, tool, arguments)
                    .await?;
            return Ok(AppliedAction {
                operation_id: None,
                undo: ActionUndo::Unavailable,
                detail: Some(answer),
            });
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
        ActionUndo::RemoveCreatedTask { task_id } => {
            let op = uuid::Uuid::new_v4().to_string();
            return crate::planning::work::chat::remove_created_task(app, &op, task_id);
        }
        ActionUndo::RestoreTask {
            task_id,
            prior,
            written,
        } => {
            let op = uuid::Uuid::new_v4().to_string();
            return crate::planning::work::chat::restore_task(app, &op, task_id, prior, written)
                .await;
        }
        ActionUndo::RestoreProject {
            project_id,
            prior,
            written,
        } => {
            let op = uuid::Uuid::new_v4().to_string();
            return crate::planning::work::chat::restore_project(
                app,
                &op,
                project_id,
                prior.as_ref(),
                written,
            );
        }
        ActionUndo::RestoreNote {
            note_id,
            body,
            written,
        } => {
            return crate::planning::work::chat::restore_note(app, note_id, body, written);
        }
        ActionUndo::Unavailable => return Err("This action cannot be undone in Sona. Close the draft in Mail or remove the message at its destination.".into()),
    };
    result.map_err(|_| "This change could not be undone. The meeting may have changed.".into())
}

fn applied_send(receipt: crate::integrations::types::SendReceipt) -> Result<AppliedAction, String> {
    if receipt.state != crate::integrations::types::SendState::Sent {
        return Err(receipt.detail);
    }
    let undo = if receipt.undo.is_some() {
        ActionUndo::ConnectionSend {
            receipt_id: receipt.id.clone(),
        }
    } else {
        ActionUndo::Unavailable
    };
    Ok(AppliedAction {
        operation_id: Some(receipt.id),
        detail: None,
        undo,
    })
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
        detail: None,
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
        detail: None,
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
        detail: None,
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
        detail: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::catalog::{save_project, ProjectEdit};
    use crate::planning::work::items;
    use crate::planning::work::types::{WorkActor, WorkOwner, WorkStatus};
    use crate::scratchpad::{self, Author};
    use rusqlite::Connection;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().expect("in-memory sqlite");
        rusqlite_migration::Migrations::new(crate::managers::history::MIGRATIONS.to_vec())
            .to_latest(&mut conn)
            .expect("history migrations");
        conn
    }

    fn project(conn: &Connection, name: &str) -> String {
        let edit = ProjectEdit {
            title: Some(name.to_owned()),
            ..ProjectEdit::default()
        };
        save_project(conn, None, &edit, 1).expect("project").id
    }

    #[test]
    fn a_task_card_names_the_task_and_both_ends_of_its_move() {
        let conn = db();
        let launch = project(&conn, "Launch");
        let hiring = project(&conn, "Hiring");
        let task = items::insert(
            &conn,
            &items::NewItem {
                title: "Send the deck",
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: Some(&launch),
                milestone_id: None,
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Me,
            1,
        )
        .expect("task");
        let moved = SonaChatActionV1::UpdateTask {
            reason: "It belongs with hiring.".to_owned(),
            task_id: task.id.clone(),
            title: None,
            details: None,
            status: None,
            priority: None,
            due: None,
            project_id: Some(hiring),
        };

        assert_eq!(
            context_in(&conn, &moved).expect("read"),
            Some(AgentPanelActionContextV1::Task {
                handle: format!("S-{}", task.number),
                title: "Send the deck".to_owned(),
                project: Some("Launch".to_owned()),
                to_project: Some("Hiring".to_owned()),
            })
        );
    }

    #[test]
    fn a_note_card_names_the_note_and_a_project_card_the_project_as_it_is_now() {
        let conn = db();
        let note =
            scratchpad::create_note_in(&conn, "Launch plan\nShip on the 27th.", Author::Me, 1)
                .expect("note");
        let launch = project(&conn, "Launch");
        let edit_note = SonaChatActionV1::EditNote {
            reason: "You asked me to add the date.".to_owned(),
            note_id: note.id.clone(),
            body: "Ship on the 28th.".to_owned(),
        };
        let rename = SonaChatActionV1::SaveProject {
            reason: "A clearer name.".to_owned(),
            project_id: Some(launch),
            name: "Launch v2".to_owned(),
            description: None,
        };

        assert_eq!(
            context_in(&conn, &edit_note).expect("read"),
            Some(AgentPanelActionContextV1::Note { title: note.title })
        );
        assert_eq!(
            context_in(&conn, &rename).expect("read"),
            Some(AgentPanelActionContextV1::Project {
                name: "Launch".to_owned()
            })
        );
    }

    #[test]
    fn an_outside_call_shows_its_arguments_decoded_and_flags_long_values() {
        let long = "x".repeat(LONG_ARGUMENT_CHARS + 1);
        let call = SonaChatActionV1::CallOutsideTool {
            reason: "Look the ticket up.".to_owned(),
            server: "Linear".to_owned(),
            tool: "search_issues".to_owned(),
            arguments: format!(
                r#"{{"query":"\u0053\u0065\u0063\u0072\u0065\u0074","filter":{{"ids":["a","{long}"]}}}}"#
            ),
        };

        let Some(AgentPanelActionContextV1::OutsideCall {
            arguments,
            long_values,
        }) = context(None, &call)
        else {
            panic!("an outside call has its arguments shown");
        };
        assert!(arguments.contains("\"query\": \"Secret\""), "{arguments}");
        assert!(!arguments.contains("\\u0053"), "{arguments}");
        assert_eq!(
            long_values,
            vec![AgentPanelLongValueV1 {
                path: "filter.ids[1]".to_owned(),
                chars: 121,
            }]
        );
    }
}
