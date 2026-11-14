use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use serde::{Deserialize, Serialize};
use specta::Type;

/// Where a project stands. `active` is Linear's "started"; the command line
/// and the bridge read either word.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Backlog,
    Planned,
    #[serde(alias = "started")]
    Active,
    Paused,
    Completed,
    Canceled,
}

/// What a label may go on: tasks and projects, or only one of them.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LabelScope {
    #[default]
    All,
    Tasks,
    Projects,
}

/// How finely a project's start or target date is meant: the day itself,
/// or the month, quarter, half-year or year it falls in.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum DateResolution {
    Day,
    Month,
    Quarter,
    HalfYear,
    Year,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ProjectHealth {
    OnTrack,
    AtRisk,
    OffTrack,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    NotStarted,
    InProgress,
    Done,
    Canceled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTargetKind {
    Project,
    Milestone,
    Task,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSourceKind {
    Meeting,
    Note,
    Loop,
    Dictation,
    Link,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PlanningCalendarKind {
    Task,
    Milestone,
    Meeting,
}

macro_rules! sql_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        impl $name {
            pub(crate) const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value,)+
                }
            }
        }

        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                match value.as_str()? {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(FromSqlError::InvalidType),
                }
            }
        }
    };
}
pub(super) use sql_enum;

sql_enum!(ProjectStatus {
    Backlog => "backlog",
    Planned => "planned",
    Active => "active",
    Paused => "paused",
    Completed => "completed",
    Canceled => "canceled",
});
sql_enum!(LabelScope {
    All => "all",
    Tasks => "tasks",
    Projects => "projects",
});
sql_enum!(DateResolution {
    Day => "day",
    Month => "month",
    Quarter => "quarter",
    HalfYear => "halfYear",
    Year => "year",
});
sql_enum!(ProjectHealth {
    OnTrack => "onTrack",
    AtRisk => "atRisk",
    OffTrack => "offTrack",
});
sql_enum!(TaskStatus {
    NotStarted => "not_started",
    InProgress => "in_progress",
    Done => "done",
    Canceled => "canceled",
});
sql_enum!(EvidenceTargetKind {
    Project => "project",
    Milestone => "milestone",
    Task => "task",
});
sql_enum!(EvidenceSourceKind {
    Meeting => "meeting",
    Note => "note",
    Loop => "loop",
    Dictation => "dictation",
    Link => "link",
});

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: ProjectStatus,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub lead_person_id: Option<String>,
    pub lead_is_self: bool,
    pub member_person_ids: Vec<String>,
    pub members_include_self: bool,
    pub target_at_ms: Option<i64>,
    pub hue: Option<u16>,
    pub archived_at_ms: Option<i64>,
    /// 0 none, 1 urgent, 2 high, 3 medium, 4 low, as tasks count it.
    pub priority: u8,
    pub start_at_ms: Option<i64>,
    pub start_resolution: Option<DateResolution>,
    pub target_resolution: Option<DateResolution>,
    pub icon: Option<String>,
    pub label_ids: Vec<String>,
    pub links: Vec<ProjectLink>,
    /// Set while the project sits in the bin; it is purged 30 days later.
    pub trashed_at_ms: Option<i64>,
    /// A line on what the project is, shown apart from the description.
    pub summary: Option<String>,
    /// The custom status name it sits under, inside its status's category.
    pub status_name_id: Option<String>,
}

/// A name of the owner's for a project status, inside one of the six
/// categories.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStatusName {
    pub id: String,
    pub category: ProjectStatus,
    pub name: String,
    pub position: i64,
}

/// One change to a project's properties, milestones, dependencies or
/// initiative, as its history lists it. Dates are epoch milliseconds and
/// people are ids or `me`, written as text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectActivity {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    /// The milestone a milestone change is about, or the other project a
    /// dependency change is about.
    pub subject: Option<String>,
    pub from_value: Option<String>,
    pub to_value: Option<String>,
    pub at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectLink {
    pub id: String,
    pub title: Option<String>,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectUpdateAuthor {
    pub kind: super::work::types::WorkActor,
    pub name: Option<String>,
}

/// A note on how a project is going, with its health and the project's
/// progress when it was written.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectUpdate {
    pub id: String,
    pub project_id: String,
    pub health: ProjectHealth,
    /// Markdown; none once deleted.
    pub body: Option<String>,
    pub progress: u8,
    pub author: ProjectUpdateAuthor,
    pub created_at_ms: i64,
    pub edited_at_ms: Option<i64>,
    pub deleted: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectUpdatePage {
    pub updates: Vec<ProjectUpdate>,
    pub next_cursor: Option<String>,
}

/// Tasks counted at the end of one week, Monday to Monday local time.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWeek {
    pub week_start_ms: i64,
    pub scope: u32,
    pub started: u32,
    pub done: u32,
}

