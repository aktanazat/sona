//! The inbox as the user reads it: one row for what an agent did to a task
//! in one sitting, the rows that need the user first.
//!
//! Notices stay one per event in `planning_notifications`; rows are built
//! from them on every read, so a mark still lands on each notice a row holds.

use super::{Notification, NotificationKind};
use crate::planning::work::types::{WorkActor, WorkStatus};
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;

/// An agent's notices on one task join one row while each follows the last
/// within this long.
pub(crate) const BURST_MS: i64 = 10 * 60 * 1000;

/// Why a row needs the user rather than only telling them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Needs {
    /// A comment names the user.
    Mention,
    /// An agent's comment asks the user something.
    Question,
    /// A task handed to the user waits in Triage for a yes or no.
    Triage,
    Reminder,
    Due,
    /// An automation run failed.
    Failed,
}

/// One thing a row's notices did, and how many times.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Part {
    /// `created`, `assigned`, `linked`, `unlinked`, `commented`, `status`,
    /// or the history kind of a changed field.
    pub(crate) what: String,
    pub(crate) count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboxRow {
    /// The newest notice's id.
    pub(crate) id: String,
    /// Every notice the row stands for, newest first: what a mark is sent.
    pub(crate) notice_ids: Vec<String>,
    /// The task, or none when the row is about several or about none.
    pub(crate) item_id: Option<String>,
    /// The tasks a "created N tasks" row covers, oldest first.
    pub(crate) item_ids: Vec<String>,
    /// The task's project as it is filed now; nothing here guesses one.
    pub(crate) project_id: Option<String>,
    pub(crate) needs: Option<Needs>,
    pub(crate) parts: Vec<Part>,
    /// "Linked 5 tasks, added 3 comments"; empty for a row of one notice
    /// that the notice already says.
    pub(crate) summary: String,
    /// The newest notice that needs the user, or the newest one when none
    /// does, for everything a single notice shows.
    pub(crate) lead: Notification,
    pub(crate) first_at_ms: i64,
    pub(crate) created_at_ms: i64,
    pub(crate) unread: bool,
}

/// What rows read about a task besides its notices.
#[derive(Clone, Debug, Default)]
pub(crate) struct TaskFacts {
    pub(crate) project_id: Option<String>,
    pub(crate) triage: bool,
}

/// What the notices do not carry: each task's project and whether it waits
/// in Triage, and the words of each comment a notice is about.
#[derive(Default)]
pub(crate) struct Context {
    pub(crate) tasks: HashMap<String, TaskFacts>,
    pub(crate) comments: HashMap<String, String>,
    /// Link notices an older build stored for a link's other end, each with
    /// the notice of the link it mirrors.
    pub(crate) echoes: HashMap<String, String>,
    /// The local model's verdict on a comment, where it has given one.
    pub(crate) judged: HashMap<String, bool>,
}

impl Context {
    pub(super) fn asking_comment(&self, comment_id: &str) -> Option<&String> {
        let body = self.comments.get(comment_id)?;
        self.judged
            .get(comment_id)
            .copied()
            .unwrap_or_else(|| asks(body))
            .then_some(body)
    }
}

pub(crate) fn context(conn: &Connection) -> Result<Context> {
    let mut tasks = conn.prepare(
        "SELECT id, project_id, status = 'triage' FROM planning_tasks
         WHERE id IN (SELECT task_id FROM planning_notifications)",
    )?;
    let tasks = tasks
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                TaskFacts {
                    project_id: row.get(1)?,
                    triage: row.get(2)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut comments = conn.prepare(
        "SELECT c.id, c.body FROM planning_task_comments c
         WHERE c.deleted_at_ms IS NULL
           AND c.id IN (SELECT comment_id FROM planning_notifications)",
    )?;
    let comments = comments
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Context {
        tasks,
        comments,
        echoes: echoes(conn)?,
        judged: super::asks::judged(conn)?,
    })
}

/// The stored link notices that mirror another stored notice. A mirror is
/// known by its history: an older build wrote a link's two entries one after
/// the other and a notice for each. What is hidden from a list changes
/// nothing here, and a notice that cannot be shown to be a mirror is kept.
fn echoes(conn: &Connection) -> Result<HashMap<String, String>> {
    struct End {
        notice: String,
        task: String,
        at_ms: i64,
        actor: String,
        mark: String,
        links: bool,
    }
    let mut ends = conn.prepare(
        "SELECT a.rowid, n.id, a.task_id, a.at_ms, a.actor,
                coalesce(a.to_value, a.from_value), a.to_value IS NOT NULL
         FROM planning_notifications n
         JOIN planning_task_activity a ON a.id = n.activity_id
         WHERE a.kind = 'relation' AND coalesce(a.to_value, a.from_value) IS NOT NULL",
    )?;
    let ends: HashMap<i64, End> = ends
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                End {
                    notice: row.get(1)?,
                    task: row.get(2)?,
                    at_ms: row.get(3)?,
                    actor: row.get(4)?,
                    mark: row.get(5)?,
                    links: row.get(6)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut echoes = HashMap::new();
    for (written, end) in &ends {
        let Some(made) = ends.get(&(written - 1)) else {
            continue;
        };
        let (Some((verb, other)), Some((made_verb, made_other))) =
            (end.mark.split_once(':'), made.mark.split_once(':'))
        else {
            continue;
        };
        if other == made.task
            && made_other == end.task
            && verb == mirrored(made_verb)
            && end.at_ms == made.at_ms
            && end.actor == made.actor
            && end.links == made.links
        {
            echoes.insert(end.notice.clone(), made.notice.clone());
        }
    }
    Ok(echoes)
}

/// Whether a comment asks its reader for something: a question, or one of
/// the ways an agent says it is waiting.
const WAITING: [&str; 8] = [
    "please confirm",
    "please decide",
    "need your",
    "needs your",
    "waiting on you",
    "blocked on you",
    "let me know",
    "your call",
];

pub(crate) fn asks(body: &str) -> bool {
    let lower = body.to_lowercase();
    let question = lower
        .split(['\n', '.', '!'])
        .any(|sentence| sentence.trim_end().ends_with('?'));
    question || WAITING.iter().any(|phrase| lower.contains(phrase))
}

/// The words of a comment notice that asks the user for something.
fn asking<'a>(notice: &Notification, context: &'a Context) -> Option<&'a String> {
    notice
        .comment_id
        .as_ref()
        .filter(|_| notice.kind == NotificationKind::Comment)
        .and_then(|id| context.asking_comment(id))
}

/// Every sentence of a comment that asks, whole;
/// its first sentence when no one sentence does.
fn asked(body: &str) -> Vec<String> {
    // A sentence ends at a line break, or at a stop that white space or the
    // end follows: the stop inside "v2.1" ends nothing.
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut letters = body.char_indices().peekable();
    let mut stopped = false;
    while let Some((at, letter)) = letters.next() {
        let next = letters.peek().map(|(_, next)| *next);
        // A closing quote or bracket after a stop belongs to its sentence,
        // and ends it only when what follows does not carry on in lowercase:
        // `rename it to "Ready!" before Friday?` is one sentence.
        let closes = stopped && matches!(letter, '"' | '\'' | ')' | ']' | '”' | '’');
        stopped = matches!(letter, '.' | '!' | '?') || closes;
        let carries_on = closes
            && body[at + letter.len_utf8()..]
                .trim_start()
                .starts_with(char::is_lowercase);
        let ends =
            letter == '\n' || (stopped && !carries_on && next.is_none_or(char::is_whitespace));
        if ends || next.is_none() {
            let end = at + letter.len_utf8();
            let sentence = body[start..end].trim();
            if !sentence.is_empty() {
                sentences.push(sentence);
            }
            start = end;
        }
    }
    let asking: Vec<&str> = sentences
        .iter()
        .copied()
        .filter(|sentence| asks(sentence))
        .collect();
    if asking.is_empty() {
        // No one sentence asks: the start of the comment, cut to a line.
        return sentences
            .first()
            .map(|said| match said.char_indices().nth(80) {
                Some((cut, _)) => format!("{}…", said[..cut].trim_end()),
                None => (*said).to_owned(),
            })
            .into_iter()
            .collect();
    }
    // Whole, however long: a cut can drop what is asked, and the screen
    // that shows the line is where it is shortened.
    asking.into_iter().map(str::to_owned).collect()
}

