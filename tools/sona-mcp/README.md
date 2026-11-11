# sona-mcp

An MCP server over Sona's local corpus and task tracker. Each call spawns the
installed Sona binary with its existing headless flags and returns the JSON.
The server keeps no corpus state and does not send source material to a
provider.

Everything it can reach is behind consent rows that are off on install.
**Settings > Agents > External access** gates every request. Every tool that
changes something — `sona_loop_resolve` and the task writes below — also
needs **Settings > Agents > External mutations**. With either row off it
returns `consent_required` naming the missing setting. A `dry_run:true`
preview needs only External access.

## Install

```bash
cd tools/sona-mcp
bun install
```

Register it with your agent:

```bash
# Claude Code
claude mcp add sona -- bun /absolute/path/to/sona/tools/sona-mcp/src/index.ts

# Codex
codex mcp add sona -- bun /absolute/path/to/sona/tools/sona-mcp/src/index.ts
```

The server uses the native app's bundled engine at
`/Applications/Sona.app/Contents/Helpers/SonaCore.app/Contents/MacOS/sona-core`.
Point `SONA_BIN` somewhere else for a dev build:

```bash
claude mcp add sona --env SONA_BIN=/path/to/sona/src-tauri/target/debug/sona \
  -- bun /absolute/path/to/sona/tools/sona-mcp/src/index.ts
```

## Tools

| Tool                 | Arguments                                | Runs                                                                    |
| -------------------- | ---------------------------------------- | ----------------------------------------------------------------------- |
| `sona_search`        | `query`, `scope?`, `limit?`              | `sona --query … [--scope …] [--limit …]`                                |
| `sona_meetings`      | `last?` \| (`from` + `to`)               | `sona --meetings [--last …] [--from … --to …]`                          |
| `sona_meeting`       | `meeting_id`                             | `sona --meeting <id>`                                                   |
| `sona_transcript`    | `meeting_id`                             | `sona --transcript <id>`                                                |
| `sona_action_items`  | `status?`, `side?`, `after?`, `limit?`   | `sona --loops [--status …] [--mine\|--waiting] [--after …] [--limit …]` |
| `sona_people`        | `name`, `limit?`                         | `sona --people <name> [--limit …]`                                      |
| `sona_person_catch_up` | `person_id`, `since_utc_ms?`            | `sona --person-catch-up <id> [--since-utc-ms …]`                        |
| `sona_person_changes`  | `person_id`, `since_utc_ms`             | `sona --person-changes <id> --since-utc-ms …`                            |
| `sona_person_stats`    | `person_id`                             | `sona --person-stats <id>`                                              |
| `sona_upcoming`      | `limit?`                                 | `sona --upcoming [--limit …]`                                           |
| `sona_plans`         | `limit?`                                 | `sona --plans [--limit …]`                                              |
| `sona_bookmarks`     | `limit?`                                 | `sona --bookmarks [--limit …]`                                          |
| `sona_roster`        | `meeting_id`                             | `sona --roster <id>`                                                    |
| `sona_place`         | `meeting_id`, `limit?`                   | `sona --place <id> [--limit …]`                                         |
| `sona_projects`      | `limit?`                                 | `sona --projects [--limit …]`                                           |
| `sona_project_get`   | `project_id`                             | `sona --project <uuid>`                                                |
| `sona_notes`         | `limit?`                                 | `sona --notes [--limit …]`                                              |
| `sona_note`          | `note_id`                                | `sona --note <id>`                                                      |
| `sona_note_context`  | `query`, `limit?`                         | `sona --note-context <text> [--limit …]`                                 |
| `sona_note_versions` | `note_id`                                | `sona --note-versions <id>`                                             |
| `sona_note_diff`     | `note_id`, `from_version`, `to_version?` | `sona --note-diff <id> --from-version … [--to-version …]`               |
| `sona_stats`         | none                                     | `sona --stats`                                                          |
| `sona_events`        | `after?`, `limit?`                       | `sona --events [--after …] [--limit …]`                                 |
| `sona_loop_resolve`  | `loop_id`                                | `sona --loop-resolve <loop_id>` — **writes**                            |

