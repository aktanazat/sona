use super::people_types::{
    PersonContributionAttribution, PersonId, PersonLinkConfidence, PersonLinkSource,
};
use super::session::MeetingSessionManager;
use super::store::MeetingStore;
use crate::managers::history::HistoryManager;
use crate::scratchpad::{self, ScratchVersionKind};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PersonMemorySourceKind {
    Conversation,
    Transcript,
    MeetingNote,
    Note,
    NoteVersion,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMemorySource {
    pub kind: PersonMemorySourceKind,
    pub id: String,
    pub title: String,
    pub link: String,
    pub at_utc_ms: i64,
    pub meeting_id: Option<String>,
    pub start_offset_ns: Option<u64>,
    pub revision: Option<String>,
    pub transcript_available: Option<bool>,
    pub audio_retained: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMemoryPassage {
    pub source: PersonMemorySource,
    pub speaker_label: String,
    pub raw_text: String,
    pub corrected_text: Option<String>,
    pub edit_revision: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMemoryContribution {
    pub id: String,
    pub passage: PersonMemoryPassage,
    pub attribution: PersonContributionAttribution,
    pub before: Option<PersonMemoryPassage>,
    pub after: Option<PersonMemoryPassage>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMemoryConversation {
    pub source: PersonMemorySource,
    pub link_source: PersonLinkSource,
    pub confidence: PersonLinkConfidence,
    pub transcript_available: bool,
    pub attributed_contributions: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMemoryNote {
    pub source: PersonMemorySource,
    pub body: String,
    pub includes_ai_synthesis: bool,
    pub directly_linked: bool,
    pub related_sources: Vec<PersonMemorySource>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PersonMemoryCommitmentRelation {
    ExplicitOwner,
    AttributedSpeech,
    SharedConversation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMemoryCommitment {
    pub id: String,
    pub interpretation: String,
    pub status: super::loop_types::MeetingLoopStatus,
    pub relation: PersonMemoryCommitmentRelation,
    pub evidence: Vec<PersonMemoryPassage>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMemoryThread {
    pub id: String,
    pub title: String,
    pub interpretation: String,
    pub origin: PersonMemoryThreadOrigin,
    pub sources: Vec<PersonMemorySource>,
    pub evidence: Vec<PersonMemoryPassage>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PersonMemoryThreadOrigin {
    ExplicitSubject,
    AiTopic,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PersonMemoryChangeKind {
    NoteEdited,
    SubjectEvidenceDiffers,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMemoryComparisonSide {
    pub source: PersonMemorySource,
    pub text: String,
    pub includes_ai_interpretation: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMemoryChange {
    pub id: String,
    pub kind: PersonMemoryChangeKind,
    pub title: String,
    pub earlier: PersonMemoryComparisonSide,
    pub later: PersonMemoryComparisonSide,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMemoryCatchUp {
    pub last_conversation: Option<PersonMemorySource>,
    pub contribution_ids: Vec<String>,
    pub commitment_ids: Vec<String>,
    pub note_ids: Vec<String>,
    pub change_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PersonMemorySnapshot {
    pub schema_version: u32,
    pub person_id: PersonId,
    pub display_name: String,
    pub people_revision: u64,
    pub read_at_utc_ms: i64,
    pub since_utc_ms: Option<i64>,
    pub conversations: Vec<PersonMemoryConversation>,
    pub contributions: Vec<PersonMemoryContribution>,
    pub notes: Vec<PersonMemoryNote>,
    pub commitments: Vec<PersonMemoryCommitment>,
    pub threads: Vec<PersonMemoryThread>,
    pub changes: Vec<PersonMemoryChange>,
    pub catch_up: PersonMemoryCatchUp,
    pub limitations: Vec<String>,
}

/// `hides_meeting` names the meetings this reader may not see, by id. Their
/// rows leave before anything below links a note to them or picks the last
/// conversation.
pub(crate) async fn read(
    app: &AppHandle,
    person_id: PersonId,
    since_utc_ms: Option<i64>,
    hides_meeting: impl Fn(&str) -> bool + Send + 'static,
) -> Result<PersonMemorySnapshot, String> {
    let meetings = app.state::<Arc<MeetingSessionManager>>();
    let store = meetings
        .store()
        .await
        .map_err(|_| "Conversations are still unlocking.".to_string())?;
    let history = app.state::<Arc<HistoryManager>>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        read_stored(&store, &history, person_id, since_utc_ms, &hides_meeting)
    })
    .await
    .map_err(|_| "This person's history could not be read.".to_string())?
}

fn read_stored(
    store: &MeetingStore,
    history: &HistoryManager,
    person_id: PersonId,
    since_utc_ms: Option<i64>,
    hides_meeting: &dyn Fn(&str) -> bool,
) -> Result<PersonMemorySnapshot, String> {
    let mut snapshot = store
        .person_memory_snapshot(person_id, since_utc_ms)
        .map_err(|_| "This person's history is unavailable.".to_string())?;
    without_meetings(&mut snapshot, hides_meeting);
    let confirmed_meetings = snapshot
        .conversations
        .iter()
        .filter(|conversation| conversation.confidence == PersonLinkConfidence::Confirmed)
        .map(|conversation| (conversation.source.id.as_str(), &conversation.source))
        .collect::<HashMap<_, _>>();
    let person_source_ids = store
        .person_source_ids(person_id)
        .map_err(|_| "Related notes could not be read.".to_string())?;
    let notes = history
        .with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT note_id FROM scratch_note_sources WHERE kind = ?1 AND target_id = ?2",
            )?;
            let mut candidates = HashMap::<String, bool>::new();
            for source_id in person_source_ids {
                for id in statement
                    .query_map(params!["person", source_id], |row| row.get::<_, String>(0))?
                {
                    candidates.insert(id?, true);
                }
            }
            for meeting_id in confirmed_meetings.keys() {
                for id in statement.query_map(params!["meeting", meeting_id], |row| {
                    row.get::<_, String>(0)
                })? {
                    candidates.entry(id?).or_insert(false);
                }
            }
            drop(statement);
            let mut notes = Vec::new();
            for (id, direct) in candidates {
                let sources = scratchpad::links::sources_with_connection(connection, &id)?;
                let related_sources = sources
                    .iter()
                    .filter(|source| source.kind == scratchpad::links::ScratchSourceKind::Meeting)
                    .filter_map(|source| {
                        confirmed_meetings.get(source.id.as_str()).copied().cloned()
                    })
                    .collect::<Vec<_>>();
                if !direct && related_sources.is_empty() {
                    continue;
                }
                let Some(note) = scratchpad::note_with_connection(connection, &id)? else {
                    continue;
                };
                let synthesis: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM scratch_note_origins WHERE note_id = ?1)",
                    params![id],
                    |row| row.get(0),
                )?;
                notes.push(PersonMemoryNote {
                    source: PersonMemorySource {
                        kind: PersonMemorySourceKind::Note,
                        id: note.id.clone(),
                        title: note.title,
                        link: format!("sona://note/{}", note.id),
                        at_utc_ms: note.updated_at_ms,
                        meeting_id: None,
                        start_offset_ns: None,
                        revision: Some(note.updated_at_ms.to_string()),
                        transcript_available: None,
                        audio_retained: None,
                    },
                    body: note.body,
                    includes_ai_synthesis: synthesis,
                    directly_linked: direct,
                    related_sources,
                });
            }
            Ok(notes)
        })
        .map_err(|_| "Related notes could not be read.".to_string())?;
    snapshot.notes.extend(notes);
    snapshot.notes.sort_by(|left, right| {
        right
            .source
            .at_utc_ms
            .cmp(&left.source.at_utc_ms)
            .then_with(|| left.source.id.cmp(&right.source.id))
    });
    for note in &snapshot.notes {
        if note.source.kind != PersonMemorySourceKind::Note {
            continue;
        }
        let versions = scratchpad::read_versions(history, &note.source.id)?.unwrap_or_default();
        let Some(later) = versions.first() else {
            continue;
        };
        if since_utc_ms.is_some_and(|since| later.saved_at_ms <= since) {
            continue;
        }
        let earlier = if let Some(since) = since_utc_ms {
            versions.iter().find(|version| version.saved_at_ms <= since)
        } else {
            versions
                .iter()
                .skip(1)
                .find(|version| version.body != later.body)
        };
        let Some(earlier) = earlier.filter(|earlier| earlier.body != later.body) else {
            continue;
        };
        let side = |version: &scratchpad::ScratchNoteVersion| {
            let mut source = note.source.clone();
            source.kind = PersonMemorySourceKind::NoteVersion;
            source.revision = Some(version.id.to_string());
            source.at_utc_ms = version.saved_at_ms;
            source.link = format!("sona://note/{}?version={}", note.source.id, version.id);
            PersonMemoryComparisonSide {
                source,
                text: version.body.clone(),
                includes_ai_interpretation: note.includes_ai_synthesis
                    || version.kind == ScratchVersionKind::Synthesis,
            }
        };
        snapshot.changes.push(PersonMemoryChange {
            id: format!("note:{}:{}:{}", note.source.id, earlier.id, later.id),
            kind: PersonMemoryChangeKind::NoteEdited,
            title: note.source.title.clone(),
            earlier: side(earlier),
            later: side(later),
        });
    }
    let note_ids = snapshot
        .notes
        .iter()
        .filter(|note| note.source.kind == PersonMemorySourceKind::Note)
        .map(|note| note.source.id.as_str())
        .collect::<HashSet<_>>();
    let subjects = history.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT subject.id, subject.title, member.note_id
             FROM scratch_subjects subject JOIN scratch_subject_notes member ON member.subject_id = subject.id
             ORDER BY subject.created_at_ms DESC, subject.id, member.added_at_ms DESC"
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }).map_err(|_| "Shared subjects could not be read.".to_string())?;
    for (id, title, note_id) in subjects {
        if !note_ids.contains(note_id.as_str()) {
            continue;
        }
        let Some(note) = snapshot.notes.iter().find(|note| note.source.id == note_id) else {
            continue;
        };
        let thread_id = format!("subject:{id}");
        if let Some(thread) = snapshot
            .threads
            .iter_mut()
            .find(|thread| thread.id == thread_id)
        {
            thread.sources.push(note.source.clone());
        } else {
            snapshot.threads.insert(
                0,
                PersonMemoryThread {
                    id: thread_id,
                    title,
                    interpretation: "Linked subject".to_string(),
                    origin: PersonMemoryThreadOrigin::ExplicitSubject,
                    sources: vec![note.source.clone()],
                    evidence: Vec::new(),
                },
            );
        }
    }
    snapshot.changes.sort_by(|left, right| {
        right
            .later
            .source
            .at_utc_ms
            .cmp(&left.later.source.at_utc_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
    let last = snapshot
        .conversations
        .iter()
        .find(|conversation| conversation.confidence == PersonLinkConfidence::Confirmed);
    let last_id = last.map(|conversation| conversation.source.id.as_str());
    snapshot.catch_up = PersonMemoryCatchUp {
        last_conversation: last.map(|conversation| conversation.source.clone()),
        contribution_ids: snapshot
            .contributions
            .iter()
            .filter(|entry| entry.passage.source.meeting_id.as_deref() == last_id)
            .take(4)
            .map(|entry| entry.id.clone())
            .collect(),
        commitment_ids: snapshot
            .commitments
            .iter()
            .filter(|entry| entry.status.is_open())
            .take(4)
            .map(|entry| entry.id.clone())
            .collect(),
        note_ids: snapshot
            .notes
            .iter()
            .filter(|note| since_utc_ms.is_none_or(|since| note.source.at_utc_ms > since))
            .take(4)
            .map(|note| note.source.id.clone())
            .collect(),
        change_ids: snapshot
            .changes
            .iter()
            .take(4)
            .map(|change| change.id.clone())
            .collect(),
    };
    Ok(snapshot)
}

/// Every row of the stored snapshot is read out of one conversation and
/// carries that meeting's id, so dropping by it leaves nothing of the meeting.
fn without_meetings(snapshot: &mut PersonMemorySnapshot, hides_meeting: &dyn Fn(&str) -> bool) {
    let hidden = |source: &PersonMemorySource| {
        source
            .meeting_id
            .as_deref()
            .is_some_and(|meeting_id| hides_meeting(meeting_id))
    };
    snapshot
        .conversations
        .retain(|conversation| !hidden(&conversation.source));
    snapshot
        .contributions
        .retain(|contribution| !hidden(&contribution.passage.source));
    snapshot.notes.retain(|note| !hidden(&note.source));
    snapshot.commitments.retain(|commitment| {
        !commitment
            .evidence
            .iter()
            .any(|passage| hidden(&passage.source))
    });
    snapshot.threads.retain_mut(|thread| {
        thread.sources.retain(|source| !hidden(source));
        thread.evidence.retain(|passage| !hidden(&passage.source));
        !thread.sources.is_empty() || !thread.evidence.is_empty()
    });
    snapshot
        .changes
        .retain(|change| !hidden(&change.earlier.source) && !hidden(&change.later.source));
}

#[tauri::command]
#[specta::specta]
pub async fn person_memory(
    app: AppHandle,
    person_id: PersonId,
    since_utc_ms: Option<i64>,
) -> Result<PersonMemorySnapshot, String> {
    read(&app, person_id, since_utc_ms, |_| false).await
}
