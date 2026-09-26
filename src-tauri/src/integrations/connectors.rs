use super::{payloads, types::*, webhook};
use reqwest::{Client, Method};
use serde_json::{json, Value};
use std::{collections::BTreeSet, time::Duration};

fn client() -> Result<Client, String> {
    Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(25))
        .build().map_err(|_| "Could not prepare a secure connection.".into())
}

async fn api(client: &Client, credential: &Credential, method: Method, url: &str, body: Option<&impl serde::Serialize>) -> Result<Value, String> {
    let mut request = client.request(method, url).bearer_auth(&credential.token);
    if url.starts_with("https://api.notion.com/") { request = request.header("Notion-Version", "2022-06-28"); }
    if let Some(body) = body { request = request.json(body); }
    let mut response = request.send().await.map_err(|_| "The service did not confirm the request. Check it before sending again.".to_owned())?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 => "The credential was refused. Replace it in Connections.".into(),
            403 => "This credential cannot access the destination. Check its permissions.".into(),
            404 => "The destination was not found or has not been shared with this connection.".into(),
            429 => "The service is busy. Wait a moment before trying again.".into(),
            _ => format!("The service returned HTTP {}. Check the destination and its permissions.",status.as_u16()),
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "The service response was interrupted.")? {
        if bytes.len() + chunk.len() > 1_048_576 { return Err("The service response was too large.".into()); }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() { return Ok(Value::Null); }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "The service returned an unreadable response.".to_owned())?;
    if url.starts_with("https://slack.com/api/") && value.get("ok") != Some(&Value::Bool(true)) {
        // Slack messages may echo secrets or content; translate only known codes.
        return Err(match value["error"].as_str() {
            Some("not_in_channel") => "Invite the Slack app to this channel first.",
            Some("channel_not_found") => "Slack could not find that channel. Use its channel ID.",
            Some("missing_scope") => "The Slack app needs chat:write and channels:read (groups:read for private channels).",
            Some("invalid_auth" | "token_revoked" | "not_authed") => "Slack refused this token. Replace it in Connections.",
            _ => "Slack refused the request. Check the token, channel and app permissions.",
        }.into());
    }
    Ok(value)
}

fn string_id(value: &Value, pointer: &str) -> Result<String, String> {
    let value = value.pointer(pointer).ok_or("The service did not return a receipt. Check the destination before sending again.")?;
    let id = value.as_str().map(str::to_owned).or_else(|| value.as_u64().map(|id| id.to_string()))
        .ok_or("The service returned an unreadable receipt.")?;
    if id.is_empty() || id.len() > 256 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b,b'-'|b'_'|b'.')) {
        return Err("The service returned an invalid receipt.".into());
    }
    Ok(id)
}

