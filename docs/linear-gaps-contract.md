# Organizer gaps: CLI and MCP contract

The verbs that close the top of the "missing" list in
`docs/cli-ui-parity/README.md`: hand-offs, the day's focus, stale work,
person briefs, meeting loops, untracked meeting actions, notes, attachment
titles, captured links and the Triage filters. Every one runs the same core
function the app's button runs; nothing here keeps its own copy of a rule.

## Shared rules

- Every verb takes `--json`, `--agent NAME` and `--operation-id UUID`; every
  write takes `--dry-run`. A read or a dry run needs External access; a write
  also needs External mutations.
- Writes that are not a task field change (`delegate`, `focus`, `keep`,
  `talking-point`, `talking-point-done`, `loop-reopen`, `loop-owner`,
  `action-track`, `action-dismiss`, `action-restore`, `note-*`) are refused
  with `invalid_request` ("… is theirs to do in Sona") unless Settings >
  Agents > Agents' changes is on Apply.
  Nothing here is proposed for later. `attachment-rename` and
  `create --source-url` follow `attach` and `create`: a new task lands in
  Triage while changes wait for the user.
- A dry run checks the request (the task, note, person or loop must exist and
  the text must not be empty) and writes nothing, not even a ledger row.
- A change already in place answers `replayed:true` and changes nothing.
  `delegate` to the agent already on it still files a ledger row under its
  fresh id.
- Task history rows are credited to `--agent` ("Agent" when left out).

## Verbs

| verb | core function | answer |
|---|---|---|
| `work delegate TASK --to AGENT` / `--take-back` | `agents::work_delegate` | `organize`: `action` `delegated`/`took_back`, `tasks`, `doer {agent, since_ms}`, `prompt` |
| `work day [DAY]` | `items::day` | `day`: `day`, `focus[]`, `planned_at_ms` |
| `work focus TASK... [--day]`, `--clear` | `work::work_day_plan` | `day` with `operation_id`, `replayed` |
| `work keep TASK...` | `work::work_keep` | `organize`: `kept`, `tasks` |
| `work list --stale` | `work_list` | open tasks untouched for 30 days, by urgency |
| `work list --source meeting,dictation,agent,person` | `work_list` | tasks whose source is one of those |
| `work snooze TASK --until later-today`, `work update --snooze-until later-today` | `work_bulk_update` | snoozed to the next whole hour three hours from now; refused when that passes midnight |
| `work brief PERSON` | `follow::person_brief` | `brief`: `title`, `people[{person_id, name, you_owe[], they_owe[], talking_points[]}]`, `last_time {meeting_id, title, at_ms, decisions[]}` |
| `work talking-point PERSON TEXT` | `follow::work_talking_point_add` | `organize`: `talking_point_added`, `person_id`, `talking_point {id, text, created_at_ms}` |
| `work talking-point-done ID [--undo]` | `follow::work_talking_point_done` | `talking_point_done` / `talking_point_reopened` |
| `work loop-reopen LOOP_ID` | `MeetingSessionManager::loop_reopen` | `loop_reopened`, `meeting_loop {loop_id, text, status, owner_person_id, owner}` |
| `work loop-owner LOOP_ID --owner PERSON\|none` | `MeetingSessionManager::loop_assign` | `loop_owner_set`, `meeting_loop` |
| `work action-track LOOP_ID --project P` | `untracked::planning_track_action` | `action_tracked`, `tasks` |
| `work action-dismiss LOOP_ID` / `action-restore` | `untracked::planning_dismiss_action` / `planning_restore_action` | `action_dismissed` / `action_restored` |
| `work note-new TEXT` | `scratchpad_create` | `note`: `action` `created`, `note {id, title, body, pinned, created_at_ms, updated_at_ms}`; a dry run gives `would_write` |
| `work note-edit ID --body TEXT` | `scratchpad_update_body` | `edited`; the old body stays as a version |
| `work note-pin ID [--unpin]` | `scratchpad_set_pinned` | `pinned` / `unpinned` |
| `work note-link ID --meeting\|--person\|--note ID` | `scratchpad_link` | `linked`, `linked {kind, id, title}` |
| `work attachment-rename TASK ID --title T` | `work::rename_attachment_as` | `attachment_renamed`, `attachment` |
| `work create TITLE --source-url URL [--source-title T]` | `agents::create_as`, then `capture::attach` | the created task; the link is kept as its source (`work show` lists it under `evidence`) |

