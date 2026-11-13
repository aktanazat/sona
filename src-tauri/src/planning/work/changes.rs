use crate::managers::history::HistoryManager;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

pub(crate) const PAGE: usize = 50;
pub(crate) const MAX_PAGE: usize = 200;

pub(crate) const CHANGES_MIGRATION: &str = "CREATE TABLE planning_changes (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    at_ms INTEGER NOT NULL,
    entity TEXT NOT NULL CHECK(entity IN ('task', 'project', 'comment', 'session',
        'session_activity')),
    entity_id TEXT NOT NULL,
    task_id TEXT,
    action TEXT NOT NULL CHECK(action IN ('create', 'update', 'remove')),
    updated_from TEXT,
    data TEXT
);
CREATE INDEX planning_changes_at ON planning_changes(at_ms);
CREATE TRIGGER planning_changes_prune AFTER INSERT ON planning_changes BEGIN
    DELETE FROM planning_changes WHERE at_ms < new.at_ms - 2592000000;
END;
CREATE TRIGGER planning_changes_task_create AFTER INSERT ON planning_tasks BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', new.id, new.id,
        'create', json_object('number', new.number, 'title', new.title, 'status', new.status));
END;
CREATE TRIGGER planning_changes_task_remove AFTER DELETE ON planning_tasks BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', old.id, old.id,
        'remove', json_object('number', old.number, 'title', old.title));
END;
CREATE TRIGGER planning_changes_task_update AFTER UPDATE ON planning_tasks
WHEN old.title IS NOT new.title OR old.details IS NOT new.details OR old.status IS NOT new.status
    OR old.priority IS NOT new.priority OR old.owner_person_id IS NOT new.owner_person_id
    OR old.owner_is_self IS NOT new.owner_is_self OR old.due_at_ms IS NOT new.due_at_ms
    OR old.project_id IS NOT new.project_id OR old.milestone_id IS NOT new.milestone_id
    OR old.snoozed_until_ms IS NOT new.snoozed_until_ms
    OR old.archived_at_ms IS NOT new.archived_at_ms OR old.duplicate_of IS NOT new.duplicate_of
    OR old.recurrence IS NOT new.recurrence OR old.cycle IS NOT new.cycle
    OR old.parent_id IS NOT new.parent_id OR old.trashed_at_ms IS NOT new.trashed_at_ms
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', new.id, new.id,
        'update', json_group_object(field, value),
        json_object('number', new.number, 'title', new.title)
    FROM (
        SELECT 'title' AS field, old.title AS value WHERE old.title IS NOT new.title
        UNION ALL SELECT 'description', old.details WHERE old.details IS NOT new.details
        UNION ALL SELECT 'status', old.status WHERE old.status IS NOT new.status
        UNION ALL SELECT 'priority', old.priority WHERE old.priority IS NOT new.priority
        UNION ALL SELECT 'owner', CASE WHEN old.owner_is_self THEN 'me' ELSE old.owner_person_id END
            WHERE old.owner_person_id IS NOT new.owner_person_id
               OR old.owner_is_self IS NOT new.owner_is_self
        UNION ALL SELECT 'due_at_ms', old.due_at_ms WHERE old.due_at_ms IS NOT new.due_at_ms
        UNION ALL SELECT 'project_id', old.project_id WHERE old.project_id IS NOT new.project_id
        UNION ALL SELECT 'milestone_id', old.milestone_id
            WHERE old.milestone_id IS NOT new.milestone_id
        UNION ALL SELECT 'snoozed_until_ms', old.snoozed_until_ms
            WHERE old.snoozed_until_ms IS NOT new.snoozed_until_ms
        UNION ALL SELECT 'archived_at_ms', old.archived_at_ms
            WHERE old.archived_at_ms IS NOT new.archived_at_ms
        UNION ALL SELECT 'duplicate_of', old.duplicate_of
            WHERE old.duplicate_of IS NOT new.duplicate_of
        UNION ALL SELECT 'recurrence', old.recurrence WHERE old.recurrence IS NOT new.recurrence
        UNION ALL SELECT 'cycle', old.cycle WHERE old.cycle IS NOT new.cycle
        UNION ALL SELECT 'parent_id', old.parent_id WHERE old.parent_id IS NOT new.parent_id
        UNION ALL SELECT 'trashed_at_ms', old.trashed_at_ms
            WHERE old.trashed_at_ms IS NOT new.trashed_at_ms
    );
END;
CREATE TRIGGER planning_changes_label_add AFTER INSERT ON planning_task_labels BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', new.task_id,
        new.task_id, 'update', json_object('labels', (
            SELECT json_group_array(label_id) FROM planning_task_labels
            WHERE task_id = new.task_id AND label_id != new.label_id));
END;
CREATE TRIGGER planning_changes_label_remove AFTER DELETE ON planning_task_labels
WHEN EXISTS (SELECT 1 FROM planning_tasks WHERE id = old.task_id) BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', old.task_id,
        old.task_id, 'update', json_object('labels', (
            SELECT json_group_array(label_id) FROM (
                SELECT label_id FROM planning_task_labels WHERE task_id = old.task_id
                UNION ALL SELECT old.label_id)));
END;
CREATE TRIGGER planning_changes_delegate_set AFTER INSERT ON planning_task_doers BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', new.task_id,
        new.task_id, 'update', json_object('delegate', NULL));
END;
CREATE TRIGGER planning_changes_delegate_change AFTER UPDATE OF agent ON planning_task_doers
WHEN old.agent IS NOT new.agent BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', new.task_id,
        new.task_id, 'update', json_object('delegate', old.agent));
END;
CREATE TRIGGER planning_changes_delegate_clear AFTER DELETE ON planning_task_doers
WHEN EXISTS (SELECT 1 FROM planning_tasks WHERE id = old.task_id) BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', old.task_id,
        old.task_id, 'update', json_object('delegate', old.agent));
END;
CREATE TRIGGER planning_changes_project_create AFTER INSERT ON planning_projects BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', new.id,
        'create', json_object('title', new.title, 'status', new.status));
END;
CREATE TRIGGER planning_changes_project_remove AFTER DELETE ON planning_projects BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', old.id,
        'remove', json_object('title', old.title));
END;
CREATE TRIGGER planning_changes_project_update AFTER UPDATE ON planning_projects
WHEN old.title IS NOT new.title OR old.description IS NOT new.description
    OR old.status IS NOT new.status OR old.priority IS NOT new.priority
    OR old.lead_person_id IS NOT new.lead_person_id OR old.start_at_ms IS NOT new.start_at_ms
    OR old.target_at_ms IS NOT new.target_at_ms OR old.archived_at_ms IS NOT new.archived_at_ms
    OR old.trashed_at_ms IS NOT new.trashed_at_ms OR old.icon IS NOT new.icon
    OR old.hue IS NOT new.hue
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', new.id,
        'update', json_group_object(field, value), json_object('title', new.title)
    FROM (
        SELECT 'title' AS field, old.title AS value WHERE old.title IS NOT new.title
        UNION ALL SELECT 'description', old.description
            WHERE old.description IS NOT new.description
        UNION ALL SELECT 'status', old.status WHERE old.status IS NOT new.status
        UNION ALL SELECT 'priority', old.priority WHERE old.priority IS NOT new.priority
        UNION ALL SELECT 'lead_person_id', old.lead_person_id
            WHERE old.lead_person_id IS NOT new.lead_person_id
        UNION ALL SELECT 'start_at_ms', old.start_at_ms WHERE old.start_at_ms IS NOT new.start_at_ms
        UNION ALL SELECT 'target_at_ms', old.target_at_ms
            WHERE old.target_at_ms IS NOT new.target_at_ms
        UNION ALL SELECT 'archived_at_ms', old.archived_at_ms
            WHERE old.archived_at_ms IS NOT new.archived_at_ms
        UNION ALL SELECT 'trashed_at_ms', old.trashed_at_ms
            WHERE old.trashed_at_ms IS NOT new.trashed_at_ms
        UNION ALL SELECT 'icon', old.icon WHERE old.icon IS NOT new.icon
        UNION ALL SELECT 'hue', old.hue WHERE old.hue IS NOT new.hue
    );
