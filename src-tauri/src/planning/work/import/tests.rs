//! Imports from other trackers, through the readers and the writer.

use super::batch::{Batch, ImportSource};
use super::sheets::{self, CsvMap};
use super::{
    boards, linear, linear_api, revert, write, ImportInclude, ImportOptions, ImportReport,
};
use crate::managers::history::MIGRATIONS;
use crate::planning::store::Refusal;
use crate::planning::work::items;
use crate::planning::work::types::{WorkActor, WorkOwner, WorkStatus};
use crate::planning::work::Change;
use rusqlite::Connection;
use rusqlite_migration::Migrations;

/// 2026-11-20 12:00 UTC.
const AT: i64 = 1_795_176_000_000;

const ISSUES: &str = r#""ID","Team","Title","Description","Status","Estimate","Priority","Project ID","Project","Creator","Assignee","Labels","Cycle Number","Cycle Name","Cycle Start","Cycle End","Created","Updated","Started","Triaged","Completed","Canceled","Archived","Due Date","Parent issue","Initiatives","Project Milestone ID","Project Milestone","SLA Status"
"AKT-1","Aktan","Plan the offsite","Venue and travel","In Progress","3","High","9b0c","Offsite","Aktan Azat","Aktan Azat","Planning, Travel","","","","","2026-11-01T10:00:00.000Z","2026-11-10T10:00:00.000Z","2026-11-02T10:00:00.000Z","","","","","2027-03-12","","Company events","m1","Venue booked",""
"AKT-2","Aktan","Book the flights","","Todo","","Medium","9b0c","Offsite","Aktan Azat","Jane Doe","Travel","","","","","2026-11-03T10:00:00.000Z","2026-11-03T10:00:00.000Z","","","","","","","AKT-1","","","",""
"AKT-3","Aktan","Ship the old landing page","","Done","1","Low","","","Aktan Azat","Aktan Azat","","","","","","2025-01-02T10:00:00.000Z","2025-01-10T10:00:00.000Z","","","2025-01-10T10:00:00.000Z","","","","","","","",""
"AKT-4","Aktan","Rewrite onboarding","","Backlog","","No priority","","","Aktan Azat","","","","","","","2024-01-02T09:00:00.000Z","2024-01-02T09:00:00.000Z","","","","","","","","","","",""
"#;

fn sona() -> Connection {
    let mut conn = Connection::open_in_memory().expect("an in-memory history database");
    Migrations::new(MIGRATIONS.to_vec())
        .to_latest(&mut conn)
        .expect("the history schema");
    conn
}

fn import(
    conn: &mut Connection,
    batch: &Batch,
    options: &ImportOptions,
) -> anyhow::Result<ImportReport> {
    let root = tempfile::tempdir().expect("a scratch folder");
    let tx = conn.transaction()?;
    let (report, _) = write(
        &tx,
        root.path(),
        ImportSource::Linear,
        "issues.csv",
        batch,
        options,
        Change {
            operation_id: "",
            actor: WorkActor::Me,
            at_ms: AT,
        },
    )?;
    tx.commit()?;
    Ok(report)
}

fn mine() -> ImportOptions {
    ImportOptions {
        me: vec!["Aktan Azat".to_owned()],
        ..ImportOptions::default()
    }
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).expect("a count")
}

fn task_titled(conn: &Connection, title: &str) -> crate::planning::work::types::WorkItem {
    let id: String = conn
        .query_row(
            "SELECT id FROM planning_tasks WHERE title = ?1",
            [title],
            |row| row.get(0),
        )
        .expect("the task");
    items::item(conn, &id).expect("the task's fields")
}