`scope` is one of `all`, `meetings`, `dictations`, `people`, `loops`, `notes`. `status`
is `open` or `done`. `side` is `mine` (what you owe) or `waiting` (what
somebody else owes you). Pass an action-items `next_cursor` as `after` with
the same filters; `has_more` is true only when that cursor is present.

Find the person with `sona_people`, then use that returned id for catch-up or
changes. Both return the native person-memory `snapshot`, with a `view` of
`catch_up` or `changes` and the person's `link`. The snapshot is not a generated
answer. `catch_up` selects ids into its conversations, contributions, notes,
commitments and changes. `limitations` names missing or incomplete evidence.
These person snapshots are not paged; `limit` is not accepted for them.

Changes contain `earlier` and `later` source records, text and AI-interpretation
labels. Cite both links. A difference in wording is not proof of a changed
decision or a contradiction. An empty comparison means no comparable retained
evidence differs, not proof nothing changed. `since_utc_ms` is an exact UTC
millisecond boundary, not a local date.

Transcript lines keep the existing `text` field as the effective reading and
add `raw_text`, nullable `corrected_text`, segment and transcript revision ids,
nullable `edit_revision` and a meeting `link`. Captured words are not overwritten.
Removed lines remain excluded. Person contributions also keep captured and
corrected wording separate, with surrounding speech when available.

```bash
sona --people "Fenil"
sona --person-catch-up <returned-person-uuid> --format text
sona --person-changes <returned-person-uuid> --since-utc-ms <utc-milliseconds>
sona --note-context "release plan" --limit 3 --format text
sona --transcript <meeting-uuid> --format text
sona --projects --limit 10 --format text
sona --project <returned-project-uuid> --format text
```

`sona_note_context` uses the same local note-search page as the CLI, with current
note bodies, matching text and resolved source references. It returns at most
the requested number of notes and does not expand related-note bodies outside
that page. Check `has_more` and `reason` before treating the result as complete.
Both context and single-note reads preserve saved passage references,
`interpretation` labels and explained connections. `interpretation:true` marks
AI interpretation, not a transcript quote. A passage reference identifies the
chosen raw or corrected wording and recording moment; it does not export a
second copy of meeting text. Connection reasons describe explicit links or
shared source records, not a guessed relationship.

Note reads include source links and related notes. Deleted or unavailable
sources have `available:false`, a generic title and no exported link or source
snapshot. Note versions are saved states, not a keystroke log: autosaves within
ten minutes can merge, and at most 50 versions are retained. Diffs are exact
single replacement hunks with original line endings, not minimal edit scripts.
Diffs also include `from_source` and `to_source`: saved version ids, save times,
version kinds and version-scoped note links.

Project reads use the native planner's saved records. `sona_projects` returns
the most recently updated projects first, defaulting to 25 rows and capped at
100. Rows include saved titles, states, revisions, task counts and the next
saved task or milestone due date. Descriptions are not included in the list.
`has_more:true` and `complete:false` mean other saved projects were omitted.
The list has no continuation cursor.

Pass a returned project id to `sona_project_get` for its saved description,
milestones, tasks and evidence references. States, revisions, dates and text
are preserved. A missing due date remains `null`; no deadline, team membership
or notification is inferred. Detail reads are not paged and reject `--limit`.
Each task includes live `owner_available` and the resolved `owner_person_id`;
a merged person resolves to the surviving id, while an unavailable owner has
no person link. These are explicit personal-plan assignments, not a team roster.

Evidence retains the saved target, source id, recording offset and revision.
The native planner resolves source availability for every detail read.
Available references carry their current title and an exact meeting, note or
loop link. Meeting passage links include `?atMs=` with the saved offset.
Unavailable references have `available:false`, `title:null` and `link:null`;
the saved reference remains, but no former source content is exported.
These reads require the existing external-access grant and expose no planner
write. They do not add notes or plans to the remote panel's default context,
call a provider or send notifications.

