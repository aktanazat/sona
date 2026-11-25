//! What a list shows besides its rows: groups and sub-groups as the app's
//! list draws them, sub-tasks under their parent, and counts by value; and
//! the saved views of the Projects page.

use super::catalog::{ExternalWorkProject, ProjectState};
use super::{
    pick, refusal, status_text, task_line, Board, ExternalWorkItem, ExternalWorkOwner,
    ExternalWorkPage, Priority, Today, Tracker, WorkAnswer, WriteArgs, READ_SCOPES, WRITE_SCOPES,
};
use crate::planning;
use crate::planning::work::views::{
    self, ProjectGroupBy, ProjectLinkFilter, ProjectListHealth, ProjectOrderBy,
    ProjectUpdateBefore, ProjectView, ProjectViewDisplay, ProjectViewFilter, ProjectViewInput,
    ProjectViewLayout,
};
use crate::planning::work::{self, types::WorkGroupBy, types::WorkItemRow, types::WorkStatus};
use crate::query::external::{ExternalError, ExternalScope};
use crate::query::QUERY_SCHEMA_VERSION;
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use tauri::Manager;

/// A field `--facets` counts tasks by.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum FacetArg {
    Status,
    Priority,
    Owner,
    Project,
    Label,
    Cycle,
    Milestone,
    Estimate,
    Delegate,
}

impl FacetArg {
    const fn key(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Priority => "priority",
            Self::Owner => "owner",
            Self::Project => "project",
            Self::Label => "label",
            Self::Cycle => "cycle",
            Self::Milestone => "milestone",
            Self::Estimate => "estimate",
            Self::Delegate => "delegate",
        }
    }
}

/// How many matching tasks hold one value: the value as `--filter` takes
/// it, its name, and the count.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkFacet {
    pub value: String,
    pub name: String,
    pub count: usize,
}

/// One group of a page: its key as the app names it (`status:done`), its
/// title, its tasks by handle, and its sub-groups.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkGroup {
    pub key: String,
    pub title: String,
    pub count: usize,
    pub tasks: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<ExternalWorkGroup>,
}

/// The value each field holds for `row`, as `(value, name)`; a label facet
/// counts a task once per label.
fn facet_values(
    field: FacetArg,
    row: &WorkItemRow,
    board: &Board,
    milestones: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let item = &row.item;
    let none = |name: &str| vec![("none".to_owned(), name.to_owned())];
    match field {
        FacetArg::Status => vec![(
            item.status.as_str().to_owned(),
            status_text(item.status).to_owned(),
        )],
        FacetArg::Priority => {
            let priority = Priority::of(item.priority);
            let word = match priority {
                Priority::None => "none",
                Priority::Urgent => "urgent",
                Priority::High => "high",
                Priority::Medium => "medium",
                Priority::Low => "low",
            };
            vec![(word.to_owned(), priority.text().to_owned())]
        }
        FacetArg::Owner if item.owner_is_self => vec![("me".to_owned(), "Me".to_owned())],
        FacetArg::Owner => item.owner_person_id.as_ref().map_or_else(
            || none("No owner"),
            |id| {
                vec![(
                    id.clone(),
                    row.owner_name
                        .clone()
                        .unwrap_or_else(|| board.person_name(id)),
                )]
            },
        ),
        FacetArg::Project => item.project_id.as_ref().map_or_else(
            || none("No project"),
            |id| vec![(id.clone(), board.project_name(id))],
        ),
        FacetArg::Label if item.label_ids.is_empty() => none("No labels"),
        FacetArg::Label => item
            .label_ids
            .iter()
            .map(|id| (id.clone(), board.label_name(id)))
            .collect(),
        FacetArg::Cycle => item.cycle.map_or_else(
            || none("No cycle"),
            |number| vec![(number.to_string(), cycle_title(board, number))],
        ),
        FacetArg::Milestone => item.milestone_id.as_ref().map_or_else(
            || none("No milestone"),
            |id| {
                vec![(
                    id.clone(),
                    milestones.get(id).cloned().unwrap_or_else(|| id.clone()),
                )]
            },
        ),
        FacetArg::Estimate => item.estimate.map_or_else(
            || none("No estimate"),
            |points| vec![(points.to_string(), format!("{points} points"))],
        ),
        FacetArg::Delegate => row.doer.as_ref().map_or_else(
            || none("No agent"),
            |doer| vec![(doer.agent.to_lowercase(), doer.agent.clone())],
        ),
    }
}

/// How many of `rows` hold each value of each field, most first; none when
/// no field is asked for.
pub(super) fn facets(
    rows: &[WorkItemRow],
    fields: &[FacetArg],
    board: &Board,
    milestones: &HashMap<String, String>,
) -> Option<BTreeMap<&'static str, Vec<ExternalWorkFacet>>> {
    if fields.is_empty() {
        return None;
    }
    let mut out = BTreeMap::new();
    for field in fields {
        let mut counts: HashMap<String, ExternalWorkFacet> = HashMap::new();
        for row in rows {
            for (value, name) in facet_values(*field, row, board, milestones) {
                counts
                    .entry(value.clone())
                    .or_insert(ExternalWorkFacet {
                        value,
                        name,
                        count: 0,
                    })
                    .count += 1;
            }
        }
        let mut counted: Vec<ExternalWorkFacet> = counts.into_values().collect();
        counted.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.name.cmp(&right.name))
        });
        out.insert(field.key(), counted);
    }
    Some(out)
}

