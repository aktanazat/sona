use super::types::{
    CaptureCompleteness, MeetingExportFormat, MeetingPhase, MeetingReviewSnapshot, ProcessingStatus,
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

#[derive(Serialize)]
struct JsonExport<'a> {
    schema_version: u32,
    review: &'a MeetingReviewSnapshot,
}

pub fn render(
    format: MeetingExportFormat,
    review: &MeetingReviewSnapshot,
) -> Result<Vec<u8>, ExportError> {
    match format {
        MeetingExportFormat::Json => serde_json::to_vec_pretty(&JsonExport {
            schema_version: MEETING_EXPORT_SCHEMA_VERSION,
            review,
        })
        .map_err(|_| ExportError::Render),
        MeetingExportFormat::Markdown => Ok(render_markdown(review).into_bytes()),
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

fn render_markdown(review: &MeetingReviewSnapshot) -> String {
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

    let _ = writeln!(markdown, "\n## Notes");
    if review.notes.is_empty() {
        let _ = writeln!(markdown, "No manual notes.");
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
        AllowedMeetingAction, DiarizationStatus, MeetingDiarizationSnapshot, MeetingSessionId,
        ProcessingStatus, StorageAvailability,
    };
    use tempfile::TempDir;

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
        let document = render(MeetingExportFormat::Json, &review()).unwrap();
        let text = String::from_utf8(document).unwrap();
        assert!(text.contains("\"schema_version\": 1"));
        assert!(text.contains("Design sync"));
        assert!(!text.contains("/tmp/"));
    }

    #[test]
    fn markdown_export_keeps_processing_failure_visible() {
        let document = render(MeetingExportFormat::Markdown, &review()).unwrap();
        let text = String::from_utf8(document).unwrap();
        assert!(text.contains("Capture completeness: partial"));
        assert!(text.contains("Processing: failed"));
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
