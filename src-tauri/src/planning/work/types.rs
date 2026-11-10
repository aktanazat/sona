//! The tracker's wire shapes: what the Tasks and Triage screens read and send.

use super::super::types::{sql_enum, EvidenceLink, ProjectStatus, TaskStatus};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use serde::{Deserialize, Deserializer, Serialize};
use specta::Type;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    Triage,
    Backlog,
    NotStarted,
    InProgress,
    Done,
    Canceled,
    Duplicate,
}

sql_enum!(WorkStatus {
    Triage => "triage",
    Backlog => "backlog",
    NotStarted => "not_started",
    InProgress => "in_progress",
    Done => "done",
    Canceled => "canceled",
    Duplicate => "duplicate",
});

impl WorkStatus {
    /// Accepted work nobody has finished: what "My work" and the counts show.
    pub(crate) const fn is_open(self) -> bool {
        matches!(self, Self::Backlog | Self::NotStarted | Self::InProgress)
    }

    /// Nothing is left to do, one way or another.
    pub(crate) const fn is_closed(self) -> bool {
        matches!(self, Self::Done | Self::Canceled | Self::Duplicate)
    }

    /// The status the project screens, which know four, show.
    pub(crate) const fn legacy(self) -> TaskStatus {
        match self {
            Self::Triage | Self::Backlog | Self::NotStarted => TaskStatus::NotStarted,
            Self::InProgress => TaskStatus::InProgress,
            Self::Done => TaskStatus::Done,
            Self::Canceled | Self::Duplicate => TaskStatus::Canceled,
        }
    }
}

impl From<TaskStatus> for WorkStatus {
    fn from(status: TaskStatus) -> Self {
        match status {
            TaskStatus::NotStarted => Self::NotStarted,
            TaskStatus::InProgress => Self::InProgress,
            TaskStatus::Done => Self::Done,
            TaskStatus::Canceled => Self::Canceled,
        }
    }
}

/// Who the work waits on, read off the owner: never stored.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkDirection {
    Mine,
    WaitingOn,
    Unattributed,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkOwner {
    #[serde(rename = "self")]
    Myself,
    #[serde(rename_all = "camelCase")]
    Person {
        person_id: String,
    },
    Nobody,
}

impl WorkOwner {
    pub(crate) fn of(owner_is_self: bool, owner_person_id: Option<&str>) -> Self {
        match (owner_is_self, owner_person_id) {
            (true, _) => Self::Myself,
            (false, Some(person_id)) => Self::Person {
                person_id: person_id.to_owned(),
            },
            (false, None) => Self::Nobody,
        }
    }

    pub(crate) const fn direction(&self) -> WorkDirection {
        match self {
            Self::Myself => WorkDirection::Mine,
            Self::Person { .. } => WorkDirection::WaitingOn,
            Self::Nobody => WorkDirection::Unattributed,
        }
    }

