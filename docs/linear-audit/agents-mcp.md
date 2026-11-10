# Linear agents and MCP audit against HEAD

Spec: `sona-linear-spec/LinearAgentsMcpVerify-report.md` (sections 2 and 3, must-have list).
Checked at HEAD `6f89dc7b` on `work/people-projects`, read-only. Uncommitted work in the tree
(the comments slice another agent is building) is not counted.

Three places:
- UI: `macos/Sona/*.swift`
- CLI: `sona-core work` (`src-tauri/src/query/external/tracker.rs`, `tracker/catalog.rs`, `src-tauri/src/planning/work/*`)
- MCP: `tools/sona-mcp/src/{tools.ts,index.ts}`

Verb list the CLI has today (`tracker.rs:81-170`): list, ready, mine, counts, cycles, cycle, show,
create, update, claim, done, accept, decline, duplicate, archive, restore, snooze, relate, unrelate,
labels, label-save, label-delete, views, view-save, view-delete, export, follow-up, ask, say,
watches, watch, unwatch, projects, project-save, project-archive, milestones, milestone-save,
milestone-delete, people, person-save, person-merge, meetings, meeting.

MCP work tools today (`tools.ts:822-1453`): sona_work_list, _get, _follow_up, _counts, _create,
_update, _claim, _done, _triage, _archive, _restore, _relate, _unrelate, _cycles, _labels,
_label_save, _label_delete, _views, _view_save, _view_delete, _ask, _take, _say, _watches, _watch.
Read-only project tools: sona_projects, sona_project (`tools.ts:638-662`).

## Must-have list (report order)

