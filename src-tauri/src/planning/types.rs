use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Active,
    Paused,
    Completed,
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

sql_enum!(ProjectStatus {
    Active => "active",
    Paused => "paused",
    Completed => "completed",
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Milestone {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub due_at_ms: Option<i64>,
    pub status: TaskStatus,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
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
    pub meeting_note_id: Option<String>,
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetail {
    pub project: Project,
    pub milestones: Vec<Milestone>,
    pub tasks: Vec<PlanningTask>,
    pub evidence: Vec<EvidenceLink>,
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
