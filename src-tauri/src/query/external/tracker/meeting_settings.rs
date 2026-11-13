//! The notes templates a person writes, and what each calendar series does
//! once a meeting's notes are ready, read and written through the functions
//! the meeting settings screen calls.

use super::library::{
    change, changed, file_made, library, made_before, told, Change, ExternalLibraryChange, Gate,
    MeetingRef,
};
use super::notes::request_json;
use super::{Tracker, WorkAnswer, READ_SCOPES, WRITE_SCOPES};
use crate::managers::history::HistoryManager;
use crate::meeting::automation_types::{
    MeetingAutomationKind, MeetingAutomationRunReceipt, MeetingAutomationSeries,
};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::template_types::{
    MeetingCustomTemplate, MeetingCustomTemplateDeleteRequest, MeetingCustomTemplateDraft,
    MeetingCustomTemplateSaveRequest, MeetingTemplateId, MeetingTemplateSection,
    MAX_CUSTOM_TEMPLATES,
};
use crate::meeting::types::MeetingCommandError;
use crate::query::external::{
    command_error, session_id, visible_meeting, ExternalError, ExternalErrorCode, ExternalScope,
};
use crate::query::{QueryError, QUERY_SCHEMA_VERSION};
use chrono::{Local, TimeZone};
use clap::{Args, Subcommand};
use serde::Serialize;
use std::fmt::Write as _;
use std::sync::Arc;
use uuid::Uuid;