    /// The form a history entry keeps: "self", "nobody", or the person's id.
    pub(crate) fn history_value(&self) -> String {
        match self {
            Self::Myself => "self".to_owned(),
            Self::Person { person_id } => person_id.clone(),
            Self::Nobody => "nobody".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkActor {
    Me,
    Sona,
    Agent,
}

sql_enum!(WorkActor {
    Me => "me",
    Sona => "sona",
    Agent => "agent",
});

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkActivityKind {
    Created,
    Status,
    Priority,
    Owner,
    Due,
    Project,
    Title,
    Details,
    Labels,
    Archived,
    Restored,
    Duplicate,
    Accepted,
    Declined,
    Snoozed,
    Relation,
    Kept,
    Recurrence,
    Cycle,
    Block,
    Parent,
    Comment,
}

sql_enum!(WorkActivityKind {
    Created => "created",
    Status => "status",
    Priority => "priority",
    Owner => "owner",
    Due => "due",
    Project => "project",
    Title => "title",
    Details => "details",
    Labels => "labels",
    Archived => "archived",
    Restored => "restored",
    Duplicate => "duplicate",
    Accepted => "accepted",
    Declined => "declined",
    Snoozed => "snoozed",
    Relation => "relation",
    Kept => "kept",
    Recurrence => "recurrence",
    Cycle => "cycle",
    Block => "block",
    Parent => "parent",
    Comment => "comment",
});

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkRelationKind {
    Blocks,
    Related,
}

sql_enum!(WorkRelationKind {
    Blocks => "blocks",
    Related => "related",
});

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkItem {
    pub id: String,
    pub number: i64,
    pub title: String,
    pub details: Option<String>,
    pub status: WorkStatus,
    pub priority: u8,
    pub project_id: Option<String>,
    pub milestone_id: Option<String>,
    pub owner_person_id: Option<String>,
    pub owner_is_self: bool,
    pub direction: WorkDirection,
    pub due_at_ms: Option<i64>,
    pub label_ids: Vec<String>,
    pub source_loop_id: Option<String>,
    pub duplicate_of_id: Option<String>,
    pub snoozed_until_ms: Option<i64>,
    pub archived_at_ms: Option<i64>,
    pub status_changed_at_ms: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub revision: i64,
    pub sort_order: f64,
    /// The number of the cycle the item is planned in.
    pub cycle: Option<i64>,
    /// The task this one is a sub-task of.
    pub parent_id: Option<String>,
}

impl WorkItem {
    pub(crate) fn owner(&self) -> WorkOwner {
        WorkOwner::of(self.owner_is_self, self.owner_person_id.as_deref())
    }
}

/// Where an item came from, for triage's splits.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkSource {
    Meeting,
    Dictation,
    Agent,
    Person,
}

/// An item with the names a list row shows beside it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemRow {
    #[serde(flatten)]
    pub item: WorkItem,
    pub project_title: Option<String>,
    pub owner_name: Option<String>,
    /// The agent working the item; none when the user is.
    pub doer: Option<WorkDoer>,
    pub source_meeting_id: Option<String>,
    pub source_meeting_title: Option<String>,
    pub source_quote: Option<String>,
    pub source_speaker: Option<String>,
    /// Offset into the source meeting where the commitment was said.
    pub source_at_ms: Option<i64>,
    pub blocked_by_open_count: u32,
    /// The meetings that raised the item or a duplicate of it, counted once
    /// each.
    pub raised_in: u32,
    pub source: WorkSource,
    /// When to ask the person who owes this item about it; none for work
    /// nobody else owes.
    pub follow_up_at_ms: Option<i64>,
    /// Mine, open, due within two days, nothing booked for it, and no free
    /// hour left on its due day.
    pub at_risk: bool,
    /// The calendar time booked to do it.
    pub time_block: Option<WorkTimeBlock>,
    /// How it comes back and how often its instances were done in time.
    pub repeat: Option<WorkRepeat>,
    /// Its sub-tasks done out of all of them, canceled and duplicate ones
    /// left out; none when it has none.
    pub sub_tasks: Option<WorkProgress>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkProgress {
    pub done: u32,
    pub total: u32,
}

/// Another task as a detail page names it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemRef {
    pub id: String,
    pub number: i64,
    pub title: String,
    pub status: WorkStatus,
}

/// Who wrote a comment: the user, Sona, or an agent by the name it gave.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkCommentAuthor {
    pub kind: WorkActor,
    pub name: Option<String>,
}

/// One comment. A thread's first comment carries its replies, oldest
/// first, and whether the thread is resolved; a deleted comment keeps its
/// place only while replies hang under it, with no body.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkComment {
    pub id: String,
    pub item_id: String,
    /// The thread's first comment, for a reply.
    pub parent_id: Option<String>,
    pub author: WorkCommentAuthor,
    /// Markdown; none once deleted.
    pub body: Option<String>,
    pub created_at_ms: i64,
    pub edited_at_ms: Option<i64>,
    pub resolved_at_ms: Option<i64>,
    pub deleted: bool,
    pub replies: Vec<WorkComment>,
}

/// A page of threads, oldest first; `next_cursor` reads the next page.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkCommentPage {
    pub threads: Vec<WorkComment>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkTimeBlock {
    pub starts_at_ms: i64,
    pub ends_at_ms: i64,
}

/// A repeating task's rule as said, and of the instances whose due day has
/// come, `kept` were done by the end of it out of `of`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkRepeat {
    pub every: String,
    pub kept: u32,
    pub of: u32,
}

/// The agent working an item, by the name it goes by, since it took it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkDoer {
    pub agent: String,
    pub since_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkProposalField {
    Title,
    Details,
    Status,
    Priority,
    Owner,
    Due,
    Project,
    Milestone,
    Labels,
    Snoozed,
    Recurrence,
    Cycle,
    Parent,
}

