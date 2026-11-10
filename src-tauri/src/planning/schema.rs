pub const MIGRATION: &str = "CREATE TABLE planning_projects (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL CHECK(length(trim(title)) > 0),
    description TEXT,
    status TEXT NOT NULL CHECK(status IN ('active', 'paused', 'completed')),
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE TABLE planning_milestones (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES planning_projects(id),
    title TEXT NOT NULL CHECK(length(trim(title)) > 0),
    due_at_ms INTEGER,
    status TEXT NOT NULL CHECK(status IN ('not_started', 'in_progress', 'done', 'canceled')),
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(id, project_id)
);
CREATE INDEX planning_milestones_project ON planning_milestones(project_id, due_at_ms, id);
CREATE TABLE planning_tasks (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES planning_projects(id),
    milestone_id TEXT,
    title TEXT NOT NULL CHECK(length(trim(title)) > 0),
    details TEXT,
    due_at_ms INTEGER,
    owner_person_id TEXT,
    owner_is_self INTEGER NOT NULL CHECK(owner_is_self IN (0, 1)),
    status TEXT NOT NULL CHECK(status IN ('not_started', 'in_progress', 'done', 'canceled')),
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK(owner_is_self = 0 OR owner_person_id IS NULL),
    FOREIGN KEY(milestone_id, project_id) REFERENCES planning_milestones(id, project_id)
);
CREATE INDEX planning_tasks_project ON planning_tasks(project_id, due_at_ms, id);
CREATE INDEX planning_tasks_owner ON planning_tasks(owner_person_id, status);
CREATE INDEX planning_tasks_due ON planning_tasks(due_at_ms, status);
CREATE INDEX planning_milestones_due ON planning_milestones(due_at_ms, status);
CREATE TABLE planning_evidence_links (
    id TEXT PRIMARY KEY NOT NULL,
    target_kind TEXT NOT NULL CHECK(target_kind IN ('project', 'milestone', 'task')),
    target_id TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('meeting', 'note', 'loop')),
    source_id TEXT NOT NULL,
    at_ms INTEGER CHECK(at_ms IS NULL OR at_ms >= 0),
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX planning_evidence_identity ON planning_evidence_links(
    target_kind, target_id, source_kind, source_id, coalesce(at_ms, -1)
);
CREATE INDEX planning_evidence_target ON planning_evidence_links(target_kind, target_id);
CREATE TABLE planning_operations (
    operation_id TEXT PRIMARY KEY NOT NULL,
    command TEXT NOT NULL,
    request_json TEXT NOT NULL,
    response_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE TRIGGER planning_project_delete AFTER DELETE ON planning_projects BEGIN
    DELETE FROM planning_tasks WHERE project_id = old.id;
    DELETE FROM planning_milestones WHERE project_id = old.id;
    DELETE FROM planning_evidence_links WHERE target_kind = 'project' AND target_id = old.id;
END;
CREATE TRIGGER planning_milestone_delete AFTER DELETE ON planning_milestones BEGIN
    UPDATE planning_tasks SET milestone_id = NULL, revision = revision + 1
        WHERE milestone_id = old.id;
    DELETE FROM planning_evidence_links WHERE target_kind = 'milestone' AND target_id = old.id;
END;
CREATE TRIGGER planning_task_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_evidence_links WHERE target_kind = 'task' AND target_id = old.id;
END;
CREATE TABLE planning_suggestions (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('possible_link', 'possible_duplicate', 'possible_conflict', 'possible_change')),
    target_kind TEXT NOT NULL CHECK(target_kind IN ('project', 'milestone', 'task')),
    target_id TEXT NOT NULL,
    target_revision INTEGER NOT NULL CHECK(target_revision > 0),
    source_kind TEXT NOT NULL CHECK(source_kind IN ('meeting', 'note', 'loop')),
    source_id TEXT NOT NULL,
    source_hash TEXT NOT NULL,
    passage_id TEXT NOT NULL,
    passage_hash TEXT NOT NULL,
    at_ms INTEGER CHECK(at_ms IS NULL OR at_ms >= 0),
    state TEXT NOT NULL CHECK(state IN ('pending', 'accepted', 'declined')),
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK(source_kind != 'note' OR at_ms IS NULL),
    CHECK(kind != 'possible_duplicate' OR target_kind = 'task'),
    CHECK(kind != 'possible_change' OR (target_kind = 'task' AND source_kind = 'loop'))
);
CREATE UNIQUE INDEX planning_suggestion_identity ON planning_suggestions(
    target_kind, target_id, target_revision, source_kind, source_id, source_hash, passage_id
);
CREATE INDEX planning_suggestions_pending ON planning_suggestions(state, created_at_ms, id);";