const TEMPLATES: &str = "List them with: sona work meeting-templates";
const LIMITS: &str = "A template needs a one-line name of at most 60 characters, a purpose of at most 300, and one to eight sections. Each section has its own one-line title of at most 60 characters, and at most 240 characters on what goes under it.";
const TEMPLATE_SAVE: &str = "cli_meeting_template_save";
const TEMPLATE_DELETE: &str = "cli_meeting_template_delete";

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum MeetingSettingsVerb {
    /// Your own notes templates and their sections.
    MeetingTemplates,
    /// Write a notes template. The template with the same name, or the one
    /// --template names, is rewritten.
    MeetingTemplateSave(TemplateSaveArgs),
    /// Delete one of your notes templates. Where it was chosen, the built-in
    /// template chosen with it is used.
    MeetingTemplateDelete(TemplateDeleteArgs),
    /// The calendar series and what each one does once a meeting's notes
    /// are ready: reminders, a Shortcut, a webhook, a saved prompt.
    SeriesAutomations,
    /// What a meeting's series automations did once its notes were ready.
    SeriesAutomationRuns(MeetingRef),
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TemplateSaveArgs {
    /// The template's name.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// A heading the notes are arranged under, in order: "TITLE", or
    /// "TITLE: what goes under it". One to eight.
    #[arg(long = "section", value_name = "SECTION", required = true)]
    pub sections: Vec<String>,
    /// What the template is for. The notes never show it. Rewriting a
    /// template keeps its purpose when this is left out.
    #[arg(long, value_name = "TEXT")]
    pub purpose: Option<String>,
    /// The template to rewrite, by name or id, when NAME renames it.
    #[arg(long, value_name = "TEMPLATE")]
    pub template: Option<String>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TemplateDeleteArgs {
    /// The template, by name or id, from `meeting-templates`.
    #[arg(value_name = "TEMPLATE")]
    pub template: String,
    #[command(flatten)]
    pub change: Change,
}

impl MeetingSettingsVerb {
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self {
            Self::MeetingTemplates | Self::SeriesAutomations | Self::SeriesAutomationRuns(_) => {
                READ_SCOPES
            }
            Self::MeetingTemplateSave(TemplateSaveArgs { change, .. })
            | Self::MeetingTemplateDelete(TemplateDeleteArgs { change, .. }) => {
                if change.dry_run {
                    READ_SCOPES
                } else {
                    WRITE_SCOPES
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalMeetingTemplates {
    pub schema_version: u32,
    pub templates: Vec<MeetingCustomTemplate>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSeriesAutomations {
    pub schema_version: u32,
    /// A webhook's target is only its host: the rest of a webhook address
    /// often carries its key.
    pub series: Vec<MeetingAutomationSeries>,
    /// True when series kept on this Mac were left out.
    pub series_left_out: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalAutomationRuns {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    pub runs: Vec<MeetingAutomationRunReceipt>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ExternalMeetingSettingsAnswer {
    Templates(ExternalMeetingTemplates),
    Automations(ExternalSeriesAutomations),
    Runs(ExternalAutomationRuns),
}

fn failed(error: MeetingCommandError) -> ExternalError {
    QueryError::from(error).into()
}

fn template_error(error: MeetingCommandError) -> ExternalError {
    match error {
        MeetingCommandError::NotFound => {
            ExternalError::new(ExternalErrorCode::NotFound, "That notes template is gone.")
                .hint(TEMPLATES)
        }
        error => failed(error),
    }
}

/// A template by its id, or by its name ignoring case.
fn pick<'a>(
    templates: &'a [MeetingCustomTemplate],
    value: &str,
) -> Result<&'a MeetingCustomTemplate, ExternalError> {
    let value = value.trim();
    let by_id = Uuid::parse_str(value).ok();
    let found: Vec<_> = templates
        .iter()
        .filter(|template| match by_id {
            Some(id) => template.template_id.uuid() == id,
            None => template.name.trim().eq_ignore_ascii_case(value),
        })
        .collect();
    match found.as_slice() {
        [one] => Ok(one),
        [] => Err(ExternalError::new(
            ExternalErrorCode::NotFound,
            format!("No notes template {value:?}."),
        )
        .hint(TEMPLATES)),
        _ => Err(ExternalError::invalid(format!(
            "{} notes templates are called {value:?}. Name one by its id.",
            found.len()
        ))
        .hint(TEMPLATES)),
    }
}

fn section(value: &str) -> MeetingTemplateSection {
    let (title, instructions) = value.split_once(':').unwrap_or((value, ""));
    MeetingTemplateSection {
        title: title.to_owned(),
        instructions: instructions.to_owned(),
    }
}

async fn template_save(
    meetings: &Arc<MeetingSessionManager>,
    history: &HistoryManager,
    args: &TemplateSaveArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    library(meetings).await?;
    let list = meetings.custom_templates().await.map_err(failed)?;
    let dry_run = args.change.dry_run;
    let summary = told(
        dry_run,
        "Saved the notes template",
        "Would save the notes template",
    );
    let request = request_json(&(&args.name, &args.sections, &args.purpose, &args.template))?;
    if let Some(given) = args.change.write.operation_id {
        let given = given.to_string();
        if made_before(history, &given, TEMPLATE_SAVE, &request)? {
            let mut answer = change(summary, dry_run, given);
            answer.template_id = pick(&list.templates, &args.name)
                .ok()
                .map(|template| template.template_id.uuid());
            answer.replayed = true;
            return Ok(answer);
        }
    }
    let existing = match &args.template {
        Some(value) => Some(pick(&list.templates, value)?),
        None => {
            let name = args.name.trim();
            let mut named = list
                .templates
                .iter()
                .filter(|template| template.name.trim().eq_ignore_ascii_case(name));
            match (named.next(), named.next()) {
                (Some(_), Some(_)) => {
                    return Err(ExternalError::invalid(format!(
                        "Several notes templates are called {name:?}. Pass --template with the id of the one to rewrite."
                    ))
                    .hint(TEMPLATES))
                }
                (one, _) => one,
            }
        }
    };
    let draft = MeetingCustomTemplateDraft {
        name: args.name.clone(),
        purpose: args
            .purpose
            .clone()
            .or_else(|| existing.map(|template| template.purpose.clone()))
            .unwrap_or_default(),
        sections: args.sections.iter().map(|value| section(value)).collect(),
    }
    .normalized()
    .map_err(|()| ExternalError::invalid(LIMITS))?;
    let template_id = existing.map(|template| template.template_id);
    if template_id.is_none() && list.templates.len() >= MAX_CUSTOM_TEMPLATES {
        return Err(ExternalError::invalid(format!(
            "You have {MAX_CUSTOM_TEMPLATES} notes templates, the most Sona keeps. Delete one first."
        ))
        .hint(TEMPLATES));
    }
    let id = args.change.write.operation_id(
        "meeting-template-save",
        &(
            template_id.map(MeetingTemplateId::uuid),
            &draft,
            list.revision,
        ),
    )?;
    let mut answer = change(summary, dry_run, id);
    answer.template_id = template_id.map(MeetingTemplateId::uuid);
    answer.replayed = existing.is_some_and(|template| {
        template.name == draft.name
            && template.purpose == draft.purpose
            && template.sections == draft.sections
    });
    if dry_run || answer.replayed {
        return Ok(answer);
    }
    gate("saving a notes template")?;
    let saved = meetings
        .save_custom_template(MeetingCustomTemplateSaveRequest {
            template_id,
            draft,
            expected_revision: list.revision,
        })
        .await
        .map_err(template_error)?;
    file_made(history, &answer.operation_id, TEMPLATE_SAVE, &request)?;
    answer.template_id = Some(saved.template_id.uuid());
    Ok(answer)
}

async fn template_delete(
    meetings: &Arc<MeetingSessionManager>,
    history: &HistoryManager,
    args: &TemplateDeleteArgs,
    gate: Gate<'_>,
) -> Result<ExternalLibraryChange, ExternalError> {
    library(meetings).await?;
    let dry_run = args.change.dry_run;
    let summary = told(
        dry_run,
        "Deleted the notes template",
        "Would delete the notes template",
    );
    let request = args.template.trim();
    if let Some(given) = args.change.write.operation_id {
        let given = given.to_string();
        if made_before(history, &given, TEMPLATE_DELETE, request)? {
            let mut answer = change(summary, dry_run, given);
            answer.replayed = true;
            return Ok(answer);
        }
    }
    let list = meetings.custom_templates().await.map_err(failed)?;
    let template_id = pick(&list.templates, &args.template)?.template_id;
    let id = args.change.write.operation_id(
        "meeting-template-delete",
        &(template_id.uuid(), list.revision),
    )?;
    let mut answer = change(summary, dry_run, id);
    answer.template_id = Some(template_id.uuid());
    if dry_run {
        return Ok(answer);
    }
    gate("deleting a notes template")?;
    meetings
        .delete_custom_template(MeetingCustomTemplateDeleteRequest {
            template_id,
            expected_revision: list.revision,
        })
        .await
        .map_err(template_error)?;
    file_made(history, &answer.operation_id, TEMPLATE_DELETE, request)?;
    Ok(answer)
}

async fn series_automations(
    meetings: &Arc<MeetingSessionManager>,
) -> Result<ExternalSeriesAutomations, ExternalError> {
    let store = library(meetings).await?;
    let roster = meetings.automation_roster().await.map_err(failed)?;
    let keys: Vec<String> = roster
        .series
        .iter()
        .map(|series| series.series_key.clone())
        .collect();
    let preferences = store
        .series_preferences_many(&keys)
        .map_err(|error| ExternalError::from(QueryError::from(error)))?;
    let (kept, mut series): (Vec<_>, Vec<_>) = roster.series.into_iter().partition(|series| {
        preferences
            .get(series.series_key.trim())
            .is_some_and(|preferences| preferences.remote_intelligence_opt_out)
    });
    for automation in series
        .iter_mut()
        .flat_map(|series| series.automations.iter_mut())
    {
        if automation.kind == MeetingAutomationKind::Webhook {
            automation.target = automation
                .target
                .as_deref()
                .and_then(|target| url::Url::parse(target).ok())
                .and_then(|address| address.host_str().map(str::to_owned));
        }
    }
    Ok(ExternalSeriesAutomations {
        schema_version: QUERY_SCHEMA_VERSION,
        series,
        series_left_out: !kept.is_empty(),
    })
}

async fn automation_runs(
    meetings: &Arc<MeetingSessionManager>,
    args: &MeetingRef,
) -> Result<ExternalAutomationRuns, ExternalError> {
    let session = session_id(&args.meeting)?;
    let store = library(meetings).await?;
    visible_meeting(&store, session)?;
    let runs = meetings
        .automation_runs(session)
        .await
        .map_err(command_error(session.uuid()))?;
    Ok(ExternalAutomationRuns {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: session.uuid(),
        runs,
    })
}

impl Tracker<'_> {
    pub(super) async fn meeting_settings(
        &self,
        verb: &MeetingSettingsVerb,
    ) -> Result<WorkAnswer, ExternalError> {
        let meetings = self.meetings;
        let gate = |what: &str| self.applies(what);
        let answer = match verb {
            MeetingSettingsVerb::MeetingTemplates => {
                library(meetings).await?;
                ExternalMeetingSettingsAnswer::Templates(ExternalMeetingTemplates {
                    schema_version: QUERY_SCHEMA_VERSION,
                    templates: meetings.custom_templates().await.map_err(failed)?.templates,
                })
            }
            MeetingSettingsVerb::MeetingTemplateSave(args) => {
                return Ok(changed(
                    template_save(meetings, self.history, args, &gate).await?,
                ));
            }
            MeetingSettingsVerb::MeetingTemplateDelete(args) => {
                return Ok(changed(
                    template_delete(meetings, self.history, args, &gate).await?,
                ));
            }
            MeetingSettingsVerb::SeriesAutomations => {
                ExternalMeetingSettingsAnswer::Automations(series_automations(meetings).await?)
            }
            MeetingSettingsVerb::SeriesAutomationRuns(args) => {
                ExternalMeetingSettingsAnswer::Runs(automation_runs(meetings, args).await?)
            }
        };
        Ok(WorkAnswer::MeetingSettings(Box::new(answer)))
    }
}

pub(super) fn day(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|at| at.format("%b %-d, %Y").to_string())
        .unwrap_or_default()
}

pub(super) fn text(answer: &ExternalMeetingSettingsAnswer, text: &mut String) {
    match answer {
        ExternalMeetingSettingsAnswer::Templates(list) => {
            if list.templates.is_empty() {
                text.push_str("No notes templates of your own.\n");
            }
            for template in &list.templates {
                let _ = writeln!(text, "{}  {}", template.name, template.template_id.uuid());
                if !template.purpose.is_empty() {
                    let _ = writeln!(text, "  For: {}", template.purpose);
                }
                for section in &template.sections {
                    if section.instructions.is_empty() {
                        let _ = writeln!(text, "  - {}", section.title);
                    } else {
                        let _ = writeln!(text, "  - {}: {}", section.title, section.instructions);
                    }
                }
            }
        }
        ExternalMeetingSettingsAnswer::Automations(list) => {
            if list.series.is_empty() {
                text.push_str("No calendar series yet.\n");
            }
            for series in &list.series {
                let _ = writeln!(
                    text,
                    "{}  ({} meetings, last on {})",
                    series.title,
                    series.meeting_count,
                    day(series.last_met_at_utc_ms)
                );
                if series.automations.is_empty() {
                    text.push_str("  Nothing runs after its meetings.\n");
                }
                for automation in &series.automations {
                    let _ = writeln!(
                        text,
                        "  {} {}{}",
                        automation.kind.as_str(),
                        if automation.enabled { "on" } else { "off" },
                        automation
                            .target
                            .as_deref()
                            .map(|target| format!("  {target}"))
                            .unwrap_or_default()
                    );
                }
            }
            if list.series_left_out {
                text.push_str("Series kept on this Mac are left out.\n");
            }
        }
        ExternalMeetingSettingsAnswer::Runs(list) => {
            if list.runs.is_empty() {
                text.push_str("No automation ran for this meeting.\n");
            }
            for run in &list.runs {
                let _ = writeln!(
                    text,
                    "{}  {}{}  effects: {}  {}{}",
                    run.kind.as_str(),
                    run.state.as_str(),
                    run.failure
                        .map(|failure| format!(" ({})", failure.as_str()))
                        .unwrap_or_default(),
                    run.effects,
                    day(run.started_at_utc_ms),
                    run.detail
                        .as_deref()
                        .map(|detail| format!("  {detail}"))
                        .unwrap_or_default()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{series_automations, ExternalSeriesAutomations};
    use crate::meeting::automation_types::{
        MeetingAutomationKind, MeetingSeriesAutomationSetRequest,
    };
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::session::{MeetingSessionManager, NoCaptureSources};
    use crate::meeting::store::workflow_core_tests::reviewable_meeting;
    use crate::meeting::store::MeetingStore;
    use crate::meeting::types::MeetingOperationId;
    use crate::secrets::{MemorySecretBackend, SecretManager};
    use std::sync::Arc;
    use tempfile::TempDir;

    const NOW: i64 = 1_767_000_000_000;

    fn event(series_key: &str, title: &str, at: i64) -> CalendarEventSummary {
        CalendarEventSummary {
            event_key: format!("{series_key}#{at}"),
            series_key: series_key.to_owned(),
            title: title.to_owned(),
            attendee_count: 2,
            start_utc_ms: at,
            end_utc_ms: at + 30 * 60 * 1_000,
            attendees: Vec::new(),
            notes: None,
            calendar_name: None,
            location: None,
            url: None,
            place: None,
        }
    }

    fn webhook(store: &MeetingStore, series_key: &str) {
        store
            .set_series_automation(
                &MeetingSeriesAutomationSetRequest {
                    operation_id: MeetingOperationId::new(),
                    series_key: series_key.to_owned(),
                    kind: MeetingAutomationKind::Webhook,
                    enabled: true,
                    target: Some(format!(
                        "https://hooks.tail1234.ts.net/{series_key}?token=s3cret"
                    )),
                    expected_revision: store.series_automations(series_key).unwrap().revision,
                },
                NOW,
            )
            .unwrap();
    }

    /// Two series, each sending a webhook whose address carries a key; the
    /// board series is kept on this Mac.
    fn listed() -> ExternalSeriesAutomations {
        tauri::async_runtime::block_on(async {
            let directory = TempDir::new().unwrap();
            let secrets = Arc::new(SecretManager::with_backend(Arc::new(
                MemorySecretBackend::new(),
            )));
            let meetings = Arc::new(
                MeetingSessionManager::with_parts(
                    None,
                    Some(directory.path().join("meetings")),
                    secrets,
                    Arc::new(NoCaptureSources),
                )
                .reading_only(),
            );
            let store = meetings.store().await.unwrap();
            for (series_key, title, at) in [
                ("pricing", "Weekly pricing", NOW),
                ("board", "Board prep", NOW + 1),
            ] {
                let meeting = reviewable_meeting(&store, title, at);
                store
                    .remember_calendar_facts(meeting, &event(series_key, title, at))
                    .unwrap();
                webhook(&store, series_key);
            }
            store
                .set_series_remote_opt_out(
                    &MeetingSeriesRemoteOptOutSetRequest {
                        operation_id: MeetingOperationId::new(),
                        series_key: "board".to_owned(),
                        remote_intelligence_opt_out: true,
                        expected_revision: 0,
                    },
                    NOW,
                )
                .unwrap();
            series_automations(&meetings).await.unwrap()
        })
    }

    #[test]
    fn a_webhook_reads_as_its_host_alone() {
        let listed = listed();
        let pricing = listed
            .series
            .iter()
            .find(|series| series.series_key == "pricing")
            .unwrap();

        assert_eq!(pricing.automations[0].kind, MeetingAutomationKind::Webhook);
        assert_eq!(
            pricing.automations[0].target.as_deref(),
            Some("hooks.tail1234.ts.net")
        );
        assert!(!serde_json::to_string(&listed).unwrap().contains("s3cret"));
    }

    #[test]
    fn a_series_kept_on_this_mac_is_left_out_and_said_to_be() {
        let listed = listed();

        assert_eq!(
            listed
                .series
                .iter()
                .map(|series| series.series_key.as_str())
                .collect::<Vec<_>>(),
            ["pricing"]
        );
        assert!(listed.series_left_out);
    }
}
