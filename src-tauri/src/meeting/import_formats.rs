//! Readers for the transcript exports other note-takers produce.
//!
//! Each vendor's documented shape is cited beside the parser that reads it, and
//! one sample of that shape lives in `fixtures/` so a format change shows up as
//! a failing test rather than as a meeting with no transcript. Nothing here
//! reaches the network: a reader is handed bytes that are already on disk.
//!
//! Detection is by content, not by file name. An export renamed on the way out
//! of a download folder is still the same export, and the three shapes are
//! unambiguous: JSON starts with a bracket, SubRip has an arrow in its timing
//! line, and Otter's text export has a clock at the end of each turn's first
//! line.
//!
//! [`read_app_meetings`] reads a whole app's worth at once, for the exports
//! that hold many meetings or arrive as a folder of files: Granola's CSV,
//! Wispr Flow's Notetaker, and Otter's TXT, SRT and DOCX files.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The largest transcript export Sona will read. Plain text at this size is
/// tens of hours of speech, so a file past it is not a transcript, and the
/// reader holds the whole thing at once — unlike the audio path, which streams.
const MAX_TRANSCRIPT_EXPORT_BYTES: u64 = 32 * 1024 * 1024;

/// How long the last segment of a timestamped export is assumed to run when the
/// vendor states a start and no end. Only the final segment needs it: every
/// earlier one ends where the next begins.
const TRAILING_SEGMENT_MS: u64 = 2_000;

/// One vendor's export, read into the fields Sona can build a meeting from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedTranscript {
    pub title: String,
    /// When the conversation happened, if the export says. Granola stamps
    /// absolute times; Otter and Circleback are relative to their own zero.
    pub started_at_utc_ms: Option<i64>,
    pub segments: Vec<ImportedSegment>,
    /// The vendor's notes or summary, as Markdown, when the export has them.
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedSegment {
    pub speaker: Option<String>,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub text: String,
}

/// Why an export was refused. The string is for the log; every refusal reaches
/// the operator as the same typed command error, because they all have the same
/// answer: this file is not an export Sona can read, so pick another one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TranscriptImportError(pub &'static str);

/// Read one transcript export from disk. The file stem is the title for the
/// formats that do not carry one.
pub fn read_transcript_export(path: &Path) -> Result<ImportedTranscript, TranscriptImportError> {
    let metadata =
        fs::metadata(path).map_err(|_| TranscriptImportError("transcript file is unreadable"))?;
    if !metadata.file_type().is_file() {
        return Err(TranscriptImportError("transcript path is not a file"));
    }
    if metadata.len() > MAX_TRANSCRIPT_EXPORT_BYTES {
        return Err(TranscriptImportError("transcript file is too large"));
    }
    let contents =
        fs::read_to_string(path).map_err(|_| TranscriptImportError("transcript is not UTF-8"))?;
    let file_stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    parse_transcript_export(&file_stem, &contents)
}

/// The pure half: pick the reader the content matches and run it.
pub fn parse_transcript_export(
    file_stem: &str,
    contents: &str,
) -> Result<ImportedTranscript, TranscriptImportError> {
    let trimmed = contents.trim_start();
    let parsed = if trimmed.starts_with('[') || trimmed.starts_with('{') {
        parse_json_export(file_stem, trimmed)
    } else if looks_like_subrip(contents) {
        parse_subrip(file_stem, contents)
    } else {
        parse_otter_text(file_stem, contents)
    }?;
    if parsed.segments.is_empty() {
        return Err(TranscriptImportError("export contains no transcript"));
    }
    Ok(parsed)
}

/// Fill in the ends the vendor left out, and give an untimed export a synthetic
/// timeline. Offsets on an imported transcript are an ordering and anchoring
/// device — there is no audio to seek — but every citation, catch-up window and
/// analytics pass reads them, so they have to be present and strictly forward.
pub fn resolve_spans(segments: &[ImportedSegment]) -> Vec<(u64, u64)> {
    let mut spans: Vec<(u64, u64)> = Vec::with_capacity(segments.len());
    for (index, segment) in segments.iter().enumerate() {
        let previous_end = spans.last().map_or(0, |(_, end)| *end);
        let start = segment.start_ms.unwrap_or(previous_end).max(previous_end);
        let next_start = segments[index.saturating_add(1)..]
            .iter()
            .find_map(|later| later.start_ms);
        let end = segment
            .end_ms
            .or(next_start)
            .filter(|end| *end > start)
            .unwrap_or_else(|| start.saturating_add(TRAILING_SEGMENT_MS));
        spans.push((start, end));
    }
    spans
}

fn looks_like_subrip(contents: &str) -> bool {
    contents
        .lines()
        .take(8)
        .any(|line| line.contains("-->") && line.contains(','))
}

// ---------------------------------------------------------------------------
// Granola and Circleback: JSON.
// ---------------------------------------------------------------------------

/// Granola's transcript item.
/// <https://docs.granola.ai/api-reference/get-transcript>
///
/// Unknown fields are accepted on purpose: this is another product's payload,
/// and a field Granola adds must not stop an export from importing.
#[derive(Debug, Deserialize)]
struct GranolaExport {
    title: Option<String>,
    summary: Option<String>,
    transcript: Vec<GranolaItem>,
}

#[derive(Debug, Deserialize)]
struct GranolaItem {
    speaker: GranolaSpeaker,
    text: String,
    start_time: String,
    end_time: String,
}

#[derive(Debug, Deserialize)]
struct GranolaSpeaker {
    /// The resolved person, when Granola identified one.
    name: Option<String>,
    /// The anonymous diarization bucket (`Speaker A`), on mobile transcripts.
    diarization_label: Option<String>,
}

/// Circleback's transcript segment.
/// <https://circleback.ai/docs/api/meetings/get-meeting-transcript>
#[derive(Debug, Deserialize)]
struct CirclebackSegment {
    speaker: Option<String>,
    text: String,
    /// Seconds from the start of the meeting.
    timestamp: f64,
}

