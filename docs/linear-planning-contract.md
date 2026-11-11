# Planning contract: project updates, project fields, milestones, cycles, progress, filters

Core, command line and bridge for LinearPlanning-report must-haves 2, 3, 5,
6, 7, 8 and 9. Swift and the MCP server read this to build on it. Core
types serialize camelCase; command-line JSON is snake_case.

## Storage

One migration, `PLANNING_MIGRATION` (`src-tauri/src/planning/schema.rs`),
appended to the history migrations.

- `planning_projects` rebuilt: status is `backlog | planned | active |
  paused | completed | canceled` (old rows keep their value); new columns
  `priority` (0 none, 1 urgent .. 4 low), `start_at_ms`,
  `start_resolution`, `target_resolution` (`day | month | quarter |
  halfYear | year`), `icon`, `trashed_at_ms`.
- `planning_project_links(id, project_id, title, url, position)`.
- `planning_project_labels(project_id, label_id)`: the task labels table is
  reused for names and hues.
- `planning_project_updates(id, project_id, health onTrack|atRisk|offTrack,
  body, progress, author_kind, author_name, created_at_ms, edited_at_ms,
  deleted)`.
- `planning_milestones.position`.
- `planning_cycle_settings` rebuilt: `weeks` 1..8, `cooldown_weeks` 0..4,
  `upcoming` 1..15 (old row kept; earlier builds capped weeks at 2).
- `planning_cycles.name`.
- `planning_cycle_days(number, day, scope, started, done)`: a cycle's daily
  counts, frozen when it ends.

Trash: a deleted project gets `trashed_at_ms`, drops out of every list,
and is purged with its milestones, links, labels and updates 30 days
later by the 60-second cycles tick (which then refreshes every window);
its tasks stay, in no project.

## Rules

- Percent complete everywhere (project, milestone, cycle success): done
  counts 100, started 25, divided by scope (canceled and duplicate tasks
  are out of scope).
- `update_missing`: the project is active and its latest update (or its
  creation, when none) is older than one cycle length + 3 days.
- Auto-draft (`updates::draft`): plain text from tasks since the last
  update: completed and started counts with titles, milestones reached,
  and the percent move when more than 2 points. Deterministic, no model.
- Progress history (`progress::outlook`, pure): weekly scope/started/done
  from task activity since the project began; prediction from velocity
  weighted to recent weeks, started counting a quarter;
  optimistic/pessimistic are the expected remaining time ±40%.
- Milestones order by `position`, then due day. `next_open` marks the
  first not complete.
- Cycles: `advance` keeps `upcoming` cycles ahead, spaced by the cooldown;
  an ended cycle's daily counts are frozen into `planning_cycle_days` and
  later reads use the frozen copy; unfinished work rolls into the cycle
  under way. Success = (done*100 + started*25)/scope at close.

## Bridge methods (`native_bridge/dispatch.rs`)

| Method | Params | Returns |
|---|---|---|
| `planning_project_detail` (existing) | `projectId` | `ProjectDetail`, now with `latestUpdate`, `updateMissing`, `progress`, `milestoneProgress` |
| `planning_project_updates` | `projectId`, `limit?` (≤100, default 20), `cursor?` (last id) | `{updates: [ProjectUpdate], nextCursor}` |
| `planning_project_update_add` | `operationId`, `projectId`, `health`, `body` | `ProjectUpdate` |
| `planning_project_update_edit` | `operationId`, `id`, `health?`, `body?` | `ProjectUpdate` |
| `planning_project_update_delete` | `operationId`, `id` | `ProjectUpdate` (deleted) |
| `planning_project_update_draft` | `projectId` | `String` markdown |
| `planning_project_save` | `operationId`, `id?` (none creates), `input` | `Project` |
| `planning_projects_trashed` | none | `[ProjectSummary]`, same shape as `planning_projects`, `trashedAtMs` set, newest deletion first; restore with `planning_project_save` `input.trashed: false` |
| `planning_milestone_reorder` | `operationId`, `projectId`, `milestoneIds` (all, in order) | `[Milestone]` |
| `work_cycle_settings_save` (existing) | `operationId`, `settings` | `WorkCycles` |
| `work_cycle_save` | `operationId`, `number`, `name?`, `startDay?`, `endDay?` (YYYY-MM-DD, end exclusive) | `WorkCycles` |
| `work_cycle_rollover` | `operationId`, `number` | `WorkCycles` |
| `work_cycle_start_today` | `operationId`, `number` | `WorkCycles` |
| `work_list` (existing) | `query.filters: [WorkCondition]` added | `WorkList` |

`planning_project_save.input` (camelCase; a missing key leaves the field,
`null` clears it): `title`, `description`, `status`, `leadPersonId`,
`memberPersonIds`, `targetAtMs`, `hue`, `priority`, `startAtMs`,
`startResolution`, `targetResolution`, `icon`, `labelIds`,
`links: [{title, url}]`, `archived`, `trashed` (true bins, false restores).

