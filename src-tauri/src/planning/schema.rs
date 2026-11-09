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
    ] {
        conn.execute_batch(migration)?;
    }
    Ok(())
}
