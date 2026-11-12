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
consent. The external writes are loop resolution and the task tracker's verbs.
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
| `--query <TEXT> [--scope S] [--limit N]`                             | Search `all`, `meetings`, `dictations`, `people`, `loops`, `notes` or `tasks`; a task matches its title, details or comments and is titled `S-12 …` |
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
sona work show S-12 --json             # its meeting quote, links, reminder, agent sessions, history and sona://task link
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
task comes back (`none` stops it); finishing it makes the next copy.
`snooze S-12 --until 2026-12-01` keeps a task out of lists and `ready` until
that day; `--until none` wakes it. List items carry `focus` (one of today's
picks), `snoozed_until_ms` and `follow_up_at_ms` (when to ask whoever owes it).

While you work a task, run a session on it so the user can watch and stop
you:

```bash
sona work session-start S-12 --agent Codex --json          # note .session.id
sona work session-plan ID --agent Codex --plan-json '[{"content":"Read the spec","status":"in_progress"}]'
sona work session-activity ID --agent Codex --type thought --body "Checking the deposit terms"
sona work session-activity ID --agent Codex --type action --action Searching --parameter "deposit" --result "2 matches"
sona work session-activity ID --agent Codex --type elicitation --body "Which deposit?" --option "10 percent" --option "20 percent"
sona work session-heartbeat ID --agent Codex --json        # stop_requested, unread_prompts
sona work session-activity ID --agent Codex --type response --body "Done: drafted with 10 percent."
```

Write within 10 seconds of starting and keep writing or heartbeating; after
30 minutes of silence the session shows as stale. Every answer carries
`stop_requested`, `unread_prompts` and an `instruction`: once the user asks
you to stop, only a `response` or an `error` is accepted, and either one ends
the session. A question with choices waits in `awaiting_input` until the user
answers; the answer arrives in `unread_prompts` with its `value`.
`sessions [TASK] [--open]` lists sessions; `list --delegate Codex` (or `any`,
`none`) lists the tasks delegated to an agent.

`save S-12 --patch-json '[{"field":"status","value":"in_progress"},{"field":"labels","op":"add","value":["bug"]},{"field":"description","op":"append","value":"Found it."}]' --expected-revision 7`
applies the steps in order, all or none: a bad step refuses the whole save
as `patch[n]`. Without a task, `save --patch-json '[{"field":"title","value":"Ship it"},…]'`
makes one. Steps `delegate`, `blocks`, `blocked_by`, `related` (op add or
remove), `duplicate_of`, `links` and `source` (a meeting id) are checked
first, then saved one by one after the fields; `--dry-run` shows them all.
`claim`, `duplicate`, `ask --take`, `watch` and `unwatch` take `--dry-run`
too. `changes --since CURSOR` follows every change to tasks, projects,
milestones, cycles, views, labels, project updates, notes, meetings Sona
took work from, comments and sessions in order, with `updated_from`,
`next_cursor` and `has_more`; a comment on a project, milestone, meeting or
note names it under `data.subject_kind` and `data.subject_id`, and a note's
text is not in the stream (read it with `note versions`). Anything tied to a
meeting kept on this Mac is left out. A cursor older than 30 days is refused
as expired, so read the tasks again and start from the newest.

