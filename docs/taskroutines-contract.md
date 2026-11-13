# Task routines: templates, reminders, label groups, decline reasons

Core, CLI and MCP for four Linear rows: task templates, a reminder on a
task, label groups with retired labels, and a reason kept when triage is
declined. Repeating tasks already existed (`work update --repeat`, and
`work done` on a repeating task makes the next copy); this slice proves
that path end to end and adds nothing to it.

## Storage

One migration, `ROUTINES_MIGRATION` (src-tauri/src/planning/schema.rs, last
in `managers/history.rs`):

- `planning_task_templates(id, name, fields JSON, revision, created_at_ms, updated_at_ms)`;
  names unique ignoring case.
- `planning_task_reminders(task_id PK, remind_at_ms, set_at_ms, actor, agent, notified_at_ms)`;
  one reminder per task, dropped with its task.
- `planning_activity_reasons(activity_id PK, reason)`: the reason for the
  `declined` history entry it names.
- `planning_labels.group_name`, `planning_labels.retired_at_ms`.

## Rules

- A template keeps a title, details, priority, owner, project, labels, due
  offset in days, repeat words and sub-task titles. Using it makes the task
  and then its sub-tasks under it in one transaction; sub-tasks take the
  task's project and priority, and the owner a new sub-task takes. Labels
  that were deleted or retired and a project put away are left off.
- Under Suggest, an agent's task from a template lands in Triage with its
  sub-tasks, filed under the agent. Template save and delete, reminders and
  label group/retire are refused for agents under Suggest, like label save.
- A reminder is due when its time has passed and Sona has not shown it.
  Setting it again clears `notified_at_ms`. The core's follow-through tick
  (every minute) posts each due reminder as a notification titled with the
  task, then marks it shown through `work_reminders_notified`; a click opens
  the task. A reminder the notification permission blocks stays due.
- A retired label stays on its tasks and in `work labels`, is refused by
  `--labels`/`--add-label` and template labels, and is not matched by `#tag`
  in typed or spoken task text. Restoring brings it back.
- A label group is a name on the label, and it is single-select: a task
  carries at most one label from a group. The core holds this for every
  write that sets labels (create, update, `--add-label`, bulk edits,
  templates saved and used, `#tag` in task text): of two labels from one
  group, one the task did not have before wins, and otherwise the one named
  last. A task that already carries two from one group keeps them until its
  labels next change. The app's label picker shows each group as one row
  that opens its labels; picking one replaces the group's other label.
  `--dry-run` (CLI and MCP `dry_run`) applies the same rule, so its
  `labels` line shows what the write will leave.
- A template's due offset (`dueInDays`, `--due-in`, `due_in_days`) sets the
  task's due date that many days after the day it is made, unless the use
  names its own due date. The app sets it in Save as template and in
  Settings › Tasks › Templates.
- Every write takes `--json`, `--agent`, `--operation-id`, `--dry-run`, and
  replays through `store::mutate` receipts. Reminders on tasks kept on this
  Mac are left out of agents' lists.

## CLI

| verb | writes |
|---|---|
| `work templates` | no |
| `work template-save NAME [--template T] [--title] [--details] [--priority] [--owner] [--project] [--labels] [--due-in DAYS\|none] [--repeat WORDS\|none] [--subtask TITLE]...` | yes |
| `work template-delete T` | yes, destroys |
| `work template-use T [--title] [--due DAY]` | yes |
| `work remind TASK WHEN\|none` (2026-11-12 09:00, tomorrow 9am, friday 3pm) | yes |
| `work reminders [--due]` | no |
| `work label-group LABEL GROUP\|none` | yes |
| `work label-retire LABEL [--restore]` | yes |
| `work decline TASK... [--reason TEXT]` | yes |

`work labels` prints each label's group and `(retired)`; `work show` prints
`reason:` under a declined history entry, and its JSON `activity[].reason`.

## MCP

`sona_work_templates`, `sona_work_template_save`, `sona_work_template_delete`
(destroys), `sona_work_template_use`, `sona_work_remind`,
`sona_work_reminders`, `sona_work_label_group` (opt-in),
`sona_work_label_retire` (opt-in), and `reason` on `sona_work_triage`
decline.

## Bridge commands for the UI

All in src-tauri/src/native_bridge/dispatch.rs. Types are the Rust ones in
src-tauri/src/planning/work/routines.rs, camelCase.

| method | params | result |
|---|---|---|
| `work_templates` | `{}` | `WorkTemplate[]` |
| `work_template_save` | `{operationId, id?: string, name, fields: WorkTemplateFields}` | `WorkTemplate` |
| `work_template_delete` | `{operationId, id}` | `WorkTemplate` (as deleted) |
| `work_template_use` | `{operationId, id, using?: {title?, dueAtMs?, status?}}` | `WorkItem[]`, the task first |
| `work_reminders` | `{dueOnly?: bool}` | `WorkReminder[]` |
| `work_reminder_set` | `{operationId, id, atMs: number\|null}` | `WorkReminder\|null` |
| `work_reminders_notified` | `{operationId, ids: string[]}` | task ids marked |
| `work_label_group` | `{operationId, id, group: string\|null}` | `WorkLabel` |
| `work_label_retire` | `{operationId, id, retired: bool}` | `WorkLabel` |
| `work_triage_decline` | `{operationId, ids, reason?: string}` | `WorkItem[]` |

