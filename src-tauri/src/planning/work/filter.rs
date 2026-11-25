//! Conditions a list or a saved view filters tasks by: a field, an
//! operator, and the values it compares against, combined in groups where
//! all or any of them must hold.

use super::agents;
use super::cycles;
use super::items::{local_midnight_ms, local_today, MAX_ESTIMATE};
use super::store::Refusal;
use super::types::{WorkCycle, WorkCycleState, WorkItem};
use anyhow::Result;
use chrono::{Datelike, Days, Months, NaiveDate};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkFilterField {
    Status,
    Priority,
    Owner,
    Delegate,
    Project,
    Milestone,
    Label,
    ProjectLabel,
    Parent,
    Cycle,
    CycleScope,
    Due,
    Created,
    Updated,
    Estimate,
    For,
    Completed,
    Repeat,
}

impl WorkFilterField {
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Priority => "priority",
            Self::Owner => "owner",
            Self::Delegate => "delegate",
            Self::Project => "project",
            Self::Milestone => "milestone",
            Self::Label => "label",
            Self::ProjectLabel => "project label",
            Self::Parent => "parent",
            Self::Cycle => "cycle",
            Self::CycleScope => "cycle scope",
            Self::Due => "due",
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Estimate => "estimate",
            Self::For => "for",
            Self::Completed => "completed",
            Self::Repeat => "repeat",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkFilterOp {
    Is,
    IsNot,
    AnyOf,
    NoneOf,
    IncludesAll,
    IncludesAny,
    Before,
    After,
}

impl WorkFilterOp {
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Is => "is",
            Self::IsNot => "is not",
            Self::AnyOf => "any of",
            Self::NoneOf => "none of",
            Self::IncludesAll => "includes all of",
            Self::IncludesAny => "includes any of",
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

/// One condition. Values are ids, status names, priorities (0 to 4 or
/// their names), cycle numbers or `current`/`next`, days as YYYY-MM-DD,
/// `today`, `tomorrow`, `yesterday`, weeks (Monday to Sunday) as
/// `this_week`, `next_week` or `last_week`, months as `this_month`,
/// `next_month` or `last_month`, or epoch ms, and agents by name. A parent
/// is a task's id or number (`S-12`). Cycle scope is `planned` for work in
/// its cycle since the start, rollovers included, or `added` for work that
/// joined after the start. An estimate is its points, 0 to 64; `for` is
/// the person the work is owed to; `completed` is the day it was marked
/// done. `none` stands for an empty field and `me` for the user as owner.
/// `project_label` matches work whose project holds the label, and `none`
/// work whose project holds none.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkCondition {
    pub field: WorkFilterField,
    pub op: WorkFilterOp,
    #[serde(default)]
    pub values: Vec<String>,
}

impl WorkCondition {
    fn invalid_repeat(&self) -> bool {
        self.field == WorkFilterField::Repeat
            && (self.values.is_empty()
                || self.values.iter().any(|value| value.trim().is_empty())
                || matches!(self.op, WorkFilterOp::Before | WorkFilterOp::After))
    }
}

/// A condition, or a group of filters of which any or all must hold. A
/// query's own list of filters all must hold, so `{"any": [...]}` in it
/// reads "this or that".
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(untagged)]
pub enum WorkFilter {
    Condition(WorkCondition),
    Any { any: Vec<WorkFilter> },
    All { all: Vec<WorkFilter> },
}

impl From<WorkCondition> for WorkFilter {
    fn from(condition: WorkCondition) -> Self {
        Self::Condition(condition)
    }
}

impl WorkFilter {
    /// A group holding nothing, at any depth: it neither holds nor fails,
    /// so a list refuses it.
    pub(crate) fn has_empty_group(&self) -> bool {
        match self {
            Self::Condition(_) => false,
            Self::Any { any: filters } | Self::All { all: filters } => {
                filters.is_empty() || filters.iter().any(Self::has_empty_group)
            }
        }
    }