fn cycle_title(board: &Board, number: i64) -> String {
    board
        .cycles
        .iter()
        .find(|cycle| cycle.number == number)
        .and_then(|cycle| cycle.name.as_deref())
        .map_or_else(
            || format!("Cycle {number}"),
            |name| format!("Cycle {number}: {name}"),
        )
}

/// A group before its tasks are named: where it sorts among its siblings,
/// its key and its title.
struct Slot {
    rank: (u8, String),
    key: String,
    title: String,
}

const STATUS_ORDER: [WorkStatus; 7] = [
    WorkStatus::Triage,
    WorkStatus::InProgress,
    WorkStatus::NotStarted,
    WorkStatus::Backlog,
    WorkStatus::Done,
    WorkStatus::Canceled,
    WorkStatus::Duplicate,
];

/// The groups `task` sits in under `by`: one, or one per label.
fn slots(
    by: WorkGroupBy,
    task: &ExternalWorkItem,
    board: &Board,
    milestones: &HashMap<String, String>,
    today: NaiveDate,
) -> Vec<Slot> {
    let slot = |order: u8, sort: String, key: String, title: String| Slot {
        rank: (order, sort),
        key,
        title,
    };
    match by {
        WorkGroupBy::Status => {
            let order = STATUS_ORDER
                .iter()
                .position(|status| *status == task.status)
                .unwrap_or_default();
            vec![slot(
                u8::try_from(order).unwrap_or(u8::MAX),
                String::new(),
                format!("status:{}", task.status.as_str()),
                status_text(task.status).to_owned(),
            )]
        }
        WorkGroupBy::Priority => {
            let value = task.priority.value();
            let order = if value == 0 { 5 } else { value };
            vec![slot(
                order,
                String::new(),
                format!("priority:{value}"),
                task.priority.text().to_owned(),
            )]
        }
        WorkGroupBy::Owner => vec![match &task.owner {
            ExternalWorkOwner::Myself => {
                slot(0, String::new(), "owner:self".to_owned(), "Me".to_owned())
            }
            ExternalWorkOwner::Person { person_id, name } => {
                let name = name.clone().unwrap_or_else(|| board.person_name(person_id));
                slot(1, name.to_lowercase(), format!("owner:{person_id}"), name)
            }
            ExternalWorkOwner::Nobody => slot(
                2,
                String::new(),
                "owner:nobody".to_owned(),
                "No owner".to_owned(),
            ),
        }],
        WorkGroupBy::Project => vec![task.project.as_ref().map_or_else(
            || {
                slot(
                    1,
                    String::new(),
                    "project:none".to_owned(),
                    "No project".to_owned(),
                )
            },
            |project| {
                slot(
                    0,
                    project.name.to_lowercase(),
                    format!("project:{}", project.id),
                    project.name.clone(),
                )
            },
        )],
        WorkGroupBy::Meeting => vec![task
            .source
            .as_ref()
            .and_then(|source| source.meeting_id.as_ref().map(|id| (id, source)))
            .map_or_else(
                || {
                    slot(
                        1,
                        String::new(),
                        "meeting:none".to_owned(),
                        "Not from a meeting".to_owned(),
                    )
                },
                |(id, source)| {
                    slot(
                        0,
                        id.clone(),
                        format!("meeting:{id}"),
                        source
                            .meeting_title
                            .clone()
                            .unwrap_or_else(|| "Meeting".to_owned()),
                    )
                },
            )],
        WorkGroupBy::Due => {
            let (order, key, title) = due_bucket(task.due_at_ms, today);
            vec![slot(
                order,
                String::new(),
                format!("due:{key}"),
                title.to_owned(),
            )]
        }
        WorkGroupBy::Cycle => vec![task.cycle.map_or_else(
            || {
                slot(
                    1,
                    String::new(),
                    "cycle:none".to_owned(),
                    "No cycle".to_owned(),
                )
            },
            |number| {
                slot(
                    0,
                    format!("{number:020}"),
                    format!("cycle:{number}"),
                    cycle_title(board, number),
                )
            },
        )],
        WorkGroupBy::Label if task.labels.is_empty() => vec![slot(
            1,
            String::new(),
            "label:none".to_owned(),
            "No labels".to_owned(),
        )],
        WorkGroupBy::Label => task
            .labels
            .iter()
            .map(|label| {
                slot(
                    0,
                    label.name.to_lowercase(),
                    format!("label:{}", label.id),
                    label.name.clone(),
                )
            })
            .collect(),
        WorkGroupBy::Milestone => vec![task.milestone_id.as_ref().map_or_else(
            || {
                slot(
                    1,
                    String::new(),
                    "milestone:none".to_owned(),
                    "No milestone".to_owned(),
                )
            },
            |id| {
                let title = milestones
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| "Milestone".to_owned());
                slot(0, title.to_lowercase(), format!("milestone:{id}"), title)
            },
        )],
        WorkGroupBy::None => vec![slot(0, String::new(), "all".to_owned(), "All".to_owned())],
    }
}