#[test]
fn a_linear_issues_export_lands_as_tasks_under_their_parent_project_and_milestone() {
    let mut conn = sona();
    let batch = linear::read_csv(ISSUES).expect("a Linear issues export");
    import(&mut conn, &batch, &mine()).expect("the import");

    let plan = task_titled(&conn, "Plan the offsite");
    let flights = task_titled(&conn, "Book the flights");
    assert_eq!(plan.status, WorkStatus::InProgress);
    assert_eq!(plan.priority, 2);
    assert_eq!(plan.estimate, Some(3));
    assert_eq!(plan.owner(), WorkOwner::Myself);
    assert_eq!(flights.owner(), WorkOwner::Nobody);
    assert_eq!(flights.parent_id.as_deref(), Some(plan.id.as_str()));
    assert_eq!(flights.project_id, plan.project_id);
    let project: String = conn
        .query_row(
            "SELECT title FROM planning_projects WHERE id = ?1",
            [&plan.project_id],
            |row| row.get(0),
        )
        .expect("the project");
    assert_eq!(project, "Offsite");
    let milestone: String = conn
        .query_row(
            "SELECT title FROM planning_milestones WHERE id = ?1",
            [&plan.milestone_id],
            |row| row.get(0),
        )
        .expect("the milestone");
    assert_eq!(milestone, "Venue booked");
    let labels: Vec<String> = {
        let mut statement = conn
            .prepare("SELECT l.name FROM planning_task_labels t JOIN planning_labels l ON l.id = t.label_id WHERE t.task_id = ?1 ORDER BY l.name")
            .expect("a label query");
        statement
            .query_map([&plan.id], |row| row.get(0))
            .expect("the labels")
            .collect::<rusqlite::Result<_>>()
            .expect("the labels")
    };
    assert_eq!(labels, ["Migrated", "Planning", "Travel"]);
    let initiative: String = conn
        .query_row(
            "SELECT i.name FROM planning_initiative_projects p JOIN planning_initiatives i ON i.id = p.initiative_id WHERE p.project_id = ?1",
            [&plan.project_id],
            |row| row.get(0),
        )
        .expect("the project's initiative");
    assert_eq!(initiative, "Company events");
}

#[test]
fn importing_the_same_export_again_skips_what_the_first_import_brought_in() {
    let mut conn = sona();
    let batch = linear::read_csv(ISSUES).expect("a Linear issues export");
    import(&mut conn, &batch, &mine()).expect("the first import");

    let again = import(&mut conn, &batch, &mine()).expect("the second import");

    assert_eq!(again.totals.tasks.skipped, 4);
    assert_eq!(again.totals.tasks.made + again.totals.tasks.archived, 0);
    assert_eq!(count(&conn, "SELECT count(*) FROM planning_tasks"), 4);
    assert_eq!(count(&conn, "SELECT count(*) FROM planning_projects"), 1);
}

#[test]
fn stale_and_finished_work_lands_archived_or_is_left_out_as_chosen() {
    let mut conn = sona();
    let batch = linear::read_csv(ISSUES).expect("a Linear issues export");
    let only_active = ImportOptions {
        include: ImportInclude {
            active: true,
            stale: false,
            completed: false,
        },
        ..mine()
    };
    let report = import(&mut conn, &batch, &only_active).expect("the import");
    assert_eq!(
        (
            report.totals.found.active,
            report.totals.found.stale,
            report.totals.found.completed
        ),
        (2, 1, 1)
    );
    assert_eq!(count(&conn, "SELECT count(*) FROM planning_tasks"), 2);

    import(&mut conn, &batch, &mine()).expect("the rest");
    assert!(task_titled(&conn, "Rewrite onboarding")
        .archived_at_ms
        .is_some());
    assert!(task_titled(&conn, "Ship the old landing page")
        .archived_at_ms
        .is_some());
    assert!(task_titled(&conn, "Plan the offsite")
        .archived_at_ms
        .is_none());
}

#[test]
fn undoing_an_import_bins_its_work_and_importing_again_brings_it_back() {
    let mut conn = sona();
    let batch = linear::read_csv(ISSUES).expect("a Linear issues export");
    let report = import(&mut conn, &batch, &mine()).expect("the import");
    let id = report.id.expect("the import's id");

    let (reverted, _) = revert(&conn, &id, WorkActor::Me, AT + 1).expect("the undo");

    assert_eq!((reverted.undone.tasks, reverted.undone.projects), (4, 1));
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM planning_tasks WHERE trashed_at_ms IS NULL"
        ),
        0
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM planning_projects WHERE trashed_at_ms IS NULL"
        ),
        0
    );
    assert_eq!(count(&conn, "SELECT count(*) FROM planning_labels"), 0);
    let again = import(&mut conn, &batch, &mine()).expect("importing again");
    assert_eq!(again.totals.tasks.made + again.totals.tasks.archived, 4);
}

#[test]
fn numbers_are_kept_only_while_sona_has_no_tasks() {
    let mut conn = sona();
    let batch = linear::read_csv(ISSUES).expect("a Linear issues export");
    let keep = ImportOptions {
        keep_numbers: true,
        ..mine()
    };
    import(&mut conn, &batch, &keep).expect("the import");
    assert_eq!(task_titled(&conn, "Rewrite onboarding").number, 4);
    assert_eq!(task_titled(&conn, "Book the flights").number, 2);

    let refused = import(&mut conn, &batch, &keep).expect_err("Sona has tasks now");
    assert_eq!(refused.downcast_ref::<Refusal>(), Some(&Refusal::HasTasks));
}