Statistics count all retained records. Each dataset has `status:available` with
totals or `status:unavailable` with an error and no totals. `sources` gives the
counting method and up to 25 sample record links per dataset, with its own
availability and `has_more`. These links are samples, not an enumeration or the
population used for totals. Counts and samples are separate reads, not a shared
transaction. `complete:false` means a count or sample was unavailable;
unavailable data is never zero. See
[`skills/sona/SKILL.md`](../../skills/sona/SKILL.md) for field definitions.

These tools do not edit, restore, link or synthesize notes. Those remain native
app actions and do not gain a route around external-write or source-transfer
consent. For human CLI output, append `--format text`; MCP keeps default JSON.
MCP publishes `readOnlyHint:true` for every read and `false` for every tool
that writes, and `destructiveHint:true` only for the writes that take away
what no tool gives back: label and view deletes, comment deletes, attachment
removal, milestone deletes, person merges, and speaker naming, since changing
who a named speaker is deletes the voice samples kept from them.
The server passes private source data to its approved client. Neither Sona nor
this server calls a model provider for these reads. External access is not
permission for an agent to send those records to another provider.

## Task tools

These run `sona work <verb> --json`. A task is named by its handle (`S-12`),
its number or its id. Eighteen are published by default:

| Tool               | Arguments                                                        | Runs                                       |
| ------------------ | ---------------------------------------------------------------- | ------------------------------------------ |
| `sona_work_list`   | filters (`status?`, `side?`, `owner?`, `label?`, …), `filter?`, `where?`, `ready?`, `limit?` | `sona work list` or `sona work ready` |
| `sona_work_get`    | `task`                                                           | `sona work show`                           |
| `sona_work_counts` | `limit?`                                                         | `sona work mine`                           |
| `sona_work_create` | `title`, fields (`priority?`, `owner?`, `due?`, `labels?`, …)    | `sona work create` — **writes**            |
| `sona_work_update` | `tasks`, fields, `parent?`, `add_labels?`, `remove_labels?`, `dry_run?` | `sona work update` — **writes**   |
| `sona_work_claim`  | `task`                                                           | `sona work claim` — **writes**             |
| `sona_work_done`   | `tasks`, `evidence?`, `dry_run?`                                 | `sona work done` — **writes**              |
| `sona_work_triage` | `tasks`, `decision` (`accept`, `decline`, `duplicate`), `of?`    | `sona work accept`/`decline`/`duplicate` — **writes** |
| `sona_work_triage_suggestions` | `task?`                                                | `sona work triage-suggestions`             |
| `sona_work_suggestion_accept` | `id`, `dry_run?`                                        | `sona work suggestion-accept` — **writes** |
| `sona_work_suggestion_dismiss` | `id`, `dry_run?`                                       | `sona work suggestion-dismiss` — **writes** |
| `sona_work_follow_up` | `meeting`                                                     | `sona work follow-up`                      |
| `sona_subtask_create` | `parent`, `titles` (a list), `owner?`, `priority?`, `dry_run?` | `sona work subtask` — **writes**        |
| `sona_comment_list` | `task`, `limit?`, `cursor?`                                     | `sona work comments`                       |
| `sona_comment_save` | `body`, `task` or `id`, `reply_to?`, `dry_run?`                 | `sona work comment add` or `comment-edit` — **writes** |
| `sona_comment_resolve` | `id`, `reopen?`, `dry_run?`                                  | `sona work comment-resolve` — **writes**   |
| `sona_task_attachments` | `task`                                                      | `sona work attachments`                    |
| `sona_task_attach` | `task`, `path` or `url`, `title?`, `subtitle?`, `dry_run?`      | `sona work attach` — **writes**            |

Every write also takes `operation_id`. Left out, Sona derives one from the
request and what it read, so a repeated call never changes anything twice: a
create repeated the same day answers with the first task and `replayed:true`,
and a change already made lists its tasks under `skipped`. Pass a new id to
create a twin on purpose. Every write carries the client's name from the MCP
handshake, so Sona shows which agent made it.

