---
name: sona
description: Read Sona conversations, people catch-up, sourced evidence changes, raw and corrected words, relevant notes, saved note differences, corpus statistics and open loops, and work Sona's task tracker, through the local sona CLI or sona-mcp. Use when preparing to talk to someone, checking what changed, retrieving note context, finding what was said, closing a loop, or picking up, starting and finishing tasks.
---

# Sona

Sona keeps conversations, dictations and editable notes locally in encrypted
storage. CLI reads do not send source material to a provider.

## Consent and writes

Every request requires **Settings > Agents > External access**, off on install.
`--loop-resolve` and every `sona work` verb that changes something also
require **Settings > Agents > External mutations**, a separate grant that is
also off on install. A `--dry-run` preview needs only External access. A
mutation grant alone does not permit reads.

A denied request exits `1` and prints a JSON refusal on stderr:

```json
{
  "schema_version": 6,
  "error": "consent_required",
  "message": "…",
  "settings_path": "…"
}
```

Tell the user the named setting. A refusal is not permission to change settings,
read the database directly, or try another transport.

Notes are readable by approved local CLI/MCP agents. They are not implicit
context for the remote panel. Note edits, links, restores and synthesis remain
native app actions; this CLI and MCP server expose no note-write tool. Do not
route note synthesis through another command to bypass mutation or source-transfer
consent. The external writes are loop resolution, the task tracker's verbs,
saving a shared-page draft and removing a shared page.
The approved MCP client receives private source data. These commands do not call
a model provider. The external-access grant is not permission to send records
to another provider, attach them to an unrelated remote request, or expand a
limited search into an unrequested corpus dump.

## Choose a read

One verb per invocation. JSON remains the default, including when stdout is a
terminal. For a person reading a result, append `--format text`. Text output has
no ANSI colors and escapes terminal control characters. `--format json` is
explicit JSON. Existing `--json` uses remain supported.

| Flag                                                                 | Answer                                                                            |
| -------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| `--query <TEXT> [--scope S] [--limit N]`                             | Search `all`, `meetings`, `dictations`, `people`, `loops` or `notes`              |
| `--meetings [--last N \| --from D --to D]`                           | Retained meetings, newest first; dates are local `YYYY-MM-DD`                     |
| `--meeting <ID>`                                                     | Summary, processing state, typed notes and loops                                  |
| `--transcript <ID>`                                                  | Speaker-labeled effective transcript, raw/corrected words and source revisions     |
| `--loops [--status open\|done] [--mine\|--waiting] [--after CURSOR]` | Loops and commitments                                                             |
| `--people <NAME>`                                                    | A person by name, alias or calendar address                                       |
| `--person-catch-up <PERSON_ID> [--since-utc-ms N]`                    | Shared person-memory snapshot and sourced catch-up selections                      |
| `--person-changes <PERSON_ID> --since-utc-ms N`                       | Same snapshot with paired earlier/later evidence and source links                  |
| `--person-stats <PERSON_ID>`                                         | The numbers the person page leads with: meetings, time together, last met, loops   |
| `--events [--after CURSOR]`                                          | Receipts and workflow runs, newest first                                          |
| `--upcoming [--limit N]`                                             | Today and the next seven local days of calendar                                   |
| `--plans [--limit N]`                                                | Plans to meet heard in meetings, newest first, each with its quote and status     |
| `--bookmarks [--limit N]`                                            | Bookmarks pressed while recording, newest meeting first, each with a moment link  |
| `--roster <ID>`                                                      | The names a meeting's call app listed, as the call showed them                     |
| `--place <ID> [--limit N]`                                           | Where a meeting was recorded and the other recordings within 100 m                 |
| `--notes [--limit N]`                                                | Notes, most recently updated first; pin state does not change this ordering       |
| `--note <NOTE_ID>`                                                   | A note with source links and related notes                                        |
| `--note-context <TEXT> [--limit N]`                                  | Current matching note bodies, matching text and resolved source references         |
| `--note-versions <NOTE_ID>`                                          | Saved version ids, kinds, times and sizes, newest first                           |
| `--note-diff <NOTE_ID> --from-version N [--to-version N]`            | Exact saved-body changes; destination defaults to newest saved version            |
| `--stats`                                                            | Content-free statistics over retained records, with availability for each dataset |
| `--export-meetings <DIR> [--export-meeting <ID>]...`                 | Readable copies on disk: lane audio WAVs, `meeting.json`, `speakers.json`; exits `1` if any meeting failed |
| `--loop-resolve <LOOP_ID>`                                           | **Writes.** Mark one loop done and return its receipt                             |