Triage holds work Sona heard in meetings. Read the quote with `show` before
deciding: `accept` moves items to Todo with Sona's suggested owner, due day and
project (`--ignore-suggestions` keeps none), `decline` cancels them and sets
their commitments aside (`--reason "the venue prints them"` keeps why in each
task's history), `duplicate S-14 --of S-12` folds one into another.

Templates make the same task with its sub-tasks again: `template-save
"Weekly report" --title "Send the weekly report" --priority high --due-in 2
--repeat "every friday" --subtask "Gather the numbers" --subtask "Write it"`
saves one (saving a name that exists changes it; `--subtask ""` clears the
sub-tasks), `templates` lists them, `template-use "Weekly report" [--title]
[--due DAY]` makes the task and its sub-tasks, `template-delete` removes one.
`remind S-12 "tomorrow 9am"` sets when Sona reminds the user of a task (`none`
clears it); `reminders [--due]` lists them. `label-group bug Type` puts a
label in a group (`none` takes it out); `label-retire legacy` keeps it on its
tasks but out of pickers and new tags, `--restore` brings it back.
`label-scope bug tasks` keeps a label to tasks (`projects` to projects,
`all` both); narrowing one already on the other kind is refused.

`ask "what did Dana promise about pricing"` answers with the lines that say
it, numbered, from meetings, what they left open and dictations; `--take 1,3`
makes Triage tasks of those lines, each keeping its quote. `say` changes tasks
from plain words: `say "push everything not urgent to monday" --dry-run`
shows what would move, the same words without `--dry-run` move it. Without a
who it means the user's own open work. `watch pricing` tells the user when a
meeting or dictation says the word; `watches` lists them, `unwatch` stops one.

Projects and People pages. Keep the catalog the pages show from the terminal:

```bash
sona work projects [--all]                       # lead, status, target day, open and done counts
sona work project-save "Q1 launch" --lead me --members "me,Dana,Lee" --target 2026-03-31 --status active
sona work project-archive "Q1 launch" [--restore]
sona work milestones "Q1 launch"
sona work milestone-save "Q1 launch" "Beta" --target 2026-02-15
sona work milestone-delete "Q1 launch" "Beta"
sona work people [--query dana]                  # aliases, role, organization, open work
sona work person-save "Dana Reyes" --aliases "Dayna,Dana R" --role "Design lead" --org Acme
sona work person-merge Dayna --into "Dana Reyes" # tasks, quotes and speaker links move
sona work person-merge Dayna --into "Dana Reyes" --voice into   # both saved a voice: into, from or both
sona work person-merge Dayna --into "Dana Reyes" --dry-run      # the person as the merge would leave them
sona work person "Dana Reyes"                    # the whole page: summary, meetings, open items, projects led or joined, documents, contact, photo
sona work person-link "Dana Reyes" <MEETING_ID> [--confirm | --remove]
sona work person-split "Dana Reyes" --new "Dana Ortiz" --meetings <ID>,<ID> [--aliases A] [--emails E] [--documents ID]
sona work person-delete "Lee Park"               # with their links and saved voice; cannot be undone
sona work person-photo "Dana Reyes" --file ~/Pictures/dana.jpg   # or --remove
sona work person-contact-remove "Dana Reyes"
sona work first-met "Dana Reyes"                 # when, where, and who introduced you
sona work first-met-answer "Dana Reyes" yes      # or no
sona work companies
sona work company Acme                           # its people, meetings and open items
```

The people writes take `--dry-run`, `--operation-id` and `--agent`, and
need the Agents' changes setting on Apply. Meetings kept on this Mac stay
out of every answer: the person page drops them and its summary,
`person-link` and `person-split` answer `not_found` for them, and a person
in one is not deleted. `--voice into` keeps the voice of the person who
keeps everything, `from` keeps the folded person's, `both` combines them;
the first two delete one saved voice for good. Rewriting a person's summary
needs the app: the text engine runs there.

`project-save` and `person-save` make the thing when nothing goes by that
name and change it when something does; `--project` or `--person` with the
old name renames. `--aliases` replaces the whole list (a rename keeps the
old name as an alias). Every place that takes a person (`--owner`, `--lead`,
`--members`, triage suggestions, speaker names) matches the name or any
alias, so a misheard first name finds the right person. `--owner`, `--lead`
and `--members` also take `me` for the user and `nobody` for no one. These
writes need
both grants and the Agents' changes setting on Apply. A running Sona shows
them within a second or two.

```bash
sona work project-save "Q1 launch" --summary "Ship the beta to ten venues"   # none clears it
sona work project-statuses                       # status names inside the six categories
sona work project-status-save "In review" --category active [--status "Review"]   # make, rename or move one
sona work project-status-delete "In review"      # its projects keep the category
sona work project-save "Q1 launch" --status-name "In review"   # none clears the name
sona work project-save "Q1 launch" --patch-json '[{"field":"description","op":"insert_after","anchor":"## Scope","value":"Adds export."}]'
sona work projects --update-before 2026-11-01    # no update posted since that day
sona work project-history "Q1 launch" [--limit 50]   # title, status, lead, members, dates, milestones
sona work milestone "Q1 launch" "Beta"           # one milestone
sona work milestone-to-project "Q1 launch" "Beta" [--dry-run]   # its tasks move; the milestone goes
sona work project-relate "Beta launch" blocked-by "Venue pilot" [--dry-run]   # Beta waits for the pilot to end
sona work project-relate "Venue pilot" blocking "Beta launch"   # the same dependency, named from the other side
sona work project-unrelate "Beta launch" blocked-by "Venue pilot"
sona work project-dependencies ["Beta launch"]   # broken: the blocker ends after the blocked project starts
sona work projects --link broken                 # also blocked or blocking: projects on that side of a dependency
sona work project-template-save "Launch kit" --project "Q1 launch" [--dry-run]   # milestones kept as days after its start
sona work project-template-list   # a sub-task reads "under <task>"
sona work project-new "Q2 launch" --template "Launch kit" --start 2026-04-01 [--dry-run]   # milestones dated from the start
sona work project-template-delete "Launch kit"   # projects made from it stay
sona work initiative-new "Spring launch" --owner me --target 2027-03-31 [--dry-run]   # a goal above projects
sona work initiative-list                       # each with its projects and tasks done
sona work initiative-show "Spring launch"
sona work initiative-update "Spring launch" --status active [--name N] [--details T] [--owner PERSON] [--target none]
sona work initiative-add-project "Spring launch" "Q1 launch"   # a project sits in one initiative; this moves it
sona work initiative-remove-project "Spring launch" "Q1 launch"
sona work projects --initiative "Spring launch"   # only its projects; --initiative none for projects in none
sona work initiative-delete "Spring launch" [--dry-run]   # its projects stay, in no initiative
```

`project "Q1 launch"` lists `sources`, the notes, meetings and links linked
to the project. `--patch-json` edits `description` or `summary` with op
`replace`, `append`, `prepend`, `insert_before`, `insert_after` or
`replace_text`; the last three need `anchor` and refuse the save when it is
not there. `project-update-draft` and `project-update` without `--body` also
read the meetings linked to the project or its tasks since the last update,
never a meeting kept on this Mac.

The meetings feed is read-only: `sona work meetings --since 2026-11-01`
(a day or milliseconds) lists id, title, start, minutes, whether it was
processed, and who was in it; `sona work meeting <ID> [--transcript]` gives
the summary, commitments, each speaker with their id under
`meeting_speakers` (`needs_name:true` for the ones nobody has named), and the
transcript with speaker turns.

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

```bash
sona work delete S-4 [S-5…]          # to the bin; restore within 30 days, then gone
sona work restore S-4                # out of the bin as it was, or out of the archive
sona work bin                        # deleted tasks with days_left
sona work archive-list               # archived and deleted tasks
sona work description-versions S-4   # every description, newest first, current marked
sona work description-version <ID>
sona work description-restore <ID>  # proposed while changes wait for the user
sona work favorite S-4 | --project "Q1 launch" | --view "Open work" | --person Danny
sona work unfavorite …; sona work favorites   # sidebar order, folders with theirs
sona work favorite-folder-save --name Launch [--position 1]   # new folder
sona work favorite-folder-save --folder Launch --name Q1 | --position 2
sona work favorite-folder-delete Launch     # its favorites stay at the top
sona work favorite-move S-4 --into Launch | --top | --position 2
sona work subscribe S-4; sona work unsubscribe S-4; sona work subscriptions
sona work inbox [--unread] [--snoozed] [--archived]
sona work inbox-mark <ID…> --mark read|unread|archive|unarchive|snooze [--until tomorrow]
sona work inbox-mark --all --mark read      # every unread one
sona work inbox-mark --all --mark archive   # clears every read one
sona work digest [--dry-run]                # the weekly project digest over the last 7 days, posted to the inbox
sona work setting project-digest true|false; sona work setting project-digest-day monday
```

The inbox is the user's: status changes, comments, mentions (`@me` or their
name) and assignments that Sona or agents made on tasks they follow, any other
field an agent changed (`changed`, with `field`: due, priority, details,
owner, labels, reminder and so on), a reminder firing (`reminder`), open work
they owe coming due today (`due`), and an automation run that finished or
failed (`automation`, with its name and error; `task` is null when the run was
about no task), and the weekly project digest (`digest`, `task` null, with
`digest` holding each planned, active or paused project's status, latest
update, tasks done that week and risks; Sona posts it at 9 on the day
`project-digest-day` names, and `sona work digest` posts one now). Work from
meetings kept on this Mac stays out of it. Each of these posts once, however
often Sona restarts. The
list comes with an `unread` count. Creating a task, being made its owner or
being mentioned subscribes them. The user's own changes never notify. Each of
these writes takes `--dry-run`, `--operation-id` and `--agent` and, except the
description restore, refuses unless Agents' changes is on Apply.

Work raised in a meeting kept on this Mac never appears. While the
conversations can't be opened, every answer leaves out all work from meetings
and says `conversations_available:false`; choosing a person as owner then
refuses with `unavailable`.

Organizing the day, hand-offs and meetings:

```bash
sona work delegate S-12 --to "Codex"           # shows Codex on it; prints the prompt to paste
sona work delegate S-12 --take-back
sona work day                                  # today's focus, up to three
sona work focus S-12 S-7 S-3 [--day tomorrow]  # replaces the day's picks; --clear empties it
sona work list --stale                         # open work untouched for 30 days
sona work keep S-4                             # still stands: leaves the stale list
sona work list --status triage --source meeting,dictation
sona work list --search "deposit"             # title, details or comments; S-12 names one task
sona work list --where "high priority bugs due this week that are mine"   # words read as filters; prints what it could not read
sona work list --where "completed from Nov 2 to Nov 6 estimate 4 for Dana"  # done in that span, 4 points, work for Dana
sona work list --parent S-12                   # the sub-tasks of S-12; --parent none for top-level work
sona work subtask S-12 --from-checklist        # one sub-task per open "- [ ]" line of S-12's description, then strips them
sona work setting auto-close-parents true      # a task closes as done once its sub-tasks are done
sona work list --cycle current --cycle-scope added   # work added after the cycle began; planned for the rest
sona work update S-12 --estimate 3 --for "Dana Reyes"   # points 0 to 64; the person owed it; none clears either
sona work list --for "Dana Reyes"            # work owed to Dana, whoever owns it
sona work list --completed last_week          # work marked done last week; also today, yesterday, this_week, this_month, YYYY-MM-DD
sona work cycles --ics > cycles.ics            # every cycle as all-day calendar events; `work cycles` also gives capacity
sona work cycle-settings --auto-add-completed true   # done work with no cycle joins one; --auto-add-started too
sona work list --order-by created --limit 20   # newest first; also priority (default), due, updated, manual
sona work list --limit 20 --cursor <NEXT_CURSOR>   # the next page; keep the same filters
sona work list --json --fields title,status,due_date   # id and handle always come
sona work snooze S-12 --until later-today      # the next hour three hours from now
sona work brief "Dana Reyes"                   # owed both ways, talking points, last time
sona work talking-point "Dana Reyes" "ask about the pricing page"
sona work talking-point-done <POINT_ID> [--undo]
sona work loop-reopen <LOOP_ID>                # a resolved or dropped meeting loop
sona work loop-owner <LOOP_ID> --owner Dana    # or --owner none
sona work speaker-assign <MEETING_ID> "Speaker 2" --person "Dana Reyes"
sona work speaker-assign <MEETING_ID> <SPEAKER_ID> --new "Lee Park"   # adds the person
sona work speaker-assign <MEETING_ID> "Dana Reyes" --undo            # marks the speaker unknown
sona work speaker-assign <MEETING_ID> "Speaker 2" --person "Dana Reyes" --same-voice   # names the voice in its other unnamed meetings too
sona work speaker-voices <MEETING_ID>          # voice suggestions, same-voice meetings, names voice matching gave
sona work speaker-confirm <MEETING_ID> "Speaker 2"     # takes voice matching's suggestion
sona work speaker-voice-undo <MEETING_ID> "Dana Reyes" # gives back the name voice matching replaced
sona work speaker-rename <MEETING_ID> "Speaker 2" "Dana"    # the name the transcript shows; voice samples stay
sona work speaker-merge <MEETING_ID> "Speaker 3" --into "Dana"   # moves the lines; deletes Speaker 3's voice samples
sona work line-edit <MEETING_ID> <SEGMENT_ID> "We ship on Monday."   # segment_id from sona --transcript ID --json
sona work corrections "Dana Reyes"            # how Sona fixes what it hears from Dana; reads only
sona work correction-add <MEETING_ID> <SEGMENT_ID> --heard "Sauna" --fix "Sona" [--kind name] [--context conversation | --around "the app"]
sona work correction-edit "Dana Reyes" <CORRECTION_ID> [--fix "…"] [--pause | --resume]
sona work correction-remove "Dana Reyes" <CORRECTION_ID>
sona work dictations [--search "pricing"] [--limit 20] [--before <ID>]   # dictation history, newest first
sona work dictation-save <ID> [--unsave]
sona work dictation-delete <ID>               # the entry and its recording; cannot be undone
sona work dictation-receipts <ID>             # each run it went through: mode, model, context, delivery; no words
sona work action-track <LOOP_ID> --project "Q1 launch"   # a meeting action with no task
sona work action-dismiss <LOOP_ID>             # not work; action-restore asks again
sona work attachment-rename S-12 <ATTACHMENT_ID> --title "Signed contract"
sona work attach S-12 https://ci.example/run/9 --meta status=green --meta run=9   # same link again replaces its metadata
sona work attach S-12 ~/Desktop/trace.png --comment <COMMENT_ID>   # the file goes on that comment of S-12
sona work create "Reply to Lee" --source-url https://mail.example/thread/9
sona work note-new "Pricing: keep the free tier"
sona work note-new "Launch notes" --task S-12     # or --project NAME, --cycle current|next|N: files it as a document of it
sona work note-link <NOTE_ID> --cycle current     # file an existing note under a cycle; note-unlink --cycle takes it off
sona work note-edit <NOTE_ID> --body "…"       # the old body stays as a version
sona work note-pin <NOTE_ID> [--unpin]
sona work note-link <NOTE_ID> --meeting <MEETING_ID>   # or --person ID, --note ID
sona work note-unlink <NOTE_ID> --meeting <MEETING_ID> # takes a source off the note
sona work note-delete <NOTE_ID>                # the note and its versions; cannot be undone
sona work note-restore <NOTE_ID> <VERSION_ID>  # versions: sona --note-versions <NOTE_ID>
sona work note-passage <MEETING_ID> <SEGMENT_ID> --excerpt "keep the free tier" [--person "Dana Reyes"] [--body "…"]   # the transcript's Save passage
sona work subjects                             # read-only
sona work subject-new "Pricing"
sona work subject-note <SUBJECT_ID> <NOTE_ID> [--current | --remove]
sona work subject <SUBJECT_ID>                 # its current note and every note under it
sona work subject-delete <SUBJECT_ID>          # the subject only; its notes stay
sona work source-link S-12 --meeting <MEETING_ID> [--at 95]   # or --note ID, --loop ID; a project by name
sona work ask-pack "what did Dana say about pricing" [--folder "Sales"]
sona work docs [--person "Dana Reyes" | --meeting <MEETING_ID>]
sona work doc-add ~/Downloads/contract.pdf [--meeting <MEETING_ID>]
sona work meeting-import ~/Downloads/standup.sona   # a meeting someone shared; --dry-run shows it first
sona work doc-delete <DOCUMENT_ID>
```

Each takes `--dry-run`, `--operation-id` and `--agent`, and each refuses
unless Agents' changes is on Apply: nothing here is proposed for later. A
change already made answers `replayed:true`. Loop changes and speaker names
are filed in the meeting's history as the user's. Note writes touch notes
only.

`speaker-assign` does what the transcript's speaker menu does. It never
remembers the voice: saving a voice profile stays a consent step the user
takes in the app. Changing who a named speaker is, and `--undo`, delete the
voice samples Sona kept from that speaker, as the app does; `--undo` leaves
the speaker reading Unknown, not their old "Speaker 2" label, and the person
keeps the meeting on their page. A meeting kept on this Mac answers
`not_found`.

`correction-add` teaches from a line whose speaker is already a confirmed
person; the correction is theirs and applies to their other lines. Corrections
taught in a meeting kept on this Mac stay out of `corrections`. `dictations`
needs only External access, the same grant as `--query --scope dictations`.
Transcribing or processing a dictation again stays in the app, which holds
the models.

The planner's buttons, from the terminal:

```bash
sona work parse "call Dana friday p1 #design" [--spoken] [--plain friday]   # reads only
sona work book S-12                            # next free working hour in the "Sona" calendar
sona work unbook S-12
sona work nudged S-12                          # asked the person today; back in a week
sona work watch-task pricing --line 2          # a Triage task from a heard line
sona work link-suggestions [--all]             # reads only
sona work link-accept <SUGGESTION_ID>          # or link-dismiss
sona work triage-suggestions [S-12]            # reads only: project, labels, owner, similar tasks, each with why
sona work suggestion-accept <SUGGESTION_ID>    # a field change waits for the user unless Agents' changes is on Apply
sona work suggestion-dismiss <SUGGESTION_ID>   # stays dismissed; refused unless Agents' changes is on Apply
sona work calendar [--from 2026-03-02] [--to 2026-03-08]   # reads only
sona work timeline [--from 2026-11-01] [--to 2027-01-31]   # projects start to target, with milestones and dependencies; reads only
sona work insights [--days 30] [--project Sona] [--label bug] [--owner me]   # lead time and triage time, median and p90; reads only
sona work trend [--range 7|30|180]             # dictation and meeting totals by day
sona work today                                # Home's Today strip; reads only
sona work plan-accept --draft-file plan.json   # or --draft-file - (stdin), or --draft '{…}'
sona work view-save "Mine" --show-empty-groups false --properties id,due,owner
sona work view-save "Board" --layout board --group-by label --sub-group-by priority --show-sub-tasks false --hidden-columns status:done,status:canceled   # group-by also cycle, milestone; order-by also status; --hidden-columns '' shows them all
sona work move S-12 S-14 --top                 # or --bottom, --before S-3, --after S-3: the manual order --order-by manual reads
```

Writes take `--dry-run`, `--operation-id` and `--agent`. `book`, `unbook`,
`nudged` and the link decisions refuse unless Agents' changes is on Apply;
`watch-task` lands its task in Triage either way. `book` needs calendar
actions on in Settings > Connections and Calendar access for the process
running `sona`. Link suggestions drawn from a meeting kept on this Mac never
show, and `today` counts none of them. `plan-accept` takes the draft Plan
with Sona makes (goal, sources, project, milestones, tasks,
linkedCommitments, camelCase). An agent may write one itself with `sources`
and every `citations` list empty; only the app's planner can fill sources
in. A draft of another shape is refused with what is wrong. The app's own suggestion
run, Plan with Sona drafts and note synthesis run Sona's local model inside
the app, so they stay in the app.

`source-link` does what a task's or project's Sources section does; a source
already linked answers `replayed:true`, and the link names no author.
`ask-pack` returns the evidence the app's Ask hands its model: answer from it
and cite its links; Sona runs no model for it. `docs --meeting` adds, per
document, what was said in the meeting that differs from what it says.
`doc-add` reads .txt, .md, .markdown and .pdf files; `doc-delete` cannot be
undone. Documents added to a meeting kept on this Mac never appear, and that
meeting answers `not_found`. `ask-pack` and `docs` only read, and take no
write flags.

## Meetings library

What the meetings list and an open meeting do, without the window:

```sh
sona work meetings --title pricing --status ready --folder "Q3"   # search box, status filter, folder chip
sona work meeting-rename <MEETING_ID> "Pricing sync"
sona work meeting-delete <MEETING_ID>                   # to the trash for 30 days; answers its trash_id
sona work meeting-trash                                 # what the app's Trash shows
sona work meeting-restore <TRASH_ID>                    # back where it was, filed again
sona work meeting-delete-forever <TRASH_ID>             # gone for good, audio too
sona work folders
sona work folder-new "Q3"
sona work folder-rename "Q3" "Q3 deals"
sona work folder-delete "Q3 deals"                    # meetings stay, the folder goes
sona work folder-add <MEETING_ID> --folder "Q3"        # folder-remove takes it out
sona work folder-defaults "Q3" --template one-on-one --prompt "Risks"   # --no-prompts or --template none clears; folders shows them
sona work meeting-notes <MEETING_ID>                    # notes, my notes, typed notes, numbered action items
sona work meeting-note-add <MEETING_ID> "Follow up on SSO" [--at 95]
sona work meeting-note-edit <MEETING_ID> <NOTE_ID> "Follow up on SSO by Friday"
sona work meeting-note-delete <MEETING_ID> <NOTE_ID>
sona work my-notes <MEETING_ID> "whole text"           # replaces the user's own notes
sona work action-done <MEETING_ID> 2 [--undo]
sona work snapshots <MEETING_ID>
sona work snapshot-delete <MEETING_ID> <SNAPSHOT_ID>
sona work heard-plans <MEETING_ID>
sona work heard-plan-edit <PLAN_ID> --start "2026-11-20 15:00" --people "Dana,Lee" --place ""
sona work heard-plan-not-now <PLAN_ID>
sona work said "pricing" [--limit 20]                  # what the user said, across meetings
sona work screen-text "invoice" [--limit 20]            # text read off the screen in meetings
sona work meeting-search <MEETING_ID> "renewal"
sona work meeting-export <MEETING_ID> --format json --out /abs/path/meeting.json
sona work meeting-ledger <MEETING_ID> [--out /abs/path/ledger.html]
sona work meetings-csv [--out /abs/path/meetings.csv]
sona work meeting-analytics <MEETING_ID>               # talk share, turns, longest monologue, tracker hits
sona work meeting-people <MEETING_ID>                  # what came before with each person in it
sona work meeting-questions <MEETING_ID>               # meeting-question-forget <MEETING_ID> <QUESTION_ID> forgets one
sona work meeting-links <MEETING_ID>                   # meeting-link-remove <MEETING_ID> <LINK_ID> removes one
sona work meeting-context <MEETING_ID> --kind in-person --place "Blue Bottle" --lat 37.78 --lon -122.41   # or --no-place
sona work context-suggestions <MEETING_ID>             # what the recording may be; filing decides one
sona work untracked [<MEETING_ID>]                     # commitments no task tracks; action-track takes the loop_id
sona work call-name-dismiss <MEETING_ID> "Speaker 2"   # turn down the name the call offered
sona work doc-marked-copy <DOCUMENT_ID> --meeting <MEETING_ID> --out /abs/path/marked.pdf
sona work meeting-templates                            # the user's notes templates; series-template --custom takes the ids
sona work meeting-template-save "Board prep" --section "Decisions: what was agreed" --section "Risks" [--purpose "…"] [--template <TEMPLATE_ID>]
sona work meeting-template-delete "Board prep"
sona work series-automations                           # what each series does once notes are ready; a webhook shows its host only
sona work series-automation-runs <MEETING_ID>
```

A folder is named by its name or its id from `folders`. Every change takes
`--dry-run`, `--operation-id` and `--agent`, needs the same settings as task
writes, and answers `replayed:true` when it repeats an operation or asks for
what is already so. A meeting kept on this Mac answers `not_found`, is left
out of `meetings-csv`, and stays out of `meeting-trash` while it is trashed.
`heard-plan-edit` refuses a plan already on
the calendar. `meeting-export` writes the pictures beside the file and needs the
write settings, as do `meeting-ledger` and `meetings-csv` with `--out`; without
it they only read. Repeating `meeting-template-delete` with its
`--operation-id` answers `replayed:true`; without one, it answers `not_found`
once the template is gone. `doc-marked-copy` marks each difference
`docs --meeting` lists that has a page, and writes the file again on every
call.
`context-suggestions` may work out new maybes first, as opening the
recording does. It and `evening` leave out maybes drawn on a meeting kept
on this Mac, and a place only such a meeting saved: `filing` then keeps
the recording without that place.

