use super::{map_store_error, now_utc_ms};
use crate::meeting::document_types::{
    DocumentDeleteRequest, DocumentIngestRequest, DocumentListResult, DocumentMutationResult,
    MarkedCopyRequest, MeetingDocument,
};
use crate::meeting::export;
use crate::meeting::pdf::{self, PdfMark};
use crate::meeting::people_types::PersonId;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::documents::DocumentBody;
use crate::meeting::types::{MeetingCommandError, MeetingSessionId};
use crate::meeting::workflow_types::{NewWorkflowEvent, WorkflowEventKind};
use std::path::{Path, PathBuf};

impl MeetingSessionManager {
    pub async fn doc_ingest(
        &self,
        request: DocumentIngestRequest,
    ) -> Result<DocumentMutationResult, MeetingCommandError> {
        let path = PathBuf::from(&request.path);
        let (title, source_name) = document_names(&path)?;
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or(MeetingCommandError::InvalidRequest)?;
        let body = match extension.as_str() {
            "txt" | "md" | "markdown" => DocumentBody::Text {
                markdown: extension != "txt",
                content: std::fs::read_to_string(&path)
                    .map_err(|_| MeetingCommandError::InvalidRequest)?,
            },
            "pdf" => tauri::async_runtime::spawn_blocking(move || pdf_body(&path))
                .await
                .map_err(|_| MeetingCommandError::ImportUnreadable)??,
            _ => return Err(MeetingCommandError::InvalidRequest),
        };
        let store = self.store().await?;
        let result = store
            .ingest_document(
                request.operation_id,
                title,
                source_name,
                body,
                request.session_id,
                now_utc_ms(),
            )
            .map_err(map_store_error)?;
        let document_id = result
            .document
            .as_ref()
            .map(|document| document.summary.id)
            .ok_or(MeetingCommandError::StorageUnavailable)?;
        self.dispatch_contained(
            store,
            NewWorkflowEvent {
                kind: WorkflowEventKind::DocumentIngested,
                payload: serde_json::json!({"document_id": document_id.uuid().to_string()}),
                occurred_at_utc_ms: now_utc_ms(),
                source: "document_import",
                dedupe_key: format!("doc-ingested:{}", document_id.uuid()),
            },
            None,
        );
        self.emit_artifact_changed(None, result.revision);
        Ok(result)
    }

    pub async fn doc_meeting_list(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<MeetingDocument>, MeetingCommandError> {
        self.store()
            .await?
            .meeting_documents(session_id)
            .map_err(map_store_error)
    }

    /// Write a copy of a meeting's PDF with each passage highlighted and its
    /// note beside it, returning how many notes the copy holds. The PDF is
    /// copied from the bytes kept when it was added, so the file it came
    /// from is never opened again.
    pub async fn doc_marked_copy(
        &self,
        request: MarkedCopyRequest,
    ) -> Result<u32, MeetingCommandError> {
        let marks = request
            .marks
            .into_iter()
            .map(|mark| {
                Ok(PdfMark {
                    page: usize::try_from(mark.page)
                        .ok()
                        .and_then(|page| page.checked_sub(1))
                        .ok_or(MeetingCommandError::InvalidRequest)?,
                    passage: mark.passage,
                    note: mark.note,
                })
            })
            .collect::<Result<Vec<_>, MeetingCommandError>>()?;
        let original = self
            .store()
            .await?
            .meeting_document_original(request.session_id, request.document_id)
            .map_err(map_store_error)?;
        let path = PathBuf::from(request.destination_path);
        tauri::async_runtime::spawn_blocking(move || {
            let marked = pdf::marked_copy(&original, &marks)
                .map_err(|_| MeetingCommandError::ExportFailed)?;
            export::write_atomic(&path, &marked.bytes)
                .map_err(|_| MeetingCommandError::ExportFailed)?;
            Ok(marked.marks)
        })
        .await
        .map_err(|_| MeetingCommandError::ExportFailed)?
    }

    pub async fn doc_list(
        &self,
        person_id: Option<PersonId>,
    ) -> Result<DocumentListResult, MeetingCommandError> {
        self.store()
            .await?
            .documents_list(person_id)
            .map_err(map_store_error)
    }

    pub async fn doc_delete(
        &self,
        request: DocumentDeleteRequest,
    ) -> Result<DocumentMutationResult, MeetingCommandError> {
        let result = self
            .store()
            .await?
            .delete_document(request.document_id, request.expected_revision)
            .map_err(map_store_error)?;
        self.emit_artifact_changed(None, result.revision);
        Ok(result)
    }
}

fn document_names(path: &Path) -> Result<(String, String), MeetingCommandError> {
    let source_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or(MeetingCommandError::InvalidRequest)?;
    let title = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or(MeetingCommandError::InvalidRequest)?;
    Ok((title.to_string(), source_name.to_string()))
}

fn pdf_body(path: &Path) -> Result<DocumentBody, MeetingCommandError> {
    let length = std::fs::metadata(path)
        .map_err(|_| MeetingCommandError::InvalidRequest)?
        .len();
    if length > pdf::MAX_PDF_BYTES {
        return Err(MeetingCommandError::ImportUnreadable);
    }
    let original = std::fs::read(path).map_err(|_| MeetingCommandError::InvalidRequest)?;
    let pages = pdf::page_texts(&original).map_err(|_| MeetingCommandError::ImportUnreadable)?;
    Ok(DocumentBody::Pdf { pages, original })
}
