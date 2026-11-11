//! `sona work` verbs for note subjects: the topics notes gather under, each
//! with the note that is current for it.

use super::notes::{conflict, request_json, Receipt};
use super::{refusal, Tracker, WorkAnswer, WriteArgs};
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use crate::scratchpad::subjects::{self, ScratchSubject, ScratchSubjectPage, SubjectNoteAction};
use clap::{ArgGroup, Args};
use serde::Serialize;
use std::fmt::Write as _;
use tauri::Manager;
use uuid::Uuid;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SubjectNewArgs {
    /// The subject's name.
    #[arg(required = true, num_args = 1.., value_name = "TITLE")]
    pub title: Vec<String>,
    /// Print what would be made and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
#[command(group(ArgGroup::new("how").args(["current", "remove"])))]
pub struct SubjectNoteArgs {
    /// The subject's id, from `sona work subjects`.
    #[arg(value_name = "SUBJECT_ID")]
    pub subject: Uuid,
    /// The note's id, from `sona --notes`.
    #[arg(value_name = "NOTE_ID")]
    pub note: Uuid,
    /// Also make it the subject's current note.
    #[arg(long)]
    pub current: bool,
    /// Take the note off the subject.
    #[arg(long)]
    pub remove: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSubject {
    pub id: String,
    pub title: String,
    pub current_note_id: Option<String>,
    pub created_at_ms: i64,
    /// The notes under it, newest first; absent in a list.
    pub note_ids: Option<Vec<String>>,
}

fn project_subject(subject: ScratchSubject, page: Option<&ScratchSubjectPage>) -> ExternalSubject {
    ExternalSubject {
        id: subject.id,
        title: subject.title,
        current_note_id: subject.current_note_id,
        created_at_ms: subject.created_at_ms,
        note_ids: page.map(|page| page.history.iter().map(|note| note.id.clone()).collect()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectAction {
    Listed,
    Created,
    Added,
    MadeCurrent,
    Removed,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSubjects {
    pub schema_version: u32,
    pub action: SubjectAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub replayed: bool,
    pub subjects: Vec<ExternalSubject>,
}

fn answer(
    action: SubjectAction,
    dry_run: bool,
    operation_id: Option<String>,
    replayed: bool,
    subjects: Vec<ExternalSubject>,
) -> WorkAnswer {
    WorkAnswer::Subjects(Box::new(ExternalSubjects {
        schema_version: QUERY_SCHEMA_VERSION,
        action,
        dry_run,
        operation_id,
        replayed,
        subjects,
    }))
}

impl Tracker<'_> {
    fn subject_page(&self, id: &str) -> Result<ScratchSubjectPage, ExternalError> {
        subjects::read(self.history, id)
            .map_err(|message| refusal(self.history, message))?
            .ok_or_else(|| {
                ExternalError::new(ExternalErrorCode::NotFound, format!("No subject {id}."))
                    .hint("List them with: sona work subjects")
            })
    }

    pub(super) fn subjects(&self) -> Result<WorkAnswer, ExternalError> {
        let listed = subjects::scratchpad_subjects(self.app.state())
            .map_err(|message| refusal(self.history, message))?;
        Ok(answer(
            SubjectAction::Listed,
            false,
            None,
            false,
            listed
                .into_iter()
                .map(|subject| project_subject(subject, None))
                .collect(),
        ))
    }

    pub(super) fn subject_new(&self, args: &SubjectNewArgs) -> Result<WorkAnswer, ExternalError> {
        let title = args.title.join(" ").trim().to_owned();
        if args.dry_run {
            return Ok(answer(
                SubjectAction::Created,
                true,
                args.write.operation_id.map(|id| id.to_string()),
                false,
                vec![ExternalSubject {
                    id: String::new(),
                    title,
                    current_note_id: None,
                    created_at_ms: 0,
                    note_ids: Some(Vec::new()),
                }],
            ));
        }
        let operation_id = args.write.fresh_operation_id();
        let request = request_json(&title)?;
        match self.receipt(&operation_id, "cli_subject_new", &request)? {
            Receipt::New(()) => {}
            Receipt::Replay(subject_id) => {
                let page = self.subject_page(&subject_id)?;
                return Ok(answer(
                    SubjectAction::Created,
                    false,
                    Some(operation_id),
                    true,
                    vec![project_subject(page.subject.clone(), Some(&page))],
                ));
            }
            Receipt::Conflict => return Err(conflict(&operation_id)),
        }
        self.applies("writing notes")?;
        let subject = subjects::scratchpad_subject_create(self.app.state(), title)
            .map_err(|message| refusal(self.history, message))?;
        self.file_receipt(&operation_id, "cli_subject_new", &request, &subject.id)?;
        Ok(answer(
            SubjectAction::Created,
            false,
            Some(operation_id),
            false,
            vec![project_subject(subject, None)],
        ))
    }

    pub(super) fn subject_note(&self, args: &SubjectNoteArgs) -> Result<WorkAnswer, ExternalError> {
        let page = self.subject_page(&args.subject.to_string())?;
        let note_id = args.note.to_string();
        let (action, how) = match (args.current, args.remove) {
            (true, _) => (SubjectAction::MadeCurrent, SubjectNoteAction::SetCurrent),
            (_, true) => (SubjectAction::Removed, SubjectNoteAction::Remove),
            _ => (SubjectAction::Added, SubjectNoteAction::Add),
        };
        let holds = page.history.iter().any(|note| note.id == note_id);
        let already = match how {
            SubjectNoteAction::Add => holds,
            SubjectNoteAction::SetCurrent => {
                page.subject.current_note_id.as_deref() == Some(note_id.as_str())
            }
            SubjectNoteAction::Remove => !holds,
        };
        if args.dry_run || already {
            return Ok(answer(
                action,
                args.dry_run,
                args.write.operation_id.map(|id| id.to_string()),
                already,
                vec![project_subject(page.subject.clone(), Some(&page))],
            ));
        }
        self.applies("writing notes")?;
        let operation_id = args
            .write
            .operation_id("subject-note", &(&page.subject.id, &note_id, how))?;
        let changed = subjects::scratchpad_subject_note(
            self.app.state(),
            page.subject.id.clone(),
            note_id,
            how,
        )
        .map_err(|message| refusal(self.history, message))?;
        Ok(answer(
            action,
            false,
            Some(operation_id),
            false,
            vec![project_subject(changed.subject.clone(), Some(&changed))],
        ))
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    let WorkAnswer::Subjects(page) = answer else {
        return;
    };
    let verb = match (page.action, page.dry_run, page.replayed) {
        (SubjectAction::Listed, _, _) => None,
        (SubjectAction::Created, true, _) => Some("Would make the subject"),
        (SubjectAction::Created, false, _) => Some("Made the subject"),
        (SubjectAction::Added, true, _) => Some("Would add the note to"),
        (SubjectAction::MadeCurrent, true, _) => Some("Would make the note current for"),
        (SubjectAction::Removed, true, _) => Some("Would take the note off"),
        (_, false, true) => Some("Already so for"),
        (SubjectAction::Added, false, false) => Some("Added the note to"),
        (SubjectAction::MadeCurrent, false, false) => Some("Made the note current for"),
        (SubjectAction::Removed, false, false) => Some("Took the note off"),
    };
    if verb.is_none() && page.subjects.is_empty() {
        text.push_str("No subjects yet. Make one with: sona work subject-new TITLE\n");
    }
    for subject in &page.subjects {
        if let Some(verb) = verb {
            let _ = write!(text, "{verb} ");
        }
        let _ = write!(text, "{}", subject.title);
        if !subject.id.is_empty() {
            let _ = write!(text, "  id {}", subject.id);
        }
        if let Some(current) = &subject.current_note_id {
            let _ = write!(text, "  current note {current}");
        }
        if let Some(notes) = &subject.note_ids {
            let _ = write!(text, "  {} notes", notes.len());
        }
        text.push('\n');
    }
}