    pub(crate) fn has_invalid_repeat(&self) -> bool {
        match self {
            Self::Condition(condition) => condition.invalid_repeat(),
            Self::Any { any: filters } | Self::All { all: filters } => {
                filters.iter().any(Self::has_invalid_repeat)
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Value {
    None,
    Me,
    Repeating,
    Text(String),
    /// Local days, from the first one's midnight to the midnight after the
    /// last.
    Day(i64, i64),
    At(i64),
    /// Work planned in its cycle from the start; holds the ids added after.
    Planned(HashSet<String>),
    /// Work that joined its cycle after the start, by id.
    Added(HashSet<String>),
    /// Projects holding a label, by id; a task matches on its project.
    Projects(HashSet<String>),
}

/// A condition with its values read: people with the ids merged into
/// them, cycles by number, days as spans.
#[derive(Clone, Debug)]
pub(crate) struct Ready {
    field: WorkFilterField,
    op: WorkFilterOp,
    values: Vec<Value>,
}

/// A filter with its conditions read.
#[derive(Clone, Debug)]
pub(crate) enum ReadyFilter {
    One(Ready),
    Any(Vec<ReadyFilter>),
    All(Vec<ReadyFilter>),
}

const fn dated(field: WorkFilterField) -> bool {
    matches!(
        field,
        WorkFilterField::Due
            | WorkFilterField::Created
            | WorkFilterField::Updated
            | WorkFilterField::Completed
    )
}

fn priority(value: &str) -> Option<&'static str> {
    Some(match value {
        "0" | "none" | "no_priority" => "0",
        "1" | "urgent" => "1",
        "2" | "high" => "2",
        "3" | "medium" => "3",
        "4" | "low" => "4",
        _ => return None,
    })
}

/// The local days `value` covers: its first day and the day after its last.
pub(crate) fn span(value: &str, today: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    let week = |weeks: i64| {
        let monday =
            today.checked_sub_days(Days::new(u64::from(today.weekday().num_days_from_monday())))?;
        let start = monday.checked_add_signed(chrono::TimeDelta::weeks(weeks))?;
        Some((start, start.checked_add_days(Days::new(7))?))
    };
    let month = |months: i32| {
        let first = today.with_day(1)?;
        let start = if months < 0 {
            first.checked_sub_months(Months::new(months.unsigned_abs()))?
        } else {
            first.checked_add_months(Months::new(months.unsigned_abs()))?
        };
        Some((start, start.checked_add_months(Months::new(1))?))
    };
    let day = match value {
        "this_week" => return week(0),
        "next_week" => return week(1),
        "last_week" => return week(-1),
        "this_month" => return month(0),
        "next_month" => return month(1),
        "last_month" => return month(-1),
        "today" => today,
        "tomorrow" => today.succ_opt()?,
        "yesterday" => today.pred_opt()?,
        _ => NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?,
    };
    Some((day, day.succ_opt()?))
}

fn day(value: &str, now_ms: i64) -> Result<Value> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(Value::At(value.parse()?));
    }
    let (first, end) = span(value, local_today(now_ms)?).ok_or(Refusal::Invalid)?;
    Ok(Value::Day(
        local_midnight_ms(first)?,
        local_midnight_ms(end)?,
    ))
}

/// The projects holding `label`, or the projects holding no label when it is `None`.
fn projects_labelled(conn: &Connection, label: Option<&str>) -> Result<HashSet<String>> {
    let ids = match label {
        Some(label) => conn
            .prepare("SELECT project_id FROM planning_project_labels WHERE label_id = ?1")?
            .query_map([label], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()?,
        None => conn
            .prepare("SELECT id FROM planning_projects WHERE id NOT IN (SELECT project_id FROM planning_project_labels)")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()?,
    };
    Ok(ids)
}

/// Reads every filter's values; `merged` gives a person with the ids
/// merged into them. A value its field cannot take is refused, and so is a
/// group with nothing in it.
pub(crate) fn ready(
    conn: &Connection,
    filters: &[WorkFilter],
    merged: impl Fn(&str) -> Vec<String>,
    now_ms: i64,
) -> Result<Vec<ReadyFilter>> {
    let mut reader = Reader {
        conn,
        merged: &merged,
        now_ms,
        cycles: None,
    };
    filters.iter().map(|filter| reader.filter(filter)).collect()
}

/// Reads conditions, every cycle read at most once for all of them.
struct Reader<'a> {
    conn: &'a Connection,
    merged: &'a dyn Fn(&str) -> Vec<String>,
    now_ms: i64,
    cycles: Option<Vec<WorkCycle>>,
}

impl Reader<'_> {
    fn filter(&mut self, filter: &WorkFilter) -> Result<ReadyFilter> {
        match filter {
            WorkFilter::Condition(condition) => self.condition(condition).map(ReadyFilter::One),
            WorkFilter::Any { any: filters } | WorkFilter::All { all: filters }
                if filters.is_empty() =>
            {
                Err(Refusal::Invalid.into())
            }
            WorkFilter::Any { any } => any
                .iter()
                .map(|filter| self.filter(filter))
                .collect::<Result<_>>()
                .map(ReadyFilter::Any),
            WorkFilter::All { all } => all
                .iter()
                .map(|filter| self.filter(filter))
                .collect::<Result<_>>()
                .map(ReadyFilter::All),
        }
    }