fn parse_json_export(
    file_stem: &str,
    contents: &str,
) -> Result<ImportedTranscript, TranscriptImportError> {
    if contents.starts_with('[') {
        let segments: Vec<CirclebackSegment> = serde_json::from_str(contents)
            .map_err(|_| TranscriptImportError("not a Circleback transcript export"))?;
        return Ok(ImportedTranscript {
            title: file_stem.to_string(),
            started_at_utc_ms: None,
            segments: segments
                .into_iter()
                .filter(|segment| !segment.text.trim().is_empty())
                .map(|segment| ImportedSegment {
                    speaker: trimmed_name(segment.speaker.as_deref()),
                    start_ms: seconds_to_ms(segment.timestamp),
                    end_ms: None,
                    text: segment.text.trim().to_string(),
                })
                .collect(),
            notes: None,
        });
    }
    let export: GranolaExport = serde_json::from_str(contents)
        .map_err(|_| TranscriptImportError("not a Granola transcript export"))?;
    // Granola stamps every item with a wall clock, so the meeting's own zero is
    // its first item and the session start is that instant.
    let anchor_ms = export
        .transcript
        .iter()
        .filter_map(|item| rfc3339_ms(&item.start_time))
        .min();
    let segments = export
        .transcript
        .into_iter()
        .filter(|item| !item.text.trim().is_empty())
        .map(|item| ImportedSegment {
            speaker: trimmed_name(
                item.speaker
                    .name
                    .as_deref()
                    .or(item.speaker.diarization_label.as_deref()),
            ),
            start_ms: offset_from(anchor_ms, &item.start_time),
            end_ms: offset_from(anchor_ms, &item.end_time),
            text: item.text.trim().to_string(),
        })
        .collect();
    Ok(ImportedTranscript {
        title: export
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or(file_stem)
            .to_string(),
        started_at_utc_ms: anchor_ms,
        segments,
        notes: trimmed_name(export.summary.as_deref()),
    })
}

fn offset_from(anchor_ms: Option<i64>, value: &str) -> Option<u64> {
    let anchor = anchor_ms?;
    u64::try_from(rfc3339_ms(value)?.checked_sub(anchor)?).ok()
}

fn rfc3339_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|stamp| stamp.timestamp_millis())
}

fn seconds_to_ms(seconds: f64) -> Option<u64> {
    (seconds.is_finite() && seconds >= 0.0).then_some((seconds * 1_000.0) as u64)
}

// ---------------------------------------------------------------------------
// Otter: SubRip and plain text.
// ---------------------------------------------------------------------------

/// Otter's SRT export, one cue per turn, with the speaker name prefixed onto
/// the cue text when "Show speaker names" is on.
/// <https://help.otter.ai/hc/en-us/articles/11742706003735-Create-captions-subtitles-for-your-video>
fn parse_subrip(
    file_stem: &str,
    contents: &str,
) -> Result<ImportedTranscript, TranscriptImportError> {
    let mut segments: Vec<ImportedSegment> = Vec::new();
    for block in paragraphs(contents) {
        let Some(timing_index) = block.iter().position(|line| line.contains("-->")) else {
            continue;
        };
        let Some((start_ms, end_ms)) = parse_subrip_timing(block[timing_index]) else {
            return Err(TranscriptImportError("SubRip cue has an unreadable timing"));
        };
        let body = block[timing_index.saturating_add(1)..].join(" ");
        let (speaker, text) = split_speaker_prefix(body.trim());
        if text.is_empty() {
            continue;
        }
        // Otter names the speaker on a turn's first cue only; the cues after
        // it are the same person until the next name.
        let speaker = speaker.or_else(|| segments.last().and_then(|last| last.speaker.clone()));
        segments.push(ImportedSegment {
            speaker,
            start_ms: Some(start_ms),
            end_ms: Some(end_ms),
            text,
        });
    }
    Ok(ImportedTranscript {
        title: file_stem.to_string(),
        started_at_utc_ms: None,
        segments,
        notes: None,
    })
}

fn parse_subrip_timing(line: &str) -> Option<(u64, u64)> {
    let (start, end) = line.split_once("-->")?;
    Some((parse_subrip_clock(start)?, parse_subrip_clock(end)?))
}

fn parse_subrip_clock(value: &str) -> Option<u64> {
    let value = value.trim();
    let (clock, millis) = match value.split_once(',').or_else(|| value.split_once('.')) {
        Some((clock, millis)) => (clock, millis.parse::<u64>().ok()?),
        None => (value, 0),
    };
    parse_clock(clock)?.checked_add(millis)
}

/// A speaker label Otter prefixes onto cue text: everything before the first
/// colon, when that prefix is short and reads like a name rather than like a
/// sentence that happens to contain one.
fn split_speaker_prefix(body: &str) -> (Option<String>, String) {
    let candidate = body
        .split_once(':')
        .filter(|(prefix, _)| {
            !prefix.is_empty()
                && prefix.len() <= 64
                && !prefix.contains(['.', '?', '!'])
                && prefix.split_whitespace().count() <= 6
        })
        .map(|(prefix, rest)| (prefix.trim().to_string(), rest.trim().to_string()));
    match candidate {
        Some((speaker, text)) if !text.is_empty() => (Some(speaker), text),
        _ => (None, body.to_string()),
    }
}

