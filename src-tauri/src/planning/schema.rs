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