const API: &str = r##"{"data": {
  "viewer": {"name": "Aktan Azat"},
  "projects": {"nodes": [{"id": "p1", "name": "Offsite", "status": {"type": "started"}, "projectMilestones": {"nodes": []}}]},
  "issueLabels": {"nodes": [{"id": "l1", "name": "Travel", "color": "#4ea7fc"}]},
  "issues": {"nodes": [{
    "id": "i1", "identifier": "AKT-1", "number": 1, "title": "Plan the offsite", "priority": 2,
    "createdAt": "2026-11-01T10:00:00.000Z", "updatedAt": "2026-11-10T10:00:00.000Z",
    "state": {"name": "In Progress", "type": "started"},
    "assignee": {"name": "Aktan Azat"}, "project": {"id": "p1"},
    "labels": {"nodes": [{"id": "l1", "name": "Travel"}]},
    "comments": {"nodes": [
      {"id": "c1", "body": "Venue is booked", "createdAt": "2026-11-05T10:00:00.000Z", "user": {"name": "Jane Doe"}},
      {"id": "c2", "body": "Thanks", "createdAt": "2026-11-06T10:00:00.000Z", "user": {"name": "Aktan Azat"}, "parent": {"id": "c1"}}
    ]}
  }]},
  "documents": {"nodes": [{"id": "d1", "title": "Offsite brief", "content": "Three days in Lisbon", "project": {"id": "p1"}}]},
  "customViews": {"nodes": [
    {"id": "v1", "name": "Travel in progress", "modelName": "Issue", "filterData": {"and": [{"labels": {"name": {"in": ["Travel"]}}}, {"state": {"type": {"eq": "started"}}}]}},
    {"id": "v2", "name": "Mine or urgent", "modelName": "Issue", "filterData": {"or": [{"assignee": {"isMe": {"eq": true}}}, {"priority": {"eq": 1}}]}}
  ]}
}}"##;

#[test]
fn work_brought_in_posts_no_notices() {
    let mut conn = sona();
    let batch = linear::read_json(API).expect("a Linear API answer");
    import(&mut conn, &batch, &ImportOptions::default()).expect("the import");

    assert_eq!(
        count(&conn, "SELECT count(*) FROM planning_task_comments"),
        2
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM planning_notifications"),
        0
    );
}

#[test]
fn a_comment_reply_and_its_authors_come_over_with_the_issue() {
    let mut conn = sona();
    let batch = linear::read_json(API).expect("a Linear API answer");
    import(&mut conn, &batch, &ImportOptions::default()).expect("the import");

    let rows: Vec<(String, Option<String>, String, bool)> = {
        let mut statement = conn
            .prepare("SELECT author_kind, author_name, body, parent_comment_id IS NOT NULL FROM planning_task_comments ORDER BY created_at_ms")
            .expect("a comment query");
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("the comments")
            .collect::<rusqlite::Result<_>>()
            .expect("the comments")
    };
    assert_eq!(
        rows,
        [
            (
                "agent".to_owned(),
                Some("Jane Doe".to_owned()),
                "Venue is booked".to_owned(),
                false
            ),
            ("me".to_owned(), None, "Thanks".to_owned(), true),
        ]
    );
}

#[test]
fn a_view_sona_can_hold_becomes_a_saved_view_and_one_it_cannot_is_left_out() {
    let mut conn = sona();
    let batch = linear::read_json(API).expect("a Linear API answer");
    let report = import(&mut conn, &batch, &ImportOptions::default()).expect("the import");

    let query: String = conn
        .query_row(
            "SELECT query_json FROM planning_views WHERE name = 'Travel in progress'",
            [],
            |row| row.get(0),
        )
        .expect("the saved view");
    let query: serde_json::Value = serde_json::from_str(&query).expect("the view's query");
    assert_eq!(query["statuses"], serde_json::json!(["in_progress"]));
    assert_eq!(query["labelIds"].as_array().map(Vec::len), Some(1));
    let left_out = report
        .rows
        .iter()
        .find(|row| row.title == "Mine or urgent")
        .expect("the other view's row");
    assert_eq!(left_out.action, super::ImportAction::LeftOut);
    assert_eq!(count(&conn, "SELECT count(*) FROM planning_views"), 1);
}