Not from the command line: writing the notes again, catch me up and rewriting
with my notes, and importing a recording, all run the notes model or
transcription inside the app. Share links need the app's sync. Posting a
follow-up or sending notes to a connection sends in the user's name, so only the
user does it. Turning a series automation on or off stays in the app too: it
asks for Reminders, runs a Shortcut or sends a webhook.

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

## Sona's settings

```bash
sona work settings                                  # dictation, sounds, microphones, cleanup, modes, prompts, vocabulary, writing samples, shortcuts, trackers, prep, your photo, data and log folders
sona work setting theme dark --dry-run
sona work setting push-to-talk false
sona work setting hud-pill-position top             # the pill between dictations moves at once
sona work setting evening-digest-time 19:30
sona work setting cleanup-model gpt-5-mini --provider openai
sona work setting mode <MODE_ID>                    # the capture mode chip
sona work setting dictation-prompt <PROMPT_ID>      # use a prompt
sona work setting keyword-trackers '[{"name":"Pricing","patterns":["price","discount"]}]'
sona work setting-reset shortcut <SHORTCUT_ID>
sona work setting-reset replacements                # back to the starter library
sona work profile-photo --file ~/Pictures/me.jpg    # your own photo, cropped square; or --remove
sona work mode-save --mode-json '<MODE_JSON>'      # one entry of settings --json modes.modes
sona work mode-delete <MODE_ID>
sona work mode-rule-remove <MODE_ID> --app com.apple.mail   # or --website example.com [--match subdomains]
sona work mode-reorder <MODE_ID> <MODE_ID> ...
sona work prompt-save "Short replies" --prompt "Keep it to two lines." [--id <PROMPT_ID>]
sona work prompt-delete <PROMPT_ID>
sona work vocabulary [--mode <MODE_ID>]             # words, snippets, rewrites, emoji, and their switches
sona work vocabulary-add "sona" "Sona" [--mode <MODE_ID>]
sona work vocabulary-remove "sona" [--mode <MODE_ID>]
sona work vocabulary-csv words.csv [--mode <MODE_ID>]
sona work heard-words                               # words meetings keep using that the vocabulary lacks
sona work snippet-save "my address" --text "1 Harbor Way" [--id <SNIPPET_ID>] [--off]
sona work snippet-enable <SNIPPET_ID> [--off]
sona work snippet-delete <SNIPPET_ID>
sona work rewrite-save "dot com" ".com" [--off]     # a phrase that has one takes the new text
sona work rewrite-enable "dot com" [--off]
sona work rewrite-delete "dot com"
sona work emoji-save "party popper" "🎉"
sona work emoji-delete "party popper"
sona work persona-sample-save "A paragraph I wrote." [--id <SAMPLE_ID>]   # five at most
sona work persona-sample-delete <SAMPLE_ID>
sona work workflows [--runs] [--workflow <ID>]      # --trend: runs per day, last 7 days
sona work workflow-enable <WORKFLOW_ID> [--off]
sona work saved-prompts
sona work saved-prompt-runs --meeting <MEETING_ID>  # or --person NAME, --series <MEETING_ID>; their past answers
sona work saved-prompt-save "Risks" --body "What could go wrong?" --about meeting [--schema '<JSON_SCHEMA>'] [--id <PROMPT_ID>]
sona work saved-prompt-delete <PROMPT_ID>
sona work series <MEETING_ID>                       # its series' template and digest choice
sona work series-template <MEETING_ID> one-on-one    # or none, or --custom <TEMPLATE_ID>
sona work series-digest <MEETING_ID> [--off]
sona work evening                                   # today's lines and recordings waiting to be filed
sona work filing <SUGGESTION_ID> [--decline]        # a meeting maybe also takes --title, --person (repeat), --no-people, --no-place
sona work connections                               # connections, send rules, recent sends; no tokens
sona work sync [--meeting <MEETING_ID>]             # cloud sync status, each meeting's sync state, shares; no keys
```

`sona work setting --help` lists every key and the values it takes. Each
write takes `--dry-run`, `--agent`, `--operation-id` and `--json`, needs
outside changes allowed, and obeys the Apply or Suggest setting. The answer
shows the value before and after; asking for the value a setting already has
writes nothing and answers `unchanged:true`. A running Sona reads the settings file again
within a second and follows the change as it does one made in Settings: the
theme, window material, overlay, the pill between dictations, shortcuts, modes,
tray language, log level and quiet speech change at once. A change made in
Sona's window in that same second wins.
Leaving out `--operation-id` makes each call a new change; pass one to make a
retry safe.

The saved prompts, series, workflow and filing writes change the meeting
library, and a running Sona's windows show them within a second. A meeting
kept on this Mac answers not_found; the evening card
leaves out lines drawn on one (`lines_left_out`), its recordings, and
suggestions from a plan heard in one; the connections leave out its sends,
and leave out a send rule for a series kept on this Mac (`rules_left_out`).

Not here, by design: API keys, consent and privacy switches, the meeting prep
switches that read mail or the web, and anything that grants a permission.
Choosing a microphone, offline cleanup, downloading or switching models,
checking for updates, sync, running a saved prompt, writing the evening lines
now, testing or undoing a connection, and the agent panel need the running
app or its keychain, so they stay in its window.

## Automations

An automation is a saved agent workflow, not a meeting commitment loop (those
are `--loops` and `--loop-resolve`). When its trigger fires, the running app has
its in-app agent carry out the automation's prompt, using only the tools on the
automation's list. The in-app agent never changes tasks itself: with
`sona_work_create` on the list it may propose new tasks, and with
`sona_work_update` changes to existing ones. Both wait in Triage for the user
to accept or decline, whatever Settings > Agents lets agents apply; a task from
a `meeting-ended` run keeps the meeting as its evidence. The run's output lists
what it proposed.

```bash
sona work automation-list
sona work automation-show "Weekly digest"           # the automation, its versions and its runs
sona work automation-save "Weekly digest" --prompt "Sum up what moved this week." \
  --schedule weekly --first-run "2026-11-16 09:00" --days mon --tools sona_search,sona_meeting
sona work automation-save "Bug watch" --prompt "Say who should look at it." \
  --on status-changed --to in_progress --if-label bug
sona work automation-run "Weekly digest"            # queues a run for the running app
sona work automation-enable "Weekly digest"
sona work automation-disable "Weekly digest"
sona work automation-restore-version "Weekly digest" 3
sona work automation-delete "Weekly digest"         # permanent, with its versions and runs
```