END;
CREATE TRIGGER planning_changes_comment_create AFTER INSERT ON planning_task_comments BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'comment', new.id,
        new.task_id, 'create', json_object('author_kind', new.author_kind,
            'author_name', new.author_name, 'parent_id', new.parent_comment_id));
END;
CREATE TRIGGER planning_changes_comment_update AFTER UPDATE ON planning_task_comments
WHEN old.body IS NOT new.body OR old.resolved_at_ms IS NOT new.resolved_at_ms
    OR old.deleted_at_ms IS NOT new.deleted_at_ms
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'comment', new.id,
        new.task_id, 'update', json_group_object(field, value)
    FROM (
        SELECT 'body' AS field, old.body AS value WHERE old.body IS NOT new.body
        UNION ALL SELECT 'resolved_at_ms', old.resolved_at_ms
            WHERE old.resolved_at_ms IS NOT new.resolved_at_ms
        UNION ALL SELECT 'deleted_at_ms', old.deleted_at_ms
            WHERE old.deleted_at_ms IS NOT new.deleted_at_ms
    );
END;
CREATE TRIGGER planning_changes_session_create AFTER INSERT ON planning_agent_sessions BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, data)
    VALUES (new.created_at_ms, 'session', new.id, new.task_id, 'create',
        json_object('agent', new.agent));
END;
CREATE TRIGGER planning_changes_session_update AFTER UPDATE ON planning_agent_sessions
WHEN old.stop_requested_at_ms IS NOT new.stop_requested_at_ms
    OR old.ended_at_ms IS NOT new.ended_at_ms OR old.plan_json IS NOT new.plan_json
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'session', new.id,
        new.task_id, 'update', json_group_object(field, json(value)),
        json_object('agent', new.agent, 'end_kind', new.end_kind)
    FROM (
        SELECT 'stop_requested_at_ms' AS field, coalesce(old.stop_requested_at_ms, 'null') AS value
            WHERE old.stop_requested_at_ms IS NOT new.stop_requested_at_ms
        UNION ALL SELECT 'ended_at_ms', coalesce(old.ended_at_ms, 'null')
            WHERE old.ended_at_ms IS NOT new.ended_at_ms
        UNION ALL SELECT 'plan', old.plan_json WHERE old.plan_json IS NOT new.plan_json
    );
END;
CREATE TRIGGER planning_changes_session_activity AFTER INSERT ON planning_agent_activities BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, data)
    SELECT new.at_ms, 'session_activity', new.id, s.task_id, 'create',
        json_object('session_id', new.session_id, 'kind', new.kind,
            'author_kind', new.author_kind, 'author_name', new.author_name)
    FROM planning_agent_sessions s WHERE s.id = new.session_id;
END;";

pub(crate) const PROJECT_LEAD_CHANGES_MIGRATION: &str =
    "DROP TRIGGER planning_changes_project_update;
CREATE TRIGGER planning_changes_project_update AFTER UPDATE ON planning_projects
WHEN old.title IS NOT new.title OR old.description IS NOT new.description
    OR old.status IS NOT new.status OR old.priority IS NOT new.priority
    OR old.lead_person_id IS NOT new.lead_person_id OR old.lead_is_self IS NOT new.lead_is_self
    OR old.start_at_ms IS NOT new.start_at_ms
    OR old.target_at_ms IS NOT new.target_at_ms OR old.archived_at_ms IS NOT new.archived_at_ms
    OR old.trashed_at_ms IS NOT new.trashed_at_ms OR old.icon IS NOT new.icon
    OR old.hue IS NOT new.hue
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', new.id,
        'update', json_group_object(field, value), json_object('title', new.title)
    FROM (
        SELECT 'title' AS field, old.title AS value WHERE old.title IS NOT new.title
        UNION ALL SELECT 'description', old.description
            WHERE old.description IS NOT new.description
        UNION ALL SELECT 'status', old.status WHERE old.status IS NOT new.status
        UNION ALL SELECT 'priority', old.priority WHERE old.priority IS NOT new.priority
        UNION ALL SELECT 'lead', CASE WHEN old.lead_is_self THEN 'me' ELSE old.lead_person_id END
            WHERE old.lead_person_id IS NOT new.lead_person_id
               OR old.lead_is_self IS NOT new.lead_is_self
        UNION ALL SELECT 'start_at_ms', old.start_at_ms WHERE old.start_at_ms IS NOT new.start_at_ms
        UNION ALL SELECT 'target_at_ms', old.target_at_ms
            WHERE old.target_at_ms IS NOT new.target_at_ms
        UNION ALL SELECT 'archived_at_ms', old.archived_at_ms
            WHERE old.archived_at_ms IS NOT new.archived_at_ms
        UNION ALL SELECT 'trashed_at_ms', old.trashed_at_ms
            WHERE old.trashed_at_ms IS NOT new.trashed_at_ms
        UNION ALL SELECT 'icon', old.icon WHERE old.icon IS NOT new.icon
        UNION ALL SELECT 'hue', old.hue WHERE old.hue IS NOT new.hue
    );
END;";

/// Milestones, cycles, views, label definitions, project updates and the
/// comments on projects, milestones, meetings and notes join the stream.
/// SQLite cannot widen a CHECK in place, so the stream is rebuilt with
/// every change and its sequence kept, and every trigger that writes to it
/// is dropped and recreated around the rebuild. `sources` keeps the
/// meetings and meeting loops a change was drawn from when it was written,
/// so a reader can still leave it out once the task or note is gone.
pub(crate) const COVERAGE_MIGRATION: &str = "DROP TRIGGER planning_changes_task_create;
DROP TRIGGER planning_changes_task_remove;
DROP TRIGGER planning_changes_task_update;
DROP TRIGGER planning_changes_label_add;
DROP TRIGGER planning_changes_label_remove;
DROP TRIGGER planning_changes_delegate_set;
DROP TRIGGER planning_changes_delegate_change;
DROP TRIGGER planning_changes_delegate_clear;
DROP TRIGGER planning_changes_project_create;
DROP TRIGGER planning_changes_project_remove;
DROP TRIGGER planning_changes_project_update;
DROP TRIGGER planning_changes_comment_create;
DROP TRIGGER planning_changes_comment_update;
DROP TRIGGER planning_changes_session_create;
DROP TRIGGER planning_changes_session_update;
DROP TRIGGER planning_changes_session_activity;
CREATE TABLE planning_changes_next (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    at_ms INTEGER NOT NULL,
    entity TEXT NOT NULL CHECK(entity IN ('task', 'project', 'comment', 'session',
        'session_activity', 'milestone', 'cycle', 'view', 'label', 'project_update', 'note',
        'meeting')),
    entity_id TEXT NOT NULL,
    task_id TEXT,
    action TEXT NOT NULL CHECK(action IN ('create', 'update', 'remove')),
    updated_from TEXT,
    data TEXT,
    sources TEXT
);
INSERT INTO planning_changes_next (seq, at_ms, entity, entity_id, task_id, action, updated_from, data)
    SELECT seq, at_ms, entity, entity_id, task_id, action, updated_from, data FROM planning_changes;
