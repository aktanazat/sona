//! The dictation history verbs: list or search it, keep an entry or stop
//! keeping it, and delete one, through the same history the Library screen
//! reads. Reading needs the same grant as `sona --query --scope dictations`.

use super::library::{file_made, made_before, told, Change, Gate};
use super::{Tracker, WorkAnswer, READ_SCOPES, WRITE_SCOPES};
use crate::managers::history::{HistoryEntry, HistoryManager, HistoryRunReceipt};
use crate::query::external::{label, ExternalError, ExternalErrorCode, ExternalScope};
use crate::query::QUERY_SCHEMA_VERSION;
use clap::{Args, Subcommand};
use serde::Serialize;
use std::fmt::Write as _;

const DEFAULT_ENTRIES: usize = 20;
const MAX_ENTRIES: usize = 100;
const DICTATION_DELETE: &str = "cli_dictation_delete";

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DictationsArgs {
    /// Only entries whose words match this.
    #[arg(long, value_name = "WORDS")]
    pub search: Option<String>,
    #[arg(long)]
    pub limit: Option<usize>,
    /// Entries older than this id, for the next page.
    #[arg(long, value_name = "ID")]
    pub before: Option<i64>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DictationSaveArgs {
    /// The entry's id, from `dictations`.
    #[arg(value_name = "ID")]
    pub id: i64,
    /// Stop keeping it.
    #[arg(long)]
    pub unsave: bool,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DictationDeleteArgs {
    /// The entry's id, from `dictations`.
    #[arg(value_name = "ID")]
    pub id: i64,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DictationReceiptsArgs {
    /// The entry's id, from `dictations`.
    #[arg(value_name = "ID")]
    pub id: i64,
}

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum DictationVerb {
    /// Dictation history, newest first, as the Library screen lists it.
    Dictations(DictationsArgs),
    /// Keep a dictation entry, or stop keeping it with --unsave.
    DictationSave(DictationSaveArgs),
    /// Delete a dictation entry and its recording.
    DictationDelete(DictationDeleteArgs),
    /// The runs a dictation went through: mode, model, words and delivery,
    /// without its text.
    DictationReceipts(DictationReceiptsArgs),
}

impl DictationVerb {
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self {
            Self::Dictations(_) | Self::DictationReceipts(_) => READ_SCOPES,
            Self::DictationSave(DictationSaveArgs { change, .. })
            | Self::DictationDelete(DictationDeleteArgs { change, .. }) => {
                if change.dry_run {
                    READ_SCOPES
                } else {
                    WRITE_SCOPES
                }
            }
        }
    }

    pub(super) fn check(&self) -> Result<(), ExternalError> {
        match self {
            Self::Dictations(DictationsArgs {
                search: Some(search),
                ..
            }) if search.trim().chars().count() < 2 => Err(ExternalError::invalid(
                "Search for at least two characters.",
            )),
            _ => Ok(()),
        }
    }
}

/// One dictation, with the words the Library row shows.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalDictation {
    pub id: i64,
    pub at_ms: i64,
    pub title: String,
    /// The finished words: the processed text when there is one.
    pub text: String,
    /// What the transcriber heard, before any processing.
    pub raw_text: String,
    pub saved: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalDictations {
    pub schema_version: u32,
    pub entries: Vec<ExternalDictation>,
    /// The id to pass as --before for the next page, when there is one.
    pub next_before: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalDictationChange {
    pub schema_version: u32,
    pub summary: String,
    pub dry_run: bool,
    /// True when an earlier run with the same operation id made it, or it
    /// already reads that way.
    pub replayed: bool,
    pub operation_id: String,
    pub dictation_id: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalDictationReceipts {
    pub schema_version: u32,
    pub dictation_id: i64,
    /// Oldest run first. A retry names the run it read again.
    pub receipts: Vec<HistoryRunReceipt>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ExternalDictationAnswer {
    Entries(ExternalDictations),
    Change(ExternalDictationChange),
    Receipts(ExternalDictationReceipts),
}

fn unreadable(history: &HistoryManager) -> ExternalError {
    if history.storage_is_ready() {
        ExternalError::new(
            ExternalErrorCode::Failed,
            "Dictation history could not be read.",
        )
    } else {
        ExternalError::new(
            ExternalErrorCode::Unavailable,
            "Dictation history is locked or not set up.",
        )
    }
}

fn missing(id: i64) -> ExternalError {
    ExternalError::new(ExternalErrorCode::NotFound, format!("No dictation {id}."))
        .hint("List them with: sona work dictations")
}

fn shown(entry: HistoryEntry) -> ExternalDictation {
    ExternalDictation {
        id: entry.id,
        at_ms: entry.timestamp * 1_000,
        title: entry.title,
        text: entry
            .post_processed_text
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| entry.transcription_text.clone()),
        raw_text: entry.transcription_text,
        saved: entry.saved,
    }
}

async fn entry(history: &HistoryManager, id: i64) -> Result<HistoryEntry, ExternalError> {
    history
        .get_entry_by_id(id)
        .await
        .map_err(|_| unreadable(history))?
        .ok_or_else(|| missing(id))
}

pub(super) async fn list(
    history: &HistoryManager,
    args: &DictationsArgs,
) -> Result<ExternalDictations, ExternalError> {
    history.ensure_storage_unlocked().await;
    let limit = Some(args.limit.unwrap_or(DEFAULT_ENTRIES).clamp(1, MAX_ENTRIES));
    let page = match args.search.as_deref() {
        Some(search) => {
            history
                .search_history_entries(search, args.before, limit)
                .await
        }
        None => history.get_history_entries(args.before, limit).await,
    }
    .map_err(|_| unreadable(history))?;
    let next_before = page
        .has_more
        .then(|| page.entries.last().map(|entry| entry.id))
        .flatten();
    Ok(ExternalDictations {
        schema_version: QUERY_SCHEMA_VERSION,
        entries: page.entries.into_iter().map(shown).collect(),
        next_before,
    })
}

async fn receipts(
    history: &HistoryManager,
    args: &DictationReceiptsArgs,
) -> Result<ExternalDictationReceipts, ExternalError> {
    history.ensure_storage_unlocked().await;
    entry(history, args.id).await?;
    Ok(ExternalDictationReceipts {
        schema_version: QUERY_SCHEMA_VERSION,
        dictation_id: args.id,
        receipts: history
            .get_run_receipts(args.id)
            .await
            .map_err(|_| unreadable(history))?,
    })
}

pub(super) async fn save(
    history: &HistoryManager,
    args: &DictationSaveArgs,
    gate: Gate<'_>,
) -> Result<ExternalDictationChange, ExternalError> {
    history.ensure_storage_unlocked().await;
    let keep = !args.unsave;
    let found = entry(history, args.id).await?;
    let dry_run = args.change.dry_run;
    let summary = if keep {
        told(dry_run, "Kept the dictation", "Would keep the dictation")
    } else {
        told(
            dry_run,
            "Stopped keeping the dictation",
            "Would stop keeping the dictation",
        )
    };
    let operation_id = args
        .change
        .write
        .operation_id("dictation-save", &(args.id, keep))?;
    let answer = ExternalDictationChange {
        schema_version: QUERY_SCHEMA_VERSION,
        summary,
        dry_run,
        replayed: found.saved == keep,
        operation_id,
        dictation_id: args.id,
    };
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate(if keep {
        "keeping a dictation"
    } else {
        "unkeeping a dictation"
    })?;
    history
        .toggle_saved_status(args.id)
        .await
        .map_err(|_| unreadable(history))?;
    Ok(answer)
}

pub(super) async fn delete(
    history: &HistoryManager,
    args: &DictationDeleteArgs,
    gate: Gate<'_>,
) -> Result<ExternalDictationChange, ExternalError> {
    history.ensure_storage_unlocked().await;
    let dry_run = args.change.dry_run;
    let request = args.id.to_string();
    let mut answer = ExternalDictationChange {
        schema_version: QUERY_SCHEMA_VERSION,
        summary: told(
            dry_run,
            "Deleted the dictation",
            "Would delete the dictation",
        ),
        dry_run,
        replayed: false,
        operation_id: args
            .change
            .write
            .operation_id("dictation-delete", &args.id)?,
        dictation_id: args.id,
    };
    if made_before(history, &answer.operation_id, DICTATION_DELETE, &request)? {
        answer.replayed = true;
        return Ok(answer);
    }
    entry(history, args.id).await?;
    if dry_run {
        return Ok(answer);
    }
    gate("deleting a dictation")?;
    history
        .delete_entry(args.id)
        .await
        .map_err(|_| unreadable(history))?;
    file_made(history, &answer.operation_id, DICTATION_DELETE, &request)?;
    Ok(answer)
}

impl Tracker<'_> {
    pub(super) async fn dictations(
        &self,
        verb: &DictationVerb,
    ) -> Result<WorkAnswer, ExternalError> {
        let gate = |what: &str| self.applies(what);
        let answer = match verb {
            DictationVerb::Dictations(args) => {
                ExternalDictationAnswer::Entries(list(self.history, args).await?)
            }
            DictationVerb::DictationSave(args) => {
                ExternalDictationAnswer::Change(save(self.history, args, &gate).await?)
            }
            DictationVerb::DictationDelete(args) => {
                ExternalDictationAnswer::Change(delete(self.history, args, &gate).await?)
            }
            DictationVerb::DictationReceipts(args) => {
                ExternalDictationAnswer::Receipts(receipts(self.history, args).await?)
            }
        };
        Ok(WorkAnswer::Dictations(Box::new(answer)))
    }
}

pub(super) fn text(answer: &ExternalDictationAnswer, text: &mut String) {
    match answer {
        ExternalDictationAnswer::Entries(list) => {
            if list.entries.is_empty() {
                text.push_str("No dictations.\n");
            }
            for entry in &list.entries {
                let _ = writeln!(
                    text,
                    "{}{}  {}  {}",
                    entry.id,
                    if entry.saved { " (kept)" } else { "" },
                    entry.title,
                    entry.text.trim()
                );
            }
            if let Some(before) = list.next_before {
                let _ = writeln!(text, "More with: --before {before}");
            }
        }
        ExternalDictationAnswer::Change(change) => {
            let _ = writeln!(
                text,
                "{}{}",
                change.summary,
                if change.replayed && !change.dry_run {
                    " (already so)"
                } else {
                    ""
                }
            );
        }
        ExternalDictationAnswer::Receipts(list) => {
            if list.receipts.is_empty() {
                text.push_str("No runs recorded for this dictation.\n");
            }
            for receipt in &list.receipts {
                let _ = write!(text, "run {}", receipt.run_id);
                if let Some(retry_of) = receipt.retry_of_run_id {
                    let _ = write!(text, " (retry of run {retry_of})");
                }
                let _ = write!(text, "  mode {}", receipt.mode.mode_id);
                if let Some(model) = &receipt.mode.model_id {
                    let _ = write!(text, "  model {model}");
                }
                if let Some(words) = receipt.word_count {
                    let _ = write!(text, "  {words} words");
                }
                let delivery = receipt.delivery_attempts.last().map_or_else(
                    || "not delivered".to_owned(),
                    |attempt| label(&attempt.delivery.outcome).replace('_', " "),
                );
                let _ = writeln!(text, "  {delivery}");
            }
        }
    }
}