/// Otter's plain-text export: an optional title and branding header, then one
/// paragraph per turn whose first line ends with the turn's clock.
/// <https://help.otter.ai/hc/en-us/articles/360047733634-Export-conversations>
///
/// A turn is recognised by that clock. An export with both "Show speaker names"
/// and "Show timestamps" turned off is indistinguishable from any other text
/// document, and is refused rather than guessed at.
fn parse_otter_text(
    file_stem: &str,
    contents: &str,
) -> Result<ImportedTranscript, TranscriptImportError> {
    let (header, segments) = otter_turns(paragraphs(contents))?;
    Ok(ImportedTranscript {
        title: header
            .into_iter()
            .next()
            .unwrap_or_else(|| file_stem.to_string()),
        started_at_utc_ms: None,
        segments,
        notes: None,
    })
}

/// Otter's turns out of blocks whose first line may be a turn header, and the
/// first line of each block before the first turn: Otter's header, whose
/// first line is the conversation name. The branding line is neither.
fn otter_turns(
    blocks: Vec<Vec<&str>>,
) -> Result<(Vec<String>, Vec<ImportedSegment>), TranscriptImportError> {
    let mut header = Vec::new();
    let mut segments: Vec<ImportedSegment> = Vec::new();
    for block in blocks {
        let Some(head) = block.first() else { continue };
        if is_otter_branding(head) {
            continue;
        }
        match parse_turn_header(head) {
            Some((speaker, start_ms)) => {
                let text = block[1..].join(" ").trim().to_string();
                if text.is_empty() {
                    continue;
                }
                segments.push(ImportedSegment {
                    speaker,
                    start_ms: Some(start_ms),
                    end_ms: None,
                    text,
                });
            }
            None if segments.is_empty() => header.extend(trimmed_name(Some(head))),
            None => {
                if let Some(last) = segments.last_mut() {
                    last.text.push(' ');
                    last.text.push_str(block.join(" ").trim());
                }
            }
        }
    }
    if segments.is_empty() {
        return Err(TranscriptImportError("no Otter turns found in the export"));
    }
    Ok((header, segments))
}

/// `Transcribed by https://otter.ai`, at the top or the foot of an export.
fn is_otter_branding(line: &str) -> bool {
    let line = line.trim().to_ascii_lowercase();
    line.starts_with("transcribed by") && line.contains("otter.ai")
}

/// `Speaker Name  1:04` — the speaker is whatever precedes the clock, and an
/// export with speaker names off leaves the clock alone on the line.
fn parse_turn_header(line: &str) -> Option<(Option<String>, u64)> {
    let line = line.trim_end();
    let (head, clock) = line.rsplit_once(char::is_whitespace).unwrap_or(("", line));
    Some((trimmed_name(Some(head)), parse_clock(clock)?))
}

/// `M:SS` or `H:MM:SS` as milliseconds.
fn parse_clock(value: &str) -> Option<u64> {
    let mut parts = value.trim().split(':').rev();
    let seconds = parts.next()?.parse::<u64>().ok()?;
    let minutes = parts.next()?.parse::<u64>().ok()?;
    let hours = match parts.next() {
        Some(hours) => hours.parse::<u64>().ok()?,
        None => 0,
    };
    if parts.next().is_some() || seconds >= 60 || minutes >= 60 {
        return None;
    }
    hours
        .checked_mul(3_600)?
        .checked_add(minutes.checked_mul(60)?)?
        .checked_add(seconds)?
        .checked_mul(1_000)
}

/// Blank-line separated blocks, each as its non-empty lines.
fn paragraphs(contents: &str) -> Vec<Vec<&str>> {
    let mut blocks = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in contents.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(line.trim());
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

fn trimmed_name(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Whole apps: Granola's CSV, Wispr Flow's Notetaker, Otter's files.
// ---------------------------------------------------------------------------

/// The largest single file a whole-app reader holds at once. Granola puts
/// every note's transcript in one CSV, so its parts run far past one
/// transcript's size.
const MAX_APP_EXPORT_BYTES: u64 = 512 * 1024 * 1024;

/// What a meeting with no title in its export is called.
const UNTITLED_MEETING: &str = "Untitled meeting";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingApp {
    Granola,
    WisprFlow,
    Otter,
}

impl MeetingApp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Granola => "granola",
            Self::WisprFlow => "wispr_flow",
            Self::Otter => "otter",
        }
    }
}

/// One meeting out of an app's export, with the app's own id for it, so a
/// second import of the same export adds only what is new.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppMeeting {
    pub source_key: String,
    pub transcript: ImportedTranscript,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AppMeetings {
    pub meetings: Vec<AppMeeting>,
    /// Meetings or files in the export Sona could not make a meeting from:
    /// no transcript, or a file that is not one of the app's exports.
    pub unreadable: u64,
}

/// What one app's import made.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
pub struct MeetingAppImportResult {
    pub imported: u64,
    /// Brought in by an earlier import of the same export.
    pub already_imported: u64,
    pub unreadable: u64,
}

/// Read every meeting in one app's export at `path`: Granola's CSV (or the
/// folder its zip unpacks to), Wispr Flow's data folder or its
/// `flow.sqlite`, or one Otter export or a folder of them.
pub fn read_app_meetings(
    app: MeetingApp,
    path: &Path,
) -> Result<AppMeetings, TranscriptImportError> {
    match app {
        MeetingApp::Granola => read_granola_csv(path),
        MeetingApp::WisprFlow => read_wispr_notetaker(path),
        MeetingApp::Otter => read_otter_files(path),
    }
}

/// `path` itself when it is a file, else its files with one of `extensions`,
/// in name order.
fn export_files(path: &Path, extensions: &[&str]) -> Result<Vec<PathBuf>, TranscriptImportError> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let entries =
        fs::read_dir(path).map_err(|_| TranscriptImportError("export folder is unreadable"))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|file| file.is_file() && has_extension(file, extensions))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(TranscriptImportError("no export files in the folder"));
    }
    Ok(files)
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|wanted| extension.eq_ignore_ascii_case(wanted))
        })
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, TranscriptImportError> {
    let metadata =
        fs::metadata(path).map_err(|_| TranscriptImportError("export file is unreadable"))?;
    if metadata.len() > MAX_APP_EXPORT_BYTES {
        return Err(TranscriptImportError("export file is too large"));
    }
    fs::read(path).map_err(|_| TranscriptImportError("export file is unreadable"))
}