DELETE FROM sqlite_sequence WHERE name = 'planning_changes_next';
INSERT INTO sqlite_sequence (name, seq)
    SELECT 'planning_changes_next', seq FROM sqlite_sequence WHERE name = 'planning_changes';
DROP TABLE planning_changes;
ALTER TABLE planning_changes_next RENAME TO planning_changes;
CREATE INDEX planning_changes_at ON planning_changes(at_ms);
CREATE TRIGGER planning_changes_prune AFTER INSERT ON planning_changes BEGIN
    DELETE FROM planning_changes WHERE at_ms < new.at_ms - 2592000000;
END;
CREATE TRIGGER planning_changes_task_create AFTER INSERT ON planning_tasks BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', new.id, new.id,
        'create', json_object('number', new.number, 'title', new.title, 'status', new.status));
END;
CREATE TRIGGER planning_changes_task_remove BEFORE DELETE ON planning_tasks BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, data, sources)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', old.id, old.id,
        'remove', json_object('number', old.number, 'title', old.title),
        (SELECT nullif(json_group_array(json_array(kind, id)), '[]') FROM (
            SELECT source_kind AS kind, source_id AS id FROM planning_evidence_links
            WHERE target_kind = 'task' AND target_id = old.id AND source_kind IN ('meeting', 'loop')
            UNION SELECT 'loop', old.source_loop_id WHERE old.source_loop_id IS NOT NULL)));
END;
CREATE TRIGGER planning_changes_task_update AFTER UPDATE ON planning_tasks
WHEN old.title IS NOT new.title OR old.details IS NOT new.details OR old.status IS NOT new.status
    OR old.priority IS NOT new.priority OR old.owner_person_id IS NOT new.owner_person_id
    OR old.owner_is_self IS NOT new.owner_is_self OR old.due_at_ms IS NOT new.due_at_ms
    OR old.project_id IS NOT new.project_id OR old.milestone_id IS NOT new.milestone_id
    OR old.snoozed_until_ms IS NOT new.snoozed_until_ms
    OR old.archived_at_ms IS NOT new.archived_at_ms OR old.duplicate_of IS NOT new.duplicate_of
    OR old.recurrence IS NOT new.recurrence OR old.cycle IS NOT new.cycle
    OR old.parent_id IS NOT new.parent_id OR old.trashed_at_ms IS NOT new.trashed_at_ms
    OR old.estimate IS NOT new.estimate OR old.for_person_id IS NOT new.for_person_id
BEGIN
    INSERT INTO planning_changes
        (at_ms, entity, entity_id, task_id, action, updated_from, data, sources)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', new.id, new.id,
        'update', json_group_object(field, value),
        json_object('number', new.number, 'title', new.title),
        (SELECT nullif(json_group_array(json_array(kind, id)), '[]') FROM (
            SELECT source_kind AS kind, source_id AS id FROM planning_evidence_links
            WHERE target_kind = 'task' AND target_id = new.id AND source_kind IN ('meeting', 'loop')
            UNION SELECT 'loop', new.source_loop_id WHERE new.source_loop_id IS NOT NULL))
    FROM (
        SELECT 'title' AS field, old.title AS value WHERE old.title IS NOT new.title
        UNION ALL SELECT 'description', old.details WHERE old.details IS NOT new.details
        UNION ALL SELECT 'status', old.status WHERE old.status IS NOT new.status
        UNION ALL SELECT 'priority', old.priority WHERE old.priority IS NOT new.priority
        UNION ALL SELECT 'owner', CASE WHEN old.owner_is_self THEN 'me' ELSE old.owner_person_id END
            WHERE old.owner_person_id IS NOT new.owner_person_id
               OR old.owner_is_self IS NOT new.owner_is_self
        UNION ALL SELECT 'due_at_ms', old.due_at_ms WHERE old.due_at_ms IS NOT new.due_at_ms
        UNION ALL SELECT 'project_id', old.project_id WHERE old.project_id IS NOT new.project_id
        UNION ALL SELECT 'milestone_id', old.milestone_id
            WHERE old.milestone_id IS NOT new.milestone_id
        UNION ALL SELECT 'snoozed_until_ms', old.snoozed_until_ms
            WHERE old.snoozed_until_ms IS NOT new.snoozed_until_ms
        UNION ALL SELECT 'archived_at_ms', old.archived_at_ms
            WHERE old.archived_at_ms IS NOT new.archived_at_ms
        UNION ALL SELECT 'duplicate_of', old.duplicate_of
            WHERE old.duplicate_of IS NOT new.duplicate_of
        UNION ALL SELECT 'recurrence', old.recurrence WHERE old.recurrence IS NOT new.recurrence
        UNION ALL SELECT 'cycle', old.cycle WHERE old.cycle IS NOT new.cycle
        UNION ALL SELECT 'parent_id', old.parent_id WHERE old.parent_id IS NOT new.parent_id
        UNION ALL SELECT 'trashed_at_ms', old.trashed_at_ms
            WHERE old.trashed_at_ms IS NOT new.trashed_at_ms
        UNION ALL SELECT 'estimate', old.estimate WHERE old.estimate IS NOT new.estimate
        UNION ALL SELECT 'for_person_id', old.for_person_id
            WHERE old.for_person_id IS NOT new.for_person_id
    );
END;
CREATE TRIGGER planning_changes_label_add AFTER INSERT ON planning_task_labels BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', new.task_id,
        new.task_id, 'update', json_object('labels', (
            SELECT json_group_array(label_id) FROM planning_task_labels
            WHERE task_id = new.task_id AND label_id != new.label_id));
END;
CREATE TRIGGER planning_changes_label_remove AFTER DELETE ON planning_task_labels
WHEN EXISTS (SELECT 1 FROM planning_tasks WHERE id = old.task_id) BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', old.task_id,
        old.task_id, 'update', json_object('labels', (
            SELECT json_group_array(label_id) FROM (
                SELECT label_id FROM planning_task_labels WHERE task_id = old.task_id
                UNION ALL SELECT old.label_id)));
END;
CREATE TRIGGER planning_changes_delegate_set AFTER INSERT ON planning_task_doers BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', new.task_id,
        new.task_id, 'update', json_object('delegate', NULL));
END;
CREATE TRIGGER planning_changes_delegate_change AFTER UPDATE OF agent ON planning_task_doers
WHEN old.agent IS NOT new.agent BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', new.task_id,
        new.task_id, 'update', json_object('delegate', old.agent));
END;
CREATE TRIGGER planning_changes_delegate_clear AFTER DELETE ON planning_task_doers
WHEN EXISTS (SELECT 1 FROM planning_tasks WHERE id = old.task_id) BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', old.task_id,
        old.task_id, 'update', json_object('delegate', old.agent));
END;
CREATE TRIGGER planning_changes_project_create AFTER INSERT ON planning_projects BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', new.id,
        'create', json_object('title', new.title, 'status', new.status));
END;
CREATE TRIGGER planning_changes_project_remove AFTER DELETE ON planning_projects BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', old.id,
        'remove', json_object('title', old.title));
