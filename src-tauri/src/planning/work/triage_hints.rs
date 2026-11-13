use super::super::store;
use super::items;
use super::sync::{self, same_words, MeetingReader};
use super::types::{
    WorkActor, WorkFieldSuggestion, WorkItem, WorkOwner, WorkPatch, WorkRelationKind, WorkStatus,
    WorkSuggestionField, WorkSuggestionValue,
};
use super::{now_ms, run_history, with_meetings, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::meeting::loop_types::MeetingLoopId;
use crate::meeting::session::MeetingSessionManager;
use anyhow::Result;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

pub(crate) const DISMISSED_HINTS_MIGRATION: &str = "CREATE TABLE planning_dismissed_hints (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    dismissed_at_ms INTEGER NOT NULL
);
CREATE INDEX planning_dismissed_hints_task ON planning_dismissed_hints(task_id);
CREATE TRIGGER planning_dismissed_hints_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_dismissed_hints WHERE task_id = old.id;
END;";

pub(crate) const GONE: &str = "This suggestion no longer applies. Reload and try again.";

const NAMESPACE: Uuid = Uuid::from_u128(0x6f3c_9d2e_51a4_4b87_9e0d_c4a1_7b58_2f63);
const RECENT: usize = 5;
const ENOUGH: usize = 2;
const RELATED: usize = 2;
const COMMON: [&str; 40] = [
    "the", "and", "for", "with", "from", "about", "that", "this", "will", "into", "our", "your",
    "their", "then", "than", "have", "has", "can", "are", "was", "were", "been", "also", "just",
    "some", "more", "its", "his", "her", "they", "them", "who", "what", "when", "how", "not",
    "but", "all", "any", "out",
];

pub(crate) fn id(task_id: &str, field: WorkSuggestionField, value: &WorkSuggestionValue) -> String {
    let field = match field {
        WorkSuggestionField::Owner => "owner",
        WorkSuggestionField::Due => "due",
        WorkSuggestionField::Project => "project",
        WorkSuggestionField::Label => "label",
        WorkSuggestionField::Duplicate => "duplicate",
        WorkSuggestionField::Related => "related",
    };
    let value = match value {
        WorkSuggestionValue::Owner(WorkOwner::Myself) => "self".to_owned(),
        WorkSuggestionValue::Owner(WorkOwner::Person { person_id }) => person_id.clone(),
        WorkSuggestionValue::Owner(WorkOwner::Nobody) => "nobody".to_owned(),
        WorkSuggestionValue::Due(at_ms) => at_ms.to_string(),
        WorkSuggestionValue::Id(id) => id.clone(),
    };
    Uuid::new_v5(&NAMESPACE, format!("{task_id}/{field}/{value}").as_bytes()).to_string()
}

fn hint(
    item: &WorkItem,
    field: WorkSuggestionField,
    value: WorkSuggestionValue,
    reason: String,
    drawn_from: Vec<String>,
) -> WorkFieldSuggestion {
    WorkFieldSuggestion {
        id: id(&item.id, field, &value),
        field,
        value,
        reason,
        quote: None,
        at_ms: None,
        meeting_id: None,
        drawn_from,
    }
}

fn words(title: &str) -> HashSet<String> {
    title
        .split(|letter: char| !letter.is_alphanumeric())
        .filter(|word| word.chars().count() > 2)
        .map(str::to_lowercase)
        .filter(|word| !COMMON.contains(&word.as_str()))
        .collect()
}

fn alike(one: &HashSet<String>, other: &HashSet<String>) -> bool {
    let shared = one.intersection(other).count();
    shared >= ENOUGH && 2 * shared >= one.len() + other.len() - shared
}

struct Past {
    id: String,
    number: i64,
    title: String,
    words: HashSet<String>,
    status: WorkStatus,
    project_id: Option<String>,
    owner: WorkOwner,
    label_ids: Vec<String>,
}

impl Past {
    const fn decided(&self) -> bool {
        !matches!(
            self.status,
            WorkStatus::Triage | WorkStatus::Canceled | WorkStatus::Duplicate
        )
    }
}

struct Window<'a> {
    tasks: Vec<&'a Past>,
    which: String,
}