#[test]
fn undoing_an_import_keeps_a_note_changed_since() {
    let mut conn = sona();
    let batch = linear::read_json(API).expect("a Linear API answer");
    let report = import(&mut conn, &batch, &ImportOptions::default()).expect("the import");
    let note: String = conn
        .query_row("SELECT id FROM scratch_notes", [], |row| row.get(0))
        .expect("the document as a note");
    crate::scratchpad::update_body_in(
        &conn,
        &note,
        "# Offsite brief\n\nFour days in Lisbon",
        crate::scratchpad::Author::Me,
        AT + 5,
    )
    .map_err(|_| "the edit")
    .expect("an edit after the import");

    let (reverted, _) =
        revert(&conn, &report.id.expect("an id"), WorkActor::Me, AT + 10).expect("the undo");

    assert_eq!((reverted.undone.notes, reverted.undone.notes_kept), (0, 1));
    assert_eq!(count(&conn, "SELECT count(*) FROM scratch_notes"), 1);
}

#[test]
fn the_import_template_reads_back_as_a_task() {
    let batch = sheets::read_csv(&sheets::template(), &CsvMap::new()).expect("the template");

    let task = &batch.tasks[0];
    assert_eq!(task.title, "Book the venue");
    assert_eq!(task.priority, 2);
    assert_eq!(task.estimate, Some(2));
    assert_eq!(task.labels, ["offsite", "venue"]);
    assert_eq!(task.project.as_deref(), Some("spring offsite"));
    assert_eq!(task.reference.as_deref(), Some("T-1"));
}

#[test]
fn a_sheet_reads_the_columns_its_map_names() {
    let sheet = "Task text,Remarks,When\nCall the caterer,Ask about vegan options,2027-03-01\n";
    let map: CsvMap =
        serde_json::from_str(r#"{"title":"Task text","details":"Remarks","due":"When"}"#)
            .expect("a map");

    let batch = sheets::read_csv(sheet, &map).expect("the sheet");

    assert_eq!(batch.tasks[0].title, "Call the caterer");
    assert_eq!(
        batch.tasks[0].details.as_deref(),
        Some("Ask about vegan options")
    );
    assert!(batch.tasks[0].due_at_ms.is_some());
}

#[test]
fn a_jira_export_takes_epics_as_projects_and_every_labels_column() {
    let export = "Summary,Issue key,Issue id,Issue Type,Status,Priority,Labels,Labels,Parent,Comment\n\
        Spring offsite,OPS-1,101,Epic,In Progress,Medium,,,,\n\
        Book the venue,OPS-2,102,Task,To Do,High,venue,offsite,101,05/Mar/24 2:22 PM;jane;Three quotes in\n";

    let batch = sheets::read_jira(export).expect("a Jira export");

    assert_eq!(batch.tasks.len(), 1);
    let task = &batch.tasks[0];
    assert_eq!(task.labels, ["venue", "offsite"]);
    assert_eq!(task.project.as_deref(), Some("spring offsite"));
    assert_eq!(task.comments[0].body, "Three quotes in");
    assert_eq!(task.comments[0].author.as_deref(), Some("jane"));
}

#[test]
fn a_trello_checklist_comes_in_as_sub_tasks_of_its_card() {
    let board = r#"{"id":"b1","name":"Offsite","lists":[{"id":"l1","name":"Doing"}],
        "labels":[],"actions":[],
        "cards":[{"id":"c1","name":"Plan the trip","idList":"l1","shortUrl":"https://trello.com/c/abc"}],
        "checklists":[{"id":"k1","idCard":"c1","checkItems":[{"id":"x1","name":"Book flights","state":"complete"}]}]}"#;

    let batch = boards::read_trello(board).expect("a Trello board");

    let item = batch
        .tasks
        .iter()
        .find(|task| task.title == "Book flights")
        .expect("the checklist item");
    assert_eq!(item.parent.as_deref(), Some("c1"));
    assert_eq!(item.status, WorkStatus::Done);
    let card = batch
        .tasks
        .iter()
        .find(|task| task.id == "c1")
        .expect("the card");
    assert_eq!(card.status, WorkStatus::InProgress);
}

#[test]
fn github_pull_requests_stay_out_of_an_issues_import() {
    let issues = r#"[
        {"number": 7, "title": "Crash on launch", "state": "OPEN", "url": "https://github.com/aktan/sona/issues/7", "labels": [{"name": "bug", "color": "d73a4a"}]},
        {"number": 8, "title": "Fix the crash", "state": "closed", "html_url": "https://github.com/aktan/sona/pull/8", "pull_request": {}}
    ]"#;

    let batch = boards::read_github(issues).expect("GitHub issues");

    assert_eq!(batch.tasks.len(), 1);
    assert_eq!(batch.tasks[0].reference.as_deref(), Some("aktan/sona#7"));
}

