use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    SlackWebhook,
    SlackBot,
    NotionPage,
    NotionDatabase,
    Hubspot,
    AttioPerson,
    AttioCompany,
    Affinity,
    PublicWebhook,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub kind: ConnectionKind,
    /// Channel, Notion parent, or optional CRM record. Never a credential URL.
    pub target: String,
    pub enabled: bool,
}

#[derive(Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ConnectionSave {
    pub connection: Connection,
    /// Blank retains the existing credential. Never returned to the shell.
    pub token: String,
    pub webhook_url: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ConnectionPreferences {
    pub email_drafts_enabled: bool,
    pub calendar_actions_enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SendScope {
    All,
    Series(String),
    Folder(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AutoSendRule {
    pub id: String,
    pub connection_id: String,
    pub scope: SendScope,
    pub enabled: bool,
    /// Store commit time of this grant. Only later meetings or folder filings may send.
    pub created_at_utc_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SendState {
    Sending,
    Sent,
    Failed,
    Undone,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SendReceipt {
    pub id: String,
    pub connection_id: String,
    pub connection_name: String,
    pub session_id: Option<String>,
    pub state: SendState,
    pub detail: String,
    pub created_at_utc_ms: i64,
    pub undo: Option<RemoteUndo>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteUndo {
    Slack { channel: String, timestamp: String },
    Notion { page_id: String },
    Hubspot { note_id: String },
    Attio { note_id: String },
    Affinity { note_id: String },
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct ConnectionScopeOption {
    pub scope: SendScope,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct ConnectionsSnapshot {
    pub connections: Vec<Connection>,
    pub rules: Vec<AutoSendRule>,
    pub preferences: ConnectionPreferences,
    pub receipts: Vec<SendReceipt>,
    pub scopes: Vec<ConnectionScopeOption>,
}

#[derive(Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct SendNotesRequest {
    pub connection_id: String,
    pub session_id: crate::meeting::types::MeetingSessionId,
    pub operation_id: String,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct ConnectionNotesPreview {
    pub title: String,
    pub notes: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct NotesDocument {
    pub session_id: String,
    pub title: String,
    pub notes: String,
    pub started_at_utc_ms: i64,
    pub attendee_emails: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Credential {
    pub token: String,
    pub webhook_url: String,
}

impl Drop for Credential {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.token.zeroize();
        self.webhook_url.zeroize();
    }
}