fn created(notice: &Notification) -> bool {
    notice.kind == NotificationKind::Assigned
        && notice.actor == WorkActor::Agent
        && notice.from.is_none()
}

/// How a relation notice links its own task, and the task it links it to.
fn link(notice: &Notification) -> Option<(&str, &str)> {
    if notice.kind != NotificationKind::Changed || notice.field.as_deref() != Some("relation") {
        return None;
    }
    let mark = notice.to.as_deref().or(notice.from.as_deref())?;
    mark.split_once(':')
}

fn other_end(notice: &Notification) -> Option<&str> {
    link(notice).map(|(_, other)| other)
}

/// How the other task's history names a link this task's names `verb`.
pub(crate) fn mirrored(verb: &str) -> &str {
    match verb {
        "blocks" => "blocked_by",
        "blocked_by" => "blocks",
        same => same,
    }
}

fn what(notice: &Notification) -> String {
    match notice.kind {
        NotificationKind::Assigned if created(notice) => "created".to_owned(),
        NotificationKind::Assigned => "assigned".to_owned(),
        NotificationKind::Comment => "commented".to_owned(),
        NotificationKind::Status => "status".to_owned(),
        NotificationKind::Unblocked => "unblocked".to_owned(),
        NotificationKind::Changed if other_end(notice).is_some() => if notice.to.is_some() {
            "linked"
        } else {
            "unlinked"
        }
        .to_owned(),
        _ => notice.field.clone().unwrap_or_default(),
    }
}

struct Building {
    notices: Vec<Notification>,
    /// The other side of a link a kept notice already tells: marked with
    /// the row, counted nowhere.
    echoes: Vec<Notification>,
    last_at_ms: i64,
    batch: bool,
}

/// What the row needs from the user, and the newest notice that needs it.
fn needs<'a>(row: &'a Building, context: &Context) -> Option<(Needs, &'a Notification)> {
    let triage = |notice: &Notification| {
        notice
            .item_id
            .as_ref()
            .and_then(|id| context.tasks.get(id))
            .is_some_and(|task| task.triage)
    };
    row.notices
        .iter()
        .rev()
        .filter_map(|notice| {
            let needs = match notice.kind {
                NotificationKind::Mention => Some(Needs::Mention),
                NotificationKind::Reminder => Some(Needs::Reminder),
                NotificationKind::Due => Some(Needs::Due),
                NotificationKind::Automation if notice.to.as_deref() == Some("failed") => {
                    Some(Needs::Failed)
                }
                NotificationKind::Comment => asking(notice, context).map(|_| Needs::Question),
                NotificationKind::Assigned if triage(notice) => Some(Needs::Triage),
                _ => None,
            };
            needs.map(|needs| (needs, notice))
        })
        .min_by_key(|(needs, _)| match needs {
            Needs::Failed => 0,
            Needs::Mention => 1,
            Needs::Question => 2,
            Needs::Due => 3,
            Needs::Reminder => 4,
            Needs::Triage => 5,
        })
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        one.to_owned()
    } else {
        format!("{count} {many}")
    }
}

fn field_name(field: &str) -> &str {
    match field {
        "due" => "due date",
        "recurrence" => "repeat",
        "block" => "blockers",
        "attachment" => "attachments",
        other => other,
    }
}

fn is_status(to: &str) -> bool {
    serde_json::from_value::<WorkStatus>(serde_json::Value::String(to.to_owned())).is_ok()
}

fn summary(parts: &[Part], status: Option<&str>) -> String {
    let mut said = Vec::new();
    let mut fields = Vec::new();
    for part in parts {
        match part.what.as_str() {
            "created" => said.push(plural(
                part.count,
                "created it for you",
                "tasks created for you",
            )),
            "assigned" => said.push("assigned it to you".to_owned()),
            "linked" => said.push(format!("linked {}", plural(part.count, "a task", "tasks"))),
            "unlinked" => said.push(format!(
                "unlinked {}",
                plural(part.count, "a task", "tasks")
            )),
            "commented" => said.push(format!(
                "added {}",
                plural(part.count, "a comment", "comments")
            )),
            "status" => said.push(match status {
                Some(to) if is_status(to) => format!("moved it to {}", to.replace('_', " ")),
                // Marking a duplicate records the task it duplicates, not a status.
                Some(_) => "marked it a duplicate".to_owned(),
                None => "changed its status".to_owned(),
            }),
            "archived" | "restored" | "snoozed" | "unblocked" => {
                said.push(format!("{} it", part.what))
            }
            field => fields.push(field_name(field)),
        }
    }
    if let Some((last, rest)) = fields.split_last() {
        said.push(if rest.is_empty() {
            format!("changed the {last}")
        } else {
            format!("changed the {} and {last}", rest.join(", "))
        });
    }
    let mut text = said.join(", ");
    if let Some(first) = text.get(..1) {
        text = first.to_uppercase() + &text[1..];
    }
    text
}

fn finish(row: Building, context: &Context) -> Option<InboxRow> {
    let needing = needs(&row, context);
    let newest = row.notices.last()?;
    // A burst leads with what it needs from the user, not with its last edit.
    let lead = needing
        .as_ref()
        .map_or(newest, |(_, notice)| notice)
        .clone();
    let needs = needing.map(|(needs, _)| needs);
    let mut parts: Vec<Part> = Vec::new();
    for notice in &row.notices {
        let what = what(notice);
        match parts.iter_mut().find(|part| part.what == what) {
            Some(part) => part.count += 1,
            None => parts.push(Part { what, count: 1 }),
        }
    }
    let status = row
        .notices
        .iter()
        .rev()
        .find(|notice| notice.kind == NotificationKind::Status)
        .and_then(|notice| notice.to.as_deref());
    let grouped = row.notices.len() > 1;
    let asking: Vec<&String> = row
        .notices
        .iter()
        .filter_map(|notice| asking(notice, context))
        .collect();
    let asked: Vec<String> = asking
        .iter()
        .flat_map(|body| asked(body))
        .map(|said| format!("\"{said}\""))
        .collect();
    let summary = if row.batch {
        format!("Created {} tasks for you", row.notices.len())
    } else if let (true, Some((last, others))) = (grouped, asked.split_last()) {
        // Every ask is named in its own words; the rest counts what is left.
        let rest: Vec<Part> = parts
            .iter()
            .map(|part| Part {
                what: part.what.clone(),
                count: part.count - asking.len() * usize::from(part.what == "commented"),
            })
            .filter(|part| part.count > 0)
            .collect();
        let rest = summary(&rest, status);
        let mut line = if others.is_empty() {
            format!("Asked {last}")
        } else {
            format!("Asked {} and {last}", others.join(", "))
        };
        if let Some(first) = rest.get(..1) {
            line = format!("{line}, {}{}", first.to_lowercase(), &rest[1..]);
        }
        line
    } else if grouped {
        summary(&parts, status)
    } else {
        String::new()
    };
    let item_ids: Vec<String> = if row.batch {
        row.notices
            .iter()
            .filter_map(|n| n.item_id.clone())
            .collect()
    } else {
        Vec::new()
    };
    let item_id = if row.batch {
        None
    } else {
        lead.item_id.clone()
    };
    let project_of = |id: &String| {
        context
            .tasks
            .get(id)
            .and_then(|task| task.project_id.clone())
    };
    let project_id = match &item_id {
        Some(id) => project_of(id),
        // A batch names a project only when every task in it shares one.
        None => item_ids
            .split_first()
            .map(|(first, rest)| (project_of(first), rest))
            .filter(|(first, rest)| rest.iter().all(|id| project_of(id) == *first))
            .and_then(|(first, _)| first),
    };
    let mut notice_ids: Vec<String> = row.notices.iter().rev().map(|n| n.id.clone()).collect();
    notice_ids.extend(row.echoes.iter().map(|n| n.id.clone()));
    Some(InboxRow {
        id: newest.id.clone(),
        notice_ids,
        item_id,
        item_ids,
        project_id,
        needs,
        parts,
        summary,
        first_at_ms: row.notices.first()?.created_at_ms,
        created_at_ms: newest.created_at_ms,
        unread: row
            .notices
            .iter()
            .chain(&row.echoes)
            .any(|notice| notice.read_at_ms.is_none()),
        lead,
    })
}