pub const QUIET_REVIEW_MIGRATION: &str = "CREATE TABLE planning_quiet_review (
    project_id TEXT PRIMARY KEY NOT NULL REFERENCES planning_projects(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    selected_sources_json TEXT NOT NULL,
    grant_revision INTEGER NOT NULL CHECK(grant_revision > 0),
    last_input_json TEXT,
    last_input_hash TEXT,
    last_grant_revision INTEGER,
    last_completed_at_ms INTEGER,
    last_outcome TEXT CHECK(last_outcome IN ('suggestions_ready', 'nothing_new')),
    CHECK((last_input_hash IS NULL AND last_completed_at_ms IS NULL AND last_outcome IS NULL)
       OR (last_input_hash IS NOT NULL AND last_completed_at_ms IS NOT NULL AND last_outcome IS NOT NULL))
);";

pub const MEETING_NOTE_LINK_MIGRATION: &str = "ALTER TABLE planning_evidence_links
    ADD COLUMN meeting_note_id TEXT CHECK(meeting_note_id IS NULL OR (source_kind = 'meeting' AND at_ms IS NULL));
DROP INDEX planning_evidence_identity;
CREATE UNIQUE INDEX planning_evidence_identity ON planning_evidence_links(
    target_kind, target_id, source_kind, source_id, coalesce(at_ms, -1), coalesce(meeting_note_id, '')
);";

pub const DISMISSED_LOOP_MIGRATION: &str = "CREATE TABLE planning_dismissed_loops (
    loop_id TEXT PRIMARY KEY NOT NULL,
    dismissed_at_ms INTEGER NOT NULL
);
CREATE INDEX planning_evidence_source ON planning_evidence_links(source_kind, source_id);";

/// Planner tasks become Sona's one tracked item. SQLite cannot drop the
/// NOT NULL on `project_id` in place, so the table is rebuilt with every id
/// kept. A task that cites a meeting commitment takes the earliest such loop
/// as its source; when several tasks cite one loop, the one still live, then
/// the oldest, keeps it, because a loop has one task.
pub const WORK_MIGRATION: &str = "DROP TRIGGER planning_project_delete;
DROP TRIGGER planning_milestone_delete;
CREATE TABLE planning_tasks_next (
    id TEXT PRIMARY KEY NOT NULL,
    number INTEGER NOT NULL UNIQUE CHECK(number > 0),
    project_id TEXT REFERENCES planning_projects(id),
    milestone_id TEXT,
    title TEXT NOT NULL CHECK(length(trim(title)) > 0),
    details TEXT,
    due_at_ms INTEGER,
    owner_person_id TEXT,
    owner_is_self INTEGER NOT NULL CHECK(owner_is_self IN (0, 1)),
    status TEXT NOT NULL CHECK(status IN ('triage', 'backlog', 'not_started', 'in_progress', 'done', 'canceled', 'duplicate')),
    priority INTEGER NOT NULL CHECK(priority BETWEEN 0 AND 4),
    source_loop_id TEXT,
    duplicate_of TEXT,
    snoozed_until_ms INTEGER,
    archived_at_ms INTEGER,
    status_changed_at_ms INTEGER NOT NULL,
    sort_order REAL NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK(owner_is_self = 0 OR owner_person_id IS NULL),
    CHECK(milestone_id IS NULL OR project_id IS NOT NULL),
    CHECK((status = 'duplicate') = (duplicate_of IS NOT NULL)),
    CHECK(duplicate_of IS NULL OR duplicate_of != id),
    FOREIGN KEY(milestone_id, project_id) REFERENCES planning_milestones(id, project_id)
);
INSERT INTO planning_tasks_next (
    id, number, project_id, milestone_id, title, details, due_at_ms, owner_person_id,
    owner_is_self, status, priority, status_changed_at_ms, sort_order, revision,
    created_at_ms, updated_at_ms
)
SELECT id, row_number() OVER (ORDER BY created_at_ms, id), project_id, milestone_id, title,
    details, due_at_ms, owner_person_id, owner_is_self, status, 0, updated_at_ms,
    created_at_ms, revision, created_at_ms, updated_at_ms
