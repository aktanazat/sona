//! `sona work` verbs for personal documents: notes private to the person in
//! Sona and the agents they are shared with, each at a role, until they are
//! moved into a project, a task or a cycle. An agent reaches only the ones
//! shared with it, by the name it gives with `--agent`.

use super::notes::{project_note, request_json, ExternalNote, Receipt};
use super::{cycle_ref, now_ms, refusal, Tracker, WorkAnswer, WriteArgs};
use crate::planning;
use crate::planning::work::agents;
use crate::planning::work::types::WorkCycleRef;
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::{note_link, QUERY_SCHEMA_VERSION};
use crate::scratchpad::documents::ScratchDocumentOf;
use crate::scratchpad::personal::{self, Reach, ScratchCollaborator, ScratchNoteRole};
use crate::scratchpad::{self, Author};
use clap::{ArgGroup, Args};
use serde::Serialize;
use std::fmt::Write as _;
use tauri::Manager;
use uuid::Uuid;

const PAGE: usize = 50;

/// A role as `--role` names it, or none, which takes the agent off.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoleArg(pub Option<ScratchNoteRole>);

pub(super) fn role_arg(value: &str) -> Result<RoleArg, String> {
    match value.trim().to_lowercase().as_str() {
        "view" => Ok(RoleArg(Some(ScratchNoteRole::View))),
        "comment" => Ok(RoleArg(Some(ScratchNoteRole::Comment))),
        "edit" => Ok(RoleArg(Some(ScratchNoteRole::Edit))),
        "none" | "null" => Ok(RoleArg(None)),
        _ => Err("expected view, comment, edit or none".to_string()),
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonalListArgs {
    /// Who is reading, as Sona shows agents: "Claude Code", "Codex".
    #[arg(long, value_name = "NAME")]
    pub agent: Option<String>,
    /// Documents to print, at most 100.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub limit: Option<u32>,
    /// The `next_cursor` of the page before.
    #[arg(long, value_name = "NOTE_ID")]
    pub cursor: Option<Uuid>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonalShowArgs {
    /// The document's id, from `sona work personal`.
    #[arg(value_name = "NOTE_ID")]
    pub note: Uuid,
    /// Who is reading, as Sona shows agents.
    #[arg(long, value_name = "NAME")]
    pub agent: Option<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonalNewArgs {
    /// The document's text. Its first words become its title.
    #[arg(required = true, num_args = 1.., value_name = "TEXT")]
    pub body: Vec<String>,
    /// Print what would be written and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonalShareArgs {
    /// The document's id, from `sona work personal`.
    #[arg(value_name = "NOTE_ID")]
    pub note: Uuid,
    /// The agent to share it with, as Sona shows agents: "Codex".
    #[arg(long = "with", value_name = "AGENT")]
    pub with: String,
    /// view, comment or edit; none takes the agent off.
    #[arg(long, value_parser = role_arg, value_name = "ROLE")]
    pub role: RoleArg,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonalPrivateArgs {
    /// The note's id, from `sona --notes`.
    #[arg(value_name = "NOTE_ID")]
    pub note: Uuid,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
#[command(group(ArgGroup::new("into").required(true).args(["task", "project", "cycle"])))]
pub struct PersonalMoveArgs {
    /// The document's id, from `sona work personal`.
    #[arg(value_name = "NOTE_ID")]
    pub note: Uuid,
    /// File it as a document of this task: S-12, 12 or its id.
    #[arg(long, value_name = "TASK")]
    pub task: Option<String>,
    /// File it as a document of this project, by name or id.
    #[arg(long, value_name = "PROJECT")]
    pub project: Option<String>,
    /// File it as a document of this cycle: current, next, or its number.
    #[arg(long, value_parser = cycle_ref, value_name = "CYCLE")]
    pub cycle: Option<WorkCycleRef>,
    /// Print where it would go and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalPersonalRow {
    pub id: String,
    pub title: String,
    /// The reader's role on it.
    pub role: ScratchNoteRole,
    pub updated_at_ms: i64,
    pub link: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalPersonalDocuments {
    pub schema_version: u32,
    pub agent: String,
    pub entries: Vec<ExternalPersonalRow>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalAction {
    Shown,
    Created,
    Shared,
    Unshared,
    Moved,
    MadePrivate,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalPersonalDocument {
    pub schema_version: u32,
    pub action: PersonalAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub replayed: bool,
    pub note: ExternalNote,
    /// The caller's role on it; null once it has moved out of the personal
    /// space, when External access alone decides.
    pub role: Option<ScratchNoteRole>,
    pub shared_with: Vec<ScratchCollaborator>,
    /// Where it was moved to.
    pub moved_to: Option<ScratchDocumentOf>,
    pub link: String,
}

impl Tracker<'_> {
    /// The document `id` the caller reaches at `needed`, refused when it is
    /// not a personal document.
    fn personal_document(
        &self,
        id: &Uuid,
        agent: Option<&str>,
        needed: ScratchNoteRole,
    ) -> Result<(scratchpad::ScratchNote, ScratchNoteRole), ExternalError> {
        let note = self.note(id, agent, needed)?;
        let reach = planning::history(self.history, |conn| {
            Ok(personal::reach(conn, &note.id, agent)?)
        })
        .map_err(|message| refusal(self.history, message))?;
        match reach {
            Some(Reach::Personal(Some(role))) => Ok((note, role)),
            _ => Err(
                ExternalError::invalid(format!("Note {id} is not a personal document."))
                    .hint("Read ordinary notes with: sona --note NOTE_ID"),
            ),
        }
    }

    fn document_answer(
        &self,
        action: PersonalAction,
        dry_run: bool,
        operation_id: Option<String>,
        replayed: bool,
        note: scratchpad::ScratchNote,
        role: Option<ScratchNoteRole>,
    ) -> Result<WorkAnswer, ExternalError> {
        let shared_with = planning::history(self.history, |conn| {
            Ok(personal::collaborators(conn, &note.id)?)
        })
        .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::PersonalDocument(Box::new(
            ExternalPersonalDocument {
                schema_version: QUERY_SCHEMA_VERSION,
                action,
                dry_run,
                operation_id,
                replayed,
                link: note_link(&note.id),
                note: project_note(note),
                role,
                shared_with,
                moved_to: None,
            },
        )))
    }

    pub(super) fn personal_list(
        &self,
        args: &PersonalListArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let limit = args
            .limit
            .and_then(|limit| usize::try_from(limit).ok())
            .unwrap_or(PAGE);
        let cursor = args.cursor.map(|cursor| cursor.to_string());
        let agent = args.agent.as_deref();
        let mut found = planning::history(self.history, |conn| {
            Ok(personal::shared_with(
                conn,
                agent,
                cursor.as_deref(),
                limit + 1,
            )?)
        })
        .map_err(|message| refusal(self.history, message))?;
        let next_cursor = (found.len() > limit).then(|| found[limit - 1].0.id.clone());
        found.truncate(limit);
        Ok(WorkAnswer::PersonalDocuments(ExternalPersonalDocuments {
            schema_version: QUERY_SCHEMA_VERSION,
            agent: agents::agent_name(agent),
            entries: found
                .into_iter()
                .map(|(note, role)| ExternalPersonalRow {
                    link: note_link(&note.id),
                    id: note.id,
                    title: note.title,
                    role,
                    updated_at_ms: note.updated_at_ms,
                })
                .collect(),
            next_cursor,
        }))
    }

    pub(super) fn personal_show(
        &self,
        args: &PersonalShowArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let (note, role) =
            self.personal_document(&args.note, args.agent.as_deref(), ScratchNoteRole::View)?;
        self.document_answer(PersonalAction::Shown, false, None, false, note, Some(role))
    }

    pub(super) fn personal_new(&self, args: &PersonalNewArgs) -> Result<WorkAnswer, ExternalError> {
        let body = args.body.join(" ");
        if args.dry_run {
            let now = now_ms();
            let note = scratchpad::ScratchNote {
                id: String::new(),
                title: scratchpad::derive_title(&body),
                body,
                pinned: false,
                created_at_ms: now,
                updated_at_ms: now,
                personal: true,
            };
            return Ok(WorkAnswer::PersonalDocument(Box::new(
                ExternalPersonalDocument {
                    schema_version: QUERY_SCHEMA_VERSION,
                    action: PersonalAction::Created,
                    dry_run: true,
                    operation_id: args.write.operation_id.map(|id| id.to_string()),
                    replayed: false,
                    link: String::new(),
                    note: project_note(note),
                    role: Some(ScratchNoteRole::Edit),
                    shared_with: vec![ScratchCollaborator {
                        agent: args.write.agent(),
                        role: ScratchNoteRole::Edit,
                        added_at_ms: now_ms(),
                    }],
                    moved_to: None,
                },
            )));
        }
        let author = Author::Agent(args.write.agent.as_deref());
        let (operation_id, note, replayed) = self.once(
            &args.write,
            "cli_personal_create",
            &body,
            "The document could not be saved.",
            |tx| personal::create_in(tx, &body, author, now_ms()),
        )?;
        self.document_answer(
            PersonalAction::Created,
            false,
            Some(operation_id),
            replayed,
            note,
            Some(ScratchNoteRole::Edit),
        )
    }

    pub(super) fn personal_share(
        &self,
        args: &PersonalShareArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let caller = args.write.agent.as_deref();
        let (note, role) = self.personal_document(&args.note, caller, ScratchNoteRole::Edit)?;
        if args.with.trim().is_empty() {
            return Err(ExternalError::invalid("Name the agent to share with."));
        }
        let RoleArg(given) = args.role;
        let action = match given {
            Some(_) => PersonalAction::Shared,
            None => PersonalAction::Unshared,
        };
        let with = agents::agent_name(Some(&args.with));
        let current = planning::history(self.history, |conn| {
            Ok(personal::collaborators(conn, &note.id)?)
        })
        .map_err(|message| refusal(self.history, message))?
        .into_iter()
        .find(|found| found.agent.eq_ignore_ascii_case(&with))
        .map(|found| found.role);
        let settled = current == given;
        if args.dry_run || settled {
            let mut answer = self.document_answer(
                action,
                args.dry_run,
                args.write.operation_id.map(|id| id.to_string()),
                settled,
                note,
                Some(role),
            )?;
            if let (WorkAnswer::PersonalDocument(document), false) = (&mut answer, settled) {
                document
                    .shared_with
                    .retain(|found| !found.agent.eq_ignore_ascii_case(&with));
                if let Some(role) = given {
                    document.shared_with.push(ScratchCollaborator {
                        agent: with.clone(),
                        role,
                        added_at_ms: now_ms(),
                    });
                }
            }
            return Ok(answer);
        }
        let operation_id = args.write.fresh_operation_id();
        let request = request_json(&(&note.id, &with, given))?;
        self.applies("sharing personal documents")?;
        let written = self.write_once(
            &operation_id,
            "cli_personal_share",
            &request,
            "The document's sharing could not be changed.",
            |tx| {
                personal::share_in(tx, &note.id, &with, given, now_ms())
                    .map(|_| (note.id.clone(), ()))
            },
        )?;
        let replayed = match written {
            Receipt::New(()) => {
                scratchpad::emit_changed(self.app, &note.id, false);
                false
            }
            Receipt::Replay(_) => true,
            Receipt::Conflict => return Err(super::notes::conflict(&operation_id)),
        };
        let own = if with.eq_ignore_ascii_case(&args.write.agent()) {
            given
        } else {
            Some(role)
        };
        self.document_answer(action, false, Some(operation_id), replayed, note, own)
    }

    pub(super) fn personal_private(
        &self,
        args: &PersonalPrivateArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let caller = args.write.agent.as_deref();
        let note = self.note(&args.note, caller, ScratchNoteRole::Edit)?;
        if args.dry_run || note.personal {
            return self.document_answer(
                PersonalAction::MadePrivate,
                args.dry_run,
                args.write.operation_id.map(|id| id.to_string()),
                note.personal,
                note,
                Some(ScratchNoteRole::Edit),
            );
        }
        let operation_id = args.write.fresh_operation_id();
        let request = request_json(&note.id)?;
        self.applies("making notes private")?;
        let written = self.write_once(
            &operation_id,
            "cli_personal_private",
            &request,
            "The note could not be made private.",
            |tx| {
                personal::make_personal_in(tx, &note.id, Author::Agent(caller), now_ms())
                    .map(|_| (note.id.clone(), ()))
            },
        )?;
        let replayed = match written {
            Receipt::New(()) => {
                scratchpad::emit_changed(self.app, &note.id, false);
                false
            }
            Receipt::Replay(_) => true,
            Receipt::Conflict => return Err(super::notes::conflict(&operation_id)),
        };
        let note = self.note(&args.note, caller, ScratchNoteRole::View)?;
        self.document_answer(
            PersonalAction::MadePrivate,
            false,
            Some(operation_id),
            replayed,
            note,
            Some(ScratchNoteRole::Edit),
        )
    }

    pub(super) async fn personal_move(
        &self,
        args: &PersonalMoveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let (note, role) = self.personal_document(
            &args.note,
            args.write.agent.as_deref(),
            ScratchNoteRole::Edit,
        )?;
        let document = match args.cycle {
            Some(cycle) => self.cycle_document(cycle)?.1,
            None => self
                .planning_document(args.task.as_deref(), args.project.as_deref())
                .await?
                .ok_or_else(|| ExternalError::invalid("Name a task, a project or a cycle."))?,
        };
        let moved = |answer: Result<WorkAnswer, ExternalError>| {
            answer.map(|mut answer| {
                if let WorkAnswer::PersonalDocument(found) = &mut answer {
                    found.moved_to = Some(document.clone());
                }
                answer
            })
        };
        if args.dry_run {
            return moved(self.document_answer(
                PersonalAction::Moved,
                true,
                args.write.operation_id.map(|id| id.to_string()),
                false,
                note,
                Some(role),
            ));
        }
        self.applies("moving personal documents")?;
        let operation_id = args.write.fresh_operation_id();
        self.file_document(&note.id, &document, &operation_id)
            .await?;
        let filed = scratchpad::scratchpad_get(self.app.state(), note.id.clone())
            .map_err(|message| refusal(self.history, message))?
            .ok_or_else(|| {
                ExternalError::new(ExternalErrorCode::NotFound, format!("No note {}.", note.id))
            })?;
        moved(self.document_answer(
            PersonalAction::Moved,
            false,
            Some(operation_id),
            false,
            filed,
            None,
        ))
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::PersonalDocuments(page) => {
            if page.entries.is_empty() {
                let _ = writeln!(
                    text,
                    "No personal documents are shared with {}.",
                    page.agent
                );
            }
            for row in &page.entries {
                let _ = writeln!(text, "{:<8} {}  {}", row.role.as_str(), row.title, row.id);
            }
            if let Some(cursor) = &page.next_cursor {
                let _ = writeln!(text, "More: sona work personal --cursor {cursor}");
            }
        }
        WorkAnswer::PersonalDocument(document) => {
            let verb = match (document.action, document.dry_run) {
                (PersonalAction::Shown, _) => "Personal document",
                (PersonalAction::Created, false) => "Made a personal document",
                (PersonalAction::Created, true) => "Would make a personal document",
                (PersonalAction::Shared, false) => "Shared the document",
                (PersonalAction::Shared, true) => "Would share the document",
                (PersonalAction::Unshared, false) => "Stopped sharing the document",
                (PersonalAction::Unshared, true) => "Would stop sharing the document",
                (PersonalAction::Moved, false) => "Moved the document",
                (PersonalAction::Moved, true) => "Would move the document",
                (PersonalAction::MadePrivate, false) => "Made the note private",
                (PersonalAction::MadePrivate, true) => "Would make the note private",
            };
            let _ = writeln!(
                text,
                "{verb}{}: {}",
                if document.replayed {
                    " (already so)"
                } else {
                    ""
                },
                document.note.title
            );
            if let Some(to) = &document.moved_to {
                let _ = writeln!(text, "Into: {} ({:?})", to.title, to.kind);
            }
            if let Some(role) = document.role {
                let _ = writeln!(text, "Your role: {}", role.as_str());
            }
            for collaborator in &document.shared_with {
                let _ = writeln!(
                    text,
                    "Shared with {} ({})",
                    collaborator.agent,
                    collaborator.role.as_str()
                );
            }
            if document.action == PersonalAction::Shown {
                let _ = writeln!(text, "\n{}", document.note.body);
            }
        }
        _ => {}
    }
}
