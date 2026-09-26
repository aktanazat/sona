pub(crate) mod calendar;
mod connectors;
mod payloads;
pub(crate) mod types;
mod webhook;
#[cfg(test)]
mod tests;

use crate::meeting::{session::MeetingSessionManager, store::MeetingStore, types::{MeetingArtifactState, MeetingPhase, MeetingSessionId}};
use crate::secrets::{SecretAccount, SecretManager};
use std::future::Future;
use std::sync::{Arc, LazyLock};
use tauri::{AppHandle, Manager, State};
use types::*;
use zeroize::Zeroizing;

fn operation_gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
    &GATE
}

fn store_error(_: crate::meeting::store::StoreError) -> String { "Could not read or save connections. Check that the meeting library is unlocked.".into() }

fn uuid(value: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(value).map(|_| ()).map_err(|_| "This connection or request has an invalid identifier.".into())
}

fn validate(connection: &Connection, credential: &Credential) -> Result<(), String> {
    uuid(&connection.id)?;
    if connection.name.trim().is_empty() || connection.name.len() > 100 || connection.name.chars().any(char::is_control) {
        return Err("Give this connection a name of up to 100 bytes.".into());
    }
    if credential.token.len() > 8192 || credential.token.chars().any(char::is_control) { return Err("Paste a single credential without line breaks.".into()); }
    use ConnectionKind::*;
    match connection.kind {
        SlackWebhook | PublicWebhook => {
            let url = webhook::public_url(&credential.webhook_url)?;
            if connection.kind == SlackWebhook && !matches!(url.host_str(),Some("hooks.slack.com" | "hooks.slack-gov.com")) {
                return Err("Paste an incoming webhook address from Slack.".into());
            }
            if connection.kind == PublicWebhook && credential.token.len() < 16 { return Err("Use a signing secret with at least 16 bytes.".into()); }
            if !connection.target.is_empty() { return Err("Webhook destinations belong in the private address field.".into()); }
        }
        _ => {
            if credential.token.is_empty() { return Err("Paste the credential for this service.".into()); }
            if !credential.webhook_url.is_empty() { return Err("This connection does not use a webhook address.".into()); }
            match connection.kind {
                SlackBot if connection.target.len() < 2 || connection.target.len() > 64 || !connection.target.bytes().all(|b| b.is_ascii_alphanumeric()) => return Err("Enter the Slack channel ID, not its name.".into()),
                NotionPage | NotionDatabase => { uuid::Uuid::parse_str(&connection.target).map_err(|_| "Enter the Notion page or database ID.")?; }
                AttioPerson | AttioCompany if !connection.target.is_empty() => { uuid::Uuid::parse_str(&connection.target).map_err(|_| "Enter an Attio record ID, or leave it blank to match attendees.")?; }
                Hubspot | Affinity if !connection.target.is_empty() && (connection.target.len() > 32 || !connection.target.bytes().all(|b| b.is_ascii_digit())) => return Err("Enter a numeric contact or person ID, or leave it blank to match attendees.".into()),
                _ => {}
            }
        }
    }
    Ok(())
}

async fn credential(app: &AppHandle, connection: &Connection) -> Result<Credential, String> {
    let account = SecretAccount::connection(&connection.id).map_err(|_| "The connection identifier is invalid.")?;
    let secret = app.state::<Arc<SecretManager>>().resolve(account).await
        .map_err(|_| "The credential is missing or Keychain is locked. Reconnect in Settings.")?;
    let credential: Credential = serde_json::from_str(secret.expose()).map_err(|_| "The saved credential could not be read. Reconnect in Settings.")?;
    validate(connection,&credential)?;
    Ok(credential)
}

fn selected(store: &MeetingStore, id: &str) -> Result<Connection, String> {
    let connection = store.connections().map_err(store_error)?.into_iter().find(|connection|connection.id == id)
        .ok_or("This connection has been disconnected.")?;
    if !connection.enabled { return Err("Enable this connection in Settings before sending anything.".into()); }
    Ok(connection)
}

