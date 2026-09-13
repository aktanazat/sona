use super::types::{
    CaptureCompleteness, GeneratedMeetingArtifacts, MeetingArtifactState, MeetingExportFormat,
    MeetingPhase, MeetingReviewSnapshot, ProcessingStatus,
};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use uuid::Uuid;

pub const MEETING_EXPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportError {
    Render,
    Io,
}

/// Everything one export says about a meeting: the review the page shows,
/// and the notes the person typed beside it.
///
/// The typed notes are a separate layer in the store — they autosave without
/// touching the session revision — so the review snapshot does not carry
/// them, and an export built from the snapshot alone used to leave the one
/// thing the person wrote themselves out of the file.
pub struct ExportDocument<'a> {
    pub review: &'a MeetingReviewSnapshot,
    /// The person's own notes, as typed. Empty when none were.
    pub user_notes: &'a str,
}

#[derive(Serialize)]
struct JsonExport<'a> {
    schema_version: u32,
    review: &'a MeetingReviewSnapshot,
    user_notes: &'a str,
}

pub fn render(
    format: MeetingExportFormat,
    document: &ExportDocument<'_>,
) -> Result<Vec<u8>, ExportError> {
    match format {
        MeetingExportFormat::Json => serde_json::to_vec_pretty(&JsonExport {
            schema_version: MEETING_EXPORT_SCHEMA_VERSION,
            review: document.review,
            user_notes: document.user_notes,
        })
        .map_err(|_| ExportError::Render),
        MeetingExportFormat::Markdown => Ok(render_markdown(document).into_bytes()),
    }
}

pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), ExportError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let Some(parent) = parent else {
        return Err(ExportError::Io);
    };
    let file_name = path.file_name().and_then(|name| name.to_str());
    let Some(file_name) = file_name.filter(|name| !name.is_empty()) else {
        return Err(ExportError::Io);
    };

    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|_| ExportError::Io)?;
        file.write_all(contents).map_err(|_| ExportError::Io)?;
        file.sync_all().map_err(|_| ExportError::Io)?;
        fs::rename(&temporary, path).map_err(|_| ExportError::Io)?;
        sync_parent_directory(parent)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn render_markdown(document: &ExportDocument<'_>) -> String {
    let review = document.review;
    let mut markdown = String::new();
    let session = &review.session;
    let _ = writeln!(markdown, "# {}", inline_text(&session.title));
    let _ = writeln!(markdown);
    let _ = writeln!(markdown, "- Phase: {}", phase_label(session.phase));
    let _ = writeln!(
        markdown,
        "- Capture completeness: {}",
        completeness_label(session.capture_completeness)
    );
    let _ = writeln!(
        markdown,
        "- Processing: {}",
        processing_label(&session.processing_status)
    );
    if let Some(started_at_utc_ms) = session.started_at_utc_ms {
        let _ = writeln!(
            markdown,
            "- Started at UTC milliseconds: {started_at_utc_ms}"
        );
    }

    let speakers = review
        .speakers
        .iter()
        .map(|speaker| (speaker.speaker_id, speaker.display_name.as_str()))
        .collect::<HashMap<_, _>>();

    let _ = writeln!(markdown, "\n## Transcript");
    let mut has_transcript = false;
    for segment in review.transcript.iter().filter(|segment| !segment.removed) {
        has_transcript = true;
        let text = segment
            .replacement_text
            .as_deref()
            .unwrap_or(segment.base.text.as_str());
        let speaker = speakers
            .get(&segment.assigned_speaker_id)
            .copied()
            .unwrap_or("Unknown speaker");
        let _ = writeln!(
            markdown,
            "- [{}–{}] {}: {}",
            format_offset(segment.base.start_offset_ns),
            format_offset(segment.base.end_offset_ns),
            inline_text(speaker),
            inline_text(text)
        );
    }
    if !has_transcript {
        let _ = writeln!(markdown, "No transcript is available.");
    }

    let _ = writeln!(markdown, "\n## Generated notes");
    match current_artifact(review) {
        Some(content) => render_generated_notes(&mut markdown, content),
        None => {
            let _ = writeln!(markdown, "No generated notes.");
        }
    }

    let _ = writeln!(markdown, "\n## Your notes");
    if document.user_notes.trim().is_empty() {
        let _ = writeln!(markdown, "No notes were typed.");
    } else {
        let _ = writeln!(markdown, "{}", document.user_notes.trim_end());
    }

    let _ = writeln!(markdown, "\n## Timestamped notes");
    if review.notes.is_empty() {
        let _ = writeln!(markdown, "No timestamped notes.");
    } else {
        for note in &review.notes {
            match (note.start_offset_ns, note.end_offset_ns) {
                (Some(start), Some(end)) => {
                    let _ = writeln!(
                        markdown,
                        "- [{}–{}] {}",
                        format_offset(start),
                        format_offset(end),
                        inline_text(&note.body)
                    );
                }
                _ => {
                    let _ = writeln!(markdown, "- {}", inline_text(&note.body));
                }
            }
        }
    }

    let _ = writeln!(markdown, "\n## Capture gaps");
    if review.gaps.is_empty() {
        let _ = writeln!(markdown, "No source gaps were recorded.");
    } else {
        for gap in &review.gaps {
            let _ = writeln!(markdown, "- {:?}", gap.reason);
        }
    }

    markdown
}