FROM planning_tasks;
UPDATE planning_tasks_next SET source_loop_id = (
    SELECT l.source_id FROM planning_evidence_links l
    WHERE l.target_kind = 'task' AND l.target_id = planning_tasks_next.id AND l.source_kind = 'loop'
    ORDER BY l.created_at_ms, l.id LIMIT 1
);
UPDATE planning_tasks_next SET source_loop_id = NULL
WHERE source_loop_id IS NOT NULL AND id != (
    SELECT t.id FROM planning_tasks_next t WHERE t.source_loop_id = planning_tasks_next.source_loop_id
    ORDER BY t.status = 'canceled', t.created_at_ms, t.id LIMIT 1
);
DROP TABLE planning_tasks;
ALTER TABLE planning_tasks_next RENAME TO planning_tasks;
CREATE INDEX planning_tasks_project ON planning_tasks(project_id, due_at_ms, id);
CREATE INDEX planning_tasks_owner ON planning_tasks(owner_person_id, status);
CREATE INDEX planning_tasks_due ON planning_tasks(due_at_ms, status);
CREATE UNIQUE INDEX planning_tasks_source_loop ON planning_tasks(source_loop_id);
CREATE INDEX planning_tasks_duplicate ON planning_tasks(duplicate_of);
CREATE TABLE planning_work_counter (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    next_number INTEGER NOT NULL CHECK(next_number > 0)
);
INSERT INTO planning_work_counter (id, next_number)
    SELECT 1, coalesce(max(number), 0) + 1 FROM planning_tasks;
CREATE TRIGGER planning_project_delete AFTER DELETE ON planning_projects BEGIN
    DELETE FROM planning_tasks WHERE project_id = old.id;
    DELETE FROM planning_milestones WHERE project_id = old.id;
    DELETE FROM planning_evidence_links WHERE target_kind = 'project' AND target_id = old.id;
END;
CREATE TRIGGER planning_milestone_delete AFTER DELETE ON planning_milestones BEGIN
    UPDATE planning_tasks SET milestone_id = NULL, revision = revision + 1
        WHERE milestone_id = old.id;
    DELETE FROM planning_evidence_links WHERE target_kind = 'milestone' AND target_id = old.id;
END;
CREATE TRIGGER planning_task_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_evidence_links WHERE target_kind = 'task' AND target_id = old.id;
END;
CREATE TABLE planning_labels (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    hue REAL NOT NULL CHECK(hue >= 0 AND hue <= 360),
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX planning_labels_name ON planning_labels(lower(name));
CREATE TABLE planning_task_labels (
    task_id TEXT NOT NULL,
    label_id TEXT NOT NULL,
    PRIMARY KEY(task_id, label_id)
);
CREATE INDEX planning_task_labels_label ON planning_task_labels(label_id);
CREATE TABLE planning_task_relations (
    id TEXT PRIMARY KEY NOT NULL,
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('blocks', 'related')),
    created_at_ms INTEGER NOT NULL,
    CHECK(from_id != to_id),
    UNIQUE(from_id, to_id, kind)
);
CREATE INDEX planning_task_relations_to ON planning_task_relations(to_id);
CREATE TABLE planning_task_activity (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    actor TEXT NOT NULL CHECK(actor IN ('me', 'sona', 'agent')),
    kind TEXT NOT NULL CHECK(kind IN ('created', 'status', 'priority', 'owner', 'due', 'project',
        'title', 'details', 'labels', 'archived', 'restored', 'duplicate', 'accepted', 'declined',
        'snoozed', 'relation')),
    from_value TEXT,
    to_value TEXT
);
CREATE INDEX planning_task_activity_task ON planning_task_activity(task_id, at_ms, id);
CREATE TABLE planning_views (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    query_json TEXT NOT NULL,
    display_json TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);";

/// Deleting a project keeps its tasks: they leave the project and its
/// milestones in one statement, since a task with a milestone must have a
/// project.
pub const PROJECT_DELETE_KEEPS_TASKS_MIGRATION: &str = "DROP TRIGGER planning_project_delete;
CREATE TRIGGER planning_project_delete AFTER DELETE ON planning_projects BEGIN
    UPDATE planning_tasks SET project_id = NULL, milestone_id = NULL, revision = revision + 1
        WHERE project_id = old.id;
    DELETE FROM planning_milestones WHERE project_id = old.id;
    DELETE FROM planning_evidence_links WHERE target_kind = 'project' AND target_id = old.id;
