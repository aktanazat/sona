use super::lifecycle::item_ref;
use super::notes::{receipt_in, Receipt};
use super::{
    handle, now_ms, refusal, task_ref, task_ref_of, Board, ExternalWorkChange, ExternalWorkRef,
    ExternalWorkSuggestion, TaskRef, Tracker, WorkAction, WorkAnswer, WriteArgs,
};
use crate::planning;
use crate::planning::store::Refusal;
use crate::planning::work::agents::AgentChange;
use crate::planning::work::triage_hints::{self, Taking};
use crate::planning::work::types::{
    WorkActor, WorkFieldSuggestion, WorkItem, WorkItemRow, WorkRelationKind, WorkStatus,
    WorkSuggestionField,
};
use crate::planning::work::{self, agents};
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use anyhow::Result as StorageResult;
use clap::Args;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use uuid::Uuid;

#[cfg(test)]
mod tests;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TriageSuggestionsArgs {
    /// S-12, 12, or the task's id. Leave it out for every task in Triage.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: Option<TaskRef>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SuggestionDecideArgs {
    /// The suggestion's id, from `sona work triage-suggestions`.
    #[arg(value_name = "ID")]
    pub id: Uuid,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionAction {
    Accepted,
    Dismissed,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalTriageTask {
    pub task: ExternalWorkRef,
    pub suggestions: Vec<ExternalWorkSuggestion>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalTriageSuggestions {
    pub schema_version: u32,
    pub tasks: Vec<ExternalTriageTask>,
    pub conversations_available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSuggestionDecision {
    pub schema_version: u32,
    pub action: SuggestionAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub replayed: bool,
    pub task: ExternalWorkRef,
    pub suggestion: ExternalWorkSuggestion,
    pub change: Option<ExternalWorkChange>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum TriageHintsAnswer {
    Suggestions(ExternalTriageSuggestions),
    Decision(Box<ExternalSuggestionDecision>),
}

pub(super) const fn field_text(field: WorkSuggestionField) -> &'static str {
    match field {
        WorkSuggestionField::Owner => "owner",
        WorkSuggestionField::Due => "due",
        WorkSuggestionField::Project => "project",
        WorkSuggestionField::Label => "label",
        WorkSuggestionField::Duplicate => "duplicate of",
        WorkSuggestionField::Related => "related to",
    }
}

fn gone(id: &str) -> ExternalError {
    ExternalError::new(
        ExternalErrorCode::NotFound,
        format!("No triage suggestion {id}: it was taken, dismissed or no longer applies."),
    )
    .hint("List them with: sona work triage-suggestions")
}

fn offered<'b>(
    board: &'b Board,
    id: &str,
) -> Result<(&'b WorkItemRow, &'b WorkFieldSuggestion), ExternalError> {
    board
        .suggestions
        .iter()
        .find_map(|(task_id, suggestions)| {
            let suggestion = suggestions.iter().find(|suggestion| suggestion.id == id)?;
            Some((board.row(task_id)?, suggestion))
        })
        .ok_or_else(|| gone(id))
}

fn decision(
    board: &Board,
    item: &WorkItem,
    suggestion: &WorkFieldSuggestion,
    action: SuggestionAction,
) -> ExternalSuggestionDecision {
    ExternalSuggestionDecision {
        schema_version: QUERY_SCHEMA_VERSION,
        action,
        dry_run: false,
        operation_id: None,
        replayed: false,
        task: item_ref(item),
        suggestion: board.suggestion(suggestion),
        change: None,
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Dismissed,
    Changed(AgentChange),
    Duplicate(WorkItem),
    Related,
}

/// What deciding a suggestion did, filed under the caller's operation id so
/// the same id answers with it again.
#[derive(Deserialize, Serialize)]
struct Decided {
    item: WorkItem,
    suggestion: WorkFieldSuggestion,
    outcome: Outcome,
}

impl Decided {
    fn answer(
        &self,
        board: &Board,
        action: SuggestionAction,
        operation_id: String,
        replayed: bool,
    ) -> Result<ExternalSuggestionDecision, ExternalError> {
        if board.row(&self.item.id).is_none() || board.fence.hides_suggestion(&self.suggestion) {
            return Err(gone(&self.suggestion.id));
        }
        let made = |action, tasks, proposed| ExternalWorkChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            operation_id: Some(operation_id.clone()),
            tasks,
            skipped: Vec::new(),
            proposed,
            replayed,
            evidence_url: None,
        };
        let change = match &self.outcome {
            Outcome::Dismissed => None,
            Outcome::Changed(change) => Some(made(
                WorkAction::Updated,
                (change.item.revision != self.item.revision)
                    .then(|| board.project_item(change.item.clone()))
                    .into_iter()
                    .collect(),
                change
                    .proposed
                    .iter()
                    .map(|proposal| board.proposal(proposal))
                    .collect(),
            )),
            Outcome::Duplicate(item) => Some(made(
                WorkAction::MarkedDuplicate,
                vec![board.project_item(item.clone())],
                Vec::new(),
            )),
            Outcome::Related => Some(made(
                WorkAction::Updated,
                vec![board.project_item(self.item.clone())],
                Vec::new(),
            )),
        };
        Ok(ExternalSuggestionDecision {
            operation_id: Some(operation_id),
            replayed,
            change,
            ..decision(board, &self.item, &self.suggestion, action)
        })
    }
}

const fn ledger_command(action: SuggestionAction) -> &'static str {
    match action {
        SuggestionAction::Accepted => "cli_suggestion_accept",
        SuggestionAction::Dismissed => "cli_suggestion_dismiss",
    }
}

/// The decision's own ledger id, so the change it made keeps the caller's.
fn ledger_id(operation_id: Uuid) -> String {
    Uuid::new_v5(&operation_id, b"triage-suggestion").to_string()
}

fn file(
    conn: &Connection,
    operation_id: Uuid,
    action: SuggestionAction,
    decided: &Decided,
) -> StorageResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO planning_operations (operation_id, command, request_json, response_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            ledger_id(operation_id),
            ledger_command(action),
            serde_json::to_string(&decided.suggestion.id)?,
            serde_json::to_string(&(decided, &decided.suggestion.drawn_from))?,
            now_ms(),
        ],
    )?;
    Ok(())
}