impl Window<'_> {
    fn share(&self, count: usize) -> String {
        if count == self.tasks.len() {
            format!("The {}", self.which)
        } else {
            format!("{count} of the {}", self.which)
        }
    }

    fn passes(&self, count: usize) -> bool {
        count >= ENOUGH && 2 * count > self.tasks.len()
    }

    fn top<'a>(&'a self, pick: impl Fn(&'a Past) -> Option<&'a str>) -> Option<(&'a str, usize)> {
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for task in &self.tasks {
            let Some(value) = pick(task) else { continue };
            match counts.iter_mut().find(|(seen, _)| *seen == value) {
                Some((_, count)) => *count += 1,
                None => counts.push((value, 1)),
            }
        }
        counts
            .into_iter()
            .find(|(_, count)| self.passes(*count))
    }
}

pub(crate) struct Pool {
    tasks: Vec<Past>,
    meetings: HashMap<String, Vec<String>>,
    people: HashMap<String, Vec<String>>,
    met: HashMap<String, HashSet<String>>,
    names: HashMap<String, String>,
    projects: HashMap<String, String>,
    labels: HashMap<String, String>,
    dismissed: HashSet<String>,
}

impl Pool {
    fn read(history: &Connection, meetings: Option<&Connection>) -> Result<Self> {
        let mut all = items::items(history, false)?;
        all.sort_by(|one, other| {
            other
                .created_at_ms
                .cmp(&one.created_at_ms)
                .then_with(|| other.number.cmp(&one.number))
        });
        let tasks = all
            .into_iter()
            .map(|item| Past {
                words: words(&item.title),
                owner: item.owner(),
                id: item.id,
                number: item.number,
                title: item.title,
                status: item.status,
                project_id: item.project_id,
                label_ids: item.label_ids,
            })
            .collect();
        let (people, met, names) = match meetings {
            Some(meetings) => people(meetings)?,
            None => (HashMap::new(), HashMap::new(), HashMap::new()),
        };
        let mut statement = history.prepare(
            "SELECT id, title FROM planning_projects
             WHERE status != 'completed' AND archived_at_ms IS NULL",
        )?;
        let projects = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let labels = items::labels(history)?
            .into_iter()
            .filter(|label| label.retired_at_ms.is_none())
            .map(|label| (label.id, label.name))
            .collect();
        let mut statement = history.prepare("SELECT id FROM planning_dismissed_hints")?;
        let dismissed = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Self {
            tasks,
            meetings: task_meetings(history)?,
            people,
            met,
            names,
            projects,
            labels,
            dismissed,
        })
    }

    fn meetings_of(&self, task_id: &str) -> &[String] {
        self.meetings.get(task_id).map_or(&[], Vec::as_slice)
    }

    fn decided<'a>(&'a self, item: &'a WorkItem) -> impl Iterator<Item = &'a Past> + 'a {
        self.tasks
            .iter()
            .filter(move |task| task.id != item.id && task.decided())
    }

    fn drawn(&self, window: &Window<'_>, mine: &[String]) -> Vec<String> {
        let mut drawn: Vec<String> = mine.to_vec();
        for task in &window.tasks {
            for meeting in self.meetings_of(&task.id) {
                if !drawn.contains(meeting) {
                    drawn.push(meeting.clone());
                }
            }
        }
        drawn
    }

    fn project(&self, window: &Window<'_>) -> Option<(String, String, usize)> {
        let (project_id, count) = window.top(|task| {
            task.project_id
                .as_deref()
                .filter(|id| self.projects.contains_key(*id))
        })?;
        let title = self.projects.get(project_id)?;
        Some((
            project_id.to_owned(),
            format!("{} went to {title}", window.share(count)),
            count,
        ))
    }

    fn people_windows<'a>(&'a self, item: &'a WorkItem, mine: &[String]) -> Vec<Window<'a>> {
        let mut seen: Vec<&str> = Vec::new();
        for meeting in mine {
            for person in self.people.get(meeting).into_iter().flatten() {
                if !seen.contains(&person.as_str()) {
                    seen.push(person.as_str());
                }
            }
        }
        let mut windows = Vec::new();
        for person in seen {
            let (Some(theirs), Some(name)) = (self.met.get(person), self.names.get(person)) else {
                continue;
            };
            let tasks: Vec<&Past> = self
                .decided(item)
                .filter(|task| {
                    let meetings = self.meetings_of(&task.id);
                    !meetings.iter().any(|meeting| mine.contains(meeting))
                        && meetings.iter().any(|meeting| theirs.contains(meeting))
                })
                .take(RECENT)
                .collect();
            windows.push(Window {
                which: format!("last {} tasks from meetings with {name}", tasks.len()),
                tasks,
            });
        }
        windows
    }

    fn hints(
        &self,
        item: &WorkItem,
        offered: &[WorkFieldSuggestion],
        linked: &HashSet<String>,
    ) -> Vec<WorkFieldSuggestion> {
        let mine = self.meetings_of(&item.id);
        let title = words(&item.title);
        let siblings = {
            let tasks: Vec<&Past> = if mine.is_empty() {
                Vec::new()
            } else {
                self.decided(item)
                    .filter(|task| {
                        self.meetings_of(&task.id)
                            .iter()
                            .any(|meeting| mine.contains(meeting))
                    })
                    .take(RECENT)
                    .collect()
            };
            Window {
                which: format!("{} other tasks from this meeting", tasks.len()),
                tasks,
            }
        };
        let similar = {
            let tasks: Vec<&Past> = self
                .decided(item)
                .filter(|task| alike(&title, &task.words))
                .take(RECENT)
                .collect();
            Window {
                which: format!("last {} tasks with similar titles", tasks.len()),
                tasks,
            }
        };
        let has = |field| offered.iter().any(|suggestion: &WorkFieldSuggestion| suggestion.field == field);
        let mut found = Vec::new();
        if item.project_id.is_none() && !has(WorkSuggestionField::Project) {
            let people = self.people_windows(item, mine);
            let by_people = people
                .iter()
                .filter_map(|window| self.project(window).map(|found| (found, window)))
                .max_by_key(|((_, _, count), window)| (*count, usize::MAX - window.tasks.len()));
            let chosen = self
                .project(&siblings)
                .map(|found| (found, &siblings))
                .or(by_people)
                .or_else(|| self.project(&similar).map(|found| (found, &similar)));
            if let Some(((project_id, reason, _), window)) = chosen {
                found.push(hint(
                    item,
                    WorkSuggestionField::Project,
                    WorkSuggestionValue::Id(project_id),
                    reason,
                    self.drawn(window, mine),
                ));
            }
        }
        if item.owner() == WorkOwner::Nobody && !has(WorkSuggestionField::Owner) {
            let top = similar.top(|task| match &task.owner {
                WorkOwner::Myself => Some("self"),
                WorkOwner::Person { person_id } => Some(person_id.as_str()),
                WorkOwner::Nobody => None,
            });
            let owner = top.and_then(|(owner, count)| match owner {
                "self" => Some((
                    WorkOwner::Myself,
                    format!("{} were yours", similar.share(count)),
                )),
                person_id => self.names.get(person_id).map(|name| {
                    (
                        WorkOwner::Person {
                            person_id: person_id.to_owned(),
                        },
                        format!("{} went to {name}", similar.share(count)),
                    )
                }),
            });
            if let Some((owner, reason)) = owner {
                found.push(hint(
                    item,
                    WorkSuggestionField::Owner,
                    WorkSuggestionValue::Owner(owner),
                    reason,
                    self.drawn(&similar, mine),
                ));
            }
        }
        for window in [&siblings, &similar] {
            let mut counted: Vec<(&str, usize)> = Vec::new();
            for task in &window.tasks {
                for label in task.label_ids.iter().map(String::as_str) {
                    if item.label_ids.iter().any(|own| own == label) || !self.labels.contains_key(label) {
                        continue;
                    }
                    match counted.iter_mut().find(|(seen, _)| *seen == label) {
                        Some((_, count)) => *count += 1,
                        None => counted.push((label, 1)),
                    }
                }
            }
            for (label, count) in counted {
                let value = WorkSuggestionValue::Id(label.to_owned());
                let taken = found.iter().any(|suggestion: &WorkFieldSuggestion| {
                    suggestion.field == WorkSuggestionField::Label && suggestion.value == value
                });
                let Some(name) = self.labels.get(label) else { continue };
                if taken || !window.passes(count) {
                    continue;
                }
                found.push(hint(
                    item,
                    WorkSuggestionField::Label,
                    value,
                    format!("{} were labeled {name}", window.share(count)),
                    self.drawn(window, mine),
                ));
            }
        }
        let same = same_words(&item.title);
        let mut related: Vec<(&Past, usize, usize)> = self
            .tasks
            .iter()
            .filter(|task| {
                task.id != item.id
                    && !task.status.is_closed()
                    && !linked.contains(&task.id)
                    && item.duplicate_of_id.as_deref() != Some(task.id.as_str())
                    && same_words(&task.title) != same
                    && alike(&title, &task.words)
            })
            .map(|task| {
                let shared = title.intersection(&task.words).count();
                (task, shared, title.len() + task.words.len() - shared)
            })
            .collect();
        related.sort_by(|(_, shared, union), (_, other_shared, other_union)| {
            (other_shared * union).cmp(&(shared * other_union))
        });
        for (task, _, _) in related.into_iter().take(RELATED) {
            found.push(hint(
                item,
                WorkSuggestionField::Related,
                WorkSuggestionValue::Id(task.id.clone()),
                format!("#{} has a similar title: {}", task.number, task.title),
                self.meetings_of(&task.id).to_vec(),
            ));
        }
        found
    }
}