/// The due group a day falls in, as the app's list names them.
fn due_bucket(due_at_ms: Option<i64>, today: NaiveDate) -> (u8, &'static str, &'static str) {
    let Some(day) = due_at_ms
        .and_then(|at| Local.timestamp_millis_opt(at).single())
        .map(|at| at.date_naive())
    else {
        return (4, "no_date", "No date");
    };
    match (day - today).num_days() {
        ..0 => (0, "overdue", "Overdue"),
        0 => (1, "today", "Today"),
        1..7 => (2, "this_week", "This week"),
        _ => (3, "later", "Later"),
    }
}

/// The page's tasks under `by`, each group then split by `then`, in the
/// order the app's list shows them; a task with two labels sits in both.
pub(super) fn groups(
    tasks: &[ExternalWorkItem],
    by: WorkGroupBy,
    then: Option<WorkGroupBy>,
    board: &Board,
    milestones: &HashMap<String, String>,
    now_ms: i64,
) -> Vec<ExternalWorkGroup> {
    let today = Local
        .timestamp_millis_opt(now_ms)
        .single()
        .map_or_else(|| Local::now().date_naive(), |now| now.date_naive());
    let mut found: Vec<(Slot, Vec<&ExternalWorkItem>)> = Vec::new();
    for task in tasks {
        for slot in slots(by, task, board, milestones, today) {
            match found.iter_mut().find(|(seen, _)| seen.key == slot.key) {
                Some((_, members)) => members.push(task),
                None => found.push((slot, vec![task])),
            }
        }
    }
    found.sort_by(|(left, _), (right, _)| left.rank.cmp(&right.rank));
    if by == WorkGroupBy::Meeting {
        found.sort_by_key(|(slot, members)| {
            (
                slot.rank.0,
                std::cmp::Reverse(members.first().map_or(0, |task| task.created_at_ms)),
            )
        });
    }
    found
        .into_iter()
        .map(|(slot, members)| {
            let owned: Vec<ExternalWorkItem> = members.iter().map(|task| (*task).clone()).collect();
            ExternalWorkGroup {
                key: slot.key,
                title: slot.title,
                count: members.len(),
                tasks: members.iter().map(|task| task.handle.clone()).collect(),
                groups: then.map_or_else(Vec::new, |then| {
                    groups(&owned, then, None, board, milestones, now_ms)
                }),
            }
        })
        .collect()
}

/// Sub-tasks moved under their parent when the parent is on the page too,
/// at any depth, each keeping its place in the page's order.
pub(super) fn nest(tasks: Vec<ExternalWorkItem>) -> Vec<ExternalWorkItem> {
    let on_page: std::collections::HashSet<String> =
        tasks.iter().map(|task| task.id.clone()).collect();
    let mut children: HashMap<String, Vec<ExternalWorkItem>> = HashMap::new();
    let mut roots = Vec::new();
    for task in tasks {
        match task.parent.as_ref().map(|parent| parent.id.clone()) {
            Some(parent) if on_page.contains(&parent) => {
                children.entry(parent).or_default().push(task);
            }
            _ => roots.push(task),
        }
    }
    fn attach(
        mut task: ExternalWorkItem,
        children: &mut HashMap<String, Vec<ExternalWorkItem>>,
    ) -> ExternalWorkItem {
        task.children = children
            .remove(&task.id)
            .unwrap_or_default()
            .into_iter()
            .map(|child| attach(child, children))
            .collect();
        task
    }
    roots
        .into_iter()
        .map(|task| attach(task, &mut children))
        .collect()
}

/// The page's tasks as lines: under their groups when grouped, sub-tasks
/// under their parent when nested, then the counts asked for.
pub(super) fn page_text(text: &mut String, page: &ExternalWorkPage, today: &Today) {
    match &page.groups {
        Some(groups) => {
            let mut by_handle = HashMap::new();
            index(&page.entries.tasks, &mut by_handle);
            group_lines(text, groups, &by_handle, today, 0);
        }
        None => tree_lines(text, &page.entries.tasks, today, 0),
    }
    if let Some(points) = page.estimate_total {
        let _ = writeln!(text, "Matching estimate points: {points}");
    }
    if let Some(facets) = &page.facets {
        for (field, counts) in facets {
            let counted: Vec<String> = counts
                .iter()
                .map(|facet| format!("{} {}", facet.name, facet.count))
                .collect();
            let _ = writeln!(text, "By {field}: {}", counted.join(" · "));
        }
    }
}

fn index<'a>(
    tasks: &'a [ExternalWorkItem],
    by_handle: &mut HashMap<&'a str, &'a ExternalWorkItem>,
) {
    for task in tasks {
        by_handle.insert(task.handle.as_str(), task);
        index(&task.children, by_handle);
    }
}

fn indented(text: &mut String, depth: usize, line: impl FnOnce(&mut String)) {
    let mut one = String::new();
    line(&mut one);
    for part in one.lines() {
        let _ = writeln!(text, "{:width$}{part}", "", width = depth * 2);
    }
}

fn tree_lines(text: &mut String, tasks: &[ExternalWorkItem], today: &Today, depth: usize) {
    for task in tasks {
        indented(text, depth, |line| task_line(line, task, today));
        tree_lines(text, &task.children, today, depth + 1);
    }
}

