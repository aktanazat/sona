//! `sona work`: the task tracker for agents and scripts.
//!
//! A subcommand rather than more corpus flags. Tracker verbs take a dozen
//! modifiers that would collide with the corpus verbs' own (`--status`,
//! `--project`, `--mine`), and a subcommand binds each modifier to its verb by
//! construction. Text is the default; `--json` prints one value.
//!
//! Every write goes through the tracker's own functions, filed under
//! [`WorkActor::Agent`], and needs both grants, as `--loop-resolve` does. Each
//! carries an operation id: the caller's `--operation-id`, or one derived from
//! the request and the revisions it read, so a retry of a change that landed
//! answers with the first result or finds nothing left to change.
//!
//! Work raised in a meeting kept local is left out of every answer and counted
//! nowhere, as that meeting's loops are. While the conversations can't be
//! opened nothing says which meetings those are, so all work from meetings is
//! left out and the answer says so.

use super::{
    day_start_utc_ms, evidence_kept_local, kept_local, local_day, local_time, project_evidence,
    store, ExternalError, ExternalErrorCode, ExternalPlanningEvidence, ExternalScope,
    DEFAULT_LIMIT, MAX_LIMIT, READ_SCOPES, WRITE_SCOPES,
};
use crate::managers::history::HistoryManager;
use crate::meeting::loop_types::MeetingLoopId;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::MeetingSessionId;
use crate::planning;
use crate::planning::work::agents::{self, AgentChange};
use crate::planning::work::ask::WorkAnswerLine;
use crate::planning::work::capture::{self, WorkCaptureSource, WorkTakenLine};
use crate::planning::work::command::WorkCommandChange;
use crate::planning::work::filter::{WorkCondition, WorkFilterField, WorkFilterOp};
use crate::planning::work::types::{
    WorkActivity, WorkActivityKind, WorkActor, WorkCycle, WorkCycleRef, WorkCycleState, WorkCycles,
    WorkDirection, WorkDisplay, WorkDue, WorkFieldSuggestion, WorkGroupBy, WorkItem,
    WorkItemDetail, WorkItemInput, WorkItemRow, WorkLabel, WorkLayout, WorkList, WorkOrderBy,
    WorkOwner, WorkPatch, WorkPersonChoice, WorkPlace, WorkProgress, WorkProjectChoice, WorkProposal,
    WorkProposalField,
    WorkQuery, WorkRelation, WorkRelationKind, WorkRepeat, WorkSource, WorkStatus,
    WorkSuggestionField, WorkSuggestionValue, WorkVia, WorkView,
};
use crate::planning::work::watch::WorkWatch;
use crate::planning::work::{self, items, parse};
use crate::query::{loop_link, task_link, KeptLocal, QueryAudience, QUERY_SCHEMA_VERSION};
use crate::settings::WorkAutonomy;
use chrono::{Local, NaiveDate, TimeDelta, Timelike};
use clap::{Args, Subcommand, ValueEnum};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::{self, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

/// Names derived operation ids, so the same request always derives the same
/// id and no other request in Sona can.
const OPERATION_NAMESPACE: Uuid = Uuid::from_u128(0x5f0e_2c71_a8d4_4b39_9e06_d13a_7c52_e8b4);

/// Open work untouched this long is stale, as the morning plan counts it.
const STALE_AFTER_MS: i64 = 30 * 24 * 3_600_000;

/// The properties the app's own new views show.
const VIEW_PROPERTIES: [&str; 6] = ["id", "priority", "labels", "project", "owner", "due"];

const OPEN_AND_TRIAGE: [WorkStatus; 4] = [
    WorkStatus::Triage,
    WorkStatus::Backlog,
    WorkStatus::NotStarted,
    WorkStatus::InProgress,
];

/// `sona work <verb>`, and how to print the answer.
#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct WorkCommand {
    #[command(subcommand)]
    pub verb: WorkVerb,
    /// Print one JSON value instead of text.
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum WorkVerb {
    /// List tasks, most urgent first. Without --status: triage and open work.
    List(ListArgs),
    /// Todo tasks nothing open blocks, not snoozed: what to pick up next.
    Ready(ReadyArgs),
    /// The counts, what you owe, and what others owe you.
    Mine(MineArgs),
    /// Triage, your open work, overdue, due today, and waiting on, as numbers.
    Counts,
    /// Cycles: how long they run, and each one's scope and done.
    Cycles(CyclesArgs),
    /// One cycle's scope and done day by day: current, next, or its number.
    Cycle(CycleArgs),
    /// Lead time (made to done) and triage time (until accepted or
    /// declined) over the last --days, as medians and 90th percentiles.
    Insights(insights::InsightsArgs),
    /// One task with its quote, links, evidence, suggestions and history.
    Show(ShowArgs),
    /// Create a task: Todo, no priority and no owner unless told otherwise.
    Create(CreateArgs),
    /// Change tasks. Several tasks take the same change together or not at all.
    Update(UpdateArgs),
    /// Start a Todo or Backlog task: it moves to In progress with the agent on it, the owner unchanged.
    Claim(ClaimArgs),
    /// Mark tasks done. A meeting commitment behind one closes with it.
    Done(DoneArgs),
    /// Accept triage items into Todo with Sona's suggested owner, due day and project.
    Accept(AcceptArgs),
    /// Decline triage items: canceled, and their meeting commitments set
    /// aside. --reason is kept in each task's history.
    Decline(routines::DeclineArgs),
    /// Mark a task a duplicate of another; its quotes and links move there.
    Duplicate(DuplicateArgs),
    /// Hide tasks from every list. Their status stays.
    Archive(TasksArgs),
    /// Bring archived tasks back, or deleted ones out of the bin.
    Restore(TasksArgs),
    /// Move tasks in the manual order lists show under --order-by manual:
    /// to the top, to the bottom, or before or after another task.
    Move(MoveArgs),
    /// Move tasks to the bin: hidden everywhere, restorable for 30 days, then
    /// deleted for good.
    Delete(TasksArgs),
    /// Deleted tasks still in the bin, with the days left before each is
    /// purged.
    Bin,
    /// The archive: archived tasks and deleted ones still in the bin.
    ArchiveList,
    /// A task's earlier descriptions, newest first.
    DescriptionVersions(lifecycle::VersionsArgs),
    /// One earlier description in full.
    DescriptionVersion(lifecycle::VersionArgs),
    /// Put an earlier description back. It waits for the user unless
    /// agents' changes apply.
    DescriptionRestore(lifecycle::VersionRestoreArgs),
    /// Add a task, a project (--project), a saved view (--view) or a person
    /// (--person) to favorites, last at the top.
    Favorite(lifecycle::FavoriteArgs),
    /// Take a task, project, view or person out of favorites.
    Unfavorite(lifecycle::FavoriteArgs),
    /// The user's favorites in the sidebar's order, folders holding theirs.
    Favorites,
    /// Make a favorites folder (--name), or rename or move one (--folder).
    FavoriteFolderSave(lifecycle::FavoriteFolderSaveArgs),
    /// Delete a favorites folder. Its favorites stay, where it was.
    FavoriteFolderDelete(lifecycle::FavoriteFolderDeleteArgs),
    /// Move a favorite into a folder (--into), out to the top (--top), or
    /// to another place (--position).
    FavoriteMove(lifecycle::FavoriteMoveArgs),
    /// Subscribe the user to tasks: what others change on them reaches the
    /// inbox.
    Subscribe(TasksArgs),
    /// Unsubscribe the user from tasks.
    Unsubscribe(TasksArgs),
    /// The tasks the user is subscribed to.
    Subscriptions,
    /// The inbox: status changes, comments, mentions and assignments others
    /// made on subscribed tasks, with the unread count.
    Inbox(lifecycle::InboxArgs),
    /// Mark notifications read or unread, archive them, or snooze them.
    InboxMark(lifecycle::InboxMarkArgs),
    /// The weekly project digest over the last seven days: each project's
    /// status, latest update, tasks done and risks, posted to the inbox.
    Digest(digest::DigestArgs),
    /// Hide tasks from lists and ready until a day; none wakes them.
    Snooze(SnoozeArgs),
    /// Link two tasks: S-1 blocks S-2, S-1 blocked-by S-2, or S-1 related S-2.
    Relate(RelateArgs),
    /// Remove a link between two tasks.
    Unrelate(RelateArgs),
    /// List labels.
    Labels,
    /// Create a label, or rename or recolor one.
    LabelSave(LabelSaveArgs),
    /// Delete a label. Its tasks lose only that label.
    LabelDelete(LabelDeleteArgs),
    /// Put a label in a group, or none to take it out of one. A task keeps
    /// one label per group: adding another from it replaces the first.
    LabelGroup(routines::LabelGroupArgs),
    /// Retire a label: its tasks keep it, pickers leave it out. --restore
    /// brings it back.
    LabelRetire(routines::LabelRetireArgs),
    /// Set what a label may go on: all, tasks or projects.
    LabelScope(routines::LabelScopeArgs),
    /// List task templates.
    Templates,
    /// Save a task template: its fields and sub-tasks. Changes the one
    /// with that name, or --template.
    TemplateSave(routines::TemplateSaveArgs),
    /// Delete a task template. Tasks made from it stay.
    TemplateDelete(routines::TemplateDeleteArgs),
    /// Make a task and its sub-tasks from a template.
    TemplateUse(routines::TemplateUseArgs),
    /// List automations: saved prompts Sona's agent runs on a schedule or
    /// when work changes.
    #[command(name = "automation-list")]
    LoopList,
    /// Show an automation with its versions and its runs.
    #[command(name = "automation-show")]
    LoopShow(agent_loops::LoopShowArgs),
    /// Save an automation: its prompt, trigger, conditions and tools.
    /// Changes the one with that name, or --automation.
    #[command(name = "automation-save")]
    LoopSave(agent_loops::LoopSaveArgs),
    /// Queue a run of an automation now; the running app starts it.
    #[command(name = "automation-run")]
    LoopRun(agent_loops::LoopArgs),
    /// Turn an automation on.
    #[command(name = "automation-enable")]
    LoopEnable(agent_loops::LoopArgs),
    /// Turn an automation off. Its versions and runs stay.
    #[command(name = "automation-disable")]
    LoopDisable(agent_loops::LoopArgs),
    /// Bring back an earlier version of an automation, as a new version.
    #[command(name = "automation-restore-version")]
    LoopRestoreVersion(agent_loops::LoopRestoreArgs),
    /// Delete an automation with its versions and runs.
    #[command(name = "automation-delete")]
    LoopDelete(agent_loops::LoopArgs),
    /// Set when Sona reminds you of a task, or none to clear it.
    Remind(routines::RemindArgs),
    /// List reminders; --due lists those whose time has come.
    Reminders(routines::RemindersArgs),
    /// List saved views.
    Views,
    /// Save a view: named filters that the app and `list --view` use.
    ViewSave(ViewSaveArgs),
    /// Delete a saved view.
    ViewDelete(ViewDeleteArgs),
    /// Write matching tasks to a folder as Markdown, one file per task.
    Export(ExportArgs),
    /// Who owes what from one meeting, by when, in their words: the
    /// follow-up to send. Nothing is sent.
    FollowUp(FollowUpArgs),
    /// Ask every conversation a question; the lines that answer it, numbered.
    /// --take makes Triage tasks of those lines, each with its quote and where it was said.
    Ask(AskArgs),
    /// Change tasks by saying it: "push everything not urgent to monday",
    /// "move Dana's tasks to next week", "mark these done".
    Say(SayArgs),
    /// Watched topics, each with the last times it came up.
    Watches,
    /// Watch a topic: Sona notifies once when a processed meeting or a dictation says it.
    Watch(WatchArgs),
    /// Stop watching a topic.
    Unwatch(UnwatchArgs),
    /// Projects with their lead, status, target day and task counts.
    Projects(catalog::ProjectsArgs),
    /// Make a project, or change one: name, details, status, lead, members, target day, colour.
    ProjectSave(catalog::ProjectSaveArgs),
    /// Put a project away, or bring it back with --restore.
    ProjectArchive(catalog::ProjectArchiveArgs),
    /// A project's milestones, with target days and task counts.
    Milestones(catalog::MilestonesArgs),
    /// Projects on a time axis from start to target day, with the
    /// milestones that fall in the days shown.
    Timeline(timeline::TimelineArgs),
    /// Make a milestone, or change one.
    MilestoneSave(catalog::MilestoneSaveArgs),
    /// Delete a milestone. Its tasks stay in the project.
    MilestoneDelete(catalog::MilestoneDeleteArgs),
    /// One milestone, with its tasks.
    Milestone(catalog::MilestoneArgs),
    /// Turn a milestone into its own project. Its tasks move with it.
    MilestoneToProject(catalog::MilestoneToProjectArgs),
    /// Make a project wait on another: `PROJECT blocked-by OTHER` or
    /// `PROJECT blocking OTHER`. The blocker should end before the other starts.
    ProjectRelate(dependencies::ProjectRelateArgs),
    /// Take a project dependency off, named the way it was made.
    ProjectUnrelate(dependencies::ProjectRelateArgs),
    /// Projects that wait on other projects, with the broken ones marked.
    ProjectDependencies(dependencies::ProjectDependenciesArgs),
    /// Initiatives, goals above projects, with their projects' progress.
    InitiativeList,
    /// One initiative, with its projects.
    InitiativeShow(initiatives::InitiativeArgs),
    /// Make an initiative.
    InitiativeNew(initiatives::InitiativeNewArgs),
    /// Change an initiative's name, details, owner, target day or status.
    InitiativeUpdate(initiatives::InitiativeUpdateArgs),
    /// Delete an initiative. Its projects stay, in no initiative.
    InitiativeDelete(initiatives::InitiativeDeleteArgs),
    /// Put a project in an initiative. A project sits in one initiative at
    /// most, so this moves it out of any other.
    InitiativeAddProject(initiatives::InitiativeProjectArgs),
    /// Take a project out of its initiative.
    InitiativeRemoveProject(initiatives::InitiativeProjectArgs),
    /// Project templates, with their milestones and tasks.
    ProjectTemplateList,
    /// Save a project as a template: its description, lead, members,
    /// milestones as days after its start, and tasks with their labels and
    /// estimates.
    ProjectTemplateSave(project_templates::ProjectTemplateSaveArgs),
    /// Delete a project template. Projects made from it stay.
    ProjectTemplateDelete(project_templates::ProjectTemplateDeleteArgs),
    /// Make a project from a template, starting on a day.
    ProjectNew(project_templates::ProjectNewArgs),
    /// Custom project statuses, each a kind of a built-in status.
    ProjectStatuses,
    /// Make a custom project status, or rename or move one.
    ProjectStatusSave(catalog::ProjectStatusSaveArgs),
    /// Delete a custom project status. Its projects keep their built-in status.
    ProjectStatusDelete(catalog::ProjectStatusDeleteArgs),
    /// A project's changes, newest first: its name, status, lead, members,
    /// dates, priority and milestones.
    ProjectHistory(catalog::ProjectHistoryArgs),
    /// People, with the other names they go by and their open work.
    People(catalog::PeopleArgs),
    /// Add a person, or change one: other names, role, organization, email, notes.
    PersonSave(catalog::PersonSaveArgs),
    /// Fold one person into another: their tasks, quotes and speaker links move.
    PersonMerge(catalog::PersonMergeArgs),
    /// One person's page in one read: summary, meetings, open items, documents, contact.
    Person(people::PersonArgs),
    /// Link a person to a meeting, confirm a link Sona suggested, or take them off it.
    PersonLink(people::PersonLinkArgs),
    /// Move some of a person's meetings, other names, emails or documents to another person.
    PersonSplit(people::PersonSplitArgs),
    /// Delete a person, with their meeting links and saved voice.
    PersonDelete(people::PersonDeleteArgs),
    /// Set a person's photo from a PNG or JPEG, or take it off.
    PersonPhoto(people::PersonPhotoArgs),
    /// Take the contact card off a person.
    PersonContactRemove(people::PersonContactRemoveArgs),
    /// When you first met a person, in which meeting, and who introduced you.
    FirstMet(people::PersonArgs),
    /// Answer who introduced you to a person: yes takes the introducer Sona heard, no turns it down.
    FirstMetAnswer(people::FirstMetArgs),
    /// Companies, from the organizations people carry.
    Companies(people::CompaniesArgs),
    /// One company: its people, meetings and open items.
    Company(people::CompanyArgs),
    /// Meetings, newest first, with when, how long, whether processed, and who was in them.
    Meetings(catalog::MeetingsArgs),
    /// One meeting: summary, commitments, and with --transcript who said what.
    Meeting(catalog::MeetingArgs),
    /// Comment on a task: add, edit, delete, resolve.
    Comment(threads::CommentCommand),
    /// A task's comment threads, oldest first.
    Comments(threads::CommentsArgs),
    /// Change a comment you wrote.
    CommentEdit(threads::CommentEditArgs),
    /// Delete a comment you wrote. Replies under it stay.
    CommentDelete(threads::CommentDeleteArgs),
    /// Resolve the thread a comment is in, or reopen it with --reopen.
    CommentResolve(threads::CommentResolveArgs),
    /// Make sub-tasks under a task, one per title. Each takes the parent's
    /// project and priority unless told otherwise.
    Subtask(threads::SubtaskArgs),
    /// Attach a file (copied into Sona) or a link to a task. The same file
    /// or link on the same task lands once.
    Attach(attachments::AttachArgs),
    /// A task's files, images and links, oldest first.
    Attachments(attachments::AttachmentsArgs),
    /// Take an attachment off a task; a file's copy is deleted.
    Detach(attachments::DetachArgs),
    /// One project: its fields, latest update, whether an update is due,
    /// progress by week, a predicted finish, and its milestones' progress.
    Project(roadmap::ProjectArgs),
    /// Post an update on a project with its health; without --body Sona
    /// drafts it from what moved since the last one.
    ProjectUpdate(roadmap::ProjectUpdateArgs),
    /// A project's updates, newest first.
    ProjectUpdates(roadmap::ProjectUpdatesArgs),
    /// Change an update you posted: its health or body.
    ProjectUpdateEdit(roadmap::ProjectUpdateEditArgs),
    /// Delete an update you posted.
    ProjectUpdateDelete(roadmap::ProjectUpdateDeleteArgs),
    /// A plain draft of what moved since the project's last update. Nothing is posted.
    ProjectUpdateDraft(roadmap::ProjectArgs),
    /// Put a project in the bin for 30 days, or take it out with --restore.
    ProjectDelete(roadmap::ProjectDeleteArgs),
    /// Put a project's milestones in order: name every one, first to last.
    MilestoneReorder(roadmap::MilestoneReorderArgs),
    /// Change how cycles run: length, start day, cooldown, how many ahead.
    CycleSettings(roadmap::CycleSettingsArgs),
    /// Rename a cycle, or move the days of one not yet over.
    CycleSave(roadmap::CycleSaveArgs),
    /// Move a cycle's unfinished work into the cycle under way, or the next.
    CycleRollover(roadmap::CycleRolloverArgs),
    /// Start the next cycle today; the one under way ends and its unfinished work moves in.
    CycleStartToday(roadmap::CycleStartTodayArgs),
    /// Hand a task to a named agent, or take it back with --take-back.
    Delegate(organize::DelegateArgs),
    /// A day's focus: up to three tasks, in order.
    Day(organize::DayArgs),
    /// Pick a day's focus: up to three tasks, in order, replacing the old ones.
    Focus(organize::FocusArgs),
    /// Keep month-old tasks as they are: they leave the stale list.
    Keep(TasksArgs),
    /// One screen before talking to someone: what you owe them, what they
    /// owe you, talking points, and what was decided last time.
    Brief(organize::BriefArgs),
    /// Add something to raise the next time you talk to someone.
    TalkingPoint(organize::TalkingPointArgs),
    /// Mark a talking point done, or not done with --undo.
    TalkingPointDone(organize::TalkingPointDoneArgs),
    /// Reopen a meeting loop that was resolved or set aside.
    LoopReopen(organize::LoopArgs),
    /// Set who owns a meeting loop, or none.
    LoopOwner(organize::LoopOwnerArgs),
    /// Say who a meeting's speaker is: a person, a new person, or unknown
    /// again with --undo. The voice itself is never remembered: saving a
    /// voice profile stays a step the user takes in the app.
    SpeakerAssign(speakers::SpeakerAssignArgs),
    /// What voice matching suggests for a meeting's speakers, where else it
    /// heard each unnamed voice, and the names it gave by itself.
    SpeakerVoices(voices::SpeakerVoicesArgs),
    /// Take voice matching's suggestion for a waiting speaker.
    SpeakerConfirm(voices::SpeakerVoiceArgs),
    /// Give a speaker voice matching named by itself back the name it had.
    SpeakerVoiceUndo(voices::SpeakerVoiceArgs),
    /// Track an action a meeting left with no task as a task in a project.
    ActionTrack(organize::ActionTrackArgs),
    /// Set an untracked meeting action aside as not work.
    ActionDismiss(organize::LoopArgs),
    /// Ask about an action set aside as not work again.
    ActionRestore(organize::LoopArgs),
    /// Make a note.
    NoteNew(notes::NoteNewArgs),
    /// Replace a note's body. The old body stays as a version.
    NoteEdit(notes::NoteEditArgs),
    /// Pin a note, or unpin it with --unpin.
    NotePin(notes::NotePinArgs),
    /// Link a meeting, a person or another note to a note.
    NoteLink(notes::NoteLinkArgs),
    /// Take a linked meeting, person or note off a note.
    NoteUnlink(notes::NoteLinkArgs),
    /// Delete a note and its versions.
    NoteDelete(notes::NoteDeleteArgs),
    /// Put one of a note's saved versions back as its body.
    NoteRestore(notes::NoteRestoreArgs),
    /// The subjects notes gather under.
    Subjects,
    /// Make a note subject.
    SubjectNew(subjects::SubjectNewArgs),
    /// Add a note to a subject, make it the current one, or take it off.
    SubjectNote(subjects::SubjectNoteArgs),
    /// Read typed or spoken words as task fields, writing nothing.
    Parse(planner::ParseArgs),
    /// Book the next free working hour for a task in the "Sona" calendar.
    Book(planner::BookArgs),
    /// Take a task's booked hour off the calendar.
    Unbook(planner::BookArgs),
    /// Mark a task waiting on someone as asked about today.
    Nudged(planner::BookArgs),
    /// Make a Triage task from a line a watch heard.
    WatchTask(planner::WatchTaskArgs),
    /// The links Sona suggests between meetings, notes and work.
    LinkSuggestions(planner::LinkSuggestionsArgs),
    /// Accept a suggested link.
    LinkAccept(planner::LinkDecideArgs),
    /// Dismiss a suggested link.
    LinkDismiss(planner::LinkDecideArgs),
    /// What Sona suggests for tasks in Triage: project, labels, owner and similar tasks, each with its reason.
    TriageSuggestions(triage_hints::TriageSuggestionsArgs),
    /// Accept a Triage suggestion; it changes the task like any other update.
    SuggestionAccept(triage_hints::SuggestionDecideArgs),
    /// Dismiss a Triage suggestion; it stays dismissed.
    SuggestionDismiss(triage_hints::SuggestionDecideArgs),
    /// Tasks, milestones and meetings by day, as the planner calendar shows them.
    Calendar(planner::CalendarArgs),
    /// Dictation and meeting totals by day.
    Trend(planner::TrendArgs),
    /// Home's Today strip: meetings today, what is processing, speakers
    /// nobody named, promises not tracked, and what the user owes.
    Today,
    /// Make a project, its milestones and tasks from a Plan with Sona draft.
    PlanAccept(planner::PlanAcceptArgs),
    /// Rename a task's attachment.
    AttachmentRename(organize::AttachmentRenameArgs),
    /// Change one task with an ordered list of field changes, all or none.
    Save(save::SaveArgs),
    /// What changed on tasks, projects, milestones, cycles, views, labels,
    /// project updates, notes, meetings, comments and agent sessions since a
    /// cursor, oldest first, each with the fields' earlier values. Anything
    /// tied to a meeting kept on this Mac is left out.
    Changes(save::ChangesArgs),
    /// Start an agent session on a task: the agent's run, which the user
    /// watches and can stop. The agent becomes the task's delegate.
    SessionStart(sessions::SessionStartArgs),
    /// Write to a session you run: a thought, an action, a question for the
    /// user, or the response or error that ends it.
    SessionActivity(sessions::SessionActivityArgs),
    /// Replace a session's plan: its checklist of steps.
    SessionPlan(sessions::SessionPlanArgs),
    /// Tell Sona the session's agent is still working. The answer says
    /// whether the user asked it to stop.
    SessionHeartbeat(sessions::SessionWriteArgs),
    /// Ask the agent running a session to stop.
    SessionStop(sessions::SessionWriteArgs),
    /// Answer the agent running a session: text, or a choice its question offered.
    SessionReply(sessions::SessionReplyArgs),
    #[command(flatten)]
    Settings(preferences::SettingsVerb),
    #[command(flatten)]
    Sources(sources::SourcesVerb),
    #[command(flatten)]
    Library(library::LibraryVerb),
    #[command(flatten)]
    Transcript(transcript::TranscriptVerb),
    #[command(flatten)]
    Dictations(dictations::DictationVerb),
    /// One session: state, plan, question, and every activity.
    Session(sessions::SessionShowArgs),
    /// Agent sessions, newest first: one task's or every task's.
    Sessions(sessions::SessionsArgs),
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CyclesArgs {
    /// Print every cycle as an iCalendar file instead, for a calendar app.
    #[arg(long)]
    pub ics: bool,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CycleArgs {
    /// current, next, or a cycle's number.
    #[arg(value_parser = cycle_ref, default_value = "current", value_name = "CYCLE")]
    pub cycle: WorkCycleRef,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ListArgs {
    #[command(flatten)]
    pub filters: Filters,
    /// Start from a saved view's filters (name or id); other flags narrow it.
    #[arg(long)]
    pub view: Option<String>,
    #[command(flatten)]
    pub page: PageArgs,
    /// Only tasks that came from these: meeting, dictation, agent, person.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "SOURCE")]
    pub source: Vec<SourceArg>,
    /// Only open work nobody has touched for 30 days.
    #[arg(long)]
    pub stale: bool,
    /// Only tasks delegated to this agent; any for every delegated task,
    /// none for tasks no agent works.
    #[arg(long, value_name = "AGENT")]
    pub delegate: Option<String>,
    /// Filter in plain words: "high priority bugs due this week that are
    /// mine". Prints what the words were read as and the words it did not
    /// understand.
    #[arg(long = "where", value_name = "WORDS")]
    pub where_words: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum SourceArg {
    Meeting,
    Dictation,
    Agent,
    Person,
}

impl SourceArg {
    const fn source(self) -> WorkSource {
        match self {
            Self::Meeting => WorkSource::Meeting,
            Self::Dictation => WorkSource::Dictation,
            Self::Agent => WorkSource::Agent,
            Self::Person => WorkSource::Person,
        }
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ReadyArgs {
    #[command(flatten)]
    pub scope: Scope,
    #[command(flatten)]
    pub page: PageArgs,
}

/// How a task list is ordered, paged and trimmed.
#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PageArgs {
    /// Rows to print, at most 100.
    #[arg(long, value_parser = limit)]
    pub limit: Option<usize>,
    /// priority (the default: priority, then the soonest due), due (soonest
    /// first, then priority), updated or created (newest first), or manual
    /// (the order tasks were dragged into).
    #[arg(long, value_enum)]
    pub order_by: Option<OrderArg>,
    /// The next_cursor of the page before, to read the rows after it. Keep
    /// the same filters; the cursor keeps its order.
    #[arg(long, value_parser = cursor, value_name = "CURSOR")]
    pub cursor: Option<Cursor>,
    /// Only these fields of each task, comma-separated; id and handle always
    /// come. JSON only.
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        value_name = "FIELD",
        requires = "json"
    )]
    pub fields: Vec<TaskField>,
}

impl PageArgs {
    /// The order rows are read in: the cursor's, which --order-by must match.
    fn order(&self) -> Result<OrderArg, ExternalError> {
        match (self.order_by, self.cursor) {
            (Some(order), Some(cursor)) if order != cursor.order => Err(ExternalError::new(
                ExternalErrorCode::InvalidRequest,
                "That cursor is from a list in another order. Drop --order-by, or start again without --cursor.",
            )),
            (Some(order), _) => Ok(order),
            (None, Some(cursor)) => Ok(cursor.order),
            (None, None) => Ok(OrderArg::Priority),
        }
    }
}

/// Where a page ended: the order it was read in and the last row's place.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    order: OrderArg,
    after: [i64; 3],
}

impl fmt::Display for Cursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [first, second, third] = self.after;
        let order = self.order.to_possible_value().ok_or(fmt::Error)?;
        write!(formatter, "{}~{first}~{second}~{third}", order.get_name())
    }
}

fn cursor(value: &str) -> Result<Cursor, String> {
    let unread = || "expected the next_cursor of an earlier page".to_owned();
    let mut parts = value.trim().split('~');
    let order = parts
        .next()
        .and_then(|name| OrderArg::from_str(name, false).ok())
        .ok_or_else(unread)?;
    let mut after = [0; 3];
    for place in &mut after {
        *place = parts
            .next()
            .and_then(|part| part.parse().ok())
            .ok_or_else(unread)?;
    }
    if parts.next().is_some() {
        return Err(unread());
    }
    Ok(Cursor { order, after })
}

/// A task's field as JSON names it, for --fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum TaskField {
    Id,
    Handle,
    Link,
    Number,
    Title,
    Details,
    Status,
    Priority,
    Owner,
    Doer,
    Direction,
    DueDate,
    DueAtMs,
    Project,
    MilestoneId,
    Labels,
    Source,
    DuplicateOfId,
    BlockedByOpen,
    SnoozedUntilMs,
    FollowUpAtMs,
    Focus,
    ArchivedAtMs,
    StatusChangedAtMs,
    CreatedAtMs,
    UpdatedAtMs,
    Revision,
    Cycle,
    AtRisk,
    TimeBlock,
    Repeat,
    Parent,
    SubTasks,
    AttachmentCount,
    Estimate,
    CompletedAtMs,
    ForPerson,
}