Paged reads default to 25 rows and cap at 100. `--meetings` also accepts `--last`
instead of `--limit`. `--from` and `--to` must appear together. A modifier that
belongs to another verb is refused rather than ignored.

meeting exports will include saved speaker labels and voice evidence from the
selected meetings only. they will exclude full saved voice profiles, your saved
voiceprint, and new identity guesses from other meetings. recordings marked “keep
local” will stay excluded.

Exit codes: `0` answered, `2` bad input, `1` consent refusal, unavailable storage,
missing record, failed operation or rejected loop write. JSON success goes to
stdout; JSON refusals go to stderr. A loop write rejected by its revision fence
still returns its receipt on stdout and exits `1`.

## Tasks

`sona work <verb>` is Sona's task tracker. It prints text; add `--json` for one
JSON value. Name a task by its handle (`S-12`), its number or its id. Run
`sona work --help` for every verb and `sona work <verb> --help` for its flags.

The loop for picking up work:

```bash
sona work ready --json                 # Todo tasks nothing open blocks
sona work show S-12 --json             # its meeting quote, links and history
sona work claim S-12 --agent "Claude Code"   # In progress, with you on it
sona work done S-12 --evidence https://github.com/org/repo/pull/7
```

Claim only work the user asked you to take, and say who you are with
`--agent` ("Claude Code", "Codex", or the name you go by). A claim keeps the
task's owner and shows you as the one doing it; claiming a task you already
work changes nothing. A refused claim means another agent or the user is on
it: pick another, do not force it with `update --status in-progress`. `done`
also closes the meeting commitment the task came from. A link given to
`--evidence` is kept with the task as evidence; any other line goes into its
details. `--due overdue,today,next:14` narrows `list` and `ready` to due
windows. `--repeat "every monday"` on `create` or `update` keeps how often a
task comes back (`none` stops it); finishing it does not make the next one.
`snooze S-12 --until 2026-12-01` keeps a task out of lists and `ready` until
that day; `--until none` wakes it. List items carry `focus` (one of today's
picks), `snoozed_until_ms` and `follow_up_at_ms` (when to ask whoever owes it).

Triage holds work Sona heard in meetings. Read the quote with `show` before
deciding: `accept` moves items to Todo with Sona's suggested owner, due day and
project (`--ignore-suggestions` keeps none), `decline` cancels them and sets
their commitments aside, `duplicate S-14 --of S-12` folds one into another.

`ask "what did Dana promise about pricing"` answers with the lines that say
it, numbered, from meetings, what they left open and dictations; `--take 1,3`
makes Triage tasks of those lines, each keeping its quote. `say` changes tasks
from plain words: `say "push everything not urgent to monday" --dry-run`
shows what would move, the same words without `--dry-run` move it. Without a
who it means the user's own open work. `watch pricing` tells the user when a
meeting or dictation says the word; `watches` lists them, `unwatch` stops one.

Every write takes `--operation-id`. Left out, Sona derives one from the request
and what it read, so running a command again never changes anything twice: a
create repeated the same day answers with the first task and `replayed:true`,
and a change already made lists its tasks under `skipped`. Pass a new id to
create a twin on purpose. Preview `update`, `done`, `accept`, `decline`,
`archive` and `restore` with `--dry-run`. Writes are filed as the agent
`--agent` names ("Agent" when left out), never as the user's own.

Unless the user sets **Settings > Agents > Agents' changes** to Apply, a
change to a task's fields waits for them: `update` and `done` answer with the
change under `proposed` (from, to, your name), and the user accepts or rejects
it in Sona. Moving a task you work between Backlog, Todo and In progress
applies at once. A task you create lands in Triage, and triage decisions,
duplicates, archiving and label changes are refused. Links between tasks
always apply.