pub(crate) async fn send(connection: &Connection, credential: &Credential, document: &NotesDocument, delivery: &str) -> Result<Option<RemoteUndo>, String> {
    use ConnectionKind::*;
    if matches!(connection.kind, SlackWebhook | PublicWebhook) {
        let body = if connection.kind == SlackWebhook {
            serde_json::to_vec(&payloads::slack(&format!("*{}*\n\n{}",document.title,document.notes),None)?)
        } else { serde_json::to_vec(&payloads::webhook(document)) }
            .map_err(|_| "Could not prepare these notes.")?;
        webhook::post(&credential.webhook_url,&credential.token,delivery,&body,connection.kind == PublicWebhook).await?;
        return Ok(None);
    }
    let client = client()?;
    match connection.kind {
        SlackBot => post_slack_with(&client, connection, credential, &format!("*{}*\n\n{}",document.title,document.notes)).await.map(Some),
        NotionPage | NotionDatabase => {
            let database = connection.kind == NotionDatabase;
            let property = if database {
                let data = api(&client,credential,Method::GET,&format!("https://api.notion.com/v1/databases/{}",connection.target),None::<&()>).await?;
                data["properties"].as_object().and_then(|properties| properties.iter().find(|(_,property)| property["type"] == "title").map(|(key,_)| key.clone()))
                    .ok_or("This Notion database has no title property.")?
            } else { "title".into() };
            let payload = payloads::notion(document,&connection.target,database,&property)?;
            let data = api(&client,credential,Method::POST,"https://api.notion.com/v1/pages",Some(&payload)).await?;
            Ok(Some(RemoteUndo::Notion {page_id:string_id(&data,"/id")?}))
        }
        Hubspot => {
            let contacts = if !connection.target.is_empty() { vec![connection.target.clone()] } else {
                if document.attendee_emails.is_empty() { return Err("This meeting has no attendee email addresses to match in HubSpot. Choose a contact ID in Connections.".into()); }
                let data = api(&client,credential,Method::POST,"https://api.hubapi.com/crm/v3/objects/contacts/search",Some(&json!({
                    "filterGroups":[{"filters":[{"propertyName":"email","operator":"IN","values":document.attendee_emails}]}],
                    "properties":["email"],"limit":100}))).await?;
                let mut contacts = Vec::new();
                for contact in data["results"].as_array().ok_or("HubSpot returned an unreadable contact list.")? {
                    if contact["properties"]["email"].as_str().is_some_and(|email| document.attendee_emails.iter().any(|expected| expected.eq_ignore_ascii_case(email))) {
                        contacts.push(string_id(contact,"/id")?);
                    }
                }
                contacts
            };
            if contacts.is_empty() { return Err("No existing HubSpot contacts match this meeting's attendees. No note was created.".into()); }
            let data = api(&client,credential,Method::POST,"https://api.hubapi.com/crm/v3/objects/notes",Some(&payloads::hubspot(document,&contacts))).await?;
            Ok(Some(RemoteUndo::Hubspot {note_id:string_id(&data,"/id")?}))
        }
        AttioPerson | AttioCompany => {
            let object = if connection.kind == AttioPerson { "people" } else { "companies" };
            let target = if connection.target.is_empty() {
                let mut records = BTreeSet::new();
                let values: BTreeSet<&str> = document.attendee_emails.iter().filter_map(|email| {
                    if object == "people" { Some(email.as_str()) } else { email.split_once('@').map(|(_,domain)|domain) }
                }).collect();
                for value in values {
                    let filter = if object == "people" { json!({"email_addresses":{"email_address":{"$eq":value}}}) }
                        else { json!({"domains":{"domain":{"$eq":value}}}) };
                    let data = api(&client,credential,Method::POST,&format!("https://api.attio.com/v2/objects/{object}/records/query"),Some(&json!({"filter":filter,"limit":2}))).await?;
                    for record in data["data"].as_array().ok_or("Attio returned an unreadable record list.")? { records.insert(string_id(record,"/id/record_id")?); }
                }
                if records.len() != 1 { return Err("Choose one Attio record ID in Connections. The attendees did not match exactly one existing record. No note was created.".into()); }
                records.into_iter().next().ok_or("No matching Attio record was found.")?
            } else { connection.target.clone() };
            let data = api(&client,credential,Method::POST,"https://api.attio.com/v2/notes",Some(&payloads::attio(document,object,&target))).await?;
            Ok(Some(RemoteUndo::Attio {note_id:string_id(&data,"/data/id/note_id")?}))
        }
        Affinity => {
            let people = if connection.target.is_empty() {
                let mut people = BTreeSet::new();
                for email in &document.attendee_emails {
                    let mut url = url::Url::parse("https://api.affinity.co/persons").map_err(|_| "Could not prepare an Affinity request.")?;
                    url.query_pairs_mut().append_pair("term",email).append_pair("page_size","100");
                    let data = api(&client,credential,Method::GET,url.as_str(),None::<&()>).await?;
                    for person in data["persons"].as_array().ok_or("Affinity returned an unreadable person list.")? {
                        if person["emails"].as_array().is_some_and(|emails| emails.iter().any(|candidate| candidate.as_str().is_some_and(|candidate|candidate.eq_ignore_ascii_case(email)))) {
                            if let Some(id) = person["id"].as_u64() { people.insert(id); }
                        }
                    }
                }
                people.into_iter().collect::<Vec<_>>()
            } else { vec![connection.target.parse::<u64>().map_err(|_| "Enter a numeric Affinity person ID.")?] };
            if people.is_empty() { return Err("No existing Affinity people match this meeting's attendees. No note was created.".into()); }
            let data = api(&client,credential,Method::POST,"https://api.affinity.co/notes",Some(&payloads::affinity(document,&people))).await?;
            Ok(Some(RemoteUndo::Affinity {note_id:string_id(&data,"/id")?}))
        }
        SlackWebhook | PublicWebhook => unreachable!("webhooks handled before API transport"),
    }
}

