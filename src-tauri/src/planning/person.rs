//! Explicit private task selections and confirmed conversation context for a live person.

use super::sources;
use super::types::{
    EvidenceLink, EvidenceSourceKind, EvidenceTargetKind, PlanningTask, ProjectStatus, TaskStatus,
};
use crate::managers::history::HistoryManager;
use crate::meeting::loop_types::MeetingLoopId;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::loops;
use anyhow::Result as StorageResult;
use rusqlite::{Connection, Row};
use serde::Serialize;
use specta::Type;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tauri::State;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PersonWorkIdentityStatus {
    Available,
    Merged,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PersonWorkRelation {
    ExplicitSelection,
    ConfirmedConversation,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PersonWorkProject {
    pub id: String,
    pub title: String,
    pub status: ProjectStatus,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PersonWorkMilestone {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PersonWorkTask {
    pub task: PlanningTask,
    pub selected_person_id: String,
    pub project: PersonWorkProject,
    pub milestone: Option<PersonWorkMilestone>,
    pub relation: PersonWorkRelation,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PersonWorkConversation {
    pub id: String,
    pub project: PersonWorkProject,
    pub target_kind: EvidenceTargetKind,
    pub target_id: String,
    pub target_title: String,
    pub meeting_id: String,
    pub meeting_title: String,
    pub meeting_started_at_ms: Option<i64>,
    pub at_ms: Option<i64>,
    pub revision: i64,
    pub relation: PersonWorkRelation,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PersonWorkSnapshot {
    pub person_id: String,
    pub canonical_person_id: Option<String>,
    pub identity_status: PersonWorkIdentityStatus,
    pub tasks: Vec<PersonWorkTask>,
    pub conversations: Vec<PersonWorkConversation>,
}

/// Reads saved private work without treating conversation context as project membership.
#[tauri::command]
#[specta::specta]
pub async fn planning_person_work(
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    person_id: String,
) -> Result<PersonWorkSnapshot, String> {
    let person_id = super::store::uuid(&person_id).map_err(super::failure)?;
    super::with_sources(&history_state, &meetings, |history, meetings| {
        read(history, meetings, &person_id)
    })
    .await
}

pub(crate) fn read(
    history: &Connection,
    meetings: &Connection,
    person_id: &str,
) -> StorageResult<PersonWorkSnapshot> {
    let canonical_person_id = sources::person(meetings, person_id)?;
    let identity_status = match canonical_person_id.as_deref() {
        None => PersonWorkIdentityStatus::Unavailable,
        Some(id) if id != person_id => PersonWorkIdentityStatus::Merged,
        Some(_) => PersonWorkIdentityStatus::Available,
    };
    let mut snapshot = PersonWorkSnapshot {
        person_id: person_id.to_owned(),
        canonical_person_id,
        identity_status,
        tasks: Vec::new(),
        conversations: Vec::new(),
    };
    if identity_status != PersonWorkIdentityStatus::Available {
        return Ok(snapshot);
    }
    snapshot.tasks = selected_tasks(history, meetings, person_id)?;
    snapshot.conversations = confirmed_conversations(history, meetings, person_id)?;
    Ok(snapshot)
}

fn selected_tasks(
    history: &Connection,
    meetings: &Connection,
    person_id: &str,
) -> StorageResult<Vec<PersonWorkTask>> {
    let mut identities = meetings.prepare(
        "SELECT id FROM persons WHERE id = ?1
         UNION ALL
         SELECT source_person_id FROM person_merge_redirects WHERE target_person_id = ?1",
    )?;
    let identities = identities.query_map([person_id], |row| row.get::<_, String>(0))?;
    let mut statement = history.prepare(
        "SELECT t.id, t.project_id, t.milestone_id, t.title, t.details, t.due_at_ms,
                t.owner_person_id, t.owner_is_self, t.status, t.revision,
                t.created_at_ms, t.updated_at_ms,
                p.title, p.status, p.revision, m.id, m.title, m.status, m.revision
         FROM planning_tasks t
         JOIN planning_projects p ON p.id = t.project_id
         LEFT JOIN planning_milestones m ON m.id = t.milestone_id AND m.project_id = t.project_id
         WHERE t.owner_person_id = ?1 AND t.owner_is_self = 0",
    )?;
    let mut tasks = Vec::new();
    for identity in identities {
        let identity = identity?;
        let rows = statement.query_map([identity], |row| read_task(row, person_id))?;
        for row in rows {
            tasks.push(row?);
        }
    }
    tasks.sort_by(|left, right| {
        left.task
            .due_at_ms
            .is_none()
            .cmp(&right.task.due_at_ms.is_none())
            .then_with(|| left.task.due_at_ms.cmp(&right.task.due_at_ms))
            .then_with(|| left.task.created_at_ms.cmp(&right.task.created_at_ms))
            .then_with(|| left.task.id.cmp(&right.task.id))
    });
    Ok(tasks)
}

fn read_task(row: &Row<'_>, person_id: &str) -> rusqlite::Result<PersonWorkTask> {
    let project_id: String = row.get(1)?;
    let milestone = match row.get::<_, Option<String>>(15)? {
        Some(id) => Some(PersonWorkMilestone {
            id,
            title: row.get(16)?,
            status: row.get(17)?,
            revision: row.get(18)?,
        }),
        None => None,
    };
    Ok(PersonWorkTask {
        task: PlanningTask {
            id: row.get(0)?,
            project_id: project_id.clone(),
            milestone_id: row.get(2)?,
            title: row.get(3)?,
            details: row.get(4)?,
            due_at_ms: row.get(5)?,
            owner_person_id: Some(person_id.to_owned()),
            owner_is_self: row.get(7)?,
            owner_available: true,
            status: row.get(8)?,
            revision: row.get(9)?,
            created_at_ms: row.get(10)?,
            updated_at_ms: row.get(11)?,
        },
        selected_person_id: row.get(6)?,
        project: PersonWorkProject {
            id: project_id,
            title: row.get(12)?,
            status: row.get(13)?,
            revision: row.get(14)?,
        },
        milestone,
        relation: PersonWorkRelation::ExplicitSelection,
    })
}

fn confirmed_conversations(
    history: &Connection,
    meetings: &Connection,
    person_id: &str,
) -> StorageResult<Vec<PersonWorkConversation>> {
    let mut statement = meetings.prepare(
        "SELECT m.id, m.started_at_utc_ms, m.title
         FROM meeting_person_links l
         JOIN persons p ON p.id = l.person_id
         JOIN meeting_sessions m ON m.id = l.meeting_id
         WHERE p.id = ?1 AND l.confidence = 'confirmed'
           AND m.phase != 'deleting' AND m.transcript_purged_at_utc_ms IS NULL
           AND (l.source IN ('manual', 'title') OR EXISTS(
                SELECT 1 FROM meeting_voice_speaker_resolutions r
                JOIN meeting_speakers s ON s.speaker_id = r.speaker_id
                WHERE s.session_id = m.id AND s.merged_into_speaker_id IS NULL
                  AND r.is_current = 1 AND r.resolution_kind = 'identified'
                  AND r.resolved_person_id = p.id
           ))",
    )?;
    let confirmed = statement
        .query_map([person_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, Option<i64>>(1)?, row.get::<_, String>(2)?),
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    if confirmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut statement = history.prepare(
        "SELECT e.id, e.target_kind, e.target_id, e.source_id, e.at_ms, e.revision, e.created_at_ms,
                p.id, p.title, p.status, p.revision,
                CASE e.target_kind WHEN 'task' THEN t.title WHEN 'milestone' THEN m.title ELSE p.title END,
                e.source_kind, e.meeting_note_id
         FROM planning_evidence_links e
         LEFT JOIN planning_tasks t ON e.target_kind = 'task' AND t.id = e.target_id
         LEFT JOIN planning_milestones m ON e.target_kind = 'milestone' AND m.id = e.target_id
         JOIN planning_projects p ON p.id = CASE e.target_kind
             WHEN 'project' THEN e.target_id WHEN 'task' THEN t.project_id ELSE m.project_id END
         WHERE e.source_kind IN ('meeting', 'loop')
         ORDER BY e.created_at_ms DESC, e.id",
    )?;
    let mut rows = statement.query([])?;
    let mut conversations = Vec::new();
    let mut seen = HashSet::new();
    while let Some(row) = rows.next()? {
        let mut link = EvidenceLink {
            id: row.get(0)?,
            target_kind: row.get(1)?,
            target_id: row.get(2)?,
            source_kind: row.get(12)?,
            source_id: row.get(3)?,
            at_ms: row.get(4)?,
            meeting_note_id: row.get(13)?,
            revision: row.get(5)?,
            created_at_ms: row.get(6)?,
            available: false,
            title: None,
        };
        let (meeting_id, loop_source) = match link.source_kind {
            EvidenceSourceKind::Meeting => (link.source_id.clone(), None),
            EvidenceSourceKind::Loop => {
                let loop_id = MeetingLoopId(link.source_id.clone());
                let Some(session_id) = loop_id.session_id() else {
                    continue;
                };
                (session_id.uuid().to_string(), Some((loop_id, session_id)))
            }
            EvidenceSourceKind::Note => continue,
        };
        let Some((started_at_ms, meeting_title)) = confirmed.get(&meeting_id) else {
            continue;
        };
        let project_id: String = row.get(7)?;
        let conversation_key = (meeting_id.clone(), project_id.clone());
        if seen.contains(&conversation_key) {
            continue;
        }
        if link.at_ms.is_none() {
            if let Some((loop_id, session_id)) = loop_source {
                let seed = loops::seeds_in(meetings, session_id)
                    .map_err(|_| anyhow::anyhow!("meeting evidence could not be read"))?
                    .into_iter()
                    .find(|seed| seed.loop_id == loop_id);
                let Some(seed) = seed else {
                    continue;
                };
                let Ok(at_ms) = i64::try_from(seed.at_ms) else {
                    continue;
                };
                link.at_ms = Some(at_ms);
            }
        }
        sources::resolve_link(history, meetings, &mut link)?;
        if !link.available {
            continue;
        }
        seen.insert(conversation_key);
        conversations.push(PersonWorkConversation {
            id: link.id,
            project: PersonWorkProject {
                id: project_id,
                title: row.get(8)?,
                status: row.get(9)?,
                revision: row.get(10)?,
            },
            target_kind: link.target_kind,
            target_id: link.target_id,
            target_title: row.get(11)?,
            meeting_id,
            meeting_title: meeting_title.clone(),
            meeting_started_at_ms: *started_at_ms,
            at_ms: link.at_ms,
            revision: link.revision,
            relation: PersonWorkRelation::ConfirmedConversation,
        });
    }
    Ok(conversations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::ledger::{
        LedgerCommitment, LedgerFirmness, LedgerReceipt, LedgerReceiptState, MeetingLedger,
    };
    use crate::meeting::loop_types::MeetingLoopKind;
    use crate::meeting::store::workflow_core_tests;
    use crate::meeting::types::{
        ArtifactCitation, CitedArtifactText, GeneratedMeetingArtifacts, TranscriptSegmentId,
    };
    use rusqlite::params;
    use uuid::Uuid;

    #[test]
    fn confirmed_person_sees_project_linked_only_through_a_retained_commitment() {
        let (_directory, meetings) = workflow_core_tests::store();
        let session_id =
            workflow_core_tests::meeting(&meetings, "Design review", 1_700_000_000_000);
        workflow_core_tests::transcript(&meetings, session_id, "I will send the outline");
        let person_id = workflow_core_tests::person(&meetings, "Dana", &[], &[]);
        let unrelated_id = workflow_core_tests::person(&meetings, "Other", &[], &[]);
        meetings
            .add_manual_person_link(session_id, person_id, 0, 1_700_000_000_000)
            .expect("confirm this person's actual meeting");
        let segment_id: String = meetings
            .with_planning_snapshot(|connection| {
                Ok(connection.query_row(
                    "SELECT segment_id FROM meeting_transcript_segments
                     WHERE transcript_revision_id = (
                         SELECT current_transcript_revision_id FROM meeting_sessions WHERE id = ?1
                     ) LIMIT 1",
                    [session_id.uuid().to_string()],
                    |row| row.get(0),
                )?)
            })
            .expect("read retained transcript citation");

        let commitment_text = "Send the outline";
        let ledger = MeetingLedger {
            headline: "An outline was promised".to_string(),
            threads: Vec::new(),
            open_loops: Vec::new(),
            commitments: vec![LedgerCommitment {
                who: "Dana".to_string(),
                what: commitment_text.to_string(),
                firmness: LedgerFirmness::Firm,
                receipt: LedgerReceipt {
                    quote: "I will send the outline".to_string(),
                    speaker: Some("Dana".to_string()),
                    t_ms: 0,
                    citations: vec![ArtifactCitation {
                        segment_id: TranscriptSegmentId::from_uuid(
                            Uuid::parse_str(&segment_id).expect("saved transcript segment id"),
                        ),
                        start_offset_ns: 0,
                        end_offset_ns: 1,
                    }],
                },
            }],
            stances: Vec::new(),
            caveats: Vec::new(),
            receipts: LedgerReceiptState::Verified,
        };
        let artifacts = GeneratedMeetingArtifacts {
            summary: CitedArtifactText {
                text: "An outline was promised".to_string(),
                citations: Vec::new(),
            },
            summary_trace: Vec::new(),
            outline: Vec::new(),
            decisions: Vec::new(),
            action_items: Vec::new(),
            key_questions: Vec::new(),
            risks: Vec::new(),
            follow_up_draft: CitedArtifactText {
                text: String::new(),
                citations: Vec::new(),
            },
            ledger: Some(ledger),
            ledger_failure: None,
            plain: false,
            plain_because: None,
            left_out: Vec::new(),
        };
        meetings
            .with_planning_snapshot(|connection| {
                let transcript_id: String = connection.query_row(
                    "SELECT current_transcript_revision_id FROM meeting_sessions WHERE id = ?1",
                    [session_id.uuid().to_string()],
                    |row| row.get(0),
                )?;
                let artifact_id = Uuid::new_v4().to_string();
                connection.execute(
                    "INSERT INTO meeting_artifact_revisions (
                        artifact_id, session_id, transcript_revision_id, input_revision,
                        template_id, template_version, generation_key, state,
                        content_json, generated_at_utc_ms
                    ) VALUES (?1, ?2, ?3, 0, 'test', 1, ?4, 'current', ?5, ?6)",
                    params![
                        artifact_id,
                        session_id.uuid().to_string(),
                        transcript_id,
                        format!("test-{artifact_id}"),
                        serde_json::to_string(&artifacts).expect("encode actual ledger"),
                        1_700_000_000_000_i64
                    ],
                )?;
                Ok(())
            })
            .expect("save the retained meeting commitment");

        let mut history = Connection::open_in_memory().expect("history");
        history
            .execute_batch(super::super::schema::MIGRATION)
            .expect("projects migration");
        history
            .execute_batch(super::super::schema::QUIET_REVIEW_MIGRATION)
            .expect("nighttime review migration");
        history
            .execute_batch(super::super::schema::MEETING_NOTE_LINK_MIGRATION)
            .expect("meeting-note identity migration");
        let loop_id =
            MeetingLoopId::derive(session_id, MeetingLoopKind::Commitment, commitment_text);
        meetings
            .with_planning_snapshot(|connection| {
                let project = super::super::store::insert_project(
                    &history,
                    "Project plan",
                    None,
                    1_700_000_000_000,
                )
                .expect("save the project");
                let link = super::super::store::link_source(
                    &mut history,
                    &super::super::store::SourceLinkDraft {
                        target_kind: EvidenceTargetKind::Project,
                        target_id: &project.id,
                        source_kind: EvidenceSourceKind::Loop,
                        source_id: loop_id.as_str(),
                        at_ms: None,
                        meeting_note_id: None,
                    },
                    &Uuid::new_v4().to_string(),
                    1_700_000_000_000,
                    |history| {
                        sources::require_source(
                            history,
                            connection,
                            EvidenceSourceKind::Loop,
                            loop_id.as_str(),
                            None,
                        )
                    },
                )
                .expect("link the retained commitment without a meeting citation");
                assert_eq!(link.source_kind, EvidenceSourceKind::Loop);
                let saved = super::super::store::records(&history, &project.id)
                    .expect("read saved evidence");
                assert_eq!(saved.evidence.len(), 1);
                assert_eq!(saved.evidence[0].id, link.id);

                let work = read(&history, connection, &person_id.uuid().to_string())
                    .expect("read confirmed person's private work");
                assert_eq!(work.conversations.len(), 1);
                let conversation = &work.conversations[0];
                assert_eq!(conversation.project.id, project.id);
                assert_eq!(conversation.meeting_id, session_id.uuid().to_string());
                assert_eq!(conversation.meeting_title, "Design review");
                assert_eq!(conversation.at_ms, Some(0));
                assert_eq!(
                    conversation.relation,
                    PersonWorkRelation::ConfirmedConversation
                );
                assert!(read(&history, connection, &unrelated_id.uuid().to_string())
                    .expect("read unrelated person")
                    .conversations
                    .is_empty());
                Ok(())
            })
            .expect("read the encrypted meeting snapshot");
    }
}
