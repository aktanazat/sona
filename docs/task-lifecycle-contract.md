# Task lifecycle: bin, archive, description history, favorites, inbox

Closes the must-have Issues rows for deleting tasks to a 30-day bin, the
archive page, description version history, favorites, subscriptions and the
inbox (`sona-linear-spec/LinearIssues-report.md`). Core, CLI and MCP are
built; the macOS UI comes later through the bridge commands named below.

## Storage

One migration, `LIFECYCLE_MIGRATION` (`planning/schema.rs`), appended last in
`managers/history.rs`:

- `planning_tasks.trashed_at_ms`: set while a task is in the bin. A binned
  task is also archived (`archived_at_ms = coalesce(archived_at_ms, now)`),
  so every list that leaves archived work out leaves it out, and
  `items::items` leaves binned tasks out even with archived ones included.
  Restore clears `trashed_at_ms`, and clears `archived_at_ms` only when the
  task was not archived before the delete.
- Purge: a task in the bin 30 days (`catalog::BIN_MS`) is deleted for good
  by the app tick (`cycles::tick`) and at the start of every delete or
  restore. Existing triggers remove its comments, links and attachments.
  A purged task cannot be restored (NotFound).
- `planning_task_versions(id, task_id, body, actor, activity_id,
  created_at_ms)`: each description edit through `items::apply` keeps the
  text it wrote; the first edit also keeps the text the task had before
  (actor NULL, "before"). The agent's name joins through
  `planning_activity_agents` on `activity_id`.
- `planning_favorites(kind task|project|view|person, target_id, folder_id,
  position, created_at_ms)`: `folder_id` NULL sits at the top.
- `planning_favorite_folders(id, name, position, created_at_ms)`: folders sit
  at the top and share its positions with the loose favorites; a folder's
  favorites count their own from 0. Folders hold favorites only. Deleting a
  folder puts its favorites at the top where it was.
- `planning_task_subscriptions(task_id, reason
  subscribed|created|assigned|mentioned, created_at_ms)`.
- `planning_notifications(id, task_id, kind status|comment|mention|assigned,
  actor sona|agent, activity_id, comment_id, from_value, to_value,
  created_at_ms, read_at_ms, snoozed_until_ms, archived_at_ms)`.
- Triggers drop a task's versions, subscription, notifications and favorite
  when the task row is deleted, and a project's or view's favorite with it.
  People live in the meetings database, so no trigger drops a person's
  favorite; readers skip a favorite whose target is gone.

History: delete records `archived` with to `bin`; restore from the bin
records `restored` with from `bin`. No new activity kinds.

## Who gets notified

The user is the inbox's one reader. `items::record` calls
`inbox::on_activity` for every history entry, in the same transaction:

- created by the user: subscribed (`created`).
- created by Sona or an agent with the user as owner: subscribed
  (`assigned`); an agent's also notifies `assigned`.
- owner changed to the user: subscribed (`assigned`); notifies `assigned`
  unless the user made the change.
- status, accept, decline, duplicate on a subscribed task by Sona or an
  agent: notifies `status` with from/to.
- a comment on a subscribed task by Sona or an agent: notifies `comment`.
- a comment by Sona or an agent that says `@me`, `@<first name>` or
  `@<full name>` (About me): subscribes (`mentioned`) and its notice becomes
  `mention`.
- anything the user did: never notifies.

Snoozed notifications stay out of the inbox and the unread count until they
wake; archived ones until asked for.

## CLI

Every write takes `--json`, `--agent`, `--operation-id`, `--dry-run`; a dry
run needs only the read scope. Delete and restore derive their operation id
from the tasks and revisions read; favorites, subscriptions and inbox marks
take a fresh one unless given, and an already-done change is listed under
`skipped` without writing. A repeated `--operation-id` replays the stored
receipt and answers `replayed: true`. Every write needs the external
mutations scope and, except the description restore, refuses while agents'
changes wait for the user; the description restore goes through
`agents::update_as` and is proposed under Suggest (`proposed: true`).
Tasks from meetings the user kept on this Mac stay hidden everywhere,
including the bin, the archive, favorites, subscriptions and the inbox.

| verb | does |
|---|---|
| `work delete TASK...` | to the bin |
| `work restore TASK...` | out of the bin when the tasks are there, else from the archive; mixing refused |
| `work bin` | deleted tasks, `days_left` before purge |
| `work archive-list` | archived tasks and deleted ones |
| `work description-versions TASK` | versions newest first, `current` marked |
| `work description-version VERSION` | one version |
| `work description-restore VERSION` | puts it back |
| `work favorite [TASK] [--project P] [--view V] [--person P]`, `work unfavorite ...` | favorites; a new one goes last at the top |
| `work favorites` | the sidebar's order, folders with their favorites |
| `work favorite-folder-save [--folder F] [--name N] [--position N]` | makes, renames or moves a folder |
| `work favorite-folder-delete FOLDER` | its favorites stay at the top |
| `work favorite-move [TASK] [--project P] [--view V] [--person P] [--into F \| --top] [--position N]` | moves one favorite |
| `work subscribe TASK...`, `work unsubscribe TASK...` | subscriptions |
| `work subscriptions` | with reason |
| `work inbox [--unread] [--snoozed] [--archived]` | notifications and `unread` |
| `work inbox-mark ID... --mark read\|unread\|archive\|unarchive\|snooze [--until DAY\|tomorrow\|none] [--all]` | marks |

## MCP (all opt-in)