END;
CREATE TRIGGER planning_changes_project_update AFTER UPDATE ON planning_projects
WHEN old.title IS NOT new.title OR old.description IS NOT new.description
    OR old.status IS NOT new.status OR old.priority IS NOT new.priority
    OR old.lead_person_id IS NOT new.lead_person_id OR old.lead_is_self IS NOT new.lead_is_self
    OR old.start_at_ms IS NOT new.start_at_ms
    OR old.target_at_ms IS NOT new.target_at_ms OR old.archived_at_ms IS NOT new.archived_at_ms
    OR old.trashed_at_ms IS NOT new.trashed_at_ms OR old.icon IS NOT new.icon
    OR old.hue IS NOT new.hue
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', new.id,
        'update', json_group_object(field, value), json_object('title', new.title)
    FROM (
        SELECT 'title' AS field, old.title AS value WHERE old.title IS NOT new.title
        UNION ALL SELECT 'description', old.description
            WHERE old.description IS NOT new.description
        UNION ALL SELECT 'status', old.status WHERE old.status IS NOT new.status
        UNION ALL SELECT 'priority', old.priority WHERE old.priority IS NOT new.priority
        UNION ALL SELECT 'lead', CASE WHEN old.lead_is_self THEN 'me' ELSE old.lead_person_id END
            WHERE old.lead_person_id IS NOT new.lead_person_id
               OR old.lead_is_self IS NOT new.lead_is_self
        UNION ALL SELECT 'start_at_ms', old.start_at_ms WHERE old.start_at_ms IS NOT new.start_at_ms
        UNION ALL SELECT 'target_at_ms', old.target_at_ms
            WHERE old.target_at_ms IS NOT new.target_at_ms
        UNION ALL SELECT 'archived_at_ms', old.archived_at_ms
            WHERE old.archived_at_ms IS NOT new.archived_at_ms
        UNION ALL SELECT 'trashed_at_ms', old.trashed_at_ms
            WHERE old.trashed_at_ms IS NOT new.trashed_at_ms
        UNION ALL SELECT 'icon', old.icon WHERE old.icon IS NOT new.icon
        UNION ALL SELECT 'hue', old.hue WHERE old.hue IS NOT new.hue
    );
END;
CREATE TRIGGER planning_changes_comment_create AFTER INSERT ON planning_task_comments BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'comment', new.id,
        new.task_id, 'create', json_object('author_kind', new.author_kind,
            'author_name', new.author_name, 'parent_id', new.parent_comment_id));
END;
CREATE TRIGGER planning_changes_comment_update AFTER UPDATE ON planning_task_comments
WHEN old.body IS NOT new.body OR old.resolved_at_ms IS NOT new.resolved_at_ms
    OR old.deleted_at_ms IS NOT new.deleted_at_ms
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'comment', new.id,
        new.task_id, 'update', json_group_object(field, value)
    FROM (
        SELECT 'body' AS field, old.body AS value WHERE old.body IS NOT new.body
        UNION ALL SELECT 'resolved_at_ms', old.resolved_at_ms
            WHERE old.resolved_at_ms IS NOT new.resolved_at_ms
        UNION ALL SELECT 'deleted_at_ms', old.deleted_at_ms
            WHERE old.deleted_at_ms IS NOT new.deleted_at_ms
    );
END;
CREATE TRIGGER planning_changes_session_create AFTER INSERT ON planning_agent_sessions BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, data)
    VALUES (new.created_at_ms, 'session', new.id, new.task_id, 'create',
        json_object('agent', new.agent));
END;
CREATE TRIGGER planning_changes_session_update AFTER UPDATE ON planning_agent_sessions
WHEN old.stop_requested_at_ms IS NOT new.stop_requested_at_ms
    OR old.ended_at_ms IS NOT new.ended_at_ms OR old.plan_json IS NOT new.plan_json
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'session', new.id,
        new.task_id, 'update', json_group_object(field, json(value)),
        json_object('agent', new.agent, 'end_kind', new.end_kind)
    FROM (
        SELECT 'stop_requested_at_ms' AS field, coalesce(old.stop_requested_at_ms, 'null') AS value
            WHERE old.stop_requested_at_ms IS NOT new.stop_requested_at_ms
        UNION ALL SELECT 'ended_at_ms', coalesce(old.ended_at_ms, 'null')
            WHERE old.ended_at_ms IS NOT new.ended_at_ms
        UNION ALL SELECT 'plan', old.plan_json WHERE old.plan_json IS NOT new.plan_json
    );
END;
CREATE TRIGGER planning_changes_session_activity AFTER INSERT ON planning_agent_activities BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, data)
    SELECT new.at_ms, 'session_activity', new.id, s.task_id, 'create',
        json_object('session_id', new.session_id, 'kind', new.kind,
            'author_kind', new.author_kind, 'author_name', new.author_name)
    FROM planning_agent_sessions s WHERE s.id = new.session_id;
END;
CREATE TRIGGER planning_changes_milestone_create AFTER INSERT ON planning_milestones BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'milestone', new.id,
        'create', json_object('project_id', new.project_id, 'title', new.title,
            'status', new.status));
END;
CREATE TRIGGER planning_changes_milestone_remove AFTER DELETE ON planning_milestones BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'milestone', old.id,
        'remove', json_object('project_id', old.project_id, 'title', old.title));
END;
CREATE TRIGGER planning_changes_milestone_update AFTER UPDATE ON planning_milestones
WHEN old.title IS NOT new.title OR old.description IS NOT new.description
    OR old.status IS NOT new.status OR old.due_at_ms IS NOT new.due_at_ms
    OR old.project_id IS NOT new.project_id
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'milestone', new.id,
        'update', json_group_object(field, value),
        json_object('project_id', new.project_id, 'title', new.title)
    FROM (
        SELECT 'title' AS field, old.title AS value WHERE old.title IS NOT new.title
        UNION ALL SELECT 'description', old.description
            WHERE old.description IS NOT new.description
        UNION ALL SELECT 'status', old.status WHERE old.status IS NOT new.status
        UNION ALL SELECT 'due_at_ms', old.due_at_ms WHERE old.due_at_ms IS NOT new.due_at_ms
        UNION ALL SELECT 'project_id', old.project_id WHERE old.project_id IS NOT new.project_id
    );
END;
CREATE TRIGGER planning_changes_cycle_create AFTER INSERT ON planning_cycles BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'cycle',
        CAST(new.number AS TEXT), 'create', json_object('number', new.number, 'name', new.name,
            'start_day', new.start_day, 'end_day', new.end_day));
END;
CREATE TRIGGER planning_changes_cycle_remove AFTER DELETE ON planning_cycles BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'cycle',
        CAST(old.number AS TEXT), 'remove', json_object('number', old.number, 'name', old.name));
END;
CREATE TRIGGER planning_changes_cycle_update AFTER UPDATE ON planning_cycles
WHEN old.name IS NOT new.name OR old.start_day IS NOT new.start_day
    OR old.end_day IS NOT new.end_day
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'cycle',
        CAST(new.number AS TEXT), 'update', json_group_object(field, value),
        json_object('number', new.number, 'name', new.name)
    FROM (
        SELECT 'name' AS field, old.name AS value WHERE old.name IS NOT new.name
        UNION ALL SELECT 'start_day', old.start_day WHERE old.start_day IS NOT new.start_day
        UNION ALL SELECT 'end_day', old.end_day WHERE old.end_day IS NOT new.end_day
    );
END;
CREATE TRIGGER planning_changes_view_create AFTER INSERT ON planning_views BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'view', new.id,
        'create', json_object('name', new.name));
END;
CREATE TRIGGER planning_changes_view_remove AFTER DELETE ON planning_views BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'view', old.id,
        'remove', json_object('name', old.name));