`settings` for `work_cycle_settings_save`: `{weeks, startWeekday,
cooldownWeeks, upcoming}`. Swift must send all four: a missing
`cooldownWeeks`/`upcoming` reads as 0 and 1 and resets them.

`WorkCycles` gains `settings.cooldownWeeks`, `settings.upcoming`; each
`WorkCycle` gains `name`, `started`, `success`; `WorkCycleDay` gains
`started`.

`WorkCondition`: `{field, op, values}`; `field` one of `status priority
owner project milestone label cycle due created updated`; `op` one of `is
is_not any_of none_of includes_all includes_any before after`. Values are
ids, stored status names (`not_started`...), priorities (0-4 or names),
`me` (owner), `none` (empty field), cycle numbers or `current`/`next`, and
days as `YYYY-MM-DD`, `today`, `tomorrow`, `yesterday` or epoch ms.
`before`/`after` take dated fields only. Labels: `is`/`includes_all` need
every value, `any_of`/`includes_any` any, `is_not`/`none_of` none. All
conditions must hold, on top of the older query fields, which still work.
Saved views store the query, so they keep their conditions.

## Command line

All writes take `--json --agent NAME --operation-id ID --dry-run`, go
through the consent gates and Apply/Suggest like `work update`, and a
repeated operation id replays the first answer.

- `work project PROJECT [--json]`: fields, latest update, update due,
  weekly progress, predicted finish, milestone progress.
- `work project-update PROJECT --health on-track|at-risk|off-track
  [--body MD]`: without `--body`, the auto-draft is posted.
- `work project-updates PROJECT [--limit N] [--cursor ID]`.
- `work project-update-edit ID [--health H] [--body MD]`,
  `work project-update-delete ID`: your own updates only.
- `work project-update-draft PROJECT`: prints the draft; nothing posted.
- `work project-save NAME` gains `--priority`, `--start DAY`,
  `--start-resolution`, `--target-resolution`, `--icon`, `--labels a,b`,
  `--link TITLE=URL` (repeatable; `none` clears), `--dry-run`; `none`
  clears each.
- `work project-delete PROJECT [--restore]`; `work projects --trashed`.
- `work milestone-reorder PROJECT M1 M2 ...`.
- `work cycle-settings [--weeks 1-8] [--start-weekday 1-7] [--cooldown
  0-4] [--upcoming 1-15]`.
- `work cycle-save CYCLE [--name N|none] [--start DAY] [--end DAY]` (end
  inclusive here).
- `work cycle-rollover CYCLE`, `work cycle-start-today [CYCLE]`.
- `work list --filter JSON` and `work view-save NAME --filter JSON`: one
  condition object or a list.

## MCP to add

`sona_project`, `sona_project_update` (add/edit/delete/draft),
`sona_project_updates`, `sona_project_delete`, `sona_milestone_reorder`,
`sona_cycle_settings`, `sona_cycle_save`, `sona_cycle_rollover`,
`sona_cycle_start_today`, and a `filter` array on `sona_list`/view save,
each wrapping the verbs above with `--json`.

## Swift to do

New project statuses (backlog, planned, paused) and fields; health picker
and update feed; trash list with restore; milestone drag order and
`nextOpen`; cycle settings sheet with cooldown and upcoming (send all
fields); cycle rename/date move/start today/rollover; cycle graph from
`days` (scope/started/done); project progress chart and prediction band;
filter builder writing `query.filters`.

## Not built

- The natural-language filter helper ("due next week", "waiting on X"):
  skipped.

## Verified

- `cargo check --lib --tests --target aarch64-apple-darwin`: clean.
- `cargo test --lib -- planning::progress planning::work::filter`: 4
  passed. Each was watched failing with its rule broken (started worth 0
  instead of 25; equal week weights; label `is` as any; `before` as ≤).
- `cargo test --lib -- planning::`: 62 passed, 1 failed. The failure,
  `the_tracker_migration_keeps_every_task_and_the_commitment_it_tracks`,
  stops at `no such column: t.parent_id`: the test skips the subtasks
  migration that HEAD's `items.rs` already reads; neither file is in this
  change.
- Anti-slop scan of the changed planning and tracker files: only the
  `downcast_ref` findings in `planning.rs`, `store.rs`, `proposal.rs`,
  which are on HEAD unchanged.
- Scratch CLI smoke, fresh HOME `/private/var/tmp/sona-scratch-planning`
  (`smoke.sh`, full output `out.txt`). This HOME cannot open the encrypted
  conversations, so `project-save` refuses as before this change
  (`unavailable`), and the project was seeded with one SQL insert dated
  22 days back. Excerpts:

```
$ work project-update-draft Harbor launch
- 3 tasks added so far.
- In focus: Venue booked, 0% complete.
- Progress: 0% to 41%.

$ work project-update Harbor launch --health at-risk --dry-run
Would post an update on Harbor launch
$ work project-updates Harbor launch
Harbor launch has no updates.

$ work project-update Harbor launch --health off-track --body ""
invalid_request: --body cannot be empty; leave it out to have Sona draft it.

$ work project-update Harbor launch --health at-risk --operation-id 1111... --json
{..."action":"added","operation_id":"1111...","update":{"id":"06d5d03f-...","health":"at_risk",...,"progress":41,...},"replayed":false}
$ (same again)
{..."update":{"id":"06d5d03f-...",...},"replayed":true}

$ work project-update-edit 06d5d03f-... --health on-track --body "Venue held; copy done."
Edited an update on Harbor launch
06d5d03f-...  On track · 41% · Agent · 2026-11-10

$ work project Harbor launch
Harbor launch · Active · 41% (1 done, 1 started of 3)
Latest update:
06d5d03f-...  On track · 41% · Agent · 2026-11-10
  Venue held; copy done.
> Venue booked · 0%
  Site live · 0%

$ work project Harbor launch --json
{..."update_missing":false,"progress":{"scope":3,"started":1,"done":1,"percent":41,"weeks":[{...scope 0}x3,{"scope":3,"started":1,"done":1}],"prediction":null},"milestones":[{"name":"Venue booked",...,"next_open":true},{"name":"Site live",...,"next_open":false}]}

$ work milestone-reorder Harbor launch "Site live" "Venue booked" --dry-run
Would order the milestones of Harbor launch:
$ work milestone-reorder Harbor launch "Site live" "Venue booked"
Ordered the milestones of Harbor launch:
  Site live
  Venue booked

$ work project-delete Harbor launch --dry-run
Dry run. Saved project Harbor launch.
In the bin; deleted for good on 2026-12-10.
$ work projects
Harbor launch  · Active · 2 open, 1 done
$ work project-delete Harbor launch
$ work projects
No projects yet. Make one with: sona work project-save NAME
$ work projects --trashed
Harbor launch  · Active · 2 open, 1 done
$ work project-delete Harbor launch --restore
$ work projects
Harbor launch  · Active · 2 open, 1 done

$ work project-update-delete 06d5d03f-... --dry-run
Would delete an update on Harbor launch
$ work project-update-delete 06d5d03f-...
Deleted an update on Harbor launch
$ work project-updates Harbor launch --limit 1
Harbor launch has no updates.

$ work cycle-settings --weeks 3 --cooldown 1 --upcoming 3 --dry-run
Dry run. Cycles now run: 3 weeks, 1 cooldown, 3 ahead.
Cycle 1   current  Mon 9 Nov to Sun 29 Nov  0 of 0 done
Cycle 2   next     Mon 7 Dec to Sun 27 Dec  0 of 0 done
...
$ work cycles
Cycles run 1 week from Monday.
$ work cycle-settings --weeks 3 --cooldown 1 --upcoming 3 --operation-id 2222... --json   (twice: same answer)
{..."weeks":3,"start_weekday":1,"cooldown_weeks":1,"upcoming":3,"cycles":[...4 cycles...]}
$ work cycle-settings --weeks 9
error: invalid value '9' for '--weeks <WEEKS>': 9 is not in 1..=8

$ work cycle-save next --name "Harbor push" --json
{..."number":2,...{"number":2,"name":"Harbor push","state":"upcoming",...}}
$ work cycle-rollover current
Moved the cycle's unfinished work on: 3 weeks, 1 cooldown, 3 ahead.
$ work cycle-start-today --dry-run
Dry run. Started the cycle today: 3 weeks, 1 cooldown, 3 ahead.
Cycle 1   ended    Mon 9 Nov to Mon 9 Nov  0 of 0 done
Cycle 2   current  Tue 10 Nov to Sun 27 Dec  0 of 0 done
...

$ work list --filter '{"field":"priority","op":"is","values":["high"]}'
S-1    In progress  High    Book the venue  · Harbor launch
$ work list --filter '{"field":"created","op":"after","values":["yesterday"]}'
S-1    In progress  High    Book the venue  · Harbor launch
S-3    Todo         -       Ship the site  · Harbor launch
$ work list --filter '{"field":"priority","op":"before","values":["high"]}'
invalid_request: Check the title, date, and selected owner.
$ work view-save Hot --filter '{"field":"priority","op":"any_of","values":["urgent","high"]}'
Saved view Hot  · priority any of urgent, high
$ work list --view Hot
S-1    In progress  High    Book the venue  · Harbor launch
```

Not verified: the new `project-save` fields and its dry run through the
command line (needs open conversations), the 30-day purge and cycle
freezing over real time, the Swift bridge calls, and a migration run on a
real older database.