END;";

/// The days a person planned, and "kept" among what an item's history can
/// say. SQLite cannot widen a CHECK in place, so the history table is
/// rebuilt with every entry and its rowid kept, since entries written in
/// the same millisecond read back in rowid order.
pub const DAY_PLAN_MIGRATION: &str = "CREATE TABLE planning_days (
    day TEXT PRIMARY KEY NOT NULL CHECK(length(day) = 10),
    focus_json TEXT NOT NULL,
    planned_at_ms INTEGER NOT NULL
);
CREATE TABLE planning_task_activity_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    actor TEXT NOT NULL CHECK(actor IN ('me', 'sona', 'agent')),
    kind TEXT NOT NULL CHECK(kind IN ('created', 'status', 'priority', 'owner', 'due', 'project',
        'title', 'details', 'labels', 'archived', 'restored', 'duplicate', 'accepted', 'declined',
        'snoozed', 'relation', 'kept')),
    from_value TEXT,
    to_value TEXT
);
INSERT INTO planning_task_activity_next (rowid, id, task_id, at_ms, actor, kind, from_value, to_value)
    SELECT rowid, id, task_id, at_ms, actor, kind, from_value, to_value FROM planning_task_activity;
DROP TABLE planning_task_activity;
ALTER TABLE planning_task_activity_next RENAME TO planning_task_activity;
CREATE INDEX planning_task_activity_task ON planning_task_activity(task_id, at_ms, id);";

/// What follow-through remembers: each reminder already shown, when each
/// waiting-on item was last nudged, and the points saved to raise with a
/// person.
pub const FOLLOW_MIGRATION: &str = "CREATE TABLE planning_reminders (
    key TEXT PRIMARY KEY NOT NULL,
    sent_at_ms INTEGER NOT NULL
);
CREATE TABLE planning_nudges (
    task_id TEXT PRIMARY KEY NOT NULL,
    nudged_at_ms INTEGER NOT NULL
);
CREATE TABLE planning_talking_points (
    id TEXT PRIMARY KEY NOT NULL,
    person_id TEXT NOT NULL,
    text TEXT NOT NULL CHECK(length(trim(text)) > 0),
    created_at_ms INTEGER NOT NULL,
    done_at_ms INTEGER
);
CREATE INDEX planning_talking_points_person ON planning_talking_points(person_id, created_at_ms);";
/// Who works a task besides its owner, the changes agents proposed for a
/// task, and which agent made a history entry its actor calls `agent`.
pub const AGENTS_MIGRATION: &str = "CREATE TABLE planning_task_doers (
    task_id TEXT PRIMARY KEY NOT NULL,
    agent TEXT NOT NULL CHECK(length(trim(agent)) > 0),
    since_ms INTEGER NOT NULL
);
CREATE TABLE planning_task_proposals (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    agent TEXT NOT NULL CHECK(length(trim(agent)) > 0),
    field TEXT NOT NULL CHECK(field IN ('title', 'details', 'status', 'priority', 'owner', 'due',
        'project', 'milestone', 'labels', 'snoozed')),
    from_value TEXT,
    patch_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('open', 'accepted', 'rejected')),
    created_at_ms INTEGER NOT NULL,
    decided_at_ms INTEGER,
    CHECK((state = 'open') = (decided_at_ms IS NULL))
);
CREATE INDEX planning_task_proposals_task ON planning_task_proposals(task_id, state, created_at_ms);
CREATE TABLE planning_activity_agents (
    activity_id TEXT PRIMARY KEY NOT NULL,
    agent TEXT NOT NULL CHECK(length(trim(agent)) > 0)
);
CREATE TRIGGER planning_task_agents_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_task_doers WHERE task_id = old.id;
    DELETE FROM planning_task_proposals WHERE task_id = old.id;
END;";

