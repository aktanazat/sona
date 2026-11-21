//! `sona work` verbs for imports from other trackers: Linear's exports or
//! saved API answer, a CSV sheet, Jira, GitHub, Asana, Shortcut and Trello.
//! A Linear API key never passes through here; reading Linear live is the
//! app's, which keeps the key in the Keychain.

use super::{now_ms, refusal, Tracker, WorkAnswer, WriteArgs};
use crate::planning;
use crate::planning::work::import::batch::ImportSource;
use crate::planning::work::import::sheets::{self, CsvMap};
use crate::planning::work::import::{
    self, ImportAction, ImportCopy, ImportInclude, ImportOptions, ImportRecord, ImportReport,
    ImportReverted, ImportTally, DEFAULT_LABEL,
};
use crate::planning::work::types::WorkActor;
use crate::query::external::ExternalError;
use crate::query::QUERY_SCHEMA_VERSION;
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::fmt::Write as _;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum SourceArg {
    Linear,
    Csv,
    Jira,
    Github,
    Asana,
    Shortcut,
    Trello,
}

impl SourceArg {
    const fn source(self) -> ImportSource {
        match self {
            Self::Linear => ImportSource::Linear,
            Self::Csv => ImportSource::Csv,
            Self::Jira => ImportSource::Jira,
            Self::Github => ImportSource::Github,
            Self::Asana => ImportSource::Asana,
            Self::Shortcut => ImportSource::Shortcut,
            Self::Trello => ImportSource::Trello,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CopyArg {
    Labels,
    Projects,
    Milestones,
    Cycles,
    Initiatives,
    Comments,
    Documents,
    Views,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum IncludeArg {
    Active,
    Stale,
    Completed,
    All,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ImportArgs {
    /// The export files: Linear's issues, projects or initiatives CSV or a
    /// saved API answer (.json); a CSV sheet; Jira's or Asana's or
    /// Shortcut's CSV; GitHub issues JSON (one file per repository); a
    /// Trello board's JSON.
    #[arg(required = true, value_name = "FILE")]
    pub files: Vec<PathBuf>,
    /// The tracker the files come from.
    #[arg(long = "from", value_enum)]
    pub from: SourceArg,
    /// For a CSV sheet: which column fills which field, as JSON such as
    /// '{"title":"Summary","details":"Notes"}'. Fields: id, title, details,
    /// status, priority, owner, labels, estimate, due, created, completed,
    /// project, parent, url. Unnamed fields are found by their usual
    /// headers; `work import-template` prints Sona's own.
    #[arg(long, value_name = "JSON")]
    pub map: Option<String>,
    /// What to copy beside the tasks. Everything unless told.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "WHAT")]
    pub copy: Option<Vec<CopyArg>>,
    /// Which tasks: active (open, touched in six months), stale (open,
    /// older), completed, or all. Stale and completed land archived.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "WHICH")]
    pub include: Option<Vec<IncludeArg>>,
    /// The label every imported task gets, or none. Migrated unless told.
    #[arg(long, value_name = "NAME")]
    pub label: Option<String>,
    /// Give each task the number it had there (ENG-12 becomes S-12). Only
    /// while Sona has no tasks.
    #[arg(long)]
    pub keep_numbers: bool,
    /// Refresh tasks an earlier import brought in instead of skipping them.
    #[arg(long)]
    pub update_existing: bool,
    /// A name you go by there: tasks assigned to it become yours and your
    /// comments stay yours. Repeat for several.
    #[arg(long, value_name = "NAME")]
    pub me: Vec<String>,
    /// Show what would be imported, by state, without importing it.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ImportRevertArgs {
    /// The import's id, from `work imports`.
    pub import: String,
    /// Show what undoing it would remove without removing it.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkImport {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: String,
    #[serde(flatten)]
    pub report: ImportReport,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkImports {
    pub schema_version: u32,
    pub imports: Vec<ImportRecord>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkImportRevert {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: String,
    #[serde(flatten)]
    pub reverted: ImportReverted,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkImportTemplate {
    pub schema_version: u32,
    pub template: String,
}

fn options(args: &ImportArgs) -> ImportOptions {
    let copy = args
        .copy
        .as_ref()
        .map_or(ImportCopy::ALL, |wanted| ImportCopy {
            labels: wanted.contains(&CopyArg::Labels),
            projects: wanted.contains(&CopyArg::Projects),
            milestones: wanted.contains(&CopyArg::Milestones),
            cycles: wanted.contains(&CopyArg::Cycles),
            initiatives: wanted.contains(&CopyArg::Initiatives),
            comments: wanted.contains(&CopyArg::Comments),
            documents: wanted.contains(&CopyArg::Documents),
            views: wanted.contains(&CopyArg::Views),
        });
    let include = args.include.as_ref().map_or(ImportInclude::ALL, |wanted| {
        let all = wanted.contains(&IncludeArg::All);
        ImportInclude {
            active: all || wanted.contains(&IncludeArg::Active),
            stale: all || wanted.contains(&IncludeArg::Stale),
            completed: all || wanted.contains(&IncludeArg::Completed),
        }
    });
    let label = match args.label.as_deref().map(str::trim) {
        None => Some(DEFAULT_LABEL.to_owned()),
        Some(name) if name.is_empty() || name.eq_ignore_ascii_case("none") => None,
        Some(name) => Some(name.to_owned()),
    };
    ImportOptions {
        copy,
        include,
        update_existing: args.update_existing,
        keep_numbers: args.keep_numbers,
        label,
        me: args
            .me
            .iter()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect(),
    }
}

impl Tracker<'_> {
    pub(super) async fn import(&self, args: &ImportArgs) -> Result<WorkAnswer, ExternalError> {
        let source = args.from.source();
        let map: CsvMap = match &args.map {
            Some(json) if source == ImportSource::Csv => serde_json::from_str(json).map_err(|_| {
                ExternalError::invalid("--map takes JSON such as {\"title\":\"Summary\"}, naming id, title, details, status, priority, owner, labels, estimate, due, created, completed, project, parent or url.")
            })?,
            Some(_) => return Err(ExternalError::invalid("--map is for --from csv.")),
            None => CsvMap::new(),
        };
        let (batch, name) =
            import::read_paths(source, &args.files, &map).map_err(ExternalError::invalid)?;
        let options = options(args);
        let operation_id = args
            .write
            .operation_id("import", &(source, &name, import::digest(&batch), &options))?;
        let root = planning::work::attachment_root(self.app)
            .map_err(|message| refusal(self.history, message))?;
        let report = if args.dry_run {
            import::rehearse(self.app, &root, source, &name, &batch, &options)
                .map_err(|message| refusal(self.history, message))?
        } else {
            self.applies("importing from another tracker")?;
            let started_ms = now_ms();
            let report = import::write_as(
                self.app,
                WorkActor::Agent,
                &operation_id,
                source,
                &name,
                &batch,
                &options,
            )
            .map_err(|message| self.write_refusal(message, &operation_id, "The import"))?;
            if let Some(id) = &report.id {
                let tasks = planning::history(self.history, |conn| import::made_tasks(conn, id))
                    .map_err(|message| refusal(self.history, message))?;
                self.credit(&args.write, &tasks, started_ms)?;
            }
            report
        };
        Ok(WorkAnswer::Import(Box::new(ExternalWorkImport {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id,
            report,
        })))
    }

    pub(super) fn imports(&self) -> Result<WorkAnswer, ExternalError> {
        let imports = planning::history(self.history, |conn| import::imports(conn))
            .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::Imports(ExternalWorkImports {
            schema_version: QUERY_SCHEMA_VERSION,
            imports,
        }))
    }

    pub(super) fn import_revert(
        &self,
        args: &ImportRevertArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let found = planning::history(self.history, |conn| import::import(conn, &args.import))
            .map_err(|_| {
                ExternalError::invalid("No import has that id. List them with: sona work imports")
            })?;
        let operation_id = args
            .write
            .operation_id("import-revert", &(&found.id, found.reverted_at_ms))?;
        let reverted = if args.dry_run {
            planning::history(self.history, |conn| {
                import::rehearse_revert(conn, &found.id)
            })
            .map_err(|message| refusal(self.history, message))?
        } else {
            self.applies("undoing an import")?;
            import::revert_as(self.app, WorkActor::Agent, &operation_id, &found.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The import"))?
        };
        Ok(WorkAnswer::ImportRevert(Box::new(
            ExternalWorkImportRevert {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: args.dry_run,
                operation_id,
                reverted,
            },
        )))
    }
}

pub(super) fn import_template() -> WorkAnswer {
    WorkAnswer::ImportTemplate(ExternalWorkImportTemplate {
        schema_version: QUERY_SCHEMA_VERSION,
        template: sheets::template(),
    })
}

fn tally(text: &mut String, what: &str, tally: ImportTally) {
    let parts: Vec<String> = [
        (tally.made, "new"),
        (tally.archived, "archived"),
        (tally.matched, "already in Sona"),
        (tally.updated, "updated"),
        (tally.skipped, "imported before"),
        (tally.left_out, "left out"),
    ]
    .iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, word)| format!("{count} {word}"))
    .collect();
    if !parts.is_empty() {
        let _ = writeln!(text, "  {what}: {}", parts.join(", "));
    }
}

fn action_word(action: ImportAction) -> &'static str {
    match action {
        ImportAction::Made => "new",
        ImportAction::Archived => "archived",
        ImportAction::Matched => "in Sona",
        ImportAction::Updated => "updated",
        ImportAction::Skipped => "skipped",
        ImportAction::LeftOut => "left out",
    }
}

fn report_text(text: &mut String, dry_run: bool, report: &ImportReport) {
    let verb = if dry_run { "Would import" } else { "Imported" };
    let found = report.totals.found;
    let _ = writeln!(
        text,
        "{verb} from {} ({}): {} active, {} stale, {} completed read",
        report.source.name(),
        report.name,
        found.active,
        found.stale,
        found.completed
    );
    let totals = &report.totals;
    tally(text, "tasks", totals.tasks);
    tally(text, "projects", totals.projects);
    tally(text, "milestones", totals.milestones);
    tally(text, "labels", totals.labels);
    tally(text, "initiatives", totals.initiatives);
    tally(text, "comments", totals.comments);
    tally(text, "notes", totals.notes);
    tally(text, "views", totals.views);
    if totals.in_cycles > 0 {
        let _ = writeln!(
            text,
            "  {} tasks planned in Sona's cycles",
            totals.in_cycles
        );
    }
    if let Some(label) = &report.label {
        let _ = writeln!(text, "  each new task labeled {label}");
    }
    if let Some(id) = &report.id {
        let _ = writeln!(text, "  undo with: sona work import-revert {id}");
    }
    for note in &report.notes {
        let _ = writeln!(text, "  {note}");
    }
    for row in &report.rows {
        let reference = row
            .source_ref
            .as_deref()
            .map(|r| format!("{r} "))
            .unwrap_or_default();
        let reason = row
            .reason
            .as_deref()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        let kind = serde_json::to_value(row.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let _ = writeln!(
            text,
            "  {} {kind} {reference}{}{reason}",
            action_word(row.action),
            row.title
        );
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::Import(import) => report_text(text, import.dry_run, &import.report),
        WorkAnswer::Imports(list) => {
            if list.imports.is_empty() {
                text.push_str("No imports yet. Import with: sona work import FILE --from linear\n");
            }
            for record in &list.imports {
                let undone = if record.reverted_at_ms.is_some() {
                    " · undone"
                } else {
                    ""
                };
                let tasks = record.totals.tasks;
                let _ = writeln!(
                    text,
                    "{} · {} · {} · {} tasks, {} projects{undone}",
                    record.id,
                    record.source.name(),
                    record.name,
                    tasks.made + tasks.archived,
                    record.totals.projects.made + record.totals.projects.archived,
                );
            }
        }
        WorkAnswer::ImportRevert(revert) => {
            let undone = &revert.reverted.undone;
            let verb = if revert.dry_run {
                "Would undo"
            } else {
                "Undid"
            };
            let _ = writeln!(
                text,
                "{verb} {} ({}): {} tasks and {} projects to the bin for 30 days; {} milestones, {} labels, {} initiatives, {} notes and {} views removed",
                revert.reverted.import.name,
                revert.reverted.import.id,
                undone.tasks,
                undone.projects,
                undone.milestones,
                undone.labels,
                undone.initiatives,
                undone.notes,
                undone.views
            );
            if undone.notes_kept > 0 {
                let _ = writeln!(
                    text,
                    "  {} notes changed since were kept",
                    undone.notes_kept
                );
            }
            if undone.labels_kept > 0 {
                let _ = writeln!(
                    text,
                    "  {} labels other tasks carry were kept",
                    undone.labels_kept
                );
            }
        }
        WorkAnswer::ImportTemplate(template) => text.push_str(&template.template),
        _ => {}
    }
}