`sona_work_list` with `ready:true` returns Todo tasks nothing open blocks, and
`due` narrows any list to due windows such as `overdue,today` or `next:14`.
`filter` is a list of conditions that must all hold, passed to `--filter` as
one JSON argument: `[{"field":"label","op":"includes_all","values":["bug","ui"]}]`.
Fields are `status`, `priority`, `owner`, `project`, `milestone`, `label`,
`cycle`, `due`, `created` and `updated`; ops are `is`, `is_not`, `any_of`,
`none_of`, `includes_all`, `includes_any`, `before` and `after` (dated fields
only). Values are ids or names, `me`, `none`, cycle numbers or
`current`/`next`, and days as `YYYY-MM-DD`, `today`, `tomorrow`, `yesterday`,
`this_week`, `next_week`, `last_week`, `this_month`, `next_month`, `last_month`
or epoch ms. `where` takes plain words instead, such as `high priority bugs due
this week that are mine`, read on this Mac without a model; the answer's
`read_as` shows each condition and the words it could not read.
`sona_work_view_save` stores a `filter` with the view. `ready:true` takes no
`filter` or `where`, and the list has no sort or cursor: Sona orders it.
`sona_work_claim` moves one Todo or Backlog task to In progress and shows the
agent on it; it refuses a task another agent works or one already in
progress, so two agents cannot both start it. The task's owner stays who it
was. `sona_work_done` closes the meeting commitment a task came from; a link
in `evidence` (a pull request, say) is kept with the task as evidence, and any
other line goes into its details. `repeat` keeps how often a task comes back
(`every monday`, `every 2 weeks`, `none` to stop); finishing the task does not
make the next one.
`sona_work_follow_up` drafts one meeting's follow-up: who owes what, by
when, quoting each promise. It sends nothing.

`sona_work_get` also returns the task's parent, its sub-tasks with how many
are done, and its comment threads. `sona_subtask_create` makes one sub-task
per title under a parent; each takes the parent's project, milestone and
priority. `sona_work_update` with `parent` moves tasks under another task,
and `parent: null` lifts them back to top level. `sona_comment_save` without
`id` comments on `task`, or replies in a thread with `reply_to`; with `id` it
changes a comment the agent wrote. Comments are shown under the agent's
name. An agent working a task it claimed should comment what it is doing,
what it found and what is left, so the user can follow along in Sona.

`sona_task_attach` copies a file into Sona (by absolute path, at most 50 MB)
or keeps a link (http, https, mailto or sona) on a task; the same file or
link twice lands once. `sona_task_attachments` and `sona_work_get` list them
with the path of Sona's copy. Agents may attach the artifacts they produce
for a task, such as a report, a screenshot or a pull request link, and never
secrets, keys, tokens or credentials. `sona_task_detach` (opt-in) removes one
the agent attached and deletes Sona's copy.

Unless the user sets **Settings > Agents > Agents' changes** to Apply, a
change to a task's fields waits for them: the answer lists it under
`proposed`, and the user accepts or rejects it in Sona. A created task lands
in Triage, and triage, archiving and label changes are refused. Comments and
attachments are added either way.