- `WorkTemplateFields`: `{title?, details?, status?, priority?, owner?: WorkOwner, projectId?, labelIds: string[], dueInDays?, recurrence?, subtasks: string[]}`
- `WorkTemplate`: `{id, name, fields, revision, createdAtMs, updatedAtMs}`
- `WorkReminder`: `{itemId, remindAtMs, setAtMs, actor, agent?, notifiedAtMs?}`
- `WorkLabel` gains `group?: string`, `retiredAtMs?: number`.
- `WorkActivity` gains `reason?: string`.

## Verified

2026-11-11, debug core `src-tauri/target/debug/sona`, fresh scratch HOMEs
under `/private/var/tmp/sona-scratch-taskroutines/` with
`SONA_SCRATCH_RUN=1` and invented data. Scripts: `smoke.sh` (CLI) and
`mcp.sh` + `mcp-smoke.ts` (MCP over stdio); outputs `smoke.out`, `mcp.out`.

- Template: `template-save` with two, then three `--subtask`;
  `template-use` made the task (High, you, due two days out, `[bug]`,
  every friday) and three sub-tasks under it; the same `--operation-id`
  replayed without a second copy; `template-delete` removed one. Over MCP
  the same with `subtasks` as a list, and `subtasks: []` cleared them.
- Reminders: a past time lists under `reminders --due`, future ones only
  under `reminders`; `none` clears; unreadable words fail with exit 2.
- Repeating: `create --due today --repeat daily` then `done` made the next
  copy due tomorrow (CLI and MCP).
- Decline: `--reason` / `reason` shows as `reason:` in `work show` and as
  `activity[].reason`; one row in `planning_activity_reasons`.
- Labels: grouped twice (second answered `Unchanged`); a retired label is
  refused by `create --labels` with a restore hint and stays on its task;
  restore and ungroup work.
- Agents under Suggest: label retire, template save and remind refused;
  `template-use` put the task and its sub-tasks in Triage. External
  mutations off: writes refused with `consent_required`, reads still work.
- Test `planning::work::routines::tests::a_reminder_set_again_after_it_was_shown_comes_due_again`
  passes, and fails (`left: []`) with `notified_at_ms = NULL` removed from
  the re-set.
- tools/sona-mcp: `bun test` 22 pass, `tsc --noEmit` clean, oxlint clean.

Single-select label groups and the template due offset, 2026-11-11:

- CLI, fresh HOME (`/private/var/tmp/sona-scratch-routinegaps/smoke.sh`,
  `smoke.out`): `update --add-label p-low` on `[p-high, bug]` left
  `[p-low, bug]` (history `labels: bug, p-high → bug, p-low`);
  `create --labels p-low,p-high` kept `[p-high]`; `template-save --labels
  p-high,p-low,bug --due-in 3` stored `p-low, bug · due in 3 days`, and
  `template-use` made the task due three days out. `update S-1 --add-label
  p-low --dry-run` printed `labels: bug, p-high → bug, p-low`, and
  `template-save Dry --labels p-high,p-low,bug --dry-run` printed
  `p-low, bug`.
- MCP (`mcp-smoke.ts`, `mcp.out`): `add_labels: large` with `dry_run: true`
  on a task labeled `small` in the same group previewed `small → large`;
  the write then left only `large`. A template with `due_in_days: "3"` made
  a task due three days out.
- Scratch app (`/private/var/tmp/sona-scratch-polish`): `work update S-7
  --add-label Design --add-label Budget` on `[Signage]` gave `[Budget,
  Design]`. In the app the label picker lists Look and Operations as one
  row each with the current label; opening Look and choosing Signage gave
  `[Budget, Signage]` in `work show S-7`. Settings › Tasks shows the
  template's due menu; choosing "Due the next day" stored `due in 1 day`.
  The Save as template sheet has the Due picker. Shots:
  `docs/linear-audit/shots/12-label-groups-picker.png`,
  `12-label-group-open.png`, `12-label-group-replaced.png`,
  `12-template-due-settings.png`, `12-save-template-due.png`.
- Tests `query::external::tracker::tests::adding_a_second_label_from_a_group_replaces_the_first_in_the_dry_run_too`
  (dry-run line and write agree) and
  `planning::work::routines::tests::a_template_due_in_three_days_makes_a_task_due_three_days_out`
  pass. Before the preview fix the first failed with the dry run showing
  `bug, p-high, p-low`; with the core rule removed the write kept three
  labels; with the offset removed the second got no due date.

Not verified: bulk label edits in the app were not driven live; they go
through the same core check the first test covers.