fn task_meetings(history: &Connection) -> Result<HashMap<String, Vec<String>>> {
    let mut statement = history.prepare(
        "SELECT id, 'loop', source_loop_id FROM planning_tasks WHERE source_loop_id IS NOT NULL
         UNION ALL
         SELECT target_id, source_kind, source_id FROM planning_evidence_links
         WHERE target_kind = 'task' AND source_kind IN ('meeting', 'loop')",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut meetings: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let (task, kind, source) = row?;
        let meeting = if kind == "loop" {
            MeetingLoopId(source)
                .session_id()
                .map(|session| session.uuid().to_string())
        } else {
            Some(source)
        };
        let Some(meeting) = meeting else { continue };
        let known = meetings.entry(task).or_default();
        if !known.contains(&meeting) {
            known.push(meeting);
        }
    }
    Ok(meetings)
}

type People = (
    HashMap<String, Vec<String>>,
    HashMap<String, HashSet<String>>,
    HashMap<String, String>,
);

fn people(meetings: &Connection) -> Result<People> {
    let mut statement = meetings.prepare(
        "SELECT meeting_id, person_id FROM meeting_person_links
         WHERE confidence = 'confirmed' ORDER BY meeting_id, person_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut people: HashMap<String, Vec<String>> = HashMap::new();
    let mut met: HashMap<String, HashSet<String>> = HashMap::new();
    for row in rows {
        let (meeting, person) = row?;
        met.entry(person.clone()).or_default().insert(meeting.clone());
        people.entry(meeting).or_default().push(person);
    }
    let mut statement = meetings.prepare("SELECT id, display_name FROM persons")?;
    let names = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok((people, met, names))
}

pub(crate) fn extend(
    history: &Connection,
    reader: &mut MeetingReader<'_>,
    item: &WorkItem,
    suggestions: &mut Vec<WorkFieldSuggestion>,
) -> Result<()> {
    if reader.hints.is_none() {
        let pool = Pool::read(history, reader.conn())?;
        reader.hints = Some(pool);
    }
    let Some(pool) = reader.hints.as_ref() else {
        return Ok(());
    };
    let linked: HashSet<String> = items::relations(history, &item.id)?
        .into_iter()
        .map(|relation| {
            if relation.from_id == item.id {
                relation.to_id
            } else {
                relation.from_id
            }
        })
        .collect();
    let found = pool.hints(item, suggestions, &linked);
    suggestions.extend(found);
    for suggestion in suggestions.iter_mut() {
        if let (WorkSuggestionField::Duplicate, WorkSuggestionValue::Id(other)) =
            (suggestion.field, &suggestion.value)
        {
            suggestion
                .drawn_from
                .extend(pool.meetings_of(other).iter().cloned());
        }
    }
    suggestions.retain(|suggestion| !pool.dismissed.contains(&suggestion.id));
    Ok(())
}

impl WorkFieldSuggestion {
    pub(crate) fn draws_on(&self, hides: impl Fn(&str) -> bool) -> bool {
        self.meeting_id.as_deref().is_some_and(&hides)
            || self.drawn_from.iter().any(|meeting| hides(meeting))
    }
}

pub(crate) enum Taking {
    Patch(WorkPatch),
    Duplicate(String),
    Related(String),
}

pub(crate) fn taking(item: &WorkItem, suggestion: &WorkFieldSuggestion) -> Option<Taking> {
    let patch = match (suggestion.field, &suggestion.value) {
        (WorkSuggestionField::Owner, WorkSuggestionValue::Owner(owner)) => WorkPatch {
            owner: Some(owner.clone()),
            ..WorkPatch::default()
        },
        (WorkSuggestionField::Due, WorkSuggestionValue::Due(at_ms)) => WorkPatch {
            due_at_ms: Some(Some(*at_ms)),
            ..WorkPatch::default()
        },
        (WorkSuggestionField::Project, WorkSuggestionValue::Id(id)) => WorkPatch {
            project_id: Some(Some(id.clone())),
            ..WorkPatch::default()
        },
        (WorkSuggestionField::Label, WorkSuggestionValue::Id(id)) => {
            let mut label_ids = item.label_ids.clone();
            if !label_ids.contains(id) {
                label_ids.push(id.clone());
            }
            WorkPatch {
                label_ids: Some(label_ids),
                ..WorkPatch::default()
            }
        }
        (WorkSuggestionField::Duplicate, WorkSuggestionValue::Id(id)) => {
            return Some(Taking::Duplicate(id.clone()))
        }
        (WorkSuggestionField::Related, WorkSuggestionValue::Id(id)) => {
            return Some(Taking::Related(id.clone()))
        }
        _ => return None,
    };
    Some(Taking::Patch(patch))
}

async fn offered(
    app: &AppHandle,
    task_id: &str,
    suggestion_id: &str,
) -> Result<(WorkItem, WorkFieldSuggestion), String> {
    let history = app.state::<Arc<HistoryManager>>();
    let meetings = app.state::<Arc<MeetingSessionManager>>();
    let user_name = meetings.user_name();
    let found = with_meetings(&history, &meetings, |history, meeting_conn| {
        let item = items::item(history, task_id)?;
        let all = items::items(history, false)?;
        let mut reader = MeetingReader::new(meeting_conn, user_name.as_deref());
        let suggestion = sync::suggestions(history, &mut reader, &all, &item)?
            .into_iter()
            .find(|suggestion| suggestion.id == suggestion_id);
        Ok(suggestion.map(|suggestion| (item, suggestion)))
    })
    .await?;
    found.ok_or_else(|| GONE.to_owned())
}

pub(crate) async fn accept(
    app: &AppHandle,
    operation_id: &str,
    task_id: &str,
    suggestion_id: &str,
) -> Result<WorkItem, String> {
    let (item, suggestion) = offered(app, task_id, suggestion_id).await?;
    match taking(&item, &suggestion) {
        Some(Taking::Patch(patch)) => {
            super::update_as(app, WorkActor::Me, operation_id, &item.id, item.revision, &patch)
                .await
        }
        Some(Taking::Duplicate(of_id)) => {
            super::mark_duplicate_as(app, WorkActor::Me, operation_id, &item.id, &of_id).await
        }
        Some(Taking::Related(to_id)) => {
            super::relation_add_as(
                app,
                WorkActor::Me,
                operation_id,
                &item.id,
                &to_id,
                WorkRelationKind::Related,
            )?;
            Ok(item)
        }
        None => Err(GONE.to_owned()),
    }
}

pub(crate) async fn dismiss(
    app: &AppHandle,
    operation_id: &str,
    task_id: &str,
    suggestion_id: &str,
) -> Result<WorkFieldSuggestion, String> {
    let (item, suggestion) = offered(app, task_id, suggestion_id).await?;
    let at_ms = now_ms();
    run_history(app, |history| {
        store::mutate(
            history,
            operation_id,
            "work_suggestion_dismiss",
            &(&item.id, &suggestion.id),
            at_ms,
            |tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO planning_dismissed_hints (id, task_id, dismissed_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![suggestion.id, item.id, at_ms],
                )?;
                let mut effects = Effects::default();
                effects.item(&item);
                Ok(Done {
                    value: (),
                    effects,
                })
            },
        )
    })?;
    Ok(suggestion)
}