| # | Feature | UI | CLI | MCP | What is missing |
|---|---|---|---|---|---|
| 1 | Task get/list/save in Linear's `save_*` shape (id means update, `patch[]`, add/remove arrays, null clears, names accepted) | built: task detail opens on click, `TaskDetailView.swift:6`, activity `:130`, delegate row `:783-786` | partial: `show`/`list`/`create`/`update` `tracker.rs:83-99`; add/remove labels, `none` clears, names for owner/project, `--expected-revision`, `--dry-run` | partial: `sona_work_get` `tools.ts:899`, `sona_work_list` `:822`, `sona_work_create` `:949`, `sona_work_update` `:974` with `add_labels`/`remove_labels` `:984-991`, `none` clears `FIELDS` `:209-253` | create and update are two tools, not one `save` keyed on `id`; no `patch[]` anchored edits to details; no `estimate`, `parentId`, `links[]`, `duplicateOf` on save (duplicate only via triage `tools.ts:1138`), no `meeting` source on create; relations are separate verbs (`relate`/`unrelate`) not add/remove arrays; list has no `delegate`, `createdAt`/`updatedAt`/`completedAt`, `fields[]`, `orderBy` or `cursor` (only `limit` + `has_more`). UI has no estimate or parent field (`WorkTypes.swift` has neither). |
| 2 | Comments on tasks, projects, milestones, meetings, notes, with threads | missing (no `Comment` type in `macos/Sona` at HEAD) | missing (no comment verb in `WorkVerb`, `tracker.rs:81-170`) | missing (no comment tool in `tools.ts`) | everything: store, threads via `parentId`, agent-authored comments, list/save/delete in all three places. Being built in an uncommitted slice right now. |
| 3 | Agent session and typed activities (thought/action/elicitation/response/error), `stop`, 10 s / 30 min liveness, shown in task detail | partial: delegate row says "waiting for you" from the bridge `TaskDetailView.swift:784-785`, `AgentBridgeStore.swift:62`; bridge console lists hook sessions bound to a task `AgentBridgeTypes.swift:77-92` and their tool events `:94-117,128-163`, replies `AgentBridgeView.swift:299` | partial: claim starts the run `tracker.rs:2110-2171`; bridge session auto-claims its task `planning/work/agents.rs:761-786`; history marks agent entries `WorkActor` `WorkTypes.swift:507` | missing: no activity tool; only claim/done `tools.ts:1031,1051` | no session object per task run with states (pending/active/error/awaitingInput/complete/stale); no typed activity log an agent can emit (`sona_work_activity`); hook events are not shown inside task detail; no user `stop` to a running agent (the bridge only observes the agent's own stop, `agent_bridge.rs:768`); no 10 s unresponsive / 30 min stale rule (only `lastSeenAtMs`, `AgentBridgeTypes.swift:83`). |
| 4 | Delegate kept separate from assignee | built: picker "Delegate to…" `WorkPickers.swift:94-96`, `WorkStore.swift:350-356`, shortcut `WorkKeyboard.swift:72`, detail row `TaskDetailView.swift:783` | built: `doer` beside owner `tracker.rs:1136-1137`, claim keeps owner `tracker.rs:100`, refuses another agent's task `:2116-2127` | partial: `sona_work_claim` sets it, owner unchanged `tools.ts:1031-1048` | no explicit delegate/undelegate verb or MCP field (only self-claim); no `delegate` filter in list (CLI, MCP or `TaskFilters.swift`). |
| 5 | Loops: schedule, task-state and meeting-ended triggers, conditions, Run now, run history, versions, per-loop tool allowlists | partial: fixed built-in workflows with on/off `WorkflowsView.swift:107-133`, run history and trend `WorkflowsStore.swift:123,360-408` | missing (no loop verb; `--events` reads receipts only) | partial: `sona_events` reads workflow results `tools.ts:780-800` | user-defined loops of any kind: triggers, conditions, instructions, Run now, versions, tool allowlist. (`sona_loop_resolve` `tools.ts:802` is a meeting commitment, not a Linear loop.) |
| 6 | Triage suggestions (project, label, person, duplicate) with accept/dismiss and a reason | built: `TriageScreen.swift:99,186-204`, chip with "why" `:292-310` | built: `accept`/`decline`/`duplicate` `tracker.rs:105-109`; suggestions with reason in `show` | built: `sona_work_triage` `tools.ts:1082-1151`; suggestions with reason and quote in `sona_work_get` `:902` | label suggestions: the field set is owner, due, project, duplicate only (`WorkTypes.swift:539`, `planning/work/types.rs:487-492`). Per-suggestion dismiss (vs whole-item decline) not seen. |
| 7a | Projects (save with state, lead, dates, labels, links) | built: `ProjectsView.swift`, detail `:457-467` | built: `projects`, `project-save`, `project-archive` `tracker.rs:150-154`, `catalog.rs:529` | partial: read only, `sona_projects`/`sona_project` `tools.ts:638-662` | MCP project save/archive. |
| 7b | Milestones | built: project timeline `ProjectsView.swift:621-627`, planner drafts `PlannerAgentStore.swift:34,213` | built: `milestones`, `milestone-save`, `milestone-delete` `tracker.rs:156-160`, `catalog.rs:691,727` | partial: read inside `sona_project` `tools.ts:650`; task `milestone` field `:244` | MCP milestone list/save/delete. |
| 7c | Project updates with health (onTrack/atRisk/offTrack) | missing (no health in `macos/Sona` work code) | missing | missing | the whole object: list/save/delete updates with body and health. |
| 7d | Labels with groups, retire/restore | partial: create/edit `WorkStore.swift:360-366` | partial: `labels`, `label-save`, `label-delete` `tracker.rs:121-125` | partial: `tools.ts:1180-1246` (opt-in) | groups (`isGroup`, `parent`, single/multi select), retire/restore instead of delete, label scope task/project. |
| 7e | Cycles with a write tool | built: cycle screen and settings `CycleView.swift:57,201`, `WorkStore.swift:182-188` | partial: read `cycles`/`cycle` `tracker.rs:91-93`; plan tasks via `update --cycle` | partial: `sona_work_cycles` read `tools.ts:1157`, `cycle` on update `:997` | CLI and MCP cannot change cycle length or start day (UI-only `work_cycle_settings_save`). |
| 7f | Views | built: `WorkStore.swift:396-417` | built: `views`, `view-save`, `view-delete` `tracker.rs:127-131`; `list --view` | built: `tools.ts:1248-1342`, `view` on list `:875` | nothing. |
| 8 | MCP server `instructions` and read-only/destructive annotations on every tool | n/a | n/a | partial: instructions built `index.ts:23-24,59`; `readOnlyHint` on every tool `index.ts:68` | `destructiveHint` (and `idempotentHint` for the operation_id-replayed writes); instructions cover tasks only, not projects/notes/meetings. |
| 9 | Agent plan checklist and `select` elicitations | missing (bridge allow/deny on permission requests is the nearest, `AgentBridgeTypes.swift:151-153`) | missing | missing | `plan[]` on a run and a choice question with options the user answers in the app. |
| 10 | Internal change-event stream (`action`, `updatedFrom`) for loops and history | partial: per-task activity list `TaskDetailView.swift:673-714`; proposals keep from/to `WorkTypes.swift:334-340` | partial: `--events` receipts; `update --dry-run` returns from/to | partial: `sona_events` `tools.ts:780-800` | one subscribable stream of entity changes with old values that loops can trigger on; receipts and task history are separate today. |

## Section 2 rows not in the must-have list

| Feature (relevant to Sona) | UI | CLI | MCP | What is missing |
|---|---|---|---|---|
| Local MCP server copying Linear's tool shape | n/a | n/a | built: stdio server `index.ts:55-70`, opt-in tool sets `tools.ts:1459-1473` | tool names still `sona_work_*`; the `sona_task_*` / `save` shape from section 3 not adopted. |
| MCP conventions (`me`/`none` sentinels, names or ids, ambiguous name refused, limit/has_more) | n/a | built: owner `me`/`nobody` `tracker.rs:3396`, names refused when ambiguous | built: `tools.ts:226-229,840-848` | `patch[]`, `fields[]`, `orderBy`, `cursor` on work lists, candidates returned on an ambiguous name (it refuses instead). |
| Agent panel ("ask in place") | built: agent panel types `ChatTypes.swift:294-738`, `ChatView.swift:10` | n/a | built: `sona_work_ask`/`sona_work_take` `tools.ts:1344-1388` | asking from a task comment (needs comments). |
| Personal skills | missing | missing | missing | no saved skills anywhere. |
| Personal guidance file for agents | missing (no guidance setting) | partial: delegate prompt written by core `planning/work/agents.rs:801` | missing | one user-editable guidance text included in the delegate prompt and MCP instructions. |
| AIG rules (badge, visible state, stop when asked) | partial: agent shown on task and in history (`WorkTypes.swift:507,526`) | n/a | n/a | visible run state and stop (see row 3). |
| MCP connectors (Sona's agent calls other MCP servers) | missing | missing | n/a | not built. |
| Cloud/local coding agent bound to a task | partial: delegate copies a prompt to the clipboard `WorkStore.swift:348-356`; bridge watches the session | partial: bridge follow-session claim `agents.rs:761` | partial: claim/done | starting an agent from Sona and a steerable session view in task detail. |

## Section 3 tools not yet present (beyond the rows above)

| Proposed tool | State |
|---|---|
| `sona_task_statuses` | missing as a tool; the six statuses are an enum in the schema `tools.ts:218` |
| `sona_label_*` with `scope: task\|project` | missing scope (labels are task-only) |
| `sona_comment_list/save/delete` | missing (row 2) |
| `sona_project_save`, `sona_milestone_*`, `sona_project_update_*` | missing (rows 7a-7c) |
| `sona_note_save` | missing; notes are read-only over MCP (`sona_notes`, `sona_note` `tools.ts:663-675`) |
| `sona_cycle_save` | missing (row 7e) |
| `sona_person_list/get` | built as `sona_people`, `sona_person_*` reads `tools.ts:484-563`; partial-name candidates not returned |
| `sona_task_attach/detach` (local path) | missing; only a done-time evidence link `tools.ts:1059-1063` |
| `sona_search` | built `tools.ts:352` |
| `sona_meeting_list/get/commitments` | built as `sona_meetings`, `sona_meeting`, `sona_action_items`, `sona_work_follow_up` `tools.ts:383,418,444,912` |
| `sona_work_activity` | missing (row 3) |

## Build queue (missing and partial only, ranked)

1. Comments with threads on task, project, milestone, meeting, note; CLI verbs and MCP list/save/delete; agent as author. L (in progress elsewhere; land it before 3).
2. Agent run session per task plus `sona_work_activity` (thought, action, response, error, elicitation), stale/unresponsive timers, shown in task detail with a stop button routed through the bridge. L
3. Single `sona_task_save` (create when no id) with `patch[]` for details, `parent`, `estimate`, `links`, `duplicate_of`, relation add/remove arrays, source meeting; list filters `delegate`, created/updated/completed ranges, `order_by`, `cursor`. M
4. MCP `destructiveHint` and `idempotentHint` on every write; widen server instructions to projects, notes and meetings. S
5. MCP project save/archive and milestone list/save/delete over the existing CLI verbs. S
6. Explicit delegate/undelegate verb and MCP field, plus a delegate filter in CLI, MCP and the task filter bar. S
7. Cycle settings write in CLI and MCP (`work_cycle_settings_save` exists for the UI). S
8. Label groups and retire/restore, with a task/project scope. M
9. Triage label suggestions and per-suggestion dismiss. M
10. Project updates with health: store, UI on the project page, CLI, MCP. M
11. One change-event stream with old values that loops and task history read. M
12. Agent plan checklist and select questions answered in the app. M
13. User-defined loops: schedule, task-state and meeting-ended triggers, conditions, Run now, run history, versions, tool allowlist. L (depends on 11)
14. Personal guidance text fed into delegate prompts and MCP instructions. S
15. Local attachments on tasks (`attach`/`detach` by path). S
