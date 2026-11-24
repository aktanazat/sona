use super::{now_ms, refusal, task_ref_of, ExternalWorkRef, Tracker, WorkAnswer, WriteArgs};
use crate::planning::work::batches::{self, BatchUndo};
use crate::planning::work::doc_proposals::{self, DocProposal, DocProposalKind};
use crate::planning::work::triage_auto::{
    self, TriageAutoField, TriageRule, TriageRuleInput, TriageRuleMatch, WorkAutoApplied,
};
use crate::planning::work::types::{WorkActor, WorkOwner};
use crate::query::external::{ExternalError, ExternalErrorCode};
use crate::query::QUERY_SCHEMA_VERSION;
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::fmt::Write as _;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum MatchArg {
    /// The title of the meeting the task came from contains the words.
    Meeting,
    /// The meeting the task came from is in the folder with this name.
    Folder,
    /// The task's title or details contain the words.
    Words,
}

impl MatchArg {
    const fn matches(self) -> TriageRuleMatch {
        match self {
            Self::Meeting => TriageRuleMatch::Meeting,
            Self::Folder => TriageRuleMatch::Folder,
            Self::Words => TriageRuleMatch::Words,
        }
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TriageRuleSaveArgs {
    /// What to look for: words, a meeting title, or a folder name.
    #[arg(value_name = "VALUE")]
    pub value: String,
    /// Where to look.
    #[arg(long = "match", value_enum, default_value = "words")]
    pub matches: MatchArg,
    /// The rule to change, by id from `sona work triage-rules`; left out, a
    /// new rule.
    #[arg(long, value_name = "ID")]
    pub rule: Option<Uuid>,
    /// The project matching tasks go to.
    #[arg(long, value_name = "PROJECT")]
    pub project: Option<String>,
    /// The label matching tasks get.
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,
    /// Who matching tasks go to: me or a person.
    #[arg(long, value_name = "OWNER")]
    pub owner: Option<String>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TriageIdArgs {
    /// The id, from `sona work triage-rules` or `sona work triage-applied`.
    #[arg(value_name = "ID")]
    pub id: Uuid,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct HousekeepingUndoArgs {
    /// The housekeeping notice's id, from `sona work inbox`.
    #[arg(value_name = "NOTICE_ID")]
    pub id: Uuid,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationAction {
    Saved,
    Deleted,
    Undone,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalDocProposals {
    pub schema_version: u32,
    pub proposals: Vec<DocProposal>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalTriageRules {
    pub schema_version: u32,
    pub rules: Vec<TriageRule>,
    pub auto_apply: Vec<TriageAutoField>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalTriageRuleChange {
    pub schema_version: u32,
    pub action: AutomationAction,
    pub dry_run: bool,
    pub operation_id: String,
    pub rule: TriageRule,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalAppliedTask {
    pub task: ExternalWorkRef,
    pub applied: Vec<WorkAutoApplied>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalTriageApplied {
    pub schema_version: u32,
    pub tasks: Vec<ExternalAppliedTask>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalTriageUndo {
    pub schema_version: u32,
    pub action: AutomationAction,
    pub dry_run: bool,
    pub operation_id: String,
    pub task: ExternalWorkRef,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalHousekeepingUndo {
    pub schema_version: u32,
    pub action: AutomationAction,
    pub dry_run: bool,
    pub operation_id: String,
    pub notice_id: String,
    #[serde(flatten)]
    pub undone: BatchUndo,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum AutomationsAnswer {
    DocProposals(ExternalDocProposals),
    TriageRules(ExternalTriageRules),
    TriageRule(ExternalTriageRuleChange),
    TriageApplied(ExternalTriageApplied),
    TriageUndo(ExternalTriageUndo),
    HousekeepingUndo(ExternalHousekeepingUndo),
}

fn answered(answer: AutomationsAnswer) -> WorkAnswer {
    WorkAnswer::Automations(Box::new(answer))
}

fn missing(what: &str, id: &Uuid, hint: &str) -> ExternalError {
    ExternalError::new(ExternalErrorCode::NotFound, format!("No {what} {id}.")).hint(hint)
}

impl Tracker<'_> {
    pub(super) fn doc_proposals(&self) -> Result<WorkAnswer, ExternalError> {
        let proposals = doc_proposals::open_read(self.history)
            .map_err(|message| refusal(self.history, message))?;
        Ok(answered(AutomationsAnswer::DocProposals(
            ExternalDocProposals {
                schema_version: QUERY_SCHEMA_VERSION,
                proposals,
            },
        )))
    }

    fn all_rules(&self) -> Result<Vec<TriageRule>, ExternalError> {
        crate::planning::history(self.history, |conn| triage_auto::rules(conn))
            .map_err(|message| refusal(self.history, message))
    }

    pub(super) fn triage_rules(&self) -> Result<WorkAnswer, ExternalError> {
        Ok(answered(AutomationsAnswer::TriageRules(
            ExternalTriageRules {
                schema_version: QUERY_SCHEMA_VERSION,
                rules: self.all_rules()?,
                auto_apply: crate::settings::get_settings(self.app).work_triage_auto_apply,
            },
        )))
    }

    pub(super) async fn triage_rule_save(
        &self,
        args: &TriageRuleSaveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let existing = match args.rule {
            Some(id) => Some(
                self.all_rules()?
                    .into_iter()
                    .find(|rule| rule.id == id.to_string())
                    .ok_or_else(|| {
                        missing("rule", &id, "List them with: sona work triage-rules")
                    })?,
            ),
            None => None,
        };
        let input = TriageRuleInput {
            matches: args.matches.matches(),
            value: args.value.clone(),
            project_id: match &args.project {
                Some(project) => board.project(project)?.map(|project| project.id.clone()),
                None => existing.as_ref().and_then(|rule| rule.project_id.clone()),
            },
            label_id: match &args.label {
                Some(label) if label.trim().is_empty() => None,
                Some(label) => board.label_ids(&[label.clone()])?.into_iter().next(),
                None => existing.as_ref().and_then(|rule| rule.label_id.clone()),
            },
            owner: match &args.owner {
                Some(owner) => {
                    Some(board.owner(owner)?).filter(|owner| *owner != WorkOwner::Nobody)
                }
                None => existing.as_ref().and_then(|rule| rule.owner.clone()),
            },
        };
        let id = existing.as_ref().map(|rule| rule.id.as_str());
        let operation_id = match id {
            Some(id) => args.write.operation_id("triage-rule-save", &(id, &input))?,
            None => args.write.fresh_operation_id(),
        };
        let rule = if args.dry_run {
            self.rehearse(|tx| triage_auto::save_rule(tx, id, &input, now_ms()))?
        } else {
            self.applies("saving a triage rule")?;
            triage_auto::rule_save_as(self.app, &operation_id, id, &input)
                .map_err(|message| self.write_refusal(message, &operation_id, "The rule"))?
        };
        Ok(answered(AutomationsAnswer::TriageRule(
            ExternalTriageRuleChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action: AutomationAction::Saved,
                dry_run: args.dry_run,
                operation_id,
                rule,
            },
        )))
    }

    pub(super) fn triage_rule_delete(
        &self,
        args: &TriageIdArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let id = args.id.to_string();
        let operation_id = args.write.operation_id("triage-rule-delete", &id)?;
        let rule = if args.dry_run {
            self.rehearse(|tx| triage_auto::delete_rule(tx, &id))?
        } else {
            self.applies("deleting a triage rule")?;
            triage_auto::rule_delete_as(self.app, &operation_id, &id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The rule"))?
        };
        Ok(answered(AutomationsAnswer::TriageRule(
            ExternalTriageRuleChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action: AutomationAction::Deleted,
                dry_run: args.dry_run,
                operation_id,
                rule,
            },
        )))
    }

    pub(super) async fn triage_applied(&self) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let applied = crate::planning::history(self.history, |conn| triage_auto::applied(conn))
            .map_err(|message| refusal(self.history, message))?;
        let tasks = applied
            .into_iter()
            .filter_map(|(task_id, applied)| {
                board
                    .rows
                    .iter()
                    .find(|row| row.item.id == task_id)
                    .map(|row| ExternalAppliedTask {
                        task: task_ref_of(row),
                        applied,
                    })
            })
            .collect();
        Ok(answered(AutomationsAnswer::TriageApplied(
            ExternalTriageApplied {
                schema_version: QUERY_SCHEMA_VERSION,
                tasks,
            },
        )))
    }

    pub(super) async fn triage_undo(
        &self,
        args: &TriageIdArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let id = args.id.to_string();
        let applied = crate::planning::history(self.history, |conn| triage_auto::applied(conn))
            .map_err(|message| refusal(self.history, message))?;
        let Some(change) = applied.values().flatten().find(|change| change.id == id) else {
            return Err(missing(
                "change Sona made in Triage",
                &args.id,
                "List them with: sona work triage-applied",
            ));
        };
        let row = board
            .rows
            .iter()
            .find(|row| row.item.id == change.task_id)
            .ok_or_else(|| missing("task for change", &args.id, "Reload and try again."))?;
        let operation_id = args.write.operation_id("triage-undo", &id)?;
        if !args.dry_run {
            self.applies("undoing a change Sona made in Triage")?;
            triage_auto::undo_as(self.app, WorkActor::Agent, &operation_id, &id)
                .await
                .map_err(|message| self.write_refusal(message, &operation_id, "The change"))?;
        }
        Ok(answered(AutomationsAnswer::TriageUndo(
            ExternalTriageUndo {
                schema_version: QUERY_SCHEMA_VERSION,
                action: AutomationAction::Undone,
                dry_run: args.dry_run,
                operation_id,
                task: task_ref_of(row),
            },
        )))
    }

    pub(super) async fn housekeeping_undo(
        &self,
        args: &HousekeepingUndoArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let id = args.id.to_string();
        let operation_id = args.write.operation_id("housekeeping-undo", &id)?;
        let undone = if args.dry_run {
            self.rehearse(|tx| batches::rehearse_undo(tx, &id, now_ms()))?
        } else {
            self.applies("undoing what Sona closed or archived")?;
            batches::undo_as(self.app, WorkActor::Agent, &operation_id, &id)
                .await
                .map_err(|message| self.write_refusal(message, &operation_id, "The notice"))?
        };
        Ok(answered(AutomationsAnswer::HousekeepingUndo(
            ExternalHousekeepingUndo {
                schema_version: QUERY_SCHEMA_VERSION,
                action: AutomationAction::Undone,
                dry_run: args.dry_run,
                operation_id,
                notice_id: id,
                undone,
            },
        )))
    }
}

const fn kind_word(kind: DocProposalKind) -> &'static str {
    match kind {
        DocProposalKind::Note => "Add to note",
        DocProposalKind::ProjectDescription => "New description for",
        DocProposalKind::ProjectUpdate => "Update on",
    }
}

const fn field_word(field: TriageAutoField) -> &'static str {
    match field {
        TriageAutoField::Project => "project",
        TriageAutoField::Labels => "label",
        TriageAutoField::Owner => "owner",
    }
}

fn rule_line(text: &mut String, rule: &TriageRule) {
    let place = match rule.matches {
        TriageRuleMatch::Words => "words",
        TriageRuleMatch::Meeting => "meeting",
        TriageRuleMatch::Folder => "folder",
    };
    let mut sets = Vec::new();
    if let Some(project) = &rule.project_id {
        sets.push(format!("project {project}"));
    }
    if let Some(label) = &rule.label_id {
        sets.push(format!("label {label}"));
    }
    match &rule.owner {
        Some(WorkOwner::Myself) => sets.push("owner me".to_owned()),
        Some(WorkOwner::Person { person_id }) => sets.push(format!("owner {person_id}")),
        Some(WorkOwner::Nobody) | None => {}
    }
    let _ = writeln!(
        text,
        "{place} \"{}\" -> {}  {}",
        rule.value,
        sets.join(", "),
        rule.id
    );
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    let WorkAnswer::Automations(answer) = answer else {
        return;
    };
    let dry = |dry_run: bool, text: &mut String| {
        if dry_run {
            text.push_str("Dry run. ");
        }
    };
    match answer.as_ref() {
        AutomationsAnswer::DocProposals(list) => {
            if list.proposals.is_empty() {
                text.push_str("No proposals are waiting.\n");
            }
            for proposal in &list.proposals {
                let _ = writeln!(
                    text,
                    "{} {} (from {})  {}",
                    kind_word(proposal.kind),
                    proposal.target_title,
                    proposal.agent,
                    proposal.id
                );
                for line in proposal.body.lines() {
                    let _ = writeln!(text, "  {line}");
                }
            }
        }
        AutomationsAnswer::TriageRules(list) => {
            let fields: Vec<&str> = list.auto_apply.iter().copied().map(field_word).collect();
            let _ = writeln!(
                text,
                "Sona fills by itself: {}.",
                if fields.is_empty() {
                    "nothing".to_owned()
                } else {
                    fields.join(", ")
                }
            );
            if list.rules.is_empty() {
                text.push_str("No triage rules.\n");
            }
            for rule in &list.rules {
                rule_line(text, rule);
            }
        }
        AutomationsAnswer::TriageRule(change) => {
            dry(change.dry_run, text);
            let verb = match change.action {
                AutomationAction::Deleted => "Deleted",
                _ => "Saved",
            };
            let _ = write!(text, "{verb} rule: ");
            rule_line(text, &change.rule);
        }
        AutomationsAnswer::TriageApplied(list) => {
            if list.tasks.is_empty() {
                text.push_str("Sona filled nothing by itself in Triage.\n");
            }
            for task in &list.tasks {
                let _ = writeln!(text, "{} {}", task.task.handle, task.task.title);
                for applied in &task.applied {
                    let _ = writeln!(
                        text,
                        "  {}: {} ({})  {}",
                        field_word(applied.field),
                        applied.value,
                        applied.reason,
                        applied.id
                    );
                }
            }
        }
        AutomationsAnswer::TriageUndo(undo) => {
            dry(undo.dry_run, text);
            let _ = writeln!(
                text,
                "Undid Sona's change on {} {}.",
                undo.task.handle, undo.task.title
            );
        }
        AutomationsAnswer::HousekeepingUndo(undo) => {
            dry(undo.dry_run, text);
            let _ = writeln!(
                text,
                "Put back {} of what Sona closed or archived; left {} you had changed since.",
                undo.undone.restored, undo.undone.skipped
            );
        }
    }
}
