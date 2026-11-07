use super::documents::DocumentBody;
use super::workflow_core_tests::{meeting, store, transcript_segments};
use crate::meeting::pdf;
use crate::meeting::types::MeetingOperationId;
use rusqlite::params;

#[test]
fn a_meeting_pdf_shows_what_was_said_against_the_page_that_says_otherwise() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Lease call", 1_793_000_000_000);
    transcript_segments(
        &store,
        meeting_id,
        &[
            "The rent is fine.",
            "So thirty days notice is fine, right?",
            "And the deposit was 2,500 dollars.",
        ],
    );
    let original = pdf::fixture::pdf(&[
        &["Residential Lease", "The rent is $2,500 per month."],
        &[
            "Tenant shall give Landlord sixty (60) days notice",
            "before moving out. The security deposit is $2,500.",
        ],
    ]);
    let pages = pdf::page_texts(&original).unwrap();
    store
        .ingest_document(
            MeetingOperationId::new(),
            "Lease".to_string(),
            "lease.pdf".to_string(),
            DocumentBody::Pdf {
                pages,
                original: original.clone(),
            },
            Some(meeting_id),
            1,
        )
        .unwrap();

    let documents = store.meeting_documents(meeting_id).unwrap();

    assert_eq!(documents.len(), 1);
    let differences = documents[0].differences.as_ref().unwrap();
    assert_eq!(differences.len(), 1);
    let difference = &differences[0];
    assert_eq!(difference.page, Some(2));
    assert_eq!(difference.written, "sixty (60) days");
    assert_eq!(difference.said, "thirty days");
    assert_eq!(difference.quote, "So thirty days notice is fine, right?");
    assert_eq!(difference.citation.start_offset_ns, 1_000_000_000);
    assert_eq!(
        store
            .meeting_document_original(meeting_id, documents[0].document.id)
            .unwrap(),
        original
    );

    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_sessions SET transcript_purged_at_utc_ms = 2 WHERE id = ?1",
            params![meeting_id.uuid().to_string()],
        )
        .unwrap();
    assert!(store.meeting_documents(meeting_id).unwrap()[0]
        .differences
        .is_none());
}