/// Evidence learns two sources the tracker captures itself: a stretch of a
/// dictation, kept as its history row and the milliseconds it covers, and a
/// link. Widening a CHECK means rebuilding the table, and SQLite refuses the
/// rebuild while triggers name it, so the three delete triggers are dropped
/// and recreated. Deleting a project keeps its tasks.
pub const CAPTURE_MIGRATION: &str = "DROP TRIGGER planning_project_delete;
DROP TRIGGER planning_milestone_delete;
DROP TRIGGER planning_task_delete;
CREATE TABLE planning_evidence_links_next (
    id TEXT PRIMARY KEY NOT NULL,
    target_kind TEXT NOT NULL CHECK(target_kind IN ('project', 'milestone', 'task')),
    target_id TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('meeting', 'note', 'loop', 'dictation', 'link')),
    source_id TEXT NOT NULL,
    at_ms INTEGER CHECK(at_ms IS NULL OR at_ms >= 0),
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at_ms INTEGER NOT NULL,
    meeting_note_id TEXT CHECK(meeting_note_id IS NULL OR (source_kind = 'meeting' AND at_ms IS NULL)),
    end_ms INTEGER CHECK(end_ms IS NULL OR (source_kind = 'dictation' AND at_ms IS NOT NULL AND end_ms >= at_ms)),
    caption TEXT CHECK(caption IS NULL OR length(trim(caption)) > 0)
);
INSERT INTO planning_evidence_links_next
    (id, target_kind, target_id, source_kind, source_id, at_ms, revision, created_at_ms, meeting_note_id)
    SELECT id, target_kind, target_id, source_kind, source_id, at_ms, revision, created_at_ms, meeting_note_id
    FROM planning_evidence_links;
DROP TABLE planning_evidence_links;
ALTER TABLE planning_evidence_links_next RENAME TO planning_evidence_links;
CREATE UNIQUE INDEX planning_evidence_identity ON planning_evidence_links(
    target_kind, target_id, source_kind, source_id, coalesce(at_ms, -1), coalesce(meeting_note_id, '')
);
CREATE INDEX planning_evidence_target ON planning_evidence_links(target_kind, target_id);
CREATE INDEX planning_evidence_source ON planning_evidence_links(source_kind, source_id);
CREATE TRIGGER planning_project_delete AFTER DELETE ON planning_projects BEGIN
    UPDATE planning_tasks SET project_id = NULL, milestone_id = NULL, revision = revision + 1
        WHERE project_id = old.id;
    DELETE FROM planning_milestones WHERE project_id = old.id;
    DELETE FROM planning_evidence_links WHERE target_kind = 'project' AND target_id = old.id;
END;
CREATE TRIGGER planning_milestone_delete AFTER DELETE ON planning_milestones BEGIN
    UPDATE planning_tasks SET milestone_id = NULL, revision = revision + 1
        WHERE milestone_id = old.id;
    DELETE FROM planning_evidence_links WHERE target_kind = 'milestone' AND target_id = old.id;
END;
CREATE TRIGGER planning_task_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_evidence_links WHERE target_kind = 'task' AND target_id = old.id;
END;
ALTER TABLE planning_tasks ADD COLUMN recurrence TEXT CHECK(recurrence IS NULL OR length(trim(recurrence)) > 0);";

/// "recurrence" among what an item's history can say and what an agent may
/// propose. Both tables are rebuilt with every row and its rowid kept, and
/// the trigger that names the proposals is dropped and recreated around
/// the rebuild.
pub const REPEAT_MIGRATION: &str = "DROP TRIGGER planning_task_agents_delete;
CREATE TABLE planning_task_activity_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    actor TEXT NOT NULL CHECK(actor IN ('me', 'sona', 'agent')),
    kind TEXT NOT NULL CHECK(kind IN ('created', 'status', 'priority', 'owner', 'due', 'project',
        'title', 'details', 'labels', 'archived', 'restored', 'duplicate', 'accepted', 'declined',
        'snoozed', 'relation', 'kept', 'recurrence')),
    from_value TEXT,
    to_value TEXT
);
INSERT INTO planning_task_activity_next (rowid, id, task_id, at_ms, actor, kind, from_value, to_value)
    SELECT rowid, id, task_id, at_ms, actor, kind, from_value, to_value FROM planning_task_activity;