    fn condition(&mut self, condition: &WorkCondition) -> Result<Ready> {
        let (conn, now_ms) = (self.conn, self.now_ms);
        let field = condition.field;
        let ranged = matches!(condition.op, WorkFilterOp::Before | WorkFilterOp::After);
        if condition.values.is_empty() || (ranged && !dated(field)) || condition.invalid_repeat() {
            return Err(Refusal::Invalid.into());
        }
        let mut values = Vec::new();
        for raw in &condition.values {
            let text = raw.trim().to_lowercase();
            if text == "none"
                && !matches!(
                    field,
                    WorkFilterField::Priority | WorkFilterField::ProjectLabel
                )
            {
                values.push(Value::None);
                continue;
            }
            match field {
                WorkFilterField::Status => {
                    cycles::status(&text).ok_or(Refusal::Invalid)?;
                    values.push(Value::Text(text));
                }
                WorkFilterField::Priority => {
                    values.push(Value::Text(
                        priority(&text).ok_or(Refusal::Invalid)?.to_owned(),
                    ));
                }
                WorkFilterField::Owner if text == "me" => values.push(Value::Me),
                WorkFilterField::Owner | WorkFilterField::For => {
                    values.extend((self.merged)(raw.trim()).into_iter().map(Value::Text));
                }
                WorkFilterField::Estimate => {
                    let points = text
                        .parse::<u8>()
                        .ok()
                        .filter(|points| *points <= MAX_ESTIMATE)
                        .ok_or(Refusal::Invalid)?;
                    values.push(Value::Text(points.to_string()));
                }
                WorkFilterField::Delegate => {
                    values.push(Value::Text(agents::agent_name(Some(raw)).to_lowercase()));
                }
                WorkFilterField::Repeat => values.push(match text.as_str() {
                    "set" => Value::Repeating,
                    _ => Value::Text(text),
                }),
                WorkFilterField::Parent => {
                    let number = text
                        .strip_prefix("s-")
                        .or_else(|| text.strip_prefix('#'))
                        .unwrap_or(&text);
                    let id = match number.parse::<i64>() {
                        Ok(number) => conn
                            .query_row(
                                "SELECT id FROM planning_tasks WHERE number = ?1",
                                [number],
                                |row| row.get(0),
                            )
                            .optional()?
                            .ok_or(Refusal::NotFound)?,
                        Err(_) => raw.trim().to_owned(),
                    };
                    values.push(Value::Text(id));
                }
                WorkFilterField::Cycle | WorkFilterField::CycleScope => {
                    if self.cycles.is_none() {
                        self.cycles = Some(cycles::all(conn, now_ms)?);
                    }
                    let all = self.cycles.as_deref().unwrap_or_default();
                    if field == WorkFilterField::CycleScope {
                        let added = all
                            .iter()
                            .flat_map(|cycle| cycle.added_after_start.iter().cloned())
                            .collect();
                        values.push(match text.as_str() {
                            "planned" => Value::Planned(added),
                            "added" => Value::Added(added),
                            _ => return Err(Refusal::Invalid.into()),
                        });
                        continue;
                    }
                    let number = match text.as_str() {
                        "current" | "next" => {
                            let state = if text == "current" {
                                WorkCycleState::Current
                            } else {
                                WorkCycleState::Upcoming
                            };
                            all.iter()
                                .find(|cycle| cycle.state == state)
                                .map(|cycle| cycle.number)
                        }
                        _ => Some(text.parse::<i64>().map_err(|_| Refusal::Invalid)?),
                    };
                    values
                        .push(number.map_or(Value::None, |number| Value::Text(number.to_string())));
                }
                WorkFilterField::Due
                | WorkFilterField::Created
                | WorkFilterField::Updated
                | WorkFilterField::Completed => {
                    values.push(day(&text, now_ms)?);
                }
                WorkFilterField::ProjectLabel => {
                    values.push(Value::Projects(projects_labelled(
                        conn,
                        (text != "none").then_some(raw.trim()),
                    )?));
                }
                WorkFilterField::Project | WorkFilterField::Milestone | WorkFilterField::Label => {
                    values.push(Value::Text(raw.trim().to_owned()));
                }
            }
        }
        Ok(Ready {
            field,
            op: condition.op,
            values,
        })
    }
}