/// The non-empty parts, one paragraph each.
fn join_notes<'a>(parts: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    let parts: Vec<&str> = parts
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Granola's CSV export, from Settings → Profile → Generate CSV: one row per
/// note that has a summary, emailed as one CSV or a zip of CSV parts.
/// <https://docs.granola.ai/help-center/sharing/exporting-notes>
///
/// Granola documents the export but not its columns. These are the ones the
/// importers that checked real exports read: `document_id, user_email,
/// document_title, workspace_name, document_created, summary, notes,
/// transcript`.
/// <https://github.com/OpenWhispr/openwhispr/blob/main/src/helpers/granolaImport.js>
///
/// The transcript has no timings, and its lines carry a `Me:` / `Them:` /
/// name prefix only sometimes. `notes` is what the person typed and
/// `summary` Granola's own notes; both become the meeting's notes.
fn read_granola_csv(path: &Path) -> Result<AppMeetings, TranscriptImportError> {
    let mut read = AppMeetings::default();
    for file in export_files(path, &["csv"])? {
        let bytes = read_bounded(&file)?;
        let contents = String::from_utf8(bytes)
            .map_err(|_| TranscriptImportError("Granola CSV is not UTF-8"))?;
        parse_granola_csv(&contents, &mut read)?;
    }
    Ok(read)
}

fn parse_granola_csv(contents: &str, read: &mut AppMeetings) -> Result<(), TranscriptImportError> {
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(contents.as_bytes());
    let headers = reader
        .headers()
        .map_err(|_| TranscriptImportError("not a Granola CSV export"))?
        .clone();
    let column = |name: &str| headers.iter().position(|header| header.trim() == name);
    let (Some(id), Some(transcript)) = (column("document_id"), column("transcript")) else {
        return Err(TranscriptImportError("not a Granola CSV export"));
    };
    let (title, created, summary, notes) = (
        column("document_title"),
        column("document_created"),
        column("summary"),
        column("notes"),
    );
    for record in reader.records() {
        let Ok(record) = record else {
            read.unreadable = read.unreadable.saturating_add(1);
            continue;
        };
        let field = |index: Option<usize>| {
            index
                .and_then(|index| record.get(index))
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let segments = field(Some(transcript))
            .map(untimed_lines)
            .unwrap_or_default();
        let Some(source_key) = field(Some(id)).filter(|_| !segments.is_empty()) else {
            read.unreadable = read.unreadable.saturating_add(1);
            continue;
        };
        read.meetings.push(AppMeeting {
            source_key: source_key.to_string(),
            transcript: ImportedTranscript {
                title: field(title).unwrap_or(UNTITLED_MEETING).to_string(),
                started_at_utc_ms: field(created).and_then(rfc3339_ms),
                segments,
                notes: join_notes([field(notes), field(summary)]),
            },
        });
    }
    Ok(())
}

/// One segment per line, with the speaker when the line names one.
fn untimed_lines(text: &str) -> Vec<ImportedSegment> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (speaker, text) = split_speaker_prefix(line);
            ImportedSegment {
                speaker,
                start_ms: None,
                end_ms: None,
                text,
            }
        })
        .collect()
}

/// Wispr Flow's Notetaker, in the same `flow.sqlite` as its dictations: the
/// `Meetings` table, and each meeting's transcript beside the database in
/// `meetings/<id>/refined.ndjson`, or `live.ndjson` before Wispr refined it.
/// <https://github.com/iamnikolie/wispr-cli> (`internal/store/models.go`,
/// `internal/store/transcript.go`)
///
/// Wispr documents no file export for Notetaker; these are the files it keeps
/// on this Mac. Its tour's demo meeting is not the person's and is left out.
fn read_wispr_notetaker(path: &Path) -> Result<AppMeetings, TranscriptImportError> {
    let (database, root) = if path.is_dir() {
        (path.join("flow.sqlite"), path.to_path_buf())
    } else {
        let root = path.parent().map(Path::to_path_buf).unwrap_or_default();
        (path.to_path_buf(), root)
    };
    if !database.is_file() {
        return Err(TranscriptImportError("no Wispr Flow database"));
    }
    let connection = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| TranscriptImportError("Wispr Flow database is unreadable"))?;
    let present = table_columns(&connection, "Meetings")
        .map_err(|_| TranscriptImportError("Wispr Flow database is unreadable"))?;
    if !present.iter().any(|column| column == "id") {
        return Err(TranscriptImportError("no Wispr Flow Meetings table"));
    }
    let column = |name: &str| {
        if present.iter().any(|column| column == name) {
            format!("\"{name}\"")
        } else {
            "NULL".to_string()
        }
    };
    let select = format!(
        "SELECT id, {title}, {created}, {summary}, {notes}, {speakers}
         FROM Meetings
         WHERE COALESCE({deleted}, 0) = 0 AND COALESCE({tour}, 0) = 0
         ORDER BY {created}, id",
        title = column("title"),
        created = column("createdAt"),
        summary = column("summary"),
        notes = column("notes"),
        speakers = column("speakerMap"),
        deleted = column("isDeleted"),
        tour = column("isTourDemo"),
    );
    let unreadable = |_| TranscriptImportError("Wispr Flow database is unreadable");
    let mut statement = connection.prepare(&select).map_err(unreadable)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(unreadable)?;
    let mut read = AppMeetings::default();
    for row in rows {
        let (id, title, created, summary, notes, speaker_map) = row.map_err(unreadable)?;
        let speakers = wispr_speaker_names(speaker_map.as_deref());
        let segments = wispr_transcript(&root, &id, &speakers);
        if segments.is_empty() {
            read.unreadable = read.unreadable.saturating_add(1);
            continue;
        }
        let notes = join_notes([notes.as_deref(), summary.as_deref()])
            .map(|notes| resolve_wispr_mentions(&notes, &speakers));
        read.meetings.push(AppMeeting {
            transcript: ImportedTranscript {
                title: trimmed_name(title.as_deref())
                    .unwrap_or_else(|| UNTITLED_MEETING.to_string()),
                started_at_utc_ms: created.as_deref().and_then(wispr_timestamp_ms),
                segments,
                notes,
            },
            source_key: id,
        });
    }
    Ok(read)
}