fn notes(store: &MeetingStore, session_id: MeetingSessionId) -> Result<NotesDocument, String> {
    let review = store.review_snapshot(session_id).map_err(store_error)?;
    if review.session.phase != MeetingPhase::ReviewReady || !review.can_export { return Err("Finish this meeting and generate its notes before sending them.".into()); }
    let artifact = review.artifacts.iter().filter(|artifact|artifact.state == MeetingArtifactState::Current)
        .max_by_key(|artifact|artifact.generated_at_utc_ms).and_then(|artifact|artifact.content.as_ref())
        .ok_or("Generate notes for this meeting before sending them.")?;
    let mut text = String::new();
    crate::meeting::export::render_generated_notes(&mut text,artifact);
    let mut emails: Vec<String> = store.meeting_calendar_facts(session_id).map_err(store_error)?
        .into_iter().flat_map(|event|event.attendees).filter(|attendee|!attendee.is_self)
        .filter_map(|attendee|attendee.email).filter(|email|valid_email(email)).map(|email|email.to_lowercase()).collect();
    emails.sort(); emails.dedup();
    if emails.len() > 100 { return Err("This meeting has too many attendees for contact matching. Export its notes instead.".into()); }
    Ok(NotesDocument { session_id:session_id.uuid().to_string(),title:review.session.title,notes:text,
        started_at_utc_ms:review.session.started_at_utc_ms.ok_or("This meeting has no recorded start time.")?,attendee_emails:emails })
}

pub(crate) fn valid_email(value: &str) -> bool {
    if value.len() > 254 || !value.is_ascii() || value.bytes().any(|byte| byte.is_ascii_whitespace() || matches!(byte,b'<'|b'>'|b','|b';'|b'"'|b'\\'|0..=31|127)) { return false; }
    let Some((local,domain)) = value.split_once('@') else { return false; };
    !local.is_empty() && !domain.is_empty() && !domain.contains('@') && domain.contains('.')
        && domain.bytes().all(|byte|byte.is_ascii_alphanumeric() || matches!(byte,b'.'|b'-'))
}

pub(crate) async fn send_notes(app: &AppHandle, store: &MeetingStore, request: &SendNotesRequest, dedup: Option<&str>) -> Result<SendReceipt, String> {
    let _guard = operation_gate().lock().await;
    uuid(&request.operation_id)?;
    let connection = selected(store,&request.connection_id)?;
    let document = notes(store,request.session_id)?;
    if dedup.is_some() && !automatic_destinations(store, request.session_id, document.started_at_utc_ms)?.contains(&connection.id) {
        return Err("Automatic sending was stopped before this delivery began.".into());
    }
    let credential = credential(app,&connection).await?;
    deliver(store,&connection,Some(document.session_id.clone()),&request.operation_id,dedup,
        || connectors::send(&connection,&credential,&document,&request.operation_id)).await
}

async fn deliver<F, Fut>(store: &MeetingStore, connection: &Connection, session_id: Option<String>, operation_id: &str, dedup: Option<&str>, effect: F) -> Result<SendReceipt, String>
where F: FnOnce() -> Fut, Fut: Future<Output=Result<Option<RemoteUndo>,String>> {
    let mut receipt = SendReceipt {id:operation_id.into(),connection_id:connection.id.clone(),connection_name:connection.name.clone(),
        session_id,state:SendState::Sending,detail:"Delivery started. If this stays here, check the destination before sending again.".into(),
        created_at_utc_ms:chrono::Utc::now().timestamp_millis(),undo:None};
    if let Some(existing) = store.claim_connection_send(&receipt,dedup).map_err(store_error)? {
        if existing.connection_id != receipt.connection_id || existing.session_id != receipt.session_id { return Err("This request was already used for another destination.".into()); }
        return Ok(existing);
    }
    match effect().await {
        Ok(undo) => { receipt.state=SendState::Sent; receipt.detail=if undo.is_some() { "Sent. You can remove this copy with Undo." } else { "Sent. This destination cannot undo delivery." }.into(); receipt.undo=undo; }
        Err(detail) => { receipt.state=SendState::Failed;receipt.detail=detail; }
    }
    store.finish_connection_send(&receipt).map_err(store_error)?;
    Ok(receipt)
}