/// The notes the review page shows: the current revision with content.
fn current_artifact(review: &MeetingReviewSnapshot) -> Option<&GeneratedMeetingArtifacts> {
    review
        .artifacts
        .iter()
        .filter(|artifact| artifact.state == MeetingArtifactState::Current)
        .max_by_key(|artifact| artifact.generated_at_utc_ms)
        .and_then(|artifact| artifact.content.as_ref())
}

/// The sections a reader looks for, in the order the review page shows them.
/// An empty section is left out rather than written as "none": the file is
/// something a person hands to someone else.
fn render_generated_notes(markdown: &mut String, content: &GeneratedMeetingArtifacts) {
    let summary = content.summary.text.trim();
    if !summary.is_empty() {
        let _ = writeln!(markdown, "{summary}");
    }
    if !content.decisions.is_empty() {
        let _ = writeln!(markdown, "\n### Decisions");
        for decision in &content.decisions {
            let _ = writeln!(markdown, "- {}", inline_text(&decision.text));
        }
    }
    if !content.action_items.is_empty() {
        let _ = writeln!(markdown, "\n### Action items");
        for item in &content.action_items {
            let mut line = format!("- {}", inline_text(&item.text.text));
            if let Some(owner) = item.owner_text.as_deref().filter(|owner| !owner.is_empty()) {
                let _ = write!(line, " — {}", inline_text(owner));
            }
            if let Some(due) = item.due_text.as_deref().filter(|due| !due.is_empty()) {
                let _ = write!(line, " (due {})", inline_text(due));
            }
            let _ = writeln!(markdown, "{line}");
        }
    }
    if !content.key_questions.is_empty() {
        let _ = writeln!(markdown, "\n### Open questions");
        for question in &content.key_questions {
            let _ = writeln!(markdown, "- {}", inline_text(&question.text));
        }
    }
    if !content.risks.is_empty() {
        let _ = writeln!(markdown, "\n### Risks");
        for risk in &content.risks {
            let _ = writeln!(markdown, "- {}", inline_text(&risk.text));
        }
    }
    let follow_up = content.follow_up_draft.text.trim();
    if !follow_up.is_empty() {
        let _ = writeln!(markdown, "\n### Follow-up draft");
        let _ = writeln!(markdown, "{follow_up}");
    }
}