fn table_columns(connection: &rusqlite::Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement =
        connection.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect();
    names
}

/// Wispr Flow's stamps, `2026-06-05 07:34:51.566 +00:00`, with the readers'
/// fallbacks: no fraction, no zone (UTC), or RFC 3339.
pub(crate) fn wispr_timestamp_ms(value: &str) -> Option<i64> {
    let value = value.trim();
    chrono::DateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f %:z")
        .map(|stamp| stamp.timestamp_millis())
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|stamp| stamp.and_utc().timestamp_millis())
        })
        .or_else(|| rfc3339_ms(value))
}

/// Speaker number to name, from `speakerMap`: `{"people": {pid: {"name"}},
/// "assignments": {sid: {"user" | "consensus" | "mic": pid}}}`, the person's
/// own assignment first.
fn wispr_speaker_names(speakers_json: Option<&str>) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let Some(map) =
        speakers_json.and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
    else {
        return names;
    };
    let Some(assignments) = map.get("assignments").and_then(|value| value.as_object()) else {
        return names;
    };
    for (speaker, assignment) in assignments {
        let name = ["user", "consensus", "mic"].iter().find_map(|key| {
            let person = json_key(assignment.get(*key)?)?;
            let name = map.get("people")?.get(&person)?.get("name")?.as_str()?;
            trimmed_name(Some(name))
        });
        if let Some(name) = name {
            names.insert(speaker.clone(), name);
        }
    }
    names
}

fn json_key(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn wispr_speaker(number: &str, names: &HashMap<String, String>) -> String {
    names
        .get(number)
        .cloned()
        .unwrap_or_else(|| format!("Speaker {number}"))
}

#[derive(Deserialize)]
struct WisprUtterance {
    text: Option<String>,
    timestamp: Option<String>,
    speaker: Option<serde_json::Value>,
}

/// The refined transcript when there is one, else the live one. A line with
/// no text (the live file's `{"meta": …}` header) is not an utterance.
fn wispr_transcript(
    root: &Path,
    meeting_id: &str,
    names: &HashMap<String, String>,
) -> Vec<ImportedSegment> {
    if Path::new(meeting_id).file_name() != Some(std::ffi::OsStr::new(meeting_id)) {
        return Vec::new();
    }
    let folder = root.join("meetings").join(meeting_id);
    let Some(contents) = ["refined.ndjson", "live.ndjson"]
        .iter()
        .find_map(|name| read_bounded(&folder.join(name)).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
    else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| serde_json::from_str::<WisprUtterance>(line).ok())
        .filter_map(|utterance| {
            let text = trimmed_name(utterance.text.as_deref())?;
            let speaker = utterance.speaker.and_then(|speaker| match speaker {
                serde_json::Value::Object(speaker) => speaker
                    .get("name")
                    .and_then(|name| trimmed_name(name.as_str()))
                    .or_else(|| Some(wispr_speaker(&json_key(speaker.get("id")?)?, names))),
                serde_json::Value::String(name) => trimmed_name(Some(&name)),
                serde_json::Value::Number(number) => {
                    Some(wispr_speaker(&number.to_string(), names))
                }
                _ => None,
            });
            Some(ImportedSegment {
                speaker,
                start_ms: utterance.timestamp.as_deref().and_then(parse_clock),
                end_ms: None,
                text,
            })
        })
        .collect()
}

/// Wispr's notes name speakers as `<@speaker:N>`.
fn resolve_wispr_mentions(notes: &str, names: &HashMap<String, String>) -> String {
    const OPEN: &str = "<@speaker:";
    let mut resolved = String::with_capacity(notes.len());
    let mut rest = notes;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find('>') else { break };
        resolved.push_str(&rest[..start]);
        resolved.push_str(&wispr_speaker(&after[..end], names));
        rest = &after[end + 1..];
    }
    resolved.push_str(rest);
    resolved
}

/// Otter's per-conversation exports, one file or a folder of them (a bulk
/// export, unzipped): TXT, SRT and DOCX. Otter puts no id in a file, so a
/// file's contents are its id.
/// <https://help.otter.ai/hc/en-us/articles/360047733634-Export-conversations>
/// <https://help.otter.ai/hc/en-us/articles/13829342669079-Bulk-export-conversations-audio-or-Takeaways>
fn read_otter_files(path: &Path) -> Result<AppMeetings, TranscriptImportError> {
    let mut read = AppMeetings::default();
    for file in export_files(path, &["txt", "srt", "docx"])? {
        let stem = file
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Otter names a file `<Title>_otter_ai.<ext>`.
        let stem = stem.strip_suffix("_otter_ai").unwrap_or(&stem).to_string();
        let parsed = read_bounded(&file).and_then(|bytes| {
            let transcript = if has_extension(&file, &["docx"]) {
                parse_otter_docx(&stem, &bytes)
            } else {
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| TranscriptImportError("transcript is not UTF-8"))?;
                parse_transcript_export(&stem, text)
            }?;
            Ok((
                crate::fs_util::hex_digest(&Sha256::digest(&bytes)),
                transcript,
            ))
        });
        match parsed {
            Ok((source_key, transcript)) => read.meetings.push(AppMeeting {
                source_key,
                transcript,
            }),
            Err(error) => {
                log::warn!("Otter import skipped a file: {}", error.0);
                read.unreadable = read.unreadable.saturating_add(1);
            }
        }
    }
    Ok(read)
}