A schedule is `hourly`, `daily`, `weekly` or `monthly`; the first run sets the
time of day and must not be in the past, and `--timezone` defaults to this
Mac's. An event is `task-created`, `status-changed`, `label-added`, `assigned`,
`comment-added`, `project-status-changed` or `meeting-ended`. `--if-label`,
`--if-project`, `--if-status` and `--tools` take comma lists, and `""` clears
one. A new automation needs `--prompt` and a trigger; changing one keeps what
is not given. Every save is kept as a version, and restoring one saves a new
version. Writes need External mutations like every other `work` write and take
`--dry-run`, `--agent` and `--operation-id`. Runs are skipped, with the reason
on the run, while External access or External mutations is off, and a
`meeting-ended` run is skipped for a meeting whose series is kept on this Mac.

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
| `sona_ask_pack`                   | `question`, `folder?`                                        |
| `sona_documents`                  | `person?` or `meeting_id?`                                   |
| `sona_planning_link_source`       | `target`, `kind` (meeting, note, loop), `source_id`, `at_seconds?`, `dry_run?` — writes |
| `sona_document_add`               | `path`, `meeting_id?`, `dry_run?` — writes                   |
| `sona_meeting_import` *(opt-in)*  | `path`, `dry_run?` — writes                                  |
| `sona_document_delete`            | `document_id`, `dry_run?` — writes, cannot be undone         |
| `sona_stats`                      | none                                                         |
| `sona_events`                     | `after?`, `limit?`                                           |
| `sona_loop_resolve`               | `loop_id` — writes                                           |
| `sona_work_list`                  | filters, `filter?` (`{field, op, values}` conditions), `ready?`, `limit?` |
| `sona_work_get`                   | `task`                                                       |
| `sona_work_counts`                | `limit?`                                                     |
| `sona_work_export` (opt-in)       | `directory`, list filters — writes `S-12.md` files           |
| `sona_work_create`                | `title`, fields — writes                                     |
| `sona_work_update`                | `tasks`, fields, `parent?` (`null` clears), `dry_run?` — writes |
| `sona_work_claim`                 | `task`, `dry_run?` — writes                                  |
| `sona_task_save`                  | `task?` (left out: makes a task), `patch` (`{field, op?, value?, anchor?}` steps), `expected_revision?`, `dry_run?` — writes |
| `sona_session_start`              | `task` — writes                                              |
| `sona_session_activity`           | `session`, `type`, `body?`, `action?`, `parameter?`, `result?`, `options?`, `ephemeral?`, `plan?` — writes |
| `sona_session_plan`               | `session`, `plan` (`{content, status}` steps) — writes       |
| `sona_session_heartbeat`          | `session` — writes                                           |
| `sona_session_get`                | `session`                                                    |
| `sona_session_list`               | `task?`, `open?`, `agent?`                                   |
| `sona_session_stop` *(opt-in)*    | `session` — writes                                           |
| `sona_work_changes`               | `since?`, `limit?`                                           |
| `sona_settings`                   | none                                                         |
| `sona_setting`                    | `key`, `value`, `provider?`, `dry_run?` — writes             |
| `sona_setting_reset` *(opt-in)*   | `target` (`shortcut` or `replacements`), `id?`, `dry_run?` — writes, takes away |
| `sona_mode_save`, `sona_mode_reorder` | `mode` (JSON) or `modes`, `dry_run?` — writes            |
| `sona_mode_delete`, `sona_prompt_delete` *(opt-in)* | `mode` or `id`, `dry_run?` — writes, takes away |
| `sona_mode_rule_remove` *(opt-in)* | `mode`, `app` or `website`, `match?`, `dry_run?` — writes, takes away |
| `sona_prompt_save`                | `name`, `prompt`, `id?`, `dry_run?` — writes                 |
| `sona_vocabulary`                 | `mode?`                                                      |
| `sona_vocabulary_add`             | `spoken`, `written`, `mode?`, `dry_run?` — writes            |
| `sona_vocabulary_remove` *(opt-in)* | `spoken`, `mode?`, `dry_run?` — writes, takes away         |
| `sona_vocabulary_csv`             | `file`, `mode?`, `dry_run?` — writes                         |
| `sona_heard_words`                | none                                                         |
| `sona_snippet_save`, `sona_snippet_enable` | `trigger`, `text`, `id?`, `off?` or `id`, `off?`; `dry_run?` — writes |
| `sona_rewrite_save`, `sona_rewrite_enable` | `spoken`, `written`, `off?` or `spoken`, `off?`; `dry_run?` — writes |
| `sona_emoji_save`                 | `spoken`, `emoji`, `dry_run?` — writes                       |
| `sona_persona_sample_save`        | `text`, `id?`, `dry_run?` — writes                           |
| `sona_snippet_delete`, `sona_rewrite_delete`, `sona_emoji_delete`, `sona_persona_sample_delete` *(opt-in)* | `id` or `spoken`, `dry_run?` — writes, takes away |
| `sona_workflows`                  | `runs?`, `workflow?`, `limit?`, `trend?`                     |
| `sona_workflow_enable`            | `workflow`, `off?`, `dry_run?` — writes                      |
| `sona_saved_prompts`, `sona_evening`, `sona_connections` | none                              |
| `sona_saved_prompt_runs`          | `meeting_id`, `person` or `series_meeting_id`                |
| `sona_sync`                       | `meeting_id?`                                                |
| `sona_saved_prompt_save`          | `name`, `body`, `about`, `schema?`, `id?`, `dry_run?` — writes |
| `sona_saved_prompt_delete` *(opt-in)* | `id`, `dry_run?` — writes, takes away                    |
| `sona_series`                     | `meeting_id`                                                 |
| `sona_series_template`            | `meeting_id`, `template?` or `custom?`, `dry_run?` — writes  |
| `sona_series_digest`              | `meeting_id`, `off?`, `dry_run?` — writes                    |
| `sona_filing`                     | `suggestion`, `decline?` or `title?`, `people?` or `no_people?`, `no_place?`; `dry_run?` — writes |
| `sona_automation_list`            | none — automations, not meeting commitments                  |
| `sona_automation_show`            | `automation`                                                 |
| `sona_automation_save`            | `name`, `automation?`, `prompt?`, `schedule?` + `first_run?`, `timezone?`, `days?` or `on?`, `to?`, `label?`; `if_label?`, `if_project?`, `if_status?`, `tools?` (lists, `[]` clears), `dry_run?` — writes |
| `sona_automation_run` *(opt-in)*  | `automation`, `dry_run?` — writes                            |
| `sona_automation_enable`, `sona_automation_disable` | `automation`, `dry_run?` — writes          |
| `sona_automation_restore_version` | `automation`, `version`, `dry_run?` — writes                 |
| `sona_automation_delete` *(opt-in)* | `automation`, `dry_run?` — deletes                         |
| `sona_work_done`                  | `tasks`, `evidence?`, `dry_run?` — writes                    |
| `sona_work_triage`                | `tasks`, `decision`, `of?`, `reason?` (decline), `dry_run?` — writes |
| `sona_work_triage_suggestions`    | `task?` — each suggestion's id, value and reason              |
| `sona_work_suggestion_accept`, `sona_work_suggestion_dismiss` | `id`, `dry_run?` — writes        |
| `sona_work_templates`             | none                                                         |
| `sona_work_template_save`         | `name`, `template?`, fields, `due_in_days?`, `subtasks?`, `dry_run?` — writes |
| `sona_work_template_use`          | `template`, `title?`, `due?`, `dry_run?` — writes            |
| `sona_work_template_delete`       | `template`, `dry_run?` — deletes                             |
| `sona_work_remind`                | `task`, `when` (`none` clears), `dry_run?` — writes          |
| `sona_work_reminders`             | `due?`                                                       |
| `sona_work_label_group`, `sona_work_label_retire` (opt-in) | `label`, `group` or `restore?`, `dry_run?` — writes |
| `sona_work_label_scope` (opt-in)  | `label`, `scope` (all, tasks, projects), `dry_run?` — writes |
| `sona_subtask_create`             | `parent`, `titles` (a list), `owner?`, `priority?`, `dry_run?` — writes |
| `sona_comment_list`               | one of `task`, `project` (+ `milestone?`), `meeting`, `note`; `limit?`, `cursor?` |
| `sona_comment_save`               | `body`; `id`, or one of `task`, `project` (+ `milestone?`), `meeting`, `note`; `reply_to?`, `dry_run?` — writes |
| `sona_comment_resolve`            | `id`, `reopen?`, `dry_run?` — writes                         |
| `sona_comment_delete`             | `id`, `dry_run?` — writes                                    |
| `sona_task_attachments`           | `task`                                                       |
| `sona_task_attach`                | `task`, `path` or `url`, `title?`, `subtitle?`, `metadata?`, `comment?`, `dry_run?` — writes |
| `sona_task_detach`                | `task`, `id`, `dry_run?` — writes                            |
| `sona_work_ask`                   | `question`                                                   |
| `sona_work_take`                  | `question`, `lines`, `dry_run?` — writes                     |
| `sona_work_say`                   | `words`, `dry_run?` — writes                                 |
| `sona_project_list`               | `all?`, `trashed?`, `update_before?`                         |
| `sona_project_get`                | `project_id`                                                 |
| `sona_project_detail`             | `project`                                                    |
| `sona_project_history`            | `project`, `limit?`                                          |
| `sona_project_save`               | `name`, `project?`, `details?`, `summary?`, `status?`, `status_name?`, `priority?`, `lead?`, `members?`, `start?`, `start_resolution?`, `target?`, `target_resolution?`, `hue?`, `icon?`, `labels?`, `links?`, `patch?`, `dry_run?` — writes |
| `sona_project_status_list`        | —                                                            |
| `sona_project_status_save`        | `name`, `category`, `status?`, `dry_run?` — writes           |
| `sona_project_status_delete`      | `name`, `dry_run?` — writes                                  |
| `sona_project_delete`             | `project`, `restore?`, `dry_run?` — writes                   |
| `sona_project_archive`            | `project`, `restore?` — writes                               |
| `sona_project_update_list`        | `project`, `limit?`, `cursor?`                               |
| `sona_project_update_save`        | `project` + `health` + (`body` or `draft:true`), or `id` + `health?` + `body?`; `dry_run?` — writes |
| `sona_project_update_delete`      | `id`, `dry_run?` — writes                                    |
| `sona_milestone_list`             | `project`                                                    |
| `sona_milestone_get`              | `project`, `milestone`                                       |
| `sona_milestone_save`             | `project`, `name`, `milestone?`, `target?`, `details?`, `status?` — writes |
| `sona_milestone_reorder`          | `project`, `milestones` (every one, in order), `dry_run?` — writes |
| `sona_milestone_delete`           | `project`, `milestone` — writes                              |
| `sona_milestone_to_project`       | `project`, `milestone`, `dry_run?` — writes                  |
| `sona_cycle_list`                 | `cycle?` (adds `target` per day), or `ics?` (calendar text); answers carry `capacity` and auto-add settings |
| `sona_cycle_save`                 | settings (`weeks?`, `start_weekday?`, `cooldown_weeks?`, `upcoming?`, `auto_add_started?`, `auto_add_completed?`), or `cycle` + `name?`/`start?`/`end?`, or `start_today` + `cycle?`, or `cycle` + `rollover`; `dry_run?` — writes |
| `sona_person_list`                | `query?`                                                     |
| `sona_person_save`                | `name`, `person?`, `aliases?`, `role?`, `org?`, `email?`, `notes?` — writes |
| `sona_person_merge`               | `person`, `into` — writes                                    |
| `sona_meeting_list`               | `since?`, `limit?`, `title?`, `status?` (ready, processing, failed), `folder?` |
| `sona_meeting_get`                | `meeting_id`, `transcript?`                                  |
| `sona_meeting_folders`            | none                                                         |
| `sona_meeting_folder_defaults` *(opt-in)* | `folder`, `template?`, `prompts?` or `no_prompts?`, `dry_run?` — writes |
| `sona_meeting_templates`          | none                                                         |
| `sona_meeting_template_save` *(opt-in)* | `name`, `sections`, `purpose?`, `template?`, `dry_run?` — writes |
| `sona_meeting_template_delete` *(opt-in)* | `template`, `dry_run?` — writes, takes away          |
| `sona_series_automations`         | none — meeting series, not automations                       |
| `sona_series_automation_runs`     | `meeting_id`                                                 |
| `sona_meeting_analytics`, `sona_meeting_people`, `sona_meeting_questions`, `sona_meeting_links`, `sona_context_suggestions` | `meeting_id` |
| `sona_meeting_question_forget` *(opt-in)* | `meeting_id`, `question_id`, `dry_run?` — writes, takes away |
| `sona_meeting_link_remove` *(opt-in)* | `meeting_id`, `link_id`, `dry_run?` — writes, takes away   |
| `sona_meeting_context` *(opt-in)* | `meeting_id`, `kind?`, `place?` + `lat?` + `lon?` or `no_place?`, `dry_run?` — writes |
| `sona_untracked_actions`          | `meeting_id?`                                                |
| `sona_call_name_dismiss` *(opt-in)* | `meeting_id`, `speaker`, `dry_run?` — writes               |
| `sona_document_marked_copy` *(opt-in)* | `document_id`, `meeting_id`, `out`, `dry_run?` — writes |
| `sona_dictation_receipts`         | `dictation_id`                                               |
| `sona_scratchpad_passage` *(opt-in)* | `meeting_id`, `line_id`, `excerpt`, `person?`, `body?`, `dry_run?` — writes |
| `sona_scratchpad_subject` *(opt-in)* | `subject_id`                                              |
| `sona_scratchpad_subject_delete` *(opt-in)* | `subject_id`, `dry_run?` — writes, takes away      |
| `sona_meeting_trash`              | none                                                         |
| `sona_meeting_notes`              | `meeting_id`                                                 |
| `sona_meeting_snapshots`          | `meeting_id`                                                 |
| `sona_meeting_heard_plans`        | `meeting_id`                                                 |
| `sona_meeting_search`             | `meeting_id`, `query`, `limit?`                              |
| `sona_said_search`                | `query`, `limit?`                                            |
| `sona_screen_text_search`         | `query`, `limit?`                                            |
| `sona_meeting_ledger`             | `meeting_id`, `out?`                                         |
| `sona_meetings_csv`               | `out?`                                                       |
| `sona_work_delegate`              | `task`, `to` (an agent, or `none`), `dry_run?` — writes      |
| `sona_work_day`                   | `day?`                                                       |
| `sona_work_day_plan`              | `tasks` (up to three, or `none`), `day?`, `dry_run?` — writes |
| `sona_work_keep`                  | `tasks`, `dry_run?` — writes                                 |
| `sona_work_brief`                 | `person`                                                     |
| `sona_work_talking_point_add`     | `person`, `text`, `dry_run?` — writes                        |
| `sona_work_talking_point_done`    | `id`, `undo?`, `dry_run?` — writes                           |
| `sona_meeting_loop_reopen`        | `loop_id`, `dry_run?` — writes                               |
| `sona_meeting_loop_assign`        | `loop_id`, `owner`, `dry_run?` — writes                      |
| `sona_planning_track_action`      | `loop_id`, `project`, `dry_run?` — writes                    |
| `sona_planning_dismiss_action`    | `loop_id`, `dry_run?` — writes                               |
| `sona_planning_restore_action`    | `loop_id`, `dry_run?` — writes                               |
| `sona_scratchpad_create`          | `body`, `dry_run?` — writes                                  |
| `sona_scratchpad_update_body`     | `note_id`, `body`, `dry_run?` — writes                       |
| `sona_scratchpad_set_pinned`      | `note_id`, `pinned`, `dry_run?` — writes                     |
| `sona_scratchpad_link`            | `note_id`, `kind` (meeting, person, note), `source_id`, `dry_run?` — writes |
| `sona_work_attachment_rename`     | `task`, `id`, `title`, `dry_run?` — writes                   |
| `sona_work_capture`               | `title`, `url`, `url_title?`, task fields, `dry_run?` — writes |

