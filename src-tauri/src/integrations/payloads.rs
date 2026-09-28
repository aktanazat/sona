use super::types::NotesDocument;
use serde::Serialize;
use serde_json::{json, Value};

pub(crate) fn slack(text: &str, channel: Option<&str>) -> Result<Value, String> {
    let text = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    if text.chars().count() > 39_000 {
        return Err("These notes are too long for one Slack message. Export them instead.".into());
    }
    let mut value = json!({"text":text,"mrkdwn":true,"unfurl_links":false,"unfurl_media":false});
    if let Some(channel) = channel {
        value["channel"] = json!(channel);
    }
    Ok(value)
}

fn rich_text(text: &str) -> Vec<Value> {
    let mut start = 0;
    let mut count = 0;
    let mut chunks = Vec::new();
    for (offset, _) in text.char_indices() {
        if count == 2000 {
            chunks.push(json!({"type":"text","text":{"content":&text[start..offset]}}));
            start = offset;
            count = 0;
        }
        count += 1;
    }
    if start < text.len() {
        chunks.push(json!({"type":"text","text":{"content":&text[start..]}}));
    }
    chunks
}

pub(crate) fn notion(
    document: &NotesDocument,
    parent: &str,
    database: bool,
    title_property: &str,
) -> Result<Value, String> {
    let mut children = Vec::new();
    for line in document
        .notes
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let (kind, text) = if let Some(text) = line.strip_prefix("### ") {
            ("heading_3", text)
        } else if let Some(text) = line.strip_prefix("## ") {
            ("heading_2", text)
        } else if let Some(text) = line.strip_prefix("# ") {
            ("heading_1", text)
        } else if let Some(text) = line.strip_prefix("- ") {
            ("bulleted_list_item", text)
        } else {
            ("paragraph", line)
        };
        let rich = rich_text(text);
        if rich.len() > 100 {
            return Err("A paragraph is too long for Notion. Export these notes instead.".into());
        }
        children.push(json!({"object":"block","type":kind,kind:{"rich_text":rich}}));
    }
    if children.len() > 100 {
        return Err(
            "These notes have more blocks than Notion accepts in one page. Export them instead."
                .into(),
        );
    }
    Ok(json!({
        "parent": if database { json!({"database_id":parent}) } else { json!({"page_id":parent}) },
        "properties": {title_property:{"type":"title","title":rich_text(&document.title)}},
        "children":children
    }))
}

fn html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// Field order matches the existing JSON key order, including the signed webhook body.
#[derive(Serialize)]
pub(crate) struct HubspotNote<'a> {
    associations: Vec<HubspotAssociation<'a>>,
    properties: HubspotNoteProperties,
}

#[derive(Serialize)]
struct HubspotNoteProperties {
    hs_note_body: String,
    hs_timestamp: String,
}

#[derive(Serialize)]
struct HubspotAssociation<'a> {
    to: HubspotContact<'a>,
    types: [HubspotAssociationType; 1],
}

#[derive(Serialize)]
struct HubspotContact<'a> {
    id: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HubspotAssociationType {
    association_category: &'static str,
    association_type_id: u16,
}

pub(crate) fn hubspot<'a>(document: &NotesDocument, contacts: &'a [String]) -> HubspotNote<'a> {
    HubspotNote {
        associations: contacts
            .iter()
            .map(|id| HubspotAssociation {
                to: HubspotContact { id },
                types: [HubspotAssociationType {
                    association_category: "HUBSPOT_DEFINED",
                    association_type_id: 202,
                }],
            })
            .collect(),
        properties: HubspotNoteProperties {
            hs_note_body: format!(
                "<h2>{}</h2><p>{}</p>",
                html(&document.title),
                html(&document.notes).replace('\n', "<br>")
            ),
            hs_timestamp: document.started_at_utc_ms.to_string(),
        },
    }
}

#[derive(Serialize)]
pub(crate) struct AttioNote<'a> {
    data: AttioNoteData<'a>,
}

#[derive(Serialize)]
struct AttioNoteData<'a> {
    content: &'a str,
    format: &'static str,
    parent_object: &'a str,
    parent_record_id: &'a str,
    title: &'a str,
}

pub(crate) fn attio<'a>(
    document: &'a NotesDocument,
    object: &'a str,
    record_id: &'a str,
) -> AttioNote<'a> {
    AttioNote {
        data: AttioNoteData {
            content: &document.notes,
            format: "markdown",
            parent_object: object,
            parent_record_id: record_id,
            title: &document.title,
        },
    }
}

#[derive(Serialize)]
pub(crate) struct AffinityNote<'a> {
    content: String,
    person_ids: &'a [u64],
    #[serde(rename = "type")]
    note_type: u8,
}

pub(crate) fn affinity<'a>(document: &NotesDocument, people: &'a [u64]) -> AffinityNote<'a> {
    AffinityNote {
        content: format!("{}\n\n{}", document.title, document.notes),
        person_ids: people,
        note_type: 0,
    }
}