DROP TABLE planning_task_activity;
ALTER TABLE planning_task_activity_next RENAME TO planning_task_activity;
CREATE INDEX planning_task_activity_task ON planning_task_activity(task_id, at_ms, id);
CREATE TABLE planning_task_proposals_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    agent TEXT NOT NULL CHECK(length(trim(agent)) > 0),
    field TEXT NOT NULL CHECK(field IN ('title', 'details', 'status', 'priority', 'owner', 'due',
        'project', 'milestone', 'labels', 'snoozed', 'recurrence')),
    from_value TEXT,
    patch_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('open', 'accepted', 'rejected')),
    created_at_ms INTEGER NOT NULL,
    decided_at_ms INTEGER,
    CHECK((state = 'open') = (decided_at_ms IS NULL))
);
INSERT INTO planning_task_proposals_next (rowid, id, task_id, agent, field, from_value, patch_json,
        state, created_at_ms, decided_at_ms)
    SELECT rowid, id, task_id, agent, field, from_value, patch_json, state, created_at_ms,
        decided_at_ms FROM planning_task_proposals;
DROP TABLE planning_task_proposals;
ALTER TABLE planning_task_proposals_next RENAME TO planning_task_proposals;
CREATE INDEX planning_task_proposals_task ON planning_task_proposals(task_id, state, created_at_ms);
CREATE TRIGGER planning_task_agents_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_task_doers WHERE task_id = old.id;
    DELETE FROM planning_task_proposals WHERE task_id = old.id;
END;";

/// Cycles, the series a repeating task's instances belong to, and the time
/// booked for a task. "cycle" and "block" join what an item's history can
/// say and "cycle" what an agent may propose, so both tables are rebuilt as
/// `REPEAT_MIGRATION` did, every row and its rowid kept.
pub const CYCLES_MIGRATION: &str = "CREATE TABLE planning_cycle_settings (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    weeks INTEGER NOT NULL CHECK(weeks IN (1, 2)),
    start_weekday INTEGER NOT NULL CHECK(start_weekday BETWEEN 1 AND 7)
);
INSERT INTO planning_cycle_settings (id, weeks, start_weekday) VALUES (1, 1, 1);
CREATE TABLE planning_cycles (
    number INTEGER PRIMARY KEY NOT NULL CHECK(number > 0),
    start_day TEXT NOT NULL CHECK(length(start_day) = 10),
    end_day TEXT NOT NULL CHECK(length(end_day) = 10 AND end_day > start_day),
    created_at_ms INTEGER NOT NULL
);
ALTER TABLE planning_tasks ADD COLUMN cycle INTEGER CHECK(cycle IS NULL OR cycle > 0);
ALTER TABLE planning_tasks ADD COLUMN series_id TEXT;
ALTER TABLE planning_tasks ADD COLUMN next_id TEXT;
CREATE INDEX planning_tasks_cycle ON planning_tasks(cycle);
CREATE INDEX planning_tasks_series ON planning_tasks(series_id);
CREATE TABLE planning_time_blocks (
    task_id TEXT PRIMARY KEY NOT NULL,
    event_id TEXT NOT NULL,
    starts_at_ms INTEGER NOT NULL,
    ends_at_ms INTEGER NOT NULL CHECK(ends_at_ms > starts_at_ms),
    booked_at_ms INTEGER NOT NULL
);
CREATE TABLE planning_task_risks (
    task_id TEXT PRIMARY KEY NOT NULL,
    checked_at_ms INTEGER NOT NULL
);
CREATE TRIGGER planning_task_time_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_time_blocks WHERE task_id = old.id;
    DELETE FROM planning_task_risks WHERE task_id = old.id;
END;
CREATE TABLE planning_task_activity_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    actor TEXT NOT NULL CHECK(actor IN ('me', 'sona', 'agent')),
    kind TEXT NOT NULL CHECK(kind IN ('created', 'status', 'priority', 'owner', 'due', 'project',
        'title', 'details', 'labels', 'archived', 'restored', 'duplicate', 'accepted', 'declined',
        'snoozed', 'relation', 'kept', 'recurrence', 'cycle', 'block')),
    from_value TEXT,
    to_value TEXT
);
INSERT INTO planning_task_activity_next (rowid, id, task_id, at_ms, actor, kind, from_value, to_value)
    SELECT rowid, id, task_id, at_ms, actor, kind, from_value, to_value FROM planning_task_activity;