fn group_lines(
    text: &mut String,
    groups: &[ExternalWorkGroup],
    by_handle: &HashMap<&str, &ExternalWorkItem>,
    today: &Today,
    depth: usize,
) {
    for group in groups {
        let _ = writeln!(
            text,
            "{:width$}{} ({})",
            "",
            group.title,
            group.count,
            width = depth * 2
        );
        if group.groups.is_empty() {
            for handle in &group.tasks {
                if let Some(task) = by_handle.get(handle.as_str()) {
                    indented(text, depth + 1, |line| task_line(line, task, today));
                }
            }
        } else {
            group_lines(text, &group.groups, by_handle, today, depth + 1);
        }
    }
}

impl Tracker<'_> {
    /// Every milestone's title by id.
    pub(super) fn milestone_titles(&self) -> Result<HashMap<String, String>, ExternalError> {
        planning::history(self.history, |conn| {
            let mut statement = conn.prepare("SELECT id, title FROM planning_milestones")?;
            let titles = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<HashMap<String, String>>>()?;
            Ok(titles)
        })
        .map_err(|message| refusal(self.history, message))
    }
}

/// The saved views of the Projects page.
#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum ProjectViewVerb {
    /// Saved project views, with their filters, layout and grouping.
    ProjectViews,
    /// Save a project view: named project filters the Projects page shows
    /// as a tab and `projects --view` reads.
    ProjectViewSave(ProjectViewSaveArgs),
    /// Delete a saved project view. No project changes.
    ProjectViewDelete(ProjectViewDeleteArgs),
}

impl ProjectViewVerb {
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self {
            Self::ProjectViews
            | Self::ProjectViewSave(ProjectViewSaveArgs { dry_run: true, .. })
            | Self::ProjectViewDelete(ProjectViewDeleteArgs { dry_run: true, .. }) => READ_SCOPES,
            Self::ProjectViewSave(_) | Self::ProjectViewDelete(_) => WRITE_SCOPES,
        }
    }
}