fn inline_text(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

fn format_offset(offset_ns: u64) -> String {
    let milliseconds = offset_ns / 1_000_000;
    let hours = milliseconds / 3_600_000;
    let minutes = (milliseconds / 60_000) % 60;
    let seconds = (milliseconds / 1_000) % 60;
    let milliseconds = milliseconds % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{milliseconds:03}")
}

const fn phase_label(phase: MeetingPhase) -> &'static str {
    match phase {
        MeetingPhase::Preflight => "preflight",
        MeetingPhase::Starting => "starting",
        MeetingPhase::CapturingRecording => "recording",
        MeetingPhase::CapturingPausing => "pausing",
        MeetingPhase::CapturingPaused => "paused",
        MeetingPhase::CapturingResuming => "resuming",
        MeetingPhase::Stopping => "stopping",
        MeetingPhase::Processing => "processing",
        MeetingPhase::ReviewReady => "review ready",
        MeetingPhase::RecoveryRequired => "recovery required",
        MeetingPhase::Deleting => "deleting",
    }
}

const fn completeness_label(completeness: CaptureCompleteness) -> &'static str {
    match completeness {
        CaptureCompleteness::NotStarted => "not started",
        CaptureCompleteness::Complete => "complete",
        CaptureCompleteness::Partial => "partial",
    }
}

const fn processing_label(status: &ProcessingStatus) -> &'static str {
    match status {
        ProcessingStatus::Pending => "pending",
        ProcessingStatus::Running => "running",
        ProcessingStatus::Succeeded => "succeeded",
        ProcessingStatus::Failed { .. } => "failed",
        ProcessingStatus::Cancelled => "cancelled",
    }
}

