//! `sona work` verbs for task templates, reminders, label groups and
//! retired labels, and a decline's reason.

use super::{
    now_ms, pick, project_label, refusal, repeat, task_ref, task_ref_of, Board,
    ExternalFieldChange, ExternalWorkChange, ExternalWorkLabel, ExternalWorkPlanned,
    ExternalWorkPreview, ExternalWorkRef, Priority, RepeatArg, TaskRef, Tracker, WorkAction,
    WorkAnswer, WriteArgs,
};
use crate::planning;
use crate::planning::work::inbox::SubjectKind;
use crate::planning::work::parse;
use crate::planning::work::routines::{
    self, SubjectReminder, WorkReminder, WorkTemplate, WorkTemplateUse,
};
use crate::planning::work::types::{WorkActor, WorkLabel, WorkOwner};
use crate::planning::LabelScope;
use crate::query::external::{local_time, ExternalError};
use crate::query::QUERY_SCHEMA_VERSION;
use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
use clap::Args;
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DeclineArgs {
    /// S-12, 12, or task ids.
    #[arg(required = true, value_parser = task_ref, value_name = "TASK")]
    pub tasks: Vec<TaskRef>,
    /// Why, kept in each task's history.
    #[arg(long)]
    pub reason: Option<String>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TemplateSaveArgs {
    /// The template's name.
    pub name: String,
    /// The template to change, by name or id; left out, the one called NAME
    /// or a new one.
    #[arg(long, value_name = "TEMPLATE")]
    pub template: Option<String>,
    /// The task's title; left out, the template's name.
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub details: Option<String>,
    #[arg(long, value_enum)]
    pub priority: Option<Priority>,
    /// me, nobody, or a person's name or id.
    #[arg(long)]
    pub owner: Option<String>,
    /// A project by name or id, or none.
    #[arg(long)]
    pub project: Option<String>,
    /// Labels by name or id, comma-separated; "" clears them.
    #[arg(long, value_delimiter = ',', value_name = "LABEL")]
    pub labels: Option<Vec<String>>,
    /// Due this many days after the day the task is made; none clears it.
    #[arg(long, value_name = "DAYS", value_parser = days)]
    pub due_in: Option<DueIn>,
    /// How often the task repeats: every monday, every 2 weeks, daily; none.
    #[arg(long, value_parser = repeat, value_name = "WORDS")]
    pub repeat: Option<RepeatArg>,
    /// A sub-task title, once per sub-task; replaces the template's. One
    /// empty title clears them.
    #[arg(long = "subtask", value_name = "TITLE")]
    pub subtasks: Option<Vec<String>>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TemplateDeleteArgs {
    /// The template, by name or id.
    pub template: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TemplateUseArgs {
    /// The template, by name or id.
    pub template: String,
    /// The task's title in place of the template's.
    #[arg(long)]
    pub title: Option<String>,
    /// Due day in place of the template's: YYYY-MM-DD, today or tomorrow.
    #[arg(long, value_parser = super::day)]
    pub due: Option<NaiveDate>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

/// When a reminder goes off, or none to clear it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WhenArg {
    Clear,
    At(i64),
}

impl WhenArg {
    const fn ms(self) -> Option<i64> {
        match self {
            Self::Clear => None,
            Self::At(at) => Some(at),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DueIn(Option<u32>);

fn days(value: &str) -> Result<DueIn, String> {
    if value.trim().eq_ignore_ascii_case("none") {
        return Ok(DueIn(None));
    }
    value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|days| *days <= 3650)
        .map(|days| DueIn(Some(days)))
        .ok_or_else(|| "expected a number of days from 0 to 3650, or none".to_owned())
}

fn when(value: &str) -> Result<WhenArg, String> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Ok(WhenArg::Clear);
    }
    let invalid = || {
        "expected a time such as 2026-11-12 09:00, tomorrow 9am or friday 3pm, or none".to_owned()
    };
    for format in ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M"] {
        if let Ok(at) = NaiveDateTime::parse_from_str(value, format) {
            return Local
                .from_local_datetime(&at)
                .earliest()
                .map(|at| WhenArg::At(at.timestamp_millis()))
                .ok_or_else(invalid);
        }
    }
    let read = parse::parse(
        value,
        parse::Said::Typed,
        &[],
        &parse::Lexicon::default(),
        &Local::now(),
    );
    match read.tokens.as_slice() {
        [parse::WorkToken {
            field: parse::WorkField::Due { at, .. },
            ..
        }] if read.title.is_empty() => Ok(WhenArg::At(*at)),
        _ => Err(invalid()),
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct RemindArgs {
    /// The task (S-12, 12, or its id) and when; with --project, --initiative
    /// or --note only when. When: 2026-11-12 09:00, tomorrow 9am, friday
    /// 3pm; none clears it.
    #[arg(value_name = "[TASK] WHEN", num_args = 1..=2, required = true)]
    pub args: Vec<String>,
    /// Remind of a project, by name or id, in place of a task.
    #[arg(long, value_name = "PROJECT", conflicts_with_all = ["initiative", "note"])]
    pub project: Option<String>,
    /// Remind of an initiative, by name or id.
    #[arg(long, value_name = "INITIATIVE", conflicts_with = "note")]
    pub initiative: Option<String>,
    /// Remind of a note, by id.
    #[arg(long, value_name = "NOTE")]
    pub note: Option<uuid::Uuid>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

impl RemindArgs {
    const fn names_subject(&self) -> bool {
        self.project.is_some() || self.initiative.is_some() || self.note.is_some()
    }

    /// The task named, none for a project, initiative or note, and when.
    fn read(&self) -> Result<(Option<TaskRef>, WhenArg), ExternalError> {
        let (task, at) = match (self.names_subject(), self.args.as_slice()) {
            (true, [at]) => (None, at),
            (false, [task, at]) => (Some(task_ref(task).map_err(ExternalError::invalid)?), at),
            (true, _) => {
                return Err(ExternalError::invalid(
                    "With --project, --initiative or --note, name only when.",
                ))
            }
            (false, _) => return Err(ExternalError::invalid("Name the task and when.")),
        };
        Ok((task, when(at).map_err(ExternalError::invalid)?))
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct RemindersArgs {
    /// Only reminders whose time has come and that Sona has not shown yet.
    #[arg(long)]
    pub due: bool,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LabelGroupArgs {
    /// The label, by name or id.
    pub label: String,
    /// The group's name, or none to take it out of its group.
    pub group: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

/// What a label may go on.
#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum LabelScopeArg {
    All,
    Tasks,
    Projects,
}

impl From<LabelScopeArg> for LabelScope {
    fn from(scope: LabelScopeArg) -> Self {
        match scope {
            LabelScopeArg::All => Self::All,
            LabelScopeArg::Tasks => Self::Tasks,
            LabelScopeArg::Projects => Self::Projects,
        }
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LabelScopeArgs {
    /// The label, by name or id.
    pub label: String,
    /// all, tasks or projects. Narrowing is refused while the label is on
    /// the kind it would leave out.
    #[arg(value_enum)]
    pub scope: LabelScopeArg,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LabelRetireArgs {
    /// The label, by name or id.
    pub label: String,
    /// Bring a retired label back to the pickers.
    #[arg(long)]
    pub restore: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkTemplate {
    pub id: String,
    pub name: String,
    pub title: String,
    pub details: Option<String>,
    pub priority: u8,
    /// you, nobody, or a person's name.
    pub owner: String,
    pub project: Option<String>,
    pub labels: Vec<String>,
    pub due_in_days: Option<u32>,
    pub repeat: Option<String>,
    pub subtasks: Vec<String>,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkTemplates {
    pub schema_version: u32,
    pub templates: Vec<ExternalWorkTemplate>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineAction {
    Saved,
    Deleted,
    Unchanged,
    Set,
    Cleared,
    Grouped,
    Ungrouped,
    Retired,
    Restored,
    Scoped,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkTemplateChange {
    pub schema_version: u32,
    pub action: RoutineAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    /// For a dry run of a new template, its id is empty.
    pub template: ExternalWorkTemplate,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkReminder {
    pub task: ExternalWorkRef,
    pub remind_at_ms: i64,
    pub remind_at: String,
    pub set_by: WorkActor,
    pub agent: Option<String>,
    /// When Sona showed it; none until then.
    pub notified_at_ms: Option<i64>,
    pub due: bool,
}

/// The project, initiative or note a reminder is on.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalReminded {
    pub kind: SubjectKind,
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSubjectReminder {
    pub subject: ExternalReminded,
    pub remind_at_ms: i64,
    pub remind_at: String,
    pub set_by: WorkActor,
    pub agent: Option<String>,
    /// When Sona posted it to the inbox; none until then.
    pub notified_at_ms: Option<i64>,
    pub due: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkReminders {
    pub schema_version: u32,
    pub due_only: bool,
    /// Reminders on tasks.
    pub reminders: Vec<ExternalWorkReminder>,
    /// Reminders on projects, initiatives and notes.
    pub subjects: Vec<ExternalSubjectReminder>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkReminderChange {
    pub schema_version: u32,
    pub action: RoutineAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub task: ExternalWorkRef,
    /// None once cleared.
    pub reminder: Option<ExternalWorkReminder>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSubjectReminderChange {
    pub schema_version: u32,
    pub action: RoutineAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub subject: ExternalReminded,
    /// None once cleared.
    pub reminder: Option<ExternalSubjectReminder>,
}

fn subject_reminder_of(
    subject: ExternalReminded,
    reminder: SubjectReminder,
    now: i64,
) -> ExternalSubjectReminder {
    ExternalSubjectReminder {
        subject,
        remind_at: local_time(reminder.remind_at_ms),
        due: reminder.notified_at_ms.is_none() && reminder.remind_at_ms <= now,
        remind_at_ms: reminder.remind_at_ms,
        set_by: reminder.actor,
        agent: reminder.agent,
        notified_at_ms: reminder.notified_at_ms,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkLabelEdit {
    pub schema_version: u32,
    pub action: RoutineAction,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub label: ExternalWorkLabel,
}

pub(super) fn reminder_of(
    row: &super::WorkItemRow,
    reminder: WorkReminder,
    now: i64,
) -> ExternalWorkReminder {
    ExternalWorkReminder {
        task: task_ref_of(row),
        remind_at: local_time(reminder.remind_at_ms),
        due: reminder.notified_at_ms.is_none() && reminder.remind_at_ms <= now,
        remind_at_ms: reminder.remind_at_ms,
        set_by: reminder.actor,
        agent: reminder.agent,
        notified_at_ms: reminder.notified_at_ms,
    }
}

impl Board {
    fn template_of(&self, template: &WorkTemplate) -> ExternalWorkTemplate {
        let fields = &template.fields;
        ExternalWorkTemplate {
            id: template.id.clone(),
            name: template.name.clone(),
            title: fields
                .title
                .clone()
                .unwrap_or_else(|| template.name.clone()),
            details: fields.details.clone(),
            priority: fields.priority.unwrap_or(0),
            owner: self.owner_text(fields.owner.as_ref().unwrap_or(&WorkOwner::Nobody)),
            project: fields.project_id.as_deref().map(|id| self.project_name(id)),
            labels: fields
                .label_ids
                .iter()
                .map(|id| self.label_name(id))
                .collect(),
            due_in_days: fields.due_in_days,
            repeat: fields.recurrence.clone(),
            subtasks: fields.subtasks.clone(),
            revision: template.revision,
        }
    }
}

impl Tracker<'_> {
    fn all_templates(&self) -> Result<Vec<WorkTemplate>, ExternalError> {
        planning::history(self.history, |conn| routines::templates(conn))
            .map_err(|message| refusal(self.history, message))
    }

    fn template_named<'t>(
        templates: &'t [WorkTemplate],
        value: &str,
    ) -> Result<&'t WorkTemplate, ExternalError> {
        pick("template", value, templates, |t| &t.id, |t| &t.name)
            .map_err(|refusal| refusal.hint("List them with: sona work templates"))
    }

    fn label_named(&self, value: &str) -> Result<WorkLabel, ExternalError> {
        let labels = self.all_labels()?;
        pick("label", value, &labels, |l| &l.id, |l| &l.name)
            .cloned()
            .map_err(|refusal| refusal.hint("List them with: sona work labels"))
    }

    pub(super) async fn templates(&self) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let templates = self.all_templates()?;
        Ok(WorkAnswer::Templates(ExternalWorkTemplates {
            schema_version: QUERY_SCHEMA_VERSION,
            templates: templates.iter().map(|t| board.template_of(t)).collect(),
        }))
    }

    pub(super) async fn template_save(
        &self,
        args: &TemplateSaveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let name = args.name.trim();
        if name.is_empty() {
            return Err(ExternalError::invalid("A template needs a name."));
        }
        let board = self.board().await?;
        let templates = self.all_templates()?;
        let existing = match &args.template {
            Some(value) => Some(Self::template_named(&templates, value)?),
            None => templates
                .iter()
                .find(|t| t.name.to_lowercase() == name.to_lowercase()),
        };
        if let Some(taken) = templates.iter().find(|t| {
            t.name.to_lowercase() == name.to_lowercase()
                && existing.is_none_or(|existing| existing.id != t.id)
        }) {
            return Err(ExternalError::invalid(format!(
                "A template called {:?} already exists.",
                taken.name
            )));
        }
        let mut fields = existing.map(|t| t.fields.clone()).unwrap_or_default();
        if let Some(title) = &args.title {
            fields.title = Some(title.clone()).filter(|title| !title.trim().is_empty());
        }
        if let Some(details) = &args.details {
            fields.details = Some(details.clone()).filter(|details| !details.trim().is_empty());
        }
        if let Some(priority) = args.priority {
            fields.priority = Some(priority.value());
        }
        if let Some(owner) = &args.owner {
            fields.owner = Some(board.owner(owner)?);
        }
        if let Some(project) = &args.project {
            fields.project_id = board.project(project)?.map(|project| project.id.clone());
        }
        if let Some(labels) = &args.labels {
            fields.label_ids = board.one_per_group(&board.label_ids(labels)?, &[]);
        }
        if let Some(DueIn(due_in)) = args.due_in {
            fields.due_in_days = due_in;
        }
        if let Some(repeat) = &args.repeat {
            fields.recurrence = repeat.every();
        }
        if let Some(subtasks) = &args.subtasks {
            fields.subtasks = subtasks
                .iter()
                .map(|title| title.trim().to_owned())
                .filter(|title| !title.is_empty())
                .collect();
        }
        if let Some(existing) = existing {
            if existing.name == name && existing.fields == fields {
                return Ok(WorkAnswer::Template(Box::new(ExternalWorkTemplateChange {
                    schema_version: QUERY_SCHEMA_VERSION,
                    action: RoutineAction::Unchanged,
                    dry_run: args.dry_run,
                    operation_id: None,
                    template: board.template_of(existing),
                })));
            }
        }
        let operation_id = match existing {
            Some(t) => args
                .write
                .operation_id("template-save", &(&t.id, t.revision, name, &fields))?,
            None => args.write.fresh_operation_id(),
        };
        if args.dry_run {
            let would = WorkTemplate {
                id: existing.map(|t| t.id.clone()).unwrap_or_default(),
                name: name.to_owned(),
                fields,
                revision: existing.map_or(1, |t| t.revision + 1),
                created_at_ms: existing.map_or_else(now_ms, |t| t.created_at_ms),
                updated_at_ms: now_ms(),
            };
            return Ok(WorkAnswer::Template(Box::new(ExternalWorkTemplateChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action: RoutineAction::Saved,
                dry_run: true,
                operation_id: Some(operation_id),
                template: board.template_of(&would),
            })));
        }
        self.applies("saving a template")?;
        let saved = routines::template_save_as(
            self.app,
            &operation_id,
            existing.map(|t| t.id.as_str()),
            name,
            &fields,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The template"))?;
        Ok(WorkAnswer::Template(Box::new(ExternalWorkTemplateChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: RoutineAction::Saved,
            dry_run: false,
            operation_id: Some(operation_id),
            template: board.template_of(&saved),
        })))
    }

    pub(super) async fn template_delete(
        &self,
        args: &TemplateDeleteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let templates = self.all_templates()?;
        let template = Self::template_named(&templates, &args.template)?;
        let operation_id = args
            .write
            .operation_id("template-delete", &(&template.id, template.revision))?;
        if !args.dry_run {
            self.applies("deleting a template")?;
            routines::template_delete_as(self.app, &operation_id, &template.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The template"))?;
        }
        Ok(WorkAnswer::Template(Box::new(ExternalWorkTemplateChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: RoutineAction::Deleted,
            dry_run: args.dry_run,
            operation_id: Some(operation_id),
            template: board.template_of(template),
        })))
    }

    pub(super) async fn template_use(
        &self,
        args: &TemplateUseArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let templates = self.all_templates()?;
        let template = Self::template_named(&templates, &args.template)?;
        let due_at_ms = args.due.map(super::midnight).transpose()?;
        let title = args
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned);
        let using = WorkTemplateUse {
            title: title.clone(),
            due_at_ms,
            status: None,
        };
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let operation_id = args.write.operation_id(
            "template-use",
            &(&template.id, template.revision, &title, due_at_ms, today),
        )?;
        if args.dry_run {
            let shown = board.template_of(template);
            let parent_title = title.unwrap_or_else(|| shown.title.clone());
            let mut changes = vec![ExternalWorkPlanned {
                id: String::new(),
                handle: "new".to_owned(),
                title: parent_title.clone(),
                fields: vec![
                    ExternalFieldChange {
                        field: "template",
                        from: "none".to_owned(),
                        to: shown.name.clone(),
                    },
                    ExternalFieldChange {
                        field: "owner",
                        from: "nobody".to_owned(),
                        to: shown.owner.clone(),
                    },
                ],
                commitment: None,
                skipped: None,
            }];
            changes.extend(shown.subtasks.iter().map(|subtask| ExternalWorkPlanned {
                id: String::new(),
                handle: "new".to_owned(),
                title: subtask.clone(),
                fields: vec![ExternalFieldChange {
                    field: "parent",
                    from: "none".to_owned(),
                    to: parent_title.clone(),
                }],
                commitment: None,
                skipped: None,
            }));
            return Ok(WorkAnswer::Preview(ExternalWorkPreview {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                action: WorkAction::Created,
                operation_id: Some(operation_id),
                changes,
            }));
        }
        let started_ms = now_ms();
        let items = routines::agent_template_use_as(
            self.app,
            &args.write.agent(),
            &operation_id,
            &template.id,
            &using,
        )
        .await
        .map_err(|message| self.write_refusal(message, &operation_id, "The template"))?;
        let replayed = items.iter().all(|item| item.created_at_ms < started_ms);
        let board = self.board().await?;
        Ok(WorkAnswer::Change(ExternalWorkChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: WorkAction::Created,
            operation_id: Some(operation_id),
            tasks: items
                .into_iter()
                .map(|item| board.project_item(item))
                .collect(),
            skipped: Vec::new(),
            proposed: Vec::new(),
            replayed,
            evidence_url: None,
        }))
    }

    /// The name a project, initiative or note goes by now; none once gone.
    fn reminded_name(&self, kind: SubjectKind, id: &str) -> Result<Option<String>, ExternalError> {
        Ok(match kind {
            SubjectKind::Project => self
                .every_project()?
                .into_iter()
                .find(|project| project.id == id)
                .map(|project| project.title),
            SubjectKind::Initiative => self
                .all_initiatives()?
                .into_iter()
                .find(|initiative| initiative.id == id)
                .map(|initiative| initiative.name),
            SubjectKind::Note => uuid::Uuid::parse_str(id)
                .ok()
                .and_then(|id| self.note(&id).ok())
                .map(|note| note.title),
            SubjectKind::View => None,
        })
    }

    pub(super) async fn reminders(
        &self,
        args: &RemindersArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let now = now_ms();
        let (found, others) = planning::history(self.history, |conn| {
            Ok(if args.due {
                (
                    routines::due_reminders(conn, now)?,
                    routines::due_subject_reminders(conn, now)?,
                )
            } else {
                (
                    routines::reminders(conn)?,
                    routines::subject_reminders(conn)?,
                )
            })
        })
        .map_err(|message| refusal(self.history, message))?;
        let mut subjects = Vec::new();
        for reminder in others {
            let Some(name) = self.reminded_name(reminder.subject_kind, &reminder.subject_id)?
            else {
                continue;
            };
            let subject = ExternalReminded {
                kind: reminder.subject_kind,
                id: reminder.subject_id.clone(),
                name,
            };
            subjects.push(subject_reminder_of(subject, reminder, now));
        }
        Ok(WorkAnswer::Reminders(ExternalWorkReminders {
            schema_version: QUERY_SCHEMA_VERSION,
            due_only: args.due,
            reminders: found
                .into_iter()
                .filter_map(|reminder| {
                    let row = board.row(&reminder.item_id)?;
                    Some(reminder_of(row, reminder, now))
                })
                .collect(),
            subjects,
        }))
    }

    /// The project, initiative or note `args` names.
    async fn reminded(&self, args: &RemindArgs) -> Result<ExternalReminded, ExternalError> {
        if let Some(project) = &args.project {
            let project = self.project_named(project)?;
            return Ok(ExternalReminded {
                kind: SubjectKind::Project,
                id: project.id,
                name: project.title,
            });
        }
        if let Some(initiative) = &args.initiative {
            let all = self.all_initiatives()?;
            let initiative = Self::pick_initiative(initiative, &all)?;
            return Ok(ExternalReminded {
                kind: SubjectKind::Initiative,
                id: initiative.id.clone(),
                name: initiative.name.clone(),
            });
        }
        let id = args.note.ok_or_else(|| {
            ExternalError::invalid("Name a task, --project, --initiative or --note.")
        })?;
        let note = self.note(&id)?;
        Ok(ExternalReminded {
            kind: SubjectKind::Note,
            id: note.id,
            name: note.title,
        })
    }

    async fn remind_subject(
        &self,
        args: &RemindArgs,
        at_ms: Option<i64>,
    ) -> Result<WorkAnswer, ExternalError> {
        let subject = self.reminded(args).await?;
        let now = now_ms();
        let current = planning::history(self.history, |conn| routines::subject_reminders(conn))
            .map_err(|message| refusal(self.history, message))?
            .into_iter()
            .find(|reminder| {
                reminder.subject_kind == subject.kind && reminder.subject_id == subject.id
            });
        let action = if at_ms.is_some() {
            RoutineAction::Set
        } else {
            RoutineAction::Cleared
        };
        let operation_id = args.write.operation_id(
            "remind",
            &(
                subject.kind,
                &subject.id,
                at_ms,
                current.as_ref().map(|reminder| reminder.set_at_ms),
            ),
        )?;
        let agent = args.write.agent();
        let reminder = if args.dry_run {
            at_ms.map(|at| SubjectReminder {
                subject_kind: subject.kind,
                subject_id: subject.id.clone(),
                remind_at_ms: at,
                set_at_ms: now,
                actor: WorkActor::Agent,
                agent: Some(agent.clone()),
                notified_at_ms: None,
            })
        } else {
            self.applies(match action {
                RoutineAction::Set => "setting a reminder",
                _ => "clearing a reminder",
            })?;
            routines::subject_reminder_set_as(
                self.app,
                WorkActor::Agent,
                Some(&agent),
                &operation_id,
                subject.kind,
                &subject.id,
                at_ms,
            )
            .map_err(|message| self.write_refusal(message, &operation_id, "What you named"))?
        };
        Ok(WorkAnswer::SubjectReminder(Box::new(
            ExternalSubjectReminderChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action,
                dry_run: args.dry_run,
                operation_id: Some(operation_id),
                reminder: reminder
                    .map(|reminder| subject_reminder_of(subject.clone(), reminder, now)),
                subject,
            },
        )))
    }

    pub(super) async fn remind(&self, args: &RemindArgs) -> Result<WorkAnswer, ExternalError> {
        let (task, when) = args.read()?;
        let at_ms = when.ms();
        let Some(task) = task else {
            return self.remind_subject(args, at_ms).await;
        };
        let board = self.board().await?;
        let row = board.task(&task)?;
        let now = now_ms();
        let current = planning::history(self.history, |conn| routines::reminders(conn))
            .map_err(|message| refusal(self.history, message))?
            .into_iter()
            .find(|reminder| reminder.item_id == row.item.id);
        let action = if at_ms.is_some() {
            RoutineAction::Set
        } else {
            RoutineAction::Cleared
        };
        let operation_id = args.write.operation_id(
            "remind",
            &(
                &row.item.id,
                at_ms,
                current.as_ref().map(|reminder| reminder.set_at_ms),
            ),
        )?;
        let agent = args.write.agent();
        if args.dry_run {
            let would = at_ms.map(|at| WorkReminder {
                item_id: row.item.id.clone(),
                remind_at_ms: at,
                set_at_ms: now,
                actor: WorkActor::Agent,
                agent: Some(agent.clone()),
                notified_at_ms: None,
            });
            return Ok(WorkAnswer::Reminder(Box::new(ExternalWorkReminderChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action,
                dry_run: true,
                operation_id: Some(operation_id),
                task: task_ref_of(row),
                reminder: would.map(|reminder| reminder_of(row, reminder, now)),
            })));
        }
        self.applies(match action {
            RoutineAction::Set => "setting a reminder",
            _ => "clearing a reminder",
        })?;
        let reminder = routines::reminder_set_as(
            self.app,
            WorkActor::Agent,
            Some(&agent),
            &operation_id,
            &row.item.id,
            at_ms,
        )
        .map_err(|message| self.write_refusal(message, &operation_id, "The task"))?;
        Ok(WorkAnswer::Reminder(Box::new(ExternalWorkReminderChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run: false,
            operation_id: Some(operation_id),
            task: task_ref_of(row),
            reminder: reminder.map(|reminder| reminder_of(row, reminder, now_ms())),
        })))
    }

    fn label_edit(
        action: RoutineAction,
        dry_run: bool,
        operation_id: Option<String>,
        label: WorkLabel,
    ) -> WorkAnswer {
        WorkAnswer::LabelEdit(ExternalWorkLabelEdit {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            dry_run,
            operation_id,
            label: project_label(label),
        })
    }

    pub(super) fn label_group(&self, args: &LabelGroupArgs) -> Result<WorkAnswer, ExternalError> {
        let label = self.label_named(&args.label)?;
        let group = Some(args.group.trim())
            .filter(|group| !group.is_empty() && !group.eq_ignore_ascii_case("none"))
            .map(str::to_owned);
        let action = if group.is_some() {
            RoutineAction::Grouped
        } else {
            RoutineAction::Ungrouped
        };
        if label.group == group {
            return Ok(Self::label_edit(
                RoutineAction::Unchanged,
                args.dry_run,
                None,
                label,
            ));
        }
        let operation_id = args
            .write
            .operation_id("label-group", &(&label.id, label.revision, &group))?;
        if args.dry_run {
            return Ok(Self::label_edit(
                action,
                true,
                Some(operation_id),
                WorkLabel { group, ..label },
            ));
        }
        self.applies("grouping a label")?;
        let saved = routines::label_group_as(self.app, &operation_id, &label.id, group.as_deref())
            .map_err(|message| self.write_refusal(message, &operation_id, "The label"))?;
        Ok(Self::label_edit(action, false, Some(operation_id), saved))
    }

    pub(super) fn label_retire(&self, args: &LabelRetireArgs) -> Result<WorkAnswer, ExternalError> {
        let label = self.label_named(&args.label)?;
        let retire = !args.restore;
        let action = if retire {
            RoutineAction::Retired
        } else {
            RoutineAction::Restored
        };
        if label.retired_at_ms.is_some() == retire {
            return Ok(Self::label_edit(
                RoutineAction::Unchanged,
                args.dry_run,
                None,
                label,
            ));
        }
        let operation_id = args
            .write
            .operation_id("label-retire", &(&label.id, label.revision, retire))?;
        if args.dry_run {
            let retired_at_ms = retire.then(now_ms);
            return Ok(Self::label_edit(
                action,
                true,
                Some(operation_id),
                WorkLabel {
                    retired_at_ms,
                    ..label
                },
            ));
        }
        self.applies(if retire {
            "retiring a label"
        } else {
            "restoring a label"
        })?;
        let saved = routines::label_retire_as(self.app, &operation_id, &label.id, retire)
            .map_err(|message| self.write_refusal(message, &operation_id, "The label"))?;
        Ok(Self::label_edit(action, false, Some(operation_id), saved))
    }

    pub(super) fn label_scope(&self, args: &LabelScopeArgs) -> Result<WorkAnswer, ExternalError> {
        let label = self.label_named(&args.label)?;
        let scope = LabelScope::from(args.scope);
        if label.scope == scope {
            return Ok(Self::label_edit(
                RoutineAction::Unchanged,
                args.dry_run,
                None,
                label,
            ));
        }
        let operation_id = args
            .write
            .operation_id("label-scope", &(&label.id, label.revision, scope))?;
        if args.dry_run {
            let rehearsed =
                self.rehearse(|conn| routines::scope_label(conn, &label.id, scope, now_ms()))?;
            return Ok(Self::label_edit(
                RoutineAction::Scoped,
                true,
                Some(operation_id),
                rehearsed,
            ));
        }
        self.applies("changing what a label goes on")?;
        let saved = routines::label_scope_as(self.app, &operation_id, &label.id, scope)
            .map_err(|message| self.write_refusal(message, &operation_id, "The label"))?;
        Ok(Self::label_edit(
            RoutineAction::Scoped,
            false,
            Some(operation_id),
            saved,
        ))
    }
}

fn action_text(action: RoutineAction) -> &'static str {
    match action {
        RoutineAction::Saved => "Saved",
        RoutineAction::Deleted => "Deleted",
        RoutineAction::Unchanged => "Unchanged",
        RoutineAction::Set => "Set",
        RoutineAction::Cleared => "Cleared",
        RoutineAction::Grouped => "Grouped",
        RoutineAction::Ungrouped => "Ungrouped",
        RoutineAction::Retired => "Retired",
        RoutineAction::Restored => "Restored",
        RoutineAction::Scoped => "Scoped",
    }
}

fn template_text(text: &mut String, template: &ExternalWorkTemplate) {
    let mut parts = vec![template.title.clone()];
    if template.priority > 0 {
        parts.push(Priority::of(template.priority).text().to_owned());
    }
    if template.owner != "nobody" {
        parts.push(template.owner.clone());
    }
    if let Some(project) = &template.project {
        parts.push(project.clone());
    }
    if !template.labels.is_empty() {
        parts.push(template.labels.join(", "));
    }
    if let Some(days) = template.due_in_days {
        parts.push(match days {
            0 => "due the day it is made".to_owned(),
            1 => "due in 1 day".to_owned(),
            days => format!("due in {days} days"),
        });
    }
    if let Some(repeat) = &template.repeat {
        parts.push(repeat.clone());
    }
    let _ = writeln!(text, "{}  {}", template.name, parts.join(" · "));
    for subtask in &template.subtasks {
        let _ = writeln!(text, "  - {subtask}");
    }
}

fn reminder_text(text: &mut String, reminder: &ExternalWorkReminder) {
    let state = match (reminder.notified_at_ms, reminder.due) {
        (Some(_), _) => "  (shown)",
        (None, true) => "  (due)",
        (None, false) => "",
    };
    let _ = writeln!(
        text,
        "{}  {}  {}{state}",
        reminder.task.handle, reminder.remind_at, reminder.task.title
    );
}

fn subject_reminder_text(text: &mut String, reminder: &ExternalSubjectReminder) {
    let state = match (reminder.notified_at_ms, reminder.due) {
        (Some(_), _) => "  (posted)",
        (None, true) => "  (due)",
        (None, false) => "",
    };
    let _ = writeln!(
        text,
        "{} {}  {}{state}",
        reminder.subject.kind.as_str(),
        reminder.remind_at,
        reminder.subject.name
    );
}

pub(super) fn label_text(label: &ExternalWorkLabel) -> String {
    let mut line = format!("{}  (hue {})", label.name, label.hue);
    if let Some(group) = &label.group {
        let _ = write!(line, "  group {group}");
    }
    if label.retired_at_ms.is_some() {
        line.push_str("  (retired)");
    }
    match label.scope {
        LabelScope::All => {}
        LabelScope::Tasks => line.push_str("  tasks only"),
        LabelScope::Projects => line.push_str("  projects only"),
    }
    line
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::Templates(list) => {
            if list.templates.is_empty() {
                text.push_str("No templates yet. Make one with: sona work template-save NAME\n");
            }
            for template in &list.templates {
                template_text(text, template);
            }
        }
        WorkAnswer::Template(change) => {
            let dry = if change.dry_run { " (dry run)" } else { "" };
            let _ = writeln!(text, "{}{dry}", action_text(change.action));
            template_text(text, &change.template);
        }
        WorkAnswer::Reminders(list) => {
            if list.reminders.is_empty() && list.subjects.is_empty() {
                text.push_str(if list.due_only {
                    "No reminders due.\n"
                } else {
                    "No reminders.\n"
                });
            }
            for reminder in &list.reminders {
                reminder_text(text, reminder);
            }
            for reminder in &list.subjects {
                subject_reminder_text(text, reminder);
            }
        }
        WorkAnswer::SubjectReminder(change) => {
            let dry = if change.dry_run { " (dry run)" } else { "" };
            match &change.reminder {
                Some(reminder) => {
                    let _ = write!(text, "{}{dry}: ", action_text(change.action));
                    subject_reminder_text(text, reminder);
                }
                None => {
                    let _ = writeln!(
                        text,
                        "{}{dry}: {} {} has no reminder",
                        action_text(change.action),
                        change.subject.kind.as_str(),
                        change.subject.name
                    );
                }
            }
        }
        WorkAnswer::Reminder(change) => {
            let dry = if change.dry_run { " (dry run)" } else { "" };
            match &change.reminder {
                Some(reminder) => {
                    let _ = write!(text, "{}{dry}: ", action_text(change.action));
                    reminder_text(text, reminder);
                }
                None => {
                    let _ = writeln!(
                        text,
                        "{}{dry}: {} has no reminder",
                        action_text(change.action),
                        change.task.handle
                    );
                }
            }
        }
        WorkAnswer::LabelEdit(change) => {
            let dry = if change.dry_run { " (dry run)" } else { "" };
            let _ = writeln!(
                text,
                "{}{dry}: {}",
                action_text(change.action),
                label_text(&change.label)
            );
        }
        _ => {}
    }
}
