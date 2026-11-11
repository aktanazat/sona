//! Conditions a list or a saved view filters tasks by, all of which must
//! hold: a field, an operator, and the values it compares against.

use super::agents;
use super::cycles;
use super::items::{local_midnight_ms, local_today};
use super::store::Refusal;
use super::types::{WorkCycleState, WorkItem};
use anyhow::Result;
use chrono::{Datelike, Days, Months, NaiveDate};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use specta::Type;

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
    Cycle,
    Due,
    Created,
    Updated,
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
            Self::Cycle => "cycle",
            Self::Due => "due",
            Self::Created => "created",
            Self::Updated => "updated",
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
/// `next_month` or `last_month`, or epoch ms, and agents by name. `none`
/// stands for an empty field and `me` for the user as owner.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkCondition {
    pub field: WorkFilterField,
    pub op: WorkFilterOp,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Value {
    None,
    Me,
    Text(String),
    /// Local days, from the first one's midnight to the midnight after the
    /// last.
    Day(i64, i64),
    At(i64),
}

/// A condition with its values read: people with the ids merged into
/// them, cycles by number, days as spans.
#[derive(Clone, Debug)]
pub(crate) struct Ready {
    field: WorkFilterField,
    op: WorkFilterOp,
    values: Vec<Value>,
}