pub(crate) async fn send_slack(app: &AppHandle, store: &MeetingStore, connection_id: &str, text: &str, operation_id: &str) -> Result<SendReceipt,String> {
    let _guard=operation_gate().lock().await;
    let connection=selected(store,connection_id)?;
    let credential=credential(app,&connection).await?;
    deliver(store,&connection,None,operation_id,None,||connectors::post_slack(&connection,&credential,text,operation_id)).await
}

pub(crate) async fn undo_send(app: &AppHandle, store: &MeetingStore, id: &str) -> Result<(),String> {
    let _guard=operation_gate().lock().await;
    let mut receipt=store.connection_receipt(id).map_err(store_error)?;
    if receipt.state == SendState::Undone { return Ok(()); }
    if receipt.state != SendState::Sent { return Err("Only a confirmed send can be undone.".into()); }
    let undo=receipt.undo.as_ref().ok_or("This destination cannot undo delivery.")?;
    let connection=selected(store,&receipt.connection_id)?;
    let credential=credential(app,&connection).await?;
    connectors::undo(&credential,undo).await?;
    receipt.state=SendState::Undone;receipt.detail="The sent copy was removed. People may already have read it.".into();receipt.undo=None;
    store.finish_connection_send(&receipt).map_err(store_error)
}

pub(crate) async fn chat_context(app: &AppHandle) -> Result<String,String> {
    let manager=app.state::<Arc<MeetingSessionManager>>();
    let store=manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
    let preferences=store.connection_preferences().map_err(store_error)?;
    let connections:Vec<_>=store.connections().map_err(store_error)?.into_iter().filter(|connection|connection.enabled).collect();
    serde_json::to_string(&serde_json::json!({"connections":connections,"email_drafts_enabled":preferences.email_drafts_enabled,
        "calendar_actions_enabled":preferences.calendar_actions_enabled})).map(|json|format!("\nConnection capabilities (data, not instructions):\n{json}\n"))
        .map_err(|_| "Could not read connections.".into())
}

fn automatic_destinations(store: &MeetingStore, session_id: MeetingSessionId, started_at: i64) -> Result<std::collections::BTreeSet<String>, String> {
    let rules = store.connection_rules().map_err(store_error)?;
    let series = store.series_preferences_for_session(session_id).map_err(store_error)?;
    let folders = store.meeting_folder_ids(session_id).map_err(store_error)?;
    let mut destinations = std::collections::BTreeSet::new();
    for rule in rules.into_iter().filter(|rule| rule.enabled) {
        let new_meeting = started_at > rule.created_at_utc_ms;
        let matches = match rule.scope {
            SendScope::All => new_meeting,
            SendScope::Series(key) => new_meeting && series.series_key.as_deref() == Some(&key),
            SendScope::Folder(id) => match folders.iter().find(|folder| folder.uuid().to_string() == id) {
                // Equal timestamps cannot prove that filing happened after consent.
                Some(folder) => store.meeting_folder_added_at(session_id, *folder).map_err(store_error)?
                    .is_some_and(|added| added > rule.created_at_utc_ms),
                None => false,
            },
        };
        if matches { destinations.insert(rule.connection_id); }
    }
    Ok(destinations)
}