END;
CREATE TRIGGER planning_changes_view_update AFTER UPDATE ON planning_views
WHEN old.name IS NOT new.name OR old.query_json IS NOT new.query_json
    OR old.display_json IS NOT new.display_json
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'view', new.id,
        'update', json_group_object(field, json(value)), json_object('name', new.name)
    FROM (
        SELECT 'name' AS field, json_quote(old.name) AS value WHERE old.name IS NOT new.name
        UNION ALL SELECT 'query', old.query_json WHERE old.query_json IS NOT new.query_json
        UNION ALL SELECT 'display', old.display_json
            WHERE old.display_json IS NOT new.display_json
    );
END;
CREATE TRIGGER planning_changes_label_create AFTER INSERT ON planning_labels BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'label', new.id,
        'create', json_object('name', new.name, 'hue', new.hue, 'group', new.group_name));
END;
CREATE TRIGGER planning_changes_label_delete AFTER DELETE ON planning_labels BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'label', old.id,
        'remove', json_object('name', old.name));
END;
CREATE TRIGGER planning_changes_label_update AFTER UPDATE ON planning_labels
WHEN old.name IS NOT new.name OR old.hue IS NOT new.hue
    OR old.group_name IS NOT new.group_name OR old.retired_at_ms IS NOT new.retired_at_ms
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'label', new.id,
        'update', json_group_object(field, value), json_object('name', new.name)
    FROM (
        SELECT 'name' AS field, old.name AS value WHERE old.name IS NOT new.name
        UNION ALL SELECT 'hue', old.hue WHERE old.hue IS NOT new.hue
        UNION ALL SELECT 'group', old.group_name WHERE old.group_name IS NOT new.group_name
        UNION ALL SELECT 'retired_at_ms', old.retired_at_ms
            WHERE old.retired_at_ms IS NOT new.retired_at_ms
    );
END;
CREATE TRIGGER planning_changes_project_update_create AFTER INSERT ON planning_project_updates
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project_update', new.id,
        'create', json_object('project_id', new.project_id, 'health', new.health,
            'author_kind', new.author_kind, 'author_name', new.author_name));
END;
CREATE TRIGGER planning_changes_project_update_remove AFTER DELETE ON planning_project_updates
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project_update', old.id,
        'remove', json_object('project_id', old.project_id));
END;
CREATE TRIGGER planning_changes_project_update_update AFTER UPDATE ON planning_project_updates
WHEN old.body IS NOT new.body OR old.health IS NOT new.health
    OR old.deleted_at_ms IS NOT new.deleted_at_ms
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project_update', new.id,
        'update', json_group_object(field, value), json_object('project_id', new.project_id)
    FROM (
        SELECT 'body' AS field, old.body AS value WHERE old.body IS NOT new.body
        UNION ALL SELECT 'health', old.health WHERE old.health IS NOT new.health
        UNION ALL SELECT 'deleted_at_ms', old.deleted_at_ms
            WHERE old.deleted_at_ms IS NOT new.deleted_at_ms
    );
END;
CREATE TRIGGER planning_changes_subject_comment_create AFTER INSERT ON planning_comments BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data, sources)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'comment', new.id,
        'create', json_object('subject_kind', new.subject_kind, 'subject_id', new.subject_id,
            'author_kind', new.author_kind, 'author_name', new.author_name,
            'parent_id', new.parent_comment_id),
        CASE WHEN new.subject_kind = 'meeting'
            THEN json_array(json_array('meeting', new.subject_id)) END);
END;
CREATE TRIGGER planning_changes_subject_comment_update AFTER UPDATE ON planning_comments
WHEN old.body IS NOT new.body OR old.resolved_at_ms IS NOT new.resolved_at_ms
    OR old.deleted_at_ms IS NOT new.deleted_at_ms
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data, sources)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'comment', new.id,
        'update', json_group_object(field, value),
        json_object('subject_kind', new.subject_kind, 'subject_id', new.subject_id),
        CASE WHEN new.subject_kind = 'meeting'
            THEN json_array(json_array('meeting', new.subject_id)) END
    FROM (
        SELECT 'body' AS field, old.body AS value WHERE old.body IS NOT new.body
        UNION ALL SELECT 'resolved_at_ms', old.resolved_at_ms
            WHERE old.resolved_at_ms IS NOT new.resolved_at_ms
        UNION ALL SELECT 'deleted_at_ms', old.deleted_at_ms
            WHERE old.deleted_at_ms IS NOT new.deleted_at_ms
    );
END;";

/// A note's changes. Kept apart from [`COVERAGE_MIGRATION`] because notes
/// are not in the planner's own schema. A note's earlier text stays in its
/// versions rather than in the stream: autosave would copy the whole note
/// at every pause in typing.
pub(crate) const NOTE_CHANGES_MIGRATION: &str =
    "CREATE TRIGGER planning_changes_note_create AFTER INSERT ON scratch_notes BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'note', new.id,
        'create', json_object('title', new.title));
END;
CREATE TRIGGER planning_changes_note_remove BEFORE DELETE ON scratch_notes BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data, sources)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'note', old.id,
        'remove', json_object('title', old.title),
        (SELECT nullif(json_group_array(json_array('meeting', target_id)), '[]')
            FROM scratch_note_sources WHERE note_id = old.id AND kind = 'meeting'));
END;
CREATE TRIGGER planning_changes_note_update AFTER UPDATE ON scratch_notes
WHEN old.title IS NOT new.title OR old.body IS NOT new.body OR old.pinned IS NOT new.pinned
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data, sources)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'note', new.id,
        'update', json_group_object(field, value), json_object('title', new.title),
        (SELECT nullif(json_group_array(json_array('meeting', target_id)), '[]')
            FROM scratch_note_sources WHERE note_id = new.id AND kind = 'meeting')
    FROM (
        SELECT 'title' AS field, old.title AS value WHERE old.title IS NOT new.title
        UNION ALL SELECT 'body', NULL WHERE old.body IS NOT new.body
        UNION ALL SELECT 'pinned', old.pinned WHERE old.pinned IS NOT new.pinned
    );
END;";

/// A project's summary and status name join its tracked fields. The status
/// name is recorded as the name it had, as the project's history does.
pub(crate) const PROJECT_FIELDS_CHANGES_MIGRATION: &str =
    "DROP TRIGGER planning_changes_project_update;
