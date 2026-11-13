# Comments and sub-tasks: core, CLI and bridge contract

Linear parity items 1-2. Core, `sona-core work` and the native bridge are in
place; the Swift UI and the MCP tools build on this file.

## Storage

- `planning_tasks.parent_id` (nullable, indexed). Nesting is allowed; a parent
  that is the task itself or one of its own sub-tasks is refused with
  "A task cannot go under itself or one of its own sub-tasks."
- `planning_task_comments`: `id, task_id, parent_comment_id, author_kind,
  author_name, body, created_at_ms, edited_at_ms, resolved_at_ms,
  deleted_at_ms`. Threads are one reply deep, as in Linear: a reply to a reply
  joins the thread root. Delete is soft: the body goes, and a deleted root
  stays listed while it has live replies. Resolve acts on the thread root.
- Deleting a task deletes its comments and lifts its sub-tasks to top level
  (trigger `planning_task_threads_delete`).
- Activity kinds gain `parent` (from/to: the old and new parent ids, null for
  none) and `comment` (to: the comment id). Proposal fields gain `parent`.
- Migration `THREADS_MIGRATION`, after `CATALOG_MIGRATION`.
- `QUERY_SCHEMA_VERSION` stays 6: fields were added, none renamed or removed,
  following the tracker's practice for additive changes.

## Swift work this needs (not done here)

`macos/Sona/WorkTypes.swift`: `WorkActivityKind` (line ~509) needs `parent`
and `comment`, `WorkProposalField` (line ~330) needs `parent`. Until then a
task whose history holds a parent change or a comment fails to decode in the
app.

## Rules

- Sub-task create inherits the parent's project, milestone and priority unless
  the input gives them. Owner, when the input gives none: me when the parent
  is mine, else the owner every existing sub-task shares, else nobody.
- Progress (`subTasks` on a row, `sub_tasks` in the CLI) counts direct
  sub-tasks, archived ones left out, canceled and duplicate ones left out;
  `done/total`. Absent when the task has no sub-tasks.
- Comments: anyone may add. The author edits. The user deletes any comment;
  Sona and agents delete only their own. Agents may add under Apply or
  Suggest; resolve and every task write follow the agent's Apply/Suggest
  setting like `work update` (Suggest turns `--parent` into a proposal and
  forces new sub-tasks into Triage).
- Every add writes a `comment` activity row under its author.

## Bridge methods (params camelCase; reply is the JSON shown)

| method | params | reply |
|---|---|---|
| `work_comments` | `itemId`, `limit?` (1-100, default 50), `cursor?` | `WorkCommentPage` |
| `work_comment_add` | `operationId`, `itemId`, `body`, `replyTo?` | `WorkComment` (the new comment) |
| `work_comment_edit` | `operationId`, `id`, `body` | `WorkComment` |
| `work_comment_delete` | `operationId`, `id` | `WorkComment` |
| `work_comment_resolve` | `operationId`, `id` (any comment in the thread), `resolved` | `WorkComment` (the thread root with replies) |
| `work_subtask_create` | `operationId`, `parentId`, `inputs: [WorkItemInput]` | `[WorkItem]` |
| `work_set_parent` | `operationId`, `id`, `parentId` (null lifts it to top level) | `WorkItem` |

`work_update` also accepts `patch.parentId` (string, or null to clear).
A repeated `operationId` with the same request returns the first answer; with
a different request it fails with "This change was already used."

### Shapes

```
WorkProgress     { done: u32, total: u32 }
WorkItemRef      { id, number, title, status }
WorkCommentAuthor{ kind: "me" | "sona" | "agent", name: string | null }
WorkComment      { id, itemId, parentId: string|null, author: WorkCommentAuthor,
                   body: string|null, createdAtMs, editedAtMs|null,
                   resolvedAtMs|null, deleted: bool, replies: [WorkComment] }
WorkCommentPage  { threads: [WorkComment], nextCursor: string|null }
```

- `WorkItem` gains `parentId: string|null`.
- `WorkItemRow` (in `work_list`) gains `subTasks: WorkProgress|null`.
- `WorkItemDetail` (`work_item`) gains `parent: WorkItemRef|null`,
  `subTasks: [WorkItem]` (archived left out, by number) and
  `comments: [WorkComment]` (every thread, oldest first).