`sona_work_ask` answers a question with the lines from meetings and
dictations that say it, numbered; `sona_work_take` makes Triage tasks of
chosen lines. `sona_work_say` changes tasks from plain words ("push
everything not urgent to monday"); run it with `dry_run:true` first.

`sona_work_triage_suggestions` lists what Sona suggests for tasks in Triage:
a project, labels, an owner, a due day, and a task one repeats or relates to.
Each has an id and its reason in plain words, such as "3 of the last 4 tasks
from meetings with Dana went to Pier Lighting". Sona works them out from past
tasks, their titles and the people in their meetings, with no model, and
suggests nothing when the evidence is thin. `sona_work_suggestion_accept`
applies one like any other change: a field change waits under `proposed`
unless the user lets agents' changes apply. `sona_work_suggestion_dismiss`
sets one aside for good, and is refused unless agents' changes apply.
Suggestions that draw on a meeting kept on this Mac are left out.

Twenty-one more are opt-in: `sona_work_archive`, `sona_work_restore`,
`sona_work_relate`, `sona_work_unrelate`, `sona_cycle_list`, `sona_cycle_save`,
`sona_work_labels`, `sona_work_label_save`, `sona_work_label_delete`,
`sona_work_views`, `sona_work_view_save`, `sona_work_view_delete`,
`sona_work_watches`, `sona_work_watch`, `sona_comment_delete`,
`sona_task_detach`, `sona_project_archive`, `sona_project_delete`,
`sona_project_update_delete`, `sona_milestone_delete` and `sona_person_merge`.
Publish them with
`SONA_MCP_TOOLS`: `all` for every tool, or a comma-separated list of names
beside the default ones. A name that matches no tool stops the server at
start.

```bash
claude mcp add sona --env SONA_MCP_TOOLS=sona_work_archive,sona_work_restore \
  -- bun /absolute/path/to/sona/tools/sona-mcp/src/index.ts
```

## Organizing, meeting loop and note tools

All opt-in. Each write takes `dry_run` and `operation_id`, needs both grants
and **Agents' changes** on Apply (refused, never proposed), and answers
`replayed:true` when the change was already made.

| Tool                            | Arguments                                                  | Runs                                  |
| ------------------------------- | ---------------------------------------------------------- | ------------------------------------- |
| `sona_work_delegate`            | `task`, `to` (an agent, or `none` to take it back)          | `sona work delegate` — **writes**     |
| `sona_work_day`                 | `day?`                                                     | `sona work day`                       |
| `sona_work_day_plan`            | `tasks` (up to three, or `none`), `day?`                   | `sona work focus` — **writes**        |
| `sona_work_keep`                | `tasks`                                                    | `sona work keep` — **writes**         |
| `sona_work_brief`               | `person`                                                   | `sona work brief`                     |
| `sona_work_talking_point_add`   | `person`, `text`                                           | `sona work talking-point` — **writes** |
| `sona_work_talking_point_done`  | `id`, `undo?`                                              | `sona work talking-point-done` — **writes** |
| `sona_meeting_loop_reopen`      | `loop_id`                                                  | `sona work loop-reopen` — **writes**  |
| `sona_meeting_loop_assign`      | `loop_id`, `owner` (a person, or `none`)                   | `sona work loop-owner` — **writes**   |
| `sona_speaker_assign`           | `meeting_id`, `speaker`, one of `person`, `new_person`, `undo` | `sona work speaker-assign` — **writes** |
| `sona_planning_track_action`    | `loop_id`, `project`                                       | `sona work action-track` — **writes** |
| `sona_planning_dismiss_action`  | `loop_id`                                                  | `sona work action-dismiss` — **writes** |
| `sona_planning_restore_action`  | `loop_id`                                                  | `sona work action-restore` — **writes** |
| `sona_scratchpad_create`        | `body`                                                     | `sona work note-new` — **writes**     |
| `sona_scratchpad_update_body`   | `note_id`, `body`                                          | `sona work note-edit` — **writes**    |
| `sona_scratchpad_set_pinned`    | `note_id`, `pinned`                                        | `sona work note-pin` — **writes**     |
| `sona_scratchpad_link`          | `note_id`, `kind` (meeting, person, note), `source_id`     | `sona work note-link` — **writes**    |
| `sona_work_attachment_rename`   | `task`, `id`, `title`                                      | `sona work attachment-rename` — **writes** |
| `sona_work_capture`             | `title`, `url`, `url_title?`, task fields                  | `sona work create --source-url` — **writes** |

`sona_work_list` also takes `source` (meeting, dictation, agent, person) and
`stale` (open work untouched for 30 days), and `sona_work_update` takes
`snooze_until: "later-today"`. Loop changes and speaker names are filed in
the meeting's history as the user's. `sona_speaker_assign` never remembers a
voice: saving a voice profile stays a consent step the user takes in the app.

## Projects, people and meetings tools

These run the same `sona work <verb> --json` catalog the Projects and People
pages show. A project, milestone or person is named by its name, any name it
goes by, or its id.

| Tool                         | Arguments                                                              | Runs                                     |
| ---------------------------- | ---------------------------------------------------------------------- | ---------------------------------------- |
| `sona_project_list`          | `all?`, `trashed?`                                                     | `sona work projects [--all] [--trashed]` |
| `sona_project_detail`        | `project`                                                              | `sona work project`                      |
| `sona_project_save`          | `name`, `project?`, `details?`, `status?`, `priority?`, `lead?`, `members?`, `start?`, `start_resolution?`, `target?`, `target_resolution?`, `hue?`, `icon?`, `labels?`, `links?`, `dry_run?` | `sona work project-save` — **writes** |
| `sona_project_delete`        | `project`, `restore?`, `dry_run?`                                      | `sona work project-delete` — **writes**  |
| `sona_project_archive`       | `project`, `restore?`                                                  | `sona work project-archive` — **writes** |
| `sona_project_update_list`   | `project`, `limit?`, `cursor?`                                         | `sona work project-updates`              |
| `sona_project_update_save`   | `project` + `health` + (`body` or `draft:true`), or `id` + `health?` + `body?`; `dry_run?` | `sona work project-update` or `project-update-edit` — **writes** |
| `sona_project_update_delete` | `id`, `dry_run?`                                                       | `sona work project-update-delete` — **writes** |
| `sona_milestone_list`        | `project`                                                              | `sona work milestones`                   |
| `sona_project_timeline`      | `from?`, `to?`                                                         | `sona work timeline`                     |
| `sona_milestone_save`        | `project`, `name`, `milestone?`, `target?`, `details?`, `status?`      | `sona work milestone-save` — **writes**  |
| `sona_milestone_reorder`     | `project`, `milestones` (every one, in order), `dry_run?`              | `sona work milestone-reorder` — **writes** |
| `sona_milestone_delete`      | `project`, `milestone`                                                 | `sona work milestone-delete` — **writes** |
| `sona_cycle_list`            | `cycle?`                                                               | `sona work cycles` or `sona work cycle`  |
| `sona_cycle_save`            | settings (`weeks?`, `start_weekday?`, `cooldown_weeks?`, `upcoming?`), or `cycle` + (`name?`, `start?`, `end?`), or `start_today` + `cycle?`, or `cycle` + `rollover`; `dry_run?` | `sona work cycle-settings`, `cycle-save`, `cycle-start-today` or `cycle-rollover` — **writes** |
| `sona_person_list`           | `query?`                                                               | `sona work people [--query …]`           |
| `sona_person_save`           | `name`, `person?`, `aliases?`, `role?`, `org?`, `email?`, `notes?`     | `sona work person-save` — **writes**     |
| `sona_person_merge`          | `person`, `into`                                                       | `sona work person-merge` — **writes**    |
| `sona_meeting_list`          | `since?`, `limit?`                                                     | `sona work meetings`                     |
| `sona_meeting_get`           | `meeting_id`, `transcript?`; answers each speaker's id under `meeting_speakers` | `sona work meeting` |

A save makes the thing when nothing goes by that name and changes it when
something does; `project`, `milestone` or `person` renames that one to `name`.
Fields left out stay as they are; `null` clears one. Every write takes
`operation_id` and needs both grants and **Agents' changes** on Apply: while
agents' changes wait for the user they are refused, never proposed. Project
saves, the bin, updates, milestone order and cycle writes also take
`dry_run`; milestone and person saves, archiving, milestone deletes and
merges do not.

`sona_project_detail` reads a project with its latest update, whether an
update is due (`update_missing`), its progress (done counts 100, started 25,
over scope), weekly history, the predicted finish with optimistic and
pessimistic days, and each milestone's progress with `next_open` on the first
not complete. `sona_project_update_save` posts an update with a `health`
(`on-track`, `at-risk`, `off-track`) and a Markdown `body`, or with
`draft:true` Sona writes the body from what moved since the last update;
`draft:true` with `dry_run:true` shows that draft without posting. With `id`
it edits an update the agent posted. `sona_project_delete` puts a project in
the bin, where it stays 30 days before Sona deletes it with its milestones,
links, labels and updates; its tasks stay, in no project. `restore:true` takes
it back out, and `sona_project_list` with `trashed:true` lists the bin.
`sona_project_save` takes six statuses (`backlog`, `planned`, `active`,
`paused`, `completed`, `canceled`), `links` as `TITLE=URL` or bare URLs, and
a resolution (`day`, `month`, `quarter`, `half-year`, `year`) for its start
and target days. `sona_cycle_save` does one kind of change per call; settings
apply to every cycle and take no `cycle`.

Work raised in a meeting kept on this Mac never appears. While the
conversations can't be opened, all work from meetings is left out and the
answer carries `conversations_available:false`; a write naming such a task
answers `not_found`.

## What comes back

One JSON object per call, as one text block. Every payload carries a
`schema_version`. Calendar occurrences from `sona_upcoming` have no `sona://`
address. Project rows carry ids for `sona_project_get`, not a new app route. A
plan from `sona_plans` links to the meeting it was heard in, and a bookmark
from `sona_bookmarks` to the moment it marks (`?atMs=`).
Meeting, loop, person, dictation and note rows carry addresses the app can open:
`sona://meeting/<uuid>`, `sona://loop/<id>`, `sona://person/<uuid>`,
`sona://dictation/<id>`, `sona://note/<uuid>`.

An empty array is two different answers, so the three reads that can hide a
degraded source carry `reason` beside their rows. `null` means every source
behind the question was read and the rows are all there are.

| `reason`               | Means                                                                                                                                                     |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `no_rows`              | Everything was read and nothing matched. Asking again the same way will not change that.                                                                  |
| `no_searchable_tokens` | The query held no word to match.                                                                                                                          |
| `semantic_unavailable` | The recall model is not on this machine, so only literal word matching ran. Rows worded differently than the question are missing and cannot be reported. |
| `filtered_out`         | The corpus holds loops; `status` or `side` excluded every one of them.                                                                                    |
| `awaiting_continuity`  | A meeting's continuity pass has not succeeded yet, so its ledger rows are not reportable. Ask again later.                                                |
| `people_index_empty`   | Diarization has not named anybody yet, so no name can match.                                                                                              |

Each read produces only its own values: `sona_search` produces
`no_searchable_tokens`, `semantic_unavailable` and `no_rows`;
`sona_action_items` produces `awaiting_continuity`, `filtered_out` and
`no_rows`; `sona_people` produces `people_index_empty` and `no_rows`.

## Refusals

Errors are MCP errors. The message leads with Sona's own token — `sona
not_found: No loop … in this corpus.` — and `data.code` carries the same token
for a caller that reads structured error data:

| `data.code`        | Means                                                                 |
| ------------------ | --------------------------------------------------------------------- |
| `consent_required` | External access is off. `data.settingsPath` names the row to turn on. |
| `unavailable`      | The corpus is not open — usually a locked login keychain — or a task write needs a person while conversations are closed. |
| `invalid_request`  | A bad id, date or limit.                                              |
| `not_found`        | The id parsed but names nothing in this corpus.                       |
| `not_installed`    | No Sona binary where this server looked.                              |
| `timed_out`        | Sona did not answer in 30s.                                           |
| `failed`           | Sona reached the corpus and the read did not finish.                  |

## Tests

```bash
bun test          # input boundaries, CLI failures, MCP error handling, tool tiers
bunx tsc --noEmit # type checking
```

Both run from the repo root as `bun run test:mcp` and `bun run typecheck:mcp`,
which install this package first; CI runs them in `code-quality.yml`.