sql_enum!(WorkProposalField {
    Title => "title",
    Details => "details",
    Status => "status",
    Priority => "priority",
    Owner => "owner",
    Due => "due",
    Project => "project",
    Milestone => "milestone",
    Labels => "labels",
    Snoozed => "snoozed",
    Recurrence => "recurrence",
    Cycle => "cycle",
    Parent => "parent",
});

/// One field an agent would change, waiting for the user. `from` is the
/// value when it was proposed and `to` the value proposed, both in the form
/// a history entry keeps; details carry their whole text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkProposal {
    pub id: String,
    pub item_id: String,
    pub agent: String,
    pub field: WorkProposalField,
    pub from: Option<String>,
    pub to: Option<String>,
    pub created_at_ms: i64,
}

/// What handing an item to an agent left: its doer, and the prompt to paste
/// into that agent, absent when the user took the item back.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkDelegation {
    pub doer: Option<WorkDoer>,
    pub prompt: Option<String>,
}

/// A new item: left-out fields start in not started, at no priority, with
/// nobody owning it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemInput {
    pub title: String,
    pub details: Option<String>,
    pub status: Option<WorkStatus>,
    pub priority: Option<u8>,
    pub project_id: Option<String>,
    pub milestone_id: Option<String>,
    pub owner: Option<WorkOwner>,
    pub due_at_ms: Option<i64>,
    pub label_ids: Option<Vec<String>>,
    /// How often it comes back, as said: "every monday". Kept as words;
    /// nothing makes the next one.
    pub recurrence: Option<String>,
}

/// Reads a present `null` as `Some(None)`, so a patch can tell "clear this"
/// from a key it left out.
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub details: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<WorkStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub project_id: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub milestone_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<WorkOwner>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub due_at_ms: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_ids: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub snoozed_until_ms: Option<Option<i64>>,
    /// How often it comes back, in the words the parser reads as one
    /// repeat, such as "every monday"; null stops it repeating.
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub recurrence: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub cycle: Option<Option<i64>>,
    /// The task to sit under; null makes it a task of its own.
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_id: Option<Option<String>>,
}

/// A cycle a query or a change names: the one under way, the one after it,
/// none, or one by number.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkCycleRef {
    Current,
    Next,
    None,
    Number { number: i64 },
}