fn decided_before(
    conn: &Connection,
    operation_id: Uuid,
    action: SuggestionAction,
    id: &str,
) -> StorageResult<Option<Decided>> {
    let request = serde_json::to_string(id)?;
    match receipt_in(
        conn,
        &ledger_id(operation_id),
        ledger_command(action),
        &request,
    )? {
        Receipt::New(()) => Ok(None),
        Receipt::Replay(stored) => {
            let (mut decided, drawn_from): (Decided, Vec<String>) = serde_json::from_str(&stored)?;
            decided.suggestion.drawn_from = drawn_from;
            Ok(Some(decided))
        }
        Receipt::Conflict => Err(Refusal::OperationConflict.into()),
    }
}

impl Tracker<'_> {
    pub(super) async fn triage_suggestions(
        &self,
        args: &TriageSuggestionsArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let rows: Vec<&WorkItemRow> = match &args.task {
            Some(task) => vec![board.task(task)?],
            None => board
                .rows
                .iter()
                .filter(|row| {
                    row.item.status == WorkStatus::Triage && row.item.archived_at_ms.is_none()
                })
                .collect(),
        };
        let tasks = rows
            .into_iter()
            .map(|row| ExternalTriageTask {
                task: task_ref_of(row),
                suggestions: board
                    .suggestions
                    .get(&row.item.id)
                    .into_iter()
                    .flatten()
                    .map(|suggestion| board.suggestion(suggestion))
                    .collect(),
            })
            .collect();
        Ok(WorkAnswer::TriageHints(Box::new(
            TriageHintsAnswer::Suggestions(ExternalTriageSuggestions {
                schema_version: QUERY_SCHEMA_VERSION,
                tasks,
                conversations_available: board.fence.is_open(),
            }),
        )))
    }

    pub(super) async fn suggestion_decide(
        &self,
        args: &SuggestionDecideArgs,
        action: SuggestionAction,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let id = args.id.to_string();
        let answered = |decision| {
            WorkAnswer::TriageHints(Box::new(TriageHintsAnswer::Decision(Box::new(decision))))
        };
        if let Some(caller) = args.write.operation_id {
            let earlier = planning::history(self.history, |conn| {
                decided_before(conn, caller, action, &id)
            })
            .map_err(|message| {
                self.write_refusal(message, &caller.to_string(), "The suggestion")
            })?;
            if let Some(decided) = earlier {
                return Ok(answered(decided.answer(
                    &board,
                    action,
                    caller.to_string(),
                    true,
                )?));
            }
        }
        let (row, suggestion) = offered(&board, &id)?;
        if args.dry_run {
            return Ok(answered(ExternalSuggestionDecision {
                dry_run: true,
                operation_id: args.write.operation_id.map(|id| id.to_string()),
                ..decision(&board, &row.item, suggestion, action)
            }));
        }
        let verb = match action {
            SuggestionAction::Accepted => "suggestion-accept",
            SuggestionAction::Dismissed => "suggestion-dismiss",
        };
        let operation_id = args
            .write
            .operation_id(verb, &(&suggestion.id, row.item.revision))?;
        let outcome = match action {
            SuggestionAction::Dismissed => {
                self.applies("dismissing a suggestion")?;
                triage_hints::dismiss(self.app, &operation_id, &row.item.id, &suggestion.id)
                    .await
                    .map_err(|message| {
                        self.write_refusal(message, &operation_id, "The suggestion")
                    })?;
                Outcome::Dismissed
            }
            SuggestionAction::Accepted => {
                self.take(row, suggestion, &operation_id, &args.write)
                    .await?
            }
        };
        let decided = Decided {
            item: row.item.clone(),
            suggestion: suggestion.clone(),
            outcome,
        };
        if let Some(caller) = args.write.operation_id {
            planning::history(self.history, |conn| file(conn, caller, action, &decided))
                .map_err(|message| refusal(self.history, message))?;
        }
        Ok(answered(decided.answer(
            &board,
            action,
            operation_id,
            false,
        )?))
    }

    async fn take(
        &self,
        row: &WorkItemRow,
        suggestion: &WorkFieldSuggestion,
        operation_id: &str,
        write: &WriteArgs,
    ) -> Result<Outcome, ExternalError> {
        let refuse = |message| self.write_refusal(message, operation_id, &handle(row.item.number));
        let started_ms = now_ms();
        match triage_hints::taking(&row.item, suggestion) {
            Some(Taking::Patch(patch)) => agents::update_as(
                self.app,
                &write.agent(),
                operation_id,
                agents::ItemChange {
                    id: &row.item.id,
                    expected_revision: row.item.revision,
                    patch: &patch,
                    link: None,
                },
            )
            .await
            .map(Outcome::Changed)
            .map_err(refuse),
            Some(Taking::Duplicate(of_id)) => {
                self.applies("marking a duplicate")?;
                let item = work::mark_duplicate_as(
                    self.app,
                    WorkActor::Agent,
                    operation_id,
                    &row.item.id,
                    &of_id,
                )
                .await
                .map_err(refuse)?;
                self.credit(write, std::slice::from_ref(&row.item.id), started_ms)?;
                Ok(Outcome::Duplicate(item))
            }
            Some(Taking::Related(to_id)) => {
                work::relation_add_as(
                    self.app,
                    WorkActor::Agent,
                    operation_id,
                    &row.item.id,
                    &to_id,
                    WorkRelationKind::Related,
                )
                .map_err(refuse)?;
                self.credit(write, &[row.item.id.clone(), to_id], started_ms)?;
                Ok(Outcome::Related)
            }
            None => Err(ExternalError::invalid(triage_hints::GONE)),
        }
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    let WorkAnswer::TriageHints(answer) = answer else {
        return;
    };
    match answer.as_ref() {
        TriageHintsAnswer::Suggestions(list) => {
            if list.tasks.iter().all(|task| task.suggestions.is_empty()) {
                text.push_str("No suggestions for these tasks.\n");
            }
            for task in list
                .tasks
                .iter()
                .filter(|task| !task.suggestions.is_empty())
            {
                let _ = writeln!(text, "{} {}", task.task.handle, task.task.title);
                for suggestion in &task.suggestions {
                    let _ = writeln!(
                        text,
                        "  {}: {} ({})  {}",
                        field_text(suggestion.field),
                        suggestion.value,
                        suggestion.reason,
                        suggestion.suggestion_id
                    );
                }
            }
        }
        TriageHintsAnswer::Decision(decision) => {
            let verb = match (decision.action, decision.dry_run) {
                (SuggestionAction::Accepted, false) => "Accepted",
                (SuggestionAction::Accepted, true) => "Would accept",
                (SuggestionAction::Dismissed, false) => "Dismissed",
                (SuggestionAction::Dismissed, true) => "Would dismiss",
            };
            let _ = writeln!(
                text,
                "{verb} for {} {}: {} {} ({})",
                decision.task.handle,
                decision.task.title,
                field_text(decision.suggestion.field),
                decision.suggestion.value,
                decision.suggestion.reason
            );
            if decision.replayed {
                text.push_str("This operation already ran; this is its answer again.\n");
            }
            if decision
                .change
                .as_ref()
                .is_some_and(|change| !change.proposed.is_empty())
            {
                text.push_str("Waiting for the user to accept in Sona.\n");
            }
        }
    }
}