CREATE TRIGGER planning_changes_project_update AFTER UPDATE ON planning_projects
WHEN old.title IS NOT new.title OR old.description IS NOT new.description
    OR old.status IS NOT new.status OR old.priority IS NOT new.priority
    OR old.lead_person_id IS NOT new.lead_person_id OR old.lead_is_self IS NOT new.lead_is_self
    OR old.start_at_ms IS NOT new.start_at_ms
    OR old.target_at_ms IS NOT new.target_at_ms OR old.archived_at_ms IS NOT new.archived_at_ms
    OR old.trashed_at_ms IS NOT new.trashed_at_ms OR old.icon IS NOT new.icon
    OR old.hue IS NOT new.hue OR old.summary IS NOT new.summary
    OR old.status_name_id IS NOT new.status_name_id
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'project', new.id,
        'update', json_group_object(field, value), json_object('title', new.title)
    FROM (
        SELECT 'title' AS field, old.title AS value WHERE old.title IS NOT new.title
        UNION ALL SELECT 'description', old.description
            WHERE old.description IS NOT new.description
        UNION ALL SELECT 'status', old.status WHERE old.status IS NOT new.status
        UNION ALL SELECT 'priority', old.priority WHERE old.priority IS NOT new.priority
        UNION ALL SELECT 'lead', CASE WHEN old.lead_is_self THEN 'me' ELSE old.lead_person_id END
            WHERE old.lead_person_id IS NOT new.lead_person_id
               OR old.lead_is_self IS NOT new.lead_is_self
        UNION ALL SELECT 'start_at_ms', old.start_at_ms WHERE old.start_at_ms IS NOT new.start_at_ms
        UNION ALL SELECT 'target_at_ms', old.target_at_ms
            WHERE old.target_at_ms IS NOT new.target_at_ms
        UNION ALL SELECT 'archived_at_ms', old.archived_at_ms
            WHERE old.archived_at_ms IS NOT new.archived_at_ms
        UNION ALL SELECT 'trashed_at_ms', old.trashed_at_ms
            WHERE old.trashed_at_ms IS NOT new.trashed_at_ms
        UNION ALL SELECT 'icon', old.icon WHERE old.icon IS NOT new.icon
        UNION ALL SELECT 'hue', old.hue WHERE old.hue IS NOT new.hue
        UNION ALL SELECT 'summary', old.summary WHERE old.summary IS NOT new.summary
        UNION ALL SELECT 'status_name',
            (SELECT name FROM planning_project_status_names WHERE id = old.status_name_id)
            WHERE old.status_name_id IS NOT new.status_name_id
    );
END;";

/// Initiatives join the stream. The rebuild runs with the legacy rename so
/// SQLite neither rewrites nor re-checks the triggers on other tables that
/// write to the stream; they name it, and find it again once it is renamed.
pub(crate) const INITIATIVE_CHANGES_MIGRATION: &str = "PRAGMA legacy_alter_table = ON;
CREATE TABLE planning_changes_next (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    at_ms INTEGER NOT NULL,
    entity TEXT NOT NULL CHECK(entity IN ('task', 'project', 'comment', 'session',
        'session_activity', 'milestone', 'cycle', 'view', 'label', 'project_update', 'note',
        'meeting', 'initiative')),
    entity_id TEXT NOT NULL,
    task_id TEXT,
    action TEXT NOT NULL CHECK(action IN ('create', 'update', 'remove')),
    updated_from TEXT,
    data TEXT,
    sources TEXT
);
INSERT INTO planning_changes_next
    (seq, at_ms, entity, entity_id, task_id, action, updated_from, data, sources)
    SELECT seq, at_ms, entity, entity_id, task_id, action, updated_from, data, sources
    FROM planning_changes;
DELETE FROM sqlite_sequence WHERE name = 'planning_changes_next';
INSERT INTO sqlite_sequence (name, seq)
    SELECT 'planning_changes_next', seq FROM sqlite_sequence WHERE name = 'planning_changes';
DROP TABLE planning_changes;
ALTER TABLE planning_changes_next RENAME TO planning_changes;
PRAGMA legacy_alter_table = OFF;
CREATE INDEX planning_changes_at ON planning_changes(at_ms);
CREATE TRIGGER planning_changes_prune AFTER INSERT ON planning_changes BEGIN
    DELETE FROM planning_changes WHERE at_ms < new.at_ms - 2592000000;
END;
CREATE TRIGGER planning_changes_initiative_create AFTER INSERT ON planning_initiatives BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'initiative', new.id,
        'create', json_object('name', new.name, 'status', new.status));
END;
CREATE TRIGGER planning_changes_initiative_remove AFTER DELETE ON planning_initiatives BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'initiative', old.id,
        'remove', json_object('name', old.name));
END;
CREATE TRIGGER planning_changes_initiative_update AFTER UPDATE ON planning_initiatives
WHEN old.name IS NOT new.name OR old.description IS NOT new.description
    OR old.owner_person_id IS NOT new.owner_person_id OR old.owner_is_self IS NOT new.owner_is_self
    OR old.target_at_ms IS NOT new.target_at_ms OR old.status IS NOT new.status
BEGIN
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, updated_from, data)
    SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'initiative', new.id,
        'update', json_group_object(field, value), json_object('name', new.name)
    FROM (
        SELECT 'name' AS field, old.name AS value WHERE old.name IS NOT new.name
        UNION ALL SELECT 'description', old.description
            WHERE old.description IS NOT new.description
        UNION ALL SELECT 'owner', CASE WHEN old.owner_is_self THEN 'me' ELSE old.owner_person_id END
            WHERE old.owner_person_id IS NOT new.owner_person_id
               OR old.owner_is_self IS NOT new.owner_is_self
        UNION ALL SELECT 'target_at_ms', old.target_at_ms
            WHERE old.target_at_ms IS NOT new.target_at_ms
        UNION ALL SELECT 'status', old.status WHERE old.status IS NOT new.status
    );
END;";

/// A task's meeting and loop links go with it when it is deleted, so the
/// task's earlier changes take them as sources first: a reader can then
/// still leave out every change on work drawn from a meeting kept on this
/// Mac. Changes on a task already gone that name no source get an
/// `unknown` one, since nothing says where that task came from.
pub(crate) const TASK_PURGE_SOURCES_MIGRATION: &str =
    "DROP TRIGGER planning_changes_task_remove;
CREATE TRIGGER planning_changes_task_remove BEFORE DELETE ON planning_tasks BEGIN
    UPDATE planning_changes SET sources = (
        SELECT json_group_array(json_array(kind, id)) FROM (
            SELECT json_extract(j.value, '$[0]') AS kind, json_extract(j.value, '$[1]') AS id
            FROM json_each(coalesce(planning_changes.sources, '[]')) j
            UNION SELECT source_kind, source_id FROM planning_evidence_links
            WHERE target_kind = 'task' AND target_id = old.id AND source_kind IN ('meeting', 'loop')
            UNION SELECT 'loop', old.source_loop_id WHERE old.source_loop_id IS NOT NULL))
    WHERE task_id = old.id AND (old.source_loop_id IS NOT NULL OR EXISTS (
        SELECT 1 FROM planning_evidence_links
        WHERE target_kind = 'task' AND target_id = old.id AND source_kind IN ('meeting', 'loop')));
    INSERT INTO planning_changes (at_ms, entity, entity_id, task_id, action, data, sources)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'task', old.id, old.id,
        'remove', json_object('number', old.number, 'title', old.title),
        (SELECT nullif(json_group_array(json_array(kind, id)), '[]') FROM (
            SELECT source_kind AS kind, source_id AS id FROM planning_evidence_links
            WHERE target_kind = 'task' AND target_id = old.id AND source_kind IN ('meeting', 'loop')
            UNION SELECT 'loop', old.source_loop_id WHERE old.source_loop_id IS NOT NULL)));
END;
UPDATE planning_changes SET sources = coalesce(
    (SELECT other.sources FROM planning_changes other
     WHERE other.task_id = planning_changes.task_id AND other.sources IS NOT NULL
     ORDER BY other.seq DESC LIMIT 1),
    json_array(json_array('unknown', task_id)))
WHERE task_id IS NOT NULL AND sources IS NULL
    AND task_id NOT IN (SELECT id FROM planning_tasks);";

/// The same for notes: a deleted note's meeting links go with it, so its
/// earlier changes and the comments on it take them as sources first.
pub(crate) const NOTE_PURGE_SOURCES_MIGRATION: &str =
    "DROP TRIGGER planning_changes_note_remove;