/// When the project would finish at its recent pace, and 40% either way.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPrediction {
    pub expected_at_ms: i64,
    pub optimistic_at_ms: i64,
    pub pessimistic_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectProgress {
    pub scope: u32,
    pub started: u32,
    pub done: u32,
    /// Done out of the scope.
    pub percent: u8,
    pub weeks: Vec<ProjectWeek>,
    /// None until a week has passed with work done, or with nothing left.
    pub prediction: Option<ProjectPrediction>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MilestoneProgress {
    pub milestone_id: String,
    pub scope: u32,
    pub started: u32,
    pub done: u32,
    pub percent: u8,
    /// The first milestone, in order, not yet complete: the current focus.
    pub next_open: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Milestone {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub description: Option<String>,
    pub due_at_ms: Option<i64>,
    pub status: TaskStatus,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// Its place among the project's milestones; none sorts by due day after.
    pub position: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlanningTask {
    pub id: String,
    pub project_id: String,
    pub milestone_id: Option<String>,
    pub title: String,
    pub details: Option<String>,
    pub due_at_ms: Option<i64>,
    pub owner_person_id: Option<String>,
    pub owner_is_self: bool,
    pub owner_available: bool,
    pub status: TaskStatus,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceLink {
    pub id: String,
    pub target_kind: EvidenceTargetKind,
    pub target_id: String,
    pub source_kind: EvidenceSourceKind,
    pub source_id: String,
    pub at_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub meeting_note_id: Option<String>,
    pub caption: Option<String>,
    pub revision: i64,
    pub created_at_ms: i64,
    pub available: bool,
    pub title: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub project: Project,
    pub open_task_count: i64,
    pub completed_task_count: i64,
    pub next_due_at_ms: Option<i64>,
    /// When the newest update was posted; none without one.
    pub latest_update_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOwnerCount {
    pub person_id: String,
    pub open_tasks: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MilestonePulse {
    pub milestone_id: String,
    /// Distinct meetings that still exist among the milestone's meeting evidence.
    pub meetings: u32,
    /// When the latest of those meetings started.
    pub last_meeting_at_ms: i64,
}

/// A project at a glance as of the moment it was read. Canceled tasks count
/// nowhere, and work is overdue once its due day has ended.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPulse {
    pub done: u32,
    pub open: u32,
    pub overdue: u32,
    /// The earliest due date, today or later, among open tasks and milestones.
    pub next_due_at_ms: Option<i64>,
    /// Distinct meetings in the project's evidence held in the last seven local days.
    pub meetings_this_week: u32,
    /// People who own open tasks, most open tasks first.
    pub owners: Vec<ProjectOwnerCount>,
    /// Milestones with meeting evidence, in the same order as the detail's milestones.
    pub milestones: Vec<MilestonePulse>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetail {
    pub project: Project,
    pub milestones: Vec<Milestone>,
    pub tasks: Vec<PlanningTask>,
    pub evidence: Vec<EvidenceLink>,
    pub pulse: ProjectPulse,
    pub latest_update: Option<ProjectUpdate>,
    /// The project is active and its last update is older than one cycle
    /// and three days.
    pub update_missing: bool,
    pub progress: ProjectProgress,
    /// In the order of `milestones`.
    pub milestone_progress: Vec<MilestoneProgress>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlanningCalendarItem {
    pub id: String,
    pub kind: PlanningCalendarKind,
    pub title: String,
    pub project_id: Option<String>,
    pub due_at_ms: i64,
    pub meeting_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlanningCalendarPage {
    pub items: Vec<PlanningCalendarItem>,
    pub conversations_available: bool,
}