impl TaskField {
    fn names(self, key: &str) -> bool {
        self.to_possible_value()
            .is_some_and(|value| value.get_name() == key)
    }
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MineArgs {
    /// Rows per list, at most 100.
    #[arg(long, value_parser = limit)]
    pub limit: Option<usize>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ShowArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct CreateArgs {
    pub title: String,
    #[command(flatten)]
    pub fields: Fields,
    #[command(flatten)]
    pub write: WriteArgs,
    /// The page or file the task came from: an http, https or file address.
    #[arg(long, value_name = "URL")]
    pub source_url: Option<String>,
    /// What that page or file is called.
    #[arg(long, requires = "source_url", value_name = "TITLE")]
    pub source_title: Option<String>,
    /// Print what would be created and write nothing.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct UpdateArgs {
    /// S-12, 12, or task ids.
    #[arg(required = true, value_parser = task_ref, value_name = "TASK")]
    pub tasks: Vec<TaskRef>,
    #[arg(long)]
    pub title: Option<String>,
    #[command(flatten)]
    pub fields: Fields,
    /// Labels to add, by name or id, comma-separated.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "LABEL",
        conflicts_with = "labels"
    )]
    pub add_label: Vec<String>,
    /// Labels to remove, by name or id, comma-separated.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "LABEL",
        conflicts_with = "labels"
    )]
    pub remove_label: Vec<String>,
    /// Hide the task until this day: YYYY-MM-DD, today, tomorrow, later-today, or none.
    #[arg(long, value_parser = snooze_day, value_name = "DAY")]
    pub snooze_until: Option<DayArg>,
    /// Plan the task in a cycle: current, next, its number, or none.
    #[arg(long, value_parser = cycle_ref, value_name = "CYCLE")]
    pub cycle: Option<WorkCycleRef>,
    /// Put the task under another: S-12, 12, its id, or none.
    #[arg(long, value_parser = threads::parent_arg, value_name = "TASK")]
    pub parent: Option<threads::ParentArg>,
    /// Points of effort, 0 to 64, or none.
    #[arg(long, value_parser = estimate_arg, value_name = "POINTS")]
    pub estimate: Option<EstimateArg>,
    /// The person the work is for, who is owed it: a name or id, or none.
    #[arg(long = "for", value_name = "PERSON")]
    pub for_person: Option<String>,
    /// Refuse unless the task is still at this revision.
    #[arg(long)]
    pub expected_revision: Option<i64>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ClaimArgs {
    /// S-12, 12, or the task's id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DoneArgs {
    /// S-12, 12, or task ids.
    #[arg(required = true, value_parser = task_ref, value_name = "TASK")]
    pub tasks: Vec<TaskRef>,
    /// A link kept with the task as evidence: http, https or file. Any
    /// other line is added to the details.
    #[arg(long, value_name = "URL")]
    pub evidence: Option<String>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct AcceptArgs {
    /// S-12, 12, or task ids.
    #[arg(required = true, value_parser = task_ref, value_name = "TASK")]
    pub tasks: Vec<TaskRef>,
    /// Accept without Sona's suggested owner, due day and project.
    #[arg(long)]
    pub ignore_suggestions: bool,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TasksArgs {
    /// S-12, 12, or task ids.
    #[arg(required = true, value_parser = task_ref, value_name = "TASK")]
    pub tasks: Vec<TaskRef>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SnoozeArgs {
    /// S-12, 12, or task ids.
    #[arg(required = true, value_parser = task_ref, value_name = "TASK")]
    pub tasks: Vec<TaskRef>,
    /// When it comes back: YYYY-MM-DD, today, tomorrow, later-today (the
    /// next hour three hours from now), or none to wake it.
    #[arg(long, value_parser = snooze_day, value_name = "DAY")]
    pub until: DayArg,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct DuplicateArgs {
    /// The duplicate: S-12, 12, or its id.
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    /// The task it repeats.
    #[arg(long, value_parser = task_ref, value_name = "TASK")]
    pub of: TaskRef,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct RelateArgs {
    #[arg(value_parser = task_ref, value_name = "TASK")]
    pub task: TaskRef,
    #[arg(value_enum)]
    pub link: LinkKind,
    #[arg(value_parser = task_ref, value_name = "OTHER")]
    pub other: TaskRef,
    /// Show the link as it would be made or removed; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LabelSaveArgs {
    /// The label's name. Saving a name that exists changes that label.
    pub name: String,
    /// The label to rename to NAME, by name or id.
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,
    /// Color: degrees on the color wheel, 0 to 360.
    #[arg(long, value_parser = clap::value_parser!(u16).range(0..=360))]
    pub hue: Option<u16>,
    /// Show the label as it would be saved; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct LabelDeleteArgs {
    /// The label's name or id.
    pub label: String,
    /// Show the label that would be deleted; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ViewSaveArgs {
    /// The view's name. Saving a name that exists changes that view.
    pub name: String,
    /// The view to rename to NAME, by name or id.
    #[arg(long, value_name = "VIEW")]
    pub view: Option<String>,
    #[command(flatten)]
    pub filters: Filters,
    #[arg(long, value_enum)]
    pub layout: Option<LayoutArg>,
    #[arg(long, value_enum)]
    pub group_by: Option<GroupArg>,
    #[arg(long, value_enum)]
    pub order_by: Option<OrderArg>,
    /// Show groups that hold no tasks: true or false.
    #[arg(long, value_name = "BOOL")]
    pub show_empty_groups: Option<bool>,
    /// The properties each row shows, in order, comma-separated.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "PROPERTY")]
    pub properties: Option<Vec<planner::PropertyArg>>,
    /// Show sub-tasks as rows of their own: true or false.
    #[arg(long, value_name = "BOOL")]
    pub show_sub_tasks: Option<bool>,
    /// Board swimlanes: rows of columns, one per group; none for one row.
    #[arg(long, value_enum)]
    pub sub_group_by: Option<GroupArg>,
    /// The board columns to leave out, comma-separated group keys such as
    /// status:done or priority:4; an empty value shows them all again.
    #[arg(long, value_delimiter = ',', value_parser = column_key, value_name = "KEY")]
    pub hidden_columns: Option<Vec<String>>,
    /// Show the view as it would be saved; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
#[command(group(clap::ArgGroup::new("place").required(true).args(["top", "bottom", "before", "after"])))]
pub struct MoveArgs {
    /// S-12, 12, or task ids, kept in their order among themselves.
    #[arg(required = true, value_parser = task_ref, value_name = "TASK")]
    pub tasks: Vec<TaskRef>,
    /// First in the manual order.
    #[arg(long)]
    pub top: bool,
    /// Last in the manual order.
    #[arg(long)]
    pub bottom: bool,
    /// Just before this task.
    #[arg(long, value_parser = task_ref, value_name = "TASK")]
    pub before: Option<TaskRef>,
    /// Just after this task.
    #[arg(long, value_parser = task_ref, value_name = "TASK")]
    pub after: Option<TaskRef>,
    /// Print where they would go and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ViewDeleteArgs {
    /// The view's name or id.
    pub view: String,
    /// Show the view that would be deleted; nothing changes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ExportArgs {
    /// The folder to write S-<number>.md files into.
    #[arg(long, value_name = "DIR")]
    pub md: PathBuf,
    #[command(flatten)]
    pub filters: Filters,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FollowUpArgs {
    /// The meeting's id.
    #[arg(value_name = "MEETING_ID")]
    pub meeting: Uuid,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct AskArgs {
    /// The question: "what did Dana promise about pricing".
    #[arg(required = true, num_args = 1.., value_name = "QUESTION")]
    pub question: Vec<String>,
    /// Make Triage tasks of these answer lines, by number: --take 1,3.
    #[arg(long, value_delimiter = ',', value_name = "N")]
    pub take: Vec<usize>,
    /// With --take: print the tasks it would make and write nothing.
    #[arg(long, requires = "take")]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SayArgs {
    /// The change in words.
    #[arg(required = true, num_args = 1.., value_name = "WORDS")]
    pub words: Vec<String>,
    /// Print what would change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct WatchArgs {
    /// The word or phrase to listen for.
    #[arg(required = true, num_args = 1.., value_name = "TOPIC")]
    pub topic: Vec<String>,
    /// Only when this person says it.
    #[arg(long, value_name = "PERSON")]
    pub person: Option<String>,
    /// Print the watch it would save and write nothing.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct UnwatchArgs {
    /// The watched phrase, or its id.
    #[arg(required = true, num_args = 1.., value_name = "TOPIC")]
    pub topic: Vec<String>,
    /// Print the watch it would stop and write nothing.
    #[arg(long)]
    pub dry_run: bool,
}

/// Which tasks a list keeps, by who and what.
#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct Scope {
    /// Only work you owe.
    #[arg(long, conflicts_with_all = ["waiting", "unowned"])]
    pub mine: bool,
    /// Only work others owe you.
    #[arg(long, conflicts_with = "unowned")]
    pub waiting: bool,
    /// Only work nobody owns.
    #[arg(long)]
    pub unowned: bool,
    /// Owners by name or id, comma-separated; me or nobody alone.
    #[arg(long, value_delimiter = ',', value_name = "PERSON")]
    pub owner: Vec<String>,
    /// Projects by name or id, comma-separated; none for work in no project.
    #[arg(long, value_delimiter = ',', value_name = "PROJECT")]
    pub project: Vec<String>,
    /// Labels by name or id, comma-separated: work with any of them.
    #[arg(long, value_delimiter = ',', value_name = "LABEL")]
    pub label: Vec<String>,
    /// urgent, high, medium, low or none, comma-separated.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub priority: Vec<Priority>,
    /// Due on or before this day: YYYY-MM-DD, today or tomorrow.
    #[arg(long, value_parser = day, value_name = "DAY")]
    pub due_by: Option<NaiveDate>,
    /// Due windows, comma-separated: overdue, today, this-week, next:N for
    /// the next N days, or none for no due day.
    #[arg(
        long,
        value_delimiter = ',',
        value_parser = due_window,
        value_name = "WINDOW"
    )]
    pub due: Vec<WorkDue>,
    /// Work raised in this meeting, by its id.
    #[arg(long, value_name = "MEETING_ID")]
    pub meeting: Option<Uuid>,
    /// Words in the title, details or comments, or a task number.
    #[arg(long)]
    pub search: Option<String>,
    /// Work planned in a cycle: current, next, its number, or none.
    #[arg(long, value_parser = cycle_ref, value_name = "CYCLE")]
    pub cycle: Option<WorkCycleRef>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct Filters {
    #[command(flatten)]
    pub scope: Scope,
    /// triage, backlog, todo, in-progress, done, canceled, duplicate, open,
    /// closed or all, comma-separated.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub status: Vec<StatusFilter>,
    /// Include archived work.
    #[arg(long)]
    pub archived: bool,
    /// Include snoozed work.
    #[arg(long)]
    pub snoozed: bool,
    /// Conditions as JSON, one object or a list: {"field":"label","op":
    /// "includes_all","values":["bug","ui"]}. Fields: status, priority,
    /// owner, delegate, project, milestone, label, parent, cycle,
    /// cycle_scope, due, created, updated, estimate, for, completed. Ops:
    /// is, is_not, any_of, none_of,
    /// includes_all, includes_any, before, after. Values are ids, names Sona
    /// stores, agent names, `me`, `none`, task numbers (S-12) for parent,
    /// planned or added for cycle_scope, cycle numbers or current/next, and
    /// days as YYYY-MM-DD/today/tomorrow.
    #[arg(long, value_parser = conditions, value_name = "JSON")]
    pub filter: Option<ConditionsArg>,
    /// Only the sub-tasks of this task (S-12 or its id); none for tasks
    /// under no other.
    #[arg(long, value_parser = parent, value_name = "TASK")]
    pub parent: Option<ParentArg>,
    /// Only work planned in its cycle from the start (rolled-over work
    /// included), or only work added to its cycle after it began.
    #[arg(long, value_enum)]
    pub cycle_scope: Option<CycleScopeArg>,
    /// Only work for these people, who are owed it: names or ids,
    /// comma-separated.
    #[arg(long = "for", value_delimiter = ',', value_name = "PERSON")]
    pub for_person: Vec<String>,
    /// Only work marked done in this window: YYYY-MM-DD, today, yesterday,
    /// this_week, last_week, this_month or last_month.
    #[arg(long, value_name = "WHEN")]
    pub completed: Option<String>,
}

/// An estimate to set: its points, or none to clear it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EstimateArg(Option<u8>);

fn estimate_arg(value: &str) -> Result<EstimateArg, String> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Ok(EstimateArg(None));
    }
    value
        .parse::<u8>()
        .ok()
        .filter(|points| *points <= items::MAX_ESTIMATE)
        .map(|points| EstimateArg(Some(points)))
        .ok_or_else(|| "An estimate is 0 to 64 points, or none.".to_owned())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParentArg {
    Task(TaskRef),
    None,
}

fn parent(value: &str) -> Result<ParentArg, String> {
    if value.trim().eq_ignore_ascii_case("none") {
        return Ok(ParentArg::None);
    }
    task_ref(value).map(ParentArg::Task)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CycleScopeArg {
    Planned,
    Added,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConditionsArg(Vec<WorkCondition>);

fn conditions(value: &str) -> Result<ConditionsArg, String> {
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(WorkCondition),
        Many(Vec<WorkCondition>),
    }
    match serde_json::from_str(value).map_err(|error| format!("Not a filter: {error}"))? {
        OneOrMany::One(condition) => Ok(ConditionsArg(vec![condition])),
        OneOrMany::Many(conditions) => Ok(ConditionsArg(conditions)),
    }
}

/// What a new task starts with, or what an update sets.
#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct Fields {
    #[arg(long)]
    pub details: Option<String>,
    /// triage, backlog, todo, in-progress, done or canceled.
    #[arg(long, value_enum)]
    pub status: Option<StatusArg>,
    /// urgent, high, medium, low or none (1 to 4, 0 for none).
    #[arg(long, value_enum)]
    pub priority: Option<Priority>,
    /// me, nobody, or a person's name or id.
    #[arg(long)]
    pub owner: Option<String>,
    /// YYYY-MM-DD, today, tomorrow, or none.
    #[arg(long, value_parser = day_or_none, value_name = "DAY")]
    pub due: Option<DayArg>,
    /// How often it repeats: every monday, every 2 weeks, every weekday or
    /// daily; none stops it. Without --due, a new task that repeats on named
    /// days is due on the next one.
    #[arg(long, value_parser = repeat, value_name = "WORDS")]
    pub repeat: Option<RepeatArg>,
    /// A project's name or id, or none.
    #[arg(long)]
    pub project: Option<String>,
    /// A milestone id in the task's project, or none.
    #[arg(long)]
    pub milestone: Option<String>,
    /// All of the task's labels, by name or id, comma-separated; "" clears them.
    #[arg(long, value_delimiter = ',', value_name = "LABEL")]
    pub labels: Option<Vec<String>>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct WriteArgs {
    /// A UUID naming this change, so a retry cannot apply it twice. Derived
    /// from the request when left out.
    #[arg(long, value_name = "UUID")]
    pub operation_id: Option<Uuid>,
    /// Who is making the change, as Sona shows it: "Claude Code", "Codex".
    /// Unless the user lets agents' changes apply, changes to a task's
    /// fields wait for them to accept in Sona.
    #[arg(long, value_name = "NAME")]
    pub agent: Option<String>,
}

/// A task as a caller names it: its number (`S-12`, `#12`, `12`) or its id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskRef {
    Number(i64),
    Id(String),
}

impl fmt::Display for TaskRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(number) => write!(formatter, "S-{number}"),
            Self::Id(id) => formatter.write_str(id),
        }
    }
}

/// A day to set, or `none` to clear it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DayArg {
    Clear,
    On(NaiveDate),
    At(i64),
}

impl DayArg {
    fn ms(self) -> Result<Option<i64>, ExternalError> {
        match self {
            Self::Clear => Ok(None),
            Self::On(day) => midnight(day).map(Some),
            Self::At(ms) => Ok(Some(ms)),
        }
    }
}

/// How often a task repeats, in the parser's words, with when it next falls
/// when the words name the days; or `none` to stop it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepeatArg {
    Clear,
    Every {
        every: String,
        next_at_ms: Option<i64>,
    },
}

impl RepeatArg {
    fn every(&self) -> Option<String> {
        match self {
            Self::Clear => None,
            Self::Every { every, .. } => Some(every.clone()),
        }
    }