const fn dated(field: WorkFilterField) -> bool {
    matches!(
        field,
        WorkFilterField::Due | WorkFilterField::Created | WorkFilterField::Updated
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
        let monday = today.checked_sub_days(Days::new(u64::from(
            today.weekday().num_days_from_monday(),
        )))?;
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
    Ok(Value::Day(local_midnight_ms(first)?, local_midnight_ms(end)?))
}

/// Reads every condition's values; `merged` gives a person with the ids
/// merged into them. A value its field cannot take is refused.
pub(crate) fn ready(
    conn: &Connection,
    conditions: &[WorkCondition],
    merged: impl Fn(&str) -> Vec<String>,
    now_ms: i64,
) -> Result<Vec<Ready>> {
    let mut cycle_numbers = None;
    let mut out = Vec::with_capacity(conditions.len());
    for condition in conditions {
        let field = condition.field;
        let ranged = matches!(condition.op, WorkFilterOp::Before | WorkFilterOp::After);
        if condition.values.is_empty() || (ranged && !dated(field)) {
            return Err(Refusal::Invalid.into());
        }
        let mut values = Vec::new();
        for raw in &condition.values {
            let text = raw.trim().to_lowercase();
            if text == "none" && field != WorkFilterField::Priority {
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
                WorkFilterField::Owner => {
                    values.extend(merged(raw.trim()).into_iter().map(Value::Text));
                }
                WorkFilterField::Delegate => {
                    values.push(Value::Text(
                        agents::agent_name(Some(raw)).to_lowercase(),
                    ));
                }
                WorkFilterField::Cycle => {
                    let number = match text.as_str() {
                        "current" | "next" => {
                            let all = match &cycle_numbers {
                                Some(all) => all,
                                None => cycle_numbers.insert(cycles::all(conn, now_ms)?),
                            };
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
                WorkFilterField::Due | WorkFilterField::Created | WorkFilterField::Updated => {
                    values.push(day(&text, now_ms)?);
                }
                WorkFilterField::Project | WorkFilterField::Milestone | WorkFilterField::Label => {
                    values.push(Value::Text(raw.trim().to_owned()));
                }
            }
        }
        out.push(Ready {
            field,
            op: condition.op,
            values,
        });
    }
    Ok(out)
}

fn text_of(field: WorkFilterField, item: &WorkItem, doer: Option<&str>) -> Option<String> {
    match field {
        WorkFilterField::Status => Some(item.status.as_str().to_owned()),
        WorkFilterField::Priority => Some(item.priority.to_string()),
        WorkFilterField::Owner => item.owner_person_id.clone(),
        WorkFilterField::Delegate => doer.map(str::to_lowercase),
        WorkFilterField::Project => item.project_id.clone(),
        WorkFilterField::Milestone => item.milestone_id.clone(),
        WorkFilterField::Cycle => item.cycle.map(|number| number.to_string()),
        WorkFilterField::Label
        | WorkFilterField::Due
        | WorkFilterField::Created
        | WorkFilterField::Updated => None,
    }
}

const fn at_of(field: WorkFilterField, item: &WorkItem) -> Option<i64> {
    match field {
        WorkFilterField::Due => item.due_at_ms,
        WorkFilterField::Created => Some(item.created_at_ms),
        WorkFilterField::Updated => Some(item.updated_at_ms),
        _ => None,
    }
}

fn hits(field: WorkFilterField, item: &WorkItem, doer: Option<&str>, value: &Value) -> bool {
    match (field, value) {
        (WorkFilterField::Label, Value::None) => item.label_ids.is_empty(),
        (WorkFilterField::Label, Value::Text(id)) => item.label_ids.contains(id),
        (WorkFilterField::Owner, Value::Me) => item.owner_is_self,
        (field, Value::None) if dated(field) => at_of(field, item).is_none(),
        (field, Value::None) => text_of(field, item, doer).is_none(),
        (field, Value::Day(start, end)) => {
            at_of(field, item).is_some_and(|at| *start <= at && at < *end)
        }
        (field, Value::At(ms)) => at_of(field, item) == Some(*ms),
        (field, Value::Text(text)) => text_of(field, item, doer).as_deref() == Some(text.as_str()),
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

/// Whether `item`, worked by the agent `doer` names, meets every condition.
pub(crate) fn holds(conditions: &[Ready], item: &WorkItem, doer: Option<&str>) -> bool {
    conditions.iter().all(|condition| {
        let field = condition.field;
        let mut values = condition.values.iter();
        let set = field == WorkFilterField::Label;
        match condition.op {
            WorkFilterOp::Is if set => values.all(|value| hits(field, item, doer, value)),
            WorkFilterOp::IncludesAll => values.all(|value| hits(field, item, doer, value)),
            WorkFilterOp::Is | WorkFilterOp::AnyOf | WorkFilterOp::IncludesAny => {
                values.any(|value| hits(field, item, doer, value))
            }
            WorkFilterOp::IsNot | WorkFilterOp::NoneOf => {
                !values.any(|value| hits(field, item, doer, value))
            }
            WorkFilterOp::Before => values.any(|value| before(field, item, value)),
            WorkFilterOp::After => values.any(|value| after(field, item, value)),
        }
    })
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
        }
    }

    fn labels(op: WorkFilterOp, values: &[&str]) -> Vec<Ready> {
        vec![Ready {
            field: WorkFilterField::Label,
            op,
            values: values
                .iter()
                .map(|value| Value::Text((*value).to_owned()))
                .collect(),
        }]
    }

    #[test]
    fn label_operators_need_all_any_or_none_of_the_labels() {
        let task = item(&["bug"], None, None);
        assert!(!holds(
            &labels(WorkFilterOp::IncludesAll, &["bug", "ui"]),
            &task,
            None
        ));
        assert!(!holds(
            &labels(WorkFilterOp::Is, &["bug", "ui"]),
            &task,
            None
        ));
        assert!(holds(
            &labels(WorkFilterOp::IncludesAny, &["bug", "ui"]),
            &task,
            None
        ));
        assert!(!holds(
            &labels(WorkFilterOp::NoneOf, &["bug", "ui"]),
            &task,
            None
        ));
        assert!(holds(&labels(WorkFilterOp::NoneOf, &["ui"]), &task, None));
    }

    #[test]
    fn none_matches_an_empty_field_and_days_bound_before_and_after() {
        let no_project = [Ready {
            field: WorkFilterField::Project,
            op: WorkFilterOp::Is,
            values: vec![Value::None],
        }];
        assert!(holds(&no_project, &item(&[], None, None), None));
        assert!(!holds(&no_project, &item(&[], Some("p"), None), None));
        let day = |op| {
            [Ready {
                field: WorkFilterField::Due,
                op,
                values: vec![Value::Day(100, 200)],
            }]
        };
        assert!(holds(
            &day(WorkFilterOp::Before),
            &item(&[], None, Some(99)),
            None
        ));
        assert!(!holds(
            &day(WorkFilterOp::Before),
            &item(&[], None, Some(100)),
            None
        ));
        assert!(holds(
            &day(WorkFilterOp::Is),
            &item(&[], None, Some(150)),
            None
        ));
        assert!(!holds(
            &day(WorkFilterOp::After),
            &item(&[], None, Some(199)),
            None
        ));
        assert!(holds(
            &day(WorkFilterOp::After),
            &item(&[], None, Some(200)),
            None
        ));
        assert!(!holds(
            &day(WorkFilterOp::After),
            &item(&[], None, None),
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
                }],
                |_| Vec::new(),
                0,
            )
            .expect("a delegate condition")
        };
        let task = item(&[], None, None);
        let codex = delegate(WorkFilterOp::Is, "codex");
        assert!(holds(&codex, &task, Some("Codex")));
        assert!(!holds(&codex, &task, Some("Claude Code")));
        assert!(!holds(&codex, &task, None));
        assert!(holds(
            &delegate(WorkFilterOp::Is, "claude"),
            &task,
            Some("Claude Code")
        ));
        assert!(holds(&delegate(WorkFilterOp::Is, "none"), &task, None));
        assert!(!holds(&delegate(WorkFilterOp::Is, "none"), &task, Some("Codex")));
        assert!(holds(&delegate(WorkFilterOp::IsNot, "none"), &task, Some("Codex")));
    }
}