/// Otter's DOCX export, as Otter's own example shows it: the title, a line
/// with the date, start time and length (`Fri, Feb 20, 2026 3:34PM • 1:38`),
/// the SUMMARY KEYWORDS and SPEAKERS lists, then a `Speaker  0:00` paragraph
/// per turn followed by its words, and the branding line.
/// <https://help.otter.ai/hc/en-us/articles/360047733634-Export-conversations>
fn parse_otter_docx(
    file_stem: &str,
    bytes: &[u8],
) -> Result<ImportedTranscript, TranscriptImportError> {
    let paragraphs = docx_paragraphs(bytes)?;
    let mut started_at_utc_ms = None;
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut in_turns = false;
    for line in paragraphs.iter().map(|paragraph| paragraph.trim()) {
        if line.is_empty() || is_otter_branding(line) {
            continue;
        }
        if !in_turns && started_at_utc_ms.is_none() {
            if let Some(started) = otter_date_line(line) {
                started_at_utc_ms = Some(started);
                continue;
            }
        }
        // The paragraph after a turn's header is its words, whatever they
        // end with: "We can meet at 3:30" is speech, not a turn.
        let awaiting_words = in_turns && blocks.last().is_some_and(|block| block.len() == 1);
        if !awaiting_words && parse_turn_header(line).is_some() {
            in_turns = true;
            blocks.push(vec![line]);
        } else if let (true, Some(block)) = (in_turns, blocks.last_mut()) {
            block.push(line);
        } else {
            blocks.push(vec![line]);
        }
    }
    let (header, segments) = otter_turns(blocks)?;
    Ok(ImportedTranscript {
        title: header
            .into_iter()
            .next()
            .unwrap_or_else(|| file_stem.to_string()),
        started_at_utc_ms,
        segments,
        notes: None,
    })
}

/// `Fri, Feb 20, 2026 3:34PM • 1:38`: the local start, as milliseconds.
fn otter_date_line(line: &str) -> Option<i64> {
    let (when, _) = line.split_once('•')?;
    let naive = chrono::NaiveDateTime::parse_from_str(when.trim(), "%a, %b %d, %Y %I:%M%p").ok()?;
    naive
        .and_local_timezone(chrono::Local)
        .earliest()
        .map(|stamp| stamp.timestamp_millis())
}