    const fn next_at_ms(&self) -> Option<i64> {
        match self {
            Self::Clear => None,
            Self::Every { next_at_ms, .. } => *next_at_ms,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum StatusArg {
    Triage,
    Backlog,
    #[value(alias = "not-started", alias = "not_started")]
    Todo,
    #[value(alias = "in_progress")]
    InProgress,
    Done,
    #[value(alias = "cancelled")]
    Canceled,
}

impl StatusArg {
    const fn status(self) -> WorkStatus {
        match self {
            Self::Triage => WorkStatus::Triage,
            Self::Backlog => WorkStatus::Backlog,
            Self::Todo => WorkStatus::NotStarted,
            Self::InProgress => WorkStatus::InProgress,
            Self::Done => WorkStatus::Done,
            Self::Canceled => WorkStatus::Canceled,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum StatusFilter {
    Triage,
    Backlog,
    #[value(alias = "not-started", alias = "not_started")]
    Todo,
    #[value(alias = "in_progress")]
    InProgress,
    Done,
    #[value(alias = "cancelled")]
    Canceled,
    Duplicate,
    Open,
    Closed,
    All,
}

impl StatusFilter {
    const fn statuses(self) -> &'static [WorkStatus] {
        match self {
            Self::Triage => &[WorkStatus::Triage],
            Self::Backlog => &[WorkStatus::Backlog],
            Self::Todo => &[WorkStatus::NotStarted],
            Self::InProgress => &[WorkStatus::InProgress],
            Self::Done => &[WorkStatus::Done],
            Self::Canceled => &[WorkStatus::Canceled],
            Self::Duplicate => &[WorkStatus::Duplicate],
            Self::Open => &[
                WorkStatus::Backlog,
                WorkStatus::NotStarted,
                WorkStatus::InProgress,
            ],
            Self::Closed => &[
                WorkStatus::Done,
                WorkStatus::Canceled,
                WorkStatus::Duplicate,
            ],
            Self::All => &[
                WorkStatus::Triage,
                WorkStatus::Backlog,
                WorkStatus::NotStarted,
                WorkStatus::InProgress,
                WorkStatus::Done,
                WorkStatus::Canceled,
                WorkStatus::Duplicate,
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    #[value(alias = "0")]
    None,
    #[value(alias = "1")]
    Urgent,
    #[value(alias = "2")]
    High,
    #[value(alias = "3")]
    Medium,
    #[value(alias = "4")]
    Low,
}

impl Priority {
    const fn of(value: u8) -> Self {
        match value {
            1 => Self::Urgent,
            2 => Self::High,
            3 => Self::Medium,
            4 => Self::Low,
            _ => Self::None,
        }
    }

    const fn value(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Urgent => 1,
            Self::High => 2,
            Self::Medium => 3,
            Self::Low => 4,
        }
    }

    const fn text(self) -> &'static str {
        match self {
            Self::None => "No priority",
            Self::Urgent => "Urgent",
            Self::High => "High",
            Self::Medium => "Medium",
            Self::Low => "Low",
        }
    }

    const fn short(self) -> &'static str {
        match self {
            Self::None => "-",
            other => other.text(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum LinkKind {
    Blocks,
    #[value(alias = "blocked_by")]
    BlockedBy,
    #[value(alias = "relates")]
    Related,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum LayoutArg {
    List,
    Board,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum GroupArg {
    Status,
    Owner,
    Project,
    Meeting,
    Due,
    Priority,
    Cycle,
    Label,
    Milestone,
    None,
}

impl GroupArg {
    const fn group(self) -> WorkGroupBy {
        match self {
            Self::Status => WorkGroupBy::Status,
            Self::Owner => WorkGroupBy::Owner,
            Self::Project => WorkGroupBy::Project,
            Self::Meeting => WorkGroupBy::Meeting,
            Self::Due => WorkGroupBy::Due,
            Self::Priority => WorkGroupBy::Priority,
            Self::Cycle => WorkGroupBy::Cycle,
            Self::Label => WorkGroupBy::Label,
            Self::Milestone => WorkGroupBy::Milestone,
            Self::None => WorkGroupBy::None,
        }
    }
}

/// A board column's group key: the grouping, a colon, and the group.
fn column_key(value: &str) -> Result<String, String> {
    let key = value.trim();
    let kinds = ["status", "owner", "project", "meeting", "due", "priority", "cycle", "label", "milestone"];
    match key.split_once(':') {
        _ if key.is_empty() => Ok(String::new()),
        Some((kind, group)) if kinds.contains(&kind) && !group.is_empty() => Ok(key.to_owned()),
        _ => Err(format!(
            "expected a group key such as status:done, its grouping one of {}",
            kinds.join(", ")
        )),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OrderArg {
    Priority,
    Due,
    Updated,
    Created,
    Status,
    Manual,
}

impl OrderArg {
    const fn order(self) -> WorkOrderBy {
        match self {
            Self::Priority => WorkOrderBy::Priority,
            Self::Due => WorkOrderBy::Due,
            Self::Updated => WorkOrderBy::Updated,
            Self::Created => WorkOrderBy::Created,
            Self::Status => WorkOrderBy::Status,
            Self::Manual => WorkOrderBy::Manual,
        }
    }
}

fn task_ref(value: &str) -> Result<TaskRef, String> {
    let text = value.trim();
    let number = text
        .strip_prefix("S-")
        .or_else(|| text.strip_prefix("s-"))
        .or_else(|| text.strip_prefix('#'))
        .unwrap_or(text);
    if let Ok(number) = number.parse::<i64>() {
        if number > 0 {
            return Ok(TaskRef::Number(number));
        }
    }
    Uuid::parse_str(text)
        .map(|id| TaskRef::Id(id.to_string()))
        .map_err(|_| "expected a task: S-12, 12, or a task id".to_owned())
}

fn limit(value: &str) -> Result<usize, String> {
    match value.trim().parse::<usize>() {
        Ok(0) | Err(_) => Err("expected a number of rows, at least 1".to_owned()),
        Ok(rows) => Ok(rows.min(MAX_LIMIT)),
    }
}

fn day(value: &str) -> Result<NaiveDate, String> {
    let today = Local::now().date_naive();
    match value.trim().to_ascii_lowercase().as_str() {
        "today" => Ok(today),
        "tomorrow" => today
            .succ_opt()
            .ok_or_else(|| "tomorrow is past the calendar".to_owned()),
        other => NaiveDate::parse_from_str(other, "%Y-%m-%d")
            .map_err(|_| "expected YYYY-MM-DD, today or tomorrow".to_owned()),
    }
}

fn day_or_none(value: &str) -> Result<DayArg, String> {
    if value.trim().eq_ignore_ascii_case("none") {
        Ok(DayArg::Clear)
    } else {
        day(value).map(DayArg::On)
    }
}

fn snooze_day(value: &str) -> Result<DayArg, String> {
    if !value.trim().eq_ignore_ascii_case("later-today") {
        return day_or_none(value);
    }
    let now = Local::now();
    let later = now + TimeDelta::hours(3);
    let hour = later
        .with_minute(0)
        .and_then(|time| time.with_second(0))
        .and_then(|time| time.with_nanosecond(0))
        .ok_or_else(|| "later today has no clock time".to_owned())?;
    let hour = if hour < later {
        hour + TimeDelta::hours(1)
    } else {
        hour
    };
    if hour.date_naive() != now.date_naive() {
        return Err("later today runs past midnight; snooze until tomorrow".to_owned());
    }
    Ok(DayArg::At(hour.timestamp_millis()))
}

fn repeat(value: &str) -> Result<RepeatArg, String> {
    if value.trim().eq_ignore_ascii_case("none") {
        return Ok(RepeatArg::Clear);
    }
    parse::repeat(value, &Local::now())
        .map(|(every, next_at_ms)| RepeatArg::Every { every, next_at_ms })
        .ok_or_else(|| {
            "expected how often it repeats, such as every monday, every 2 weeks, every weekday or daily, or none".to_owned()
        })
}

fn due_window(value: &str) -> Result<WorkDue, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "overdue" => Ok(WorkDue::Overdue),
        "today" => Ok(WorkDue::Today),
        "this-week" => Ok(WorkDue::ThisWeek),
        "none" => Ok(WorkDue::NoDate),
        other => other
            .strip_prefix("next:")
            .and_then(|days| days.parse::<u16>().ok())
            .filter(|days| *days > 0)
            .map(|days| WorkDue::NextDays { days })
            .ok_or_else(|| {
                "expected overdue, today, this-week, next:N for the next N days, or none".to_owned()
            }),
    }
}

/// A due window as --due takes it.
fn due_flag(due: WorkDue) -> String {
    match due {
        WorkDue::Overdue => "overdue".to_owned(),
        WorkDue::Today => "today".to_owned(),
        WorkDue::ThisWeek => "this-week".to_owned(),
        WorkDue::NextDays { days } => format!("next:{days}"),
        WorkDue::NoDate => "none".to_owned(),
    }
}

fn cycle_ref(value: &str) -> Result<WorkCycleRef, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "current" => Ok(WorkCycleRef::Current),
        "next" => Ok(WorkCycleRef::Next),
        "none" => Ok(WorkCycleRef::None),
        other => other
            .parse::<i64>()
            .ok()
            .filter(|number| *number > 0)
            .map(|number| WorkCycleRef::Number { number })
            .ok_or_else(|| "expected a cycle: current, next, none, or its number".to_owned()),
    }
}

fn midnight(day: NaiveDate) -> Result<i64, ExternalError> {
    day_start_utc_ms(&day.format("%Y-%m-%d").to_string())
}

impl WorkVerb {
    /// Which grants this verb needs. A dry run only reads.
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self {
            Self::Settings(verb) => verb.required_scopes(),
            Self::Sources(verb) => verb.required_scopes(),
            Self::Library(verb) => verb.required_scopes(),
            Self::Transcript(verb) => verb.required_scopes(),
            Self::Dictations(verb) => verb.required_scopes(),
            Self::List(_)
            | Self::Bin
            | Self::ArchiveList
            | Self::DescriptionVersions(_)
            | Self::DescriptionVersion(_)
            | Self::Favorites
            | Self::Subscriptions
            | Self::Inbox(_)
            | Self::Delete(TasksArgs { dry_run: true, .. })
            | Self::DescriptionRestore(lifecycle::VersionRestoreArgs { dry_run: true, .. })
            | Self::Favorite(lifecycle::FavoriteArgs { dry_run: true, .. })
            | Self::Unfavorite(lifecycle::FavoriteArgs { dry_run: true, .. })
            | Self::FavoriteFolderSave(lifecycle::FavoriteFolderSaveArgs { dry_run: true, .. })
            | Self::FavoriteFolderDelete(lifecycle::FavoriteFolderDeleteArgs { dry_run: true, .. })
            | Self::FavoriteMove(lifecycle::FavoriteMoveArgs { dry_run: true, .. })
            | Self::Subscribe(TasksArgs { dry_run: true, .. })
            | Self::Unsubscribe(TasksArgs { dry_run: true, .. })
            | Self::InboxMark(lifecycle::InboxMarkArgs { dry_run: true, .. })
            | Self::Digest(digest::DigestArgs { dry_run: true, .. })
            | Self::Ready(_)
            | Self::Mine(_)
            | Self::Counts
            | Self::Cycles(_)
            | Self::Cycle(_)
            | Self::Show(_)
            | Self::Labels
            | Self::Views
            | Self::FollowUp(_)
            | Self::Projects(_)
            | Self::Milestones(_)
            | Self::Timeline(_)
            | Self::Insights(_)
            | Self::People(_)
            | Self::Person(_)
            | Self::Companies(_)
            | Self::Company(_)
            | Self::SpeakerVoices(_)
            | Self::FirstMet(_)
            | Self::Meetings(_)
            | Self::Meeting(_)
            | Self::Comments(_)
            | Self::Comment(threads::CommentCommand {
                verb:
                    threads::CommentVerb::Add(threads::CommentAddArgs { dry_run: true, .. })
                    | threads::CommentVerb::Edit(threads::CommentEditArgs { dry_run: true, .. })
                    | threads::CommentVerb::Delete(threads::CommentDeleteArgs {
                        dry_run: true,
                        ..
                    })
                    | threads::CommentVerb::Resolve(threads::CommentResolveArgs {
                        dry_run: true,
                        ..
                    }),
            })
            | Self::CommentEdit(threads::CommentEditArgs { dry_run: true, .. })
            | Self::CommentDelete(threads::CommentDeleteArgs { dry_run: true, .. })
            | Self::CommentResolve(threads::CommentResolveArgs { dry_run: true, .. })
            | Self::Subtask(threads::SubtaskArgs { dry_run: true, .. })
            | Self::Attachments(_)
            | Self::Attach(attachments::AttachArgs { dry_run: true, .. })
            | Self::Detach(attachments::DetachArgs { dry_run: true, .. })
            | Self::Update(UpdateArgs { dry_run: true, .. })
            | Self::Done(DoneArgs { dry_run: true, .. })
            | Self::Accept(AcceptArgs { dry_run: true, .. })
            | Self::Decline(routines::DeclineArgs { dry_run: true, .. })
            | Self::Templates
            | Self::LoopList
            | Self::LoopShow(_)
            | Self::LoopSave(agent_loops::LoopSaveArgs { dry_run: true, .. })
            | Self::LoopRun(agent_loops::LoopArgs { dry_run: true, .. })
            | Self::LoopEnable(agent_loops::LoopArgs { dry_run: true, .. })
            | Self::LoopDisable(agent_loops::LoopArgs { dry_run: true, .. })
            | Self::LoopRestoreVersion(agent_loops::LoopRestoreArgs { dry_run: true, .. })
            | Self::LoopDelete(agent_loops::LoopArgs { dry_run: true, .. })
            | Self::Reminders(_)
            | Self::LabelGroup(routines::LabelGroupArgs { dry_run: true, .. })
            | Self::LabelRetire(routines::LabelRetireArgs { dry_run: true, .. })
            | Self::LabelScope(routines::LabelScopeArgs { dry_run: true, .. })
            | Self::Milestone(_)
            | Self::ProjectStatuses
            | Self::ProjectHistory(_)
            | Self::MilestoneToProject(catalog::MilestoneToProjectArgs { dry_run: true, .. })
            | Self::ProjectRelate(dependencies::ProjectRelateArgs { dry_run: true, .. })
            | Self::ProjectUnrelate(dependencies::ProjectRelateArgs { dry_run: true, .. })
            | Self::ProjectDependencies(_)
            | Self::InitiativeList
            | Self::InitiativeShow(_)
            | Self::InitiativeNew(initiatives::InitiativeNewArgs { dry_run: true, .. })
            | Self::InitiativeUpdate(initiatives::InitiativeUpdateArgs { dry_run: true, .. })
            | Self::InitiativeDelete(initiatives::InitiativeDeleteArgs { dry_run: true, .. })
            | Self::InitiativeAddProject(initiatives::InitiativeProjectArgs { dry_run: true, .. })
            | Self::InitiativeRemoveProject(initiatives::InitiativeProjectArgs {
                dry_run: true, ..
            })
            | Self::ProjectTemplateList
            | Self::ProjectTemplateSave(project_templates::ProjectTemplateSaveArgs {
                dry_run: true,
                ..
            })
            | Self::ProjectTemplateDelete(project_templates::ProjectTemplateDeleteArgs {
                dry_run: true,
                ..
            })
            | Self::ProjectNew(project_templates::ProjectNewArgs { dry_run: true, .. })
            | Self::ProjectStatusSave(catalog::ProjectStatusSaveArgs { dry_run: true, .. })
            | Self::ProjectStatusDelete(catalog::ProjectStatusDeleteArgs { dry_run: true, .. })
            | Self::TemplateSave(routines::TemplateSaveArgs { dry_run: true, .. })
            | Self::TemplateDelete(routines::TemplateDeleteArgs { dry_run: true, .. })
            | Self::TemplateUse(routines::TemplateUseArgs { dry_run: true, .. })
            | Self::Remind(routines::RemindArgs { dry_run: true, .. })
            | Self::Archive(TasksArgs { dry_run: true, .. })
            | Self::Restore(TasksArgs { dry_run: true, .. })
            | Self::Move(MoveArgs { dry_run: true, .. })
            | Self::Snooze(SnoozeArgs { dry_run: true, .. })
            | Self::Say(SayArgs { dry_run: true, .. })
            | Self::Watches
            | Self::Project(_)
            | Self::ProjectUpdates(_)
            | Self::ProjectUpdateDraft(_)
            | Self::ProjectUpdate(roadmap::ProjectUpdateArgs { dry_run: true, .. })
            | Self::ProjectUpdateEdit(roadmap::ProjectUpdateEditArgs { dry_run: true, .. })
            | Self::ProjectUpdateDelete(roadmap::ProjectUpdateDeleteArgs { dry_run: true, .. })
            | Self::ProjectDelete(roadmap::ProjectDeleteArgs { dry_run: true, .. })
            | Self::ProjectSave(catalog::ProjectSaveArgs { dry_run: true, .. })
            | Self::ProjectArchive(catalog::ProjectArchiveArgs { dry_run: true, .. })
            | Self::MilestoneSave(catalog::MilestoneSaveArgs { dry_run: true, .. })
            | Self::MilestoneDelete(catalog::MilestoneDeleteArgs { dry_run: true, .. })
            | Self::MilestoneReorder(roadmap::MilestoneReorderArgs { dry_run: true, .. })
            | Self::CycleSettings(roadmap::CycleSettingsArgs { dry_run: true, .. })
            | Self::CycleSave(roadmap::CycleSaveArgs { dry_run: true, .. })
            | Self::CycleRollover(roadmap::CycleRolloverArgs { dry_run: true, .. })
            | Self::CycleStartToday(roadmap::CycleStartTodayArgs { dry_run: true, .. })
            | Self::Create(CreateArgs { dry_run: true, .. })
            | Self::Day(_)
            | Self::Brief(_)
            | Self::Delegate(organize::DelegateArgs { dry_run: true, .. })
            | Self::Focus(organize::FocusArgs { dry_run: true, .. })
            | Self::Keep(TasksArgs { dry_run: true, .. })
            | Self::TalkingPoint(organize::TalkingPointArgs { dry_run: true, .. })
            | Self::TalkingPointDone(organize::TalkingPointDoneArgs { dry_run: true, .. })
            | Self::LoopReopen(organize::LoopArgs { dry_run: true, .. })
            | Self::LoopOwner(organize::LoopOwnerArgs { dry_run: true, .. })
            | Self::SpeakerAssign(speakers::SpeakerAssignArgs { dry_run: true, .. })
            | Self::PersonLink(people::PersonLinkArgs { dry_run: true, .. })
            | Self::PersonMerge(catalog::PersonMergeArgs { dry_run: true, .. })
            | Self::PersonSave(catalog::PersonSaveArgs { dry_run: true, .. })
            | Self::PersonSplit(people::PersonSplitArgs { dry_run: true, .. })
            | Self::PersonDelete(people::PersonDeleteArgs { dry_run: true, .. })
            | Self::PersonPhoto(people::PersonPhotoArgs { dry_run: true, .. })
            | Self::PersonContactRemove(people::PersonContactRemoveArgs { dry_run: true, .. })
            | Self::FirstMetAnswer(people::FirstMetArgs { dry_run: true, .. })
            | Self::SpeakerConfirm(voices::SpeakerVoiceArgs { dry_run: true, .. })
            | Self::SpeakerVoiceUndo(voices::SpeakerVoiceArgs { dry_run: true, .. })
            | Self::ActionTrack(organize::ActionTrackArgs { dry_run: true, .. })
            | Self::ActionDismiss(organize::LoopArgs { dry_run: true, .. })
            | Self::ActionRestore(organize::LoopArgs { dry_run: true, .. })
            | Self::NoteNew(notes::NoteNewArgs { dry_run: true, .. })
            | Self::NoteEdit(notes::NoteEditArgs { dry_run: true, .. })
            | Self::NotePin(notes::NotePinArgs { dry_run: true, .. })
            | Self::NoteLink(notes::NoteLinkArgs { dry_run: true, .. })
            | Self::NoteUnlink(notes::NoteLinkArgs { dry_run: true, .. })
            | Self::NoteDelete(notes::NoteDeleteArgs { dry_run: true, .. })
            | Self::NoteRestore(notes::NoteRestoreArgs { dry_run: true, .. })
            | Self::Subjects
            | Self::SubjectNew(subjects::SubjectNewArgs { dry_run: true, .. })
            | Self::SubjectNote(subjects::SubjectNoteArgs { dry_run: true, .. })
            | Self::Parse(_)
            | Self::Book(planner::BookArgs { dry_run: true, .. })
            | Self::Unbook(planner::BookArgs { dry_run: true, .. })
            | Self::Nudged(planner::BookArgs { dry_run: true, .. })
            | Self::WatchTask(planner::WatchTaskArgs { dry_run: true, .. })
            | Self::Relate(RelateArgs { dry_run: true, .. })
            | Self::Unrelate(RelateArgs { dry_run: true, .. })
            | Self::LabelSave(LabelSaveArgs { dry_run: true, .. })
            | Self::LabelDelete(LabelDeleteArgs { dry_run: true, .. })
            | Self::ViewSave(ViewSaveArgs { dry_run: true, .. })
            | Self::ViewDelete(ViewDeleteArgs { dry_run: true, .. })
            | Self::LinkSuggestions(_)
            | Self::LinkAccept(planner::LinkDecideArgs { dry_run: true, .. })
            | Self::LinkDismiss(planner::LinkDecideArgs { dry_run: true, .. })
            | Self::TriageSuggestions(_)
            | Self::SuggestionAccept(triage_hints::SuggestionDecideArgs { dry_run: true, .. })
            | Self::SuggestionDismiss(triage_hints::SuggestionDecideArgs { dry_run: true, .. })
            | Self::Calendar(_)
            | Self::Trend(_)
            | Self::Today
            | Self::PlanAccept(planner::PlanAcceptArgs { dry_run: true, .. })
            | Self::AttachmentRename(organize::AttachmentRenameArgs { dry_run: true, .. })
            | Self::Save(save::SaveArgs { dry_run: true, .. })
            | Self::Changes(_)
            | Self::SessionStart(sessions::SessionStartArgs { dry_run: true, .. })
            | Self::SessionActivity(sessions::SessionActivityArgs { dry_run: true, .. })
            | Self::SessionPlan(sessions::SessionPlanArgs { dry_run: true, .. })
            | Self::SessionHeartbeat(sessions::SessionWriteArgs { dry_run: true, .. })
            | Self::SessionStop(sessions::SessionWriteArgs { dry_run: true, .. })
            | Self::SessionReply(sessions::SessionReplyArgs { dry_run: true, .. })
            | Self::Claim(ClaimArgs { dry_run: true, .. })
            | Self::Duplicate(DuplicateArgs { dry_run: true, .. })
            | Self::Ask(AskArgs { dry_run: true, .. })
            | Self::Watch(WatchArgs { dry_run: true, .. })
            | Self::Unwatch(UnwatchArgs { dry_run: true, .. })
            | Self::Session(_)
            | Self::Sessions(_) => {
                READ_SCOPES
            }
            Self::Ask(AskArgs { take, .. }) if take.is_empty() => READ_SCOPES,
            Self::Export(_)
            | Self::Create(_)
            | Self::Update(_)
            | Self::Claim(_)
            | Self::Done(_)
            | Self::Accept(_)
            | Self::Decline(_)
            | Self::Duplicate(_)
            | Self::Archive(_)
            | Self::Restore(_)
            | Self::Move(_)
            | Self::Delete(_)
            | Self::DescriptionRestore(_)
            | Self::Favorite(_)
            | Self::Unfavorite(_)
            | Self::FavoriteFolderSave(_)
            | Self::FavoriteFolderDelete(_)
            | Self::FavoriteMove(_)
            | Self::Subscribe(_)
            | Self::Unsubscribe(_)
            | Self::InboxMark(_)
            | Self::Digest(_)
            | Self::Snooze(_)
            | Self::Relate(_)
            | Self::Unrelate(_)
            | Self::LabelSave(_)
            | Self::LabelDelete(_)
            | Self::LabelGroup(_)
            | Self::LabelRetire(_)
            | Self::LabelScope(_)
            | Self::MilestoneToProject(_)
            | Self::ProjectRelate(_)
            | Self::ProjectUnrelate(_)
            | Self::InitiativeNew(_)
            | Self::InitiativeUpdate(_)
            | Self::InitiativeDelete(_)
            | Self::InitiativeAddProject(_)
            | Self::InitiativeRemoveProject(_)
            | Self::ProjectTemplateSave(_)
            | Self::ProjectTemplateDelete(_)
            | Self::ProjectNew(_)
            | Self::ProjectStatusSave(_)
            | Self::ProjectStatusDelete(_)
            | Self::TemplateSave(_)
            | Self::TemplateDelete(_)
            | Self::TemplateUse(_)
            | Self::LoopSave(_)
            | Self::LoopRun(_)
            | Self::LoopEnable(_)
            | Self::LoopDisable(_)
            | Self::LoopRestoreVersion(_)
            | Self::LoopDelete(_)
            | Self::Remind(_)
            | Self::ProjectSave(_)
            | Self::ProjectArchive(_)
            | Self::MilestoneSave(_)
            | Self::MilestoneDelete(_)
            | Self::PersonSave(_)
            | Self::PersonMerge(_)
            | Self::PersonLink(_)
            | Self::PersonSplit(_)
            | Self::PersonDelete(_)
            | Self::PersonPhoto(_)
            | Self::PersonContactRemove(_)
            | Self::FirstMetAnswer(_)
            | Self::ViewSave(_)
            | Self::ViewDelete(_)
            | Self::Ask(_)
            | Self::Say(_)
            | Self::Watch(_)
            | Self::Unwatch(_)
            | Self::Comment(_)
            | Self::CommentEdit(_)
            | Self::CommentDelete(_)
            | Self::CommentResolve(_)
            | Self::Subtask(_)
            | Self::Attach(_)
            | Self::ProjectUpdate(_)
            | Self::ProjectUpdateEdit(_)
            | Self::ProjectUpdateDelete(_)
            | Self::ProjectDelete(_)
            | Self::MilestoneReorder(_)
            | Self::CycleSettings(_)
            | Self::CycleSave(_)
            | Self::CycleRollover(_)
            | Self::CycleStartToday(_)
            | Self::Detach(_)
            | Self::Delegate(_)
            | Self::Focus(_)
            | Self::Keep(_)
            | Self::TalkingPoint(_)
            | Self::TalkingPointDone(_)
            | Self::LoopReopen(_)
            | Self::LoopOwner(_)
            | Self::SpeakerAssign(_)
            | Self::SpeakerConfirm(_)
            | Self::SpeakerVoiceUndo(_)
            | Self::ActionTrack(_)
            | Self::ActionDismiss(_)
            | Self::ActionRestore(_)
            | Self::NoteNew(_)
            | Self::NoteEdit(_)
            | Self::NotePin(_)
            | Self::NoteLink(_)
            | Self::NoteUnlink(_)
            | Self::NoteDelete(_)
            | Self::NoteRestore(_)
            | Self::SubjectNew(_)
            | Self::SubjectNote(_)
            | Self::Book(_)
            | Self::Unbook(_)
            | Self::Nudged(_)
            | Self::WatchTask(_)
            | Self::LinkAccept(_)
            | Self::LinkDismiss(_)
            | Self::SuggestionAccept(_)
            | Self::SuggestionDismiss(_)
            | Self::PlanAccept(_)
            | Self::AttachmentRename(_)
            | Self::Save(_)
            | Self::SessionStart(_)
            | Self::SessionActivity(_)
            | Self::SessionPlan(_)
            | Self::SessionHeartbeat(_)
            | Self::SessionStop(_)
            | Self::SessionReply(_) => WRITE_SCOPES,
        }
    }

    /// What clap cannot check: a flag that needs one task, a change that
    /// changes nothing, a name left empty.
    pub(super) fn check(&self) -> Result<(), ExternalError> {
        let refusal = match self {
            Self::Create(args) if args.title.trim().is_empty() => "A task needs a title.",
            Self::Create(args)
                if args
                    .source_url
                    .as_deref()
                    .is_some_and(|url| agents::evidence_link(url.trim()).is_none()) =>
            {
                "--source-url must be an http, https or file address."
            }
            Self::SpeakerAssign(args) if args.new.as_deref().is_some_and(|name| name.trim().is_empty()) => {
                "--new needs the person's name."
            }
            Self::PersonSplit(args) if args.new.as_deref().is_some_and(|name| name.trim().is_empty()) => {
                "--new needs the new person's name."
            }
            Self::NoteNew(args) if args.body.join(" ").trim().is_empty() => {
                "A note needs some text."
            }
            Self::AttachmentRename(args) if args.title.trim().is_empty() => {
                "--title cannot be empty."
            }
            Self::TalkingPoint(args) if args.text.join(" ").trim().is_empty() => {
                "A talking point needs some text."
            }
            Self::Update(args)
                if args
                    .title
                    .as_deref()
                    .is_some_and(|title| title.trim().is_empty()) =>
            {
                "--title cannot be empty."
            }
            Self::Update(args) if args.expected_revision.is_some() && args.tasks.len() > 1 => {
                "--expected-revision fences one task; name one."
            }
            Self::Update(args) if !args.changes_something() => {
                "Name a change: --title, --details, --status, --priority, --owner, --due, --repeat, --project, --milestone, --labels, --add-label, --remove-label, --snooze-until, --cycle, --parent, --estimate or --for."
            }
            Self::Comment(threads::CommentCommand {
                verb:
                    threads::CommentVerb::Add(threads::CommentAddArgs { body, .. })
                    | threads::CommentVerb::Edit(threads::CommentEditArgs { body, .. }),
            })
            | Self::CommentEdit(threads::CommentEditArgs { body, .. })
                if body.trim().is_empty() =>
            {
                "--body cannot be empty."
            }
            Self::Done(args) if args.evidence.is_some() && args.tasks.len() > 1 => {
                "--evidence belongs to one task; name one."
            }
            Self::Done(args)
                if args.evidence.as_deref().is_some_and(|evidence| {
                    evidence.trim().is_empty() || evidence.contains(['\n', '\r'])
                }) =>
            {
                "--evidence takes one line: a link or a short note."
            }
            Self::LabelSave(args) if args.name.trim().is_empty() => "A label needs a name.",
            Self::ViewSave(args) if args.name.trim().is_empty() => "A view needs a name.",
            Self::ProjectSave(args) if args.name.trim().is_empty() => "A project needs a name.",
            Self::ProjectStatusSave(args) if args.name.trim().is_empty() => {
                "A status needs a name."
            }
            Self::MilestoneSave(args) if args.name.trim().is_empty() => {
                "A milestone needs a name."
            }
            Self::PersonSave(args) if args.name.trim().is_empty() => "A person needs a name.",
            Self::ProjectUpdate(args)
                if args.body.as_deref().is_some_and(|body| body.trim().is_empty()) =>
            {
                "--body cannot be empty; leave it out to have Sona draft it."
            }
            Self::ProjectUpdateEdit(args) if args.health.is_none() && args.body.is_none() => {
                "Name a change: --health or --body."
            }
            Self::ProjectUpdateEdit(args)
                if args.body.as_deref().is_some_and(|body| body.trim().is_empty()) =>
            {
                "--body cannot be empty."
            }
            Self::CycleSettings(args) if !args.changes_something() => {
                "Name a change: --weeks, --start-weekday, --cooldown, --upcoming, --auto-add-started or --auto-add-completed."
            }
            Self::CycleSave(args) if args.name.is_none() && args.start.is_none() && args.end.is_none() => {
                "Name a change: --name, --start or --end."
            }
            Self::Ask(args) if args.take.contains(&0) => "Answer lines count from 1.",
            Self::Watch(args) if args.topic.join(" ").trim().is_empty() => {
                "Name a word or phrase to watch."
            }
            Self::Sources(verb) => return verb.check(),
            Self::Library(verb) => return verb.check(),
            Self::Transcript(verb) => return verb.check(),
            Self::Dictations(verb) => return verb.check(),
            _ => return Ok(()),
        };
        Err(ExternalError::invalid(refusal))
    }
}

impl UpdateArgs {
    fn changes_something(&self) -> bool {
        let fields = &self.fields;
        self.title.is_some()
            || fields.details.is_some()
            || fields.status.is_some()
            || fields.priority.is_some()
            || fields.owner.is_some()
            || fields.due.is_some()
            || fields.repeat.is_some()
            || fields.project.is_some()
            || fields.milestone.is_some()
            || fields.labels.is_some()
            || !self.add_label.is_empty()
            || !self.remove_label.is_empty()
            || self.snooze_until.is_some()
            || self.cycle.is_some()
            || self.parent.is_some()
            || self.estimate.is_some()
            || self.for_person.is_some()
    }
}

impl WriteArgs {
    /// The caller's id, or one derived from the verb, what it read and the
    /// agent named: the same request against the same revisions derives the
    /// same id.
    fn operation_id(&self, verb: &str, request: &impl Serialize) -> Result<String, ExternalError> {
        if let Some(id) = self.operation_id {
            return Ok(id.to_string());
        }
        let mut name = verb.as_bytes().to_vec();
        name.push(0);
        serde_json::to_writer(&mut name, request).map_err(|_| {
            ExternalError::new(ExternalErrorCode::Failed, "The change could not be named.")
        })?;
        if self.agent.is_some() {
            name.push(0);
            name.extend_from_slice(self.agent().as_bytes());
        }
        Ok(Uuid::new_v5(&OPERATION_NAMESPACE, &name).to_string())
    }

    /// The caller's id, or a new one. For creations a deleted twin could
    /// otherwise replay: a label or view made, deleted, and made again.
    fn fresh_operation_id(&self) -> String {
        self.operation_id.unwrap_or_else(Uuid::new_v4).to_string()
    }

    /// The agent making the change, by the name Sona shows.
    fn agent(&self) -> String {
        agents::agent_name(self.agent.as_deref())
    }
}

/// One `sona work` answer. Serialised untagged, like the corpus answers.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum WorkAnswer {
    Page(ExternalWorkPage),
    Timeline(timeline::ExternalWorkTimeline),
    Insights(insights::ExternalWorkInsights),
    Mine(ExternalWorkMine),
    Counts(ExternalWorkCountsPage),
    Detail(Box<ExternalWorkDetail>),
    Change(ExternalWorkChange),
    Preview(ExternalWorkPreview),
    Relation(ExternalWorkRelationChange),
    Labels(ExternalWorkLabels),
    Label(ExternalWorkLabelChange),
    LabelEdit(routines::ExternalWorkLabelEdit),
    Templates(routines::ExternalWorkTemplates),
    Template(Box<routines::ExternalWorkTemplateChange>),
    Reminders(routines::ExternalWorkReminders),
    Reminder(Box<routines::ExternalWorkReminderChange>),
    AgentLoops(agent_loops::ExternalAgentLoops),
    AgentLoop(Box<agent_loops::ExternalAgentLoop>),
    AgentLoopChange(Box<agent_loops::ExternalAgentLoopChange>),
    AgentLoopRun(Box<agent_loops::ExternalAgentLoopRun>),
    Views(ExternalWorkViews),
    View(Box<ExternalWorkViewChange>),
    Export(ExternalWorkExport),
    FollowUp(ExternalWorkFollowUp),
    Cycles(ExternalWorkCycles),
    Cycle(Box<ExternalWorkCycleProgress>),
    Lines(ExternalWorkLines),
    Watches(ExternalWorkWatches),
    Projects(catalog::ExternalWorkProjects),
    Project(catalog::ExternalWorkProjectChange),
    Milestones(catalog::ExternalWorkMilestones),
    Milestone(catalog::ExternalWorkMilestoneChange),
    People(catalog::ExternalWorkPeople),
    Person(catalog::ExternalWorkPersonChange),
    Meetings(catalog::ExternalWorkMeetings),
    MeetingDetail(Box<catalog::ExternalWorkMeetingDetail>),
    Comments(threads::ExternalWorkComments),
    Comment(Box<threads::ExternalWorkCommentChange>),
    Attachments(attachments::ExternalWorkAttachments),
    Attachment(Box<attachments::ExternalWorkAttachmentChange>),
    ProjectDetail(Box<roadmap::ExternalWorkProjectDetail>),
    ProjectDependencies(dependencies::ExternalWorkProjectDependencies),
    ProjectDependency(Box<dependencies::ExternalWorkProjectDependencyChange>),
    Initiatives(initiatives::ExternalWorkInitiatives),
    InitiativeDetail(Box<initiatives::ExternalWorkInitiativeDetail>),
    InitiativeChange(Box<initiatives::ExternalWorkInitiativeChange>),
    ProjectTemplates(project_templates::ExternalWorkProjectTemplates),
    ProjectTemplate(Box<project_templates::ExternalWorkProjectTemplateChange>),
    ProjectNew(Box<project_templates::ExternalWorkProjectNew>),
    ProjectUpdates(roadmap::ExternalWorkProjectUpdates),
    ProjectUpdate(Box<roadmap::ExternalWorkProjectUpdateChange>),
    ProjectDraft(roadmap::ExternalWorkProjectDraft),
    MilestoneOrder(roadmap::ExternalWorkMilestoneOrder),
    CycleChange(Box<roadmap::ExternalWorkCycleChange>),
    Organize(Box<organize::ExternalOrganizeChange>),
    Speaker(Box<speakers::ExternalSpeakerChange>),
    PersonPage(Box<people::ExternalPersonPage>),
    Companies(people::ExternalCompanies),
    Company(Box<people::ExternalCompany>),
    FirstMet(Box<people::ExternalFirstMet>),
    PeopleChange(Box<people::ExternalPeopleChange>),
    SpeakerVoices(Box<voices::ExternalSpeakerVoices>),
    VoiceChange(Box<voices::ExternalVoiceChange>),
    Sources(Box<sources::ExternalSourcesAnswer>),
    Library(Box<library::ExternalLibraryAnswer>),
    Transcript(Box<transcript::ExternalTranscriptAnswer>),
    Dictations(Box<dictations::ExternalDictationAnswer>),
    Day(organize::ExternalWorkDay),
    Brief(Box<organize::ExternalWorkBrief>),
    Note(Box<notes::ExternalNoteChange>),
    Subjects(Box<subjects::ExternalSubjects>),
    Planner(Box<planner::PlannerAnswer>),
    TriageHints(Box<triage_hints::TriageHintsAnswer>),
    ProjectAdmin(Box<catalog::ProjectAdminAnswer>),
    Settings(Box<preferences::ExternalSettings>),
    Changes(save::ExternalWorkChanges),
    Sessions(sessions::ExternalWorkSessions),
    Session(Box<sessions::ExternalWorkSessionChange>),
    Lifecycle(Box<lifecycle::ExternalLifecycleChange>),
    Archive(lifecycle::ExternalWorkArchive),
    Versions(lifecycle::ExternalWorkVersions),
    Favorites(lifecycle::ExternalWorkFavorites),
    Subscriptions(lifecycle::ExternalWorkSubscriptions),
    Inbox(lifecycle::ExternalWorkInbox),
    Digest(Box<digest::ExternalWorkDigest>),
}

/// What `ask` found: the lines that answer, numbered for --take.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkLines {
    pub schema_version: u32,
    pub question: String,
    /// What the question was read as: the words, the person, the time.
    pub read_as: Vec<String>,
    pub lines: Vec<ExternalWorkLine>,
    /// The tasks --take made.
    pub taken: Vec<ExternalWorkItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkLine {
    pub number: usize,
    pub quote: String,
    pub said_by: Option<String>,
    /// The meeting or dictation it is from.
    pub from: String,
    /// The local day it was said, YYYY-MM-DD.
    pub said_on: Option<String>,
    pub link: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkWatches {
    pub schema_version: u32,
    pub watches: Vec<ExternalWorkWatch>,
    /// True when this answer is the watch just removed.
    pub removed: bool,
    /// True when nothing was saved or stopped.
    pub dry_run: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkWatch {
    pub id: String,
    pub topic: String,
    pub person: Option<String>,
    /// The last times it came up, newest first.
    pub heard: Vec<ExternalWorkLine>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkItem {
    pub id: String,
    /// `S-<number>`, accepted anywhere an id is.
    pub handle: String,
    pub number: i64,
    pub title: String,
    pub details: Option<String>,
    pub status: WorkStatus,
    pub priority: Priority,
    pub owner: ExternalWorkOwner,
    /// The agent working on it beside its owner, when one is.
    pub doer: Option<String>,
    pub direction: WorkDirection,
    /// The local day it is due, YYYY-MM-DD.
    pub due_date: Option<String>,
    pub due_at_ms: Option<i64>,
    pub project: Option<ExternalWorkName>,
    pub milestone_id: Option<String>,
    pub labels: Vec<ExternalWorkName>,
    /// The meeting commitment the task came from.
    pub source: Option<ExternalWorkSource>,
    pub duplicate_of_id: Option<String>,
    /// Open tasks that block this one.
    pub blocked_by_open: u32,
    pub snoozed_until_ms: Option<i64>,
    /// When to ask the person who owes it about it; none for work nobody
    /// else owes.
    pub follow_up_at_ms: Option<i64>,
    /// Picked as one of today's focus tasks.
    pub focus: bool,
    pub archived_at_ms: Option<i64>,
    pub status_changed_at_ms: i64,
    /// When it was marked done; none while it is not done.
    pub completed_at_ms: Option<i64>,
    /// Points of effort, 0 to 64.
    pub estimate: Option<u8>,
    /// The person the work is for, who is owed it.
    pub for_person: Option<ExternalWorkName>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub revision: i64,
    /// The cycle it is planned in, by number.
    pub cycle: Option<i64>,
    /// Mine, due within two days, nothing booked for it, and no free hour
    /// left on its due day.
    pub at_risk: bool,
    /// The calendar hour booked to do it.
    pub time_block: Option<ExternalWorkBlock>,
    /// How it repeats, and of its instances due so far how many were done
    /// by the end of their day.
    pub repeat: Option<WorkRepeat>,
    /// The task it is a sub-task of, when the caller can see it.
    pub parent: Option<ExternalWorkRef>,
    /// Its sub-tasks done out of all of them, canceled and duplicate ones
    /// left out; none when it has none.
    pub sub_tasks: Option<WorkProgress>,
    /// Files, images and links on it.
    pub attachment_count: u32,
    /// Opens the task in Sona: `sona://task/<id>`.
    pub link: String,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ExternalWorkBlock {
    pub starts_at_ms: i64,
    pub ends_at_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExternalWorkOwner {
    #[serde(rename = "self")]
    Myself,
    Person {
        person_id: String,
        name: Option<String>,
    },
    Nobody,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkName {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkSource {
    pub loop_id: String,
    pub link: String,
    pub meeting_id: Option<String>,
    pub meeting_title: Option<String>,
    pub quote: Option<String>,
    pub speaker: Option<String>,
    /// Offset into the meeting where it was said.
    pub at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkPage {
    pub schema_version: u32,
    pub entries: PageTasks,
    /// Matching tasks before `--limit`.
    pub total: usize,
    pub has_more: bool,
    /// Pass as --cursor for the next page; none on the last.
    pub next_cursor: Option<String>,
    /// False while the conversations can't be opened: work from meetings is
    /// left out.
    pub conversations_available: bool,
    /// What `--where` was read as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_as: Option<words::ExternalWorkWhere>,
}

/// A page's tasks, each trimmed to the fields asked for.
#[derive(Clone, Debug)]
pub struct PageTasks {
    pub tasks: Vec<ExternalWorkItem>,
    /// The fields to keep beside id and handle; every field when empty.
    pub fields: Vec<TaskField>,
}

impl Serialize for PageTasks {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error as _, SerializeSeq as _};
        if self.fields.is_empty() {
            return self.tasks.serialize(serializer);
        }
        let mut list = serializer.serialize_seq(Some(self.tasks.len()))?;
        for task in &self.tasks {
            let mut shown = serde_json::to_value(task).map_err(S::Error::custom)?;
            if let serde_json::Value::Object(fields) = &mut shown {
                fields.retain(|key, _| {
                    key == "id"
                        || key == "handle"
                        || self.fields.iter().any(|field| field.names(key))
                });
            }
            list.serialize_element(&shown)?;
        }
        list.end()
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ExternalWorkCounts {
    pub triage: u32,
    pub mine_open: u32,
    pub mine_overdue: u32,
    pub mine_due_today: u32,
    pub waiting_on: u32,
    /// Waiting-on work whose follow-up moment has come.
    pub waiting_on_stale: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkCountsPage {
    pub schema_version: u32,
    #[serde(flatten)]
    pub counts: ExternalWorkCounts,
    /// False while the conversations can't be opened: work from meetings is
    /// not counted.
    pub conversations_available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkMine {
    pub schema_version: u32,
    pub counts: ExternalWorkCounts,
    pub i_owe: Vec<ExternalWorkItem>,
    pub waiting_on: Vec<ExternalWorkItem>,
    pub has_more: bool,
    /// False while the conversations can't be opened: work from meetings is
    /// left out.
    pub conversations_available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkDetail {
    pub schema_version: u32,
    pub task: ExternalWorkItem,
    /// How often the task repeats, in the parser's words: "every monday".
    pub recurrence: Option<String>,
    pub relations: Vec<ExternalWorkLink>,
    pub evidence: Vec<ExternalPlanningEvidence>,
    /// What Sona suggests for a task still in triage.
    pub suggestions: Vec<ExternalWorkSuggestion>,
    pub activity: Vec<ExternalWorkActivity>,
    /// Changes agents proposed that wait for the user, oldest first.
    pub proposals: Vec<ExternalWorkProposal>,
    pub parent: Option<ExternalWorkRef>,
    /// Its sub-tasks the caller can see, archived ones left out.
    pub sub_tasks: Vec<ExternalWorkItem>,
    /// Every comment thread, oldest first.
    pub comments: Vec<threads::ExternalWorkComment>,
    /// Files, images and links, oldest first.
    pub attachments: Vec<attachments::ExternalWorkAttachment>,
    /// When Sona reminds the user of it, if a reminder is set.
    pub reminder: Option<routines::ExternalWorkReminder>,
    /// Agent sessions working on it, newest first.
    pub sessions: Vec<sessions::ExternalWorkSession>,
    /// False while the conversations can't be opened: meeting quotes and
    /// suggestions are left out.
    pub conversations_available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkLink {
    pub id: String,
    /// How the other task stands to this one.
    pub kind: ExternalLinkKind,
    pub task: ExternalWorkRef,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalLinkKind {
    Blocks,
    BlockedBy,
    Related,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkRef {
    pub id: String,
    pub handle: String,
    pub title: String,
    pub status: WorkStatus,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkSuggestion {
    pub suggestion_id: String,
    pub field: WorkSuggestionField,
    /// The suggestion as a person reads it: a name, a day, a title.
    pub value: String,
    /// The person, project or task suggested, when it is one.
    pub id: Option<String>,
    pub due_at_ms: Option<i64>,
    pub reason: String,
    pub quote: Option<String>,
    pub meeting_id: Option<String>,
    pub at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkActivity {
    pub at_ms: i64,
    pub actor: WorkActor,
    pub kind: WorkActivityKind,
    pub from: Option<String>,
    pub to: Option<String>,
    /// The agent behind an agent's change, when it gave its name.
    pub agent: Option<String>,
    /// How the agent reached Sona: mcp or cli. None for the user and Sona.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<WorkVia>,
    /// The meeting a task heard there came from, on its created entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meeting_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meeting_title: Option<String>,
    /// Why, when one was given: a decline's reason.
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkAction {
    Created,
    Updated,
    Claimed,
    Done,
    Accepted,
    Declined,
    MarkedDuplicate,
    Archived,
    Restored,
    Snoozed,
    Moved,
}

impl WorkAction {
    const fn verb(self) -> &'static str {
        match self {
            Self::Created => "create",
            Self::Updated => "update",
            Self::Claimed => "claim",
            Self::Done => "done",
            Self::Accepted => "accept",
            Self::Declined => "decline",
            Self::MarkedDuplicate => "duplicate",
            Self::Archived => "archive",
            Self::Restored => "restore",
            Self::Snoozed => "snooze",
            Self::Moved => "move",
        }
    }

    const fn heading(self) -> &'static str {
        match self {
            Self::Created => "Created",
            Self::Updated => "Updated",
            Self::Claimed => "Claimed: in progress, the owner unchanged",
            Self::Done => "Done",
            Self::Accepted => "Accepted",
            Self::Declined => "Declined",
            Self::MarkedDuplicate => "Marked duplicate",
            Self::Archived => "Archived",
            Self::Restored => "Restored",
            Self::Snoozed => "Snoozed",
            Self::Moved => "Moved in the manual order",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkChange {
    pub schema_version: u32,
    pub action: WorkAction,
    /// Absent when nothing needed changing.
    pub operation_id: Option<String>,
    pub tasks: Vec<ExternalWorkItem>,
    pub skipped: Vec<ExternalWorkSkip>,
    /// Changes left waiting for the user to accept them in Sona.
    pub proposed: Vec<ExternalWorkProposal>,
    /// True when this create answered from an earlier run of the same
    /// request rather than creating another task.
    pub replayed: bool,
    /// The link kept with the task as evidence of the work, when one was.
    pub evidence_url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkSkip {
    pub id: String,
    pub handle: String,
    pub reason: String,
}

/// A change an agent proposed, both values as a person reads them.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkProposal {
    pub id: String,
    pub task_id: String,
    pub handle: String,
    pub agent: String,
    pub field: WorkProposalField,
    pub from: String,
    pub to: String,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkPreview {
    pub schema_version: u32,
    pub dry_run: bool,
    pub action: WorkAction,
    /// The id the same command would write under, unless the tasks change
    /// first.
    pub operation_id: Option<String>,
    pub changes: Vec<ExternalWorkPlanned>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkPlanned {
    pub id: String,
    pub handle: String,
    pub title: String,
    pub fields: Vec<ExternalFieldChange>,
    pub commitment: Option<CommitmentEffect>,
    /// Why this task would be left alone.
    pub skipped: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalFieldChange {
    pub field: &'static str,
    pub from: String,
    pub to: String,
}

/// What a change does to the meeting commitment a task came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentEffect {
    Closes,
    SetsAside,
    Reopens,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkRelationChange {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: String,
    pub removed: bool,
    pub relation: ExternalWorkRelation,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkRelation {
    pub id: String,
    pub kind: WorkRelationKind,
    pub from: ExternalWorkRef,
    pub to: ExternalWorkRef,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkLabels {
    pub schema_version: u32,
    pub labels: Vec<ExternalWorkLabel>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkLabel {
    pub id: String,
    pub name: String,
    pub hue: f64,
    pub revision: i64,
    /// The label group it sits in.
    pub group: Option<String>,
    /// Set once retired: its tasks keep it, pickers leave it out.
    pub retired_at_ms: Option<i64>,
    /// What it may go on: all, tasks or projects.
    pub scope: crate::planning::LabelScope,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkLabelChange {
    pub schema_version: u32,
    pub dry_run: bool,
    /// Absent when the label already read this way.
    pub operation_id: Option<String>,
    pub deleted: bool,
    pub label: ExternalWorkLabel,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkViews {
    pub schema_version: u32,
    pub views: Vec<ExternalWorkView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkView {
    pub id: String,
    pub name: String,
    pub filters: ExternalWorkFilters,
    pub layout: WorkLayout,
    pub group_by: WorkGroupBy,
    pub order_by: WorkOrderBy,
    pub show_empty_groups: bool,
    pub properties: Vec<String>,
    pub show_sub_tasks: bool,
    pub sub_group_by: WorkGroupBy,
    pub hidden_columns: Vec<String>,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkFilters {
    pub statuses: Option<Vec<WorkStatus>>,
    pub direction: Option<WorkDirection>,
    pub owner_person_ids: Option<Vec<String>>,
    pub project_ids: Option<Vec<String>>,
    pub label_ids: Option<Vec<String>>,
    pub priorities: Option<Vec<Priority>>,
    pub due_before_ms: Option<i64>,
    /// Due windows, as --due takes them.
    pub due: Option<Vec<String>>,
    pub meeting_id: Option<String>,
    pub search: Option<String>,
    pub include_archived: bool,
    pub include_snoozed: bool,
    pub cycle: Option<WorkCycleRef>,
    pub conditions: Vec<WorkCondition>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkViewChange {
    pub schema_version: u32,
    pub dry_run: bool,
    pub operation_id: String,
    pub deleted: bool,
    pub view: ExternalWorkView,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkExport {
    pub schema_version: u32,
    pub directory: String,
    pub files: Vec<String>,
    /// False while the conversations can't be opened: work from meetings is
    /// not written.
    pub conversations_available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkFollowUp {
    pub schema_version: u32,
    pub meeting_id: String,
    /// The message, ready to paste or send. Empty when nobody owes anything.
    pub text: String,
    pub lines: Vec<ExternalWorkFollowUpLine>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkFollowUpLine {
    pub id: String,
    pub owner: String,
    pub title: String,
    pub due_at_ms: Option<i64>,
    pub quote: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkCycles {
    pub schema_version: u32,
    /// How many weeks each cycle runs.
    pub weeks: u8,
    /// The weekday each starts on, 1 for Monday through 7 for Sunday.
    pub start_weekday: u8,
    /// Weeks off between one cycle and the next.
    pub cooldown_weeks: u8,
    /// How many cycles are made ahead.
    pub upcoming: u8,
    /// Started work with no cycle joins one by itself.
    pub auto_add_started: bool,
    /// Done work with no cycle joins one by itself.
    pub auto_add_completed: bool,
    /// The tasks a cycle usually finishes: the mean done of the last three
    /// ended cycles. None until that mean reaches one task.
    pub capacity: Option<u32>,
    pub cycles: Vec<ExternalWorkCycle>,
    /// With --ics, every cycle as an iCalendar file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkCycle {
    pub number: i64,
    pub name: Option<String>,
    pub state: WorkCycleState,
    /// The local days it runs, YYYY-MM-DD, both included.
    pub first_day: String,
    pub last_day: String,
    /// Tasks planned in it and not dropped.
    pub scope: u32,
    pub started: u32,
    pub done: u32,
    /// Done counts fully and started a quarter, out of scope; as it closed
    /// for a past cycle.
    pub success: Option<u8>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkCycleProgress {
    pub schema_version: u32,
    pub cycle: ExternalWorkCycle,
    /// Tasks moved in unfinished from earlier cycles.
    pub carried_over: u32,
    /// Scope, started and done as each day ended, through today; frozen
    /// once the cycle ended.
    pub days: Vec<ExternalWorkCycleDay>,
    /// Where an even pace would have the done count at each day's end, for
    /// every day of the cycle; flat on weekends.
    pub target: Vec<ExternalWorkCycleTarget>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkCycleTarget {
    pub day: String,
    pub done: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalWorkCycleDay {
    pub day: String,
    pub scope: u32,
    pub started: u32,
    pub done: u32,
}

/// Answer one allowed `sona work` verb.
pub(super) async fn answer(
    verb: &WorkVerb,
    app: &AppHandle,
    meetings: &Arc<MeetingSessionManager>,
    history: &HistoryManager,
    calendar: &dyn crate::meeting::detection::calendar::CalendarSource,
) -> Result<WorkAnswer, ExternalError> {
    let tracker = Tracker {
        app,
        meetings,
        history,
    };
    match verb {
        WorkVerb::List(args) => tracker.list(args).await,
        WorkVerb::Ready(args) => tracker.ready(args).await,
        WorkVerb::Mine(args) => tracker.mine(args).await,
        WorkVerb::Counts => tracker.counts().await,
        WorkVerb::Cycles(args) => tracker.cycles(args),
        WorkVerb::Cycle(args) => tracker.cycle(args),
        WorkVerb::Show(args) => tracker.show(args).await,
        WorkVerb::Create(args) => tracker.create(args).await,
        WorkVerb::Update(args) => tracker.update(args).await,
        WorkVerb::Claim(args) => tracker.claim(args).await,
        WorkVerb::Done(args) => tracker.done(args).await,
        WorkVerb::Accept(args) => {
            tracker
                .triage(
                    &args.tasks,
                    Decision::Accept {
                        apply_suggestions: !args.ignore_suggestions,
                    },
                    args.dry_run,
                    &args.write,
                )
                .await
        }
        WorkVerb::Decline(args) => {
            tracker
                .triage(
                    &args.tasks,
                    Decision::Decline {
                        reason: args.reason.as_deref(),
                    },
                    args.dry_run,
                    &args.write,
                )
                .await
        }
        WorkVerb::Duplicate(args) => tracker.duplicate(args).await,
        WorkVerb::Archive(args) => tracker.archive(args, true).await,
        WorkVerb::Restore(args) => tracker.restore(args).await,
        WorkVerb::Move(args) => tracker.move_tasks(args).await,
        WorkVerb::Delete(args) => tracker.delete(args).await,
        WorkVerb::Bin => tracker.archive_list(true).await,
        WorkVerb::ArchiveList => tracker.archive_list(false).await,
        WorkVerb::DescriptionVersions(args) => tracker.description_versions(args).await,
        WorkVerb::DescriptionVersion(args) => tracker.description_version(args).await,
        WorkVerb::DescriptionRestore(args) => tracker.description_restore(args).await,
        WorkVerb::Favorite(args) => tracker.favorite(args, true).await,
        WorkVerb::Unfavorite(args) => tracker.favorite(args, false).await,
        WorkVerb::Favorites => tracker.favorites().await,
        WorkVerb::FavoriteFolderSave(args) => tracker.favorite_folder_save(args).await,
        WorkVerb::FavoriteFolderDelete(args) => tracker.favorite_folder_delete(args).await,
        WorkVerb::FavoriteMove(args) => tracker.favorite_move(args).await,
        WorkVerb::Subscribe(args) => tracker.subscribe(args, true).await,
        WorkVerb::Unsubscribe(args) => tracker.subscribe(args, false).await,
        WorkVerb::Subscriptions => tracker.subscriptions().await,
        WorkVerb::Inbox(args) => tracker.inbox(args).await,
        WorkVerb::InboxMark(args) => tracker.inbox_mark(args).await,
        WorkVerb::Digest(args) => tracker.digest(args).await,
        WorkVerb::Snooze(args) => tracker.snooze(args).await,
        WorkVerb::Relate(args) => tracker.relate(args).await,
        WorkVerb::Unrelate(args) => tracker.unrelate(args).await,
        WorkVerb::Labels => tracker.labels(),
        WorkVerb::LabelSave(args) => tracker.label_save(args),
        WorkVerb::LabelDelete(args) => tracker.label_delete(args),
        WorkVerb::LabelGroup(args) => tracker.label_group(args),
        WorkVerb::LabelRetire(args) => tracker.label_retire(args),
        WorkVerb::LabelScope(args) => tracker.label_scope(args),
        WorkVerb::Templates => tracker.templates().await,
        WorkVerb::TemplateSave(args) => tracker.template_save(args).await,
        WorkVerb::TemplateDelete(args) => tracker.template_delete(args).await,
        WorkVerb::TemplateUse(args) => tracker.template_use(args).await,
        WorkVerb::LoopList => tracker.loop_list(),
        WorkVerb::LoopShow(args) => tracker.loop_show(args),
        WorkVerb::LoopSave(args) => tracker.loop_save(args).await,
        WorkVerb::LoopRun(args) => tracker.loop_run(args),
        WorkVerb::LoopEnable(args) => tracker.loop_enable(args, true),
        WorkVerb::LoopDisable(args) => tracker.loop_enable(args, false),
        WorkVerb::LoopRestoreVersion(args) => tracker.loop_restore(args),
        WorkVerb::LoopDelete(args) => tracker.loop_delete(args),
        WorkVerb::Remind(args) => tracker.remind(args).await,
        WorkVerb::Reminders(args) => tracker.reminders(args).await,
        WorkVerb::Projects(args) => tracker.projects(args).await,
        WorkVerb::ProjectSave(args) => tracker.project_save(args).await,
        WorkVerb::ProjectArchive(args) => tracker.project_archive(args).await,
        WorkVerb::Milestones(args) => tracker.milestones(args).await,
        WorkVerb::Timeline(args) => tracker.timeline(args).await,
        WorkVerb::Insights(args) => tracker.insights(args).await,
        WorkVerb::MilestoneSave(args) => tracker.milestone_save(args).await,
        WorkVerb::MilestoneDelete(args) => tracker.milestone_delete(args).await,
        WorkVerb::Milestone(args) => tracker.milestone(args).await,
        WorkVerb::MilestoneToProject(args) => tracker.milestone_to_project(args).await,
        WorkVerb::ProjectRelate(args) => tracker.project_relate(args),
        WorkVerb::ProjectUnrelate(args) => tracker.project_unrelate(args),
        WorkVerb::ProjectDependencies(args) => tracker.project_dependencies(args),
        WorkVerb::InitiativeList => tracker.initiative_list().await,
        WorkVerb::InitiativeShow(args) => tracker.initiative_show(args).await,
        WorkVerb::InitiativeNew(args) => tracker.initiative_new(args).await,
        WorkVerb::InitiativeUpdate(args) => tracker.initiative_update(args).await,
        WorkVerb::InitiativeDelete(args) => tracker.initiative_delete(args).await,
        WorkVerb::InitiativeAddProject(args) => tracker.initiative_add_project(args).await,
        WorkVerb::InitiativeRemoveProject(args) => tracker.initiative_remove_project(args).await,
        WorkVerb::ProjectTemplateList => tracker.project_template_list().await,
        WorkVerb::ProjectTemplateSave(args) => tracker.project_template_save(args).await,
        WorkVerb::ProjectTemplateDelete(args) => tracker.project_template_delete(args).await,
        WorkVerb::ProjectNew(args) => tracker.project_new(args).await,
        WorkVerb::ProjectStatuses => tracker.project_statuses(),
        WorkVerb::ProjectStatusSave(args) => tracker.project_status_save(args),
        WorkVerb::ProjectStatusDelete(args) => tracker.project_status_delete(args),
        WorkVerb::ProjectHistory(args) => tracker.project_history(args).await,
        WorkVerb::People(args) => tracker.people(args).await,
        WorkVerb::PersonSave(args) => tracker.person_save(args).await,
        WorkVerb::PersonMerge(args) => tracker.person_merge(args).await,
        WorkVerb::Person(args) => tracker.person_page(args).await,
        WorkVerb::PersonLink(args) => tracker.person_link(args).await,
        WorkVerb::PersonSplit(args) => tracker.person_split(args).await,
        WorkVerb::PersonDelete(args) => tracker.person_delete(args).await,
        WorkVerb::PersonPhoto(args) => tracker.person_photo(args).await,
        WorkVerb::PersonContactRemove(args) => tracker.person_contact_remove(args).await,
        WorkVerb::FirstMet(args) => tracker.first_met(args).await,
        WorkVerb::FirstMetAnswer(args) => tracker.first_met_answer(args).await,
        WorkVerb::Companies(args) => tracker.companies(args).await,
        WorkVerb::Company(args) => tracker.company(args).await,
        WorkVerb::Meetings(args) => tracker.meetings(args).await,
        WorkVerb::Meeting(args) => tracker.meeting(args).await,
        WorkVerb::Views => tracker.views(),
        WorkVerb::ViewSave(args) => tracker.view_save(args).await,
        WorkVerb::ViewDelete(args) => tracker.view_delete(args),
        WorkVerb::Export(args) => tracker.export(args).await,
        WorkVerb::FollowUp(args) => tracker.follow_up(args).await,
        WorkVerb::Ask(args) => tracker.ask(args).await,
        WorkVerb::Say(args) => tracker.say(args).await,
        WorkVerb::Watches => tracker.watches().await,
        WorkVerb::Watch(args) => tracker.watch(args).await,
        WorkVerb::Unwatch(args) => tracker.unwatch(args).await,
        WorkVerb::Comments(args) => tracker.comments(args).await,
        WorkVerb::Comment(threads::CommentCommand { verb }) => match verb {
            threads::CommentVerb::Add(args) => tracker.comment_add(args).await,
            threads::CommentVerb::Edit(args) => tracker.comment_edit(args).await,
            threads::CommentVerb::Delete(args) => tracker.comment_delete(args).await,
            threads::CommentVerb::Resolve(args) => tracker.comment_resolve(args).await,
        },
        WorkVerb::CommentEdit(args) => tracker.comment_edit(args).await,
        WorkVerb::CommentDelete(args) => tracker.comment_delete(args).await,
        WorkVerb::CommentResolve(args) => tracker.comment_resolve(args).await,
        WorkVerb::Subtask(args) => tracker.subtask(args).await,
        WorkVerb::Attach(args) => tracker.attach(args).await,
        WorkVerb::Attachments(args) => tracker.attachments(args).await,
        WorkVerb::Detach(args) => tracker.detach(args).await,
        WorkVerb::Project(args) => tracker.project(args).await,
        WorkVerb::ProjectUpdate(args) => tracker.project_update(args).await,
        WorkVerb::ProjectUpdates(args) => tracker.project_updates(args).await,
        WorkVerb::ProjectUpdateEdit(args) => tracker.project_update_edit(args).await,
        WorkVerb::ProjectUpdateDelete(args) => tracker.project_update_delete(args),
        WorkVerb::ProjectUpdateDraft(args) => tracker.project_update_draft(args).await,
        WorkVerb::ProjectDelete(args) => tracker.project_delete(args).await,
        WorkVerb::MilestoneReorder(args) => tracker.milestone_reorder(args),
        WorkVerb::CycleSettings(args) => tracker.cycle_settings(args),
        WorkVerb::CycleSave(args) => tracker.cycle_save(args),
        WorkVerb::CycleRollover(args) => tracker.cycle_rollover(args),
        WorkVerb::CycleStartToday(args) => tracker.cycle_start_today(args),
        WorkVerb::Delegate(args) => tracker.delegate(args).await,
        WorkVerb::Day(args) => tracker.day(args).await,
        WorkVerb::Focus(args) => tracker.focus(args).await,
        WorkVerb::Keep(args) => tracker.keep(args).await,
        WorkVerb::Brief(args) => tracker.brief(args).await,
        WorkVerb::TalkingPoint(args) => tracker.talking_point(args).await,
        WorkVerb::TalkingPointDone(args) => tracker.talking_point_done(args),
        WorkVerb::LoopReopen(args) => tracker.loop_reopen(args).await,
        WorkVerb::LoopOwner(args) => tracker.loop_owner(args).await,
        WorkVerb::SpeakerAssign(args) => tracker.speaker_assign(args).await,
        WorkVerb::SpeakerVoices(args) => tracker.speaker_voices(args).await,
        WorkVerb::SpeakerConfirm(args) => tracker.speaker_confirm(args).await,
        WorkVerb::SpeakerVoiceUndo(args) => tracker.speaker_voice_undo(args).await,
        WorkVerb::ActionTrack(args) => tracker.action_track(args).await,
        WorkVerb::ActionDismiss(args) => tracker.action_set_aside(args, true).await,
        WorkVerb::ActionRestore(args) => tracker.action_set_aside(args, false).await,
        WorkVerb::NoteNew(args) => tracker.note_new(args).await,
        WorkVerb::Settings(verb) => tracker.settings_verb(verb).await,
        WorkVerb::Sources(verb) => tracker.sources(verb).await,
        WorkVerb::Library(verb) => tracker.library(verb).await,
        WorkVerb::Transcript(verb) => tracker.transcript(verb).await,
        WorkVerb::Dictations(verb) => tracker.dictations(verb).await,
        WorkVerb::NoteEdit(args) => tracker.note_edit(args),
        WorkVerb::NotePin(args) => tracker.note_pin(args),
        WorkVerb::NoteLink(args) => tracker.note_link(args).await,
        WorkVerb::NoteUnlink(args) => tracker.note_unlink(args).await,
        WorkVerb::NoteDelete(args) => tracker.note_delete(args).await,
        WorkVerb::NoteRestore(args) => tracker.note_restore(args),
        WorkVerb::Subjects => tracker.subjects(),
        WorkVerb::SubjectNew(args) => tracker.subject_new(args),
        WorkVerb::SubjectNote(args) => tracker.subject_note(args),
        WorkVerb::Parse(args) => tracker.parse(args).await,
        WorkVerb::Book(args) => tracker.book(args).await,
        WorkVerb::Unbook(args) => tracker.unbook(args).await,
        WorkVerb::Nudged(args) => tracker.nudged(args).await,
        WorkVerb::WatchTask(args) => tracker.watch_task(args).await,
        WorkVerb::LinkSuggestions(args) => tracker.link_suggestions(args).await,
        WorkVerb::LinkAccept(args) => {
            tracker
                .link_decide(args, planner::LinkAction::Accepted)
                .await
        }
        WorkVerb::LinkDismiss(args) => {
            tracker
                .link_decide(args, planner::LinkAction::Dismissed)
                .await
        }
        WorkVerb::TriageSuggestions(args) => tracker.triage_suggestions(args).await,
        WorkVerb::SuggestionAccept(args) => {
            tracker
                .suggestion_decide(args, triage_hints::SuggestionAction::Accepted)
                .await
        }
        WorkVerb::SuggestionDismiss(args) => {
            tracker
                .suggestion_decide(args, triage_hints::SuggestionAction::Dismissed)
                .await
        }
        WorkVerb::Calendar(args) => tracker.calendar(args).await,
        WorkVerb::Trend(args) => tracker.trend(args).await,
        WorkVerb::Today => tracker.today(calendar).await,
        WorkVerb::PlanAccept(args) => tracker.plan_accept(args).await,
        WorkVerb::Save(args) => tracker.save(args).await,
        WorkVerb::Changes(args) => tracker.changes(args).await,
        WorkVerb::SessionStart(args) => tracker.session_start(args).await,
        WorkVerb::SessionActivity(args) => tracker.session_activity(args).await,
        WorkVerb::SessionPlan(args) => tracker.session_plan(args).await,
        WorkVerb::SessionHeartbeat(args) => tracker.session_heartbeat(args).await,
        WorkVerb::SessionStop(args) => tracker.session_stop(args).await,
        WorkVerb::SessionReply(args) => tracker.session_reply(args).await,
        WorkVerb::Session(args) => tracker.session_show(args).await,
        WorkVerb::Sessions(args) => tracker.sessions(args).await,
        WorkVerb::AttachmentRename(args) => tracker.attachment_rename(args).await,
    }
}

impl Tracker<'_> {
    async fn ask(&self, args: &AskArgs) -> Result<WorkAnswer, ExternalError> {
        let question = args.question.join(" ");
        let answer = work::ask::answer(self.app, QueryAudience::External, &question)
            .await
            .map_err(|message| refusal(self.history, message))?;
        let mut taken = Vec::new();
        if !args.take.is_empty() {
            let mut lines = Vec::with_capacity(args.take.len());
            for number in &args.take {
                let line = answer.lines.get(number - 1).ok_or_else(|| {
                    ExternalError::invalid(format!(
                        "There is no line {number}; the answer has {}.",
                        answer.lines.len()
                    ))
                })?;
                lines.push(WorkTakenLine {
                    quote: line.quote.clone(),
                    source: line.source.clone(),
                    person_id: line.person_id.clone(),
                });
            }
            let operation_id = args.write.operation_id("take", &(&question, &args.take))?;
            if args.dry_run {
                return self.take_preview(operation_id, &lines).await;
            }
            let started_ms = now_ms();
            let items =
                work::capture::take_lines_as(self.app, WorkActor::Agent, &operation_id, &lines)
                    .await
                    .map_err(|message| self.write_refusal(message, &operation_id, "These lines"))?;
            let ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
            self.credit(&args.write, &ids, started_ms)?;
            let board = self.board().await?;
            taken = items
                .into_iter()
                .map(|item| board.project_item(item))
                .collect();
        }
        Ok(WorkAnswer::Lines(ExternalWorkLines {
            schema_version: QUERY_SCHEMA_VERSION,
            question,
            read_as: answer.read_as,
            lines: answer
                .lines
                .iter()
                .enumerate()
                .map(|(index, line)| external_line(index + 1, line))
                .collect(),
            taken,
        }))
    }

    async fn say(&self, args: &SayArgs) -> Result<WorkAnswer, ExternalError> {
        let words = args.words.join(" ");
        let command = work::command::command_preview(self.history, self.meetings, &words, &[])
            .await
            .map_err(|message| refusal(self.history, message))?
            .ok_or_else(|| {
                ExternalError::invalid("That does not read as a change to tasks.").hint(
                    "Say it like: push everything not urgent to monday, or move Dana's tasks to next week.",
                )
            })?;
        let board = self.board().await?;
        let rows: Vec<&WorkItemRow> = command
            .items
            .iter()
            .filter_map(|row| board.row(&row.item.id))
            .collect();
        match command.change {
            WorkCommandChange::Archive => {
                let tasks = rows
                    .iter()
                    .map(|row| TaskRef::Id(row.item.id.clone()))
                    .collect();
                self.archive(
                    &TasksArgs {
                        tasks,
                        dry_run: args.dry_run,
                        write: args.write.clone(),
                    },
                    true,
                )
                .await
            }
            WorkCommandChange::Update { patch } => {
                let planned = rows.into_iter().map(|row| (row, patch.clone())).collect();
                self.apply(
                    &board,
                    WorkAction::Updated,
                    args.dry_run,
                    &args.write,
                    planned,
                    None,
                )
                .await
            }
        }
    }

    async fn watches(&self) -> Result<WorkAnswer, ExternalError> {
        let watches = work::watch::list(self.app)
            .await
            .map_err(|message| refusal(self.history, message))?;
        self.watches_answer(watches, false, false).await
    }

    async fn watch(&self, args: &WatchArgs) -> Result<WorkAnswer, ExternalError> {
        let person_id = match &args.person {
            Some(name) => Some(self.board().await?.person(name)?.id.clone()),
            None => None,
        };
        let topic = args.topic.join(" ");
        let watch = if args.dry_run {
            work::watch::save_preview(self.app, &topic, person_id).await
        } else {
            work::watch::save(self.app, &topic, person_id).await
        }
        .map_err(|message| refusal(self.history, message))?;
        self.watches_answer(vec![watch], false, args.dry_run).await
    }

    async fn unwatch(&self, args: &UnwatchArgs) -> Result<WorkAnswer, ExternalError> {
        let topic = args.topic.join(" ");
        let topic = topic.trim();
        let found = work::watch::list(self.app)
            .await
            .map_err(|message| refusal(self.history, message))?
            .into_iter()
            .find(|watch| watch.id == topic || watch.phrase.eq_ignore_ascii_case(topic))
            .ok_or_else(|| {
                ExternalError::new(
                    ExternalErrorCode::NotFound,
                    format!("Nothing watches \u{201C}{topic}\u{201D}."),
                )
                .hint("List them with: sona work watches")
            })?;
        if !args.dry_run {
            work::watch::remove(self.app, &found.id)
                .map_err(|message| refusal(self.history, message))?;
        }
        self.watches_answer(vec![found], true, args.dry_run).await
    }

    /// The tasks `ask --take` would make, or the ones already following
    /// those lines, with nothing written.
    async fn take_preview(
        &self,
        operation_id: String,
        lines: &[WorkTakenLine],
    ) -> Result<WorkAnswer, ExternalError> {
        let planned = work::capture::take_lines_preview(self.app, lines)
            .await
            .map_err(|message| self.write_refusal(message, &operation_id, "These lines"))?;
        let board = self.board().await?;
        let changes = planned
            .into_iter()
            .map(|line| match line {
                work::capture::TakenLine::Followed(item) => ExternalWorkPlanned {
                    handle: handle(item.number),
                    title: item.title,
                    id: item.id,
                    fields: Vec::new(),
                    commitment: None,
                    skipped: Some("this task already follows the line".to_owned()),
                },
                work::capture::TakenLine::New(input) => {
                    let mut fields = vec![ExternalFieldChange {
                        field: "status",
                        from: String::new(),
                        to: status_text(WorkStatus::Triage).to_owned(),
                    }];
                    let mut field = |field: &'static str, to: String| {
                        fields.push(ExternalFieldChange {
                            field,
                            from: String::new(),
                            to,
                        });
                    };
                    if let Some(owner) = &input.owner {
                        field("owner", board.owner_text(owner));
                    }
                    if let Some(priority) = input.priority {
                        field("priority", Priority::of(priority).text().to_owned());
                    }
                    if input.due_at_ms.is_some() {
                        field("due", day_text(input.due_at_ms));
                    }
                    if let Some(project) = &input.project_id {
                        field("project", board.project_name(project));
                    }
                    if let Some(labels) = input.label_ids.as_deref().filter(|ids| !ids.is_empty()) {
                        field("labels", board.labels_text(labels));
                    }
                    ExternalWorkPlanned {
                        id: String::new(),
                        handle: "new".to_owned(),
                        title: input.title,
                        fields,
                        commitment: None,
                        skipped: None,
                    }
                }
            })
            .collect();
        Ok(WorkAnswer::Preview(ExternalWorkPreview {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: true,
            action: WorkAction::Created,
            operation_id: Some(operation_id),
            changes,
        }))
    }

    /// Watches as an outside caller may see them: lines from meetings kept
    /// on this Mac are left out.
    async fn watches_answer(
        &self,
        watches: Vec<WorkWatch>,
        removed: bool,
        dry_run: bool,
    ) -> Result<WorkAnswer, ExternalError> {
        let fence = Fence::read(self.meetings).await?;
        Ok(WorkAnswer::Watches(ExternalWorkWatches {
            schema_version: QUERY_SCHEMA_VERSION,
            watches: watches
                .into_iter()
                .map(|watch| ExternalWorkWatch {
                    heard: watch
                        .hits
                        .iter()
                        .filter(|hit| !fence.hides_line(&hit.line))
                        .enumerate()
                        .map(|(index, hit)| external_line(index + 1, &hit.line))
                        .collect(),
                    id: watch.id,
                    topic: watch.phrase,
                    person: watch.person_name,
                })
                .collect(),
            removed,
            dry_run,
        }))
    }
}

fn external_line(number: usize, line: &WorkAnswerLine) -> ExternalWorkLine {
    ExternalWorkLine {
        number,
        quote: line.quote.clone(),
        said_by: line.speaker.clone(),
        from: line.title.clone(),
        said_on: iso_day(line.when_utc_ms),
        link: line.link.clone(),
    }
}

#[derive(Clone, Copy)]
enum Decision<'r> {
    Accept { apply_suggestions: bool },
    Decline { reason: Option<&'r str> },
}

struct Tracker<'a> {
    app: &'a AppHandle,
    meetings: &'a Arc<MeetingSessionManager>,
    history: &'a HistoryManager,
}

/// Every task an outside caller may see, with the names to show beside them.
struct Board {
    rows: Vec<WorkItemRow>,
    labels: Vec<WorkLabel>,
    projects: Vec<WorkProjectChoice>,
    people: Vec<WorkPersonChoice>,
    suggestions: BTreeMap<String, Vec<WorkFieldSuggestion>>,
    /// Today's focus picks, by id.
    focus: Vec<String>,
    /// How often each repeating task repeats, by id.
    repeats: HashMap<String, String>,
    fence: Fence,
    cycles: Vec<WorkCycle>,
}

impl Tracker<'_> {
    async fn board(&self) -> Result<Board, ExternalError> {
        let fence = Fence::read(self.meetings).await?;
        let list = self
            .listed(
                &fence,
                WorkQuery {
                    include_archived: true,
                    include_snoozed: true,
                    ..WorkQuery::default()
                },
            )
            .await?;
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let (focus, repeats) = planning::history(self.history, |conn| {
            Ok((items::day(conn, &today)?.focus_ids, items::repeats(conn)?))
        })
        .map_err(|message| refusal(self.history, message))?;
        Ok(Board {
            rows: list.items,
            labels: list.labels,
            projects: list.projects,
            people: list.people,
            suggestions: list.suggestions,
            focus,
            repeats,
            fence,
            cycles: list.cycles,
        })
    }

    async fn listed(&self, fence: &Fence, query: WorkQuery) -> Result<WorkList, ExternalError> {
        let mut list = work::work_list(self.app.state(), self.app.state(), query)
            .await
            .map_err(|message| refusal(self.history, message))?;
        let hidden = planning::history(self.history, |history| fence.hidden_tasks(history))
            .map_err(|message| refusal(self.history, message))?;
        list.items.retain(|row| !hidden.contains(&row.item.id));
        list.suggestions
            .retain(|id, _| list.items.iter().any(|row| row.item.id == *id));
        for suggestions in list.suggestions.values_mut() {
            suggestions.retain(|suggestion| !fence.hides_suggestion(suggestion));
        }
        Ok(list)
    }

    fn write_refusal(&self, message: String, operation_id: &str, subject: &str) -> ExternalError {
        match message.as_str() {
            "This item changed. Reload and try again." => ExternalError::invalid(format!(
                "{subject} changed while this ran. Read it again and retry."
            )),
            "This change was already used. Reload and try again." => ExternalError::invalid(
                format!(
                    "Operation id {operation_id} was already used for a different change. Pass a new --operation-id, or leave it out."
                ),
            ),
            _ => refusal(self.history, message),
        }
    }

    async fn list(&self, args: &ListArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let read_as = match &args.where_words {
            Some(text) => Some(self.where_words(text).await?),
            None => None,
        };
        let base = match &args.view {
            Some(view) => self.view_named(view)?.query,
            None => WorkQuery {
                statuses: Some(OPEN_AND_TRIAGE.to_vec()),
                ..WorkQuery::default()
            },
        };
        let mut query = args.filters.narrow(&board, base)?;
        if let Some(read_as) = &read_as {
            if args.view.is_none()
                && args.filters.status.is_empty()
                && (read_as.names(WorkFilterField::Status)
                    || read_as.names(WorkFilterField::Completed))
            {
                query.statuses = None;
            }
            query.filters.extend(read_as.conditions());
        }
        if let Some(delegate) = args.delegate.as_deref().map(str::trim) {
            let (op, value) = if delegate.eq_ignore_ascii_case("any") {
                (WorkFilterOp::IsNot, "none")
            } else {
                (WorkFilterOp::Is, delegate)
            };
            query.filters.push(WorkCondition {
                field: WorkFilterField::Delegate,
                op,
                values: vec![value.to_owned()],
            });
        }
        let mut rows = self.listed(&board.fence, query).await?.items;
        if !args.source.is_empty() {
            rows.retain(|row| {
                args.source
                    .iter()
                    .any(|source| source.source() == row.source)
            });
        }
        if args.stale {
            let cutoff = now_ms() - STALE_AFTER_MS;
            rows.retain(|row| row.item.status.is_open() && row.item.updated_at_ms < cutoff);
        }
        let mut listed = page(rows, &args.page, &board)?;
        listed.read_as = read_as;
        Ok(WorkAnswer::Page(listed))
    }

    async fn ready(&self, args: &ReadyArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let query = args.scope.narrow(
            &board,
            WorkQuery {
                statuses: Some(vec![WorkStatus::NotStarted]),
                ..WorkQuery::default()
            },
        )?;
        let mut rows = self.listed(&board.fence, query).await?.items;
        rows.retain(|row| row.blocked_by_open_count == 0);
        Ok(WorkAnswer::Page(page(rows, &args.page, &board)?))
    }

    async fn mine(&self, args: &MineArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let now = now_ms();
        let counts = board.counts(now)?;
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
        let (i_owe, more_owed) = board.open_for(WorkDirection::Mine, now, limit);
        let (waiting_on, more_waiting) = board.open_for(WorkDirection::WaitingOn, now, limit);
        Ok(WorkAnswer::Mine(ExternalWorkMine {
            schema_version: QUERY_SCHEMA_VERSION,
            counts,
            i_owe,
            waiting_on,
            has_more: more_owed || more_waiting,
            conversations_available: board.fence.is_open(),
        }))
    }

    async fn counts(&self) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        Ok(WorkAnswer::Counts(ExternalWorkCountsPage {
            schema_version: QUERY_SCHEMA_VERSION,
            counts: board.counts(now_ms())?,
            conversations_available: board.fence.is_open(),
        }))
    }

    async fn show(&self, args: &ShowArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let id = board.task(&args.task)?.item.id.clone();
        let detail = work::work_item(self.app.clone(), self.app.state(), self.app.state(), id)
            .await
            .map_err(|message| refusal(self.history, message))?;
        let mut answer = board.detail(detail);
        let task_id = answer.task.id.clone();
        answer.sessions = work::sessions::sessions_read(self.app, Some(&task_id), false)
            .map_err(|message| refusal(self.history, message))?
            .into_iter()
            .filter_map(|session| {
                let row = board.row(&session.item_id)?;
                Some(sessions::project(session, task_ref_of(row)))
            })
            .collect();
        let now = now_ms();
        answer.reminder = planning::history(self.history, |conn| work::routines::reminders(conn))
            .map_err(|message| refusal(self.history, message))?
            .into_iter()
            .find(|reminder| reminder.item_id == task_id)
            .and_then(|reminder| {
                let row = board.row(&reminder.item_id)?;
                Some(routines::reminder_of(row, reminder, now))
            });
        Ok(WorkAnswer::Detail(Box::new(answer)))
    }

    async fn create(&self, args: &CreateArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let fields = &args.fields;
        let input = WorkItemInput {
            title: args.title.trim().to_owned(),
            details: fields
                .details
                .as_deref()
                .map(str::trim)
                .filter(|details| !details.is_empty())
                .map(str::to_owned),
            status: fields.status.map(StatusArg::status),
            priority: fields.priority.map(Priority::value),
            project_id: fields
                .project
                .as_deref()
                .map(|project| board.project(project))
                .transpose()?
                .flatten()
                .map(|project| project.id.clone()),
            milestone_id: fields
                .milestone
                .as_deref()
                .map(milestone)
                .transpose()?
                .flatten(),
            owner: fields
                .owner
                .as_deref()
                .map(|owner| board.owner(owner))
                .transpose()?,
            due_at_ms: match fields.due.map(DayArg::ms).transpose()? {
                Some(due) => due,
                None => fields.repeat.as_ref().and_then(RepeatArg::next_at_ms),
            },
            label_ids: fields
                .labels
                .as_deref()
                .map(|labels| board.label_ids(labels))
                .transpose()?,
            recurrence: fields.repeat.as_ref().and_then(RepeatArg::every),
        };
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let source = args
            .source_url
            .as_deref()
            .map(|url| WorkCaptureSource::Link {
                url: url.trim().to_owned(),
                title: args.source_title.clone(),
            });
        let operation_id = args
            .write
            .operation_id("create", &(&input, today, &source))?;
        if args.dry_run {
            return Ok(WorkAnswer::Preview(ExternalWorkPreview {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                action: WorkAction::Created,
                operation_id: Some(operation_id),
                changes: vec![ExternalWorkPlanned {
                    id: String::new(),
                    handle: String::new(),
                    title: input.title,
                    fields: args
                        .source_url
                        .iter()
                        .map(|url| ExternalFieldChange {
                            field: "source",
                            from: String::new(),
                            to: url.trim().to_owned(),
                        })
                        .collect(),
                    commitment: None,
                    skipped: None,
                }],
            }));
        }
        let started_ms = now_ms();
        let item = agents::create_as(self.app, &args.write.agent(), &operation_id, &input)
            .await
            .map_err(|message| self.write_refusal(message, &operation_id, "The new task"))?;
        if let Some(source) = source {
            crate::planning::history(&self.app.state::<Arc<HistoryManager>>(), |conn| {
                capture::attach(conn, &item.id, &source, started_ms)
            })
            .map_err(|message| self.write_refusal(message, &operation_id, "The new task"))?;
            self.credit(&args.write, std::slice::from_ref(&item.id), started_ms)?;
        }
        let replayed = item.created_at_ms < started_ms;
        Ok(WorkAnswer::Change(ExternalWorkChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action: WorkAction::Created,
            operation_id: Some(operation_id),
            tasks: vec![board.project_item(item)],
            skipped: Vec::new(),
            proposed: Vec::new(),
            replayed,
            evidence_url: None,
        }))
    }

    async fn update(&self, args: &UpdateArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let targets = board.tasks(&args.tasks)?;
        if let (Some(expected), [row]) = (args.expected_revision, targets.as_slice()) {
            if row.item.revision != expected {
                return Err(ExternalError::invalid(format!(
                    "{} is at revision {}, not {expected}: it changed since it was read. Read it again and retry.",
                    handle(row.item.number),
                    row.item.revision
                )));
            }
        }
        let base = board.patch(args)?;
        if let Some(Some(parent_id)) = &base.parent_id {
            if let Some(row) = targets
                .iter()
                .find(|row| board.above(&row.item.id, parent_id))
            {
                return Err(ExternalError::invalid(format!(
                    "{} cannot go under {}: a task cannot sit under itself or one of its own sub-tasks.",
                    handle(row.item.number),
                    board.task_text(parent_id)
                )));
            }
        }
        let add = board.label_ids(&args.add_label)?;
        let remove = board.label_ids(&args.remove_label)?;
        let planned = targets
            .into_iter()
            .map(|row| (row, relabeled(&base, row, &add, &remove)))
            .collect();
        self.apply(
            &board,
            WorkAction::Updated,
            args.dry_run,
            &args.write,
            planned,
            None,
        )
        .await
    }

    async fn snooze(&self, args: &SnoozeArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let until = args.until.ms()?;
        let action = if until.is_some() {
            WorkAction::Snoozed
        } else {
            WorkAction::Updated
        };
        let planned = board
            .tasks(&args.tasks)?
            .into_iter()
            .map(|row| {
                let patch = WorkPatch {
                    snoozed_until_ms: Some(until),
                    ..WorkPatch::default()
                };
                (row, patch)
            })
            .collect();
        self.apply(&board, action, args.dry_run, &args.write, planned, None)
            .await
    }

    async fn claim(&self, args: &ClaimArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let name = handle(row.item.number);
        let agent = args.write.agent();
        if row.item.archived_at_ms.is_some() {
            return Err(ExternalError::invalid(format!(
                "{name} is archived. Restore it first: sona work restore {name}"
            )));
        }
        let works_it = match &row.doer {
            Some(doer) if !doer.agent.eq_ignore_ascii_case(&agent) => {
                return Err(ExternalError::invalid(
                    if row.item.status == WorkStatus::InProgress {
                        format!(
                            "{} is working on {name}. Find another with: sona work ready",
                            doer.agent
                        )
                    } else {
                        format!(
                            "{name} is delegated to {}. Find another with: sona work ready",
                            doer.agent
                        )
                    },
                ));
            }
            doer => doer.is_some(),
        };
        match row.item.status {
            WorkStatus::Triage => Err(ExternalError::invalid(format!(
                "{name} is still in triage. Accept it first: sona work accept {name}"
            ))),
            WorkStatus::InProgress if works_it && args.dry_run => {
                Ok(WorkAnswer::Preview(ExternalWorkPreview {
                    schema_version: QUERY_SCHEMA_VERSION,
                    dry_run: true,
                    action: WorkAction::Claimed,
                    operation_id: None,
                    changes: vec![ExternalWorkPlanned {
                        skipped: Some(format!("{agent} already works it")),
                        ..board.planned(row, &WorkPatch::default(), None)
                    }],
                }))
            }
            WorkStatus::InProgress if works_it => Ok(WorkAnswer::Change(ExternalWorkChange {
                schema_version: QUERY_SCHEMA_VERSION,
                action: WorkAction::Claimed,
                operation_id: None,
                tasks: vec![project_task(row, &board)],
                skipped: Vec::new(),
                proposed: Vec::new(),
                replayed: false,
                evidence_url: None,
            })),
            WorkStatus::InProgress => Err(ExternalError::invalid(format!(
                "{name} is already in progress. Find another with: sona work ready"
            ))),
            WorkStatus::Done | WorkStatus::Canceled | WorkStatus::Duplicate => {
                Err(ExternalError::invalid(format!(
                    "{name} is {}; there is nothing to claim.",
                    status_text(row.item.status).to_lowercase()
                )))
            }
            WorkStatus::Backlog | WorkStatus::NotStarted => {
                let operation_id = args
                    .write
                    .operation_id("claim", &(&row.item.id, row.item.revision))?;
                if args.dry_run {
                    let started = WorkPatch {
                        status: Some(WorkStatus::InProgress),
                        ..WorkPatch::default()
                    };
                    let mut planned = board.planned(row, &started, None);
                    planned.fields.push(ExternalFieldChange {
                        field: "worked by",
                        from: "nobody".to_owned(),
                        to: agent,
                    });
                    return Ok(WorkAnswer::Preview(ExternalWorkPreview {
                        schema_version: QUERY_SCHEMA_VERSION,
                        dry_run: true,
                        action: WorkAction::Claimed,
                        operation_id: Some(operation_id),
                        changes: vec![planned],
                    }));
                }
                let item = agents::claim_as(
                    self.app,
                    &agent,
                    &operation_id,
                    &row.item.id,
                    Some(row.item.revision),
                )
                .await
                .map_err(|message| self.write_refusal(message, &operation_id, &name))?;
                let mut task = board.project_item(item);
                task.doer = Some(agent);
                Ok(WorkAnswer::Change(ExternalWorkChange {
                    schema_version: QUERY_SCHEMA_VERSION,
                    action: WorkAction::Claimed,
                    operation_id: Some(operation_id),
                    tasks: vec![task],
                    skipped: Vec::new(),
                    proposed: Vec::new(),
                    replayed: false,
                    evidence_url: None,
                }))
            }
        }
    }

    async fn done(&self, args: &DoneArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let link = args.evidence.as_deref().and_then(agents::evidence_link);
        let note = args.evidence.as_deref().filter(|_| link.is_none());
        let planned = board
            .tasks(&args.tasks)?
            .into_iter()
            .map(|row| {
                let patch = WorkPatch {
                    status: Some(WorkStatus::Done),
                    details: note.and_then(|note| with_evidence(row.item.details.as_deref(), note)),
                    ..WorkPatch::default()
                };
                (row, patch)
            })
            .collect();
        self.apply(
            &board,
            WorkAction::Done,
            args.dry_run,
            &args.write,
            planned,
            link.as_deref(),
        )
        .await
    }

    /// Writes one patch per task: fenced alone, all or none when the patches
    /// agree, and otherwise one task at a time, stopping at the first refusal.
    /// `link` is kept with the one task as evidence of the work.
    async fn apply(
        &self,
        board: &Board,
        action: WorkAction,
        dry_run: bool,
        write: &WriteArgs,
        planned: Vec<(&WorkItemRow, WorkPatch)>,
        link: Option<&str>,
    ) -> Result<WorkAnswer, ExternalError> {
        let mut changing = Vec::new();
        let mut skipped = Vec::new();
        for (row, patch) in planned {
            if board.diff(row, &patch).is_empty() && link.is_none() {
                skipped.push(skip(row, unchanged_reason(row, &patch)));
            } else {
                changing.push((row, patch));
            }
        }
        let operation_id = match changing.as_slice() {
            [] => None,
            [(row, patch)] => Some(write.operation_id(
                action.verb(),
                &(&row.item.id, row.item.revision, patch, link),
            )?),
            several => Some(
                write.operation_id(
                    action.verb(),
                    &several
                        .iter()
                        .map(|(row, patch)| (&row.item.id, row.item.revision, patch))
                        .collect::<Vec<_>>(),
                )?,
            ),
        };
        if dry_run {
            let mut changes: Vec<ExternalWorkPlanned> = changing
                .iter()
                .map(|(row, patch)| {
                    let mut planned = board.planned(row, patch, None);
                    planned.fields.extend(link.map(|link| ExternalFieldChange {
                        field: "evidence",
                        from: String::new(),
                        to: link.to_owned(),
                    }));
                    planned
                })
                .collect();
            changes.extend(skipped.into_iter().map(|skip| ExternalWorkPlanned {
                id: skip.id,
                handle: skip.handle,
                title: String::new(),
                fields: Vec::new(),
                commitment: None,
                skipped: Some(skip.reason),
            }));
            return Ok(WorkAnswer::Preview(ExternalWorkPreview {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                action,
                operation_id,
                changes,
            }));
        }
        let Some(operation_id) = operation_id else {
            return Ok(changed(action, None, Vec::new(), skipped));
        };
        let agent = write.agent();
        let changes = match changing.as_slice() {
            [(row, patch)] => vec![agents::update_as(
                self.app,
                &agent,
                &operation_id,
                agents::ItemChange {
                    id: &row.item.id,
                    expected_revision: row.item.revision,
                    patch,
                    link,
                },
            )
            .await
            .map_err(|message| {
                self.write_refusal(message, &operation_id, &handle(row.item.number))
            })?],
            several if several.iter().all(|(_, patch)| *patch == several[0].1) => {
                let ids: Vec<String> = several.iter().map(|(row, _)| row.item.id.clone()).collect();
                agents::bulk_update_as(self.app, &agent, &operation_id, &ids, &several[0].1)
                    .await
                    .map_err(|message| {
                        self.write_refusal(message, &operation_id, "One of these tasks")
                    })?
            }
            several => self.apply_each(&agent, &operation_id, several).await?,
        };
        Ok(WorkAnswer::Change(ExternalWorkChange {
            evidence_url: link.map(str::to_owned),
            ..board.agent_changed(action, operation_id, changes, skipped)
        }))
    }

    async fn apply_each(
        &self,
        agent: &str,
        operation_id: &str,
        several: &[(&WorkItemRow, WorkPatch)],
    ) -> Result<Vec<AgentChange>, ExternalError> {
        let base = Uuid::parse_str(operation_id).map_err(|_| {
            ExternalError::new(ExternalErrorCode::Failed, "The change could not be named.")
        })?;
        let mut changes = Vec::with_capacity(several.len());
        for (row, patch) in several {
            let task_operation = Uuid::new_v5(&base, row.item.id.as_bytes()).to_string();
            match agents::update_as(
                self.app,
                agent,
                &task_operation,
                agents::ItemChange {
                    id: &row.item.id,
                    expected_revision: row.item.revision,
                    patch,
                    link: None,
                },
            )
            .await
            {
                Ok(change) => changes.push(change),
                Err(message) => {
                    let refusal =
                        self.write_refusal(message, &task_operation, &handle(row.item.number));
                    let done: Vec<String> = several[..changes.len()]
                        .iter()
                        .map(|(row, _)| handle(row.item.number))
                        .collect();
                    let kept = if done.is_empty() {
                        "Nothing was changed.".to_owned()
                    } else {
                        format!(
                            "Already changed: {}. The same command again changes only what is left.",
                            done.join(", ")
                        )
                    };
                    return Err(ExternalError {
                        message: format!("{} {kept}", refusal.message),
                        ..refusal
                    });
                }
            }
        }
        Ok(changes)
    }

    /// Refuses `what` while agents' changes wait for the user: it has no
    /// field change to propose in its place.
    fn applies(&self, what: &str) -> Result<(), ExternalError> {
        match agents::autonomy(self.app) {
            WorkAutonomy::Apply => Ok(()),
            WorkAutonomy::Suggest => Err(ExternalError::invalid(format!(
                "While agents' changes wait for the user, {what} is theirs to do in Sona. Propose a field change with: sona work update. The user can let agents' changes apply in Sona's Settings > Agents."
            ))),
        }
    }

    /// Files the history the tracker's own functions wrote for these tasks
    /// since `since_ms` under the agent that asked.
    fn credit(
        &self,
        write: &WriteArgs,
        ids: &[String],
        since_ms: i64,
    ) -> Result<(), ExternalError> {
        agents::credit_since(self.app, ids, since_ms, &write.agent())
            .map_err(|message| refusal(self.history, message))
    }

    async fn triage(
        &self,
        tasks: &[TaskRef],
        decision: Decision<'_>,
        dry_run: bool,
        write: &WriteArgs,
    ) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let action = match decision {
            Decision::Accept { .. } => WorkAction::Accepted,
            Decision::Decline { .. } => WorkAction::Declined,
        };
        let mut waiting = Vec::new();
        let mut skipped = Vec::new();
        for row in board.tasks(tasks)? {
            if row.item.status == WorkStatus::Triage && row.item.archived_at_ms.is_none() {
                waiting.push(row);
            } else if row.item.archived_at_ms.is_some() {
                skipped.push(skip(row, "archived".to_owned()));
            } else {
                skipped.push(skip(
                    row,
                    format!(
                        "not in triage; it is {}",
                        status_text(row.item.status).to_lowercase()
                    ),
                ));
            }
        }
        let revisions: Vec<(&str, i64)> = waiting
            .iter()
            .map(|row| (row.item.id.as_str(), row.item.revision))
            .collect();
        let operation_id = if waiting.is_empty() {
            None
        } else {
            Some(match decision {
                Decision::Accept { apply_suggestions } => {
                    write.operation_id("accept", &(&revisions, apply_suggestions))?
                }
                Decision::Decline { reason } => {
                    write.operation_id("decline", &(&revisions, reason))?
                }
            })
        };
        if dry_run {
            let mut changes: Vec<ExternalWorkPlanned> = waiting
                .iter()
                .map(|row| {
                    let (patch, commitment) = match decision {
                        Decision::Accept { apply_suggestions } => {
                            (board.acceptance(row, apply_suggestions), None)
                        }
                        Decision::Decline { .. } => (
                            WorkPatch {
                                status: Some(WorkStatus::Canceled),
                                ..WorkPatch::default()
                            },
                            row.item
                                .source_loop_id
                                .is_some()
                                .then_some(CommitmentEffect::SetsAside),
                        ),
                    };
                    board.planned(row, &patch, Some(commitment))
                })
                .collect();
            changes.extend(skipped.into_iter().map(|skip| ExternalWorkPlanned {
                id: skip.id,
                handle: skip.handle,
                title: String::new(),
                fields: Vec::new(),
                commitment: None,
                skipped: Some(skip.reason),
            }));
            return Ok(WorkAnswer::Preview(ExternalWorkPreview {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                action,
                operation_id,
                changes,
            }));
        }
        let Some(operation_id) = operation_id else {
            return Ok(changed(action, None, Vec::new(), skipped));
        };
        self.applies(match decision {
            Decision::Accept { .. } => "accepting triage",
            Decision::Decline { .. } => "declining triage",
        })?;
        let ids: Vec<String> = waiting.iter().map(|row| row.item.id.clone()).collect();
        let started_ms = now_ms();
        let items = match decision {
            Decision::Accept { apply_suggestions } => {
                work::triage_accept_as(
                    self.app,
                    WorkActor::Agent,
                    &operation_id,
                    &ids,
                    apply_suggestions,
                )
                .await
            }
            Decision::Decline { reason } => {
                work::triage_decline_as(self.app, WorkActor::Agent, &operation_id, &ids, reason)
                    .await
            }
        }
        .map_err(|message| self.write_refusal(message, &operation_id, "One of these items"))?;
        self.credit(write, &ids, started_ms)?;
        let tasks = items
            .into_iter()
            .map(|item| board.project_item(item))
            .collect();
        Ok(changed(action, Some(operation_id), tasks, skipped))
    }

    async fn duplicate(&self, args: &DuplicateArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let row = board.task(&args.task)?;
        let of = board.task(&args.of)?;
        if row.item.id == of.item.id {
            return Err(ExternalError::invalid("A task cannot duplicate itself."));
        }
        if of.item.duplicate_of_id.as_deref() == Some(row.item.id.as_str()) {
            return Err(ExternalError::invalid(format!(
                "{} is already a duplicate of {}.",
                handle(of.item.number),
                handle(row.item.number)
            )));
        }
        self.applies("marking a duplicate")?;
        let operation_id = args
            .write
            .operation_id("duplicate", &(&row.item.id, row.item.revision, &of.item.id))?;
        if args.dry_run {
            let (root, _) = self.rehearse(|conn| {
                items::mark_duplicate(conn, &row.item.id, &of.item.id, WorkActor::Agent, now_ms())
            })?;
            let duplicate = WorkPatch {
                status: Some(WorkStatus::Duplicate),
                ..WorkPatch::default()
            };
            let mut planned = board.planned(row, &duplicate, Some(None));
            planned.fields.push(ExternalFieldChange {
                field: "duplicate of",
                from: row
                    .item
                    .duplicate_of_id
                    .as_deref()
                    .map_or_else(|| "none".to_owned(), |id| board.task_text(id)),
                to: board.task_text(&root),
            });
            return Ok(WorkAnswer::Preview(ExternalWorkPreview {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                action: WorkAction::MarkedDuplicate,
                operation_id: Some(operation_id),
                changes: vec![planned],
            }));
        }
        let started_ms = now_ms();
        let item = work::mark_duplicate_as(
            self.app,
            WorkActor::Agent,
            &operation_id,
            &row.item.id,
            &of.item.id,
        )
        .await
        .map_err(|message| self.write_refusal(message, &operation_id, &handle(row.item.number)))?;
        self.credit(&args.write, std::slice::from_ref(&row.item.id), started_ms)?;
        Ok(changed(
            WorkAction::MarkedDuplicate,
            Some(operation_id),
            vec![board.project_item(item)],
            Vec::new(),
        ))
    }

    async fn archive(&self, args: &TasksArgs, archived: bool) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let action = if archived {
            WorkAction::Archived
        } else {
            WorkAction::Restored
        };
        let mut changing = Vec::new();
        let mut skipped = Vec::new();
        for row in board.tasks(&args.tasks)? {
            if row.item.archived_at_ms.is_some() == archived {
                let reason = if archived {
                    "already archived"
                } else {
                    "not archived"
                };
                skipped.push(skip(row, reason.to_owned()));
            } else {
                changing.push(row);
            }
        }
        let revisions: Vec<(&str, i64)> = changing
            .iter()
            .map(|row| (row.item.id.as_str(), row.item.revision))
            .collect();
        let operation_id = if changing.is_empty() {
            None
        } else {
            Some(args.write.operation_id(action.verb(), &revisions)?)
        };
        if args.dry_run {
            let (from, to) = if archived {
                ("no", "yes")
            } else {
                ("yes", "no")
            };
            let mut changes: Vec<ExternalWorkPlanned> = changing
                .iter()
                .map(|row| ExternalWorkPlanned {
                    id: row.item.id.clone(),
                    handle: handle(row.item.number),
                    title: row.item.title.clone(),
                    fields: vec![ExternalFieldChange {
                        field: "archived",
                        from: from.to_owned(),
                        to: to.to_owned(),
                    }],
                    commitment: None,
                    skipped: None,
                })
                .collect();
            changes.extend(skipped.into_iter().map(|skip| ExternalWorkPlanned {
                id: skip.id,
                handle: skip.handle,
                title: String::new(),
                fields: Vec::new(),
                commitment: None,
                skipped: Some(skip.reason),
            }));
            return Ok(WorkAnswer::Preview(ExternalWorkPreview {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                action,
                operation_id,
                changes,
            }));
        }
        let Some(operation_id) = operation_id else {
            return Ok(changed(action, None, Vec::new(), skipped));
        };
        self.applies(if archived { "archiving" } else { "restoring" })?;
        let ids: Vec<String> = changing.iter().map(|row| row.item.id.clone()).collect();
        let started_ms = now_ms();
        let items =
            work::set_archived_as(self.app, WorkActor::Agent, &operation_id, &ids, archived)
                .map_err(|message| {
                    self.write_refusal(message, &operation_id, "One of these tasks")
                })?;
        self.credit(&args.write, &ids, started_ms)?;
        let tasks = items
            .into_iter()
            .map(|item| board.project_item(item))
            .collect();
        Ok(changed(action, Some(operation_id), tasks, skipped))
    }

    async fn move_tasks(&self, args: &MoveArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let rows = board.tasks(&args.tasks)?;
        let ids: Vec<String> = rows.iter().map(|row| row.item.id.clone()).collect();
        let beside = |task: &TaskRef| -> Result<(String, String), ExternalError> {
            let row = board.task(task)?;
            if ids.contains(&row.item.id) {
                return Err(ExternalError::new(
                    ExternalErrorCode::InvalidRequest,
                    "A task can't move beside itself. Pick a task that isn't moving.",
                ));
            }
            Ok((row.item.id.clone(), handle(row.item.number)))
        };
        let (place, to) = match (&args.before, &args.after) {
            (Some(task), _) => {
                let (id, handle) = beside(task)?;
                (WorkPlace::Before(id), format!("before {handle}"))
            }
            (None, Some(task)) => {
                let (id, handle) = beside(task)?;
                (WorkPlace::After(id), format!("after {handle}"))
            }
            (None, None) if args.bottom => (WorkPlace::Bottom, "bottom".to_owned()),
            (None, None) => (WorkPlace::Top, "top".to_owned()),
        };
        let revisions: Vec<(&str, i64)> = rows
            .iter()
            .map(|row| (row.item.id.as_str(), row.item.revision))
            .collect();
        let operation_id = args
            .write
            .operation_id(WorkAction::Moved.verb(), &(&revisions, &place))?;
        if args.dry_run {
            self.rehearse(|conn| items::place_manually(conn, &ids, &place))?;
            return Ok(WorkAnswer::Preview(ExternalWorkPreview {
                schema_version: QUERY_SCHEMA_VERSION,
                dry_run: true,
                action: WorkAction::Moved,
                operation_id: Some(operation_id),
                changes: rows
                    .iter()
                    .map(|row| ExternalWorkPlanned {
                        id: row.item.id.clone(),
                        handle: handle(row.item.number),
                        title: row.item.title.clone(),
                        fields: vec![ExternalFieldChange {
                            field: "manual_order",
                            from: String::new(),
                            to: to.clone(),
                        }],
                        commitment: None,
                        skipped: None,
                    })
                    .collect(),
            }));
        }
        self.applies("moving a task in the manual order")?;
        let started_ms = now_ms();
        let items = work::move_tasks(self.app, &operation_id, &ids, &place)
            .map_err(|message| self.write_refusal(message, &operation_id, "One of these tasks"))?;
        self.credit(&args.write, &ids, started_ms)?;
        let tasks = items
            .into_iter()
            .filter(|item| ids.contains(&item.id))
            .map(|item| board.project_item(item))
            .collect();
        Ok(changed(WorkAction::Moved, Some(operation_id), tasks, Vec::new()))
    }

    async fn relate(&self, args: &RelateArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (from, to, kind) = board.link(args)?;
        let operation_id = args.write.fresh_operation_id();
        let relation = if args.dry_run {
            self.rehearse(|conn| {
                items::add_relation(
                    conn,
                    &from.item.id,
                    &to.item.id,
                    kind,
                    WorkActor::Agent,
                    now_ms(),
                )
            })?
        } else {
            let started_ms = now_ms();
            let relation = work::relation_add_as(
                self.app,
                WorkActor::Agent,
                &operation_id,
                &from.item.id,
                &to.item.id,
                kind,
            )
            .map_err(|message| self.write_refusal(message, &operation_id, "The link"))?;
            self.credit(
                &args.write,
                &[from.item.id.clone(), to.item.id.clone()],
                started_ms,
            )?;
            relation
        };
        Ok(WorkAnswer::Relation(ExternalWorkRelationChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id,
            removed: false,
            relation: relation_between(&relation, from, to),
        }))
    }

    async fn unrelate(&self, args: &RelateArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let (from, to, kind) = board.link(args)?;
        let relations =
            planning::history(self.history, |conn| items::relations(conn, &from.item.id))
                .map_err(|message| refusal(self.history, message))?;
        let found = relations
            .into_iter()
            .find(|relation| {
                relation.kind == kind
                    && ((relation.from_id == from.item.id && relation.to_id == to.item.id)
                        || (kind == WorkRelationKind::Related
                            && relation.from_id == to.item.id
                            && relation.to_id == from.item.id))
            })
            .ok_or_else(|| {
                ExternalError::new(
                    ExternalErrorCode::NotFound,
                    format!(
                        "{} and {} have no such link.",
                        handle(from.item.number),
                        handle(to.item.number)
                    ),
                )
            })?;
        let operation_id = args.write.operation_id("unrelate", &found.id)?;
        if args.dry_run {
            self.rehearse(|conn| {
                items::remove_relation(conn, &found.id, WorkActor::Agent, now_ms())
            })?;
        } else {
            let started_ms = now_ms();
            work::relation_remove_as(self.app, WorkActor::Agent, &operation_id, &found.id)
                .map_err(|message| self.write_refusal(message, &operation_id, "The link"))?;
            self.credit(
                &args.write,
                &[from.item.id.clone(), to.item.id.clone()],
                started_ms,
            )?;
        }
        Ok(WorkAnswer::Relation(ExternalWorkRelationChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id,
            removed: true,
            relation: relation_between(&found, from, to),
        }))
    }

    fn all_labels(&self) -> Result<Vec<WorkLabel>, ExternalError> {
        work::work_labels(self.app.state()).map_err(|message| refusal(self.history, message))
    }

    fn all_views(&self) -> Result<Vec<WorkView>, ExternalError> {
        work::work_views(self.app.state()).map_err(|message| refusal(self.history, message))
    }

    fn cycles(&self, args: &CyclesArgs) -> Result<WorkAnswer, ExternalError> {
        let listing = work::cycles::work_cycles(self.app.clone())
            .map_err(|message| refusal(self.history, message))?;
        let mut answer = external_cycles(&listing);
        if args.ics {
            answer.calendar = Some(work::cycles::calendar(&listing.cycles, now_ms()));
        }
        Ok(WorkAnswer::Cycles(answer))
    }

    fn cycle(&self, args: &CycleArgs) -> Result<WorkAnswer, ExternalError> {
        let listing = work::cycles::work_cycles(self.app.clone())
            .map_err(|message| refusal(self.history, message))?;
        let number = named_cycle(&listing.cycles, args.cycle)?;
        let progress = work::cycles::work_cycle(self.app.clone(), number)
            .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::Cycle(Box::new(ExternalWorkCycleProgress {
            schema_version: QUERY_SCHEMA_VERSION,
            cycle: project_cycle(&progress.cycle),
            carried_over: progress.carried_over,
            days: progress
                .days
                .into_iter()
                .map(|day| ExternalWorkCycleDay {
                    day: day.day,
                    scope: day.scope,
                    started: day.started,
                    done: day.done,
                })
                .collect(),
            target: progress
                .target
                .into_iter()
                .map(|target| ExternalWorkCycleTarget {
                    day: target.day,
                    done: target.done,
                })
                .collect(),
        })))
    }

    fn view_named(&self, value: &str) -> Result<WorkView, ExternalError> {
        let views = self.all_views()?;
        pick("view", value, &views, |view| &view.id, |view| &view.name)
            .cloned()
            .map_err(|refusal| refusal.hint("List them with: sona work views"))
    }

    fn labels(&self) -> Result<WorkAnswer, ExternalError> {
        Ok(WorkAnswer::Labels(ExternalWorkLabels {
            schema_version: QUERY_SCHEMA_VERSION,
            labels: self.all_labels()?.into_iter().map(project_label).collect(),
        }))
    }

    fn label_save(&self, args: &LabelSaveArgs) -> Result<WorkAnswer, ExternalError> {
        let labels = self.all_labels()?;
        let name = args.name.trim();
        let existing = match &args.label {
            Some(label) => Some(
                pick(
                    "label",
                    label,
                    &labels,
                    |label| &label.id,
                    |label| &label.name,
                )
                .map_err(|refusal| refusal.hint("List them with: sona work labels"))?,
            ),
            None => labels
                .iter()
                .find(|label| label.name.to_lowercase() == name.to_lowercase()),
        };
        if let Some(taken) = labels.iter().find(|label| {
            label.name.to_lowercase() == name.to_lowercase()
                && existing.is_none_or(|existing| existing.id != label.id)
        }) {
            return Err(ExternalError::invalid(format!(
                "A label called {:?} already exists.",
                taken.name
            )));
        }
        let (id, hue, operation_id) = match existing {
            Some(label) => {
                let hue = args.hue.map_or(label.hue, f64::from);
                if label.name == name && label.hue.to_bits() == hue.to_bits() {
                    return Ok(WorkAnswer::Label(ExternalWorkLabelChange {
                        schema_version: QUERY_SCHEMA_VERSION,
                        dry_run: args.dry_run,
                        operation_id: None,
                        deleted: false,
                        label: project_label(label.clone()),
                    }));
                }
                let operation_id = args
                    .write
                    .operation_id("label-save", &(&label.id, label.revision, name, hue))?;
                (Some(label.id.clone()), hue, operation_id)
            }
            None => (
                None,
                args.hue.map_or_else(|| hue_for(name), f64::from),
                args.write.fresh_operation_id(),
            ),
        };
        let label = if args.dry_run {
            self.rehearse(|conn| items::save_label(conn, id.as_deref(), name, hue, now_ms()))?
        } else {
            self.applies("saving a label")?;
            work::work_label_save(
                self.app.clone(),
                operation_id.clone(),
                id,
                name.to_owned(),
                hue,
            )
            .map_err(|message| self.write_refusal(message, &operation_id, "The label"))?
        };
        Ok(WorkAnswer::Label(ExternalWorkLabelChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id: Some(operation_id),
            deleted: false,
            label: project_label(label),
        }))
    }

    fn label_delete(&self, args: &LabelDeleteArgs) -> Result<WorkAnswer, ExternalError> {
        let labels = self.all_labels()?;
        let label = pick(
            "label",
            &args.label,
            &labels,
            |label| &label.id,
            |label| &label.name,
        )
        .map_err(|refusal| refusal.hint("List them with: sona work labels"))?
        .clone();
        let operation_id = args.write.operation_id("label-delete", &label.id)?;
        if args.dry_run {
            self.rehearse(|conn| items::delete_label(conn, &label.id))?;
        } else {
            self.applies("deleting a label")?;
            work::work_label_delete(self.app.clone(), operation_id.clone(), label.id.clone())
                .map_err(|message| self.write_refusal(message, &operation_id, "The label"))?;
        }
        Ok(WorkAnswer::Label(ExternalWorkLabelChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id: Some(operation_id),
            deleted: true,
            label: project_label(label),
        }))
    }

    fn views(&self) -> Result<WorkAnswer, ExternalError> {
        Ok(WorkAnswer::Views(ExternalWorkViews {
            schema_version: QUERY_SCHEMA_VERSION,
            views: self.all_views()?.iter().map(project_view).collect(),
        }))
    }

    async fn view_save(&self, args: &ViewSaveArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let views = self.all_views()?;
        let name = args.name.trim();
        let existing = match &args.view {
            Some(view) => Some(
                pick("view", view, &views, |view| &view.id, |view| &view.name)
                    .map_err(|refusal| refusal.hint("List them with: sona work views"))?,
            ),
            None => views
                .iter()
                .find(|view| view.name.to_lowercase() == name.to_lowercase()),
        };
        let query = args.filters.narrow(
            &board,
            existing.map_or_else(WorkQuery::default, |view| view.query.clone()),
        )?;
        let mut display = existing.map_or_else(
            || WorkDisplay {
                layout: WorkLayout::List,
                group_by: WorkGroupBy::Status,
                order_by: WorkOrderBy::Priority,
                show_empty_groups: false,
                properties: VIEW_PROPERTIES.map(str::to_owned).to_vec(),
                show_sub_tasks: true,
                sub_group_by: WorkGroupBy::None,
                hidden_columns: Vec::new(),
            },
            |view| view.display.clone(),
        );
        if let Some(layout) = args.layout {
            display.layout = match layout {
                LayoutArg::List => WorkLayout::List,
                LayoutArg::Board => WorkLayout::Board,
            };
        }
        if let Some(group) = args.group_by {
            display.group_by = group.group();
        }
        if let Some(order) = args.order_by {
            display.order_by = order.order();
        }
        if let Some(show) = args.show_empty_groups {
            display.show_empty_groups = show;
        }
        if let Some(properties) = &args.properties {
            display.properties = properties
                .iter()
                .map(|property| property.as_str().to_owned())
                .collect();
        }
        if let Some(show) = args.show_sub_tasks {
            display.show_sub_tasks = show;
        }
        if let Some(lanes) = args.sub_group_by {
            display.sub_group_by = lanes.group();
        }
        if let Some(hidden) = &args.hidden_columns {
            display.hidden_columns = hidden.iter().filter(|key| !key.is_empty()).cloned().collect();
        }
        let (id, operation_id) = match existing {
            Some(view) => (
                Some(view.id.clone()),
                args.write.operation_id(
                    "view-save",
                    &(&view.id, view.revision, name, &query, &display),
                )?,
            ),
            None => (None, args.write.fresh_operation_id()),
        };
        let view = if args.dry_run {
            self.rehearse(|conn| {
                items::save_view(conn, id.as_deref(), name, &query, &display, now_ms())
            })?
        } else {
            work::work_view_save(
                self.app.clone(),
                operation_id.clone(),
                id,
                name.to_owned(),
                query,
                display,
            )
            .map_err(|message| self.write_refusal(message, &operation_id, "The view"))?
        };
        Ok(WorkAnswer::View(Box::new(ExternalWorkViewChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id,
            deleted: false,
            view: project_view(&view),
        })))
    }

    fn view_delete(&self, args: &ViewDeleteArgs) -> Result<WorkAnswer, ExternalError> {
        let view = self.view_named(&args.view)?;
        let operation_id = args.write.operation_id("view-delete", &view.id)?;
        if args.dry_run {
            self.rehearse(|conn| items::delete_view(conn, &view.id))?;
        } else {
            work::work_view_delete(self.app.clone(), operation_id.clone(), view.id.clone())
                .map_err(|message| self.write_refusal(message, &operation_id, "The view"))?;
        }
        Ok(WorkAnswer::View(Box::new(ExternalWorkViewChange {
            schema_version: QUERY_SCHEMA_VERSION,
            dry_run: args.dry_run,
            operation_id,
            deleted: true,
            view: project_view(&view),
        })))
    }

    async fn export(&self, args: &ExportArgs) -> Result<WorkAnswer, ExternalError> {
        let board = self.board().await?;
        let query = args.filters.narrow(&board, WorkQuery::default())?;
        let mut rows = self.listed(&board.fence, query).await?.items;
        rows.sort_by_key(|row| row.item.number);
        std::fs::create_dir_all(&args.md).map_err(|error| {
            ExternalError::new(
                ExternalErrorCode::Failed,
                format!("{} could not be created: {error}", args.md.display()),
            )
        })?;
        let mut files = Vec::with_capacity(rows.len());
        for row in &rows {
            let task = project_task(row, &board);
            let path = args.md.join(format!("{}.md", task.handle));
            std::fs::write(&path, markdown(&task)).map_err(|error| {
                ExternalError::new(
                    ExternalErrorCode::Failed,
                    format!("{} could not be written: {error}", path.display()),
                )
            })?;
            files.push(path.display().to_string());
        }
        Ok(WorkAnswer::Export(ExternalWorkExport {
            schema_version: QUERY_SCHEMA_VERSION,
            directory: args.md.display().to_string(),
            files,
            conversations_available: board.fence.is_open(),
        }))
    }

    async fn follow_up(&self, args: &FollowUpArgs) -> Result<WorkAnswer, ExternalError> {
        let meeting_id = args.meeting.to_string();
        match Fence::read(self.meetings).await? {
            Fence::Closed => return Err(conversations_closed()),
            fence if fence.hides_meeting(&meeting_id) => {
                return Err(ExternalError::new(
                    ExternalErrorCode::NotFound,
                    "No meeting has that id.",
                ));
            }
            Fence::Open(_) => {}
        }
        let follow_up = work::live::work_follow_up(self.app.state(), self.app.state(), meeting_id)
            .await
            .map_err(|message| refusal(self.history, message))?;
        Ok(WorkAnswer::FollowUp(ExternalWorkFollowUp {
            schema_version: QUERY_SCHEMA_VERSION,
            meeting_id: follow_up.meeting_id,
            text: follow_up.text,
            lines: follow_up
                .lines
                .into_iter()
                .map(|line| ExternalWorkFollowUpLine {
                    id: line.item_id,
                    owner: line.owner_name,
                    title: line.title,
                    due_at_ms: line.due_at_ms,
                    quote: line.quote,
                })
                .collect(),
        }))
    }
}

impl ExternalError {
    fn hint(self, hint: &str) -> Self {
        Self {
            message: format!("{} {hint}", self.message),
            ..self
        }
    }
}

/// A tracker refusal as an outside caller should branch on it.
fn refusal(history: &HistoryManager, message: String) -> ExternalError {
    let code = match message.as_str() {
        "This item no longer exists." => ExternalErrorCode::NotFound,
        planning::CONVERSATIONS_CLOSED
        | planning::PORTABLE_NO_CONVERSATIONS
        | "Planning is paused while recording." => ExternalErrorCode::Unavailable,
        _ if !history.storage_is_ready() => ExternalErrorCode::Unavailable,
        "Plans could not be saved or read. Try again." | "Conversations could not be read." => {
            ExternalErrorCode::Failed
        }
        _ => ExternalErrorCode::InvalidRequest,
    };
    ExternalError::new(code, message)
}

/// Which work from meetings stays out of an answer.
enum Fence {
    /// The conversations are open: work from meetings kept on this Mac.
    Open(KeptLocal),
    /// Nothing says which meetings are kept on this Mac, so all of it.
    Closed,
}

impl Fence {
    async fn read(meetings: &Arc<MeetingSessionManager>) -> Result<Self, ExternalError> {
        match store(meetings).await {
            Ok(store) => kept_local(&store).map(Self::Open),
            Err(_) => Ok(Self::Closed),
        }
    }

    const fn is_open(&self) -> bool {
        matches!(self, Self::Open(_))
    }

    fn hides_meeting(&self, meeting_id: &str) -> bool {
        match self {
            Self::Open(kept_local) => kept_local.hides_meeting(meeting_id),
            Self::Closed => true,
        }
    }

    /// The tasks it leaves out, by id: each whose own commitment or evidence
    /// names a meeting it hides, whatever the task's row shows of it.
    fn hidden_tasks(&self, history: &Connection) -> anyhow::Result<HashSet<String>> {
        work::drawing_on(history, |kind, source_id| self.hides_source(kind, source_id))
    }

    fn hides_suggestion(&self, suggestion: &WorkFieldSuggestion) -> bool {
        suggestion.draws_on(|meeting_id| match self {
            Self::Open(kept_local) => kept_local.hides_meeting(meeting_id),
            Self::Closed => true,
        })
    }

    fn hides_evidence(&self, evidence: &planning::EvidenceLink) -> bool {
        self.hides_source(evidence.source_kind, &evidence.source_id)
    }

    fn hides_source(&self, kind: planning::EvidenceSourceKind, source_id: &str) -> bool {
        match self {
            Self::Open(kept_local) => evidence_kept_local(kind, source_id, kept_local),
            Self::Closed => match kind {
                planning::EvidenceSourceKind::Meeting | planning::EvidenceSourceKind::Loop => true,
                planning::EvidenceSourceKind::Note
                | planning::EvidenceSourceKind::Dictation
                | planning::EvidenceSourceKind::Link => false,
            },
        }
    }

    fn hides_line(&self, line: &WorkAnswerLine) -> bool {
        let WorkCaptureSource::Meeting { session_id, .. } = &line.source else {
            return false;
        };
        match self {
            Self::Open(kept_local) => kept_local.hides_meeting(session_id),
            Self::Closed => true,
        }
    }
}

/// The refusal for a request that needs the people or meetings in the
/// conversations while those can't be opened.
fn conversations_closed() -> ExternalError {
    ExternalError::new(
        ExternalErrorCode::Unavailable,
        if crate::portable::is_portable() {
            planning::PORTABLE_NO_CONVERSATIONS
        } else {
            planning::CONVERSATIONS_CLOSED
        },
    )
}

/// What to tell a caller whose answer lacks work from meetings. Portable mode
/// keeps no conversations, so there it lacks nothing.
fn left_out(conversations_available: bool) -> Option<&'static str> {
    (!conversations_available && !crate::portable::is_portable()).then_some(
        "Work and quotes from meetings are left out while conversations can't be opened.",
    )
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn handle(number: i64) -> String {
    format!("S-{number}")
}

/// Priority first with none last, then the soonest due, then the oldest.
fn urgency(row: &WorkItemRow) -> (u8, i64, i64) {
    let priority = match row.item.priority {
        0 => u8::MAX,
        priority => priority,
    };
    (
        priority,
        row.item.due_at_ms.unwrap_or(i64::MAX),
        row.item.number,
    )
}

/// Where `row` sorts in `order`, smallest first; the number breaks ties, so
/// no two rows share a place.
fn place(row: &WorkItemRow, order: OrderArg) -> [i64; 3] {
    let item = &row.item;
    let (priority, due, number) = urgency(row);
    let priority = i64::from(priority);
    match order {
        OrderArg::Priority => [priority, due, number],
        OrderArg::Due => [due, priority, number],
        OrderArg::Updated => [-item.updated_at_ms, -number, 0],
        OrderArg::Created => [-item.created_at_ms, -number, 0],
        OrderArg::Status => [status_rank(item.status), priority, number],
        OrderArg::Manual => [sortable(item.sort_order), number, 0],
    }
}

/// A status's place when a list orders by status: the order the app groups
/// them in, work under way first.
const fn status_rank(status: WorkStatus) -> i64 {
    match status {
        WorkStatus::Triage => 0,
        WorkStatus::InProgress => 1,
        WorkStatus::NotStarted => 2,
        WorkStatus::Backlog => 3,
        WorkStatus::Done => 4,
        WorkStatus::Canceled => 5,
        WorkStatus::Duplicate => 6,
    }
}

/// `value` as an integer that sorts the way the float does.
fn sortable(value: f64) -> i64 {
    let bits = i64::from_ne_bytes(value.to_bits().to_ne_bytes());
    if bits < 0 {
        bits ^ i64::MAX
    } else {
        bits
    }
}

fn page(
    mut rows: Vec<WorkItemRow>,
    paging: &PageArgs,
    board: &Board,
) -> Result<ExternalWorkPage, ExternalError> {
    let order = paging.order()?;
    rows.sort_by_cached_key(|row| place(row, order));
    let start = paging.cursor.map_or(0, |cursor| {
        rows.partition_point(|row| place(row, order) <= cursor.after)
    });
    let end = rows
        .len()
        .min(start + paging.limit.unwrap_or(DEFAULT_LIMIT));
    let shown = &rows[start..end];
    let has_more = end < rows.len();
    Ok(ExternalWorkPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries: PageTasks {
            tasks: shown.iter().map(|row| project_task(row, board)).collect(),
            fields: paging.fields.clone(),
        },
        total: rows.len(),
        has_more,
        next_cursor: shown
            .last()
            .filter(|_| has_more)
            .map(|row| {
                Cursor {
                    order,
                    after: place(row, order),
                }
                .to_string()
            }),
        conversations_available: board.fence.is_open(),
        read_as: None,
    })
}

fn changed(
    action: WorkAction,
    operation_id: Option<String>,
    tasks: Vec<ExternalWorkItem>,
    skipped: Vec<ExternalWorkSkip>,
) -> WorkAnswer {
    WorkAnswer::Change(ExternalWorkChange {
        schema_version: QUERY_SCHEMA_VERSION,
        action,
        operation_id,
        tasks,
        skipped,
        proposed: Vec::new(),
        replayed: false,
        evidence_url: None,
    })
}

fn skip(row: &WorkItemRow, reason: String) -> ExternalWorkSkip {
    ExternalWorkSkip {
        id: row.item.id.clone(),
        handle: handle(row.item.number),
        reason,
    }
}

fn unchanged_reason(row: &WorkItemRow, patch: &WorkPatch) -> String {
    match patch.status {
        Some(status) if status == row.item.status => {
            format!("already {}", status_text(status).to_lowercase())
        }
        _ => "nothing to change".to_owned(),
    }
}

/// The details with one more line, unless that line is already there.
fn with_evidence(details: Option<&str>, evidence: &str) -> Option<Option<String>> {
    let line = format!("Evidence: {}", evidence.trim());
    match details.map(str::trim).filter(|details| !details.is_empty()) {
        Some(details) if details.lines().any(|existing| existing.trim() == line) => None,
        Some(details) => Some(Some(format!("{details}\n\n{line}"))),
        None => Some(Some(line)),
    }
}

/// The patch with this task's own labels added and removed.
fn relabeled(patch: &WorkPatch, row: &WorkItemRow, add: &[String], remove: &[String]) -> WorkPatch {
    if add.is_empty() && remove.is_empty() {
        return patch.clone();
    }
    let mut label_ids = row.item.label_ids.clone();
    for id in add {
        if !label_ids.contains(id) {
            label_ids.push(id.clone());
        }
    }
    label_ids.retain(|id| !remove.contains(id));
    WorkPatch {
        label_ids: Some(label_ids),
        ..patch.clone()
    }
}

fn milestone(value: &str) -> Result<Option<String>, ExternalError> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    Uuid::parse_str(value)
        .map(|id| Some(id.to_string()))
        .map_err(|_| {
            ExternalError::invalid(format!(
                "{value:?} is not a milestone id. Read a project's milestones with: sona --project PROJECT_ID"
            ))
        })
}

/// Finds one choice by id, then by its exact name, then by a part of its
/// name that only one choice has.
fn pick<'a, T>(
    kind: &str,
    value: &str,
    choices: &'a [T],
    id: impl Fn(&T) -> &String,
    name: impl Fn(&T) -> &String,
) -> Result<&'a T, ExternalError> {
    let wanted = value.trim();
    if let Some(choice) = choices
        .iter()
        .find(|choice| id(choice).eq_ignore_ascii_case(wanted))
    {
        return Ok(choice);
    }
    let lower = wanted.to_lowercase();
    let exact: Vec<&T> = choices
        .iter()
        .filter(|choice| name(choice).to_lowercase() == lower)
        .collect();
    let matches = if exact.is_empty() && !lower.is_empty() {
        choices
            .iter()
            .filter(|choice| name(choice).to_lowercase().contains(&lower))
            .collect()
    } else {
        exact
    };
    match matches.as_slice() {
        [choice] => Ok(*choice),
        [] => Err(ExternalError::invalid(format!(
            "No {kind} is called {wanted:?}."
        ))),
        several => Err(ExternalError::invalid(format!(
            "{wanted:?} matches {} {kind}s: {}. Use the full name or the id.",
            several.len(),
            several
                .iter()
                .take(8)
                .map(|choice| format!("{} ({})", name(choice), id(choice)))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Finds one person by id, then by exact name or alias, then by a part of a
/// name or alias that only one person has. An alias is a name the person goes
/// by, so a spelling the transcript keeps getting wrong finds them.
fn pick_person<'a>(
    value: &str,
    people: &'a [WorkPersonChoice],
) -> Result<&'a WorkPersonChoice, ExternalError> {
    let wanted = value.trim();
    if let Some(person) = people
        .iter()
        .find(|person| person.id.eq_ignore_ascii_case(wanted))
    {
        return Ok(person);
    }
    let lower = wanted.to_lowercase();
    let goes_by = |person: &WorkPersonChoice, matches: &dyn Fn(&str) -> bool| {
        matches(&person.name.to_lowercase())
            || person
                .aliases
                .iter()
                .any(|alias| matches(&alias.to_lowercase()))
    };
    let exact: Vec<&WorkPersonChoice> = people
        .iter()
        .filter(|person| goes_by(person, &|name| name == lower))
        .collect();
    let matches = if exact.is_empty() && !lower.is_empty() {
        people
            .iter()
            .filter(|person| goes_by(person, &|name| name.contains(&lower)))
            .collect()
    } else {
        exact
    };
    match matches.as_slice() {
        [person] => Ok(*person),
        [] => Err(ExternalError::invalid(format!(
            "No person is called {wanted:?}."
        ))),
        several => Err(ExternalError::invalid(format!(
            "{wanted:?} matches {} people: {}. Use the full name or the id.",
            several.len(),
            several
                .iter()
                .take(8)
                .map(|person| format!("{} ({})", person.name, person.id))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn hue_for(name: &str) -> f64 {
    let hash = name.to_lowercase().bytes().fold(0_u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    f64::from(hash % 360)
}

impl Board {
    fn row(&self, id: &str) -> Option<&WorkItemRow> {
        self.rows.iter().find(|row| row.item.id == id)
    }

    fn task(&self, task: &TaskRef) -> Result<&WorkItemRow, ExternalError> {
        let found = match task {
            TaskRef::Number(number) => self.rows.iter().find(|row| row.item.number == *number),
            TaskRef::Id(id) => self.row(id),
        };
        found.ok_or_else(|| {
            let missing =
                ExternalError::new(ExternalErrorCode::NotFound, format!("No task {task}."));
            match left_out(self.fence.is_open()) {
                Some(note) => missing.hint(note),
                None => missing,
            }
        })
    }

    fn tasks(&self, tasks: &[TaskRef]) -> Result<Vec<&WorkItemRow>, ExternalError> {
        let mut found: Vec<&WorkItemRow> = Vec::with_capacity(tasks.len());
        for task in tasks {
            let row = self.task(task)?;
            if !found.iter().any(|seen| seen.item.id == row.item.id) {
                found.push(row);
            }
        }
        Ok(found)
    }

    fn person(&self, value: &str) -> Result<&WorkPersonChoice, ExternalError> {
        if !self.fence.is_open() {
            return Err(conversations_closed());
        }
        pick_person(value, &self.people)
            .map_err(|refusal| refusal.hint("Look people up with: sona --people NAME"))
    }

    fn owner(&self, value: &str) -> Result<WorkOwner, ExternalError> {
        match value.trim().to_lowercase().as_str() {
            "me" | "self" | "you" | "mine" => Ok(WorkOwner::Myself),
            "nobody" | "none" | "no one" | "null" => Ok(WorkOwner::Nobody),
            _ => self.person(value).map(|person| WorkOwner::Person {
                person_id: person.id.clone(),
            }),
        }
    }

    /// `None` for "none": work in no project.
    fn project(&self, value: &str) -> Result<Option<&WorkProjectChoice>, ExternalError> {
        if value.trim().eq_ignore_ascii_case("none") {
            return Ok(None);
        }
        pick(
            "project",
            value,
            &self.projects,
            |project| &project.id,
            |project| &project.title,
        )
        .map(Some)
        .map_err(|refusal| refusal.hint("List them with: sona --projects"))
    }

    fn label_ids(&self, names: &[String]) -> Result<Vec<String>, ExternalError> {
        let mut ids = Vec::with_capacity(names.len());
        for name in names.iter().filter(|name| !name.trim().is_empty()) {
            let label = pick(
                "label",
                name,
                &self.labels,
                |label| &label.id,
                |label| &label.name,
            )
            .map_err(|refusal| {
                refusal.hint(
                    "List them with: sona work labels; make one with: sona work label-save NAME",
                )
            })?;
            if label.retired_at_ms.is_some() {
                return Err(ExternalError::invalid(format!(
                    "The label {:?} is retired. Bring it back with: sona work label-retire {:?} --restore",
                    label.name, label.name
                )));
            }
            if !ids.contains(&label.id) {
                ids.push(label.id.clone());
            }
        }
        Ok(ids)
    }

    fn label_name(&self, id: &str) -> String {
        self.labels
            .iter()
            .find(|label| label.id == id)
            .map_or_else(|| id.to_owned(), |label| label.name.clone())
    }

    fn project_name(&self, id: &str) -> String {
        self.projects
            .iter()
            .find(|project| project.id == id)
            .map_or_else(|| id.to_owned(), |project| project.title.clone())
    }

    fn person_name(&self, id: &str) -> String {
        self.people
            .iter()
            .find(|person| person.id == id)
            .map_or_else(|| id.to_owned(), |person| person.name.clone())
    }

    fn owner_text(&self, owner: &WorkOwner) -> String {
        match owner {
            WorkOwner::Myself => "you".to_owned(),
            WorkOwner::Person { person_id } => self.person_name(person_id),
            WorkOwner::Nobody => "nobody".to_owned(),
        }
    }

    fn task_text(&self, id: &str) -> String {
        self.row(id).map_or_else(
            || id.to_owned(),
            |row| format!("{} {}", handle(row.item.number), row.item.title),
        )
    }

    fn counts(&self, now: i64) -> Result<ExternalWorkCounts, ExternalError> {
        let rows = self.rows.iter().map(|row| (&row.item, row.follow_up_at_ms));
        let counts = items::counts(rows, now).map_err(|_| {
            ExternalError::new(ExternalErrorCode::Failed, "The counts could not be read.")
        })?;
        Ok(ExternalWorkCounts {
            triage: counts.triage,
            mine_open: counts.mine_open,
            mine_overdue: counts.mine_overdue,
            mine_due_today: counts.mine_due_today,
            waiting_on: counts.waiting_on,
            waiting_on_stale: counts.waiting_on_stale,
        })
    }

    /// Open work on one side, as the counts count it.
    fn open_for(
        &self,
        direction: WorkDirection,
        now: i64,
        limit: usize,
    ) -> (Vec<ExternalWorkItem>, bool) {
        let mut rows: Vec<&WorkItemRow> = self
            .rows
            .iter()
            .filter(|row| {
                row.item.direction == direction
                    && row.item.status.is_open()
                    && row.item.archived_at_ms.is_none()
                    && !items::snoozed(&row.item, now)
            })
            .collect();
        rows.sort_by_key(|row| urgency(row));
        let more = rows.len() > limit;
        let tasks = rows
            .into_iter()
            .take(limit)
            .map(|row| project_task(row, self))
            .collect();
        (tasks, more)
    }

    fn patch(&self, args: &UpdateArgs) -> Result<WorkPatch, ExternalError> {
        let fields = &args.fields;
        Ok(WorkPatch {
            title: args.title.as_deref().map(|title| title.trim().to_owned()),
            details: fields.details.as_deref().map(|details| {
                Some(details.trim())
                    .filter(|details| !details.is_empty())
                    .map(str::to_owned)
            }),
            status: fields.status.map(StatusArg::status),
            priority: fields.priority.map(Priority::value),
            project_id: fields
                .project
                .as_deref()
                .map(|project| {
                    self.project(project)
                        .map(|project| project.map(|project| project.id.clone()))
                })
                .transpose()?,
            milestone_id: fields.milestone.as_deref().map(milestone).transpose()?,
            owner: fields
                .owner
                .as_deref()
                .map(|owner| self.owner(owner))
                .transpose()?,
            due_at_ms: fields.due.map(DayArg::ms).transpose()?,
            recurrence: fields.repeat.as_ref().map(RepeatArg::every),
            label_ids: fields
                .labels
                .as_deref()
                .map(|labels| self.label_ids(labels))
                .transpose()?,
            snoozed_until_ms: args.snooze_until.map(DayArg::ms).transpose()?,
            cycle: args
                .cycle
                .map(|cycle| match cycle {
                    WorkCycleRef::None => Ok(None),
                    named => {
                        let number = named_cycle(&self.cycles, named)?;
                        if self.cycles.iter().any(|cycle| {
                            cycle.number == number && cycle.state == WorkCycleState::Past
                        }) {
                            return Err(ExternalError::invalid(format!(
                                "Cycle {number} has ended. Plan work in the current cycle or a later one."
                            )));
                        }
                        Ok(Some(number))
                    }
                })
                .transpose()?,
            parent_id: args
                .parent
                .as_ref()
                .map(|parent| match parent {
                    threads::ParentArg::None => Ok(None),
                    threads::ParentArg::Task(task) => {
                        self.task(task).map(|row| Some(row.item.id.clone()))
                    }
                })
                .transpose()?,
            estimate: args.estimate.map(|EstimateArg(points)| points),
            for_person_id: args
                .for_person
                .as_deref()
                .map(|person| {
                    if person.trim().eq_ignore_ascii_case("none") {
                        Ok(None)
                    } else {
                        self.person(person).map(|person| Some(person.id.clone()))
                    }
                })
                .transpose()?,
        })
    }

    /// What accepting this item sets, from the suggestions Sona shows.
    fn acceptance(&self, row: &WorkItemRow, apply_suggestions: bool) -> WorkPatch {
        let mut patch = WorkPatch {
            status: Some(WorkStatus::NotStarted),
            ..WorkPatch::default()
        };
        if !apply_suggestions {
            return patch;
        }
        for suggestion in self.suggestions.get(&row.item.id).into_iter().flatten() {
            match (&suggestion.field, &suggestion.value) {
                (WorkSuggestionField::Owner, WorkSuggestionValue::Owner(owner)) => {
                    patch.owner = Some(owner.clone());
                }
                (WorkSuggestionField::Due, WorkSuggestionValue::Due(due_at_ms)) => {
                    patch.due_at_ms = Some(Some(*due_at_ms));
                }
                (WorkSuggestionField::Project, WorkSuggestionValue::Id(project_id)) => {
                    patch.project_id = Some(Some(project_id.clone()));
                }
                _ => {}
            }
        }
        patch
    }

    /// The fields this patch would change on this task, as a person reads
    /// them. Empty when the patch would write nothing.
    fn diff(&self, row: &WorkItemRow, patch: &WorkPatch) -> Vec<ExternalFieldChange> {
        let item = &row.item;
        let mut changes = Vec::new();
        let mut field = |field: &'static str, from: String, to: String| {
            if from != to {
                changes.push(ExternalFieldChange { field, from, to });
            }
        };
        if let Some(title) = &patch.title {
            field("title", item.title.clone(), title.trim().to_owned());
        }
        if let Some(details) = &patch.details {
            field(
                "details",
                item.details
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
                details.as_deref().unwrap_or_default().trim().to_owned(),
            );
        }
        if let Some(status) = patch.status {
            field(
                "status",
                status_text(item.status).to_owned(),
                status_text(status).to_owned(),
            );
        }
        if let Some(priority) = patch.priority {
            field(
                "priority",
                Priority::of(item.priority).text().to_owned(),
                Priority::of(priority).text().to_owned(),
            );
        }
        if let Some(owner) = &patch.owner {
            field(
                "owner",
                self.owner_text(&item.owner()),
                self.owner_text(owner),
            );
        }
        if let Some(due) = patch.due_at_ms {
            field("due", day_text(item.due_at_ms), day_text(due));
        }
        if let Some(repeat) = &patch.recurrence {
            let every =
                |every: Option<&String>| every.map_or_else(|| "none".to_owned(), Clone::clone);
            field(
                "repeats",
                every(self.repeats.get(&item.id)),
                every(repeat.as_ref()),
            );
        }
        if let Some(project) = &patch.project_id {
            let name = |id: Option<&String>| {
                id.map_or_else(|| "none".to_owned(), |id| self.project_name(id))
            };
            field(
                "project",
                name(item.project_id.as_ref()),
                name(project.as_ref()),
            );
        }
        if let Some(milestone) = &patch.milestone_id {
            field(
                "milestone",
                item.milestone_id
                    .clone()
                    .unwrap_or_else(|| "none".to_owned()),
                milestone.clone().unwrap_or_else(|| "none".to_owned()),
            );
        }
        if let Some(label_ids) = &patch.label_ids {
            field(
                "labels",
                self.labels_text(&item.label_ids),
                self.labels_text(&self.one_per_group(label_ids, &item.label_ids)),
            );
        }
        if let Some(snoozed) = patch.snoozed_until_ms {
            field(
                "snoozed until",
                day_text(item.snoozed_until_ms),
                day_text(snoozed),
            );
        }
        if let Some(cycle) = patch.cycle {
            let number =
                |cycle: Option<i64>| cycle.map_or_else(|| "none".to_owned(), |n| n.to_string());
            field("cycle", number(item.cycle), number(cycle));
        }
        if let Some(parent) = &patch.parent_id {
            let name =
                |id: Option<&String>| id.map_or_else(|| "none".to_owned(), |id| self.task_text(id));
            field(
                "parent",
                name(item.parent_id.as_ref()),
                name(parent.as_ref()),
            );
        }
        if let Some(estimate) = patch.estimate {
            let points =
                |points: Option<u8>| points.map_or_else(|| "none".to_owned(), |n| n.to_string());
            field("estimate", points(item.estimate), points(estimate));
        }
        if let Some(for_person) = &patch.for_person_id {
            let name = |id: Option<&String>| {
                id.map_or_else(|| "none".to_owned(), |id| self.person_name(id))
            };
            field(
                "for",
                name(item.for_person_id.as_ref()),
                name(for_person.as_ref()),
            );
        }
        changes
    }

    fn one_per_group(&self, ids: &[String], kept: &[String]) -> Vec<String> {
        items::keep_one_per_group(ids, kept, |id| {
            self.labels
                .iter()
                .find(|label| label.id == id)
                .and_then(|label| label.group.clone())
        })
    }

    fn labels_text(&self, ids: &[String]) -> String {
        let mut names: Vec<String> = ids.iter().map(|id| self.label_name(id)).collect();
        names.sort();
        names.dedup();
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        }
    }

    /// One task's dry-run line. `commitment` overrides what the status change
    /// alone would do to the meeting commitment behind it.
    fn planned(
        &self,
        row: &WorkItemRow,
        patch: &WorkPatch,
        commitment: Option<Option<CommitmentEffect>>,
    ) -> ExternalWorkPlanned {
        let commitment = commitment.unwrap_or_else(|| {
            let from_meeting = self.rows.iter().any(|member| {
                (member.item.id == row.item.id
                    || member.item.duplicate_of_id.as_deref() == Some(row.item.id.as_str()))
                    && member.item.source_loop_id.is_some()
            });
            patch
                .status
                .filter(|_| from_meeting)
                .and_then(|status| commitment_effect(row.item.status, status))
        });
        ExternalWorkPlanned {
            id: row.item.id.clone(),
            handle: handle(row.item.number),
            title: row.item.title.clone(),
            fields: self.diff(row, patch),
            commitment,
            skipped: None,
        }
    }

    /// Picks the two ends of a link in the order the tracker stores them.
    fn link(
        &self,
        args: &RelateArgs,
    ) -> Result<(&WorkItemRow, &WorkItemRow, WorkRelationKind), ExternalError> {
        let task = self.task(&args.task)?;
        let other = self.task(&args.other)?;
        if task.item.id == other.item.id {
            return Err(ExternalError::invalid("A task cannot link to itself."));
        }
        Ok(match args.link {
            LinkKind::Blocks => (task, other, WorkRelationKind::Blocks),
            LinkKind::BlockedBy => (other, task, WorkRelationKind::Blocks),
            LinkKind::Related => (task, other, WorkRelationKind::Related),
        })
    }

    /// The row the write returned, with the names this board read.
    fn project_item(&self, item: WorkItem) -> ExternalWorkItem {
        let before = self.row(&item.id);
        let row = WorkItemRow {
            project_title: item.project_id.as_deref().map(|id| self.project_name(id)),
            owner_name: item
                .owner_person_id
                .as_deref()
                .map(|id| self.person_name(id)),
            source_meeting_id: before.and_then(|row| row.source_meeting_id.clone()),
            source_meeting_title: before.and_then(|row| row.source_meeting_title.clone()),
            source_quote: before.and_then(|row| row.source_quote.clone()),
            source_speaker: before.and_then(|row| row.source_speaker.clone()),
            source_at_ms: before.and_then(|row| row.source_at_ms),
            blocked_by_open_count: before.map_or(0, |row| row.blocked_by_open_count),
            doer: before.and_then(|row| row.doer.clone()),
            raised_in: before.map_or(0, |row| row.raised_in),
            source: before.map_or(WorkSource::Agent, |row| row.source),
            follow_up_at_ms: before.and_then(|row| row.follow_up_at_ms),
            at_risk: before.is_some_and(|row| row.at_risk),
            time_block: before.and_then(|row| row.time_block),
            repeat: before.and_then(|row| row.repeat.clone()),
            sub_tasks: before.and_then(|row| row.sub_tasks),
            attachment_count: before.map_or(0, |row| row.attachment_count),
            item,
        };
        project_task(&row, self)
    }

    fn detail(&self, detail: WorkItemDetail) -> ExternalWorkDetail {
        let id = detail.item.item.id.clone();
        let here_closed = detail.item.item.status.is_closed();
        let relations = detail
            .relations
            .iter()
            .filter_map(|relation| {
                let outgoing = relation.from_id == id;
                let other = if outgoing {
                    &relation.to_id
                } else {
                    &relation.from_id
                };
                let row = self.row(other)?;
                let blocker_closed = if outgoing {
                    here_closed
                } else {
                    row.item.status.is_closed()
                };
                let kind = match relation.kind {
                    WorkRelationKind::Related => ExternalLinkKind::Related,
                    WorkRelationKind::Blocks if blocker_closed => ExternalLinkKind::Related,
                    WorkRelationKind::Blocks if outgoing => ExternalLinkKind::Blocks,
                    WorkRelationKind::Blocks => ExternalLinkKind::BlockedBy,
                };
                Some(ExternalWorkLink {
                    id: relation.id.clone(),
                    kind,
                    task: task_ref_of(row),
                })
            })
            .collect();
        let evidence = detail
            .evidence
            .into_iter()
            .filter(|evidence| !self.fence.hides_evidence(evidence))
            .map(project_evidence)
            .collect();
        let suggestions = detail
            .suggestions
            .iter()
            .filter(|suggestion| !self.fence.hides_suggestion(suggestion))
            .map(|suggestion| self.suggestion(suggestion))
            .collect();
        let heard = detail.item.item.source_loop_id.is_some();
        let activity = detail
            .activity
            .iter()
            .map(|entry| {
                let mut line = self.activity(entry);
                if heard && matches!(entry.kind, WorkActivityKind::Created) {
                    line.meeting_id.clone_from(&detail.item.source_meeting_id);
                    line.meeting_title.clone_from(&detail.item.source_meeting_title);
                }
                line
            })
            .collect();
        let parent = detail
            .parent
            .as_ref()
            .and_then(|parent| self.row(&parent.id))
            .map(task_ref_of);
        let sub_tasks = detail
            .sub_tasks
            .iter()
            .filter_map(|item| self.row(&item.id))
            .map(|row| project_task(row, self))
            .collect();
        ExternalWorkDetail {
            parent,
            sub_tasks,
            comments: detail
                .comments
                .into_iter()
                .map(threads::project_comment)
                .collect(),
            attachments: detail
                .attachments
                .into_iter()
                .map(attachments::project_attachment)
                .collect(),
            reminder: None,
            sessions: Vec::new(),
            schema_version: QUERY_SCHEMA_VERSION,
            conversations_available: self.fence.is_open(),
            task: project_task(&detail.item, self),
            recurrence: detail.recurrence,
            relations,
            evidence,
            suggestions,
            activity,
            proposals: detail
                .proposals
                .iter()
                .map(|proposal| self.proposal(proposal))
                .collect(),
        }
    }

    fn suggestion(&self, suggestion: &WorkFieldSuggestion) -> ExternalWorkSuggestion {
        let (value, id, due_at_ms) = match (&suggestion.field, &suggestion.value) {
            (_, WorkSuggestionValue::Owner(owner)) => (
                self.owner_text(owner),
                match owner {
                    WorkOwner::Person { person_id } => Some(person_id.clone()),
                    WorkOwner::Myself | WorkOwner::Nobody => None,
                },
                None,
            ),
            (_, WorkSuggestionValue::Due(due_at_ms)) => {
                (day_text(Some(*due_at_ms)), None, Some(*due_at_ms))
            }
            (WorkSuggestionField::Project, WorkSuggestionValue::Id(id)) => {
                (self.project_name(id), Some(id.clone()), None)
            }
            (WorkSuggestionField::Label, WorkSuggestionValue::Id(id)) => {
                (self.label_name(id), Some(id.clone()), None)
            }
            (_, WorkSuggestionValue::Id(id)) => (self.task_text(id), Some(id.clone()), None),
        };
        ExternalWorkSuggestion {
            suggestion_id: suggestion.id.clone(),
            field: suggestion.field,
            value,
            id,
            due_at_ms,
            reason: suggestion.reason.clone(),
            quote: suggestion.quote.clone(),
            meeting_id: suggestion.meeting_id.clone(),
            at_ms: suggestion.at_ms,
        }
    }

    /// One stored history value read as a name or a day.
    fn stored(&self, kind: WorkActivityKind, value: &str) -> String {
        match kind {
            WorkActivityKind::Status
            | WorkActivityKind::Created
            | WorkActivityKind::Accepted
            | WorkActivityKind::Declined => status_named(value)
                .map_or_else(|| value.to_owned(), |status| status_text(status).to_owned()),
            WorkActivityKind::Duplicate => status_named(value).map_or_else(
                || self.task_text(value),
                |status| status_text(status).to_owned(),
            ),
            WorkActivityKind::Priority => value.parse::<u8>().map_or_else(
                |_| value.to_owned(),
                |priority| Priority::of(priority).text().to_owned(),
            ),
            WorkActivityKind::Owner => match value {
                "self" => "you".to_owned(),
                "nobody" => "nobody".to_owned(),
                person_id => self.person_name(person_id),
            },
            WorkActivityKind::Due | WorkActivityKind::Snoozed | WorkActivityKind::Block => value
                .parse::<i64>()
                .map_or_else(|_| value.to_owned(), |ms| day_text(Some(ms))),
            WorkActivityKind::Cycle => format!("cycle {value}"),
            WorkActivityKind::Project => self.project_name(value),
            WorkActivityKind::Labels => {
                let ids: Vec<String> = value
                    .split(',')
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
                    .collect();
                self.labels_text(&ids)
            }
            WorkActivityKind::Relation => match value.split_once(':') {
                Some((kind, id)) => {
                    format!("{} {}", kind.replace('_', " "), self.task_text(id))
                }
                None => value.to_owned(),
            },
            WorkActivityKind::Parent => self.task_text(value),
            WorkActivityKind::ForPerson => self.person_name(value),
            WorkActivityKind::Estimate => format!("{value} pt"),
            WorkActivityKind::Title
            | WorkActivityKind::Details
            | WorkActivityKind::Archived
            | WorkActivityKind::Restored
            | WorkActivityKind::Kept
            | WorkActivityKind::Recurrence
            | WorkActivityKind::Comment
            | WorkActivityKind::Attachment => value.to_owned(),
        }
    }

    /// One history entry with ids and stored values read as names and days.
    fn activity(&self, entry: &WorkActivity) -> ExternalWorkActivity {
        let read =
            |value: &Option<String>| value.as_deref().map(|value| self.stored(entry.kind, value));
        ExternalWorkActivity {
            at_ms: entry.at_ms,
            actor: entry.actor,
            kind: entry.kind,
            from: read(&entry.from),
            to: read(&entry.to),
            agent: entry.agent.clone(),
            via: entry.via,
            meeting_id: None,
            meeting_title: None,
            reason: entry.reason.clone(),
        }
    }

    /// A proposal with both values read as names and days.
    fn proposal(&self, proposal: &WorkProposal) -> ExternalWorkProposal {
        let kind = match proposal.field {
            WorkProposalField::Title => Some(WorkActivityKind::Title),
            WorkProposalField::Details => Some(WorkActivityKind::Details),
            WorkProposalField::Status => Some(WorkActivityKind::Status),
            WorkProposalField::Priority => Some(WorkActivityKind::Priority),
            WorkProposalField::Owner => Some(WorkActivityKind::Owner),
            WorkProposalField::Due => Some(WorkActivityKind::Due),
            WorkProposalField::Project => Some(WorkActivityKind::Project),
            WorkProposalField::Labels => Some(WorkActivityKind::Labels),
            WorkProposalField::Snoozed => Some(WorkActivityKind::Snoozed),
            WorkProposalField::Recurrence => Some(WorkActivityKind::Recurrence),
            WorkProposalField::Cycle => Some(WorkActivityKind::Cycle),
            WorkProposalField::Parent => Some(WorkActivityKind::Parent),
            WorkProposalField::Estimate => Some(WorkActivityKind::Estimate),
            WorkProposalField::ForPerson => Some(WorkActivityKind::ForPerson),
            WorkProposalField::Milestone => None,
        };
        let read = |value: &Option<String>| match (value.as_deref(), kind) {
            (None, _) => "none".to_owned(),
            (Some(value), Some(kind)) => self.stored(kind, value),
            (Some(value), None) => value.to_owned(),
        };
        ExternalWorkProposal {
            id: proposal.id.clone(),
            task_id: proposal.item_id.clone(),
            handle: self
                .row(&proposal.item_id)
                .map_or_else(|| proposal.item_id.clone(), |row| handle(row.item.number)),
            agent: proposal.agent.clone(),
            field: proposal.field,
            from: read(&proposal.from),
            to: read(&proposal.to),
            created_at_ms: proposal.created_at_ms,
        }
    }

    /// The answer to an agent's change: the tasks it changed, and the
    /// changes left waiting for the user.
    fn agent_changed(
        &self,
        action: WorkAction,
        operation_id: String,
        changes: Vec<AgentChange>,
        skipped: Vec<ExternalWorkSkip>,
    ) -> ExternalWorkChange {
        let mut tasks = Vec::new();
        let mut proposed = Vec::new();
        for change in changes {
            proposed.extend(
                change
                    .proposed
                    .iter()
                    .map(|proposal| self.proposal(proposal)),
            );
            if self
                .row(&change.item.id)
                .is_none_or(|row| row.item.revision != change.item.revision)
            {
                tasks.push(self.project_item(change.item));
            }
        }
        ExternalWorkChange {
            schema_version: QUERY_SCHEMA_VERSION,
            action,
            operation_id: Some(operation_id),
            tasks,
            skipped,
            proposed,
            replayed: false,
            evidence_url: None,
        }
    }
}

/// A link between two tasks the caller named, ends in the stored order.
fn relation_between(
    relation: &WorkRelation,
    one: &WorkItemRow,
    other: &WorkItemRow,
) -> ExternalWorkRelation {
    let end = |id: &str| task_ref_of(if one.item.id == id { one } else { other });
    ExternalWorkRelation {
        id: relation.id.clone(),
        kind: relation.kind,
        from: end(&relation.from_id),
        to: end(&relation.to_id),
    }
}

fn project_view(view: &WorkView) -> ExternalWorkView {
    let query = &view.query;
    ExternalWorkView {
        id: view.id.clone(),
        name: view.name.clone(),
        filters: ExternalWorkFilters {
            statuses: query.statuses.clone(),
            direction: query.direction,
            owner_person_ids: query.owner_person_ids.clone(),
            project_ids: query.project_ids.clone(),
            label_ids: query.label_ids.clone(),
            priorities: query
                .priorities
                .as_ref()
                .map(|priorities| priorities.iter().copied().map(Priority::of).collect()),
            due_before_ms: query.due_before_ms,
            due: query
                .due
                .as_ref()
                .map(|due| due.iter().copied().map(due_flag).collect()),
            meeting_id: query.source_meeting_id.clone(),
            search: query.search.clone(),
            include_archived: query.include_archived,
            include_snoozed: query.include_snoozed,
            cycle: query.cycle,
            conditions: query.filters.clone(),
        },
        layout: view.display.layout,
        group_by: view.display.group_by,
        order_by: view.display.order_by,
        show_empty_groups: view.display.show_empty_groups,
        properties: view.display.properties.clone(),
        show_sub_tasks: view.display.show_sub_tasks,
        sub_group_by: view.display.sub_group_by,
        hidden_columns: view.display.hidden_columns.clone(),
        revision: view.revision,
    }
}

impl Scope {
    fn narrow(&self, board: &Board, mut query: WorkQuery) -> Result<WorkQuery, ExternalError> {
        if self.mine {
            query.direction = Some(WorkDirection::Mine);
        }
        if self.waiting {
            query.direction = Some(WorkDirection::WaitingOn);
        }
        if self.unowned {
            query.direction = Some(WorkDirection::Unattributed);
        }
        if !self.owner.is_empty() {
            let mut people = Vec::new();
            let mut sides = Vec::new();
            for owner in &self.owner {
                match board.owner(owner)? {
                    WorkOwner::Myself => sides.push(WorkDirection::Mine),
                    WorkOwner::Nobody => sides.push(WorkDirection::Unattributed),
                    WorkOwner::Person { person_id } => people.push(person_id),
                }
            }
            match (sides.as_slice(), people.is_empty()) {
                ([], _) => query.owner_person_ids = Some(people),
                ([side], true) if !(self.mine || self.waiting || self.unowned) => {
                    query.direction = Some(*side);
                }
                _ => {
                    return Err(ExternalError::invalid(
                        "--owner me or nobody goes on its own: not with other owners, or with --mine, --waiting or --unowned.",
                    ))
                }
            }
        }
        if !self.project.is_empty() {
            let mut ids = Vec::with_capacity(self.project.len());
            for project in &self.project {
                ids.push(
                    board
                        .project(project)?
                        .map_or_else(|| "none".to_owned(), |project| project.id.clone()),
                );
            }
            query.project_ids = Some(ids);
        }
        if !self.label.is_empty() {
            query.label_ids = Some(board.label_ids(&self.label)?);
        }
        if !self.priority.is_empty() {
            query.priorities = Some(
                self.priority
                    .iter()
                    .map(|priority| priority.value())
                    .collect(),
            );
        }
        if let Some(day) = self.due_by {
            let next = day
                .succ_opt()
                .ok_or_else(|| ExternalError::invalid("--due-by is past the calendar."))?;
            query.due_before_ms = Some(midnight(next)?);
        }
        if !self.due.is_empty() {
            query.due = Some(self.due.clone());
        }
        if let Some(meeting) = self.meeting {
            match &board.fence {
                Fence::Closed => return Err(conversations_closed()),
                Fence::Open(kept_local)
                    if kept_local.hides(MeetingSessionId::from_uuid(meeting)) =>
                {
                    return Err(ExternalError::new(
                        ExternalErrorCode::NotFound,
                        format!("No meeting {meeting}."),
                    ));
                }
                Fence::Open(_) => {}
            }
            query.source_meeting_id = Some(meeting.to_string());
        }
        if let Some(search) = &self.search {
            query.search = Some(match task_ref(search) {
                Ok(TaskRef::Number(number)) => format!("#{number}"),
                _ => search.clone(),
            });
        }
        if self.cycle.is_some() {
            query.cycle = self.cycle;
        }
        Ok(query)
    }
}

/// The cycle a caller named, among the cycles there are.
fn named_cycle(cycles: &[WorkCycle], named: WorkCycleRef) -> Result<i64, ExternalError> {
    let current = cycles
        .iter()
        .find(|cycle| cycle.state == WorkCycleState::Current);
    let found = match named {
        WorkCycleRef::Current => current,
        WorkCycleRef::Next => current.and_then(|current| {
            cycles
                .iter()
                .find(|cycle| cycle.starts_at_ms >= current.ends_at_ms)
        }),
        WorkCycleRef::Number { number } => cycles.iter().find(|cycle| cycle.number == number),
        WorkCycleRef::None => None,
    };
    found.map(|cycle| cycle.number).ok_or_else(|| {
        ExternalError::new(
            ExternalErrorCode::NotFound,
            "No such cycle. List them with: sona work cycles",
        )
    })
}

fn external_cycles(listing: &WorkCycles) -> ExternalWorkCycles {
    ExternalWorkCycles {
        schema_version: QUERY_SCHEMA_VERSION,
        weeks: listing.settings.weeks,
        start_weekday: listing.settings.start_weekday,
        cooldown_weeks: listing.settings.cooldown_weeks,
        upcoming: listing.settings.upcoming,
        auto_add_started: listing.settings.auto_add_started,
        auto_add_completed: listing.settings.auto_add_completed,
        capacity: listing.capacity,
        cycles: listing.cycles.iter().map(project_cycle).collect(),
        calendar: None,
    }
}

fn project_cycle(cycle: &WorkCycle) -> ExternalWorkCycle {
    let last_day = NaiveDate::parse_from_str(&cycle.end_day, "%Y-%m-%d")
        .ok()
        .and_then(|end| end.pred_opt())
        .map_or_else(
            || cycle.end_day.clone(),
            |last| last.format("%Y-%m-%d").to_string(),
        );
    ExternalWorkCycle {
        number: cycle.number,
        name: cycle.name.clone(),
        state: cycle.state,
        first_day: cycle.start_day.clone(),
        last_day,
        scope: cycle.scope,
        started: cycle.started,
        done: cycle.done,
        success: cycle.success,
    }
}

impl Filters {
    fn narrow(&self, board: &Board, mut query: WorkQuery) -> Result<WorkQuery, ExternalError> {
        if !self.status.is_empty() {
            let mut statuses: Vec<WorkStatus> = Vec::new();
            for status in self.status.iter().flat_map(|filter| filter.statuses()) {
                if !statuses.contains(status) {
                    statuses.push(*status);
                }
            }
            query.statuses = Some(statuses);
        }
        if self.archived {
            query.include_archived = true;
        }
        if self.snoozed {
            query.include_snoozed = true;
        }
        if let Some(ConditionsArg(conditions)) = &self.filter {
            query.filters.extend(conditions.iter().cloned());
        }
        if let Some(parent) = &self.parent {
            let value = match parent {
                ParentArg::Task(task) => board.task(task)?.item.id.clone(),
                ParentArg::None => "none".to_owned(),
            };
            query.filters.push(WorkCondition {
                field: WorkFilterField::Parent,
                op: WorkFilterOp::Is,
                values: vec![value],
            });
        }
        if let Some(scope) = self.cycle_scope {
            let value = match scope {
                CycleScopeArg::Planned => "planned",
                CycleScopeArg::Added => "added",
            };
            query.filters.push(WorkCondition {
                field: WorkFilterField::CycleScope,
                op: WorkFilterOp::Is,
                values: vec![value.to_owned()],
            });
        }
        if !self.for_person.is_empty() {
            let values = self
                .for_person
                .iter()
                .map(|person| board.person(person).map(|person| person.id.clone()))
                .collect::<Result<Vec<_>, _>>()?;
            query.filters.push(WorkCondition {
                field: WorkFilterField::For,
                op: WorkFilterOp::AnyOf,
                values,
            });
        }
        if let Some(completed) = &self.completed {
            if self.status.is_empty() {
                query.statuses = Some(vec![WorkStatus::Done]);
            }
            query.filters.push(WorkCondition {
                field: WorkFilterField::Completed,
                op: WorkFilterOp::Is,
                values: vec![completed.trim().to_owned()],
            });
        }
        self.scope.narrow(board, query)
    }
}

fn commitment_effect(from: WorkStatus, to: WorkStatus) -> Option<CommitmentEffect> {
    match (from.is_closed(), to) {
        (_, WorkStatus::Done) if from != WorkStatus::Done => Some(CommitmentEffect::Closes),
        (_, WorkStatus::Canceled) if from != WorkStatus::Canceled => {
            Some(CommitmentEffect::SetsAside)
        }
        (
            true,
            WorkStatus::Triage
            | WorkStatus::Backlog
            | WorkStatus::NotStarted
            | WorkStatus::InProgress,
        ) => Some(CommitmentEffect::Reopens),
        _ => None,
    }
}

fn status_named(value: &str) -> Option<WorkStatus> {
    serde_json::from_value(serde_json::Value::String(value.to_owned())).ok()
}

const fn status_text(status: WorkStatus) -> &'static str {
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

fn iso_day(utc_ms: i64) -> Option<String> {
    local_day(utc_ms).map(|day| day.format("%Y-%m-%d").to_string())
}

fn day_text(utc_ms: Option<i64>) -> String {
    utc_ms
        .and_then(iso_day)
        .unwrap_or_else(|| "none".to_owned())
}

fn task_ref_of(row: &WorkItemRow) -> ExternalWorkRef {
    ExternalWorkRef {
        id: row.item.id.clone(),
        handle: handle(row.item.number),
        title: row.item.title.clone(),
        status: row.item.status,
    }
}

fn project_label(label: WorkLabel) -> ExternalWorkLabel {
    ExternalWorkLabel {
        id: label.id,
        name: label.name,
        hue: label.hue,
        revision: label.revision,
        group: label.group,
        retired_at_ms: label.retired_at_ms,
        scope: label.scope,
    }
}

fn project_task(row: &WorkItemRow, board: &Board) -> ExternalWorkItem {
    let item = &row.item;
    ExternalWorkItem {
        id: item.id.clone(),
        handle: handle(item.number),
        number: item.number,
        title: item.title.clone(),
        details: item.details.clone(),
        status: item.status,
        priority: Priority::of(item.priority),
        owner: match item.owner() {
            WorkOwner::Myself => ExternalWorkOwner::Myself,
            WorkOwner::Person { person_id } => ExternalWorkOwner::Person {
                person_id,
                name: row.owner_name.clone(),
            },
            WorkOwner::Nobody => ExternalWorkOwner::Nobody,
        },
        doer: row.doer.as_ref().map(|doer| doer.agent.clone()),
        direction: item.direction,
        due_date: item.due_at_ms.and_then(iso_day),
        due_at_ms: item.due_at_ms,
        project: item.project_id.as_ref().map(|id| ExternalWorkName {
            id: id.clone(),
            name: row.project_title.clone().unwrap_or_else(|| id.clone()),
        }),
        milestone_id: item.milestone_id.clone(),
        labels: item
            .label_ids
            .iter()
            .map(|id| ExternalWorkName {
                id: id.clone(),
                name: board.label_name(id),
            })
            .collect(),
        source: item
            .source_loop_id
            .as_ref()
            .map(|loop_id| ExternalWorkSource {
                loop_id: loop_id.clone(),
                link: loop_link(&MeetingLoopId(loop_id.clone())),
                meeting_id: row.source_meeting_id.clone(),
                meeting_title: row.source_meeting_title.clone(),
                quote: row.source_quote.clone(),
                speaker: row.source_speaker.clone(),
                at_ms: row.source_at_ms,
            }),
        duplicate_of_id: item.duplicate_of_id.clone(),
        blocked_by_open: row.blocked_by_open_count,
        snoozed_until_ms: item.snoozed_until_ms,
        follow_up_at_ms: row.follow_up_at_ms,
        focus: board.focus.contains(&item.id),
        archived_at_ms: item.archived_at_ms,
        status_changed_at_ms: item.status_changed_at_ms,
        completed_at_ms: item.completed_at_ms,
        estimate: item.estimate,
        for_person: item.for_person_id.as_ref().map(|id| ExternalWorkName {
            id: id.clone(),
            name: board.person_name(id),
        }),
        created_at_ms: item.created_at_ms,
        updated_at_ms: item.updated_at_ms,
        revision: item.revision,
        cycle: item.cycle,
        at_risk: row.at_risk,
        time_block: row.time_block.map(|block| ExternalWorkBlock {
            starts_at_ms: block.starts_at_ms,
            ends_at_ms: block.ends_at_ms,
        }),
        repeat: row.repeat.clone(),
        parent: item
            .parent_id
            .as_deref()
            .and_then(|id| board.row(id))
            .map(task_ref_of),
        sub_tasks: row.sub_tasks,
        attachment_count: row.attachment_count,
        link: task_link(&item.id),
    }
}

/// One task as a Markdown file: YAML front matter, then the title, details
/// and the words it came from.
fn markdown(task: &ExternalWorkItem) -> String {
    let quoted = |value: &str| serde_json::Value::String(value.to_owned()).to_string();
    let mut text = String::from("---\n");
    let _ = writeln!(text, "id: {}", task.id);
    let _ = writeln!(text, "handle: {}", task.handle);
    let _ = writeln!(text, "title: {}", quoted(&task.title));
    let _ = writeln!(text, "status: {}", status_text(task.status).to_lowercase());
    let _ = writeln!(
        text,
        "priority: {}",
        Priority::text(task.priority).to_lowercase()
    );
    let owner = match &task.owner {
        ExternalWorkOwner::Myself => "me".to_owned(),
        ExternalWorkOwner::Person { name, person_id } => {
            quoted(name.as_deref().unwrap_or(person_id))
        }
        ExternalWorkOwner::Nobody => "nobody".to_owned(),
    };
    let _ = writeln!(text, "owner: {owner}");
    if let Some(due) = &task.due_date {
        let _ = writeln!(text, "due: {due}");
    }
    if let Some(points) = task.estimate {
        let _ = writeln!(text, "estimate: {points}");
    }
    if let Some(person) = &task.for_person {
        let _ = writeln!(text, "for: {}", quoted(&person.name));
    }
    if let Some(completed) = task.completed_at_ms {
        let _ = writeln!(text, "completed: {}", local_time(completed));
    }
    if let Some(project) = &task.project {
        let _ = writeln!(text, "project: {}", quoted(&project.name));
    }
    let labels: Vec<String> = task
        .labels
        .iter()
        .map(|label| quoted(&label.name))
        .collect();
    let _ = writeln!(text, "labels: [{}]", labels.join(", "));
    if let Some(source) = &task.source {
        let _ = writeln!(text, "source: {}", source.link);
        if let Some(meeting) = &source.meeting_title {
            let _ = writeln!(text, "meeting: {}", quoted(meeting));
        }
    }
    if task.archived_at_ms.is_some() {
        text.push_str("archived: true\n");
    }
    let _ = writeln!(text, "updated: {}", local_time(task.updated_at_ms));
    let _ = writeln!(text, "revision: {}", task.revision);
    let _ = writeln!(text, "---\n\n# {}", task.title);
    if let Some(details) = &task.details {
        let _ = writeln!(text, "\n{details}");
    }
    if let Some(quote) = task
        .source
        .as_ref()
        .and_then(|source| source.quote.as_deref())
    {
        text.push('\n');
        for line in quote.lines() {
            let _ = writeln!(text, "> {line}");
        }
        if let Some(said) = task.source.as_ref().and_then(said_by) {
            let _ = writeln!(text, ">\n> {said}");
        }
    }
    text
}

/// "Dana, Weekly sync at 12:04", as much of it as is known.
fn said_by(source: &ExternalWorkSource) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(speaker) = &source.speaker {
        parts.push(speaker.clone());
    }
    match (&source.meeting_title, source.at_ms) {
        (Some(title), Some(at_ms)) => parts.push(format!("{title} at {}", offset(at_ms))),
        (Some(title), None) => parts.push(title.clone()),
        (None, Some(at_ms)) => parts.push(format!("at {}", offset(at_ms))),
        (None, None) => {}
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn offset(at_ms: i64) -> String {
    let seconds = at_ms.max(0) / 1000;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

/// The local day and time the text form measures due days and snoozes by.
struct Today {
    day: String,
    now_ms: i64,
}

impl WorkAnswer {
    /// The answer as plain text for a person at a terminal.
    pub(super) fn text(&self) -> String {
        let now_ms = now_ms();
        let today = Today {
            day: iso_day(now_ms).unwrap_or_default(),
            now_ms,
        };
        let mut text = String::new();
        match self {
            Self::Page(page) => {
                if let Some(read_as) = &page.read_as {
                    words::text(read_as, &mut text);
                }
                if page.entries.tasks.is_empty() {
                    text.push_str("No tasks match.\n");
                }
                for task in &page.entries.tasks {
                    task_line(&mut text, task, &today);
                }
                if page.has_more {
                    let _ = writeln!(
                        text,
                        "{} of {} shown. Raise --limit or narrow the filters.",
                        page.entries.tasks.len(),
                        page.total
                    );
                }
                if let Some(cursor) = &page.next_cursor {
                    let _ = writeln!(text, "More: --cursor {cursor}");
                }
            }
            Self::Mine(mine) => {
                counts_line(&mut text, &mine.counts);
                section(&mut text, "You owe", &mine.i_owe, &today);
                section(&mut text, "Waiting on", &mine.waiting_on, &today);
                if mine.has_more {
                    text.push_str("\nMore not shown. Raise --limit.\n");
                }
            }
            Self::Counts(counts) => counts_line(&mut text, &counts.counts),
            Self::Detail(detail) => detail_text(&mut text, detail, &today),
            Self::Change(change) => change_text(&mut text, change, &today),
            Self::Preview(preview) => preview_text(&mut text, preview),
            Self::Relation(change) => {
                let relation = &change.relation;
                let link = match relation.kind {
                    WorkRelationKind::Blocks => "blocks",
                    WorkRelationKind::Related => "is related to",
                };
                if change.dry_run {
                    text.push_str("Dry run. ");
                }
                let _ = writeln!(
                    text,
                    "{} {} {link} {} {}.",
                    if change.removed {
                        "Unlinked:"
                    } else {
                        "Linked:"
                    },
                    relation.from.handle,
                    relation.to.handle,
                    relation.to.title
                );
            }
            Self::Labels(labels) => {
                if labels.labels.is_empty() {
                    text.push_str("No labels yet. Make one with: sona work label-save NAME\n");
                }
                for label in &labels.labels {
                    let _ = writeln!(text, "{}", routines::label_text(label));
                }
            }
            Self::Projects(_)
            | Self::Project(_)
            | Self::Milestones(_)
            | Self::Milestone(_)
            | Self::People(_)
            | Self::Person(_)
            | Self::Meetings(_)
            | Self::MeetingDetail(_) => catalog::text(self, &mut text),
            Self::Timeline(timeline) => timeline::text(timeline, &mut text),
            Self::Insights(insights) => insights::text(insights, &mut text),
            Self::ProjectDetail(_)
            | Self::ProjectUpdates(_)
            | Self::ProjectUpdate(_)
            | Self::ProjectDraft(_)
            | Self::MilestoneOrder(_)
            | Self::CycleChange(_) => roadmap::text(self, &mut text),
            Self::ProjectDependencies(_) | Self::ProjectDependency(_) => {
                dependencies::text(self, &mut text);
            }
            Self::Initiatives(_) | Self::InitiativeDetail(_) | Self::InitiativeChange(_) => {
                initiatives::text(self, &mut text);
            }
            Self::ProjectTemplates(_) | Self::ProjectTemplate(_) | Self::ProjectNew(_) => {
                project_templates::text(self, &mut text);
            }
            Self::Comments(_) | Self::Comment(_) => threads::text(self, &mut text),
            Self::Attachments(_) | Self::Attachment(_) => attachments::text(self, &mut text),
            Self::Organize(_) | Self::Day(_) | Self::Brief(_) => organize::text(self, &mut text),
            Self::Speaker(_) => speakers::text(self, &mut text),
            Self::PersonPage(_)
            | Self::Companies(_)
            | Self::Company(_)
            | Self::FirstMet(_)
            | Self::PeopleChange(_) => people::text(self, &mut text),
            Self::SpeakerVoices(_) | Self::VoiceChange(_) => voices::text(self, &mut text),
            Self::Sources(answer) => sources::text(answer, &mut text),
            Self::Library(answer) => library::text(answer, &mut text),
            Self::Transcript(answer) => transcript::text(answer, &mut text),
            Self::Dictations(answer) => dictations::text(answer, &mut text),
            Self::Settings(_) => preferences::text(self, &mut text),
            Self::Note(_) => notes::text(self, &mut text),
            Self::Subjects(_) => subjects::text(self, &mut text),
            Self::Planner(_) => planner::text(self, &mut text),
            Self::TriageHints(_) => triage_hints::text(self, &mut text),
            Self::ProjectAdmin(answer) => catalog::admin_text(answer, &mut text),
            Self::Changes(_) => save::text(self, &mut text),
            Self::Sessions(_) | Self::Session(_) => sessions::text(self, &mut text),
            Self::Digest(answer) => digest::text(answer, &mut text),
            Self::Lifecycle(_)
            | Self::Archive(_)
            | Self::Versions(_)
            | Self::Favorites(_)
            | Self::Subscriptions(_)
            | Self::Inbox(_) => lifecycle::text(self, &mut text),
            Self::LabelEdit(_)
            | Self::Templates(_)
            | Self::Template(_)
            | Self::Reminders(_)
            | Self::Reminder(_) => routines::text(self, &mut text),
            Self::AgentLoops(_)
            | Self::AgentLoop(_)
            | Self::AgentLoopChange(_)
            | Self::AgentLoopRun(_) => agent_loops::text(self, &mut text),
            Self::Label(change) => {
                let verb = match (change.deleted, &change.operation_id) {
                    (true, _) => "Deleted",
                    (false, None) => "Unchanged",
                    (false, Some(_)) => "Saved",
                };
                if change.dry_run {
                    text.push_str("Dry run. ");
                }
                let _ = writeln!(
                    text,
                    "{verb} label {} (hue {}).",
                    change.label.name, change.label.hue
                );
            }
            Self::Views(views) => {
                if views.views.is_empty() {
                    text.push_str("No saved views.\n");
                }
                for view in &views.views {
                    let _ = writeln!(text, "{}  · {}", view.name, filters_text(&view.filters));
                }
            }
            Self::View(change) => {
                if change.dry_run {
                    text.push_str("Dry run. ");
                }
                let _ = writeln!(
                    text,
                    "{} view {}  · {}",
                    if change.deleted { "Deleted" } else { "Saved" },
                    change.view.name,
                    filters_text(&change.view.filters)
                );
            }
            Self::Export(export) => {
                let _ = writeln!(
                    text,
                    "Wrote {} {} to {}.",
                    export.files.len(),
                    if export.files.len() == 1 {
                        "file"
                    } else {
                        "files"
                    },
                    export.directory
                );
            }
            Self::FollowUp(follow_up) if follow_up.text.is_empty() => {
                text.push_str("Nobody owes anything from this meeting yet.\n");
            }
            Self::FollowUp(follow_up) => text.push_str(&follow_up.text),
            Self::Cycles(listing) if listing.calendar.is_some() => {
                text.push_str(&listing.calendar.as_deref().unwrap_or_default().replace("\r\n", "\n"));
            }
            Self::Cycles(listing) => {
                let _ = writeln!(
                    text,
                    "Cycles run {} {} from {}.",
                    listing.weeks,
                    if listing.weeks == 1 { "week" } else { "weeks" },
                    weekday_name(listing.start_weekday)
                );
                let joins = match (listing.auto_add_started, listing.auto_add_completed) {
                    (true, true) => "Started and done work with no cycle joins one by itself.",
                    (true, false) => "Started work with no cycle joins one by itself.",
                    (false, true) => "Done work with no cycle joins one by itself.",
                    (false, false) => "Work joins a cycle only when planned in one.",
                };
                let _ = writeln!(text, "{joins}");
                if let Some(capacity) = listing.capacity {
                    let _ = writeln!(text, "A cycle usually finishes {capacity} tasks.");
                }
                for cycle in &listing.cycles {
                    cycle_line(&mut text, cycle);
                }
            }
            Self::Cycle(progress) => {
                cycle_line(&mut text, &progress.cycle);
                if progress.carried_over > 0 {
                    let _ = writeln!(text, "{} carried over unfinished.", progress.carried_over);
                }
                for day in &progress.days {
                    let target = progress
                        .target
                        .iter()
                        .find(|target| target.day == day.day)
                        .map_or(0.0, |target| target.done);
                    let _ = writeln!(
                        text,
                        "{}  {} of {} done, target {target}",
                        short_day(&day.day),
                        day.done,
                        day.scope
                    );
                }
            }
            Self::Lines(answer) => {
                if answer.lines.is_empty() {
                    text.push_str("Nothing said answers that.\n");
                }
                for line in &answer.lines {
                    line_text(&mut text, line);
                }
                if !answer.taken.is_empty() {
                    text.push_str("\nIn Triage:\n");
                    for task in &answer.taken {
                        task_line(&mut text, task, &today);
                    }
                }
            }
            Self::Watches(answer) => {
                if answer.watches.is_empty() {
                    text.push_str(
                        "Nothing is watched. Watch a topic with: sona work watch WORDS\n",
                    );
                }
                for watch in &answer.watches {
                    let verb = if answer.removed {
                        "Stopped watching"
                    } else {
                        "Watching"
                    };
                    if answer.dry_run {
                        text.push_str("Dry run. ");
                    }
                    let _ = write!(text, "{verb} \u{201C}{}\u{201D}", watch.topic);
                    if let Some(person) = &watch.person {
                        let _ = write!(text, " from {person}");
                    }
                    text.push('\n');
                    if !answer.removed {
                        for line in &watch.heard {
                            text.push_str("  ");
                            line_text(&mut text, line);
                        }
                    }
                }
            }
        }
        if let Some(note) = left_out(self.conversations_available()) {
            let _ = writeln!(text, "{note}");
        }
        let end = text.trim_end().len();
        text.truncate(end);
        text
    }

    fn conversations_available(&self) -> bool {
        match self {
            Self::Page(page) => page.conversations_available,
            Self::Mine(mine) => mine.conversations_available,
            Self::Counts(counts) => counts.conversations_available,
            Self::Detail(detail) => detail.conversations_available,
            Self::Digest(answer) => answer.conversations_available,
            Self::Export(export) => export.conversations_available,
            Self::Insights(insights) => insights.conversations_available,
            Self::TriageHints(answer) => match answer.as_ref() {
                triage_hints::TriageHintsAnswer::Suggestions(list) => list.conversations_available,
                triage_hints::TriageHintsAnswer::Decision(_) => true,
            },
            Self::Change(_)
            | Self::Preview(_)
            | Self::Relation(_)
            | Self::Labels(_)
            | Self::Label(_)
            | Self::LabelEdit(_)
            | Self::Templates(_)
            | Self::Template(_)
            | Self::Reminders(_)
            | Self::Reminder(_)
            | Self::AgentLoops(_)
            | Self::AgentLoop(_)
            | Self::AgentLoopChange(_)
            | Self::AgentLoopRun(_)
            | Self::Views(_)
            | Self::View(_)
            | Self::FollowUp(_)
            | Self::Cycles(_)
            | Self::Cycle(_)
            | Self::Lines(_)
            | Self::Watches(_)
            | Self::Projects(_)
            | Self::Project(_)
            | Self::Milestones(_)
            | Self::Timeline(_)
            | Self::Milestone(_)
            | Self::People(_)
            | Self::Person(_)
            | Self::Meetings(_)
            | Self::ProjectDetail(_)
            | Self::ProjectDependencies(_)
            | Self::ProjectDependency(_)
            | Self::Initiatives(_)
            | Self::InitiativeDetail(_)
            | Self::InitiativeChange(_)
            | Self::ProjectTemplates(_)
            | Self::ProjectTemplate(_)
            | Self::ProjectNew(_)
            | Self::ProjectUpdates(_)
            | Self::ProjectUpdate(_)
            | Self::ProjectDraft(_)
            | Self::MilestoneOrder(_)
            | Self::CycleChange(_)
            | Self::MeetingDetail(_)
            | Self::Comments(_)
            | Self::Comment(_)
            | Self::Attachments(_)
            | Self::Attachment(_)
            | Self::Organize(_)
            | Self::Speaker(_)
            | Self::PersonPage(_)
            | Self::Companies(_)
            | Self::Company(_)
            | Self::FirstMet(_)
            | Self::PeopleChange(_)
            | Self::SpeakerVoices(_)
            | Self::VoiceChange(_)
            | Self::Sources(_)
            | Self::Library(_)
            | Self::Transcript(_)
            | Self::Dictations(_)
            | Self::Day(_)
            | Self::Brief(_)
            | Self::Note(_)
            | Self::Subjects(_)
            | Self::Planner(_)
            | Self::Settings(_)
            | Self::Changes(_)
            | Self::Sessions(_)
            | Self::Session(_)
            | Self::Lifecycle(_)
            | Self::Archive(_)
            | Self::Versions(_)
            | Self::Favorites(_)
            | Self::Subscriptions(_)
            | Self::Inbox(_)
            | Self::ProjectAdmin(_) => true,
        }
    }
}

fn weekday_name(weekday: u8) -> &'static str {
    match weekday {
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        6 => "Saturday",
        _ => "Sunday",
    }
}

fn cycle_line(text: &mut String, cycle: &ExternalWorkCycle) {
    let state = match cycle.state {
        WorkCycleState::Past => "ended",
        WorkCycleState::Current => "current",
        WorkCycleState::Upcoming => "next",
    };
    let _ = writeln!(
        text,
        "Cycle {:<4}{:<9}{} to {}  {} of {} done",
        cycle.number,
        state,
        short_day(&cycle.first_day),
        short_day(&cycle.last_day),
        cycle.done,
        cycle.scope
    );
}

fn task_line(text: &mut String, task: &ExternalWorkItem, today: &Today) {
    let _ = write!(
        text,
        "{:<7}{:<13}{:<8}{}",
        task.handle,
        status_text(task.status),
        task.priority.short(),
        task.title
    );
    let mut notes = Vec::new();
    match &task.owner {
        ExternalWorkOwner::Myself => notes.push("you".to_owned()),
        ExternalWorkOwner::Person { name, .. } => {
            notes.push(name.clone().unwrap_or_else(|| "someone".to_owned()));
        }
        ExternalWorkOwner::Nobody => {}
    }
    if let Some(doer) = &task.doer {
        notes.push(if task.status.is_open() {
            format!("{doer} on it")
        } else {
            format!("{doer} worked on it")
        });
    }
    if let Some(person) = &task.for_person {
        notes.push(format!("for {}", person.name));
    }
    if let Some(points) = task.estimate {
        notes.push(format!("{points} pt"));
    }
    if let Some(completed) = task.completed_at_ms {
        notes.push(format!("done {}", short_day(&day_text(Some(completed)))));
    }
    if let Some(due) = &task.due_date {
        let late = task.status.is_open() && *due < today.day;
        notes.push(format!(
            "due {}{}",
            short_day(due),
            if late { " (overdue)" } else { "" }
        ));
    }
    if let Some(follow_up) = task.follow_up_at_ms.filter(|_| task.status.is_open()) {
        notes.push(if follow_up <= today.now_ms {
            "follow up now".to_owned()
        } else {
            format!("follow up {}", short_day(&day_text(Some(follow_up))))
        });
    }
    if task.focus {
        notes.push("focus today".to_owned());
    }
    if task.at_risk && task.status.is_open() {
        notes.push("at risk".to_owned());
    }
    if let Some(cycle) = task.cycle {
        notes.push(format!("cycle {cycle}"));
    }
    if let Some(repeat) = &task.repeat {
        notes.push(if repeat.of > 0 {
            format!("{} (kept {} of {})", repeat.every, repeat.kept, repeat.of)
        } else {
            repeat.every.clone()
        });
    }
    if let Some(parent) = &task.parent {
        notes.push(format!("under {}", parent.handle));
    }
    if let Some(progress) = task.sub_tasks {
        notes.push(format!("{}/{} sub-tasks done", progress.done, progress.total));
    }
    match task.attachment_count {
        0 => {}
        1 => notes.push("1 attachment".to_owned()),
        count => notes.push(format!("{count} attachments")),
    }
    if let Some(project) = &task.project {
        notes.push(project.name.clone());
    }
    if !task.labels.is_empty() {
        let names: Vec<&str> = task
            .labels
            .iter()
            .map(|label| label.name.as_str())
            .collect();
        notes.push(format!("[{}]", names.join(", ")));
    }
    if task.blocked_by_open > 0 {
        notes.push(format!("blocked by {}", task.blocked_by_open));
    }
    if let Some(until) = task.snoozed_until_ms.filter(|until| *until > today.now_ms) {
        notes.push(format!("snoozed until {}", day_text(Some(until))));
    }
    if task.archived_at_ms.is_some() {
        notes.push("archived".to_owned());
    }
    if task.status == WorkStatus::Triage {
        if let Some(meeting) = task
            .source
            .as_ref()
            .and_then(|source| source.meeting_title.as_deref())
        {
            notes.push(format!("from {meeting}"));
        }
    }
    if !notes.is_empty() {
        text.push_str("  · ");
        text.push_str(&notes.join(" · "));
    }
    text.push('\n');
}

fn short_day(iso: &str) -> String {
    NaiveDate::parse_from_str(iso, "%Y-%m-%d").map_or_else(
        |_| iso.to_owned(),
        |day| day.format("%a %-d %b").to_string(),
    )
}

fn line_text(text: &mut String, line: &ExternalWorkLine) {
    let _ = write!(text, "{:>2}. ", line.number);
    if let Some(said_by) = &line.said_by {
        let _ = write!(text, "{said_by}: ");
    }
    let _ = write!(text, "\u{201C}{}\u{201D}  · {}", line.quote, line.from);
    if let Some(day) = &line.said_on {
        let _ = write!(text, ", {}", short_day(day));
    }
    let _ = writeln!(text, "  {}", line.link);
}

fn counts_line(text: &mut String, counts: &ExternalWorkCounts) {
    let _ = writeln!(
        text,
        "Triage {} · yours open {} · overdue {} · due today {} · waiting on {} ({} to follow up)",
        counts.triage,
        counts.mine_open,
        counts.mine_overdue,
        counts.mine_due_today,
        counts.waiting_on,
        counts.waiting_on_stale
    );
}

fn section(text: &mut String, heading: &str, tasks: &[ExternalWorkItem], today: &Today) {
    let _ = writeln!(text, "\n{heading}");
    if tasks.is_empty() {
        text.push_str("  nothing\n");
    }
    for task in tasks {
        text.push_str("  ");
        task_line(text, task, today);
    }
}

fn detail_text(text: &mut String, detail: &ExternalWorkDetail, today: &Today) {
    let task = &detail.task;
    task_line(text, task, today);
    if let Some(details) = &task.details {
        for line in details.lines() {
            let _ = writeln!(text, "  {line}");
        }
    }
    if let Some(every) = &detail.recurrence {
        let _ = writeln!(text, "\nRepeats {every}");
    }
    if let Some(source) = &task.source {
        let said = said_by(source).unwrap_or_else(|| "a meeting".to_owned());
        match &source.quote {
            Some(quote) => {
                let _ = writeln!(text, "\nFrom {said}: \"{quote}\"");
            }
            None => {
                let _ = writeln!(text, "\nFrom {said}");
            }
        }
        let _ = writeln!(text, "  {}", source.link);
    }
    if !detail.relations.is_empty() {
        text.push('\n');
        for link in &detail.relations {
            let kind = match link.kind {
                ExternalLinkKind::Blocks => "Blocks",
                ExternalLinkKind::BlockedBy => "Blocked by",
                ExternalLinkKind::Related => "Related to",
            };
            let _ = writeln!(
                text,
                "{kind} {} {} ({})",
                link.task.handle,
                link.task.title,
                status_text(link.task.status)
            );
        }
    }
    if !detail.evidence.is_empty() {
        text.push_str("\nEvidence\n");
        for evidence in &detail.evidence {
            let title = evidence.title.as_deref();
            let named = match (evidence.source_kind, evidence.caption.as_deref()) {
                (planning::EvidenceSourceKind::Dictation, Some(quote)) => title.map_or_else(
                    || format!("\"{quote}\""),
                    |title| format!("\"{quote}\" in {title}"),
                ),
                (planning::EvidenceSourceKind::Link, Some(caption)) => caption.to_owned(),
                _ => title.unwrap_or("unavailable source").to_owned(),
            };
            match evidence.link.as_deref().filter(|link| *link != named) {
                Some(link) => {
                    let _ = writeln!(text, "  {named}  {link}");
                }
                None => {
                    let _ = writeln!(text, "  {named}");
                }
            }
        }
    }
    if !detail.suggestions.is_empty() {
        text.push_str("\nSuggested\n");
        for suggestion in &detail.suggestions {
            let field = triage_hints::field_text(suggestion.field);
            let _ = writeln!(
                text,
                "  {field}: {} ({})",
                suggestion.value, suggestion.reason
            );
        }
    }
    if !detail.proposals.is_empty() {
        text.push_str("\nWaiting for the user to accept in Sona\n");
        for proposal in &detail.proposals {
            proposal_line(text, proposal);
        }
    }
    if let Some(parent) = &detail.parent {
        let _ = writeln!(
            text,
            "\nSub-task of {} {} ({})",
            parent.handle,
            parent.title,
            status_text(parent.status)
        );
    }
    if !detail.sub_tasks.is_empty() {
        text.push_str("\nSub-tasks\n");
        for sub_task in &detail.sub_tasks {
            text.push_str("  ");
            task_line(text, sub_task, today);
        }
    }
    if !detail.comments.is_empty() {
        text.push_str("\nComments\n");
        threads::threads_text(text, &detail.comments);
    }
    if !detail.attachments.is_empty() {
        text.push_str("\nAttachments\n");
        attachments::attachments_text(text, &detail.attachments);
    }
    if let Some(reminder) = &detail.reminder {
        let _ = writeln!(
            text,
            "\nReminder {}{}",
            reminder.remind_at,
            if reminder.notified_at_ms.is_some() {
                " (shown)"
            } else {
                ""
            }
        );
    }
    if !detail.sessions.is_empty() {
        text.push_str("\nAgent sessions\n");
        for session in &detail.sessions {
            text.push_str("  ");
            sessions::session_line(text, session);
        }
    }
    if !detail.activity.is_empty() {
        text.push_str("\nHistory\n");
        for entry in &detail.activity {
            let named = entry.agent.as_deref().unwrap_or("agent");
            let actor = match (entry.actor, entry.via) {
                (WorkActor::Me, _) => "you".to_owned(),
                (WorkActor::Sona, _) => "Sona".to_owned(),
                (WorkActor::Agent, Some(via)) => format!("{named} ({})", via.text()),
                (WorkActor::Agent, None) => named.to_owned(),
            };
            let kind = serde_json::to_value(entry.kind)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            let change = match (entry.kind, entry.from.as_deref(), entry.to.as_deref()) {
                (WorkActivityKind::Created, ..)
                    if entry.meeting_id.is_some() || entry.meeting_title.is_some() =>
                {
                    entry.meeting_title.as_deref().map_or_else(
                        || "heard in a meeting".to_owned(),
                        |title| format!("heard in “{title}”"),
                    )
                }
                (WorkActivityKind::Archived, _, Some(work::bin::BIN)) => "deleted".to_owned(),
                (WorkActivityKind::Restored, Some(work::bin::BIN), _) => {
                    "restored from Recently deleted".to_owned()
                }
                (_, Some(from), Some(to)) => format!("{kind}: {from} → {to}"),
                (_, None, Some(to)) => format!("{kind}: {to}"),
                (_, Some(from), None) => format!("{kind}: removed {from}"),
                (_, None, None) => kind,
            };
            let _ = writeln!(text, "  {}  {actor}  {change}", local_time(entry.at_ms));
            if let Some(reason) = &entry.reason {
                let _ = writeln!(text, "      reason: {reason}");
            }
        }
    }
    let _ = writeln!(
        text,
        "\nid {} · revision {}\n{}",
        task.id, task.revision, task.link
    );
}

fn change_text(text: &mut String, change: &ExternalWorkChange, today: &Today) {
    if change.tasks.is_empty()
        && change.skipped.is_empty()
        && change.proposed.is_empty()
        && change.evidence_url.is_none()
    {
        text.push_str("Nothing to change.\n");
    }
    if !change.tasks.is_empty() {
        if change.replayed {
            text.push_str(
                "Already created by this same request earlier today; pass a new --operation-id to create another:\n",
            );
        } else {
            let _ = writeln!(text, "{}:", change.action.heading());
        }
        for task in &change.tasks {
            task_line(text, task, today);
        }
    }
    if let Some(link) = &change.evidence_url {
        let _ = writeln!(text, "Kept as evidence: {link}");
    }
    if !change.proposed.is_empty() {
        text.push_str("Waiting for the user to accept in Sona:\n");
        for proposal in &change.proposed {
            proposal_line(text, proposal);
        }
    }
    for skip in &change.skipped {
        let _ = writeln!(text, "Left {} alone: {}.", skip.handle, skip.reason);
    }
}

/// `S-12 priority: No priority → Urgent (Codex)`, a long value cut to its
/// first line.
fn proposal_line(text: &mut String, proposal: &ExternalWorkProposal) {
    let first_line = |value: &str| {
        let mut lines = value.lines();
        let first = lines.next().unwrap_or_default();
        if lines.next().is_some() {
            format!("{first}…")
        } else {
            first.to_owned()
        }
    };
    let _ = writeln!(
        text,
        "  {} {}: {} → {} ({})",
        proposal.handle,
        proposal.field.as_str(),
        first_line(&proposal.from),
        first_line(&proposal.to),
        proposal.agent
    );
}

fn preview_text(text: &mut String, preview: &ExternalWorkPreview) {
    for change in &preview.changes {
        if let Some(reason) = &change.skipped {
            let _ = writeln!(text, "Would leave {} alone: {reason}.", change.handle);
            continue;
        }
        let _ = writeln!(text, "{} {}", change.handle, change.title);
        if change.fields.is_empty() {
            text.push_str("  nothing would change\n");
        }
        for field in &change.fields {
            if field.from.is_empty() {
                let _ = writeln!(text, "  {}: {}", field.field, field.to);
            } else {
                let _ = writeln!(text, "  {}: {} → {}", field.field, field.from, field.to);
            }
        }
        if let Some(effect) = change.commitment {
            let effect = match effect {
                CommitmentEffect::Closes => "closes the meeting commitment it came from",
                CommitmentEffect::SetsAside => "sets aside the meeting commitment it came from",
                CommitmentEffect::Reopens => "reopens the meeting commitment it came from",
            };
            let _ = writeln!(text, "  {effect}");
        }
    }
    text.push_str("Dry run: nothing was written.");
    if let Some(operation_id) = &preview.operation_id {
        let _ = write!(
            text,
            " The same command without --dry-run writes under operation id {operation_id}."
        );
    }
    text.push('\n');
}

fn filters_text(filters: &ExternalWorkFilters) -> String {
    let mut parts = Vec::new();
    if let Some(statuses) = &filters.statuses {
        let names: Vec<&str> = statuses.iter().map(|status| status_text(*status)).collect();
        parts.push(names.join(", "));
    }
    match filters.direction {
        Some(WorkDirection::Mine) => parts.push("yours".to_owned()),
        Some(WorkDirection::WaitingOn) => parts.push("waiting on others".to_owned()),
        Some(WorkDirection::Unattributed) => parts.push("unowned".to_owned()),
        None => {}
    }
    for (name, values) in [
        ("owners", &filters.owner_person_ids),
        ("projects", &filters.project_ids),
        ("labels", &filters.label_ids),
    ] {
        if let Some(values) = values {
            parts.push(format!("{name} {}", values.len()));
        }
    }
    if let Some(priorities) = &filters.priorities {
        let names: Vec<&str> = priorities.iter().map(|priority| priority.text()).collect();
        parts.push(names.join(", "));
    }
    if let Some(before) = filters.due_before_ms {
        parts.push(format!("due before {}", day_text(Some(before))));
    }
    if let Some(due) = &filters.due {
        parts.push(format!("due {}", due.join(", ")));
    }
    if let Some(search) = &filters.search {
        parts.push(format!("matching {search:?}"));
    }
    if filters.include_archived {
        parts.push("with archived".to_owned());
    }
    for condition in &filters.conditions {
        parts.push(format!(
            "{} {} {}",
            condition.field.word(),
            condition.op.word(),
            condition.values.join(", ")
        ));
    }
    if parts.is_empty() {
        "everything".to_owned()
    } else {
        parts.join(" · ")
    }
}

mod attachments;
mod catalog;
mod preferences;
mod lifecycle;
mod notes;
mod organize;
mod people;
mod roadmap;
mod save;
pub(crate) use save::MeetingFence;
mod speakers;
mod voices;
mod sources;
mod library;
mod transcript;
mod dictations;
mod sessions;
mod routines;
mod agent_loops;
mod threads;
mod planner;
mod triage_hints;
mod subjects;
mod dependencies;
mod initiatives;
mod digest;
mod project_templates;
mod timeline;
mod insights;
mod words;

#[cfg(test)]
mod tests;