#[derive(Serialize)]
pub(crate) struct WebhookNotes<'a> {
    event: &'static str,
    meeting: WebhookMeeting<'a>,
    version: u8,
}

#[derive(Serialize)]
struct WebhookMeeting<'a> {
    notes: &'a str,
    session_id: &'a str,
    started_at_utc_ms: i64,
    title: &'a str,
}

pub(crate) fn webhook(document: &NotesDocument) -> WebhookNotes<'_> {
    WebhookNotes {
        event: "meeting.notes_ready",
        meeting: WebhookMeeting {
            notes: &document.notes,
            session_id: &document.session_id,
            started_at_utc_ms: document.started_at_utc_ms,
            title: &document.title,
        },
        version: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document() -> NotesDocument {
        NotesDocument {
            session_id: "meeting-1".into(),
            title: "Roadmap & next".into(),
            notes: "Ready <soon>\n### Decisions\n- Ship".into(),
            started_at_utc_ms: 1_700_000_000_000,
            attendee_emails: vec!["alex@example.com".into()],
        }
    }

    #[test]
    fn slack_cannot_turn_notes_into_mentions() {
        assert_eq!(
            slack("Hello <!channel> & team", Some("C123")).unwrap(),
            json!({
            "text":"Hello &lt;!channel&gt; &amp; team","channel":"C123","mrkdwn":true,
            "unfurl_links":false,"unfurl_media":false})
        );
        assert!(slack(&"x".repeat(39_001), None).is_err());
    }

    #[test]
    fn notion_page_and_database_preserve_blocks_and_title_property() {
        let page = notion(&document(), "parent", false, "title").unwrap();
        assert_eq!(page["parent"], json!({"page_id":"parent"}));
        assert_eq!(
            page["children"],
            json!([
                {"object":"block","type":"paragraph","paragraph":{"rich_text":[{"type":"text","text":{"content":"Ready <soon>"}}]}},
                {"object":"block","type":"heading_3","heading_3":{"rich_text":[{"type":"text","text":{"content":"Decisions"}}]}},
                {"object":"block","type":"bulleted_list_item","bulleted_list_item":{"rich_text":[{"type":"text","text":{"content":"Ship"}}]}}
            ])
        );
        let db = notion(&document(), "db", true, "Meeting").unwrap();
        assert_eq!(db["parent"], json!({"database_id":"db"}));
        assert_eq!(
            db["properties"]["Meeting"]["title"][0]["text"]["content"],
            "Roadmap & next"
        );
        let long = "é".repeat(2001);
        let chunks = rich_text(&long);
        assert_eq!(
            chunks[0]["text"]["content"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            2000
        );
        assert_eq!(chunks[1]["text"]["content"], "é");
    }

    #[test]
    fn crm_notes_use_existing_records_and_escape_html() -> serde_json::Result<()> {
        assert_eq!(
            serde_json::to_vec(&hubspot(&document(), &["42".into()]))?,
            serde_json::to_vec(&json!({"properties":{
            "hs_timestamp":"1700000000000","hs_note_body":"<h2>Roadmap &amp; next</h2><p>Ready &lt;soon&gt;<br>### Decisions<br>- Ship</p>"},
            "associations":[{"to":{"id":"42"},"types":[{"associationCategory":"HUBSPOT_DEFINED","associationTypeId":202}]}]}))?
        );
        assert_eq!(
            serde_json::to_vec(&attio(&document(), "people", "record"))?,
            serde_json::to_vec(
                &json!({"data":{"parent_object":"people","parent_record_id":"record",
            "title":"Roadmap & next","format":"markdown","content":"Ready <soon>\n### Decisions\n- Ship"}})
            )?
        );
        assert_eq!(
            serde_json::to_value(attio(&document(), "companies", "company"))?["data"]
                ["parent_object"],
            "companies"
        );
        assert_eq!(
            serde_json::to_vec(&affinity(&document(), &[42]))?,
            serde_json::to_vec(
                &json!({"content":"Roadmap & next\n\nReady <soon>\n### Decisions\n- Ship","person_ids":[42],"type":0})
            )?
        );
        Ok(())
    }

    #[test]
    fn webhook_envelope_contains_only_generated_notes_not_matching_addresses(
    ) -> serde_json::Result<()> {
        assert_eq!(
            serde_json::to_vec(&webhook(&document()))?,
            serde_json::to_vec(
                &json!({"event":"meeting.notes_ready","version":1,"meeting":{
            "session_id":"meeting-1","title":"Roadmap & next","notes":"Ready <soon>\n### Decisions\n- Ship",
            "started_at_utc_ms":1700000000000_i64}})
            )?
        );
        let mut empty = document();
        empty.notes.clear();
        assert_eq!(
            notion(&empty, "parent", false, "title").unwrap()["children"],
            json!([])
        );
        Ok(())
    }
}
