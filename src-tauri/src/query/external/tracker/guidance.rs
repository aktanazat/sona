use super::{now_ms, refusal, Tracker, WorkAnswer, WriteArgs};
use crate::planning;
use crate::planning::work::agents::{self, WorkGuidance};
use crate::query::external::{local_time, ExternalError};
use crate::query::QUERY_SCHEMA_VERSION;
use clap::Args;
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct GuidanceSaveArgs {
    /// The standing guidance. Pass "" to clear it.
    #[arg(value_name = "TEXT")]
    pub text: String,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalGuidance {
    pub schema_version: u32,
    #[serde(flatten)]
    pub current: WorkGuidance,
    pub history: Vec<WorkGuidance>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalGuidanceChange {
    pub schema_version: u32,
    pub dry_run: bool,
    pub changed: bool,
    pub operation_id: String,
    #[serde(flatten)]
    pub guidance: WorkGuidance,
}

impl Tracker<'_> {
    fn current_guidance(&self) -> Result<WorkGuidance, ExternalError> {
        planning::history(self.history, |conn| agents::guidance(conn))
            .map_err(|message| refusal(self.history, message))
    }

    pub(super) fn guidance(&self) -> Result<WorkAnswer, ExternalError> {
        let (current, history) = planning::history(self.history, |conn| {
            Ok((agents::guidance(conn)?, agents::guidance_history(conn)?))
        })
        .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::Guidance(ExternalGuidance {
            schema_version: QUERY_SCHEMA_VERSION,
            current,
            history,
        }))
    }

    pub(super) fn guidance_save(
        &self,
        args: &GuidanceSaveArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let before = self.current_guidance()?;
        let operation_id = args
            .write
            .operation_id("guidance-save", &(before.version, &args.text))?;
        let by = args.write.agent();
        let saved = if args.dry_run {
            self.history
                .with_connection(|conn| {
                    let tx = conn.transaction()?;
                    agents::save_guidance(&tx, &args.text, &by, now_ms())
                })
                .map_err(|error| refusal(self.history, planning::failure(error)))?
        } else {
            self.applies("the standing guidance")?;
            agents::guidance_save_as(self.app, &operation_id, &args.text, &by)
                .map_err(|message| self.write_refusal(message, &operation_id, "The guidance"))?
        };
        Ok(WorkAnswer::GuidanceChange(Box::new(ExternalGuidanceChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            changed: saved.version != before.version,
            operation_id,
            guidance: saved,
        })))
    }
}

pub(super) fn text(answer: &WorkAnswer, text: &mut String) {
    match answer {
        WorkAnswer::Guidance(page) => {
            let current = &page.current;
            if current.version == 0 {
                text.push_str("No standing guidance.\n");
            } else {
                let _ = writeln!(text, "{}", current.text);
                let _ = writeln!(
                    text,
                    "Version {}, saved {} by {}.",
                    current.version,
                    local_time(current.saved_at_ms),
                    current.saved_by
                );
            }
            for version in &page.history {
                let _ = writeln!(
                    text,
                    "  v{} {}: {}",
                    version.version,
                    local_time(version.saved_at_ms),
                    version.text
                );
            }
        }
        WorkAnswer::GuidanceChange(change) => {
            let what = match (change.dry_run, change.changed) {
                (_, false) => "Guidance unchanged",
                (true, true) => "Would save guidance",
                (false, true) => "Saved guidance",
            };
            let _ = writeln!(text, "{what}, version {}.", change.guidance.version);
        }
        _ => {}
    }
}
