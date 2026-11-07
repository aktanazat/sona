use super::types::{ArtifactCitation, MeetingOperationId, MeetingSessionId};
use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(transparent)]
pub struct DocumentId(pub Uuid);

impl DocumentId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

impl Default for DocumentId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct DocumentSummary {
    pub id: DocumentId,
    pub title: String,
    pub source_name: String,
    pub media_type: String,
    pub created_at_utc_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct Document {
    pub summary: DocumentSummary,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct DocumentIngestRequest {
    pub path: String,
    pub operation_id: MeetingOperationId,
    /// The meeting the document belongs to, when it was added to one.
    #[serde(default)]
    pub session_id: Option<MeetingSessionId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct DocumentDeleteRequest {
    pub document_id: DocumentId,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct DocumentListResult {
    pub schema_version: u32,
    pub revision: u64,
    pub entries: Vec<Document>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct DocumentMutationResult {
    pub schema_version: u32,
    pub revision: u64,
    pub document: Option<Document>,
    pub removed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingDocument {
    pub document: DocumentSummary,
    /// What was said in the meeting that differs from the document; `None`
    /// once the meeting's transcript is deleted and nothing can be compared.
    pub differences: Option<Vec<DocumentDifference>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct DocumentDifference {
    /// The PDF page the passage is on, counted from 1.
    pub page: Option<u32>,
    /// The document's words around what it says, as they are on the page.
    pub passage: String,
    /// What the document says: "sixty (60) days".
    pub written: String,
    /// What was said instead: "thirty days".
    pub said: String,
    /// The words around what was said.
    pub quote: String,
    /// Who said it, when Sona may name them.
    pub speaker: Option<String>,
    pub citation: ArtifactCitation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct DocumentMark {
    /// The page, counted from 1.
    pub page: u32,
    /// The text to highlight, as it is on the page.
    pub passage: String,
    pub note: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MarkedCopyRequest {
    pub session_id: MeetingSessionId,
    pub document_id: DocumentId,
    pub destination_path: String,
    pub marks: Vec<DocumentMark>,
}
