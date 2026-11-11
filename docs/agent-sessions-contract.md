# Agent sessions, one-call task save, and the change stream

Rows closed from the Agents/MCP report (`sona-linear-spec/LinearAgentsMcpVerify-report.md`):
must-haves 1 (task save with `patch[]`), 3 (sessions, typed activities, stop,
liveness), 4 (delegate filter), 9 (plan checklist and select questions) and
10 (change stream). Loops (must-have 5) are not built here.

## Core

- `src-tauri/src/planning/work/sessions.rs`
  - Migration `SESSIONS_MIGRATION`: tables `planning_agent_sessions` and
    `planning_agent_activities`, and a trigger that deletes a task's sessions with it.
  - A session belongs to one task and one agent. Activities are `thought`,
    `action`, `elicitation`, `response` and `error`, written by the agent, plus
    `prompt` and `stop`, written by whoever watches it.
  - State is read, never stored:
    - `complete` or `error` once a response or an error ends it;
    - `awaiting_input` while an elicitation has no later prompt or agent activity;
    - `pending` until the agent's first activity or heartbeat, with
      `unresponsive: true` after 10 seconds;
    - `stale` after 30 minutes with no activity or heartbeat from the agent;
    - `active` otherwise.
  - Rules the store enforces:
    - only the session's agent writes activities, plans and heartbeats;
    - an ended session takes no more writes;
    - after a stop request only a response or an error is accepted;
    - an ephemeral thought or action is replaced by the next activity;
    - a plan replaces the whole list (at most 50 steps; statuses
      `pending`, `in_progress`, `completed`, `canceled`);
    - choices belong only to an elicitation (at most 20, distinct values);
    - a prompt's `value` must be one of the standing question's values;
    - an agent cannot answer its own session.
  - Starting a session makes the agent the task's delegate when the task has
    none and agents' changes apply. A task delegated to another agent is refused.
    Status and owner are never changed.
  - Every write goes through `store::mutate`, so a repeated operation id
    replays from its receipt in one transaction.
- `src-tauri/src/planning/work/changes.rs`
  - Migration `CHANGES_MIGRATION`: table `planning_changes(seq, at_ms, entity,
    entity_id, task_id, action, updated_from, data)`, filled by SQLite triggers
    on tasks, task labels, task delegates, projects, comments, sessions and
    session activities.
  - `updated_from` holds the old value of each field that changed. Revision,
    sort order and timestamps alone write nothing. Heartbeats write nothing.
  - `PROJECT_LEAD_CHANGES_MIGRATION` (after `SELF_LEAD_MIGRATION`) rebuilds the
    project trigger so a lead change reads `{"lead": <old>}`, with `"me"` when
    the user led it and null for no lead; `lead_is_self` alone is a change.
  - Rows older than 30 days are pruned on insert. A cursor older than the
    oldest kept row is refused as expired, and one past the newest is refused.
  - Known risk: a later migration that rebuilds `planning_tasks`,
    `planning_projects`, `planning_task_comments`, `planning_task_labels` or
    `planning_task_doers` drops these triggers and has to create them again.
- `src-tauri/src/planning/work/items.rs`: a task's labels are now written as
  a difference (remove the dropped, add the new), so the change stream records
  only real label changes.

## CLI (`sona work …`, also `sona-core work …`)

Every write takes `--json`, `--agent`, `--operation-id` and `--dry-run`; a dry
run runs the write in a transaction that is rolled back.

| Verb | Does |
| --- | --- |
| `save TASK --patch-json JSON [--expected-revision N]` | ordered steps, all or none; the write is `update` with the folded result, so Apply/Suggest and the revision fence hold |
| `changes [--since CURSOR] [--limit N]` | ordered changes with `updated_from`, `next_cursor`, `has_more` |
| `list --delegate AGENT\|any\|none` | tasks by delegate |
| `session-start TASK` | new session; fresh operation id unless one is given |
| `session-activity SESSION --type T [--body] [--action --parameter --result] [--option C …\|--options-json] [--ephemeral] [--plan-json]` | one activity |
| `session-plan SESSION --plan-json JSON` | replace the plan |
| `session-heartbeat SESSION` | liveness; answer carries `stop_requested`, `unread_prompts`, `instruction` |
| `session-stop SESSION` | record a stop request |
| `session-reply SESSION [--body] [--value]` | answer the agent; refused while agents' changes wait for the user |
| `session SESSION` | one session with every activity |
| `sessions [TASK] [--open] [--agent NAME]` | sessions newest first |

`save` patch steps: `{field, op?, value?, anchor?}`. Fields: `title`,
`description`, `status`, `priority`, `owner`, `due`, `project`, `milestone`,
`labels` (`op` set/add/remove), `parent`, `cycle`, `repeat`, `snooze`.
`description` takes `op` replace, append, prepend, insert_before, insert_after,
replace_text; the last three need `anchor` and refuse when it is missing. A
null value clears. A bad step refuses the whole save as `patch[n]: …`. `save`
changes an existing task; it does not create one (use `create`).