DROP TABLE planning_task_activity;
ALTER TABLE planning_task_activity_next RENAME TO planning_task_activity;
CREATE INDEX planning_task_activity_task ON planning_task_activity(task_id, at_ms, id);
DROP TRIGGER planning_task_agents_delete;
CREATE TABLE planning_task_proposals_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    agent TEXT NOT NULL CHECK(length(trim(agent)) > 0),
    field TEXT NOT NULL CHECK(field IN ('title', 'details', 'status', 'priority', 'owner', 'due',
        'project', 'milestone', 'labels', 'snoozed', 'recurrence', 'cycle')),
    from_value TEXT,
    patch_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('open', 'accepted', 'rejected')),
    created_at_ms INTEGER NOT NULL,
    decided_at_ms INTEGER,
    CHECK((state = 'open') = (decided_at_ms IS NULL))
);
INSERT INTO planning_task_proposals_next (rowid, id, task_id, agent, field, from_value, patch_json,
        state, created_at_ms, decided_at_ms)
    SELECT rowid, id, task_id, agent, field, from_value, patch_json, state, created_at_ms,
        decided_at_ms FROM planning_task_proposals;
DROP TABLE planning_task_proposals;
ALTER TABLE planning_task_proposals_next RENAME TO planning_task_proposals;
CREATE INDEX planning_task_proposals_task ON planning_task_proposals(task_id, state, created_at_ms);
CREATE TRIGGER planning_task_agents_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_task_doers WHERE task_id = old.id;
    DELETE FROM planning_task_proposals WHERE task_id = old.id;
END;";

/// Topics to listen for, each said once per meeting or dictation that
/// raises it: the line that did, and the task made from it.
pub const WATCH_MIGRATION: &str = "CREATE TABLE planning_watches (
    id TEXT PRIMARY KEY NOT NULL,
    phrase TEXT NOT NULL CHECK(length(trim(phrase)) > 0),
    person_id TEXT,
    created_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX planning_watches_phrase ON planning_watches(lower(phrase), coalesce(person_id, ''));
CREATE TABLE planning_watch_hits (
    id TEXT PRIMARY KEY NOT NULL,
    watch_id TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('meeting', 'dictation')),
    source_id TEXT NOT NULL,
    title TEXT NOT NULL,
    quote TEXT NOT NULL CHECK(length(trim(quote)) > 0),
    speaker TEXT,
    person_id TEXT,
    segment_id TEXT CHECK(segment_id IS NULL OR source_kind = 'meeting'),
    at_ms INTEGER NOT NULL CHECK(at_ms >= 0),
    end_ms INTEGER CHECK(end_ms IS NULL OR end_ms >= at_ms),
    start_offset_ns INTEGER,
    said_at_ms INTEGER NOT NULL,
    link TEXT NOT NULL,
    heard_at_ms INTEGER NOT NULL,
    task_id TEXT,
    UNIQUE(watch_id, source_kind, source_id)
);
CREATE INDEX planning_watch_hits_watch ON planning_watch_hits(watch_id, heard_at_ms);
CREATE TRIGGER planning_watch_delete AFTER DELETE ON planning_watches BEGIN
    DELETE FROM planning_watch_hits WHERE watch_id = old.id;
END;";

/// What the project and people pages carry beyond a title: who leads, who is
/// on it, its target day and color, and whether it is put away.
pub const CATALOG_MIGRATION: &str = "ALTER TABLE planning_projects ADD COLUMN lead_person_id TEXT;
ALTER TABLE planning_projects ADD COLUMN target_at_ms INTEGER;
ALTER TABLE planning_projects ADD COLUMN hue INTEGER CHECK(hue IS NULL OR (hue >= 0 AND hue <= 360));
ALTER TABLE planning_projects ADD COLUMN archived_at_ms INTEGER;
ALTER TABLE planning_milestones ADD COLUMN description TEXT;
CREATE TABLE planning_project_members (
    project_id TEXT NOT NULL REFERENCES planning_projects(id) ON DELETE CASCADE,
    person_id TEXT NOT NULL,
    PRIMARY KEY (project_id, person_id)
);
CREATE INDEX planning_project_members_person ON planning_project_members(person_id);";