/// Rows for `notices` (newest first, as `notifications` returns them): the
/// ones that need the user first, then newest first.
pub(crate) fn rows(notices: Vec<Notification>, context: &Context) -> Vec<InboxRow> {
    build(notices, context, false)
}

/// Rows as `rows`, but one per task: a task's notices read as one row, whichever
/// agent filed them and however far apart they are.
pub(crate) fn tasks(notices: Vec<Notification>, context: &Context) -> Vec<InboxRow> {
    build(notices, context, true)
}

fn build(notices: Vec<Notification>, context: &Context, by_task: bool) -> Vec<InboxRow> {
    let agent = |notice: &Notification| notice.agent.clone().unwrap_or_default();
    let mut oldest_first = notices;
    oldest_first.reverse();

    // Tasks one agent files for the user in one run read as one row.
    let mut batch_of: HashMap<String, usize> = HashMap::new();
    let mut runs: Vec<Vec<usize>> = Vec::new();
    let mut open: HashMap<String, (usize, i64)> = HashMap::new();
    for (at, notice) in oldest_first.iter().enumerate() {
        if !created(notice) {
            continue;
        }
        let run = match open.get(&agent(notice)) {
            Some((run, last)) if notice.created_at_ms - last <= BURST_MS => *run,
            _ => {
                runs.push(Vec::new());
                runs.len() - 1
            }
        };
        runs[run].push(at);
        open.insert(agent(notice), (run, notice.created_at_ms));
    }
    for (run, members) in runs.iter().enumerate() {
        if members.len() > 1 {
            for member in members {
                batch_of.insert(oldest_first[*member].id.clone(), run);
            }
        }
    }

    let mut built: Vec<Building> = Vec::new();
    let mut bursts: HashMap<(String, String), usize> = HashMap::new();
    let mut batches: HashMap<usize, usize> = HashMap::new();
    // The row each link notice went to, for the mirror an older build stored.
    let mut told: HashMap<String, usize> = HashMap::new();
    let listed: std::collections::HashSet<String> = oldest_first
        .iter()
        .map(|notice| notice.id.clone())
        .collect();
    let mut mirrors = Vec::new();
    for notice in oldest_first {
        let at_ms = notice.created_at_ms;
        if let Some(run) = batch_of.get(&notice.id) {
            let row = *batches.entry(*run).or_insert_with(|| {
                built.push(Building {
                    notices: Vec::new(),
                    echoes: Vec::new(),
                    last_at_ms: at_ms,
                    batch: true,
                });
                built.len() - 1
            });
            built[row].notices.push(notice);
            built[row].last_at_ms = at_ms;
            continue;
        }
        let joins = (by_task || notice.actor == WorkActor::Agent)
            && matches!(
                notice.kind,
                NotificationKind::Changed
                    | NotificationKind::Comment
                    | NotificationKind::Assigned
                    | NotificationKind::Status
            );
        let Some(task) = notice.item_id.clone().filter(|_| joins) else {
            built.push(Building {
                notices: vec![notice],
                echoes: Vec::new(),
                last_at_ms: at_ms,
                batch: false,
            });
            continue;
        };
        // A mirror goes to its link's row once every row is built: the two
        // share an instant, so either may come first.
        if context
            .echoes
            .get(&notice.id)
            .is_some_and(|made| listed.contains(made))
        {
            mirrors.push(notice);
            continue;
        }
        let key = (
            task.clone(),
            if by_task {
                String::new()
            } else {
                agent(&notice)
            },
        );
        let row = match bursts.get(&key) {
            Some(row) if by_task || at_ms - built[*row].last_at_ms <= BURST_MS => *row,
            _ => {
                built.push(Building {
                    notices: Vec::new(),
                    echoes: Vec::new(),
                    last_at_ms: at_ms,
                    batch: false,
                });
                bursts.insert(key, built.len() - 1);
                built.len() - 1
            }
        };
        if link(&notice).is_some() {
            told.insert(notice.id.clone(), row);
        }
        built[row].notices.push(notice);
        built[row].last_at_ms = at_ms;
    }
    for mirror in mirrors {
        let row = context
            .echoes
            .get(&mirror.id)
            .and_then(|made| told.get(made));
        match row {
            Some(row) => built[*row].echoes.push(mirror),
            // Its link is listed but joined no row of links: nothing is dropped.
            None => built.push(Building {
                last_at_ms: mirror.created_at_ms,
                notices: vec![mirror],
                echoes: Vec::new(),
                batch: false,
            }),
        }
    }

    let mut rows: Vec<InboxRow> = built
        .into_iter()
        .filter_map(|row| finish(row, context))
        .collect();
    rows.reverse();
    rows.sort_by_key(|row| (row.needs.is_none(), std::cmp::Reverse(row.created_at_ms)));
    rows
}