async fn post_slack_with(client: &Client, connection: &Connection, credential: &Credential, text: &str) -> Result<RemoteUndo, String> {
    let payload = payloads::slack(text,Some(&connection.target))?;
    let data = api(client,credential,Method::POST,"https://slack.com/api/chat.postMessage",Some(&payload)).await?;
    Ok(RemoteUndo::Slack {channel:string_id(&data,"/channel")?,timestamp:string_id(&data,"/ts")?})
}

pub(crate) async fn post_slack(connection: &Connection, credential: &Credential, text: &str, delivery: &str) -> Result<Option<RemoteUndo>, String> {
    match connection.kind {
        ConnectionKind::SlackBot => post_slack_with(&client()?,connection,credential,text).await.map(Some),
        ConnectionKind::SlackWebhook => {
            let body = serde_json::to_vec(&payloads::slack(text,None)?).map_err(|_| "Could not prepare this message.")?;
            webhook::post(&credential.webhook_url,"",delivery,&body,false).await?;
            Ok(None)
        }
        _ => Err("Choose a Slack connection for this message.".into()),
    }
}

pub(crate) async fn test(connection: &Connection, credential: &Credential) -> Result<String, String> {
    use ConnectionKind::*;
    let client = client()?;
    match connection.kind {
        SlackWebhook => { post_slack(connection,credential,"Sona connection test. No meeting notes were sent.",&uuid::Uuid::new_v4().to_string()).await?; }
        PublicWebhook => {
            webhook::post(&credential.webhook_url,&credential.token,&uuid::Uuid::new_v4().to_string(),
                br#"{"event":"connection.test","version":1,"message":"No meeting notes were sent."}"#,true).await?;
        }
        SlackBot => {
            api(&client,credential,Method::POST,"https://slack.com/api/auth.test",Some(&json!({}))).await?;
            api(&client,credential,Method::GET,&format!("https://slack.com/api/conversations.info?channel={}",connection.target),None::<&()>).await?;
        }
        NotionPage | NotionDatabase => {
            let kind = if connection.kind == NotionPage { "pages" } else { "databases" };
            api(&client,credential,Method::GET,&format!("https://api.notion.com/v1/{kind}/{}",connection.target),None::<&()>).await?;
        }
        Hubspot => { api(&client,credential,Method::GET,"https://api.hubapi.com/crm/v3/objects/contacts?limit=1",None::<&()>).await?; }
        AttioPerson | AttioCompany => { api(&client,credential,Method::GET,"https://api.attio.com/v2/objects",None::<&()>).await?; }
        Affinity => { api(&client,credential,Method::GET,"https://api.affinity.co/auth/whoami",None::<&()>).await?; }
    }
    Ok(if matches!(connection.kind,SlackWebhook|PublicWebhook) { "Test delivered. No meeting notes were sent." } else { "Connected. The service accepted this credential. Sending notes also needs write permission." }.into())
}

pub(crate) async fn undo(credential: &Credential, receipt: &RemoteUndo) -> Result<(), String> {
    let client = client()?;
    match receipt {
        RemoteUndo::Slack {channel,timestamp} => { api(&client,credential,Method::POST,"https://slack.com/api/chat.delete",Some(&json!({"channel":channel,"ts":timestamp}))).await?; }
        RemoteUndo::Notion {page_id} => { api(&client,credential,Method::PATCH,&format!("https://api.notion.com/v1/pages/{page_id}"),Some(&json!({"archived":true}))).await?; }
        RemoteUndo::Hubspot {note_id} => { api(&client,credential,Method::DELETE,&format!("https://api.hubapi.com/crm/v3/objects/notes/{note_id}"),None::<&()>).await?; }
        RemoteUndo::Attio {note_id} => { api(&client,credential,Method::DELETE,&format!("https://api.attio.com/v2/notes/{note_id}"),None::<&()>).await?; }
        RemoteUndo::Affinity {note_id} => { api(&client,credential,Method::DELETE,&format!("https://api.affinity.co/notes/{note_id}"),None::<&()>).await?; }
    }
    Ok(())
}