Sessions and changes on tasks kept on this Mac (the meeting fence) are left out.

## MCP (`tools/sona-mcp`)

`sona_task_save`, `sona_session_start`, `sona_session_activity`,
`sona_session_plan`, `sona_session_heartbeat`, `sona_session_get`,
`sona_session_list`, `sona_work_changes`, and `sona_session_stop` (opt-in).
`sona_work_list` takes `delegate`. List arguments of objects (patch steps, plan
steps, choices) travel as one JSON argument.

## Bridge commands for the UI

Implemented in `native_bridge/dispatch.rs`; the shapes are camelCase.

- `work_sessions {itemId?: string, open?: bool}` → `[WorkSession]`
- `work_session {id}` → `WorkSession`
- `work_session_stop {operationId, id}` → `WorkSession` (the user asks the agent to stop)
- `work_session_reply {operationId, id, body?: string, value?: string}` → `WorkSession`
- `work_changes {since?: number, limit?: number}` → `{changes: [WorkChange], nextCursor, hasMore}`

`WorkSession`:
`{id, itemId, agent, state: "pending"|"active"|"awaiting_input"|"stale"|"complete"|"error",
unresponsive, createdAtMs, lastActivityAtMs?, heartbeatAtMs?, stopRequestedAtMs?,
stopRequestedBy?, endedAtMs?, plan: [{content, status: "pending"|"in_progress"|"completed"|"canceled"}],
question?: WorkSessionActivity, unreadPrompts: [WorkSessionActivity], activities: [WorkSessionActivity]}`
(`activities` is empty in `work_sessions`).

`WorkSessionActivity`:
`{id, kind: "thought"|"action"|"response"|"error"|"elicitation"|"prompt"|"stop", atMs,
author: {kind: "me"|"sona"|"agent", name?}, body?, action?, parameter?, result?,
options: [{label, value}], value?, ephemeral}`

`WorkChange`: `{seq, atMs, entity: "task"|"project"|"comment"|"session"|"session_activity",
entityId, itemId?, action: "create"|"update"|"remove", updatedFrom?, data?}`

The bridge does not filter tasks kept on this Mac: it serves the user's own app.

## Verified

Scratch run on 2026-11-11: `/private/var/tmp/sona-scratch-agentsessions/smoke.sh`
(fresh HOME, `SONA_SCRATCH_RUN=1`, debug `target/debug/sona`, invented tasks),
output in `out.txt` beside it. Through the CLI and through MCP stdio
(`tools/sona-mcp/src/index.ts`, client "Claude Code"):

- CLI session on S-1 by Codex: start (pending), plan, ephemeral thought
  replaced by the next action, select question (`awaiting input`), another
  agent's write refused, a value that is not a choice refused
  (`fifty is not one of the choices: ten, twenty`), the answer `ten` shows in
  `unread_prompts` of the next heartbeat, stop request, a thought after it
  refused (`Reviewer asked you to stop. End the session with --type response or
  --type error.`), heartbeat shows `stop_requested: true` with the
  instruction, the response ends it as `complete`, a later write refused as
  ended.
- MCP session on S-2: `sona_session_start`, activity with a plan,
  elicitation with choices (`awaiting_input`), `sona_session_get`,
  `sona_session_stop`, heartbeat shows the stop, an `error` activity ends it as
  `error`; `sona_session_list` lists both sessions.
- `list --delegate Codex`, `"Claude Code"`, `none`, `any` each return the
  expected tasks.
- `save` applied four steps in order (status, label add, description append,
  insert_before). Two bad saves (missing anchor at `patch[2]`, unknown field at
  `patch[1]`) left the row byte-identical: `UNCHANGED: yes`. `sona_task_save`
  dry run showed the fields, the real call wrote them.
- `changes`: pages of 5 and of 7 cover all 25 rows exactly once
  (`paged 25 of 25`), `updated_from` holds the old status and description,
  and the old delegate when a session start made the agent the delegate,
  after deleting rows 1 to 10 `--since 3` is refused as expired (CLI and MCP,
  MCP code `invalid_request`), `--since 99999` is refused as past the newest.
- A repeated `session-start` with the same operation id answers
  `replayed: true` with the same session; one session row was written.

Tests: `planning::work::sessions::tests::a_stop_request_leaves_the_agent_only_an_ending_activity`,
`planning::work::changes::tests::pages_meet_and_a_pruned_cursor_is_refused_as_expired`
and `planning::work::changes::tests::handing_a_project_lead_to_the_user_is_a_change`.
The lead case is proven by the test only: in a scratch home `project-save --lead me`
answers `unavailable: Conversations and the people in them can't be opened right now.`
MCP: `bun test`, `npx tsc --noEmit`, `npx oxlint` in `tools/sona-mcp`.

Not verified: the bridge commands were compiled but not called from the app;
the 10-second unresponsive and 30-minute stale states were not waited out
live; `save` on milestone, cycle, repeat and snooze fields was not run.
