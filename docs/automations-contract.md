# Automations: saved agent workflows

Must-have 5 of the Agents/MCP report (`sona-linear-spec/LinearAgentsMcpVerify-report.md`,
line 160): schedule, task-state and meeting-ended triggers, conditions, Run now, run
history, versions and per-automation tool allowlists.

An automation is a saved prompt for Sona's own in-app agent (the agent behind the Ask
panel). When its trigger fires, the running app asks that agent to carry out the
prompt, with only the lookups on the automation's allowlist. Each firing is a run with
a status. Every edit is kept as a version, and restoring an older version saves it
again as a new one.

Users and agents see the name "Automations": the sidebar entry, the screen, the CLI
verbs `automation-*`, the MCP tools `sona_automation_*` and the JSON keys `automation`,
`automations` and `automationId`. The code keeps the name it was built under, "loop":
the module `planning::work::loops`, the tables `planning_loop*`, the Rust and Swift
types `WorkLoop*`, and the bridge commands `work_loop*`. Meeting commitments are also
called loops (`MeetingLoopId`, `work loop-reopen`, `work loop-owner`,
`sona_loop_resolve`, `sona_meeting_loop_*`); they are a separate thing. Meeting
series automations in Settings (`meeting::automations`) are a separate thing too.

## Core

- `src-tauri/src/planning/work/loops.rs`
  - Migration `LOOPS_MIGRATION` (last in `MIGRATIONS`): tables `planning_loops`,
    `planning_loop_versions` and `planning_loop_runs`. It rebuilds no task, project,
    comment, label or delegate table, so the change-stream triggers are untouched.
  - An automation has a name (1 to 64 characters, unique ignoring case), a prompt (1
    to 4,000 characters), an enabled flag, a trigger, conditions, a tool allowlist and
    a version number. Each save that changes something adds a version row with who
    saved it ("you" from the app, the agent's name from the CLI or MCP).
  - Triggers (`kind`):
    - `schedule`: `every` hourly, daily, weekly (on chosen days) or monthly, an IANA
      `timezone` and `firstRunAtMs`. The first run sets the time of day, and the day
      of the month for monthly (clamped to the month's last day). It can't be in the
      past when the schedule is created or changed (60 seconds of grace). Times are
      worked out in the automation's time zone, so a daily 9:00 stays 9:00 across
      daylight saving.
    - `task_created`, `status_changed` (optionally `to` one status), `label_added`
      (optionally one `labelId`), `assigned` (a task gets an owner or a delegate),
      `comment_added`, `project_status_changed` (optionally `to` one project status),
      `initiative_status_changed` (optionally `to` one initiative status),
      `milestone_done` and `project_update_posted`. These read the change stream
      (`planning_changes`).
    - `meeting_ended`: once per meeting, when its notes are ready.
    - `cycle_started` and `cycle_ended`: once per cycle, on its first or last day, for
      cycles whose day comes after the automation was turned on.
  - Conditions: any-of lists of labels, projects and task statuses, at most 8 entries
    in all, checked against the task as it is when the trigger is read. Task triggers
    take all three; project triggers (`project_status_changed`, `milestone_done`,
    `project_update_posted`) take only projects; the rest take none.
  - Event triggers keep their own cursor into the change stream. Turning an
    automation on or changing its trigger moves the cursor to the newest change, so
    nothing older fires. Each queued run carries a cause key (`schedule:<due>`,
    `change:<seq>`, `meeting:<id>`, `cycle-started:<number>`,
    `cycle-ended:<number>`, `manual:<operation id>`), unique per automation, so the
    same change or the same due time never queues a second run.
  - An automation may set how hard the agent thinks on a run (`reasoning`: low,
    medium or high; left out, the model decides). It is kept with each version.
  - A new value is read from the change stream itself: the old value kept by the next
    change to the same field, or the row as it is now when none followed. A task moved
    to Done and back to Todo before the trigger was read still fires a "moved to Done"
    run for a `to: done` automation.
  - Runs: `queued`, `running`, `done`, `failed` or `skipped`, with the cause in plain
    words ("Run now", "Scheduled", "S-12 moved to Done", "Meeting ended: Weekly sync"),
    start and end times, the agent's answer or the error, and the tools it was
    refused.
  - Every write goes through `store::mutate`, so a repeated operation id replays its
    receipt instead of writing twice.
- `src-tauri/src/agent_panel/mod.rs`
  - `run_loop_turn` runs one agent turn without the panel window, beside
    `run_chat_turn` (both share `settle_turn`, which submits, polls and validates a
    relay job). It gets up to three lookup rounds, like the panel.
  - The allowlist is enforced where lookups are dispatched (`loop_lookups`): a lookup
    the automation doesn't allow is answered "<tool> is not allowed in this
    automation" and recorded on the run as refused. The relay's turn has only a yes/no
    for tools, so the agent still sees all of them; it is refused when it asks.
  - Allowlist names are MCP tool names. The read-only ones map to the in-app
    lookups: `sona_search` (search, dictation), `sona_meetings` (recent),
    `sona_meeting`, `sona_transcript`, `sona_people` (person), `sona_action_items`
    (loops), `sona_upcoming`, `sona_stats` (word stats, activity) and `sona_skills`
    (the user's saved skills). Five more let the run propose changes (see below):
    `sona_work_create`, `sona_work_update`, `sona_scratchpad_update_body`,
    `sona_project_save` and `sona_project_update_save`. Other names are refused when
    the automation is saved.
- The scheduler (`loops::start`, from the app's setup in `lib.rs`) lives in the app
  process only. Every 10 seconds, or at once after a save, Run now or a meeting:
  1. it queues due schedules and matching changes in one transaction;
  2. while Settings > Agents > External access (conversations open to agents) or
     External mutations (agents' changes) is off, it marks every queued run skipped
     with that reason;
  3. otherwise it runs queued runs one at a time, oldest first, and records each
     result.

  A run caused by a task opens an agent session on that task named "Sona
  automation" (without making it the delegate) and ends it with the answer or the
  error, so the run shows on the task. When another agent is the task's delegate, the
  session store refuses a second agent, so that run keeps the task but has no
  session. Runs left `running` when the app stopped are marked failed at the next
  start.
- `meeting_ended` is queued from `planning::work::take_in`, which the meeting
  pipeline calls after a meeting's notes land. Meetings that started before the
  automation was armed don't fire. A run about a meeting whose series is kept on this
  Mac is skipped ("this meeting's series is kept on this Mac, so it isn't sent to the
  in-app agent").

## What the agent can do

The in-app agent never changes anything itself. An automation's output is its
answer, kept on the run and, for task runs, in the task's session.

With a proposal tool on its list, the run may end its answer with a
`sona-proposals` block. Sona files what it proposes and lists it under the answer;
nothing is applied until the user accepts it:

- `sona_work_create`: new tasks, which wait in Triage.
- `sona_work_update`: changes to existing tasks (title, details, status,
  priority), which wait in Triage field by field.
- `sona_scratchpad_update_body`: text added to the end of a note.
- `sona_project_save`: a project's new description.
- `sona_project_update_save`: a drafted project update.

The last three wait as document proposals (`sona_doc_proposal_list`; accepted or
declined in Triage). The prompt lists up to 30 projects and 15 notes it may name.
Proposals of a kind the list doesn't allow are refused and recorded on the run. More
than 10 of one kind keeps none, and none are kept when the agents' switches closed
during the run.

## Bridge (Swift `core.request`, params camelCase)

- `work_loops {}` → `{ loops: [WorkLoop], tools: [{ name, title }] }`
- `work_loop { loopId }` → `{ automation, versions (newest first), runs (newest first, at most 50) }`
- `work_loop_save { operationId, loopId|null, input: { name, prompt, trigger, conditions, tools, reasoning } }` → WorkLoop
- `work_loop_run { operationId, loopId }` → WorkLoopRun (queued)
- `work_loop_enable { operationId, loopId, enabled }` → WorkLoop
- `work_loop_restore_version { operationId, loopId, version }` → WorkLoop
- `work_loop_delete { operationId, loopId }` → the deleted WorkLoop; its versions and runs go with it

WorkLoop: `{ id, name, prompt, enabled, trigger, conditions: { labels, projects,
statuses }, tools, reasoning, version, nextRunAtMs, lastRun, createdAtMs,
updatedAtMs }`.
`nextRunAtMs` is null for event triggers and for automations that are off.
WorkLoopRun: `{ id, automationId, version, cause, taskId, meetingId, status,
queuedAtMs, startedAtMs, endedAtMs, output, error, refusedTools, sessionId }`. Errors
are plain sentences, such as "The first run can't be in the past." Every change, and
every run that starts or ends, emits `work-changed`.

## CLI (`sona work`)

- `automation-list` and `automation-show AUTOMATION` read. AUTOMATION is a name or an
  id.
- `automation-save NAME [--automation AUTOMATION] [--prompt TEXT]` with one trigger:
  - `--schedule hourly|daily|weekly|monthly --first-run "YYYY-MM-DD HH:MM" [--timezone ZONE] [--days mon,wed]`
  - `--on task-created|status-changed|label-added|assigned|comment-added|project-status-changed|initiative-status|milestone-done|project-update|meeting-ended|cycle-started|cycle-ended [--to STATUS] [--label LABEL]`
  - and `[--if-label ...] [--if-project ...] [--if-status ...] [--tools ...] [--reasoning default|low|medium|high]`, where "" clears a list.
  A new automation needs `--prompt` and a trigger; changing one keeps what isn't
  given.
- `automation-run AUTOMATION` queues a run for the running app.
- `automation-enable AUTOMATION`, `automation-disable AUTOMATION`,
  `automation-restore-version AUTOMATION VERSION`, `automation-delete AUTOMATION`.
- Writes take `--dry-run`, `--agent NAME` and `--operation-id UUID`, and need External
  mutations and agents' changes set to apply, like every other `work` write. A dry run
  makes the same change in a transaction that is rolled back.
- JSON answers carry `schema_version` and: `automations` and `tools` for
  `automation-list`; `automation`, `versions` and `runs` for `automation-show`;
  `action` (`saved`, `unchanged`, `enabled`, `disabled`, `restored`, `deleted`),
  `dry_run`, `operation_id` and `automation` for the other writes; `dry_run`,
  `operation_id` and `run` for `automation-run`. The automation and run objects use
  the bridge's camelCase keys.

## MCP

`sona_automation_list`, `sona_automation_show`, `sona_automation_save`,
`sona_automation_enable`, `sona_automation_disable`,
`sona_automation_restore_version`; `sona_automation_run` and `sona_automation_delete`
are opt-in. All of them call the CLI verbs above.

## App

Sidebar > Automations (`macos/Sona/LoopsScreen.swift`, `LoopsStore.swift`,
`WorkLoopsTypes.swift`): a list with each automation's trigger, next run and last
result; an editor for name, prompt, trigger, conditions, tools and how hard the agent
thinks; Run now; run history; version history with Restore; Delete. Unknown trigger
kinds and statuses decode to a fallback.

## Not built

- Slack, pull request and webhook triggers (not Sona's).
- Automations that change things directly. A run only proposes; the user accepts.
- Runs while Sona is closed. A schedule that came due while the app was closed runs
  once at the next start, not once per missed time.

## Verified

In a scratch app with invented data and an unpaired in-app agent, so no run
reached a model (2026-11-11), under the earlier `loop-*` verb names:

- Saving a daily 09:00 automation answered `nextRunAtMs` for the next morning, and
  the list and the screen showed it.
- Run now queued a run; the app picked it up within one tick and recorded it failed
  with "The in-app agent isn't connected. Pair it in Settings > Agents."
- With a `status_changed` `to: done` automation, `work update S-2 --status done` gave
  exactly one run ("S-2 moved to Done"), still one 12 seconds later; S-4 and S-5 gave
  one each, and S-5 (no delegate) got a session from the automation.
- Saving with `--tools sona_work_update` was refused, naming the tools it can use.
  (That tool was allowed later, for proposals.)
- A second save made version 2; restoring version 1 made version 3, restored from 1,
  with version 1's prompt.

Rechecked after the rename, in the same scratch app: `automation-list --json` answers
`automations` and `tools`; `automation-show --json` answers `automation`, `versions`
and `runs`, each run with `automationId`; `loop-list` is no longer a verb;
`automation-disable` turned both automations off; the sidebar, list and detail read
"Automations".

Tests: `a_loop_refuses_a_lookup_outside_its_allowlist` (agent panel) and
`a_task_moved_to_done_queues_one_run_however_often_the_stream_is_read` (loops).

Not run live: `meeting_ended`, the skip while a gate is off, a schedule coming due,
a run with a paired agent, and the MCP tools against a live core.