Work raised in a meeting kept on this Mac never appears. While the
conversations can't be opened, every answer leaves out all work from meetings
and says `conversations_available:false`; choosing a person as owner then
refuses with `unavailable`.

## Shared meeting pages

Sona can publish one meeting's notes as an encrypted page on the user's own
site. Publishing needs the user's click on Share in Sona, or `sona share
--confirm` run at a terminal where the user then types `share` and presses
Return. You cannot publish: `share` without `--confirm` refuses with
`confirmation_required`, `--confirm` from a process without a terminal
refuses with `confirmation_requires_terminal`, and any answer but `share`
refuses with `confirmation_required`. Meetings kept on this Mac are never
shared. Prepare the page instead and tell the user it is waiting in Sona.

```bash
sona share-preview <MEETING> --json                      # the exact page that would go out
sona share-preview <MEETING> --include title,summary,action_items \
  --speakers numbered --expires 7d --save --agent "Claude Code" --json
sona shares --json                                       # every link, its contents and state
sona unshare <SLUG_OR_URL> --dry-run --json              # which share would be removed
sona unshare <SLUG_OR_URL> --json                        # remove it from the site
```

Sections: `title`, `summary`, `action_items`, `decisions`, `notes`,
`transcript`, `audio`; the defaults leave out notes, transcript and audio.
`--speakers numbered` replaces speaker names with Speaker 1, Speaker 2 in the
transcript; names written inside the summary, notes or owners stay. `--save`
keeps the draft for the user, shown as prepared by your `--agent` name, and
needs External mutations, as does `unshare`. A preview without `--save`,
`shares` and `unshare --dry-run` need External access only. Share only what
the user asked to share, and never paste a share link anywhere they did not
ask for it: the link carries the page's key.

## Before talking to someone

Look up the person first. Names and aliases find candidate profiles; they do not
prove identity. Use the exact returned person id and disambiguate candidates
before attributing speech.

```bash
sona --people "Fenil"
sona --person-catch-up <returned-person-uuid> --format text
sona --person-changes <returned-person-uuid> --since-utc-ms <utc-milliseconds>
```

Both person commands return `schema_version`, `view` (`catch_up` or `changes`),
`link` and the typed native `snapshot`. The snapshot keeps its own schema
version; do not substitute the external envelope version for it. It includes:

- `conversations`: source records with real person-link confidence, source and
  transcript availability.
- `contributions`: attributed passages, attribution kind and surrounding speech
  when available. Each passage separates `raw_text`, nullable `corrected_text`
  and nullable `edit_revision`; `source.revision` identifies the raw transcript.
- `notes`: linked note bodies and whether they include AI synthesis.
- `commitments`: ledger interpretations, status, source passages and `relation`
  (`explicit_owner`, `attributed_speech`, `shared_conversation`). Only explicit
  ownership names who owes a promise. Speech attribution or a shared conversation
  does not establish ownership. An interpretation is not a direct quote.
- `threads`: shared subject groupings with source evidence and interpretation.
- `changes`: paired `earlier` and `later` sides, each with `source`, `text` and
  `includes_ai_interpretation`.
- `catch_up`: the last confirmed conversation and selected contribution,
  commitment, note and change ids referencing entries in the same snapshot.
- `limitations`: missing, unavailable or incomplete evidence.

Source records include nullable `transcript_available` and `audio_retained`.
False means that source material is unavailable. Null does not establish
availability. A source link can still open the conversation or written takeaway;
it is not a promise that its old transcript or recording can be recovered.

Person snapshots are not paged. They use the same person-memory owner as the
native page; `--limit` is refused rather than silently ignored. Catch-up text
shows the selected evidence, not the whole timeline. JSON retains the snapshot.

`--since-utc-ms` is UTC milliseconds since the Unix epoch. It is optional for
catch-up and required for changes. It is a comparison boundary, not a blanket
filter that removes all earlier context from the snapshot.

Cite both sides when explaining a change. Describe what the evidence shows:
“the earlier note says thursday; the later note says friday,” not “fenil changed
his mind” unless an attributed source actually says so. Subject wording differences
are not automatic contradictions or changed decisions. Keep both sources visible
when they disagree.

An empty `changes` array means no comparable retained evidence differs. Earlier
versions may be missing or merged, and unknown speakers may remain unattributed.
Do not turn that into “nothing changed.” Do not guess identity, fill a missing
baseline, or silently prefer the newest statement.

## Captured and corrected words

`--transcript` keeps the existing line `text` as the effective reading. Each
retained line also contains `raw_text`, nullable `corrected_text`, `segment_id`,
`transcript_revision_id`, nullable `edit_revision`, and a source `link`.
`raw_text` is the stored recognition output, not verified speech. A correction
does not overwrite it and is not proof of what the audio said.

```bash
sona --transcript <meeting-uuid>
sona --transcript <meeting-uuid> --format text
```

Text output labels captured and corrected wording when a human replacement is
present. Removed lines stay excluded in both formats; raw access is not a route
around retention or removal. Offsets remain milliseconds from the recording.
Use the source link and listen when the wording matters.

## Notes and changes

Start with `--notes` or `--query <TEXT> --scope notes`, then read the note id.
A note result is:

```text
{
  "schema_version": 6,
  "memory": {
    "note": {"id": str, "title": str, "body": str, "pinned": bool,
             "created_at_ms": int, "updated_at_ms": int},
    "sources": [{"kind": "note"|"meeting"|"person", "id": str,
                 "title": str, "link": str, "revision": int|null,
                 "available": bool}],
    "related_notes": [note],
    "passages": [{"meeting_id": str, "segment_id": str,
                  "start_ms": int, "end_ms": int, "person_id": str|null,
                  "wording": "raw"|"corrected", "link": str}],
    "interpretation": bool,
    "connections": [{"note_id": str, "reasons": [str]}]
  },
  "link": "sona://note/<uuid>"
}
```

An unavailable or deleted source has `available:false`, a generic title and an
empty link. Its old snapshot is not exported. Do not infer its contents from a
remaining source id. A deleted note cannot be read or diffed; its saved versions
are removed with it. Related notes are existing notes, not generated assertions.

For a focused question, request relevant note context instead of dumping all
notes:

```bash
sona --note-context "release plan" --limit 3 --format text
```

The JSON has `query`, `entries`, `has_more` and `reason`. Each entry includes the
current `note`, `matched_text`, resolved `sources`, saved `passages`,
`interpretation`, explained `connections` and the note `link`. It uses
the same local note search and page size as `--query --scope notes`. It returns
no generated answer and does not include unrelated or related-note bodies
outside that result page. `has_more:true` means more matching notes exist.
Respect the user's scope before broadening a request.

Quote the note as a saved note, not as transcript evidence. Follow its available
source references for direct speech. Unavailable references cannot establish
what was said. Do not invent a relationship because two notes use similar words.

`interpretation:true` identifies AI interpretation; do not quote that note as
captured speech. Saved passages identify the selected raw or corrected wording,
person link when explicitly supplied and the recording moment. They contain
references, not another exported copy of source speech. Unavailable meeting
passage references are removed by the note owner. Connection reasons explain
actual links or shared source records; they do not establish a personal
relationship, agreement or promise.

`--note-versions` returns `note_id`, `link`, and `entries` containing `id`,
`kind` (`edit`, `restore` or `synthesis`), `saved_at_ms`, `words` and `bytes`.
These are saved states, not a keystroke audit: edits within ten minutes may merge
into the newest version, and only the newest 50 versions are retained. Read the
version list again if a version is no longer available.

```bash
sona --note-versions <note-uuid>
sona --note-diff <note-uuid> --from-version 12 --to-version 18 --format text
```

A JSON diff identifies both version ids and save times. `changed:false` means
identical bodies. Otherwise, `old_start`, `old_lines`, `new_start`, `new_lines`
and `lines` describe one exact replacement hunk with up to three shared context
lines on either side. Each line has `kind:context|remove|add` and `text`, which
preserves its original line ending. `removed_lines` and `added_lines` count the
replaced region, not a minimal edit distance. Separate changes can therefore
include unchanged lines between them. A final newline change is significant.
The text format uses ordinary unified-diff prefixes and marks missing final
newlines. Reading a diff never restores or writes a note.
`from_source` and `to_source` identify the two saved versions with `version_id`,
`saved_at_ms`, `kind` and a version-scoped note `link`. Cite both when discussing
the change. Version retention still applies when a link is opened later.

## Statistics

`--stats` returns `measured_at_utc_ms`, `population:"retained_records"`,
`complete`, and three datasets: `meetings`, `dictations`, `notes`.

Each dataset is either `{"status":"available","totals":{…}}` or
`{"status":"unavailable","error":{…}}`. An unavailable dataset has no totals.
A partial report is a successful read with `complete:false`; it is not evidence
of an empty corpus. Zero is meaningful only inside available totals.

- Meetings: `meetings`, `verified_captured_duration_ms`, `transcript_segments`,
  `generated_action_items`. Verified audio merges overlapping capture sources.
- Dictations: `entries`, `total_duration_ms`, `total_words`, `by_source` with
  the same totals and a nullable `source_kind`. Null means provenance was not
  recorded; do not label old records microphone captures.
- Notes: `notes`, `pinned`, `saved_versions`, `characters`. Character counts
  cover current bodies; versions are counted separately.

These are separate reads, not a single cross-database transaction. They describe
records retained now, not all conversations ever captured. The report does not
read the calendar or estimate missing recordings.
`sources` contains a separate availability result for each dataset. An available
source result has `method`, `sample_links` and `has_more`. At most 25 links are
returned per dataset. They sample newest meetings, newest dictation ids, or most
recently updated notes; they are not every record used to compute totals.
Counts and samples are separate reads, so do not claim a shared transaction.
`complete:false` also covers an unavailable source sample. A missing sample
does not turn otherwise available totals into zero.

## Reading other results

Every current payload carries `schema_version:6`. Existing verb fields remain
available. Search rows contain `kind`, `id`, `title`, `snippet`, `when_utc_ms` and
`link`; `kind` can now be `note` as well as `meeting`, `dictation`, `person` or
`loop`. Dictation ids are digit strings, not meeting uuids.

`--project` carries `pulse`, the numbers the project page leads with: `done`,
`open`, `overdue`, a nullable `next_due_at_ms` (today or later only),
`meetings_last_7_days`, `owners` (`person_id`, `open_tasks`, most first) and
per-milestone `meetings` with `last_meeting_at_ms`. Canceled tasks count
nowhere. Work due today is open, not overdue, until its local day ends. Owners
are person ids only; read the person to learn a name.

`--person-stats` carries the numbers the person page leads with. `meetings` has
`confirmed` and `suggested`; a suggested link counts nowhere else.
`recorded_ms`, `last_met_utc_ms`, `talk_share_avg_permille` (their average
share of the talking), `ledger_outcomes` (`landed`, `open`, `dropped`) and
`usual_gap_days` are null when the corpus cannot supply them, never zero.
`open_loops` counts open and carried loops as `i_owe` and `owed_to_me`.
`weekly_counts` has 52 confirmed-meeting counts, oldest first, by local week
starting Monday; the last is the week holding `read_at_utc_ms`. A week
before the corpus's first meeting is null, since Sona was not recording then.
`usual_gap_days` is the median gap between distinct meeting days and needs at
least four. No transcript text, titles or names; read the person for those.

- Meetings, people, upcoming events and notes return `has_more`. Narrow a
  request or raise its limit when there are more rows.
- Loops return `next_cursor` plus `has_more`. Resume with `--after` and the
  same status and side filters. Events also resume using their `next_cursor`.
- Search's cursor is informational; the CLI has no search-cursor input. Narrow
  the words or raise the limit instead.
- `reason:null` means no source limitation was reported. `no_rows` means no
  records matched. `semantic_unavailable` means only literal matches ran.
  `no_searchable_tokens` means the query held no words to search.
- Loops may say `awaiting_continuity`: some rows are withheld until meetings
  have been connected. `filtered_out` means filters excluded all available
  loops. People may say `people_index_empty`; no named person is indexed yet.

An empty meeting summary is not an empty capture. Read `processing_status`:
`pending`, `running`, `succeeded`, `cancelled`, or `failed` with a reason and
possibly a cause. The meeting's `phase` tracks its review lifecycle instead.
Read `--transcript` for captured words when generated output is absent.

`--upcoming` names `calendar_access_subject:"responsible_process"`. On macOS
this is often the terminal, not the installed app. An empty list under access
other than `authorized` means this caller could not read the calendar, not that
the user has a free week. Calendar rows use `event_key` and have no `link`.

Loop resolution reads the current revision before writing. A replay can return
the existing receipt without another revision. After a reopen, a later resolve
can commit again. A rejected receipt means re-read the loop before deciding
again. External receipts record `actor:"external"`; do not describe them as the
user's own click.

## MCP

`tools/sona-mcp` is a stateless stdio server over the same CLI flags. It runs no
separate query or mutation logic. Register it with:

```bash
claude mcp add sona -- bun /absolute/path/to/sona/tools/sona-mcp/src/index.ts
```

| Tool                              | Arguments                                                    |
| --------------------------------- | ------------------------------------------------------------ |
| `sona_search`                     | `query`, `scope?`, `limit?`                                  |
| `sona_meetings`                   | `last?` or `from` + `to`                                     |
| `sona_meeting`, `sona_transcript` | `meeting_id`                                                 |
| `sona_action_items`               | `status?`, `side?` (`mine` or `waiting`), `after?`, `limit?` |
| `sona_people`                     | `name`, `limit?`                                             |
| `sona_person_catch_up`            | `person_id`, `since_utc_ms?`                                 |
| `sona_person_changes`             | `person_id`, `since_utc_ms`                                  |
| `sona_upcoming`, `sona_plans`, `sona_bookmarks`, `sona_notes` | `limit?`                               |
| `sona_roster`                     | `meeting_id`                                                 |
| `sona_place`                      | `meeting_id`, `limit?`                                       |
| `sona_note`, `sona_note_versions` | `note_id`                                                    |
| `sona_note_context`               | `query`, `limit?`                                            |
| `sona_note_diff`                  | `note_id`, `from_version`, `to_version?`                     |
| `sona_stats`                      | none                                                         |
| `sona_events`                     | `after?`, `limit?`                                           |
| `sona_loop_resolve`               | `loop_id` — writes                                           |
| `sona_work_list`                  | filters, `ready?`, `limit?`                                  |
| `sona_work_get`                   | `task`                                                       |
| `sona_work_counts`                | `limit?`                                                     |
| `sona_work_create`                | `title`, fields — writes                                     |
| `sona_work_update`                | `tasks`, fields, `dry_run?` — writes                         |
| `sona_work_claim`                 | `task` — writes                                              |
| `sona_work_done`                  | `tasks`, `evidence?`, `dry_run?` — writes                    |
| `sona_work_triage`                | `tasks`, `decision`, `of?`, `dry_run?` — writes              |
| `sona_work_ask`                   | `question`                                                   |
| `sona_work_take`                  | `question`, `lines` — writes                                 |
| `sona_work_say`                   | `words`, `dry_run?` — writes                                 |

Archiving, links, labels, views and watches are opt-in tools: set
`SONA_MCP_TOOLS=all`, or name the ones to add, when registering the server.
Task export is CLI-only.

The result is the CLI JSON in one text block. Refusal messages lead with Sona's
code, and `data.code` preserves it. Consent failures also carry
`data.settingsPath`. `consent_required` maps to MCP InvalidRequest;
`invalid_request` and `not_found` to InvalidParams; unavailable storage and
execution failures to InternalError.
Tools also declare `readOnlyHint:true`, except loop resolution and the task
writes declare `false`.
The hints describe behavior; the CLI consent gate still makes the permission
decision before any corpus read.

## Evidence rules

Cite the returned `sona://` link. Meeting, loop, person, dictation and note links
open those records in Sona. Events can have a null link; calendar occurrences
and write receipts have none.

Quote stored words as evidence, not as certain ground truth. Recognition errors
can remain in raw transcripts. Corrections are a separate reading of the source,
not proof of what was said. Do not invent a person's identity, commitment, date,
location or source when the record does not name one.

Read before writing. Use loop ids returned by a read. There is no external note
write or synthesis tool. Preserve original source records and their links rather
than presenting a synthesis as a new transcript.