CREATE TRIGGER planning_changes_note_remove BEFORE DELETE ON scratch_notes BEGIN
    UPDATE planning_changes SET sources = (
        SELECT json_group_array(json_array(kind, id)) FROM (
            SELECT json_extract(j.value, '$[0]') AS kind, json_extract(j.value, '$[1]') AS id
            FROM json_each(coalesce(planning_changes.sources, '[]')) j
            UNION SELECT 'meeting', target_id FROM scratch_note_sources
            WHERE note_id = old.id AND kind = 'meeting'))
    WHERE ((entity = 'note' AND entity_id = old.id)
            OR (entity = 'comment' AND json_extract(data, '$.subject_kind') = 'note'
                AND json_extract(data, '$.subject_id') = old.id))
        AND EXISTS (SELECT 1 FROM scratch_note_sources WHERE note_id = old.id AND kind = 'meeting');
    INSERT INTO planning_changes (at_ms, entity, entity_id, action, data, sources)
    VALUES (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), 'note', old.id,
        'remove', json_object('title', old.title),
        (SELECT nullif(json_group_array(json_array('meeting', target_id)), '[]')
            FROM scratch_note_sources WHERE note_id = old.id AND kind = 'meeting'));
END;
UPDATE planning_changes SET sources = coalesce(
    (SELECT other.sources FROM planning_changes other
     WHERE other.entity = 'note' AND other.entity_id = coalesce(
         CASE WHEN planning_changes.entity = 'note' THEN planning_changes.entity_id END,
         json_extract(planning_changes.data, '$.subject_id'))
       AND other.sources IS NOT NULL
     ORDER BY other.seq DESC LIMIT 1),
    json_array(json_array('unknown', coalesce(
        CASE WHEN entity = 'note' THEN entity_id END, json_extract(data, '$.subject_id')))))
WHERE sources IS NULL
    AND ((entity = 'note' AND entity_id NOT IN (SELECT id FROM scratch_notes))
        OR (entity = 'comment' AND json_extract(data, '$.subject_kind') = 'note'
            AND json_extract(data, '$.subject_id') NOT IN (SELECT id FROM scratch_notes)));";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkChange {
    pub seq: i64,
    pub at_ms: i64,
    pub entity: String,
    pub entity_id: String,
    pub item_id: Option<String>,
    pub action: String,
    pub updated_from: Option<serde_json::Value>,
    pub data: Option<serde_json::Value>,
    /// The meetings and meeting loops the change was drawn from when it was
    /// written, as (kind, id): what a reader outside the app checks against
    /// the meetings kept on this Mac.
    #[serde(skip)]
    pub sources: Vec<(String, String)>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkChangePage {
    pub changes: Vec<WorkChange>,
    pub next_cursor: i64,
    pub has_more: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CursorRefusal {
    Expired { oldest: i64 },
    Ahead { head: i64 },
}

fn json(value: Option<String>) -> Result<Option<serde_json::Value>> {
    Ok(value.map(|text| serde_json::from_str(&text)).transpose()?)
}

fn head(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'planning_changes'",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

pub(crate) fn page(
    conn: &Connection,
    since: Option<i64>,
    limit: usize,
) -> Result<std::result::Result<WorkChangePage, CursorRefusal>> {
    let head = head(conn)?;
    let oldest: i64 = conn.query_row(
        "SELECT coalesce(min(seq), ?1 + 1) FROM planning_changes",
        [head],
        |row| row.get(0),
    )?;
    let after = match since {
        Some(cursor) if cursor > head || cursor < 0 => {
            return Ok(Err(CursorRefusal::Ahead { head }))
        }
        Some(cursor) if cursor + 1 < oldest => return Ok(Err(CursorRefusal::Expired { oldest })),
        Some(cursor) => cursor,
        None => oldest - 1,
    };
    let fetch = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
    let mut statement = conn.prepare(
        "SELECT seq, at_ms, entity, entity_id, task_id, action, updated_from, data, sources
         FROM planning_changes WHERE seq > ?1 ORDER BY seq LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![after, fetch], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = rows.len() > limit;
    let mut changes = Vec::with_capacity(limit.min(rows.len()));
    for (seq, at_ms, entity, entity_id, item_id, action, updated_from, data, sources) in
        rows.into_iter().take(limit)
    {
        changes.push(WorkChange {
            seq,
            at_ms,
            entity,
            entity_id,
            item_id,
            action,
            updated_from: json(updated_from)?,
            data: json(data)?,
            sources: sources
                .map(|text| serde_json::from_str(&text))
                .transpose()?
                .unwrap_or_default(),
        });
    }
    let next_cursor = changes.last().map_or(after, |change| change.seq);
    Ok(Ok(WorkChangePage {
        changes,
        next_cursor,
        has_more,
    }))
}

/// A meeting whose commitments were just taken in: the first time as
/// created, after that as updated. The caller leaves out a meeting kept on
/// this Mac.
pub(crate) fn meeting_taken_in(conn: &Connection, meeting_id: &str, now_ms: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO planning_changes (at_ms, entity, entity_id, action, sources)
         SELECT ?2, 'meeting', ?1,
             CASE WHEN EXISTS (SELECT 1 FROM planning_changes
                 WHERE entity = 'meeting' AND entity_id = ?1) THEN 'update' ELSE 'create' END,
             json_array(json_array('meeting', ?1))",
        params![meeting_id, now_ms],
    )?;
    Ok(())
}

pub(crate) fn page_read(
    app: &AppHandle,
    since: Option<i64>,
    limit: usize,
) -> Result<std::result::Result<WorkChangePage, CursorRefusal>, String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
        page(conn, since, limit)
    })
}

/// The cursor that leaves the newest `limit` changes to read, or none when
/// every kept change fits.
fn newest_start(conn: &Connection, limit: usize) -> Result<Option<i64>> {
    let head = head(conn)?;
    let oldest: i64 = conn.query_row(
        "SELECT coalesce(min(seq), ?1 + 1) FROM planning_changes",
        [head],
        |row| row.get(0),
    )?;
    let start = head - i64::try_from(limit).unwrap_or(i64::MAX);
    Ok((start >= oldest - 1).then_some(start))
}