fn text_of(field: WorkFilterField, item: &WorkItem, doer: Option<&str>) -> Option<String> {
    match field {
        WorkFilterField::Status => Some(item.status.as_str().to_owned()),
        WorkFilterField::Priority => Some(item.priority.to_string()),
        WorkFilterField::Owner => item.owner_person_id.clone(),
        WorkFilterField::Delegate => doer.map(str::to_lowercase),
        WorkFilterField::Project => item.project_id.clone(),
        WorkFilterField::Milestone => item.milestone_id.clone(),
        WorkFilterField::Parent => item.parent_id.clone(),
        WorkFilterField::Estimate => item.estimate.map(|points| points.to_string()),
        WorkFilterField::For => item.for_person_id.clone(),
        WorkFilterField::Cycle | WorkFilterField::CycleScope => {
            item.cycle.map(|number| number.to_string())
        }
        WorkFilterField::Label
        | WorkFilterField::Due
        | WorkFilterField::Created
        | WorkFilterField::Updated
        | WorkFilterField::Completed
        | WorkFilterField::Repeat
        | WorkFilterField::ProjectLabel => None,
    }
}

const fn at_of(field: WorkFilterField, item: &WorkItem) -> Option<i64> {
    match field {
        WorkFilterField::Due => item.due_at_ms,
        WorkFilterField::Created => Some(item.created_at_ms),
        WorkFilterField::Updated => Some(item.updated_at_ms),
        WorkFilterField::Completed => item.completed_at_ms,
        _ => None,
    }
}

fn hits(
    field: WorkFilterField,
    item: &WorkItem,
    doer: Option<&str>,
    repeat: Option<&str>,
    value: &Value,
) -> bool {
    match (field, value) {
        (WorkFilterField::CycleScope, Value::Added(ids)) => {
            item.cycle.is_some() && ids.contains(&item.id)
        }
        (WorkFilterField::CycleScope, Value::Planned(ids)) => {
            item.cycle.is_some() && !ids.contains(&item.id)
        }
        (_, Value::Planned(_) | Value::Added(_)) => false,
        (WorkFilterField::Repeat, Value::Repeating) => repeat.is_some(),
        (WorkFilterField::Repeat, Value::None) => repeat.is_none(),
        (WorkFilterField::Repeat, Value::Text(rule)) => repeat == Some(rule.as_str()),
        (_, Value::Repeating) => false,
        (WorkFilterField::Label, Value::None) => item.label_ids.is_empty(),
        (WorkFilterField::Label, Value::Text(id)) => item.label_ids.contains(id),
        (WorkFilterField::ProjectLabel, Value::Projects(ids)) => {
            item.project_id.as_ref().is_some_and(|id| ids.contains(id))
        }
        (WorkFilterField::Owner, Value::Me) => item.owner_is_self,
        (field, Value::None) if dated(field) => at_of(field, item).is_none(),
        (field, Value::None) => text_of(field, item, doer).is_none(),
        (field, Value::Day(start, end)) => {
            at_of(field, item).is_some_and(|at| *start <= at && at < *end)
        }
        (field, Value::At(ms)) => at_of(field, item) == Some(*ms),
        (field, Value::Text(text)) => text_of(field, item, doer).as_deref() == Some(text.as_str()),
        (_, Value::Projects(_)) => false,
        (_, Value::Me) => false,
    }
}