fn sync_parent_directory(parent: &Path) -> Result<(), ExportError> {
    #[cfg(unix)]
    {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ExportError::Io)
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::types::{
        AllowedMeetingAction, CitedArtifactText, DiarizationStatus, MeetingActionItem,
        MeetingArtifactId, MeetingArtifactRevision, MeetingDiarizationSnapshot, MeetingSessionId,
        ProcessingStatus, StorageAvailability, TranscriptRevisionId,
    };
    use tempfile::TempDir;

    fn cited(text: &str) -> CitedArtifactText {
        CitedArtifactText {
            text: text.to_string(),
            citations: Vec::new(),
        }
    }

    /// A revision in `state`, written at `generated_at_utc_ms`, whose summary
    /// is `summary`.
    fn revision(
        session_id: MeetingSessionId,
        state: MeetingArtifactState,
        generated_at_utc_ms: i64,
        summary: &str,
    ) -> MeetingArtifactRevision {
        MeetingArtifactRevision {
            artifact_id: MeetingArtifactId::new(),
            session_id,
            transcript_revision_id: TranscriptRevisionId::new(),
            input_revision: 1,
            template_id: "general".to_string(),
            template_version: 1,
            generation_key: generated_at_utc_ms.to_string(),
            state,
            generated_at_utc_ms,
            content: Some(GeneratedMeetingArtifacts {
                summary: cited(summary),
                summary_trace: Vec::new(),
                outline: Vec::new(),
                decisions: vec![cited("Ship the redesign in May.")],
                action_items: vec![MeetingActionItem {
                    text: cited("Draft the launch note"),
                    owner_text: Some("Priya".to_string()),
                    due_text: Some("Friday".to_string()),
                }],
                key_questions: Vec::new(),
                risks: Vec::new(),
                follow_up_draft: cited(""),
                ledger: None,
                ledger_failure: None,
            }),
        }
    }

    fn review() -> MeetingReviewSnapshot {
        MeetingReviewSnapshot {
            session: super::super::types::MeetingSessionSnapshot {
                session_id: MeetingSessionId::new(),
                phase: MeetingPhase::ReviewReady,
                revision: 4,
                title: "Design sync".to_string(),
                started_at_utc_ms: None,
                elapsed_offset_ns: None,
                sources: Vec::new(),
                open_capture_window_started_at_ns: None,
                capture_completeness: CaptureCompleteness::Partial,
                storage: StorageAvailability::Available,
                processing_status: ProcessingStatus::Failed {
                    reason: super::super::types::ProcessingFailure::LocalModelUnavailable,
                    cause: None,
                },
                preflight_local_processing: None,
                retention_deadline_utc_ms: None,
                allowed_actions: vec![AllowedMeetingAction::Export],
            },
            tracks: Vec::new(),
            gaps: Vec::new(),
            speakers: Vec::new(),
            transcript: Vec::new(),
            notes: Vec::new(),
            artifacts: Vec::new(),
            questions: Vec::new(),
            diarization: MeetingDiarizationSnapshot {
                status: DiarizationStatus::ModelUnavailable,
                model_id: "local-speaker-diarization".to_string(),
                model_version: "unavailable".to_string(),
                generation_id: None,
                assigned_segment_count: 0,
            },
            can_export: true,
            remote_cancellation_pending: false,
        }
    }

    #[test]
    fn json_export_is_versioned_and_path_free() {
        let review = review();
        let document = render(
            MeetingExportFormat::Json,
            &ExportDocument {
                review: &review,
                user_notes: "",
            },
        )
        .unwrap();
        let text = String::from_utf8(document).unwrap();
        assert!(text.contains("\"schema_version\": 1"));
        assert!(text.contains("Design sync"));
        assert!(!text.contains("/tmp/"));
    }

    #[test]
    fn markdown_export_keeps_processing_failure_visible() {
        let review = review();
        let document = render(
            MeetingExportFormat::Markdown,
            &ExportDocument {
                review: &review,
                user_notes: "",
            },
        )
        .unwrap();
        let text = String::from_utf8(document).unwrap();
        assert!(text.contains("Capture completeness: partial"));
        assert!(text.contains("Processing: failed"));
    }

    /// The file used to hold the transcript and the timestamped notes and
    /// nothing a person would hand to a colleague: not the generated notes
    /// the page shows, and not the notes they typed themselves.
    #[test]
    fn markdown_export_carries_the_current_notes_and_the_typed_notes() {
        let mut review = review();
        let session_id = review.session.session_id;
        review.artifacts = vec![
            revision(
                session_id,
                MeetingArtifactState::OutOfDate,
                20,
                "An earlier reading.",
            ),
            revision(
                session_id,
                MeetingArtifactState::Current,
                10,
                "We agreed on the May launch.",
            ),
        ];
        let document = render(
            MeetingExportFormat::Markdown,
            &ExportDocument {
                review: &review,
                user_notes: "Ask Priya about the venue.\n",
            },
        )
        .unwrap();
        let text = String::from_utf8(document).unwrap();

        let generated = text
            .split("## Generated notes\n")
            .nth(1)
            .and_then(|rest| rest.split("\n## Your notes\n").next())
            .expect("a generated notes section");
        assert!(
            generated.contains("We agreed on the May launch."),
            "the current revision's summary is the export's, not the out-of-date one's: {generated}"
        );
        assert!(!generated.contains("An earlier reading."));
        assert!(generated.contains("- Ship the redesign in May."));
        assert!(generated.contains("- Draft the launch note — Priya (due Friday)"));
        let typed = text
            .split("## Your notes\n")
            .nth(1)
            .and_then(|rest| rest.split("\n## Timestamped notes\n").next())
            .expect("a typed notes section");
        assert_eq!(typed.trim(), "Ask Priya about the venue.");
    }

    #[test]
    fn json_export_carries_the_typed_notes() {
        let review = review();
        let document = render(
            MeetingExportFormat::Json,
            &ExportDocument {
                review: &review,
                user_notes: "Ask Priya about the venue.",
            },
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&document).unwrap();
        assert_eq!(parsed["user_notes"], "Ask Priya about the venue.");
    }

    #[test]
    fn atomic_write_replaces_only_completed_output() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("meeting.md");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read(path).unwrap(), b"second");
    }
}