/// A status a project view keeps, or any.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ProjectStatusArg {
    Any,
    Backlog,
    Planned,
    #[value(alias = "started")]
    Active,
    Paused,
    Completed,
    Canceled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ProjectLinkArg {
    Any,
    Blocked,
    Blocking,
    Broken,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ProjectLayoutArg {
    List,
    Board,
    Timeline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ProjectGroupArg {
    Status,
    Lead,
    Priority,
    Quarter,
    Health,
    Initiative,
    Label,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ProjectHealthArg {
    Any,
    OnTrack,
    AtRisk,
    OffTrack,
    Outdated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ProjectOrderArg {
    Manual,
    Status,
    Priority,
    Name,
    Start,
    Target,
    Created,
    Updated,
}

impl ProjectHealthArg {
    pub(super) fn health(self) -> Option<ProjectListHealth> {
        match self {
            Self::Any => None,
            Self::OnTrack => Some(ProjectListHealth::OnTrack),
            Self::AtRisk => Some(ProjectListHealth::AtRisk),
            Self::OffTrack => Some(ProjectListHealth::OffTrack),
            Self::Outdated => Some(ProjectListHealth::Outdated),
        }
    }
}

impl ProjectGroupArg {
    pub(super) fn group(self) -> ProjectGroupBy {
        match self {
            Self::Status => ProjectGroupBy::Status,
            Self::Lead => ProjectGroupBy::Lead,
            Self::Priority => ProjectGroupBy::Priority,
            Self::Quarter => ProjectGroupBy::Quarter,
            Self::Health => ProjectGroupBy::Health,
            Self::Initiative => ProjectGroupBy::Initiative,
            Self::Label => ProjectGroupBy::Label,
            Self::None => ProjectGroupBy::None,
        }
    }
}

impl ProjectOrderArg {
    pub(super) fn order(self) -> ProjectOrderBy {
        match self {
            Self::Manual => ProjectOrderBy::Manual,
            Self::Status => ProjectOrderBy::Status,
            Self::Priority => ProjectOrderBy::Priority,
            Self::Name => ProjectOrderBy::Name,
            Self::Start => ProjectOrderBy::Start,
            Self::Target => ProjectOrderBy::Target,
            Self::Created => ProjectOrderBy::Created,
            Self::Updated => ProjectOrderBy::Updated,
        }
    }
}

/// How long ago a latest update must be: 1w, 2w, 1m, a day as
/// YYYY-MM-DD, or any.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateBeforeArg(Option<ProjectUpdateBefore>);

fn update_before(value: &str) -> Result<UpdateBeforeArg, String> {
    let value = value.trim();
    Ok(UpdateBeforeArg(match value {
        "any" => None,
        "1w" => Some(ProjectUpdateBefore::OneWeek),
        "2w" => Some(ProjectUpdateBefore::TwoWeeks),
        "1m" => Some(ProjectUpdateBefore::OneMonth),
        day => Some(ProjectUpdateBefore::Day {
            day: NaiveDate::parse_from_str(day, "%Y-%m-%d")
                .map_err(|_| "expected 1w, 2w, 1m, YYYY-MM-DD or any".to_owned())?
                .format("%Y-%m-%d")
                .to_string(),
        }),
    }))
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectViewSaveArgs {
    /// The view's name. Saving a name that exists changes that view.
    pub name: String,
    /// The project view to rename to NAME, by name or id.
    #[arg(long, value_name = "VIEW")]
    pub view: Option<String>,
    /// An SF Symbol shown beside its name; an empty value clears it.
    #[arg(long, value_name = "SYMBOL")]
    pub icon: Option<String>,
    /// Only projects in this status; any for every status.
    #[arg(long, value_enum)]
    pub status: Option<ProjectStatusArg>,
    /// Only projects with an open task past its due day: true or false.
    #[arg(long, value_name = "BOOL")]
    pub overdue: Option<bool>,
    /// Only projects with open work you own: true or false.
    #[arg(long, value_name = "BOOL")]
    pub mine: Option<bool>,
    /// Only projects whose latest update is older than 1w, 2w or 1m, or
    /// from before a day (YYYY-MM-DD), and those with none; any for all.
    #[arg(long, value_parser = update_before, value_name = "WHEN")]
    pub update_before: Option<UpdateBeforeArg>,
    /// Only projects that wait on another (blocked), that another waits on
    /// (blocking), or with a broken dependency (broken); any for all.
    #[arg(long, value_enum)]
    pub link: Option<ProjectLinkArg>,
    /// Only the projects in this initiative, by name or id; none for those
    /// in no initiative, any for all.
    #[arg(long, value_name = "INITIATIVE")]
    pub initiative: Option<String>,
    /// Only projects with this health; outdated for those with no current
    /// update; any for all.
    #[arg(long, value_enum)]
    pub health: Option<ProjectHealthArg>,
    /// Only projects with this label, by name or id; any for all.
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,
    #[arg(long, value_enum)]
    pub layout: Option<ProjectLayoutArg>,
    /// status, lead, priority, quarter (of the target day), health,
    /// initiative, label or none.
    #[arg(long, value_enum)]
    pub group_by: Option<ProjectGroupArg>,
    /// The order the projects list in: manual (drag order) by default.
    #[arg(long, value_enum)]
    pub order_by: Option<ProjectOrderArg>,
    /// Descending instead of ascending: true or false.
    #[arg(long, value_name = "BOOL")]
    pub reverse: Option<bool>,
    /// Show the view as it would be saved; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ProjectViewDeleteArgs {
    /// The view's name or id.
    pub view: String,
    /// Show the view that would be deleted; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectView {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub filter: ProjectViewFilter,
    pub layout: ProjectViewLayout,
    pub group_by: ProjectGroupBy,
    pub order_by: Option<ProjectOrderBy>,
    pub reverse: bool,
    pub revision: i64,
    /// Opens the view on the Projects page: `sona://projects?view=<id>`.
    pub link: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectViews {
    pub schema_version: u32,
    pub views: Vec<ExternalProjectView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectViewChange {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: String,
    pub deleted: bool,
    pub view: ExternalProjectView,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ExternalProjectViewAnswer {
    Views(ExternalProjectViews),
    Change(ExternalProjectViewChange),
}

fn external(view: &ProjectView) -> ExternalProjectView {
    ExternalProjectView {
        id: view.id.clone(),
        name: view.name.clone(),
        icon: view.icon.clone(),
        filter: view.filter.clone(),
        layout: view.display.layout,
        group_by: view.display.group_by,
        order_by: view.display.order_by,
        reverse: view.display.reverse,
        revision: view.revision,
        link: format!("sona://projects?view={}", view.id),
    }
}

/// One group of projects, by id, as `projects --view` groups them.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectGroup {
    pub key: String,
    pub title: String,
    pub projects: Vec<String>,
}

impl Tracker<'_> {
    pub(super) fn every_project_view(&self) -> Result<Vec<ProjectView>, ExternalError> {
        work::work_project_views(self.app.state()).map_err(|message| refusal(self.history, message))
    }

    pub(super) fn project_view_named(&self, value: &str) -> Result<ProjectView, ExternalError> {
        let saved = self.every_project_view()?;
        pick(
            "project view",
            value,
            &saved,
            |view| &view.id,
            |view| &view.name,
        )
        .cloned()
        .map_err(|refusal| refusal.hint("List them with: sona work project-views"))
    }

    /// The label id a project filter keeps; any keeps every label.
    pub(super) fn label_filter(&self, value: &str) -> Result<Option<String>, ExternalError> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("any") {
            return Ok(None);
        }
        let all = self.all_labels()?;
        Ok(Some(
            pick("label", value, &all, |item| &item.id, |item| &item.name)?
                .id
                .clone(),
        ))
    }

    pub(super) async fn project_view_verb(
        &self,
        verb: &ProjectViewVerb,
    ) -> Result<WorkAnswer, ExternalError> {
        let answer = match verb {
            ProjectViewVerb::ProjectViews => {
                ExternalProjectViewAnswer::Views(ExternalProjectViews {
                    schema_version: QUERY_SCHEMA_VERSION,
                    views: self.every_project_view()?.iter().map(external).collect(),
                })
            }
            ProjectViewVerb::ProjectViewSave(args) => self.project_view_save(args)?,
            ProjectViewVerb::ProjectViewDelete(args) => self.project_view_delete(args)?,
        };
        Ok(WorkAnswer::ProjectViews(Box::new(answer)))
    }

    fn project_view_save(
        &self,
        args: &ProjectViewSaveArgs,
    ) -> Result<ExternalProjectViewAnswer, ExternalError> {
        let saved = self.every_project_view()?;
        let name = args.name.trim();
        let existing = match &args.view {
            Some(value) => Some(
                pick(
                    "project view",
                    value,
                    &saved,
                    |view| &view.id,
                    |view| &view.name,
                )
                .map_err(|refusal| refusal.hint("List them with: sona work project-views"))?,
            ),
            None => saved
                .iter()
                .find(|view| view.name.to_lowercase() == name.to_lowercase()),
        };
        let mut filter =
            existing.map_or_else(ProjectViewFilter::default, |view| view.filter.clone());
        let mut display = existing.map_or_else(ProjectViewDisplay::default, |view| view.display);
        if let Some(status) = args.status {
            filter.status = match status {
                ProjectStatusArg::Any => None,
                ProjectStatusArg::Backlog => Some(ProjectState::Backlog.into()),
                ProjectStatusArg::Planned => Some(ProjectState::Planned.into()),
                ProjectStatusArg::Active => Some(ProjectState::Active.into()),
                ProjectStatusArg::Paused => Some(ProjectState::Paused.into()),
                ProjectStatusArg::Completed => Some(ProjectState::Completed.into()),
                ProjectStatusArg::Canceled => Some(ProjectState::Canceled.into()),
            };
        }
        if let Some(overdue) = args.overdue {
            filter.overdue_only = overdue;
        }
        if let Some(mine) = args.mine {
            filter.mine_only = mine;
        }
        if let Some(UpdateBeforeArg(before)) = &args.update_before {
            filter.update_before.clone_from(before);
        }
        if let Some(link) = args.link {
            filter.link = match link {
                ProjectLinkArg::Any => None,
                ProjectLinkArg::Blocked => Some(ProjectLinkFilter::Blocked),
                ProjectLinkArg::Blocking => Some(ProjectLinkFilter::Blocking),
                ProjectLinkArg::Broken => Some(ProjectLinkFilter::Broken),
            };
        }
        if let Some(initiative) = args.initiative.as_deref().map(str::trim) {
            filter.initiative = if initiative.eq_ignore_ascii_case("any") {
                None
            } else if initiative.eq_ignore_ascii_case("none") {
                Some("none".to_owned())
            } else {
                let all = self.all_initiatives()?;
                Some(Self::pick_initiative(initiative, &all)?.id.clone())
            };
        }
        if let Some(health) = args.health {
            filter.health = health.health();
        }
        if let Some(label) = args.label.as_deref() {
            filter.label = self.label_filter(label)?;
        }
        if let Some(layout) = args.layout {
            display.layout = match layout {
                ProjectLayoutArg::List => ProjectViewLayout::List,
                ProjectLayoutArg::Board => ProjectViewLayout::Board,
                ProjectLayoutArg::Timeline => ProjectViewLayout::Timeline,
            };
        }
        if let Some(group) = args.group_by {
            display.group_by = group.group();
        }
        if let Some(order) = args.order_by {
            display.order_by = Some(order.order());
        }
        if let Some(reverse) = args.reverse {
            display.reverse = reverse;
        }
        let input = ProjectViewInput {
            name: name.to_owned(),
            icon: match &args.icon {
                Some(icon) => Some(icon.clone()).filter(|icon| !icon.trim().is_empty()),
                None => existing.and_then(|view| view.icon.clone()),
            },
            filter,
            display,
        };
        let (id, operation_id) = match existing {
            Some(view) => (
                Some(view.id.clone()),
                args.write
                    .operation_id("project-view-save", &(&view.id, view.revision, &input))?,
            ),
            None => (None, args.write.fresh_operation_id()),
        };
        let view = if args.dry_run {
            self.rehearse(|conn| {
                views::save_project_view(conn, id.as_deref(), &input, super::now_ms())
            })?
        } else {
            work::work_project_view_save(self.app.clone(), operation_id.clone(), id, input)
                .map_err(|message| self.write_refusal(message, &operation_id, "The project view"))?
        };
        Ok(ExternalProjectViewAnswer::Change(
            ExternalProjectViewChange {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: args.dry_run,
                operation_id,
                deleted: false,
                view: external(&view),
            },
        ))
    }

    fn project_view_delete(
        &self,
        args: &ProjectViewDeleteArgs,
    ) -> Result<ExternalProjectViewAnswer, ExternalError> {
        let view = self.project_view_named(&args.view)?;
        let operation_id = args.write.operation_id("project-view-delete", &view.id)?;
        if args.dry_run {
            self.rehearse(|conn| views::delete_project_view(conn, &view.id))?;
        } else {
            work::work_project_view_delete(self.app.clone(), operation_id.clone(), view.id.clone())
                .map_err(|message| {
                    self.write_refusal(message, &operation_id, "The project view")
                })?;
        }
        Ok(ExternalProjectViewAnswer::Change(
            ExternalProjectViewChange {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: args.dry_run,
                operation_id,
                deleted: true,
                view: external(&view),
            },
        ))
    }
}

/// Whether a project row stays under a project view's filter: `open` holds
/// every task row, for the overdue and mine filters.
pub(super) fn project_keeps(
    filter: &ProjectViewFilter,
    project: &ExternalWorkProject,
    tasks: &[WorkItemRow],
    links: &dyn Fn(ProjectLinkFilter, &str) -> bool,
    now_ms: i64,
) -> bool {
    let today = Local
        .timestamp_millis_opt(now_ms)
        .single()
        .map_or_else(|| Local::now().date_naive(), |now| now.date_naive());
    let open = || {
        tasks.iter().filter(|row| {
            row.item.project_id.as_deref() == Some(project.id.as_str()) && row.item.status.is_open()
        })
    };
    if filter.status.is_some_and(|status| status != project.status) {
        return false;
    }
    if filter.overdue_only
        && !open().any(|row| {
            row.item
                .due_at_ms
                .and_then(|at| Local.timestamp_millis_opt(at).single())
                .is_some_and(|due| due.date_naive() < today)
        })
    {
        return false;
    }
    if filter.mine_only && !open().any(|row| row.item.owner_is_self) {
        return false;
    }
    if let Some(before) = &filter.update_before {
        let cutoff = match before {
            ProjectUpdateBefore::OneWeek => today.checked_sub_days(chrono::Days::new(7)),
            ProjectUpdateBefore::TwoWeeks => today.checked_sub_days(chrono::Days::new(14)),
            ProjectUpdateBefore::OneMonth => today.checked_sub_months(chrono::Months::new(1)),
            ProjectUpdateBefore::Day { day } => NaiveDate::parse_from_str(day, "%Y-%m-%d").ok(),
        }
        .and_then(|day| planning::work::items::local_midnight_ms(day).ok());
        if let (Some(cutoff), Some(latest)) = (cutoff, project.latest_update_at_ms) {
            if latest >= cutoff {
                return false;
            }
        }
    }
    if filter.link.is_some_and(|link| !links(link, &project.id)) {
        return false;
    }
    if filter.health.is_some_and(|health| health != project.health) {
        return false;
    }
    if filter
        .label
        .as_ref()
        .is_some_and(|label| !project.label_ids.contains(label))
    {
        return false;
    }
    match filter.initiative.as_deref() {
        None => true,
        Some("none") => project.initiative.is_none(),
        Some(id) => project
            .initiative
            .as_ref()
            .is_some_and(|initiative| initiative.id == id),
    }
}

/// Projects in the groups `by` makes, in the order the Projects page shows
/// them; none when `by` is none. A project with several labels is in each.
pub(super) fn project_groups(
    projects: &[ExternalWorkProject],
    by: ProjectGroupBy,
) -> Option<Vec<ExternalProjectGroup>> {
    if matches!(by, ProjectGroupBy::None) {
        return None;
    }
    let mut found: Vec<((u8, String), ExternalProjectGroup)> = Vec::new();
    for project in projects {
        for (rank, key, title) in group_keys(project, by) {
            match found.iter_mut().find(|(_, group)| group.key == key) {
                Some((_, group)) => group.projects.push(project.id.clone()),
                None => found.push((
                    rank,
                    ExternalProjectGroup {
                        key,
                        title,
                        projects: vec![project.id.clone()],
                    },
                )),
            }
        }
    }
    found.sort_by(|(left, _), (right, _)| left.cmp(right));
    Some(found.into_iter().map(|(_, group)| group).collect())
}

/// The groups one project is in, as (rank, key, title).
fn group_keys(
    project: &ExternalWorkProject,
    by: ProjectGroupBy,
) -> Vec<((u8, String), String, String)> {
    match by {
        ProjectGroupBy::None => Vec::new(),
        ProjectGroupBy::Status => {
            let word = serde_json::to_value(project.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            let mut title = word.clone();
            if let Some(first) = title.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            vec![(
                (status_rank(project.status), String::new()),
                format!("status:{word}"),
                title,
            )]
        }
        ProjectGroupBy::Lead => vec![match (&project.lead, project.lead_is_self) {
            (_, true) => ((0, String::new()), "lead:self".to_owned(), "Me".to_owned()),
            (Some(lead), false) => (
                (1, lead.name.to_lowercase()),
                format!("lead:{}", lead.id),
                lead.name.clone(),
            ),
            (None, false) => (
                (2, String::new()),
                "lead:none".to_owned(),
                "No lead".to_owned(),
            ),
        }],
        ProjectGroupBy::Priority => {
            let value = project.priority.value();
            vec![(
                (priority_rank(&project.priority), String::new()),
                format!("priority:{value}"),
                project.priority.text().to_owned(),
            )]
        }
        ProjectGroupBy::Quarter => vec![match project
            .target_at_ms
            .and_then(|at| Local.timestamp_millis_opt(at).single())
        {
            Some(target) => {
                let quarter = (target.month() - 1) / 3 + 1;
                let year = target.year();
                (
                    (0, format!("{year:04}{quarter}")),
                    format!("quarter:{year}-q{quarter}"),
                    format!("Q{quarter} {year}"),
                )
            }
            None => (
                (1, String::new()),
                "quarter:none".to_owned(),
                "No target".to_owned(),
            ),
        }],
        ProjectGroupBy::Health => {
            let (rank, word, title) = match project.health {
                ProjectListHealth::OnTrack => (0, "on_track", "On track"),
                ProjectListHealth::AtRisk => (1, "at_risk", "At risk"),
                ProjectListHealth::OffTrack => (2, "off_track", "Off track"),
                ProjectListHealth::Outdated => (3, "outdated", "Outdated"),
            };
            vec![(
                (rank, String::new()),
                format!("health:{word}"),
                title.to_owned(),
            )]
        }
        ProjectGroupBy::Initiative => vec![match &project.initiative {
            Some(initiative) => (
                (
                    initiative_rank(project.initiative_status),
                    initiative.name.to_lowercase(),
                ),
                format!("initiative:{}", initiative.id),
                initiative.name.clone(),
            ),
            None => (
                (u8::MAX, String::new()),
                "initiative:none".to_owned(),
                "No initiative".to_owned(),
            ),
        }],
        ProjectGroupBy::Label => project
            .labels
            .iter()
            .map(|label| {
                (
                    (0, label.to_lowercase()),
                    format!("label:{}", label.to_lowercase()),
                    label.clone(),
                )
            })
            .collect(),
    }
}

fn status_rank(status: planning::ProjectStatus) -> u8 {
    let order = [
        planning::ProjectStatus::Backlog,
        planning::ProjectStatus::Planned,
        planning::ProjectStatus::Active,
        planning::ProjectStatus::Paused,
        planning::ProjectStatus::Completed,
        planning::ProjectStatus::Canceled,
    ]
    .iter()
    .position(|candidate| *candidate == status)
    .unwrap_or_default();
    u8::try_from(order).unwrap_or(u8::MAX)
}

fn priority_rank(priority: &Priority) -> u8 {
    let value = priority.value();
    if value == 0 {
        5
    } else {
        value
    }
}

fn initiative_rank(status: Option<planning::work::initiatives::InitiativeStatus>) -> u8 {
    use planning::work::initiatives::InitiativeStatus as Status;
    match status {
        Some(Status::Active) => 0,
        Some(Status::Planned) => 1,
        Some(Status::Paused) => 2,
        Some(Status::Backlog) => 3,
        Some(Status::Completed) => 4,
        Some(Status::Canceled) => 5,
        None => 6,
    }
}

/// Sorts projects by `by`, descending when `reverse` is set. Equal projects
/// keep the order they came in.
pub(super) fn project_order(
    projects: &mut [ExternalWorkProject],
    by: ProjectOrderBy,
    reverse: bool,
) {
    projects.sort_by(|left, right| {
        let order = match by {
            ProjectOrderBy::Manual => left.position.cmp(&right.position),
            ProjectOrderBy::Status => status_rank(left.status).cmp(&status_rank(right.status)),
            ProjectOrderBy::Priority => {
                priority_rank(&left.priority).cmp(&priority_rank(&right.priority))
            }
            ProjectOrderBy::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
            ProjectOrderBy::Start => left
                .start_at_ms
                .unwrap_or(i64::MAX)
                .cmp(&right.start_at_ms.unwrap_or(i64::MAX)),
            ProjectOrderBy::Target => left
                .target_at_ms
                .unwrap_or(i64::MAX)
                .cmp(&right.target_at_ms.unwrap_or(i64::MAX)),
            ProjectOrderBy::Created => left.created_at_ms.cmp(&right.created_at_ms),
            ProjectOrderBy::Updated => left.updated_at_ms.cmp(&right.updated_at_ms),
        };
        if reverse {
            order.reverse()
        } else {
            order
        }
    });
}

pub(super) fn text(answer: &ExternalProjectViewAnswer, text: &mut String) {
    match answer {
        ExternalProjectViewAnswer::Views(views) => {
            if views.views.is_empty() {
                text.push_str("No saved project views.\n");
            }
            for view in &views.views {
                let _ = writeln!(
                    text,
                    "{}  · {}\n  {}",
                    view.name,
                    filter_text(view),
                    view.link
                );
            }
        }
        ExternalProjectViewAnswer::Change(change) => {
            if change.dry_run {
                text.push_str("Dry run. ");
            }
            let _ = writeln!(
                text,
                "{} project view {}  · {}",
                if change.deleted { "Deleted" } else { "Saved" },
                change.view.name,
                filter_text(&change.view)
            );
        }
    }
}

fn filter_text(view: &ExternalProjectView) -> String {
    let filter = &view.filter;
    let mut parts = Vec::new();
    if let Some(status) = filter.status {
        parts.push(format!("{status:?}").to_lowercase());
    }
    if filter.overdue_only {
        parts.push("with overdue tasks".to_owned());
    }
    if filter.mine_only {
        parts.push("with my work".to_owned());
    }
    match &filter.update_before {
        Some(ProjectUpdateBefore::OneWeek) => parts.push("no update for a week".to_owned()),
        Some(ProjectUpdateBefore::TwoWeeks) => parts.push("no update for two weeks".to_owned()),
        Some(ProjectUpdateBefore::OneMonth) => parts.push("no update for a month".to_owned()),
        Some(ProjectUpdateBefore::Day { day }) => parts.push(format!("no update since {day}")),
        None => {}
    }
    if let Some(link) = filter.link {
        parts.push(format!("{link:?}").to_lowercase());
    }
    match filter.initiative.as_deref() {
        Some("none") => parts.push("in no initiative".to_owned()),
        Some(_) => parts.push("in one initiative".to_owned()),
        None => {}
    }
    let shown = if parts.is_empty() {
        "every project".to_owned()
    } else {
        parts.join(" · ")
    };
    format!(
        "{shown} · {} by {}",
        format!("{:?}", view.layout).to_lowercase(),
        format!("{:?}", view.group_by).to_lowercase()
    )
}