/// The paragraphs of a DOCX file's body, as text.
fn docx_paragraphs(bytes: &[u8]) -> Result<Vec<String>, TranscriptImportError> {
    use quick_xml::events::Event;
    let not_docx = || TranscriptImportError("not a DOCX file");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| not_docx())?;
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .map_err(|_| not_docx())?
        .take(MAX_TRANSCRIPT_EXPORT_BYTES)
        .read_to_string(&mut xml)
        .map_err(|_| not_docx())?;
    let mut reader = quick_xml::Reader::from_str(&xml);
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event().map_err(|_| not_docx())? {
            Event::Start(element) if element.name().as_ref() == b"w:t" => in_text = true,
            Event::End(element) => match element.name().as_ref() {
                b"w:t" => in_text = false,
                b"w:p" => paragraphs.push(std::mem::take(&mut current)),
                _ => {}
            },
            Event::Empty(element) => match element.name().as_ref() {
                b"w:tab" => current.push('\t'),
                b"w:br" => current.push(' '),
                _ => {}
            },
            Event::Text(text) if in_text => {
                current.push_str(&text.decode().map_err(|_| not_docx())?);
            }
            Event::GeneralRef(reference) if in_text => {
                if let Some(character) = reference.resolve_char_ref().map_err(|_| not_docx())? {
                    current.push(character);
                } else {
                    let name = reference.decode().map_err(|_| not_docx())?;
                    current.push_str(
                        quick_xml::escape::resolve_predefined_entity(&name).ok_or_else(not_docx)?,
                    );
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(paragraphs)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OTTER_TEXT: &str = include_str!("fixtures/otter_export.txt");
    const OTTER_SRT: &str = include_str!("fixtures/otter_export.srt");
    const GRANOLA: &str = include_str!("fixtures/granola_note.json");
    const CIRCLEBACK: &str = include_str!("fixtures/circleback_transcript.json");

    #[test]
    fn otter_text_export_keeps_speakers_and_turn_clocks() {
        let parsed = parse_transcript_export("otter_export", OTTER_TEXT).unwrap();

        assert_eq!(parsed.title, "Weekly product sync");
        assert_eq!(parsed.started_at_utc_ms, None);
        assert_eq!(
            parsed.segments,
            vec![
                ImportedSegment {
                    speaker: Some("Priya Raman".to_string()),
                    start_ms: Some(2_000),
                    end_ms: None,
                    text:
                        "Let's start with the pricing page. The new tiers are live behind a flag."
                            .to_string(),
                },
                ImportedSegment {
                    speaker: Some("Tom Alvarez".to_string()),
                    start_ms: Some(19_000),
                    end_ms: None,
                    text: "I can flip the flag on Thursday once the copy review is done."
                        .to_string(),
                },
                ImportedSegment {
                    speaker: Some("Priya Raman".to_string()),
                    start_ms: Some(64_000),
                    end_ms: None,
                    text: "Thursday works. I'll write the changelog entry today.".to_string(),
                },
            ]
        );
    }

    #[test]
    fn otter_srt_export_joins_wrapped_cue_lines() {
        let parsed = parse_transcript_export("otter_export", OTTER_SRT).unwrap();

        assert_eq!(
            parsed.segments,
            vec![
                ImportedSegment {
                    speaker: Some("Priya Raman".to_string()),
                    start_ms: Some(2_000),
                    end_ms: Some(11_480),
                    text: "Let's start with the pricing page.".to_string(),
                },
                ImportedSegment {
                    speaker: Some("Tom Alvarez".to_string()),
                    start_ms: Some(19_120),
                    end_ms: Some(24_600),
                    text: "I can flip the flag on Thursday once the copy review is done."
                        .to_string(),
                },
                ImportedSegment {
                    speaker: Some("Priya Raman".to_string()),
                    start_ms: Some(64_000),
                    end_ms: Some(69_250),
                    text: "Thursday works.".to_string(),
                },
            ]
        );
    }

    #[test]
    fn granola_export_anchors_absolute_stamps_to_the_first_item() {
        let parsed = parse_transcript_export("granola_note", GRANOLA).unwrap();

        assert_eq!(parsed.title, "Quarterly yoghurt budget review");
        assert_eq!(parsed.started_at_utc_ms, Some(1_775_660_404_000));
        assert_eq!(
            parsed.notes.as_deref(),
            Some("The quarterly yoghurt budget review was a success.")
        );
        assert_eq!(
            parsed.segments,
            vec![
                ImportedSegment {
                    speaker: Some("Oat Benson".to_string()),
                    start_ms: Some(0),
                    end_ms: Some(5_500),
                    text: "I'm done pretending. Greek is the only yoghurt that deserves us."
                        .to_string(),
                },
                ImportedSegment {
                    speaker: Some("Speaker B".to_string()),
                    start_ms: Some(6_250),
                    end_ms: Some(10_000),
                    text: "Finally. Regular yoghurt is just milk that gave up halfway.".to_string(),
                },
            ]
        );
    }

    #[test]
    fn circleback_export_carries_a_null_speaker_through() {
        let parsed = parse_transcript_export("circleback_transcript", CIRCLEBACK).unwrap();

        assert_eq!(parsed.title, "circleback_transcript");
        assert_eq!(
            parsed.segments,
            vec![
                ImportedSegment {
                    speaker: Some("John Appleseed".to_string()),
                    start_ms: Some(4_560),
                    end_ms: None,
                    text: "Hey, how's it going?".to_string(),
                },
                ImportedSegment {
                    speaker: Some("Samantha Grey".to_string()),
                    start_ms: Some(18_320),
                    end_ms: None,
                    text: "Going well, just testing the Circleback API.".to_string(),
                },
                ImportedSegment {
                    speaker: None,
                    start_ms: Some(32_000),
                    end_ms: None,
                    text: "Sorry, joining late.".to_string(),
                },
            ]
        );
    }

    #[test]
    fn prose_with_no_turns_is_refused() {
        assert!(parse_transcript_export(
            "notes",
            "Some notes I typed after the call.\n\nThey are not a transcript.\n",
        )
        .is_err());
        assert!(parse_transcript_export("empty", "").is_err());
        assert!(parse_transcript_export("truncated", "{\"transcript\": [").is_err());
        assert!(parse_transcript_export("wrong-json", "[{\"line\": \"hello\"}]").is_err());
    }

    #[test]
    fn a_cue_with_a_broken_clock_refuses_the_whole_file() {
        assert_eq!(
            parse_transcript_export("otter", "1\n00:00:02,000 --> later\nHello.\n"),
            Err(TranscriptImportError("SubRip cue has an unreadable timing")),
        );
    }

    /// Every span moves forward and none is empty, whatever the vendor left
    /// out: `append_transcript_segments` rejects a segment that does not.
    #[test]
    fn spans_stay_forward_when_ends_are_missing() {
        let spans = resolve_spans(&[
            ImportedSegment {
                speaker: None,
                start_ms: Some(4_560),
                end_ms: None,
                text: "one".to_string(),
            },
            ImportedSegment {
                speaker: None,
                start_ms: None,
                end_ms: None,
                text: "two".to_string(),
            },
            ImportedSegment {
                speaker: None,
                start_ms: Some(18_320),
                end_ms: None,
                text: "three".to_string(),
            },
        ]);

        assert_eq!(
            spans,
            vec![(4_560, 18_320), (18_320, 20_320), (20_320, 22_320)]
        );
        assert!(spans.windows(2).all(|pair| pair[0].1 <= pair[1].0));
        assert!(spans.iter().all(|(start, end)| start < end));
    }

    /// A vendor that hands back an out-of-order or duplicated stamp must not
    /// produce a segment the store refuses.
    #[test]
    fn spans_absorb_a_stamp_that_goes_backwards() {
        let spans = resolve_spans(&[
            ImportedSegment {
                speaker: None,
                start_ms: Some(10_000),
                end_ms: Some(12_000),
                text: "one".to_string(),
            },
            ImportedSegment {
                speaker: None,
                start_ms: Some(4_000),
                end_ms: Some(6_000),
                text: "two".to_string(),
            },
        ]);

        assert_eq!(spans, vec![(10_000, 12_000), (12_000, 14_000)]);
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/meeting/fixtures")
            .join(name)
    }

    fn segment(
        speaker: Option<&str>,
        start_ms: Option<u64>,
        end_ms: Option<u64>,
        text: &str,
    ) -> ImportedSegment {
        ImportedSegment {
            speaker: speaker.map(str::to_string),
            start_ms,
            end_ms,
            text: text.to_string(),
        }
    }

    #[test]
    fn granola_csv_keeps_titles_dates_speakers_and_both_kinds_of_notes() {
        let read = read_app_meetings(MeetingApp::Granola, &fixture("granola_export.csv"))
            .expect("read Granola CSV");

        assert_eq!(
            read.meetings,
            vec![
                AppMeeting {
                    source_key: "00000000-0000-4000-8000-000000000001".to_string(),
                    transcript: ImportedTranscript {
                        title: "Pricing, packaging review".to_string(),
                        started_at_utc_ms: Some(1_792_852_480_866),
                        segments: vec![
                            segment(Some("Me"), None, None, "Morning, everyone."),
                            segment(Some("Them"), None, None, "Sounds right to me."),
                        ],
                        notes: Some(
                            "Ask about annual billing.\n\n### Decisions\n- Keep the Team plan at \"fifteen\" per seat"
                                .to_string()
                        ),
                    },
                },
                AppMeeting {
                    source_key: "00000000-0000-4000-8000-000000000002".to_string(),
                    transcript: ImportedTranscript {
                        title: UNTITLED_MEETING.to_string(),
                        started_at_utc_ms: Some(1_793_002_500_000),
                        segments: vec![
                            segment(None, None, None, "Hello there."),
                            segment(None, None, None, "Great, thanks."),
                        ],
                        notes: Some("**Action Items:**\n1. Rebook the room".to_string()),
                    },
                },
            ]
        );
        assert_eq!(read.unreadable, 1);
    }

    #[test]
    fn a_csv_without_granola_columns_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("contacts.csv");
        fs::write(&file, "name,email\nAna,ana@example.test\n").expect("write csv");
        assert!(read_app_meetings(MeetingApp::Granola, &file).is_err());
    }

    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).expect("create dir");
        for entry in fs::read_dir(from).expect("read dir") {
            let path = entry.expect("entry").path();
            let target = to.join(path.file_name().expect("name"));
            if path.is_dir() {
                copy_dir(&path, &target);
            } else {
                fs::copy(&path, &target).expect("copy");
            }
        }
    }

    #[test]
    fn wispr_notetaker_resolves_speakers_and_skips_demo_deleted_and_empty_meetings() {
        let dir = tempfile::tempdir().expect("temp dir");
        copy_dir(&fixture("wispr_flow_notetaker"), dir.path());
        rusqlite::Connection::open(dir.path().join("flow.sqlite"))
            .expect("open db")
            .execute_batch(&fs::read_to_string(dir.path().join("flow.sql")).expect("read sql"))
            .expect("build db");

        let read = read_app_meetings(MeetingApp::WisprFlow, dir.path()).expect("read Wispr");

        assert_eq!(
            read.meetings,
            vec![
                AppMeeting {
                    source_key: "aaaa1111-0000-0000-0000-000000000001".to_string(),
                    transcript: ImportedTranscript {
                        title: "Planning sync".to_string(),
                        started_at_utc_ms: Some(1_790_071_200_000),
                        segments: vec![
                            segment(Some("Ana Lee"), Some(0), None, "Hi all."),
                            segment(Some("Speaker 3"), Some(5_000), None, "Hello."),
                        ],
                        notes: Some(
                            "Bring the launch checklist.\n\nDecided to ship Friday. Ana Lee owns the release notes."
                                .to_string()
                        ),
                    },
                },
                AppMeeting {
                    source_key: "aaaa1111-0000-0000-0000-000000000002".to_string(),
                    transcript: ImportedTranscript {
                        title: UNTITLED_MEETING.to_string(),
                        started_at_utc_ms: Some(1_790_177_400_000),
                        segments: vec![
                            segment(Some("Sam"), Some(62_000), None, "Can you hear me?"),
                            segment(Some("Speaker 4"), Some(69_000), None, "Yes, loud and clear."),
                        ],
                        notes: None,
                    },
                },
            ]
        );
        assert_eq!(read.unreadable, 1);
    }

    #[test]
    fn otter_folder_reads_docx_srt_and_txt_and_counts_what_is_not_an_export() {
        let read = read_app_meetings(MeetingApp::Otter, &fixture("otter_folder"))
            .expect("read Otter folder");
        let started = chrono::NaiveDate::from_ymd_opt(2026, 2, 20)
            .and_then(|day| day.and_hms_opt(15, 34, 0))
            .and_then(|at| at.and_local_timezone(chrono::Local).earliest())
            .map(|at| at.timestamp_millis());

        let transcripts: Vec<&ImportedTranscript> = read
            .meetings
            .iter()
            .map(|meeting| &meeting.transcript)
            .collect();
        assert_eq!(
            transcripts,
            vec![
                &ImportedTranscript {
                    title: "Sales Weekly Call".to_string(),
                    started_at_utc_ms: started,
                    segments: vec![
                        segment(
                            Some("Gemma Allen"),
                            Some(0),
                            None,
                            "And earlier, I have a question about the roadmap & pricing."
                        ),
                        segment(Some("Speaker 1"), Some(40_000), None, "We can meet at 3:30"),
                    ],
                    notes: None,
                },
                &ImportedTranscript {
                    title: "Sales Weekly Call".to_string(),
                    started_at_utc_ms: None,
                    segments: vec![
                        segment(
                            Some("Gemma Allen"),
                            Some(96),
                            Some(3_036),
                            "And earlier, I have"
                        ),
                        segment(
                            Some("Gemma Allen"),
                            Some(3_096),
                            Some(6_576),
                            "a question about the roadmap."
                        ),
                        segment(
                            Some("Speaker 1"),
                            Some(40_000),
                            Some(43_500),
                            "We have been talking about that for years."
                        ),
                    ],
                    notes: None,
                },
                &ImportedTranscript {
                    title: "Sales Weekly Call".to_string(),
                    started_at_utc_ms: None,
                    segments: vec![
                        segment(
                            Some("Gemma Allen"),
                            Some(0),
                            None,
                            "And earlier, I have a question about the roadmap."
                        ),
                        segment(
                            Some("Speaker 1"),
                            Some(40_000),
                            None,
                            "We have been talking about that for years."
                        ),
                    ],
                    notes: None,
                },
            ]
        );
        assert_eq!(read.unreadable, 1);
        let keys: std::collections::HashSet<&str> = read
            .meetings
            .iter()
            .map(|meeting| meeting.source_key.as_str())
            .collect();
        assert_eq!(keys.len(), 3);
    }
}