`sona_work_delete`, `sona_work_restore` (now also the bin), `sona_work_bin`,
`sona_work_archive_list`, `sona_work_description_versions`,
`sona_work_description_version`, `sona_work_description_restore`,
`sona_work_favorite` / `sona_work_unfavorite` (`kind` task, project, view or
person, `target`), `sona_work_favorites`, `sona_work_favorite_folder_save`
(`folder`, `name`, `position`), `sona_work_favorite_folder_delete`
(`folder`), `sona_work_favorite_move` (`kind`, `target`, `into`, `top`,
`position`), `sona_work_subscribe`, `sona_work_unsubscribe`,
`sona_work_subscriptions`, `sona_work_inbox`, `sona_work_inbox_mark`
(`notifications` ids or `"all"`, `mark`, `until`), `sona_work_digest`
(`dry_run`).

## Bridge commands for the UI (not added yet)

Core functions are in place; each bridge command is a thin call with
`WorkActor::Me`.

| command | request | response | core |
|---|---|---|---|
| `work_delete` | `{operationId, ids: [String]}` | `[WorkItem]` | `bin::set_trashed_as(app, Me, op, ids, true)` |
| `work_undelete` | `{operationId, ids}` | `[WorkItem]` | `bin::set_trashed_as(.., false)` |
| `work_bin` | `{}` | `[{item: WorkItem, trashedAtMs, purgeAtMs}]` | `bin::binned(conn, now)` |
| `work_archive_list` | `{}` | `{archived: [WorkItem], deleted: [Binned]}` | `bin::archived`, `bin::binned` |
| `work_description_versions` | `{id}` | `[{id, itemId, body?, actor?, agent?, createdAtMs}]` | `versions::list` |
| `work_description_restore` | `{operationId, id, expectedRevision, versionId}` | `WorkItem` | `work_update` with `details` from `versions::version` |
| `work_favorite_set` | `{operationId, kind: task\|project\|view\|person, id, on}` | `bool` | `favorites::set_as` |
| `work_favorites` | `{}` | `{folders: [{id, name, position, createdAtMs}], favorites: [{kind, targetId, folderId?, position, createdAtMs}]}` | `favorites::favorites` |
| `work_favorite_folder_save` | `{operationId, id?, name?, index?}` | `{id, name, position, createdAtMs}` | `favorites::save_folder_as` |
| `work_favorite_folder_delete` | `{operationId, id}` | `null` | `favorites::delete_folder_as` |
| `work_favorite_move` | `{operationId, entry: {favorite: {kind, id}}\|{folder: {id}}, into?, index?}` | `bool` changed | `favorites::move_as` |
| `work_subscribe` | `{operationId, ids, on}` | `[String]` changed | `inbox::subscribe_as` |
| `work_subscriptions` | `{}` | `[{itemId, reason, createdAtMs}]` | `inbox::subscriptions` |
| `work_inbox` | `{unreadOnly?, snoozed?, archived?}` | `{unread, notifications: [{id, itemId, kind, actor, agent?, commentId?, from?, to?, createdAtMs, readAtMs?, snoozedUntilMs?, archivedAtMs?}]}` | `inbox::notifications` |
| `work_inbox_mark` | `{operationId, ids, mark: read\|unread\|archive\|unarchive\|snooze, untilMs?}` | `[Notification]` | `inbox::mark_as` |

The core structs serialise snake_case today; the bridge should map them the
way `dispatch.rs` maps the other work types.

## Verified

Scratch home `/private/var/tmp/sona-scratch-lifecycle/`, invented tasks, the
debug `sona` built from this tree, `smoke.sh` (CLI) and `mcp-smoke.ts` (MCP
stdio, `SONA_MCP_TOOLS=all`), 2026-11-11:

- `delete --dry-run`: "Would move to the bin", then `trashed after dry run: 0`.
- `delete --operation-id U` twice: second answers `replayed:true`; one
  receipt, one `bin` history row. Same through MCP.
- `list` leaves the deleted task out; `bin` shows "purged in 30 days";
  `archive-list` shows the archived task and the deleted one; restoring both
  in one call is refused (`invalid_request`); each restores alone.
- Description: two agent edits give three versions (original, Codex, Claude
  Code); restoring the original brings "First draft of the plan" back and
  adds a fourth version. Same through MCP.
- Favorites: task, then again (skipped "already a favorite"), view;
  unfavorite leaves only the view.
- Subscriptions: agent-created task owned by the user is subscribed
  (`assigned`); `subscribe` again is skipped.
- Inbox: agent status change, agent comment, `@me` comment by another agent
  (`mention`), agent assigning the user: 5 unread. A dry-run mark leaves 5;
  the mark leaves 4; repeating its operation id answers `replayed:true`.
  Snooze hides it until tomorrow (shown with `--snoozed`), archive hides it
  (shown with `--archived`), `--all --mark read` leaves 0. After
  `unsubscribe`, a new agent comment adds nothing.
- Purge: a binned task aged 31 days is gone from the bin, `restore` answers
  `not_found`, and its row is deleted.
- MCP: every new tool listed with the right read-only and destructive hints;
  each call above answered through stdio.

Unit tests (`planning::work::tests`):
`restoring_from_the_bin_brings_back_whether_the_task_was_archived` and
`the_inbox_hears_what_others_change_on_the_users_tasks_and_not_the_user`
(the user's own status change does not notify; only the agent's does).

Not verified: the user's own change through the app (the CLI only writes as
an agent; covered by the unit test), the `@<name>` mention form (needs a
user name in settings), Suggest mode refusals and the proposed description
restore, and project favorites by name.
