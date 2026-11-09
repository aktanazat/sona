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
| `sona_project`       | `project_id`                             | `sona --project <uuid>`                                                |
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

Pass a returned project id to `sona_project` for its saved description,
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
that writes.
The server passes private source data to its approved client. Neither Sona nor
this server calls a model provider for these reads. External access is not
permission for an agent to send those records to another provider.

## Task tools

These run `sona work <verb> --json`. A task is named by its handle (`S-12`),
its number or its id. Eight are published by default:

| Tool               | Arguments                                                        | Runs                                       |
| ------------------ | ---------------------------------------------------------------- | ------------------------------------------ |
| `sona_work_list`   | filters (`status?`, `side?`, `owner?`, `label?`, …), `ready?`    | `sona work list` or `sona work ready`      |
| `sona_work_get`    | `task`                                                           | `sona work show`                           |
| `sona_work_counts` | `limit?`                                                         | `sona work mine`                           |
| `sona_work_create` | `title`, fields (`priority?`, `owner?`, `due?`, `labels?`, …)    | `sona work create` — **writes**            |
| `sona_work_update` | `tasks`, fields, `add_labels?`, `remove_labels?`, `dry_run?`     | `sona work update` — **writes**            |
| `sona_work_claim`  | `task`                                                           | `sona work claim` — **writes**             |
| `sona_work_done`   | `tasks`, `evidence?`, `dry_run?`                                 | `sona work done` — **writes**              |
| `sona_work_triage` | `tasks`, `decision` (`accept`, `decline`, `duplicate`), `of?`    | `sona work accept`/`decline`/`duplicate` — **writes** |

Every write also takes `operation_id`. Left out, Sona derives one from the
request and what it read, so a repeated call never changes anything twice: a
create repeated the same day answers with the first task and `replayed:true`,
and a change already made lists its tasks under `skipped`. Pass a new id to
create a twin on purpose. Every write carries the client's name from the MCP
handshake, so Sona shows which agent made it.

`sona_work_list` with `ready:true` returns Todo tasks nothing open blocks.
`sona_work_claim` moves one Todo or Backlog task to In progress and shows the
agent on it; it refuses a task another agent works or one already in
progress, so two agents cannot both start it. The task's owner stays who it
was. `sona_work_done` closes the meeting commitment a task came from, and
`evidence` (a pull request link, say) becomes a line in its details.

Unless the user sets **Settings > Agents > Agents' changes** to Apply, a
change to a task's fields waits for them: the answer lists it under
`proposed`, and the user accepts or rejects it in Sona. A created task lands
in Triage, and triage, archiving and label changes are refused.

Ten more are opt-in: `sona_work_archive`, `sona_work_restore`,
`sona_work_relate`, `sona_work_unrelate`, `sona_work_labels`,
`sona_work_label_save`, `sona_work_label_delete`, `sona_work_views`,
`sona_work_view_save` and `sona_work_view_delete`. Publish them with
`SONA_MCP_TOOLS`: `all` for every tool, or a comma-separated list of names
beside the default ones. A name that matches no tool stops the server at
start.

```bash
claude mcp add sona --env SONA_MCP_TOOLS=sona_work_archive,sona_work_restore \
  -- bun /absolute/path/to/sona/tools/sona-mcp/src/index.ts
```

Work raised in a meeting kept on this Mac never appears. While the
conversations can't be opened, all work from meetings is left out and the
answer carries `conversations_available:false`; a write naming such a task
answers `not_found`.

## What comes back

One JSON object per call, as one text block. Every payload carries a
`schema_version`. Calendar occurrences from `sona_upcoming` have no `sona://`
address. Project rows carry ids for `sona_project`, not a new app route. A
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