/// A due window that moves with the calendar, on this Mac's clock. Weeks
/// start on Monday.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkDue {
    /// Accepted, unfinished work due before today.
    Overdue,
    Today,
    /// Due from today through Sunday.
    ThisWeek,
    /// Due from today through the `days`th day after it.
    NextDays {
        days: u16,
    },
    NoDate,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct WorkQuery {
    pub statuses: Option<Vec<WorkStatus>>,
    pub direction: Option<WorkDirection>,
    /// People, each matched with the ids merged into them or that they were
    /// merged into.
    pub owner_person_ids: Option<Vec<String>>,
    /// Project ids, where `"none"` stands for work in no project.
    pub project_ids: Option<Vec<String>>,
    pub label_ids: Option<Vec<String>>,
    pub priorities: Option<Vec<u8>>,
    pub due_before_ms: Option<i64>,
    /// Any of these windows; with `due_before_ms`, both must hold.
    pub due: Option<Vec<WorkDue>>,
    pub source_meeting_id: Option<String>,
    pub search: Option<String>,
    pub include_archived: bool,
    pub include_snoozed: bool,
    pub cycle: Option<WorkCycleRef>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkSuggestionField {
    Owner,
    Due,
    Project,
    Duplicate,
}

/// A suggestion's value: an owner, a due time in epoch ms, or a project or
/// item id, as `field` says.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(untagged)]
pub enum WorkSuggestionValue {
    Owner(WorkOwner),
    Due(i64),
    Id(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkFieldSuggestion {
    pub field: WorkSuggestionField,
    pub value: WorkSuggestionValue,
    pub reason: String,
    pub quote: Option<String>,
    /// Offset into the meeting the suggestion was read from.
    pub at_ms: Option<i64>,
    pub meeting_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkActivity {
    pub id: String,
    pub item_id: String,
    pub at_ms: i64,
    pub actor: WorkActor,
    pub kind: WorkActivityKind,
    pub from: Option<String>,
    pub to: Option<String>,
    /// The agent behind an `agent` entry, when it gave its name.
    pub agent: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkRelation {
    pub id: String,
    pub from_id: String,
    pub to_id: String,
    pub kind: WorkRelationKind,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkLabel {
    pub id: String,
    pub name: String,
    /// Degrees on the color wheel, 0 to 360.
    pub hue: f64,
    pub revision: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkLayout {
    List,
    Board,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkGroupBy {
    Status,
    Owner,
    Project,
    Meeting,
    Due,
    Priority,
    None,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkOrderBy {
    Priority,
    Due,
    Updated,
    Created,
    Manual,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkDisplay {
    pub layout: WorkLayout,
    pub group_by: WorkGroupBy,
    pub order_by: WorkOrderBy,
    pub show_empty_groups: bool,
    pub properties: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkView {
    pub id: String,
    pub name: String,
    pub query: WorkQuery,
    pub display: WorkDisplay,
    pub revision: i64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkCounts {
    pub triage: u32,
    pub mine_open: u32,
    pub mine_overdue: u32,
    pub mine_due_today: u32,
    pub waiting_on: u32,
    /// Waiting-on work whose follow-up moment has come.
    pub waiting_on_stale: u32,
}

/// One local day's plan: the tasks picked to focus on, in the order they
/// were picked, and when; a day never planned has no `planned_at_ms`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkDay {
    pub day: String,
    pub focus_ids: Vec<String>,
    pub planned_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkProjectChoice {
    pub id: String,
    pub title: String,
    pub status: ProjectStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkPersonChoice {
    pub id: String,
    pub name: String,
    /// Other names the person goes by, such as a spelling the transcript
    /// keeps getting wrong.
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkList {
    pub items: Vec<WorkItemRow>,
    pub labels: Vec<WorkLabel>,
    pub projects: Vec<WorkProjectChoice>,
    pub people: Vec<WorkPersonChoice>,
    /// What Sona suggests for each listed item still in triage, by item id.
    pub suggestions: BTreeMap<String, Vec<WorkFieldSuggestion>>,
    /// Changes agents proposed for listed items, by item id, oldest first.
    pub proposals: BTreeMap<String, Vec<WorkProposal>>,
    /// False when the conversations could not be opened: every item is
    /// listed, but nothing read from its meeting, and no people.
    pub conversations_available: bool,
    /// Every cycle, oldest first, so a row's number reads as a cycle.
    pub cycles: Vec<WorkCycle>,
}

/// How long a cycle runs and the weekday each starts on, 1 for Monday
/// through 7 for Sunday.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkCycleSettings {
    pub weeks: u8,
    pub start_weekday: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkCycleState {
    Past,
    Current,
    Upcoming,
}

/// A cycle from the local midnight `start_day` begins to the one `end_day`
/// begins; its scope is every task planned in it that was not dropped, and
/// `done` the ones finished.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkCycle {
    pub number: i64,
    pub start_day: String,
    pub end_day: String,
    pub starts_at_ms: i64,
    pub ends_at_ms: i64,
    pub state: WorkCycleState,
    pub scope: u32,
    pub done: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkCycles {
    pub settings: WorkCycleSettings,
    pub cycles: Vec<WorkCycle>,
}

/// Scope and done as each day of a cycle ended, through today.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkCycleDay {
    pub day: String,
    pub scope: u32,
    pub done: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkCycleProgress {
    pub cycle: WorkCycle,
    pub days: Vec<WorkCycleDay>,
    /// Tasks moved in unfinished from earlier cycles.
    pub carried_over: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemDetail {
    pub item: WorkItemRow,
    pub recurrence: Option<String>,
    pub evidence: Vec<EvidenceLink>,
    pub relations: Vec<WorkRelation>,
    pub activity: Vec<WorkActivity>,
    pub suggestions: Vec<WorkFieldSuggestion>,
    /// False when the conversations could not be opened: the item's
    /// meeting, its meeting evidence and its people are not read.
    pub conversations_available: bool,
    pub proposals: Vec<WorkProposal>,
    pub parent: Option<WorkItemRef>,
    /// Its sub-tasks, archived ones left out, in number order.
    pub sub_tasks: Vec<WorkItem>,
    /// Every thread, oldest first.
    pub comments: Vec<WorkComment>,
}

/// Sent after every committed change to tracker items, with the items it
/// touched; an empty list means labels or views changed.
#[derive(Clone, Debug, Deserialize, Serialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
pub struct WorkChanged {
    pub ids: Vec<String>,
}