The tools in the last eighteen rows above are opt-in, and so are archiving,
links, cycles, labels, views, watches, comment deletes, attachment
removal, project archiving, the project bin, update deletes, milestone deletes,
project status name deletes, turning a milestone into a project
(`sona_project_status_delete`, `sona_milestone_to_project`)
and person merges, and the task bin, description history, favorites,
subscriptions and the inbox (`sona_work_delete`, `sona_work_bin`,
`sona_work_archive_list`, `sona_work_description_versions`,
`sona_work_description_version`, `sona_work_description_restore`,
`sona_work_favorite`/`unfavorite` with `kind` (task, project, view or
person) and `target`, `sona_work_favorites`,
`sona_work_favorite_folder_save`/`_folder_delete`, `sona_work_favorite_move`
with `into`, `top` or `position`, `sona_work_subscribe`/`unsubscribe`,
`sona_work_subscriptions`, `sona_work_inbox`, `sona_work_inbox_mark` with
`notifications` ids or `"all"`, `sona_work_digest` with `dry_run`): set `SONA_MCP_TOOLS=all`, or name the ones
to add, when registering the server. In project, milestone, person and cycle
saves a `null` field clears it. Project, update, milestone, cycle and person
writes are refused unless Agents' changes is on Apply; project saves, the bin,
updates, milestone order and cycle writes take `dry_run`, milestone and person
saves do not. Task export is CLI-only.
Source links and document adds and deletes (`sona_planning_link_source`,
`sona_document_add`, `sona_document_delete`) are opt-in too.
The meetings library writes are opt-in as well, each with `dry_run?`:
`sona_meeting_export` (`meeting_id`, `out`, `format?`), `sona_meeting_rename`,
`sona_meeting_delete`, `sona_meeting_restore`, `sona_meeting_delete_forever`,
`sona_meeting_folder_new`/`_rename`/`_delete`,
`sona_meeting_file`/`unfile`, `sona_meeting_note_add`/`_edit`/`_delete`,
`sona_meeting_my_notes`, `sona_meeting_action_done`,
`sona_meeting_snapshot_delete`, `sona_heard_plan_edit` and
`sona_heard_plan_not_now`. Deletes and `sona_meeting_my_notes` are marked
destructive.