/// The unread rows among `notices`. Built from read and unread notices
/// alike, so reading part of a burst does not split what is left of it.
pub(crate) fn waiting(
    notices: Vec<Notification>,
    context: &Context,
    by_task: bool,
) -> Vec<InboxRow> {
    let mut rows = build(notices, context, by_task);
    rows.retain(|row| row.unread);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::schema;
    use crate::planning::work::inbox::{self, Mark, Shown};
    use crate::planning::work::items;
    use crate::planning::work::types::{
        WorkActivityKind, WorkItemInput, WorkOwner, WorkRelationKind,
    };
    use crate::planning::work::{create, Change};
    use std::collections::BTreeSet;
    use std::time::Instant;

    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;
    const START: i64 = 1_800_000_000_000;

    /// A synthetic inbox shaped like the one issue 42 counted: 686 notices
    /// on 102 tasks in 5 projects over 52 hours, 682 of them an agent's,
    /// most links told from both ends. Nothing in it was ever recorded.
    struct Fixture {
        /// Newest first, as `notifications` returns them.
        notices: Vec<Notification>,
        context: Context,
        /// The notices each row should hold, as written by the generator.
        rows: BTreeSet<BTreeSet<String>>,
        /// The rows that need the user, by any notice in them.
        needing: BTreeSet<BTreeSet<String>>,
    }

    struct Dice(u64);

    impl Dice {
        fn roll(&mut self, sides: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % sides
        }
    }

    /// Comments an agent leaves, and whether each one waits on the user.
    /// The last four are the hard ones: an ask with no question mark and
    /// none of the stock phrases, and questions the comment answers itself.
    const COMMENTS: [(&str, bool); 16] = [
        ("Linked the related rollout tasks.", false),
        ("Moved the due date to match the launch plan.", false),
        ("Should this ship before the pricing page or after?", true),
        ("Split the checklist into three sub-tasks.", false),
        ("Which vendor do you want for the venue?", true),
        ("Tests pass on the branch; nothing else to do here.", false),
        ("Blocked on you: the contract needs a signature.", true),
        ("Added the notes from the sync as an attachment.", false),
        ("Please confirm the budget ceiling before I book.", true),
        ("Closed the duplicate and kept the older task.", false),
        ("Two options for the banner copy. Your call.", true),
        ("Raised the priority after the outage report.", false),
        (
            "Unsure whether Friday or Monday is better; holding until I hear back.",
            true,
        ),
        (
            "I cannot pick the color without sign-off from the owner.",
            true,
        ),
        (
            "Why did the import fail? The cache was stale; cleared it.",
            false,
        ),
        (
            "Is the migration done? Yes, verified on the replica.",
            false,
        ),
    ];

    const FIELDS: [&str; 6] = ["priority", "due", "title", "details", "labels", "parent"];

    fn fixture() -> Fixture {
        let mut dice = Dice(42);
        let mut context = Context::default();
        let task = |number: usize| format!("task-{number:03}");
        for number in 0..102 {
            context.tasks.insert(
                task(number),
                TaskFacts {
                    project_id: Some(format!("project-{}", number % 5)),
                    triage: false,
                },
            );
        }
        let mut notices: Vec<Notification> = Vec::new();
        let mut rows = BTreeSet::new();
        let mut needing = BTreeSet::new();
        let post = |notices: &mut Vec<Notification>,
                    kind: NotificationKind,
                    actor: WorkActor,
                    task: Option<String>,
                    at: i64| {
            let id = format!("notice-{:04}", notices.len());
            notices.push(Notification {
                excerpt: None,
                id: id.clone(),
                item_id: task,
                kind,
                actor,
                agent: (actor == WorkActor::Agent).then(|| "Scout".to_owned()),
                comment_id: None,
                field: None,
                automation: None,
                from: None,
                to: None,
                created_at_ms: at,
                read_at_ms: None,
                snoozed_until_ms: None,
                archived_at_ms: None,
                digest: None,
                priority: kind.priority(),
                subject: None,
                earlier: Vec::new(),
            });
            id
        };

        // Three runs that file tasks for the user: one row each.
        let mut next_task = 0;
        for (run, size) in [12usize, 8, 5].into_iter().enumerate() {
            let mut row = BTreeSet::new();
            for step in 0..size {
                let at = START + run as i64 * 600 * MINUTE + step as i64 * 4_000;
                let id = post(
                    &mut notices,
                    NotificationKind::Assigned,
                    WorkActor::Agent,
                    Some(task(next_task)),
                    at,
                );
                notices.last_mut().expect("the notice").to = Some("self".to_owned());
                row.insert(id);
                next_task += 1;
            }
            rows.insert(row);
        }

        // Three tasks handed over one at a time, still waiting in Triage.
        for number in 25..28 {
            let at = START + 100 * MINUTE + number as i64 * 45 * MINUTE;
            let id = post(
                &mut notices,
                NotificationKind::Assigned,
                WorkActor::Agent,
                Some(task(number)),
                at,
            );
            notices.last_mut().expect("the notice").to = Some("self".to_owned());
            context
                .tasks
                .get_mut(&task(number))
                .expect("the task")
                .triage = true;
            let row = BTreeSet::from([id]);
            needing.insert(row.clone());
            rows.insert(row);
        }

        // What Sona itself posts, and six mentions: each its own row.
        let own = [
            (NotificationKind::Reminder, WorkActor::Sona),
            (NotificationKind::Reminder, WorkActor::Sona),
            (NotificationKind::Due, WorkActor::Sona),
            (NotificationKind::Digest, WorkActor::Sona),
            (NotificationKind::Mention, WorkActor::Agent),
            (NotificationKind::Mention, WorkActor::Agent),
            (NotificationKind::Mention, WorkActor::Agent),
            (NotificationKind::Mention, WorkActor::Agent),
            (NotificationKind::Mention, WorkActor::Agent),
            (NotificationKind::Mention, WorkActor::Agent),
        ];
        for (step, (kind, actor)) in own.into_iter().enumerate() {
            let at = START + 37 * MINUTE + step as i64 * 300 * MINUTE;
            let about = (kind != NotificationKind::Digest).then(|| task(30 + step));
            let id = post(&mut notices, kind, actor, about, at);
            let row = BTreeSet::from([id]);
            if kind != NotificationKind::Digest {
                needing.insert(row.clone());
            }
            rows.insert(row);
        }

        // Sittings: an agent works one task for a few minutes, then leaves
        // it for at least half an hour. Links are told from both ends.
        let mut free_at = vec![START; 102];
        for number in 25..28 {
            free_at[number] = START + 100 * MINUTE + number as i64 * 45 * MINUTE;
        }
        let mut sitting = 0;
        let mut comment = 0;
        let mut links = 0;
        while notices.len() < 686 {
            // Every task is worked once before any is worked again.
            let number = if sitting < 102 {
                sitting
            } else {
                dice.roll(102) as usize
            };
            sitting += 1;
            let mut at = free_at[number] + 30 * MINUTE + dice.roll(40 * MINUTE as u64) as i64;
            let mut row = BTreeSet::new();
            let mut asks_user = false;
            let size = 1 + dice.roll(6) as usize;
            for _ in 0..size {
                if notices.len() >= 686 {
                    break;
                }
                at += 5_000 + dice.roll(4 * MINUTE as u64) as i64;
                match dice.roll(10) {
                    0..=3 if notices.len() + 2 <= 686 && links < 118 => {
                        let other = (number + 1 + dice.roll(101) as usize) % 102;
                        let id = post(
                            &mut notices,
                            NotificationKind::Changed,
                            WorkActor::Agent,
                            Some(task(number)),
                            at,
                        );
                        let told = notices.last_mut().expect("the notice");
                        told.field = Some("relation".to_owned());
                        told.to = Some(format!("related:{}", task(other)));
                        row.insert(id.clone());
                        let echo = post(
                            &mut notices,
                            NotificationKind::Changed,
                            WorkActor::Agent,
                            Some(task(other)),
                            at,
                        );
                        let echoed = notices.last_mut().expect("the notice");
                        echoed.field = Some("relation".to_owned());
                        echoed.to = Some(format!("related:{}", task(number)));
                        context.echoes.insert(echo.clone(), id);
                        row.insert(echo);
                        links += 1;
                    }
                    4..=5 => {
                        let (body, asks) = COMMENTS[comment % COMMENTS.len()];
                        let id = post(
                            &mut notices,
                            NotificationKind::Comment,
                            WorkActor::Agent,
                            Some(task(number)),
                            at,
                        );
                        let comment_id = format!("comment-{comment}");
                        notices.last_mut().expect("the notice").comment_id =
                            Some(comment_id.clone());
                        context.comments.insert(comment_id, body.to_owned());
                        asks_user |= asks;
                        comment += 1;
                        row.insert(id);
                    }
                    6 => {
                        let id = post(
                            &mut notices,
                            NotificationKind::Status,
                            WorkActor::Agent,
                            Some(task(number)),
                            at,
                        );
                        notices.last_mut().expect("the notice").to = Some("in_progress".to_owned());
                        row.insert(id);
                    }
                    _ => {
                        let id = post(
                            &mut notices,
                            NotificationKind::Changed,
                            WorkActor::Agent,
                            Some(task(number)),
                            at,
                        );
                        notices.last_mut().expect("the notice").field =
                            Some(FIELDS[dice.roll(6) as usize].to_owned());
                        row.insert(id);
                    }
                }
            }
            free_at[number] = at;
            if row.is_empty() {
                continue;
            }
            if asks_user {
                needing.insert(row.clone());
            }
            rows.insert(row);
        }
        // Newest first, and of two written in the same millisecond the later
        // one first, as the table's row order has them.
        notices.reverse();
        notices.sort_by_key(|notice| std::cmp::Reverse(notice.created_at_ms));
        Fixture {
            notices,
            context,
            rows,
            needing,
        }
    }

    fn held(row: &InboxRow) -> BTreeSet<String> {
        row.notice_ids.iter().cloned().collect()
    }

    /// How many of the first `top` rows need the user.
    fn needed_on_top(
        shown: &[BTreeSet<String>],
        needing: &BTreeSet<BTreeSet<String>>,
        top: usize,
    ) -> usize {
        shown
            .iter()
            .take(top)
            .filter(|row| needing.iter().any(|needs| !needs.is_disjoint(row)))
            .count()
    }

    #[test]
    fn the_fixture_has_the_shape_issue_42_counted() {
        let fixture = fixture();
        assert_eq!(fixture.notices.len(), 686);
        let sona = fixture
            .notices
            .iter()
            .filter(|notice| notice.actor == WorkActor::Sona)
            .count();
        assert_eq!(sona, 4);
        let tasks: BTreeSet<_> = fixture
            .notices
            .iter()
            .filter_map(|n| n.item_id.clone())
            .collect();
        assert_eq!(tasks.len(), 102);
    }

    #[test]
    fn rows_collapse_the_synthetic_inbox_and_put_what_needs_the_user_first() {
        let fixture = fixture();
        let top = fixture.needing.len();

        // Before: one row per notice, newest first.
        let before: Vec<BTreeSet<String>> = fixture
            .notices
            .iter()
            .map(|notice| BTreeSet::from([notice.id.clone()]))
            .collect();
        let before_exact = before
            .iter()
            .filter(|row| fixture.rows.contains(*row))
            .count();
        let before_top = needed_on_top(&before, &fixture.needing, top);

        let after = rows(fixture.notices.clone(), &fixture.context);
        let shown: Vec<BTreeSet<String>> = after.iter().map(held).collect();
        let exact = shown
            .iter()
            .filter(|row| fixture.rows.contains(*row))
            .count();
        let flagged: BTreeSet<_> = after
            .iter()
            .filter(|row| row.needs.is_some())
            .map(held)
            .collect();
        let right = flagged.intersection(&fixture.needing).count();
        let after_top = needed_on_top(&shown, &fixture.needing, top);

        println!(
            "inbox fixture: {} notices, {} rows expected, {top} need the user",
            before.len(),
            fixture.rows.len()
        );
        println!("before: {} rows shown, {before_exact}/{} expected rows shown as one row, {before_top}/{top} of the top {top} need the user", before.len(), fixture.rows.len());
        println!("after:  {} rows shown, {exact}/{} expected rows shown as one row, {after_top}/{top} of the top {top} need the user", shown.len(), fixture.rows.len());
        println!(
            "after:  needs-the-user precision {right}/{}, recall {right}/{top}",
            flagged.len()
        );

        assert_eq!(shown.len(), fixture.rows.len());
        assert_eq!(exact, fixture.rows.len());
        // Every notice is in exactly one row, so a mark reaches all of them.
        let all: Vec<&String> = after.iter().flat_map(|row| &row.notice_ids).collect();
        assert_eq!(all.len(), 686);
        assert_eq!(all.iter().collect::<BTreeSet<_>>().len(), 686);
        // The rule flags nothing that does not need the user and misses the
        // asks with no question mark and none of its phrases (11 of 54 rows
        // here). These floors are what it scores today, there to catch a
        // slide; a judgment that reads the comment should raise the second.
        assert_eq!(right, flagged.len(), "precision {right}/{}", flagged.len());
        assert!(right * 100 >= top * 79, "recall {right}/{top}");
        assert!(after_top > before_top);
    }

    #[test]
    fn rows_for_the_synthetic_inbox_are_built_in_a_few_milliseconds() {
        let fixture = fixture();
        let mut took: Vec<u128> = (0..200)
            .map(|_| {
                let notices = fixture.notices.clone();
                let start = Instant::now();
                let built = rows(notices, &fixture.context);
                let took = start.elapsed().as_micros();
                assert!(!built.is_empty());
                took
            })
            .collect();
        took.sort_unstable();
        println!(
            "rows for 686 notices: median {} us, p95 {} us",
            took[100], took[190]
        );
        assert!(took[100] < 50_000, "median {} us", took[100]);
    }

    #[test]
    fn a_burst_says_what_it_did_in_one_line() {
        let fixture = fixture();
        let mut notices: Vec<Notification> = Vec::new();
        let template = fixture.notices[0].clone();
        let mut add = |kind, field: Option<&str>, to: Option<&str>| {
            let at = START + notices.len() as i64 * 30_000;
            notices.insert(
                0,
                Notification {
                    id: format!("n{}", notices.len()),
                    item_id: Some("task-050".to_owned()),
                    kind,
                    actor: WorkActor::Agent,
                    agent: Some("Scout".to_owned()),
                    comment_id: None,
                    field: field.map(str::to_owned),
                    from: None,
                    to: to.map(str::to_owned),
                    created_at_ms: at,
                    ..template.clone()
                },
            );
        };
        for other in 1..=5 {
            add(
                NotificationKind::Changed,
                Some("relation"),
                Some(&format!("related:task-00{other}")),
            );
        }
        for _ in 0..3 {
            add(NotificationKind::Comment, None, None);
        }
        add(NotificationKind::Changed, Some("priority"), Some("1"));
        add(NotificationKind::Changed, Some("due"), None);
        let built = rows(notices, &fixture.context);
        assert_eq!(built.len(), 1);
        assert_eq!(
            built[0].summary,
            "Linked 5 tasks, added 3 comments, changed the priority and due date"
        );
        assert_eq!(built[0].project_id.as_deref(), Some("project-0"));
        assert_eq!(built[0].notice_ids.len(), 10);
        assert!(built[0].needs.is_none());
    }

    fn agent_notice(id: &str, at_ms: i64, read: bool, to: Option<&str>) -> Notification {
        Notification {
            id: id.to_owned(),
            item_id: Some("task-050".to_owned()),
            kind: NotificationKind::Changed,
            actor: WorkActor::Agent,
            agent: Some("Scout".to_owned()),
            comment_id: None,
            field: Some(if to.is_some() { "relation" } else { "priority" }.to_owned()),
            from: None,
            to: to.map(str::to_owned),
            created_at_ms: at_ms,
            read_at_ms: read.then_some(at_ms),
            ..fixture().notices[0].clone()
        }
    }

    #[test]
    fn reading_the_middle_of_a_burst_leaves_one_unread_row() {
        let context = fixture().context;
        // Newest first; the read middle notice bridges a gap over ten minutes.
        let notices = vec![
            agent_notice("c", START + 12 * MINUTE, false, None),
            agent_notice("b", START + 6 * MINUTE, true, None),
            agent_notice("a", START, false, None),
        ];
        let built = waiting(notices, &context, false);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].notice_ids.len(), 3);
    }

    #[test]
    fn a_row_is_unread_while_its_stored_echo_is() {
        let mut context = fixture().context;
        context
            .echoes
            .insert("echo".to_owned(), "source".to_owned());
        let echo = Notification {
            item_id: Some("task-001".to_owned()),
            ..agent_notice("echo", START, false, Some("related:task-050"))
        };
        let source = agent_notice("source", START, true, Some("related:task-001"));
        let built = waiting(vec![echo, source], &context, false);
        assert_eq!(built.len(), 1);
        assert!(built[0].unread);
        assert_eq!(built[0].notice_ids, ["source", "echo"]);
    }

    #[test]
    fn a_duplicate_is_not_named_as_a_status() {
        let context = fixture().context;
        let marked = Notification {
            kind: NotificationKind::Status,
            field: None,
            to: Some("0b0e6f0e-6f0e-4b0e-8f0e-0b0e6f0e6f0e".to_owned()),
            ..agent_notice("b", START + MINUTE, false, None)
        };
        let built = rows(
            vec![marked, agent_notice("a", START, false, None)],
            &context,
        );
        assert_eq!(
            built[0].summary,
            "Marked it a duplicate, changed the priority"
        );
        let moved = Notification {
            kind: NotificationKind::Status,
            field: None,
            to: Some("in_progress".to_owned()),
            ..agent_notice("b", START + MINUTE, false, None)
        };
        let built = rows(vec![moved, agent_notice("a", START, false, None)], &context);
        assert_eq!(
            built[0].summary,
            "Moved it to in progress, changed the priority"
        );
    }

    #[test]
    fn a_comment_asks_when_it_questions_or_waits_and_not_when_it_tells() {
        assert!(asks("Which vendor do you want for the venue?"));
        assert!(asks("Blocked on you: the contract needs a signature."));
        assert!(!asks("Linked the related rollout tasks."));
        assert!(!asks(
            "See https://example.com/search?q=venue for the list."
        ));
    }

    fn by_agent(operation_id: &str, at_ms: i64) -> Change<'_> {
        Change {
            operation_id,
            actor: WorkActor::Agent,
            at_ms,
        }
    }

    /// A history with two tasks an agent filed for the user.
    fn two_filed() -> (Connection, [String; 2]) {
        // PANIC: a fixture that cannot open its database is a broken test.
        let mut history = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&history).expect("the planner schema");
        let made = ["Book the venue", "Pick the caterer"].map(|title| file(&mut history, title));
        (history, made)
    }

    fn file(history: &mut Connection, title: &str) -> String {
        {
            create(
                history,
                None,
                by_agent(&uuid::Uuid::new_v4().to_string(), START),
                &WorkItemInput {
                    title: title.to_owned(),
                    details: None,
                    status: None,
                    priority: None,
                    project_id: None,
                    milestone_id: None,
                    owner: Some(WorkOwner::Myself),
                    due_at_ms: None,
                    label_ids: None,
                    recurrence: None,
                },
            )
            .expect("a task an agent files for the user")
            .value
            .id
        }
    }

    #[test]
    fn stored_links_are_read_and_built_in_a_few_milliseconds() {
        // PANIC: a fixture that cannot open its database is a broken test.
        let mut history = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&history).expect("the planner schema");
        let made: Vec<String> = (0..102)
            .map(|number| file(&mut history, &format!("Task {number}")))
            .collect();
        for number in 0..343 {
            items::add_relation(
                &history,
                &made[number % 102],
                &made[(number + 1 + number / 102) % 102],
                WorkRelationKind::Related,
                WorkActor::Agent,
                START + MINUTE + number as i64,
            )
            .expect("a link");
        }
        store_mirrors(&history);
        let mut facts = Vec::new();
        let mut whole = Vec::new();
        for _ in 0..200 {
            let start = Instant::now();
            let stored =
                inbox::notifications(&history, Shown::default(), START + HOUR).expect("the inbox");
            let read = start.elapsed();
            let context = context(&history).expect("the facts");
            let known = start.elapsed();
            echoes(&history).expect("the mirrors");
            let again = start.elapsed() - known;
            facts.push(again.as_micros());
            assert_eq!(context.echoes.len(), 343);
            let links = stored
                .iter()
                .filter(|notice| notice.field.as_deref() == Some("relation"))
                .count();
            assert_eq!(links, 686);
            let built = rows(stored, &context);
            // The lookup already ran once inside the facts.
            whole.push((start.elapsed() - again).as_micros());
            assert!(!built.is_empty());
        }
        facts.sort_unstable();
        whole.sort_unstable();
        println!(
            "686 stored link notices, 343 mirrors: the mirror lookup median {} us, p95 {} us; read, facts and rows median {} us, p95 {} us",
            facts[100], facts[190], whole[100], whole[190]
        );
        assert!(whole[100] < 50_000, "median {} us", whole[100]);
    }

    #[test]
    fn two_links_made_at_once_from_either_task_each_keep_their_notice() {
        let (history, made) = two_filed();
        let at = START + MINUTE;
        items::add_relation(
            &history,
            &made[0],
            &made[1],
            WorkRelationKind::Blocks,
            WorkActor::Agent,
            at,
        )
        .expect("the first blocks the second");
        items::add_relation(
            &history,
            &made[1],
            &made[0],
            WorkRelationKind::Related,
            WorkActor::Agent,
            at,
        )
        .expect("the second relates to the first");
        let mut told: Vec<_> = inbox::notifications(&history, Shown::default(), at + MINUTE)
            .expect("the inbox")
            .into_iter()
            .filter(|notice| notice.field.as_deref() == Some("relation"))
            .map(|notice| (notice.item_id, notice.to))
            .collect();
        told.sort();
        let mut made_from = vec![
            (Some(made[0].clone()), Some(format!("blocks:{}", made[1]))),
            (Some(made[1].clone()), Some(format!("related:{}", made[0]))),
        ];
        made_from.sort();
        assert_eq!(
            told, made_from,
            "each link is told on the task it was made from"
        );
    }

    #[test]
    fn stored_links_fold_only_their_own_mirror() {
        let mut context = fixture().context;
        context
            .echoes
            .insert("mirror".to_owned(), "first".to_owned());
        let on = |id: &str, task: &str, to: &str| Notification {
            item_id: Some(task.to_owned()),
            ..agent_notice(id, START, false, Some(to))
        };
        // Newest first. Two links at one instant, one made from each task,
        // and the mirror of the first as an older build stored it.
        let notices = vec![
            on("second", "task-051", "related:task-050"),
            on("mirror", "task-051", "blocked_by:task-050"),
            on("first", "task-050", "blocks:task-051"),
        ];
        let built = rows(notices, &context);
        let held = |task: &str| -> Vec<String> {
            built
                .iter()
                .filter(|row| row.item_id.as_deref() == Some(task))
                .flat_map(|row| row.notice_ids.clone())
                .collect()
        };
        assert_eq!(built.len(), 2);
        assert_eq!(held("task-050"), ["first", "mirror"]);
        assert_eq!(held("task-051"), ["second"]);
        assert!(built
            .iter()
            .all(|row| row.parts.iter().map(|p| p.count).sum::<usize>() == 1));
    }

    #[test]
    fn a_stored_mirror_listed_before_its_link_still_folds_into_it() {
        let mut context = fixture().context;
        context
            .echoes
            .insert("mirror".to_owned(), "link".to_owned());
        let on = |id: &str, task: &str, to: &str| Notification {
            item_id: Some(task.to_owned()),
            ..agent_notice(id, START, false, Some(to))
        };
        // Newest first, so the mirror is read before the link it mirrors.
        let notices = vec![
            on("link", "task-050", "blocks:task-051"),
            on("mirror", "task-051", "blocked_by:task-050"),
        ];
        let built = rows(notices, &context);
        assert_eq!(built.len(), 1, "one link, told once");
        assert_eq!(built[0].item_id.as_deref(), Some("task-050"));
        assert_eq!(built[0].notice_ids, ["link", "mirror"]);
    }

    #[test]
    fn a_burst_leads_with_the_ask_a_later_edit_follows() {
        let mut context = fixture().context;
        context.comments.insert(
            "ask".to_owned(),
            "Please confirm the venue before I pay the deposit.".to_owned(),
        );
        let ask = Notification {
            kind: NotificationKind::Comment,
            comment_id: Some("ask".to_owned()),
            field: None,
            ..agent_notice("ask", START, false, None)
        };
        let notices = vec![
            agent_notice("edit-2", START + 2 * MINUTE, false, None),
            agent_notice("edit-1", START + MINUTE, false, None),
            ask,
        ];
        let built = rows(notices, &context);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].needs, Some(Needs::Question));
        assert_eq!(built[0].lead.id, "ask", "the ask leads the row");
        assert_eq!(built[0].id, "edit-2");
        assert_eq!(built[0].created_at_ms, START + 2 * MINUTE);
        assert_eq!(built[0].notice_ids, ["edit-2", "edit-1", "ask"]);
    }

    #[test]
    fn an_approval_request_leads_the_edits_around_it() {
        let mut context = fixture().context;
        let said = [
            ("ask", "Could you approve the left mockup?"),
            (
                "note",
                "I left two mockups on the task and can proceed once one is selected.",
            ),
        ];
        for (id, body) in said {
            context.comments.insert(id.to_owned(), body.to_owned());
        }
        let changed = |id: &str, field: &str, to: &str, at_ms: i64| Notification {
            field: Some(field.to_owned()),
            to: Some(to.to_owned()),
            ..agent_notice(id, at_ms, false, None)
        };
        let wrote = |id: &str, at_ms: i64| Notification {
            kind: NotificationKind::Comment,
            comment_id: Some(id.to_owned()),
            field: None,
            ..agent_notice(id, at_ms, false, None)
        };
        // Newest first, one minute apart.
        let notices = vec![
            changed("title", "title", "Pick a mockup", START + 4 * MINUTE),
            wrote("note", START + 3 * MINUTE),
            wrote("ask", START + 2 * MINUTE),
            Notification {
                kind: NotificationKind::Status,
                ..changed("status", "status", "in_progress", START + MINUTE)
            },
            changed("priority", "priority", "high", START),
        ];
        let built = rows(notices, &context);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].notice_ids.len(), 5);
        assert_eq!(built[0].needs, Some(Needs::Question));
        assert_eq!(built[0].lead.id, "ask", "the approval request leads");
        assert_eq!(built[0].lead.comment_id.as_deref(), Some("ask"));
        assert_eq!(
            built[0].summary,
            "Asked \"Could you approve the left mockup?\", moved it to in progress, \
             added a comment, changed the priority and title"
        );
    }

    #[test]
    fn a_related_link_and_a_block_made_back_at_once_each_keep_their_notice() {
        let (history, made) = two_filed();
        let at = START + MINUTE;
        items::add_relation(
            &history,
            &made[0],
            &made[1],
            WorkRelationKind::Related,
            WorkActor::Agent,
            at,
        )
        .expect("the first relates to the second");
        items::add_relation(
            &history,
            &made[1],
            &made[0],
            WorkRelationKind::Blocks,
            WorkActor::Agent,
            at,
        )
        .expect("the second blocks the first");
        let stored = inbox::notifications(&history, Shown::default(), at + MINUTE)
            .expect("the inbox")
            .into_iter()
            .filter(|notice| notice.field.as_deref() == Some("relation"))
            .collect::<Vec<_>>();
        let context = context(&history).expect("the facts");
        let mut shown: Vec<_> = rows(stored.clone(), &context)
            .into_iter()
            .flat_map(|row| row.notice_ids)
            .collect();
        shown.sort();
        let mut told: Vec<_> = stored
            .iter()
            .map(|notice| (notice.item_id.clone(), notice.to.clone()))
            .collect();
        told.sort();
        let mut made_from = vec![
            (Some(made[0].clone()), Some(format!("related:{}", made[1]))),
            (Some(made[1].clone()), Some(format!("blocks:{}", made[0]))),
        ];
        made_from.sort();
        assert_eq!(
            told, made_from,
            "each link is told on the task it was made from"
        );
        let mut ids: Vec<_> = stored.into_iter().map(|notice| notice.id).collect();
        ids.sort();
        assert_eq!(shown, ids, "and the rows keep both");
    }

    #[test]
    fn an_ask_is_named_by_the_sentence_that_asks() {
        assert_eq!(
            asked("Drafted both. Please confirm the venue before I pay. Thanks!"),
            ["Please confirm the venue before I pay."]
        );
        assert_eq!(asked("Two mockups are up"), ["Two mockups are up"]);
        assert_eq!(
            asked("Built it. Can you approve v2.1? It ships Friday."),
            ["Can you approve v2.1?"]
        );
        assert_eq!(
            asked(
                "The brief says \"ship the long version of the onboarding flow first.\" Should I?"
            ),
            ["Should I?"]
        );
        assert_eq!(
            asked("Done. Can I rename the label to \"Ready!\" before Friday?"),
            ["Can I rename the label to \"Ready!\" before Friday?"]
        );
        let long = format!("Could you {}?", "look at this ".repeat(12));
        assert_eq!(asked(&long), [long.as_str()], "whole, however long");
    }

    #[test]
    fn one_comment_with_two_asks_names_both() {
        assert_eq!(
            asked(
                "Could you approve the left mockup? Also, please confirm the launch date. Thanks!"
            ),
            [
                "Could you approve the left mockup?",
                "Also, please confirm the launch date."
            ]
        );
        let mut context = fixture().context;
        context.comments.insert(
            "both".to_owned(),
            "Could you approve the left mockup? Please confirm the launch date.".to_owned(),
        );
        let comment = Notification {
            kind: NotificationKind::Comment,
            comment_id: Some("both".to_owned()),
            field: None,
            ..agent_notice("a", START, false, None)
        };
        let built = rows(
            vec![agent_notice("b", START + MINUTE, false, None), comment],
            &context,
        );
        assert_eq!(
            built[0].summary,
            "Asked \"Could you approve the left mockup?\" and \"Please confirm the launch date.\", changed the priority"
        );
    }

    #[test]
    fn a_long_sentence_keeps_the_words_that_ask() {
        // A request in lowercase after a quoted sentence reads as one
        // sentence; the line still shows the request.
        let shown = asked(
            "The brief says \"ship the long version of the onboarding flow before anything else.\" should I go ahead with that?",
        );
        assert_eq!(shown.len(), 1);
        assert!(
            shown[0].ends_with("should I go ahead with that?"),
            "{shown:?}"
        );
        let waits = format!(
            "{} and I need your call on the venue.",
            "I drafted it ".repeat(8)
        );
        assert_eq!(asked(&waits), [waits.as_str()]);
    }

    #[test]
    fn every_ask_shows_however_long_the_words_around_it() {
        // The reviewer's case: a quoted question, a long stretch, a request.
        let shown = asked(
            "QA wrote \"Is staging ready?\" after the overnight run finished on all four regions and the dashboards went green, please confirm the live release tag.",
        );
        assert!(
            shown
                .iter()
                .any(|part| part.contains("please confirm the live release tag.")),
            "{shown:?}"
        );

        // What is to be confirmed, what is to be done and a question that
        // follows a request are not cut away or split.
        for body in [
            "Please confirm: the invoice for the March offsite goes to the finance team and the deposit comes out of the events budget.",
            "Let me know which vendor to book for the offsite in March, and can I rename the label to \"Ready!\" before Friday?",
            "I need your sign-off on the budget for the offsite, and should I book the larger room for the whole week?",
        ] {
            assert!(body.chars().count() > 80);
            assert_eq!(asked(body), [body]);
        }

        // Every ask the rule recognises is in the line, whatever surrounds it.
        let fill = |letters: usize| {
            "the nightly export ran clean and ".repeat(letters / 33 + 1)[..letters].to_owned()
        };
        let asks = [
            "please confirm the live release tag",
            "need your sign-off on the budget",
            "let me know which vendor to book",
            "your call on the venue",
        ];
        for length in [0, 20, 79, 80, 81, 200, 600] {
            for quote in [
                "",
                "QA wrote \"Is staging ready?\" and ",
                "the brief says \"Ship it!\" and ",
            ] {
                for count in 1..=asks.len() {
                    let mut body = quote.to_owned();
                    for ask in &asks[..count] {
                        body.push_str(&fill(length));
                        body.push_str(ask);
                        body.push_str(", ");
                    }
                    body.push_str(&fill(length));
                    body.push_str("so should I ship it today?");
                    let shown = asked(&body);
                    for ask in asks[..count].iter().chain(&["should I ship it today?"]) {
                        assert!(
                            shown.iter().any(|part| part.contains(ask)),
                            "{ask:?} is missing from {shown:?} for {body:?}"
                        );
                    }
                    assert_eq!(shown, [body.as_str()], "one sentence, shown whole");
                }
            }
        }
    }

    #[test]
    fn every_ask_in_a_burst_is_named() {
        let mut context = fixture().context;
        let said = [
            ("first", "Could you approve the left mockup?"),
            ("second", "Please confirm the launch date."),
        ];
        for (id, body) in said {
            context.comments.insert(id.to_owned(), body.to_owned());
        }
        let wrote = |id: &str, at_ms: i64| Notification {
            kind: NotificationKind::Comment,
            comment_id: Some(id.to_owned()),
            field: None,
            ..agent_notice(id, at_ms, false, None)
        };
        let title = Notification {
            field: Some("title".to_owned()),
            ..agent_notice("title", START + 2 * MINUTE, false, None)
        };
        let built = rows(
            vec![
                title,
                wrote("second", START + MINUTE),
                wrote("first", START),
            ],
            &context,
        );
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].needs, Some(Needs::Question));
        assert_eq!(built[0].lead.id, "second");
        assert_eq!(
            built[0].summary,
            "Asked \"Could you approve the left mockup?\" and \
             \"Please confirm the launch date.\", changed the title"
        );
    }

    #[test]
    fn a_link_made_back_after_its_removal_keeps_its_notice() {
        let (history, made) = two_filed();
        let at = START + MINUTE;
        let relation = items::add_relation(
            &history,
            &made[0],
            &made[1],
            WorkRelationKind::Related,
            WorkActor::Agent,
            at,
        )
        .expect("the first relates to the second");
        items::remove_relation(&history, &relation.id, WorkActor::Agent, at)
            .expect("the link removed");
        items::add_relation(
            &history,
            &made[1],
            &made[0],
            WorkRelationKind::Related,
            WorkActor::Agent,
            at,
        )
        .expect("the second relates to the first");
        let stored = inbox::notifications(&history, Shown::default(), at + MINUTE)
            .expect("the inbox")
            .into_iter()
            .filter(|notice| notice.field.as_deref() == Some("relation"))
            .collect::<Vec<_>>();
        let told: Vec<_> = stored
            .iter()
            .rev()
            .map(|notice| {
                (
                    notice.item_id.clone(),
                    notice.from.clone(),
                    notice.to.clone(),
                )
            })
            .collect();
        let (first, second) = (Some(made[0].clone()), Some(made[1].clone()));
        assert_eq!(
            told,
            [
                (first.clone(), None, Some(format!("related:{}", made[1]))),
                (first, Some(format!("related:{}", made[1])), None),
                (second, None, Some(format!("related:{}", made[0]))),
            ],
            "the link, its removal, and the link made back from the second"
        );
        let context = context(&history).expect("the facts");
        let shown: usize = rows(stored, &context)
            .iter()
            .map(|row| row.notice_ids.len())
            .sum();
        assert_eq!(shown, 3, "and the rows keep all three");
    }

    /// Stores a notice for every link entry that has none, as builds before
    /// this one did for a link's other end.
    fn store_mirrors(history: &Connection) {
        history
            .execute(
                "INSERT INTO planning_notifications (id, task_id, kind, actor, activity_id, field,
                     from_value, to_value, created_at_ms)
                 SELECT lower(hex(randomblob(16))), a.task_id, 'changed', a.actor, a.id,
                     'relation', a.from_value, a.to_value, a.at_ms
                 FROM planning_task_activity a
                 WHERE a.kind = 'relation' AND NOT EXISTS (
                     SELECT 1 FROM planning_notifications n WHERE n.activity_id = a.id)
                 ORDER BY a.rowid",
                [],
            )
            .expect("the mirrors stored");
    }

    /// Links the first task to the second, removes the link and makes it
    /// back from the second, all at `at`, with or without an edit written
    /// in between.
    fn linked_removed_and_linked_back(
        history: &Connection,
        made: &[String; 2],
        at: i64,
        edit_between: bool,
    ) {
        let relation = items::add_relation(
            history,
            &made[0],
            &made[1],
            WorkRelationKind::Related,
            WorkActor::Agent,
            at,
        )
        .expect("the first relates to the second");
        if edit_between {
            items::record(
                history,
                &made[1],
                at,
                WorkActor::Agent,
                WorkActivityKind::Priority,
                None,
                Some("high"),
            )
            .expect("an edit in between");
        }
        items::remove_relation(history, &relation.id, WorkActor::Agent, at)
            .expect("the link removed");
        items::add_relation(
            history,
            &made[1],
            &made[0],
            WorkRelationKind::Related,
            WorkActor::Agent,
            at,
        )
        .expect("the second relates to the first");
    }

    /// How many links and removals the inbox's rows tell, and for which tasks.
    fn told(history: &Connection, now_ms: i64) -> (usize, usize, Vec<String>) {
        let stored = inbox::notifications(history, Shown::default(), now_ms).expect("the inbox");
        let context = context(history).expect("the facts");
        let built = rows(stored, &context);
        let count = |what: &str| -> usize {
            built
                .iter()
                .flat_map(|row| &row.parts)
                .filter(|part| part.what == what)
                .map(|part| part.count)
                .sum()
        };
        let mut tasks: Vec<String> = built
            .iter()
            .filter(|row| row.parts.iter().any(|part| part.what == "linked"))
            .filter_map(|row| row.item_id.clone())
            .collect();
        tasks.sort();
        (count("linked"), count("unlinked"), tasks)
    }

    /// The oldest notice of a link, or of a removal, on one task.
    fn link_notice(history: &Connection, task: &str, links: bool) -> String {
        inbox::notifications(history, Shown::default(), START + HOUR)
            .expect("the inbox")
            .into_iter()
            .rev()
            .find(|notice| {
                notice.field.as_deref() == Some("relation")
                    && notice.item_id.as_deref() == Some(task)
                    && notice.to.is_some() == links
            })
            .expect("the notice")
            .id
    }

    #[test]
    fn hiding_a_removal_leaves_the_link_and_the_link_made_back_apart() {
        let cases = [
            (Mark::Archive, false),
            (Mark::Archive, true),
            (Mark::Snooze(Some(START + 2 * HOUR)), false),
            (Mark::Snooze(Some(START + 2 * HOUR)), true),
        ];
        for (hide, edit_between) in cases {
            let (history, made) = two_filed();
            linked_removed_and_linked_back(&history, &made, START + MINUTE, edit_between);
            let mut both = made.to_vec();
            both.sort();
            assert_eq!(told(&history, START + HOUR), (2, 1, both.clone()));
            let removal = link_notice(&history, &made[0], false);
            inbox::mark(&history, &removal, hide, START + HOUR).expect("the removal hidden");
            assert_eq!(
                told(&history, START + HOUR),
                (2, 0, both),
                "each link is still told on the task it was made from"
            );
        }
    }

    #[test]
    fn stored_mirrors_fold_into_their_own_link_whatever_is_hidden() {
        let (history, made) = two_filed();
        let at = START + MINUTE;
        linked_removed_and_linked_back(&history, &made, at, false);
        store_mirrors(&history);
        let stored = inbox::notifications(&history, Shown::default(), START + HOUR)
            .expect("the inbox")
            .into_iter()
            .filter(|notice| notice.field.as_deref() == Some("relation"))
            .count();
        assert_eq!(stored, 6, "three links or removals, stored from both ends");
        assert_eq!(context(&history).expect("the facts").echoes.len(), 3);
        let mut both = made.to_vec();
        both.sort();
        assert_eq!(told(&history, START + HOUR), (2, 1, both.clone()));

        // The removal's own notice is archived: its mirror stays, as itself.
        let removal = link_notice(&history, &made[0], false);
        inbox::mark(&history, &removal, Mark::Archive, START + HOUR).expect("archived");
        assert_eq!(told(&history, START + HOUR), (2, 1, both.clone()));

        // The first link's own notice is snoozed: its mirror on the second
        // task stays, and is not taken for the link made back from there.
        let first = link_notice(&history, &made[0], true);
        inbox::mark(
            &history,
            &first,
            Mark::Snooze(Some(START + 2 * HOUR)),
            START + HOUR,
        )
        .expect("snoozed");
        let (linked, unlinked, _) = told(&history, START + HOUR);
        assert_eq!(
            (linked, unlinked),
            (2, 1),
            "nothing told is lost or doubled"
        );
    }

    #[test]
    fn a_link_an_agent_makes_is_news_once_and_history_on_both_tasks() {
        let (history, made) = two_filed();
        let relation = items::add_relation(
            &history,
            &made[0],
            &made[1],
            WorkRelationKind::Related,
            WorkActor::Agent,
            START + MINUTE,
        )
        .expect("the link");
        let links = |history: &Connection| {
            inbox::notifications(history, Shown::default(), START + 2 * MINUTE)
                .expect("the inbox")
                .into_iter()
                .filter(|notice| notice.field.as_deref() == Some("relation"))
                .map(|notice| notice.item_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(links(&history), [Some(made[0].clone())]);
        let told: i64 = history
            .query_row(
                "SELECT count(*) FROM planning_task_activity WHERE kind = 'relation'",
                [],
                |row| row.get(0),
            )
            .expect("the history");
        assert_eq!(told, 2);

        items::remove_relation(&history, &relation.id, WorkActor::Agent, START + 3 * MINUTE)
            .expect("the link removed");
        assert_eq!(
            links(&history),
            [Some(made[0].clone()), Some(made[0].clone())]
        );

        let context = context(&history).expect("the context");
        let built = rows(
            inbox::notifications(&history, Shown::default(), START + 4 * MINUTE)
                .expect("the inbox"),
            &context,
        );
        assert_eq!(
            built.len(),
            2,
            "the two filed tasks, and the links on the first"
        );
        assert!(built
            .iter()
            .any(|row| row.summary == "Created 2 tasks for you"));
        assert!(built
            .iter()
            .any(|row| row.summary == "Linked a task, unlinked a task"));
    }
}