#[tokio::test]
async fn reading_linear_live_follows_every_page_of_issues() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback server");
    let address = listener.local_addr().expect("its address");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            let body = loop {
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => break None,
                    Ok(read) => request.extend_from_slice(&chunk[..read]),
                }
                let text = String::from_utf8_lossy(&request).into_owned();
                let Some((head, body)) = text.split_once("\r\n\r\n") else {
                    continue;
                };
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if body.len() >= length {
                    break Some(body.to_owned());
                }
            };
            let Some(body) = body else { continue };
            let asked: serde_json::Value = serde_json::from_str(&body).expect("a GraphQL request");
            let query = asked["query"].as_str().unwrap_or_default();
            let after = asked["variables"]["after"].as_str();
            let data = if query.contains("viewer") {
                serde_json::json!({"viewer": {"name": "Aktan Azat"}})
            } else if query.contains("issues(") {
                let (id, more, cursor) = match after {
                    None => ("i1", true, "page-2"),
                    Some(_) => ("i2", false, "end"),
                };
                serde_json::json!({"issues": {
                    "nodes": [{"id": id, "title": format!("Issue {id}"), "state": {"type": "unstarted"}}],
                    "pageInfo": {"hasNextPage": more, "endCursor": cursor}
                }})
            } else {
                let key = query
                    .split_once('{')
                    .and_then(|(_, rest)| rest.trim().split('(').next())
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                serde_json::json!({ key: {"nodes": [], "pageInfo": {"hasNextPage": false, "endCursor": null}} })
            };
            let reply = serde_json::json!({ "data": data }).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });

    let batch = linear_api::fetch(&format!("http://{address}/graphql"), "lin_api_test")
        .await
        .expect("the workspace");

    let titles: Vec<&str> = batch.tasks.iter().map(|task| task.title.as_str()).collect();
    assert_eq!(titles, ["Issue i1", "Issue i2"]);
    assert_eq!(batch.me, ["Aktan Azat"]);
}

#[test]
fn undoing_the_first_import_after_a_rerun_lets_the_next_import_bring_it_back() {
    let mut conn = sona();
    let batch = linear::read_csv(ISSUES).expect("a Linear issues export");
    let first = import(&mut conn, &batch, &mine()).expect("the first import");
    import(&mut conn, &batch, &mine()).expect("a rerun that skips everything");

    revert(&conn, &first.id.expect("the first import's id"), WorkActor::Me, AT + 1)
        .expect("the undo");
    let again = import(&mut conn, &batch, &mine()).expect("importing again");

    assert_eq!(again.totals.tasks.made + again.totals.tasks.archived, 4);
}

#[test]
fn refreshing_keeps_what_the_source_does_not_carry() {
    let mut conn = sona();
    let batch = linear::read_csv(ISSUES).expect("a Linear issues export");
    import(&mut conn, &batch, &mine()).expect("the import");
    let flights = task_titled(&conn, "Book the flights");
    let onboarding = task_titled(&conn, "Rewrite onboarding");
    items::apply(
        &conn,
        &onboarding.id,
        &crate::planning::work::types::WorkPatch {
            priority: Some(1),
            ..Default::default()
        },
        crate::planning::work::types::WorkActivityKind::Status,
        WorkActor::Me,
        AT + 1,
    )
    .expect("setting a priority in Sona");

    let mut refresh = ImportOptions {
        update_existing: true,
        ..mine()
    };
    refresh.copy.projects = false;
    refresh.copy.milestones = false;
    import(&mut conn, &batch, &refresh).expect("the refresh");

    assert_eq!(task_titled(&conn, "Book the flights").project_id, flights.project_id);
    assert!(flights.project_id.is_some());
    assert_eq!(task_titled(&conn, "Rewrite onboarding").priority, 1);
}

#[test]
fn a_number_too_large_to_keep_leaves_the_task_counter_working() {
    let mut conn = sona();
    let huge = ISSUES.replace("\"AKT-4\"", "\"AKT-9223372036854775807\"");
    let batch = linear::read_csv(&huge).expect("a Linear issues export");
    let keep = ImportOptions {
        keep_numbers: true,
        ..mine()
    };

    let report = import(&mut conn, &batch, &keep).expect("the import");

    assert!(report.notes.iter().any(|note| note.contains("numbers were not kept")));
    assert_eq!(
        count(
            &conn,
            "SELECT typeof(next_number) = 'integer' FROM planning_work_counter WHERE id = 1"
        ),
        1
    );
}