/// Comments on a task, a reply kept under its thread's first comment, and a
/// task's parent. "parent" and "comment" join what an item's history can
/// say and "parent" what an agent may propose, so both tables are rebuilt as
/// `CYCLES_MIGRATION` did, every row and its rowid kept.
pub const THREADS_MIGRATION: &str = "ALTER TABLE planning_tasks ADD COLUMN parent_id TEXT;
CREATE INDEX planning_tasks_parent ON planning_tasks(parent_id);
CREATE TABLE planning_task_comments (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    parent_comment_id TEXT,
    author_kind TEXT NOT NULL CHECK(author_kind IN ('me', 'sona', 'agent')),
    author_name TEXT CHECK(author_name IS NULL OR length(trim(author_name)) > 0),
    body TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    edited_at_ms INTEGER,
    resolved_at_ms INTEGER,
    deleted_at_ms INTEGER,
    CHECK(deleted_at_ms IS NOT NULL OR length(trim(body)) > 0),
    CHECK(parent_comment_id IS NULL OR resolved_at_ms IS NULL),
    CHECK(parent_comment_id IS NULL OR parent_comment_id != id)
);
CREATE INDEX planning_task_comments_task ON planning_task_comments(task_id, created_at_ms, id);
CREATE INDEX planning_task_comments_thread ON planning_task_comments(parent_comment_id);
CREATE TRIGGER planning_task_threads_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_task_comments WHERE task_id = old.id;
    UPDATE planning_tasks SET parent_id = NULL, revision = revision + 1 WHERE parent_id = old.id;
END;
CREATE TABLE planning_task_activity_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    actor TEXT NOT NULL CHECK(actor IN ('me', 'sona', 'agent')),
    kind TEXT NOT NULL CHECK(kind IN ('created', 'status', 'priority', 'owner', 'due', 'project',
        'title', 'details', 'labels', 'archived', 'restored', 'duplicate', 'accepted', 'declined',
        'snoozed', 'relation', 'kept', 'recurrence', 'cycle', 'block', 'parent', 'comment')),
    from_value TEXT,
    to_value TEXT
);
INSERT INTO planning_task_activity_next (rowid, id, task_id, at_ms, actor, kind, from_value, to_value)
    SELECT rowid, id, task_id, at_ms, actor, kind, from_value, to_value FROM planning_task_activity;
DROP TABLE planning_task_activity;
ALTER TABLE planning_task_activity_next RENAME TO planning_task_activity;
CREATE INDEX planning_task_activity_task ON planning_task_activity(task_id, at_ms, id);
DROP TRIGGER planning_task_agents_delete;
CREATE TABLE planning_task_proposals_next (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    agent TEXT NOT NULL CHECK(length(trim(agent)) > 0),
    field TEXT NOT NULL CHECK(field IN ('title', 'details', 'status', 'priority', 'owner', 'due',
        'project', 'milestone', 'labels', 'snoozed', 'recurrence', 'cycle', 'parent')),
    from_value TEXT,
    patch_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('open', 'accepted', 'rejected')),
    created_at_ms INTEGER NOT NULL,
    decided_at_ms INTEGER,
    CHECK((state = 'open') = (decided_at_ms IS NULL))
);
INSERT INTO planning_task_proposals_next (rowid, id, task_id, agent, field, from_value, patch_json,
        state, created_at_ms, decided_at_ms)
    SELECT rowid, id, task_id, agent, field, from_value, patch_json, state, created_at_ms,
        decided_at_ms FROM planning_task_proposals;
DROP TABLE planning_task_proposals;
ALTER TABLE planning_task_proposals_next RENAME TO planning_task_proposals;
CREATE INDEX planning_task_proposals_task ON planning_task_proposals(task_id, state, created_at_ms);
CREATE TRIGGER planning_task_agents_delete AFTER DELETE ON planning_tasks BEGIN
    DELETE FROM planning_task_doers WHERE task_id = old.id;
    DELETE FROM planning_task_proposals WHERE task_id = old.id;
END;";

/// Every planner migration in order, for tests that build the schema without
/// the rest of the history database.
#[cfg(test)]
pub(crate) fn apply_all(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    for migration in [
        MIGRATION,
        QUIET_REVIEW_MIGRATION,
        MEETING_NOTE_LINK_MIGRATION,
        DISMISSED_LOOP_MIGRATION,
        WORK_MIGRATION,
        PROJECT_DELETE_KEEPS_TASKS_MIGRATION,
        DAY_PLAN_MIGRATION,
        FOLLOW_MIGRATION,
        AGENTS_MIGRATION,
        CAPTURE_MIGRATION,
        REPEAT_MIGRATION,
        CYCLES_MIGRATION,
        WATCH_MIGRATION,
        CATALOG_MIGRATION,
        THREADS_MIGRATION,
    ] {
        conn.execute_batch(migration)?;
    }
    Ok(())
}
