//! `sona --plans`: the plans to meet heard in recorded meetings.

use super::external::ExternalError;
use super::{meeting_link, KeptLocal, QueryError, QUERY_SCHEMA_VERSION};
use crate::meeting::heard_plan_types::{HeardPlan, HeardPlanStatus};
use crate::meeting::store::MeetingStore;
use serde::Serialize;
use uuid::Uuid;

/// One plan to meet, kept whatever became of it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalPlanRow {
    pub id: Uuid,
    pub title: String,
    pub status: HeardPlanStatus,
    /// The time as it was said.
    pub when_text: String,
    /// The time those words name, read against the meeting's date. Null when
    /// they named none.
    pub start_utc_ms: Option<i64>,
    pub end_utc_ms: Option<i64>,
    /// The words named a day and no time.
    pub all_day: bool,
    pub people: Vec<String>,
    pub place: Option<String>,
    /// The words it was heard in. Null once the meeting's transcript is deleted.
    pub quote: Option<String>,
    pub meeting_id: Uuid,
    /// The later recording that matched it, when one did.
    pub happened_meeting_id: Option<Uuid>,
    pub heard_at_utc_ms: i64,
    pub link: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalPlansPage {
    pub schema_version: u32,
    pub entries: Vec<ExternalPlanRow>,
    pub has_more: bool,
}

/// Newest first. A meeting whose series is kept on this Mac is left out as
/// the place a plan was heard and as the place one happened.
pub(crate) fn plans_page(
    store: &MeetingStore,
    limit: usize,
) -> Result<ExternalPlansPage, ExternalError> {
    let kept_local = KeptLocal::read(store).map_err(QueryError::from)?;
    let mut plans = store
        .all_heard_plans_without(kept_local.sessions())
        .map_err(QueryError::from)?
        .into_iter();
    let entries = plans.by_ref().take(limit).map(row).collect();
    Ok(ExternalPlansPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries,
        has_more: plans.next().is_some(),
    })
}

fn row(plan: HeardPlan) -> ExternalPlanRow {
    ExternalPlanRow {
        id: plan.plan_id.uuid(),
        title: plan.title,
        status: plan.status,
        when_text: plan.when_text,
        start_utc_ms: plan.window.map(|window| window.start_utc_ms),
        end_utc_ms: plan.window.map(|window| window.end_utc_ms),
        all_day: plan.window.is_some_and(|window| window.all_day),
        people: plan.people,
        place: plan.place,
        quote: plan.quote,
        meeting_id: plan.session_id.uuid(),
        happened_meeting_id: plan.happened_session_id.map(|session_id| session_id.uuid()),
        heard_at_utc_ms: plan.created_at_utc_ms,
        link: meeting_link(plan.session_id),
    }
}