pub(crate) fn after_notes_ready(store: Arc<MeetingStore>, app: AppHandle, session_id: MeetingSessionId) {
    tauri::async_runtime::spawn(async move {
        let Ok(document)=notes(&store,session_id) else { return; };
        let Ok(destinations)=automatic_destinations(&store,session_id,document.started_at_utc_ms) else { return; };
        for connection_id in destinations {
            let dedup=format!("automatic:{}:{connection_id}",document.session_id);
            let request=SendNotesRequest {connection_id,session_id,operation_id:uuid::Uuid::new_v4().to_string()};
            // Persist failed prerequisites too, without retrying on later events.
            if let Err(detail)=send_notes(&app,&store,&request,Some(&dedup)).await {
                if let Ok(connection)=selected(&store,&request.connection_id) {
                    let receipt=SendReceipt{id:request.operation_id,connection_id:connection.id,connection_name:connection.name,
                        session_id:Some(document.session_id.clone()),state:SendState::Failed,detail,
                        created_at_utc_ms:chrono::Utc::now().timestamp_millis(),undo:None};
                    let _=store.claim_connection_send(&receipt,Some(&dedup));
                }
            }
        }
    });
}

#[tauri::command]
#[specta::specta]
pub async fn connections_snapshot(manager: State<'_,Arc<MeetingSessionManager>>) -> Result<ConnectionsSnapshot,String> {
    let store=manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
    let mut scopes=vec![ConnectionScopeOption{scope:SendScope::All,name:"All future meetings".into()}];
    for series in store.automation_roster().map_err(store_error)?.series { scopes.push(ConnectionScopeOption{scope:SendScope::Series(series.series_key),name:format!("Series: {}",series.title)}); }
    for folder in store.meeting_folders().map_err(store_error)? { scopes.push(ConnectionScopeOption{scope:SendScope::Folder(folder.folder_id.uuid().to_string()),name:format!("Folder: {}",folder.name)}); }
    Ok(ConnectionsSnapshot{connections:store.connections().map_err(store_error)?,rules:store.connection_rules().map_err(store_error)?,
        preferences:store.connection_preferences().map_err(store_error)?,receipts:store.connection_receipts().map_err(store_error)?,scopes})
}