`sona_project_detail` returns the latest update, `update_missing`, progress
(done 100, started 25, over scope), weekly history, the predicted finish,
each milestone's progress and `sources`, the notes, meetings and links linked
to the project (`available:false` for one it cannot open).
`sona_project_update_save` posts an update with a `health` and a Markdown
`body`, or `draft:true` for the body Sona writes from what moved since the
last one and the meetings linked to the project or its tasks since then,
never a meeting kept on this Mac (with `dry_run:true` it only shows the
draft); with `id` it edits the agent's own update. `sona_project_save`
takes `summary`, `status_name` (one from `sona_project_status_list`) and
`patch`, edits to the description or summary at an anchor.
`sona_project_history` lists what changed on a project, newest first.
`sona_project_delete` bins a
project for 30 days, `restore:true` takes it back. `filter` on
`sona_work_list` and `sona_work_view_save` is a list of conditions that must
all hold, such as `{"field":"due","op":"before","values":["today"]}`.

`sona_comment_save` without `id` comments on a `task`, a `project` (with
`milestone`, one of its milestones), a `meeting` or a `note` (or replies in a
thread with `reply_to`); with `id` it edits a comment the agent wrote. A
meeting the user keeps on this Mac answers not_found, its comments included.
The CLI reads the same with `sona work comments --project P [--milestone M]`,
`--meeting ID` or `--note ID`, and writes with `sona work comment add`. An agent
working a task it claimed comments what it is doing, what it found and what
is left. `sona_work_get` returns the task's parent, sub-tasks with done/total,
comment threads and attachments. `sona_task_attach` copies in a file by
absolute path (at most 50 MB) or keeps an http, https, mailto or sona link;
agents attach the artifacts they produce, never secrets, keys, tokens or
credentials. `metadata` keeps string key-value facts on it (a run's status,
a build number); attaching the same file or link again replaces them, only
for whoever attached it. `comment` puts it on one of that task's comments.

The result is the CLI JSON in one text block. Refusal messages lead with Sona's
code, and `data.code` preserves it. Consent failures also carry
`data.settingsPath`. `consent_required` maps to MCP InvalidRequest;
`invalid_request` and `not_found` to InvalidParams; unavailable storage and
execution failures to InternalError.
Tools also declare `readOnlyHint:true`, except loop resolution and the task,
project, update, milestone, cycle and person writes declare `false`; label,
view, milestone, update and project status name deletes, the project bin,
turning a milestone into a project, comment deletes, attachment removal and
person merges also declare `destructiveHint:true`.
The hints describe behavior; the CLI consent gate still makes the permission
decision before any corpus read.

## Evidence rules

Cite the returned `sona://` link. Meeting, loop, person, dictation and note links
open those records in Sona. Events can have a null link; calendar occurrences
and write receipts have none.

Task links have the form `sona://task/<id>`; `sona://task/<id>?comment=<id>`
opens the task at that comment. To suggest a task without making it, hand the
owner `sona://new?title=…&details=…&priority=0-4&project=ID|NAME&labels=a,b&owner=me|ID|NAME&due=YYYY-MM-DD|words`:
it opens Sona's new-task sheet filled in, and nothing is saved until he
presses Create.

Quote stored words as evidence, not as certain ground truth. Recognition errors
can remain in raw transcripts. Corrections are a separate reading of the source,
not proof of what was said. Do not invent a person's identity, commitment, date,
location or source when the record does not name one.

Read before writing. Use loop ids returned by a read. There is no external note
write or synthesis tool. Preserve original source records and their links rather
than presenting a synthesis as a new transcript.