/// A page of changes for the app: after `since`, or with `latest` and no
/// cursor the newest ones, or else from the oldest kept.
pub fn work_changes(
    app: AppHandle,
    since: Option<i64>,
    limit: Option<usize>,
    latest: bool,
) -> Result<WorkChangePage, String> {
    let limit = limit.unwrap_or(PAGE).clamp(1, MAX_PAGE);
    let since = match since {
        None if latest => crate::planning::history(&app.state::<Arc<HistoryManager>>(), |conn| {
            newest_start(conn, limit)
        })?,
        since => since,
    };
    match page_read(&app, since, limit)? {
        Ok(page) => Ok(page),
        Err(CursorRefusal::Expired { oldest }) => Err(format!(
            "That cursor has expired: changes are kept 30 days and the oldest kept is {oldest}. Read again without a cursor."
        )),
        Err(CursorRefusal::Ahead { head }) => Err(format!(
            "That cursor is past the newest change, {head}."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::schema;
    use crate::planning::work::items::{self, NewItem};
    use crate::planning::work::types::{WorkActor, WorkOwner, WorkStatus};

    fn filed(conn: &Connection, title: &str) -> String {
        items::insert(
            conn,
            &NewItem {
                title,
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Me,
            0,
        )
        .expect("a filed task")
        .id
    }

    fn seqs(page: &WorkChangePage) -> Vec<i64> {
        page.changes.iter().map(|change| change.seq).collect()
    }

    #[test]
    fn pages_meet_and_a_pruned_cursor_is_refused_as_expired() {
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        for title in ["Book the venue", "Order the banners", "Print the menus"] {
            filed(&conn, title);
        }

        let first = page(&conn, None, 2).expect("a read").expect("a page");
        let second = page(&conn, Some(first.next_cursor), 2)
            .expect("a read")
            .expect("a page");
        conn.execute("DELETE FROM planning_changes WHERE seq <= 2", [])
            .expect("pruned rows");
        let stale = page(&conn, Some(1), 2).expect("a read");

        assert_eq!((seqs(&first), first.has_more), (vec![1, 2], true));
        assert_eq!((seqs(&second), second.has_more), (vec![3], false));
        assert!(
            matches!(stale, Err(CursorRefusal::Expired { oldest: 3 })),
            "a cursor behind the oldest kept change was served"
        );
    }

    #[test]
    fn handing_a_project_lead_to_the_user_is_a_change() {
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let project = crate::planning::catalog::save_project(
            &conn,
            None,
            &crate::planning::catalog::ProjectEdit {
                title: Some("Harbor festival".to_owned()),
                ..Default::default()
            },
            1,
        )
        .expect("a project");
        let before = page(&conn, None, 50).expect("a read").expect("a page");

        crate::planning::catalog::save_project(
            &conn,
            Some(&project.id),
            &crate::planning::catalog::ProjectEdit {
                lead: Some(WorkOwner::Myself),
                ..Default::default()
            },
            2,
        )
        .expect("the lead handed over");
        let after = page(&conn, Some(before.next_cursor), 50)
            .expect("a read")
            .expect("a page");

        let leads: Vec<_> = after
            .changes
            .iter()
            .filter(|change| change.entity == "project")
            .map(|change| change.updated_from.clone())
            .collect();
        assert_eq!(leads, [Some(serde_json::json!({ "lead": null }))]);
    }

    #[test]
    fn milestones_views_labels_and_subject_comments_are_changes() {
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let project = crate::planning::catalog::save_project(
            &conn,
            None,
            &crate::planning::catalog::ProjectEdit {
                title: Some("Harbor festival".to_owned()),
                ..Default::default()
            },
            1,
        )
        .expect("a project");
        let before = page(&conn, None, 50).expect("a read").expect("a page");

        conn.execute_batch(&format!(
            "INSERT INTO planning_milestones
                 (id, project_id, title, status, revision, created_at_ms, updated_at_ms)
                 VALUES ('m1', '{project}', 'Venue booked', 'not_started', 1, 2, 2);
             UPDATE planning_milestones SET status = 'done' WHERE id = 'm1';
             INSERT INTO planning_labels (id, name, hue, revision, created_at_ms, updated_at_ms)
                 VALUES ('l1', 'Urgent', 10, 1, 2, 2);
             INSERT INTO planning_views
                 (id, name, query_json, display_json, revision, created_at_ms, updated_at_ms)
                 VALUES ('v1', 'Mine', '{{}}', '{{}}', 1, 2, 2);
             INSERT INTO planning_comments
                 (id, subject_kind, subject_id, author_kind, author_name, body, created_at_ms)
                 VALUES ('c1', 'meeting', 'meeting-1', 'me', 'Me', 'Ask about rain', 3);",
            project = project.id
        ))
        .expect("planner rows");
        let after = page(&conn, Some(before.next_cursor), 50)
            .expect("a read")
            .expect("a page");

        let seen: Vec<_> = after
            .changes
            .iter()
            .map(|change| {
                (
                    change.entity.as_str(),
                    change.action.as_str(),
                    change.updated_from.clone(),
                    change.sources.clone(),
                )
            })
            .collect();
        let meeting = vec![("meeting".to_owned(), "meeting-1".to_owned())];
        assert_eq!(
            seen,
            [
                ("milestone", "create", None, vec![]),
                (
                    "milestone",
                    "update",
                    Some(serde_json::json!({ "status": "not_started" })),
                    vec![]
                ),
                ("label", "create", None, vec![]),
                ("view", "create", None, vec![]),
                ("comment", "create", None, meeting),
            ]
        );
    }

    #[test]
    fn a_project_summary_and_status_name_are_changes() {
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let project = crate::planning::catalog::save_project(
            &conn,
            None,
            &crate::planning::catalog::ProjectEdit {
                title: Some("Harbor festival".to_owned()),
                ..Default::default()
            },
            1,
        )
        .expect("a project");
        conn.execute_batch(&format!(
            "INSERT INTO planning_project_status_names
                 (id, category, name, position, created_at_ms, updated_at_ms)
                 VALUES ('s1', 'active', 'Pilot', 0, 2, 2);
             UPDATE planning_projects SET status_name_id = 's1' WHERE id = '{id}';",
            id = project.id
        ))
        .expect("a status name");
        let before = page(&conn, None, 50).expect("a read").expect("a page");

        conn.execute_batch(&format!(
            "UPDATE planning_projects SET summary = 'Rain plan agreed' WHERE id = '{id}';
             UPDATE planning_projects SET status_name_id = NULL WHERE id = '{id}';",
            id = project.id
        ))
        .expect("project edits");
        let after = page(&conn, Some(before.next_cursor), 50)
            .expect("a read")
            .expect("a page");

        let seen: Vec<_> = after
            .changes
            .iter()
            .map(|change| (change.entity.as_str(), change.updated_from.clone()))
            .collect();
        assert_eq!(
            seen,
            [
                ("project", Some(serde_json::json!({ "summary": null }))),
                ("project", Some(serde_json::json!({ "status_name": "Pilot" }))),
            ]
        );
    }

    #[test]
    fn initiatives_are_changes_and_other_tables_still_write_after_the_rebuild() {
        let conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let project = crate::planning::catalog::save_project(
            &conn,
            None,
            &crate::planning::catalog::ProjectEdit {
                title: Some("Harbor festival".to_owned()),
                ..Default::default()
            },
            1,
        )
        .expect("a project");
        let before = page(&conn, None, 50).expect("a read").expect("a page");

        conn.execute_batch(&format!(
            "INSERT INTO planning_initiatives (id, name, created_at_ms, updated_at_ms)
                 VALUES ('i1', 'Waterfront', 2, 2);
             UPDATE planning_initiatives SET status = 'active', owner_is_self = 1 WHERE id = 'i1';
             INSERT INTO planning_initiative_projects (project_id, initiative_id, added_at_ms)
                 VALUES ('{id}', 'i1', 3);
             UPDATE planning_projects SET title = 'Harbor fair' WHERE id = '{id}';
             DELETE FROM planning_initiative_projects WHERE project_id = '{id}';
             DELETE FROM planning_initiatives WHERE id = 'i1';",
            id = project.id
        ))
        .expect("initiative edits");
        let after = page(&conn, Some(before.next_cursor), 50)
            .expect("a read")
            .expect("a page");

        let seen: Vec<_> = after
            .changes
            .iter()
            .map(|change| {
                (
                    change.seq - before.next_cursor,
                    change.entity.as_str(),
                    change.action.as_str(),
                    change.updated_from.clone(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                (1, "initiative", "create", None),
                (
                    2,
                    "initiative",
                    "update",
                    Some(serde_json::json!({ "owner": null, "status": "planned" }))
                ),
                (3, "project", "update", Some(serde_json::json!({ "initiative": null }))),
                (4, "project", "update", Some(serde_json::json!({ "title": "Harbor festival" }))),
                (5, "project", "update", Some(serde_json::json!({ "initiative": "i1" }))),
                (6, "initiative", "remove", None),
            ]
        );
    }
}