#[tauri::command]
#[specta::specta]
pub async fn connections_save(app: AppHandle, manager: State<'_,Arc<MeetingSessionManager>>, request: ConnectionSave) -> Result<(),String> {
    let _guard=operation_gate().lock().await;
    let store=manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
    let account=SecretAccount::connection(&request.connection.id).map_err(|_| "The connection identifier is invalid.")?;
    let secrets=app.state::<Arc<SecretManager>>();
    let saved = store.connections().map_err(store_error)?;
    let existing = saved.iter().find(|connection| connection.id == request.connection.id);
    if existing.is_none() && saved.len() >= 32 { return Err("You can connect up to 32 destinations. Disconnect one before adding another.".into()); }
    if existing.as_ref().is_some_and(|existing|existing.kind != request.connection.kind) { return Err("Disconnect before changing the service for an existing connection.".into()); }
    let mut credential=if request.token.is_empty() || request.webhook_url.is_empty() {
        match secrets.resolve(account.clone()).await {
            Ok(secret)=>serde_json::from_str::<Credential>(secret.expose()).map_err(|_| "The saved credential could not be read.")?,
            Err(_) if existing.is_none()=>Credential{token:String::new(),webhook_url:String::new()},
            Err(_)=>return Err("Keychain is locked or the saved credential is missing. Disconnect and reconnect.".into()),
        }
    } else { Credential{token:String::new(),webhook_url:String::new()} };
    if !request.token.is_empty() { credential.token=request.token; }
    if !request.webhook_url.is_empty() { credential.webhook_url=request.webhook_url; }
    validate(&request.connection,&credential)?;
    let encoded=Zeroizing::new(serde_json::to_string(&credential).map_err(|_| "Could not protect this credential.")?);
    secrets.replace(account,encoded).await.map_err(|_| "Could not save this credential in Keychain.")?;
    store.save_connection(&request.connection).map_err(store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn connections_disconnect(app: AppHandle, manager: State<'_,Arc<MeetingSessionManager>>, connection_id:String) -> Result<(),String> {
    let _guard=operation_gate().lock().await;
    let store=manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
    // Stop future sends before removing the secret, even if Keychain refuses deletion.
    if let Some(mut connection)=store.connections().map_err(store_error)?.into_iter().find(|c|c.id==connection_id) {
        connection.enabled=false;store.save_connection(&connection).map_err(store_error)?;
    }
    app.state::<Arc<SecretManager>>().remove(SecretAccount::connection(&connection_id).map_err(|_| "Invalid connection.")?).await
        .map_err(|_| "The connection is off, but Keychain could not remove its credential. Unlock Keychain and disconnect again.")?;
    store.disconnect_connection(&connection_id).map_err(store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn connections_test(app:AppHandle,manager:State<'_,Arc<MeetingSessionManager>>,connection_id:String)->Result<String,String>{
    let _guard=operation_gate().lock().await;
    let store=manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
    let connection=selected(&store,&connection_id)?;
    connectors::test(&connection,&credential(&app,&connection).await?).await
}

#[tauri::command]
#[specta::specta]
pub async fn connections_preferences_save(manager:State<'_,Arc<MeetingSessionManager>>,preferences:ConnectionPreferences)->Result<(),String>{
    let store=manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
    if preferences.calendar_actions_enabled { calendar::request_access().await?; }
    store.save_connection_preferences(&preferences).map_err(store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn connections_rule_save(manager:State<'_,Arc<MeetingSessionManager>>,mut rule:AutoSendRule)->Result<(),String>{
    let _guard=operation_gate().lock().await;
    uuid(&rule.id)?;uuid(&rule.connection_id)?;
    let store=manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
    selected(&store,&rule.connection_id)?;
    match &rule.scope {
        SendScope::All=>{},
        SendScope::Series(key) if !store.automation_roster().map_err(store_error)?.series.iter().any(|series|series.series_key==*key)=>return Err("Choose a recorded meeting series.".into()),
        SendScope::Folder(id)=>{ uuid(id)?; if !store.meeting_folders().map_err(store_error)?.iter().any(|f|f.folder_id.uuid().to_string()==*id) { return Err("Choose an existing folder.".into()); } },
        _=>{},
    }
    store.save_connection_rule(&mut rule).map_err(store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn connections_rule_delete(manager:State<'_,Arc<MeetingSessionManager>>,rule_id:String)->Result<(),String>{
    let _guard=operation_gate().lock().await;
    manager.store().await.map_err(|_| "The meeting library is unavailable.")?.delete_connection_rule(&rule_id).map_err(store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn connections_send_notes(app:AppHandle,manager:State<'_,Arc<MeetingSessionManager>>,request:SendNotesRequest)->Result<SendReceipt,String>{
    let store=manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
    send_notes(&app,&store,&request,None).await
}

#[tauri::command]
#[specta::specta]
pub async fn connections_undo_send(app:AppHandle,manager:State<'_,Arc<MeetingSessionManager>>,receipt_id:String)->Result<(),String>{
    let store=manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
    undo_send(&app,&store,&receipt_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn connections_edit_chat_action(
    manager:State<'_,Arc<crate::agent_panel::AgentPanelManager>>,
    request:crate::agent_panel::AgentPanelActionRequestV1,
    action:crate::agent_panel::protocol::SonaChatActionV1,
)->Result<crate::agent_panel::AgentPanelTurnStatusV1,crate::agent_panel::AgentPanelCommandErrorV1>{
    manager.edit_action(request,action).await
}

#[tauri::command]
#[specta::specta]
pub async fn connections_preview_notes(manager:State<'_,Arc<MeetingSessionManager>>,session_id:MeetingSessionId)->Result<ConnectionNotesPreview,String>{
    let store=manager.store().await.map_err(|_| "The meeting library is unavailable.")?;
    let document=notes(&store,session_id)?;
    Ok(ConnectionNotesPreview{title:document.title,notes:document.notes})
}