fn before(field: WorkFilterField, item: &WorkItem, value: &Value) -> bool {
    let Some(at) = at_of(field, item) else {
        return false;
    };
    match value {
        Value::Day(start, _) | Value::At(start) => at < *start,
        _ => false,
    }
}

fn after(field: WorkFilterField, item: &WorkItem, value: &Value) -> bool {
    let Some(at) = at_of(field, item) else {
        return false;
    };
    match value {
        Value::Day(_, end) => at >= *end,
        Value::At(ms) => at > *ms,
        _ => false,
    }
}

pub(crate) fn holds(
    filters: &[ReadyFilter],
    item: &WorkItem,
    doer: Option<&str>,
    repeat: Option<&str>,
) -> bool {
    filters
        .iter()
        .all(|filter| meets(filter, item, doer, repeat))
}

fn meets(filter: &ReadyFilter, item: &WorkItem, doer: Option<&str>, repeat: Option<&str>) -> bool {
    let condition = match filter {
        ReadyFilter::One(condition) => condition,
        ReadyFilter::Any(filters) => {
            return filters
                .iter()
                .any(|filter| meets(filter, item, doer, repeat));
        }
        ReadyFilter::All(filters) => return holds(filters, item, doer, repeat),
    };
    let field = condition.field;
    let mut values = condition.values.iter();
    let set = matches!(
        field,
        WorkFilterField::Label | WorkFilterField::ProjectLabel
    );
    match condition.op {
        WorkFilterOp::Is if set => values.all(|value| hits(field, item, doer, repeat, value)),
        WorkFilterOp::IncludesAll => values.all(|value| hits(field, item, doer, repeat, value)),
        WorkFilterOp::Is | WorkFilterOp::AnyOf | WorkFilterOp::IncludesAny => {
            values.any(|value| hits(field, item, doer, repeat, value))
        }
        WorkFilterOp::IsNot | WorkFilterOp::NoneOf => {
            !values.any(|value| hits(field, item, doer, repeat, value))
        }
        WorkFilterOp::Before => values.any(|value| before(field, item, value)),
        WorkFilterOp::After => values.any(|value| after(field, item, value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::work::types::{WorkDirection, WorkStatus};

    fn item(labels: &[&str], project: Option<&str>, due: Option<i64>) -> WorkItem {
        WorkItem {
            id: "a".to_owned(),
            number: 1,
            title: "Book the venue".to_owned(),
            details: None,
            status: WorkStatus::InProgress,
            priority: 2,
            project_id: project.map(str::to_owned),
            milestone_id: None,
            owner_person_id: None,
            owner_is_self: true,
            direction: WorkDirection::Mine,
            due_at_ms: due,
            label_ids: labels.iter().map(|label| (*label).to_owned()).collect(),
            source_loop_id: None,
            duplicate_of_id: None,
            snoozed_until_ms: None,
            archived_at_ms: None,
            status_changed_at_ms: 0,
            created_at_ms: 0,
            updated_at_ms: 0,
            revision: 1,
            sort_order: 0.0,
            cycle: None,
            parent_id: None,
            estimate: None,
            completed_at_ms: None,
            for_person_id: None,
        }
    }

    fn labels(op: WorkFilterOp, values: &[&str]) -> Vec<ReadyFilter> {
        vec![ReadyFilter::One(Ready {
            field: WorkFilterField::Label,
            op,
            values: values
                .iter()
                .map(|value| Value::Text((*value).to_owned()))
                .collect(),
        })]
    }

    #[test]
    fn label_operators_need_all_any_or_none_of_the_labels() {
        let task = item(&["bug"], None, None);
        assert!(!holds(
            &labels(WorkFilterOp::IncludesAll, &["bug", "ui"]),
            &task,
            None,
            None
        ));
        assert!(!holds(
            &labels(WorkFilterOp::Is, &["bug", "ui"]),
            &task,
            None,
            None
        ));
        assert!(holds(
            &labels(WorkFilterOp::IncludesAny, &["bug", "ui"]),
            &task,
            None,
            None
        ));
        assert!(!holds(
            &labels(WorkFilterOp::NoneOf, &["bug", "ui"]),
            &task,
            None,
            None
        ));
        assert!(holds(
            &labels(WorkFilterOp::NoneOf, &["ui"]),
            &task,
            None,
            None
        ));
    }

    #[test]
    fn none_matches_an_empty_field_and_days_bound_before_and_after() {
        let no_project = [ReadyFilter::One(Ready {
            field: WorkFilterField::Project,
            op: WorkFilterOp::Is,
            values: vec![Value::None],
        })];
        assert!(holds(&no_project, &item(&[], None, None), None, None));
        assert!(!holds(&no_project, &item(&[], Some("p"), None), None, None));
        let day = |op| {
            [ReadyFilter::One(Ready {
                field: WorkFilterField::Due,
                op,
                values: vec![Value::Day(100, 200)],
            })]
        };
        assert!(holds(
            &day(WorkFilterOp::Before),
            &item(&[], None, Some(99)),
            None,
            None
        ));
        assert!(!holds(
            &day(WorkFilterOp::Before),
            &item(&[], None, Some(100)),
            None,
            None
        ));
        assert!(holds(
            &day(WorkFilterOp::Is),
            &item(&[], None, Some(150)),
            None,
            None
        ));
        assert!(!holds(
            &day(WorkFilterOp::After),
            &item(&[], None, Some(199)),
            None,
            None
        ));
        assert!(holds(
            &day(WorkFilterOp::After),
            &item(&[], None, Some(200)),
            None,
            None
        ));
        assert!(!holds(
            &day(WorkFilterOp::After),
            &item(&[], None, None),
            None,
            None
        ));
    }

    #[test]
    fn delegate_matches_the_agent_by_any_spelling_and_none_when_no_agent_has_it() {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        let delegate = |op, value: &str| {
            ready(
                &conn,
                &[WorkCondition {
                    field: WorkFilterField::Delegate,
                    op,
                    values: vec![value.to_owned()],
                }
                .into()],
                |_| Vec::new(),
                0,
            )
            .expect("a delegate condition")
        };
        let task = item(&[], None, None);
        let codex = delegate(WorkFilterOp::Is, "codex");
        assert!(holds(&codex, &task, Some("Codex"), None));
        assert!(!holds(&codex, &task, Some("Claude Code"), None));
        assert!(!holds(&codex, &task, None, None));
        assert!(holds(
            &delegate(WorkFilterOp::Is, "claude"),
            &task,
            Some("Claude Code"),
            None
        ));
        assert!(holds(
            &delegate(WorkFilterOp::Is, "none"),
            &task,
            None,
            None
        ));
        assert!(!holds(
            &delegate(WorkFilterOp::Is, "none"),
            &task,
            Some("Codex"),
            None
        ));
        assert!(holds(
            &delegate(WorkFilterOp::IsNot, "none"),
            &task,
            Some("Codex"),
            None
        ));
    }

    #[test]
    fn an_any_group_keeps_work_that_meets_one_of_its_filters() {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        let read = |json: &str| {
            let filters: Vec<WorkFilter> = serde_json::from_str(json).expect("filters as JSON");
            ready(&conn, &filters, |_| Vec::new(), 0)
        };
        let either = read(
            r#"[{"field":"label","op":"includes_any","values":["bug"]},
                {"any":[{"field":"project","op":"is","values":["p"]},
                        {"all":[{"field":"project","op":"is","values":["none"]},
                                {"field":"label","op":"includes_any","values":["ui"]}]}]}]"#,
        )
        .expect("a group of conditions");
        assert!(holds(&either, &item(&["bug"], Some("p"), None), None, None));
        assert!(holds(
            &either,
            &item(&["bug", "ui"], None, None),
            None,
            None
        ));
        assert!(!holds(&either, &item(&["bug"], None, None), None, None));
        assert!(!holds(
            &either,
            &item(&["bug"], Some("q"), None),
            None,
            None
        ));
        assert!(!holds(&either, &item(&["ui"], Some("p"), None), None, None));
        assert!(read(r#"[{"any":[]}]"#).is_err());
    }
}