## CLI (`sona-core work ...`)

Every write verb takes `--json`, `--agent NAME`, `--operation-id ID` and
`--dry-run`; a dry run needs only the read scope.

- `comment add <TASK> --body TEXT [--reply-to COMMENT]`; without
  `--operation-id`, the id is derived from the task, the thread, the
  trimmed text and the day, so a retry that day posts once
- `comments <TASK> [--limit N] [--cursor COMMENT]`
- `comment edit <COMMENT> --body TEXT` (also `comment-edit`)
- `comment delete <COMMENT>` (also `comment-delete`)
- `comment resolve <COMMENT> [--reopen]` (also `comment-resolve`)
- `subtask <PARENT> <TITLE>... [--owner me|nobody|NAME] [--priority P]`
- `update <TASK> --parent <TASK|none|null>`; `--owner` also takes `null`.
- `list` rows gain `parent` (`{id, handle, title, status}`) and `sub_tasks`
  (`{done, total}`); text shows `under S-3` and `1/2 sub-tasks done`.
- `show` gains `parent`, `sub_tasks` (task rows) and `comments`; text shows
  "Sub-task of", "Sub-tasks" and "Comments" sections.

CLI JSON for a comment: `{id, task_id, reply_to, author: {kind, name}, body,
created_at_ms, edited_at_ms, resolved_at_ms, deleted, replies}`. A comment
write answers `{schema_version, action: added|edited|deleted|resolved|reopened,
dry_run, operation_id, task, comment, replayed}`.

## Verified

- `cargo check --lib --tests`: clean, no new warnings.
- `cargo test --lib -- planning::work::comments`: 3 passed
  (`a_task_cannot_go_under_its_own_sub_task`,
  `a_reply_to_a_reply_joins_the_thread_root`,
  `deleting_a_thread_root_keeps_its_replies_readable`). With the cycle check,
  the root lookup and the deleted-root rule each broken on purpose, all three
  failed on their assertions; restored afterwards.
- Scratch smoke, 2026-11-10: a debug `sona` built from this tree, run as
  `HOME=/private/var/tmp/sona-scratch-comments SONA_SCRATCH_RUN=1 sona work ...`
  against a fresh tracker. Long JSON task rows trimmed to the fields that
  matter (`…`).

