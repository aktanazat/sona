//! Tasks as a CSV file a spreadsheet opens: one row per task, the way the
//! app names each field.

use super::types::{WorkItemRow, WorkLabel, WorkStatus};
use chrono::{Local, TimeZone};
use std::collections::HashMap;
use std::fmt::Write as _;

const HEADER: [&str; 15] = [
    "ID",
    "Title",
    "Status",
    "Priority",
    "Owner",
    "Project",
    "Labels",
    "Due",
    "Cycle",
    "Estimate",
    "Parent",
    "Created",
    "Updated",
    "Completed",
    "Link",
];

/// The rows in the order given, under a header row; `labels` names the
/// labels the rows carry.
pub(crate) fn csv(rows: &[WorkItemRow], labels: &[WorkLabel]) -> String {
    let names: HashMap<&str, &str> = labels
        .iter()
        .map(|label| (label.id.as_str(), label.name.as_str()))
        .collect();
    let numbers: HashMap<&str, i64> = rows
        .iter()
        .map(|row| (row.item.id.as_str(), row.item.number))
        .collect();
    let mut out = String::new();
    line(&mut out, HEADER.iter().map(|title| (*title).to_owned()));
    for row in rows {
        let item = &row.item;
        let owner = if item.owner_is_self {
            "Me".to_owned()
        } else {
            row.owner_name.clone().unwrap_or_default()
        };
        let labels: Vec<&str> = item
            .label_ids
            .iter()
            .filter_map(|id| names.get(id.as_str()).copied())
            .collect();
        let parent = item.parent_id.as_deref().map_or_else(String::new, |id| {
            numbers
                .get(id)
                .map_or_else(|| id.to_owned(), |number| format!("S-{number}"))
        });
        line(
            &mut out,
            [
                format!("S-{}", item.number),
                item.title.clone(),
                status(item.status).to_owned(),
                priority(item.priority).to_owned(),
                owner,
                row.project_title.clone().unwrap_or_default(),
                labels.join(", "),
                day(item.due_at_ms),
                item.cycle
                    .map(|cycle| cycle.to_string())
                    .unwrap_or_default(),
                item.estimate
                    .map(|points| points.to_string())
                    .unwrap_or_default(),
                parent,
                day(Some(item.created_at_ms)),
                day(Some(item.updated_at_ms)),
                day(item.completed_at_ms),
                crate::query::task_link(&item.id),
            ]
            .into_iter(),
        );
    }
    out
}

/// One record: fields holding a comma, a quote or a line break are quoted,
/// their quotes doubled. Lines end in CRLF, as RFC 4180 has them. A field a
/// spreadsheet would run as a formula, such as a title heard in a meeting
/// that starts with `=`, starts with an apostrophe instead.
fn line(out: &mut String, fields: impl Iterator<Item = String>) {
    for (index, mut field) in fields.enumerate() {
        if index > 0 {
            out.push(',');
        }
        if field.starts_with(['=', '+', '-', '@', '\t', '\r']) {
            field.insert(0, '\'');
        }
        if field.contains([',', '"', '\n', '\r']) {
            let _ = write!(out, "\"{}\"", field.replace('"', "\"\""));
        } else {
            out.push_str(&field);
        }
    }
    out.push_str("\r\n");
}

const fn status(status: WorkStatus) -> &'static str {
    match status {
        WorkStatus::Triage => "Triage",
        WorkStatus::Backlog => "Backlog",
        WorkStatus::NotStarted => "Todo",
        WorkStatus::InProgress => "In progress",
        WorkStatus::Done => "Done",
        WorkStatus::Canceled => "Canceled",
        WorkStatus::Duplicate => "Duplicate",
    }
}

const fn priority(priority: u8) -> &'static str {
    match priority {
        1 => "Urgent",
        2 => "High",
        3 => "Medium",
        4 => "Low",
        _ => "No priority",
    }
}

/// The local day `at_ms` falls on, YYYY-MM-DD; empty for none.
fn day(at_ms: Option<i64>) -> String {
    at_ms
        .and_then(|at| Local.timestamp_millis_opt(at).single())
        .map(|at| at.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::line;

    #[test]
    fn a_field_is_quoted_when_it_must_be_and_never_runs_as_a_formula() {
        let mut out = String::new();
        line(
            &mut out,
            [
                "plain".to_owned(),
                "a, b".to_owned(),
                "say \"hi\"".to_owned(),
                "=HYPERLINK(\"x\")".to_owned(),
                "-1+2".to_owned(),
            ]
            .into_iter(),
        );
        assert_eq!(
            out,
            "plain,\"a, b\",\"say \"\"hi\"\"\",\"'=HYPERLINK(\"\"x\"\")\",'-1+2\r\n"
        );
    }
}