The event brief (`work_brief` for a calendar event) stays in the app: it
needs the calendar runtime the CLI does not start.

## Replay

- `delegate`, `focus`, `keep`, `talking-point*`, `action-*` and
  `attachment-rename` run through the tracker's ledger (`planning_operations`).
  The same `--operation-id` answers the first result with `replayed:true`.
  Left out, `keep` and `talking-point` derive one per day and request; the
  others take a fresh id and replay because the state already matches.
- `loop-reopen` and `loop-owner` derive an id from the verb, the loop and its
  revision, and answer `replayed:true` when the loop is already open or
  already has that owner.
- `note-*` keep their own ledger rows (`cli_note_create`, `cli_note_edit`,
  `cli_note_pin`, `cli_note_link`). Without an id, pinning a pinned note or
  linking a linked source answers `replayed:true` with `operation_id:null`.
- `create --source-url` derives its id from the task fields, the day and the
  link; running it twice makes one task and one source link.

## Known gaps

- Loop changes are filed in the meeting's history as the user's: the
  meeting store has no agent author.
- A note ledger row is written after the note, not in the same transaction.
  A crash between the two lets one retry with the same id write the change
  again.
- Notes: delete, restore a version, unlink, subjects and synthesis are not
  in the CLI yet.

## Verified

Scratch HOME (`/private/var/tmp/sona-scratch-gaps`, `SONA_SCRATCH_RUN=1`,
both grants on, Agents' changes on Apply), debug core built from this tree,
script `smoke.sh` there, output `out.txt`:

- `create --source-url`: dry run writes no evidence row (0); two runs make
  one task (second `replayed:true`, same id) and one evidence link (1);
  history is `agent|Codex`; `--source-url "not a link"` exits 2 with
  `invalid_request`.
- `delegate`: dry run shows the doer; the run returns the hand-off prompt;
  the same request again answers `replayed:true`; `--take-back` works.
- `focus`: dry run leaves `planning_days` at 0; `--operation-id` run then
  rerun answers `replayed:true` with the same `planned_at_ms`; `day` reads
  it back; `--clear --day tomorrow` empties tomorrow.
- `list --stale` shows the task aged 40 days by sqlite; `keep` (derived id)
  then rerun answers `replayed:true`; `list --stale` is then empty.
  `list --source agent` lists the three agent tasks, `--source meeting`
  none.
- `snooze --until later-today` at 01:29 stored 05:00 the same day.
- `attachment-rename`: dry run, run, rerun `replayed:true`, blank title
  exits 2.
- `note-new` dry run leaves 0 notes; an id-pinned create run twice leaves
  one note (`replayed:true`); `note-edit` keeps one version; `note-pin`
  and `note-link` rerun answer `replayed:true`.
- MCP over stdio (`SONA_MCP_TOOLS=all`): `sona_work_keep` ran;
  `sona_scratchpad_set_pinned` with `dry_run:true` answered `unpinned` and
  the note stayed pinned (1).
- `tools/sona-mcp`: `bun test` 22 pass, 0 fail; `tsc --noEmit` exit 0;
  `oxlint src test` exit 0. `cargo check --lib --tests` and `cargo clippy
  --lib` clean in these files; anti-slop `ast-grep scan` exit 0.

Not verified: `brief`, `talking-point`, `talking-point-done` on a real
point, `loop-reopen`, `loop-owner` and `action-*`. They need the encrypted
conversation store, which a scratch HOME cannot open; there they refuse
with `unavailable` (exit 1), and an unknown talking point with
`not_found`. Check them against the real store with the app running.