```
$ work create "Launch the harbor site" --priority high --owner me
S-1    Todo         High    Launch the harbor site  · you

$ work subtask S-1 "Book the venue" "Order the banners" --json
{"action":"created","tasks":[
  {"handle":"S-2","title":"Book the venue","priority":"high","owner":{"kind":"self"}, …
   "parent":{"handle":"S-1","title":"Launch the harbor site","status":"not_started"},"sub_tasks":null},
  {"handle":"S-3","title":"Order the banners","priority":"high","owner":{"kind":"self"}, …
   "parent":{"handle":"S-1", …},"sub_tasks":null}], "replayed":false}

$ work update S-3 --status done
S-3    Done         High    Order the banners  · you · under S-1

$ work list
S-1    Todo         High    Launch the harbor site  · you · 1/2 sub-tasks done
S-2    Todo         High    Book the venue  · you · under S-1

$ work create "Loose task"
S-4    Todo         -       Loose task

$ work update S-4 --parent S-1 --dry-run
S-4 Loose task
  parent: none → S-1 Launch the harbor site
Dry run: nothing was written. …

$ work show S-1                      # the dry run left S-4 out
S-1    Todo         High    Launch the harbor site  · you · 1/2 sub-tasks done
Sub-tasks
  S-2    Todo         High    Book the venue  · you · under S-1
  S-3    Done         High    Order the banners  · you · under S-1

$ work update S-4 --parent S-1 --operation-id 11111111-1111-4111-8111-111111111111 --json
{"action":"updated","operation_id":"11111111-…","tasks":[{"handle":"S-4","revision":2, …
  "parent":{"handle":"S-1", …}}],"skipped":[]}
$ (same command again)
{"action":"updated","operation_id":null,"tasks":[],"skipped":[{"handle":"S-4","reason":"nothing to change"}]}

$ work update S-1 --parent S-2
invalid_request: S-1 cannot go under S-2 Book the venue: a task cannot sit under itself or one of its own sub-tasks.

$ work update S-4 --parent none
S-4    Todo         -       Loose task

$ work comment add S-1 --body "Who books the venue?" --dry-run
Would comment on S-1 Launch the harbor site
$ work comments S-1 --json
{"schema_version":6,"task":{"handle":"S-1", …},"threads":[],"next_cursor":null}

$ work comment add S-1 --body "Who books the venue?" --operation-id 22222222-2222-4222-8222-222222222222 --json
{"action":"added","operation_id":"22222222-…","comment":{"id":"e7f77ae7-…","reply_to":null,
  "author":{"kind":"agent","name":"Agent"},"body":"Who books the venue?", …},"replayed":false}
$ (same command again)
{"action":"added","operation_id":"22222222-…","comment":{"id":"e7f77ae7-…", …},"replayed":true}

$ work comment add S-1 --body "Dana books it" --reply-to e7f77ae7-… --agent Organizer     -> 34e1d845-…
$ work comment add S-1 --body "Confirmed with Dana" --reply-to 34e1d845-… --agent Organizer --json
{"action":"added","comment":{"id":"b3d85b70-…","reply_to":"e7f77ae7-…",   # joined the root
  "author":{"kind":"agent","name":"Organizer"}, …}}

$ work comment-edit 34e1d845-… --body "Dana books it **this week**" --agent Organizer
Edited a comment on S-1 Launch the harbor site
  2026-11-10 15:46 -08:00  Organizer   (edited)  id 34e1d845-…
    Dana books it **this week**
$ work comment-edit 34e1d845-… --body "Not mine to edit"
invalid_request: Only whoever wrote a comment can change it.

$ work comment resolve 34e1d845-… --dry-run
Would resolve a thread on S-1 Launch the harbor site
$ work comments S-1                  # still open after the dry run
  2026-11-10 15:46 -08:00  Agent    id e7f77ae7-…
    Who books the venue?
      … Organizer   (edited)  id 34e1d845-…   Dana books it **this week**
      … Organizer    id b3d85b70-…   Confirmed with Dana
$ work comment-resolve 34e1d845-…
Resolved a thread on S-1 Launch the harbor site
  2026-11-10 15:46 -08:00  Agent   (resolved)  id e7f77ae7-…
$ work comment-resolve e7f77ae7-… --reopen --json
{"action":"reopened","comment":{"id":"e7f77ae7-…","resolved_at_ms":null,"replies":[…2…]}, …}

$ work comment-delete e7f77ae7-… --dry-run
Would delete a comment on S-1 Launch the harbor site   (body still shown)
$ work comment-delete e7f77ae7-…
Deleted a comment on S-1 Launch the harbor site
$ work comments S-1 --json
{"threads":[{"id":"e7f77ae7-…","body":null,"deleted":true,"replies":[
  {"id":"34e1d845-…","body":"Dana books it **this week**","edited_at_ms":1794354370384, …},
  {"id":"b3d85b70-…","body":"Confirmed with Dana", …}]}],"next_cursor":null}

$ work show S-1
S-1    Todo         High    Launch the harbor site  · you · 1/2 sub-tasks done
Sub-tasks
  S-2    Todo         High    Book the venue  · you · under S-1
  S-3    Done         High    Order the banners  · you · under S-1
Comments
  2026-11-10 15:46 -08:00  Agent  deleted  id e7f77ae7-…
      … Organizer   (edited)  id 34e1d845-…   Dana books it **this week**
      … Organizer    id b3d85b70-…   Confirmed with Dana
History
  … Agent  created: Todo
  … Agent  comment: e7f77ae7-…
  … Organizer  comment: 34e1d845-…
  … Organizer  comment: b3d85b70-…

$ work show S-4
History
  … Agent  created: Todo
  … Agent  parent: S-1 Launch the harbor site
  … Agent  parent: removed S-1 Launch the harbor site
```

Not verified: the bridge methods were compiled but not called from the app;
Suggest-mode agents (proposal for `--parent`, Triage for sub-tasks) were not
run in the smoke.
