# Sona UI, CLI and MCP parity

The rule: everything the app's UI can do, the command line can do too, and
more. Consent, privacy and security toggles stay in the UI on purpose.

This file is the summary table. The four inventories beside it keep the
Swift file:line evidence for each row:
[tasks.md](tasks.md), [projects-people.md](projects-people.md),
[meetings-notes-search.md](meetings-notes-search.md),
[chat-settings-rest.md](chat-settings-rest.md).

Checked 2026-11-11 on branch `work/people-projects`, against:

- bridge methods: `src-tauri/src/native_bridge/dispatch.rs`
- CLI: `sona work --help` and `sona --help` from the debug core
  (`src-tauri/src/query/external/tracker.rs`, `WorkVerb`)
- MCP: `tools/sona-mcp/src/tools.ts`. A tool marked *(opt-in)* is published
  only when `SONA_MCP_TOOLS` names it or is `all`; the rest are published by
  default.

Every CLI read needs Settings > Agents > External access, and every write
also needs External mutations. Agent writes wait for the owner's accept
unless agent changes are allowed.

Status values:

- **both**: the UI and the CLI can each do it.
- **CLI only**: the CLI can do it and the UI cannot (the "and more").
- **missing**: the UI can do it and the CLI cannot yet.
- **UI-only by design**: kept out of the CLI on purpose; the reason follows.
- **window-bound**: only makes sense with a window, the microphone, the
  pasteboard, Finder or audio output; the reason follows.

The MCP column says whether an agent over MCP can do it; `—` means no tool.

## Tasks

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Open Tasks; tabs I owe, Waiting on, All; sidebar counts | `work_list`, `work_counts` | `work list [--mine\|--waiting\|--unowned]`, `work mine`, `work counts` | `sona_work_list`, `sona_work_counts` | both |
| What to pick up next (todo, unblocked, not snoozed) | — | `work ready` | `sona_work_list` (`ready`) | CLI only |
| Include archived tasks in a list | — | `work list --archived` | `sona_work_list` (`archived`) | CLI only |
| New task: composer, group header +, quick entry | `work_create` | `work create TITLE --details --status --priority --owner --due --repeat --project --milestone --labels` | `sona_work_create` | both |
| Typed words read as fields (owner, due, "every monday") with chips | `work_parse` | `work parse WORDS [--spoken] [--plain WORD]` | `sona_work_parse` *(opt-in)* | both |
| Quick entry keeps the page it was pressed over as the task's link | `work_capture` | `work create TITLE --source-url URL [--source-title T] [--dry-run]` | `sona_work_capture` *(opt-in)* | both |
| Quick entry words read as a change | `work_command` | `work say "…" [--dry-run]` | `sona_work_say` | both |
| Reschedule overdue (Overdue group button, ⌘K Reschedule overdue) | `work_bulk_update` | `work list --due overdue`, then `work update … --due DAY` | `sona_work_list`, `sona_work_update` | both |
| Drag tasks onto a board column or group | `work_update`, `work_bulk_update` | `work update --status/--priority/--owner/--project/--due` | `sona_work_update` | both |
| Palette: find a task (title, description or number), project, person or label; `/` opens it, O I recent tasks, O U people, O L labels | none (matches loaded rows) | `work list --search`, `work projects`, `work people --query`, `work labels` | `sona_work_list` (`search`), `sona_project_list`, `sona_person_list`, `sona_work_labels` *(opt-in)* | both |
| Copy ID / Copy link / Copy as Markdown / Copy create link (`sona://new?title=…&details=…&priority=…&project=…&labels=…&owner=…&due=…` opens the new-task sheet filled in) | none (pasteboard, `TaskCopy`) | `work show S-N [--json]`; `work export --md DIR` writes Markdown; `work create --title … --priority …` makes what a create link fills in | `sona_work_get`, `sona_work_create` | window-bound: pasteboard; the text itself is readable |
| Export matching tasks as Markdown files | — | `work export --md DIR` + list filters | `sona_work_export` *(opt-in)* | CLI only |
| Selection, peek, collapse groups, open filter/display panels | none | — (verbs take several task refs) | — | window-bound: window state |
| Undo (⌘Z, toast) | inverse of the last call | — (inverse verbs: `work update`, `work archive`, `work restore`) | — | window-bound: the undo stack lives in the window |
| Spoken ramble to Triage; dictated change | events `work-ramble-filed`, `work-command-heard` | typed: `work say` | `sona_work_say` | window-bound: microphone |

## Task page

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Open a task: quote, evidence, suggestions, activity (an agent's entries say whether it came through MCP or the CLI; the first entry names the meeting it was heard in), relations, reminder, agent sessions, `sona://task/<id>` link | `work_item` | `work show S-N` (prints the reminder, agent sessions, `via`, the meeting and `link`) | `sona_work_get` (`link`, `reminder`, `sessions`, `activity[].via`, `activity[].meeting_title`) | both |
| Status picker | `work_update` | `work update --status`, `work done` | `sona_work_update`, `sona_work_done` | both |
| Mark done with an evidence link | `work_update` | `work done --evidence URL` | `sona_work_done` (`evidence`) | CLI only (the evidence link) |
| Agent starts a task as itself | — | `work claim [--dry-run]` | `sona_work_claim` (`dry_run`) | CLI only |
| Agent session on a task: thoughts, actions, questions with choices, answer or error, plan checklist, heartbeat; list one task's or every task's sessions | `work_sessions`, `work_session` (bridge, no screen yet) | `work session-start`, `work session-activity --type …`, `work session-plan`, `work session-heartbeat`, `work session`, `work sessions [TASK] [--open --agent]` | `sona_session_start`, `sona_session_activity`, `sona_session_plan`, `sona_session_heartbeat`, `sona_session_get`, `sona_session_list` | CLI only |
| Ask an agent to stop its session; answer its question | `work_session_stop`, `work_session_reply` (bridge, no screen yet) | `work session-stop`, `work session-reply [--body --value]` | `sona_session_stop` *(opt-in)* | CLI only |
| Change a task in one save, ordered steps, all or none; or make one; with delegate, blocks/blocked_by/related, duplicate_of, links and source steps saved after the fields | — | `work save [TASK] --patch-json JSON [--expected-revision --dry-run]` | `sona_task_save` (`task` optional) | CLI only |
| Follow every change since a cursor, with the values before: tasks, projects, milestones, cycles, views, labels, project updates, notes (not their text), meetings Sona took work from, comments on tasks, projects, milestones, meetings and notes, and agent sessions; anything tied to a meeting kept on this Mac is left out for agents | Tasks bar → Recent changes (clock): the newest changes, a task's opens it (`work_changes`, `latest`) | `work changes [--since --limit]` | `sona_work_changes` | both |
| Tasks delegated to an agent | — | `work list --delegate AGENT\|any\|none` | `sona_work_list` (`delegate`) | CLI only |
| Priority, owner, assign to me, due, labels, project, milestone, cycle | `work_update`, `work_bulk_update` | `work update --priority --owner --due --labels --add-label --remove-label --project --milestone --cycle` | `sona_work_update` | both |
| Rename, edit details | `work_update` | `work update --title`, `--details` | `sona_work_update` | both |
| Repeats; finishing a repeating task makes the next copy | `work_update` (`recurrence`); the Repeats menu also reads a typed rule ("every 2 weeks") through `work_parse` | `work update --repeat WORDS\|none`, `work create --repeat` | `sona_work_update` (`repeat`) | both |
| Task templates: list, save, edit fields (title, details, priority, owner, project, labels, repeat, sub-tasks, due), delete, make a task with its sub-tasks from one (⌥C opens the new-task sheet on its template list); due N days after it is made | `work_templates`, `work_template_save` (`dueInDays`; Save as template and Settings › Tasks › Edit set it; Edit also sets `labelIds` and `recurrence`), `work_template_delete`, `work_template_use` | `work templates`, `work template-save NAME --labels L,… --repeat WORDS --subtask T --due-in DAYS\|none`, `work template-delete`, `work template-use T [--title] [--due]` | `sona_work_templates`, `sona_work_template_save` (`labels`, `repeat`, `due_in_days`), `sona_work_template_delete`, `sona_work_template_use` | both |
| Reminder on a task: set (H off Triage, or ⌘K Remind me, with a typed time such as "fri 10am"), clear, list, due now; firing posts an inbox notice once | `work_reminder_set`, `work_reminders` (`dueOnly`); the background tick posts the `reminder` notice and marks it notified | `work remind TASK WHEN\|none`, `work reminders [--due]`, `work inbox` (`reminder`) | `sona_work_remind`, `sona_work_reminders`, `sona_work_inbox` | both |
| Snooze to a day | `work_bulk_update` | `work snooze --until DAY`, `work update --snooze-until DAY` | `sona_work_update` (`snooze_until`) | both |
| Snooze "Later today" (a time) | `work_bulk_update` | `work snooze TASK --until later-today`, `work update --snooze-until later-today` | `sona_work_update` (`snooze_until`) | both |
| Archive | `work_archive` | `work archive` | `sona_work_archive` *(opt-in)* | both |
| Restore (undo of archive) | `work_restore` | `work restore` | `sona_work_restore` *(opt-in)* | both |
| Delete to the bin (⌘⌫, ⌘K, bulk bar); restore within 30 days (Archive page Restore or #), then purged | `work_delete`, `work_restore` | `work delete`, `work restore` | `sona_work_delete`, `sona_work_restore` *(opt-in)* | both |
| Bin and Archive page (archived and deleted tasks, days left; J/K move, # restores) | `work_bin`, `work_archive_list` | `work bin`, `work archive-list` | `sona_work_bin`, `sona_work_archive_list` *(opt-in)* | both |
| Description history: list, show, restore a version | `work_description_versions`, `work_description_restore` (task bar clock, ⌘K Description history) | `work description-versions TASK`, `work description-version ID`, `work description-restore ID` | `sona_work_description_versions`, `sona_work_description_version`, `sona_work_description_restore` *(opt-in)* | both |
| Favorite a task, project, view or person; list favorites | `work_favorite_set` (star in the task bar, on a project page and on a person page, ⌥F on a task or a project, ⌘K) | `work favorite [TASK] [--project --view --person]`, `work unfavorite`, `work favorites` | `sona_work_favorite`, `sona_work_unfavorite`, `sona_work_favorites` *(opt-in)* | both |
| Favorites folders and order | `work_favorite_folder_save`, `work_favorite_folder_delete`, `work_favorite_move` (sidebar: drag a favorite or folder to reorder or into a folder; right-click Move to Folder, New Folder, Rename, Delete Folder) | `work favorite-folder-save [--folder --name --position]`, `work favorite-folder-delete FOLDER`, `work favorite-move [TASK] [--project --view --person] [--into F \| --top] [--position N]` | `sona_work_favorite_folder_save`, `sona_work_favorite_folder_delete`, `sona_work_favorite_move` *(opt-in)* | both |
| Subscribe, unsubscribe (auto on create, assign, mention) | `work_subscribe` (bell in the task bar, ⇧S, ⌘K) | `work subscribe`, `work unsubscribe`, `work subscriptions` | `sona_work_subscribe`, `sona_work_unsubscribe`, `sona_work_subscriptions` *(opt-in)* | both |
| Inbox: notifications (status, comment, mention, assigned, an agent's other field changes, reminder, due today, automation run ended), unread count, read/unread, mark all read, clear read, snooze, archive, peek | Inbox screen (`work_inbox`, `work_inbox_mark`): Space peek, U, ⌥U mark all read, H, ⌫, ⇧⌫ clear read | `work inbox [--unread --snoozed --archived]`, `work inbox-mark ID… --mark … [--until]`, `work inbox-mark --all --mark read\|archive` | `sona_work_inbox`, `sona_work_inbox_mark` (`notifications: "all"`) *(opt-in)* | both |
| Relations: blocks, blocked by, related; remove; a finished blocker reads as related; `S-12` written in a description or comment links that task as related | `work_relation_add`, `work_relation_remove`; mentions link through `work_update`, `work_comment_add`, `work_comment_edit` | `work relate S-1 blocks\|blocked-by\|related S-2`, `work unrelate`; mentions through `work update --details`, `work comment` | `sona_work_relate`, `sona_work_unrelate` *(opt-in)*; mentions through any description or comment write | both |
| Set or remove the parent | `work_update` (`parentId`) | `work update --parent TASK\|none` | `sona_work_update` (`parent`) | both |
| Estimate: points 0 to 64 (offered 1, 2, 4, 8, 16), shown in the row and kept in history | `work_update` (`estimate`); sidebar Estimate, ⇧E, ⌘K Set estimate | `work update --estimate N\|none`, `work save --patch-json '[{"field":"estimate","value":3}]'` | `sona_work_update` (`estimate`), `sona_task_save` (field `estimate`) | both |
| Who the work is for: the person owed it, kept apart from its owner; the person page lists it under Tasks for them | `work_update` (`forPersonId`); sidebar For, ⌘K Set who it’s for; person page Tasks for them | `work update --for PERSON\|none`, `work list --for PERSON`, `work save` (field `for`) | `sona_work_update` (`for`), `sona_work_list` (`for`), `sona_task_save` (field `for`) | both |
| When a task was completed: set when it is marked done, cleared when it reopens; older done work takes it from its history | `work_list` (`completedAtMs` on each task); sidebar Completed line | `work show` (`completed_at_ms`), `work list --completed WHEN` | `sona_work_get`, `sona_work_list` (`completed`) | both |
| Add sub-tasks; make them from the description's open `- [ ]` lines | `work_subtask_create`; the details' checklist button or right-click Make sub-tasks from the checklist | `work subtask PARENT TITLE… [--owner --priority]`, `work subtask PARENT --from-checklist` | `sona_subtask_create` (`from_checklist`) | both |
| Read comment threads on a task, a project, a milestone, a meeting or a note; a meeting kept on this Mac reads as absent to agents, and the app shows its threads without agents' comments | `work_comments` (`subject`: `project`, `milestone`, `meeting`, `note`; `keptLocal` on the page); project page Comments section, milestone row's Comments sheet, meeting review Comments section, note page Comments section | `work comments TASK`, `work comments --project P [--milestone M]`, `--meeting ID`, `--note ID` | `sona_comment_list` (`task`, `project` + `milestone?`, `meeting` or `note`) | both |
| Comment, or reply in a thread | `work_comment_add` (`subject`) | `work comment add TASK\|--project P [--milestone M]\|--meeting ID\|--note ID [--reply-to]` | `sona_comment_save` (`task`, `project` + `milestone?`, `meeting` or `note`) | both |
| Edit own comment | `work_comment_edit` | `work comment edit`, `work comment-edit` | `sona_comment_save` (`id`) | both |
| Copy a comment's link (`sona://task/<id>?comment=<id>`) | none (comment's Copy link, `TaskCopy.commentLink`) | `work comments TASK` prints each task comment's link; `--json` carries it as `link` | `sona_comment_list`, `sona_comment_save` (`link` on a task's comment) | both (the link text; the pasteboard itself is window-bound) |
| Delete own comment | `work_comment_delete` | `work comment delete`, `work comment-delete` | `sona_comment_delete` *(opt-in)* | both |
| Resolve or reopen a thread | `work_comment_resolve` | `work comment-resolve [--reopen]` | `sona_comment_resolve` | both |
| List attachments, with each one's key-value metadata and the comment it was added with | `work_attachments` (tile lists the metadata; a comment's files show under that comment, not in Attachments) | `work attachments TASK` | `sona_task_attachments` | both |
| Attach a file | `work_attachment_attach_file` | `work attach TASK PATH [--title --subtitle]` | `sona_task_attach` (`path`) | both |
| Files on a comment | `work_attachment_attach_file` (`commentId`; the comment box's paperclip) | `work attach TASK PATH --comment COMMENT_ID` | `sona_task_attach` (`comment`) | both |
| Key-value metadata on an attachment; attaching the same file or link again replaces it, for whoever attached it | display only: the tile lists it | `work attach TASK PATH\|URL --meta KEY=VALUE…` | `sona_task_attach` (`metadata`) | CLI and MCP set it; the app shows it |
| Attach a link | `work_attachment_attach_link` (Add link form: title, subtitle; the tile shows the subtitle) | `work attach TASK URL [--title --subtitle]` | `sona_task_attach` (`url`) | both |
| Rename an attachment | `work_attachment_rename` | `work attachment-rename TASK ID --title` | `sona_work_attachment_rename` *(opt-in)* | both |
| Detach | `work_attachment_detach` | `work detach TASK ID` | `sona_task_detach` *(opt-in)* | both |
| Open an attachment, Copy path, Copy link | none | — | — | window-bound: Finder, pasteboard |
| Delegate to a named agent (copies the hand-off prompt) | `work_delegate` | `work delegate TASK --to AGENT`, `--take-back`; prints the prompt | `sona_work_delegate` *(opt-in)* | both |
| Book time / unbook | `work_time_block`, `work_time_block_remove` | `work book TASK`, `work unbook TASK` | `sona_work_book`, `sona_work_unbook` *(opt-in)* | both |
| Nudge: record it (Copy) | `work_nudged` | `work nudged TASK` | `sona_work_nudged` *(opt-in)* | both |
| Nudge: open a Mail draft | `work_nudge_mail` | — | — | window-bound: Mail compose window |
| Accept or reject an agent's proposed change | `work_proposals_accept`, `work_proposals_reject` | — | — | UI-only by design: the owner's approval of agent changes |
| Play the moment's audio; open the source meeting | audio, navigation | read it: `work meeting ID --transcript` | `sona_meeting_get` | window-bound: audio output |

## Triage

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Open Triage | `work_list` | `work list --status triage` | `sona_work_list` (`status`) | both |
| Group by source meeting | `work_list` | `work list --status triage --meeting ID` | `sona_work_list` (`meeting_id`) | both |
| Source splits: meetings, dictation, agents, people | none (client filter on source) | `work list --source meeting,dictation,agent,person` | `sona_work_list` (`source`) | both |
| Accept (with Sona's suggestions) | `work_triage_accept` | `work accept [--ignore-suggestions]` | `sona_work_triage` (`accept`) | both |
| Sona's suggestions, each with its reason (project, labels, owner, due day, duplicate or related task): use or dismiss one, on a Triage chip or the task page | `work_list` / `work_item` (`suggestions`), `work_suggestion_accept`, `work_suggestion_dismiss` | `work triage-suggestions [TASK]`, `work suggestion-accept ID`, `work suggestion-dismiss ID` | `sona_work_triage_suggestions`, `sona_work_suggestion_accept`, `sona_work_suggestion_dismiss` | both |
| Decline | `work_triage_decline` | `work decline` | `sona_work_triage` (`decline`) | both |
| Decline with a reason kept in history | `work_triage_decline` (`reason`: the decline picker reads what is typed into it as the reason) | `work decline --reason TEXT`; `work show` prints it | `sona_work_triage` (`reason`) | both |
| Duplicate of | `work_mark_duplicate` | `work duplicate TASK --of TASK [--dry-run]` | `sona_work_triage` (`duplicate`, `dry_run`) | both |
| Snooze | `work_bulk_update` | `work snooze --until DAY` | `sona_work_update` (`snooze_until`) | both |
| Ask every conversation; take answer lines into Triage | `work_ask`, `work_take_lines` | `work ask "Q"`, `work ask "Q" --take=1,3 [--dry-run]` | `sona_work_ask`, `sona_work_take` (`dry_run`) | both |
| Agents' proposals group | `work_proposals_accept`, `work_proposals_reject` | — | — | UI-only by design: human approval gate |

## Projects

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| List projects | `planning_projects` | `work projects [--all]`, `sona --projects` | `sona_project_list`, `sona_projects` | both |
| Filter or search the list (status, overdue, mine) | none (client filter) | `work projects`, then `work list --project P --due overdue` | `sona_project_list`, `sona_work_list` | both |
| Bin: list projects deleted in the last 30 days | `planning_projects_trashed` | `work projects --trashed` | `sona_project_list` (`trashed`) | both |
| Open a project: fields, latest update, progress, predicted finish | `planning_project` | `work project P`, `sona --project UUID` | `sona_project_detail`, `sona_project_get` | both |
| New project | `planning_create_project` | `work project-save NAME --details` | `sona_project_save` | both |
| Status (menu, board drag, detail picker) | `planning_update_project`, `planning_project_save` | `work project-save NAME --status` | `sona_project_save` | both |
| Priority, lead, members (you among them: "Me", `me`), start and target with resolution, labels, colour, icon, links, description | `planning_project_save` (`leadIsSelf`, `membersIncludeSelf`) | `work project-save NAME --priority --lead me\|PERSON\|nobody --members me,PERSON --start --start-resolution --target --target-resolution --labels --hue --icon --link --details` | `sona_project_save` | both |
| Rename a project (click its name on the project page) | `planning_project_save` (`title`) | `work project-save NEW --project OLD` | `sona_project_save` | both |
| Put away (archive) or bring back | — | `work project-archive [--restore]` | `sona_project_archive` *(opt-in)* | CLI only |
| Move to the bin, restore from the bin | `planning_project_save` (`trashed`) | `work project-delete [--restore]` | `sona_project_delete` *(opt-in)* | both |
| Summary: a short line under the name, apart from the description | `planning_project_save` (`summary`) | `work project-save NAME --summary TEXT\|none` | `sona_project_save` (`summary`) | both |
| Status names inside the six categories: list, add, rename, move to another category, delete; set one on a project | `planning_project_status_names`, `planning_project_status_name_save`, `planning_project_status_name_delete`, `planning_project_save` (`statusNameId`) | `work project-statuses`, `work project-status-save NAME --category C [--status OLD]`, `work project-status-delete NAME`, `work project-save NAME --status-name NAME\|none` | `sona_project_status_list`, `sona_project_status_save`, `sona_project_status_delete` *(opt-in)*, `sona_project_save` (`status_name`) | both |
| Filter: latest update posted before a day | `planning_projects` (`latestUpdateAtMs`; client filter) | `work projects --update-before DAY` | `sona_project_list` (`update_before`) | both |
| History: title, status, status name, lead, members, dates, priority and milestone changes | `planning_project_history` | `work project-history P [--limit N]` | `sona_project_history` | both |
| Linked notes, meetings and links on the project | `planning_project` | `work project P` (`sources`) | `sona_project_detail` (`sources`) | both |
| Edit the description or summary in place at an anchor | — (agents only) | `work project-save NAME --patch-json '[{"field":"description","op":"insert_after","anchor":"…","value":"…"}]'` | `sona_project_save` (`patch`) | CLI only |
| Add a task in the project | `planning_create_task` | `work create TITLE --project P --milestone M --owner --due` | `sona_work_create` | both |
| Change a task's status in the project | `planning_update_task` | `work update --status`, `work done` | `sona_work_update`, `sona_work_done` | both |
| Link a meeting, note or loop as a source | `planning_link_source` | `work source-link TASK\|PROJECT --meeting ID [--at SECONDS] \| --note ID \| --loop ID` | `sona_planning_link_source` *(opt-in)* | both |
| New note about a task (task More menu) | `scratchpad_create` then `planning_link_source` | — (by hand: make the note, then `work source-link TASK --note ID`) | — (by hand: same two steps) | both; linking needs the task to be in a project |
| Review links: list, accept, dismiss suggestions | `planning_suggestions`, `planning_accept_suggestion`, `planning_decline_suggestion` | `work link-suggestions [--all]`, `work link-accept ID`, `work link-dismiss ID` | `sona_work_link_suggestions`, `sona_work_link_accept`, `sona_work_link_dismiss` *(opt-in)* | both |
| Review links: generate suggestions | `planning_generate_suggestions` | — | — | window-bound: runs the local model inside the app |
| Plan with Sona: draft, revise | `planning_prepare`, `planning_revise` | — (by hand: `work project-save`, `work milestone-save`, `work create`) | — | window-bound: runs the local model inside the app |
| Plan with Sona: create from draft | `planning_accept` | `work plan-accept --draft-file PATH\|- \| --draft JSON` | `sona_work_plan_accept` *(opt-in)* | both |
| Planner calendar (milestones and tasks by date range) | `planning_calendar` | `work calendar [--from DAY] [--to DAY]` | `sona_work_calendar` *(opt-in)* | both |
| Timeline: projects as bars from start to target, milestones, today; zoom week, month, quarter, year; drag a bar to move both dates, or one end to move that date | `planning_projects`, `planning_project`, `planning_project_save` | `work timeline [--from DAY] [--to DAY]`, `work project-save P --start DAY --target DAY` | `sona_project_timeline`, `sona_project_save` | both |
| Project dependencies: blocked by, blocking; add, open, remove from the project's properties, its ⋯ menu, a list row's or timeline bar's menu, Cmd K on a project page ("Blocked by …", "Blocking …", "Remove link: …"), or by dragging the circle past a timeline bar's end onto another bar; a broken one (the blocker ends after the blocked project starts) in red; a repeat says it was already linked | `planning_project_dependencies`, `planning_project_dependency_add`, `planning_project_dependency_remove` | `work project-relate P blocked-by\|blocking OTHER [--dry-run]` ("Already linked" on a repeat), `work project-unrelate P blocked-by\|blocking OTHER`, `work project-dependencies [P]`; `work project P` (`blocked_by`, `blocking`) | `sona_project_relate` (`already`), `sona_project_unrelate`, `sona_project_dependency_list`; `sona_project_detail` (`blocked_by`, `blocking`) | both |
| Filter: blocked, blocking, broken dependency | `planning_project_dependencies` (client filter) | `work projects --link blocked\|blocking\|broken` | `sona_project_list` (`link`) | both |
| Dependency lines on the timeline: blue when kept, red when broken; click one to open either project or remove it | `planning_project_dependencies`, `planning_project_dependency_remove` | `work timeline` (`dependencies`, each with `broken`) | `sona_project_timeline` (`dependencies`) | both |
| Project templates: Save as template on a project page (description, lead, members, milestones as days after the start, tasks with priorities, labels and estimates; a name in use is refilled); New project sheet › Template with a start day; delete (the trash button beside the New project sheet's Template choice) | `planning_project_templates`, `planning_project_template_save`, `planning_project_from_template`, `planning_project_template_delete` | `work project-template-list`, `work project-template-save NAME --project P [--dry-run]`, `work project-new NAME --template T [--start DAY] [--dry-run]`, `work project-template-delete T [--dry-run]` | `sona_project_template_list`, `sona_project_template_save`, `sona_project_new`, `sona_project_template_delete` | both |
| Initiatives: goals above projects with a name, details, owner, target day and status (planned, active, completed); a sidebar page listing each with its projects done and tasks done; an initiative page to edit them, add, open or take out projects, and delete; the project page's Initiative row sets, moves or clears it (a project sits in one at most) | `planning_initiatives`, `planning_initiative_save`, `planning_initiative_delete`, `planning_initiative_add_project`, `planning_initiative_remove_project` | `work initiative-list`, `work initiative-show I`, `work initiative-new NAME [--details T] [--owner PERSON] [--target DAY] [--status S] [--dry-run]`, `work initiative-update I [--name N] [--details T] [--owner PERSON] [--target DAY] [--status S] [--dry-run]`, `work initiative-delete I [--dry-run]`, `work initiative-add-project I P [--dry-run]`, `work initiative-remove-project I P [--dry-run]` | `sona_initiative_list`, `sona_initiative_get`, `sona_initiative_new`, `sona_initiative_update`, `sona_initiative_delete`, `sona_initiative_add_project`, `sona_initiative_remove_project` | both |
| Insights tab: lead time (created to done) and triage time (until accepted or declined) as median and p90 over 7, 30, 90 or 365 days, by week, narrowed by label and owner | `work_insights` (`query`, `days`) | `work insights [--days N] [--project P] [--label L] [--owner PERSON]` | `sona_work_insights` | both |
| Nighttime review: allow, choose sources, turn off | `planning_quiet_review_set`, `planning_quiet_review_grants` | — | — | UI-only by design: consent for unattended reading |
| Nighttime review run or cancel | `planning_quiet_review_run`, `planning_quiet_review_cancel` | — | — | window-bound: the scheduler needs an idle, awake Mac |

## Milestones

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| List with target days, task counts, progress | `planning_project` | `work milestones P`, `work project P` | `sona_milestone_list`, `sona_project_detail` | both |
| Add a milestone | `planning_create_milestone` | `work milestone-save P NAME --target DAY` | `sona_milestone_save` | both |
| Change status | `planning_update_milestone` | `work milestone-save P NAME --status` | `sona_milestone_save` | both |
| Drag to reorder | `planning_milestone_reorder` | `work milestone-reorder P M1 M2 …` | `sona_milestone_reorder` | both |
| Move the target (drag its diamond on the projects timeline) | `planning_milestone_save` | `work milestone-save P NAME --target DAY` | `sona_milestone_save` | both |
| Rename, details, target day (Edit milestone on the project page) | `planning_milestone_save` | `work milestone-save P NEW --milestone OLD --details` | `sona_milestone_save` | both |
| Delete (its tasks stay; its comments go) | `planning_milestone_delete` (milestone row's menu › Delete milestone…, after a confirm) | `work milestone-delete` | `sona_milestone_delete` *(opt-in)* | both |
| Read one milestone | `planning_project` | `work milestone P M` | `sona_milestone_get` | both |
| Turn a milestone into a project (its tasks move; the milestone is deleted) | `planning_milestone_to_project` | `work milestone-to-project P M [--dry-run]` | `sona_milestone_to_project` *(opt-in)* | both |

## Project updates

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Read the update feed | `planning_project_updates` | `work project-updates P [--limit --cursor]` | `sona_project_update_list` | both |
| Update due, health, latest update | `planning_project` | `work project P` | `sona_project_detail` | both |
| Draft from what moved, and from meetings linked to the project or its tasks since the last update (never a meeting kept on this Mac) | `planning_project_update_draft` | `work project-update-draft P` | — (`sona_project_update_save` without `body` posts the draft) | both |
| Post an update with health | `planning_project_update_add` | `work project-update P --health on-track\|at-risk\|off-track [--body]` | `sona_project_update_save` | both |
| Edit own update | `planning_project_update_edit` | `work project-update-edit ID [--health --body]` | `sona_project_update_save` (`id`) | both |
| Delete own update | `planning_project_update_delete` | `work project-update-delete ID` | `sona_project_update_delete` *(opt-in)* | both |

## Cycles

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Cycle list: every cycle under way, ahead and ended, each opening its own page (Cycle tab › All cycles; J/K move, Space peeks, Return opens, Esc on a page goes back) | `work_cycles` | `work cycles` | `sona_cycle_list` *(opt-in)* | both |
| Capacity: what a cycle usually finishes, the mean done of the last three ended; a dial on upcoming cycles against their scope | `work_cycles` (`capacity`) | `work cycles` (`capacity`) | `sona_cycle_list` (`capacity`) *(opt-in)* | both |
| Cycle page graph: scope, started, done, daily finished bars and a target line flat on weekends | `work_cycle` (`days`, `target`) | `work cycle current\|next\|N` (`days`, `target`) | `sona_cycle_list` (`cycle`) *(opt-in)* | both |
| Cycle settings: length, start day, cooldown, how many ahead, auto-add started and finished work with no cycle | `work_cycle_settings_save` | `work cycle-settings --weeks --start-weekday --cooldown --upcoming --auto-add-started BOOL --auto-add-completed BOOL` | `sona_cycle_save` (`auto_add_started`, `auto_add_completed`) *(opt-in)* | both |
| Rename a cycle | `work_cycle_save` | `work cycle-save CYCLE --name` | `sona_cycle_save` (`name`) *(opt-in)* | both |
| Move a cycle's days (cycle page › Dates: last day of the one under way, first and last day of one ahead) | `work_cycle_save` (`startDay`, `endDay`) | `work cycle-save CYCLE --start --end` | `sona_cycle_save` (`start`, `end`) *(opt-in)* | both |
| Export every cycle as a calendar file (Cycle tab › All cycles › Export calendar…) | `work_cycle_calendar` | `work cycles --ics` | `sona_cycle_list` (`ics`) *(opt-in)* | both |
| Cycles on the project timeline, as bands behind the lanes | `work_cycles` | `work timeline` (`cycles`) | `sona_project_timeline` (`cycles`) | both |
| Roll unfinished work forward | `work_cycle_rollover` | `work cycle-rollover CYCLE` | `sona_cycle_save` (`rollover`) *(opt-in)* | both |
| Start the next cycle today | `work_cycle_start_today` | `work cycle-start-today [CYCLE]` | `sona_cycle_save` (`start_today`) *(opt-in)* | both |
| Put a task in a cycle | `work_update` | `work update --cycle current\|next\|N\|none` | `sona_work_update` (`cycle`) | both |

## Views and filters

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Filter: search, status, priority, owner, project, label, due, cycle, snoozed | `work_list` | `work list --search --status --priority --owner --project --label --due --due-by --cycle --snoozed` | `sona_work_list` | both |
| Filter builder with operators (is, is not, any of, none of, includes all, before, after) | `work_list` (`query.filters`) | `work list --filter JSON` | `sona_work_list` (`filter`) | both |
| Filter in words: typed words become removable condition chips; words it cannot read stay shown. Reads "completed today", "done from Nov 2 to Nov 6", "estimate 4" / "4 points" and "for Dana" too | `work_where` | `work list --where WORDS` | `sona_work_list` (`where`) | both |
| Filter by parent task: only a task's sub-tasks, or work under no parent | `work_list` (`query.filters` field `parent`); Add condition › Parent | `work list --parent TASK\|none` | `sona_work_list` (`parent`) | both |
| Filter by estimate, by who the work is for, or by when it was completed | `work_list` (`query.filters` fields `estimate`, `for`, `completed`); Add condition › Estimate, For, Completed | `work list --for PERSON --completed WHEN`, `work list --filter '{"field":"estimate","op":"is","values":["3"]}'` | `sona_work_list` (`for`, `completed`, `filter`) | both |
| Cycle filter: planned at start (rolled-over work counts) or added after the cycle began | `work_list` (`query.filters` field `cycle_scope`), `work_cycles` (`addedAfterStart`); Filter › Cycle › Planned at start / Added after start | `work list --cycle-scope planned\|added` | `sona_work_list` (`cycle_scope`) | both |
| Order the list: priority, due, updated, created, status, manual | `work_view_save` (`display.orderBy`) | `work list\|ready --order-by ORDER` | `sona_work_list` (`order_by`) | both |
| Move tasks in the manual order: ⌥⇧↑ to the top, ⌥⇧↓ to the bottom (the list switches to Manual) | `work_move` (`place`: top, bottom, before or after a task) | `work move TASK… --top\|--bottom\|--before T\|--after T` | `sona_work_move` *(opt-in)* | both |
| Read a long list a page at a time | none (the window loads every row) | `work list\|ready --limit N --cursor NEXT_CURSOR` | `sona_work_list` (`limit`, `cursor`) | CLI only |
| Only some fields of each task | none (the window shows its display properties) | `work list\|ready --json --fields title,status,…` | `sona_work_list` (`fields`) | CLI only |
| Saved view tabs | `work_views`, `work_list` | `work views`, `work list --view V` | `sona_work_views` *(opt-in)*, `sona_work_list` (`view`) | both |
| Save a view, save edits to it | `work_view_save` | `work view-save NAME [--view V] --filter --layout --group-by --order-by` + filter flags | `sona_work_view_save` *(opt-in)* | both |
| Delete a view | `work_view_delete` | `work view-delete` | `sona_work_view_delete` *(opt-in)* | both |
| Display: show empty groups, shown properties | `work_view_save` | `work view-save NAME --show-empty-groups BOOL --properties id,due,…` | `sona_work_view_save` *(opt-in)* | both |
| Group by cycle, label (a task with two labels sits in both groups) or milestone; drop a card on a group or add from its `+` to take that cycle, label or milestone | `work_view_save` (`display.groupBy`), `work_update` | `work view-save NAME --group-by cycle\|label\|milestone` | `sona_work_view_save` (`group_by`) *(opt-in)* | both |
| Display: show or hide sub-tasks | `work_view_save` (`display.showSubTasks`) | `work view-save NAME --show-sub-tasks BOOL` | `sona_work_view_save` (`show_sub_tasks`) *(opt-in)* | both |
| Board: `+` on a column, hide a column (column menu; Display options › Hidden columns shows them again), swimlanes (Display options › Swimlanes) | `work_view_save` (`display.hiddenColumns`, `display.subGroupBy`), `work_create` | `work view-save NAME --hidden-columns status:done,… --sub-group-by GROUP`; `work create` | `sona_work_view_save` (`hidden_columns`, `sub_group_by`) *(opt-in)*, `sona_work_create` | both |
| Revert unsaved view edits, clear filter chips | none | — | — | window-bound: the window's draft |
| Labels list; new label from a picker | `work_labels`, `work_label_save` | `work labels`, `work label-save NAME` | `sona_work_labels`, `sona_work_label_save` *(opt-in)* | both |
| Rename, recolor, delete a label | — (no UI caller) | `work label-save NAME --label OLD --hue`, `work label-delete` | `sona_work_label_save`, `sona_work_label_delete` *(opt-in)* | CLI only |
| Label scope: tasks, projects or all; a tasks label stays off projects and a projects label off tasks | `work_label_scope` | `work label-scope LABEL all\|tasks\|projects` | `sona_work_label_scope` *(opt-in)* | both |
| Label groups, single-select (a second label from a group replaces the first); retire and restore a label (kept on its tasks, out of pickers) | `work_label_group`, `work_label_retire` (Settings › Tasks); the label picker shows a group as one row; every label write keeps one per group in the core | `work label-group LABEL GROUP\|none`, `work label-retire LABEL [--restore]`; `--labels`/`--add-label` keep one per group | `sona_work_label_group`, `sona_work_label_retire` *(opt-in)* | both |

## People

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| List people (People list; J/K move, Space peeks, Return opens) | `people_list` | `work people [--query]`, `sona --people NAME` | `sona_person_list`, `sona_people` | both |
| Person history, catch-up, what changed, stats | `person_memory` | `sona --person-catch-up ID`, `--person-changes ID --since-utc-ms`, `--person-stats ID` | `sona_person_catch_up`, `sona_person_changes`, `sona_person_stats` | both |
| Person's work and open loops | `planning_person_work`, `work_list`, `open_loops_inbox` | `work list --owner PERSON`, `sona --loops [--mine\|--waiting]` | `sona_work_list` (`owner`), `sona_action_items` | both |
| Person page in one read (summary, meeting links, documents, projects led or joined, contact) | `person_detail`, `planning_projects` (Projects on the Work tab) | `work person NAME` | `sona_person_get` | both |
| Add a person directly | — (UI adds only through speaker naming or split) | `work person-save NAME` | `sona_person_save` | CLI only |
| Rename | `person_rename` | `work person-save NEW --person OLD` | `sona_person_save` | both |
| Other names, role, organization, notes | — | `work person-save NAME --aliases --role --org --notes` | `sona_person_save` | CLI only |
| Add, edit or remove an email | `person_emails_set` (Emails on the Profile tab) | `work person-save NAME --email` | `sona_person_save` (`email`) | both |
| Merge into another person | `person_merge` | `work person-merge A --into B` | `sona_person_merge` *(opt-in)* | both |
| Merge: choose which saved voice survives | `person_merge` (`voiceProfileResolution`) | `work person-merge A --into B --voice into\|from\|both` | `sona_person_merge` (`voice`) | both |
| Split a person | `person_split` | `work person-split NAME --into B\|--new NAME --meetings/--aliases/--emails/--documents` | `sona_person_split` | both |
| Delete a person | `person_delete` | `work person-delete NAME` | `sona_person_delete` | both |
| Regenerate summary | `person_summary_regenerate` | — | — | window-bound: the summary is written by the meeting text engine inside the app; the command-line core mounts no engine |
| Photo from a file; remove photo | `person_avatar_set` | `work person-photo NAME --file PATH\|--remove` | `sona_person_photo` | both |
| Photo from Contacts or a public image URL | `person_avatar_source`, `person_avatar_set` | — | — | UI-only by design: Contacts access and the no-login request toggle |
| Attach a contact | `person_contact_set` | — | — | UI-only by design: only the contact picked in the system picker reaches Sona |
| Remove a contact | `person_contact_set` (null) | `work person-contact-remove NAME` | `sona_person_contact_remove` | both |
| Link, confirm or remove a meeting link | `link_add_manual`, `link_confirm`, `link_remove` | `work person-link NAME MEETING [--confirm\|--remove]` | `sona_person_link` | both |
| First met: read, yes/no | `person_first_met`, `person_first_met_decide` | `work first-met NAME`, `work first-met-answer NAME yes\|no` | `sona_person_first_met`, `sona_person_first_met_answer` | both |
| Name a speaker as a person or a new person; mark unknown | `voice_identify_speaker` | `work speaker-assign MEETING SPEAKER --person P\|--new NAME\|--undo`; speaker ids from `work meeting ID` | `sona_speaker_assign` *(opt-in)* | both: filed as the user's; never remembers the voice, which stays the UI's consent step |
| Confirm a voice suggestion with its same-voice meetings; undo an automatic name | `voice_identify_speaker` (`same_voice`), `meeting_speaker_rename` | `work speaker-voices MEETING`, `work speaker-confirm MEETING SPEAKER`, `work speaker-assign ... --same-voice`, `work speaker-voice-undo MEETING SPEAKER` | `sona_speaker_voices`, `sona_speaker_confirm`, `sona_speaker_assign` (`same_voice`), `sona_speaker_voice_undo` | both |
| Save a voice profile; remove one | `voice_enroll_profile`, `voice_remove_profile` | — | — | UI-only by design: biometric consent |
| Watch a phrase from the person page | `work_watch_save` | `work watch TOPIC --person P` | `sona_work_watch` *(opt-in)* | both |
| Companies list and page | `companies_list`, `organization_detail` | `work companies`, `work company NAME` | `sona_company_list`, `sona_company_get` | both |
| Play "each time it came up" | `conversation_reel_for_loop` | — | — | window-bound: audio output |

## Meetings, notes, search

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| List meetings, time window | `meeting_list` | `work meetings [--since --limit]`, `sona --meetings --last --from --to` | `sona_meeting_list`, `sona_meetings` | both |
| Title search, status filter, folder chips | `meeting_list` | `work meetings --title --status --folder` | `sona_meeting_list` (`title`, `status`, `folder`) | both |
| Open a meeting: notes, ledger, transcript | `meeting_get` | `work meeting ID [--transcript]`, `sona --meeting ID`, `sona --transcript ID` | `sona_meeting_get`, `sona_meeting`, `sona_transcript` | both |
| Wrap up, write the follow-up | `meeting_wrap`, `meeting_follow_up_draft`, `work_follow_up` | `work follow-up MEETING` | `sona_work_follow_up` | both |
| Post the follow-up to a connection | `work_follow_up_post` | — | — | UI-only by design: sends in the owner's name to Slack, and the core allows it only on his keypress |
| Ledger loops: read, done, drop | `meeting_loops`, `meeting_loop_resolve` | `sona --loops`, `sona --loop-resolve LOOP_ID` | `sona_action_items`, `sona_loop_resolve` | both |
| Loop reopen, loop owner | `meeting_loop_reopen`, `meeting_loop_assign` | `work loop-reopen LOOP_ID`, `work loop-owner LOOP_ID --owner PERSON\|none` | `sona_meeting_loop_reopen`, `sona_meeting_loop_assign` *(opt-in)* | both |
| Live card Accept / Decline | `work_triage_accept`, `work_triage_decline` | `work accept`, `work decline` | `sona_work_triage` | both |
| Untracked action: Track, Dismiss, Restore | `planning_track_action`, `planning_dismiss_action`, `planning_restore_action` | `work action-track LOOP_ID --project P`, `work action-dismiss LOOP_ID`, `work action-restore LOOP_ID` | `sona_planning_track_action`, `sona_planning_dismiss_action`, `sona_planning_restore_action` *(opt-in)* | both |
| Who was in the call; place; bookmarks; heard plans | `meeting_call_name_status`, `conversation_place_review`, `meeting_heard_plans` | `sona --roster ID`, `--place ID`, `--bookmarks`, `--plans` | `sona_roster`, `sona_place`, `sona_bookmarks`, `sona_plans` | both |
| Upcoming meetings | `meeting_upcoming_events` | `sona --upcoming` | `sona_upcoming` | both |
| Rename a meeting | `meeting_title_set` | `work meeting-rename` | `sona_meeting_rename` *(opt-in)* | both |
| Folders: create, rename, delete, add a meeting | `meeting_folder_create`, `meeting_folder_rename`, `meeting_folder_delete`, `meeting_folder_add_meeting` | `work folders`, `work folder-new`, `work folder-rename`, `work folder-delete`, `work folder-add`, `work folder-remove` | `sona_meeting_folders`, `sona_meeting_folder_new`/`_rename`/`_delete`, `sona_meeting_file`/`unfile` *(writes opt-in)* | both |
| Delete, trash, restore, delete forever | `meeting_delete`, `meeting_trash_list`, `meeting_trash_restore`, `meeting_trash_delete_forever` | `work meeting-delete`, `work meeting-trash`, `work meeting-restore ENTRY`, `work meeting-delete-forever ENTRY` | `sona_meeting_delete` *(opt-in, destructive)*, `sona_meeting_trash`, `sona_meeting_restore` *(opt-in)*, `sona_meeting_delete_forever` *(opt-in, destructive)* | both (a meeting kept on this Mac stays out of the trash list and answers not_found) |
| Export Markdown/JSON, all as CSV, ledger HTML | `meeting_export`, `meeting_export_all_csv`, `produce_ledger_html` | `work meeting-export --format --out`, `work meetings-csv [--out]`, `work meeting-ledger [--out]` | `sona_meeting_export` *(opt-in)*, `sona_meetings_csv`, `sona_meeting_ledger` | both |
| Write the notes again, catch me up, rewrite with my notes | `meeting_artifacts_regenerate`, `meeting_catch_up`, `reenhance_meeting_with_notes` | — | — | window-bound: runs the notes model through the app's meeting processing |
| Speaker rename or merge, edit a line, transcript corrections | `meeting_speaker_rename`, `meeting_speaker_merge`, `meeting_segment_edit`, `transcript_correction_list`, `transcript_correction_teach`, `transcript_correction_update`, `transcript_correction_remove` | `work speaker-rename MEETING SPEAKER NAME`, `work speaker-merge MEETING SPEAKER --into OTHER`, `work line-edit MEETING LINE TEXT`, `work corrections PERSON`, `work correction-add MEETING LINE --heard --fix`, `work correction-edit PERSON ID`, `work correction-remove PERSON ID` | `sona_speaker_rename`, `sona_speaker_merge` *(opt-in, destructive)*, `sona_transcript_line_edit`, `sona_transcript_corrections`, `sona_transcript_correction_add`/`_edit`, `sona_transcript_correction_remove` *(opt-in, destructive)*; writes opt-in | both (a meeting kept on this Mac answers not_found, and its corrections stay out of the list) |
| Meeting notes and my notes; action item done | `meeting_note_create`, `meeting_note_update`, `meeting_note_delete`, `save_meeting_user_notes`, `set_action_item_done` | `work meeting-notes`, `work meeting-note-add`/`-edit`/`-delete`, `work my-notes`, `work action-done [--undo]` | `sona_meeting_notes`, `sona_meeting_note_add`/`_edit`/`_delete`, `sona_meeting_my_notes`, `sona_meeting_action_done` *(writes opt-in)* | both |
| Ask about one meeting or one folder | `sona_query_pack` | `work ask-pack QUESTION [--folder F]` returns the same evidence pack; the model answer runs in the app's agent panel (window-bound), so the calling agent answers from the pack. One meeting: the app's "ask about this meeting" sends the same unscoped pack, and `work meeting ID --transcript` reads that meeting whole | `sona_ask_pack` | both |
| Documents: add, list, delete | `doc_ingest`, `doc_list`, `doc_delete` | `work docs [--person P \| --meeting ID]`, `work doc-add PATH [--meeting ID]`, `work doc-delete ID` | `sona_documents`, `sona_document_add` *(opt-in)*, `sona_document_delete` *(opt-in)* | both |
| Import a recording or transcript into the library | `meeting_import_recording`, `meeting_import_transcript` | — (`sona -f WAV` transcribes without saving) | — | window-bound: import hands the meeting to the app's transcription and notes processing, which a one-shot CLI would cut off |
| Share links: create, revoke | `cloud_share_create`, `cloud_share_revoke` | — | — | window-bound: a share is sealed with the sync keys and uploaded by the app's cloud sync runtime, which the CLI process does not run |
| One meeting's sync state and its share links | `cloud_sync_meeting_status_get`, `cloud_share_list` | `work sync --meeting ID` | `sona_sync` (`meeting_id`) | both (kind, expiry and state of each share, never its link; a meeting kept on this Mac answers not_found) |
| Import a shared .sona meeting | `cloud_share_import_file` | `work meeting-import FILE [--dry-run]` | `sona_meeting_import` *(opt-in)* | both (opens with the key inside the file, no sync account; the app's list refreshes, the CLI does not open the meeting's window) |
| Create a public browser share | `cloud_browser_share_create` | — | — | UI-only by design: publishes a link |
| Send notes to a connection | `connections_send_notes` | — | — | UI-only by design: sends the notes outside in the owner's name |
| Record, pause, stop a meeting; live help; bookmarks while live | `meeting_start`, `meeting_pause`, `meeting_stop`, `meeting_live_help`, `conversation_bookmark_create` | — | — | window-bound: live microphone and recorder |
| Recording consent panel; always record a series | `meeting_consent_panel_start`, `meeting_series_always_record_set` | — | — | UI-only by design: recording consent |
| Snapshots: delete, list | `meeting_snapshot_delete`, `meeting_snapshot_list` | `work snapshots`, `work snapshot-delete` | `sona_meeting_snapshots`, `sona_meeting_snapshot_delete` *(opt-in, destructive)* | both |
| Automatic snapshots, on-screen text capture | `meeting_snapshot_automatic_set`, `meeting_snapshot_text_set` | — | — | UI-only by design: screen-capture consent |
| Heard plan: add to calendar | `meeting_heard_plan_add_to_calendar` | — | — | UI-only by design: writes to the owner's calendar |
| Heard plan: edit, not now | `meeting_heard_plan_edit`, `meeting_heard_plan_not_now` | `work heard-plans`, `work heard-plan-edit`, `work heard-plan-not-now` | `sona_meeting_heard_plans`, `sona_heard_plan_edit`, `sona_heard_plan_not_now` *(writes opt-in)* | both |
| Notes: list, open, search, versions, diff, related | `scratchpad_list`, `scratchpad_get`, `scratchpad_versions` | `sona --notes`, `--note ID`, `--query T --scope notes`, `--note-versions ID`, `--note-diff ID`, `--note-context T` | `sona_notes`, `sona_note`, `sona_search`, `sona_note_versions`, `sona_note_diff`, `sona_note_context` | both |
| Notes: new, edit, pin | `scratchpad_create`, `scratchpad_update_body`, `scratchpad_set_pinned` | `work note-new TEXT`, `work note-edit ID --body`, `work note-pin ID [--unpin]` | `sona_scratchpad_create`, `sona_scratchpad_update_body`, `sona_scratchpad_set_pinned` *(opt-in)* | both; versions an agent saves carry `actor: agent` and its `--agent` name, shown in the app's history as "by NAME" |
| Notes as documents of a task, project or cycle; `/` blocks and `@` mentions in the editor | "New note" on a task, a project's Sources and a cycle page; the note's "About" section; `scratchpad_cycle_notes`, `scratchpad_link_cycle`, `scratchpad_unlink_cycle`; `planning_link_source` with a note | `work note-new TEXT --task S-N\|--project NAME\|--cycle current\|next\|N`, `work note-link ID --cycle N`, `work note-unlink ID --cycle N`, `work source-link TARGET --note ID`; `sona --note ID` lists `documents` | `sona_scratchpad_create` with `task`/`project`/`cycle`, `sona_scratchpad_link`/`unlink` with `kind: "cycle"`, `sona_planning_link_source` | both; `/` and `@` are editor typing aids, the CLI writes the same Markdown and links directly |
| Notes: delete, restore a version | `scratchpad_delete`, `scratchpad_restore_version` | `work note-delete ID`, `work note-restore ID VERSION` | `sona_scratchpad_delete` *(opt-in, destructive)*, `sona_scratchpad_restore_version` *(opt-in)* | both |
| Notes: link a meeting, person or note as a source | `scratchpad_link` | `work note-link ID --meeting\|--person\|--note ID\|--cycle N` | `sona_scratchpad_link` *(opt-in)* | both |
| Notes: unlink a source, subjects | `scratchpad_unlink`, `scratchpad_subjects`, `scratchpad_subject_create`, `scratchpad_subject_note` | `work note-unlink ID --meeting\|--person\|--note ID`, `work subjects`, `work subject-new TITLE`, `work subject-note SUBJECT NOTE [--current\|--remove]` | `sona_scratchpad_unlink`, `sona_scratchpad_subjects`, `sona_scratchpad_subject_create`, `sona_scratchpad_subject_note` *(opt-in)* | both |
| Notes: synthesis | `scratchpad_synthesize` | — | — | window-bound: runs the local model inside the app |
| Palette corpus search; events | `sona_query_search`, `sona_query_events` | `sona --query T [--scope]`, `sona --events` | `sona_search`, `sona_events` | both |
| One search over tasks and meeting text; a task matches its title, details or comments; meetings kept on this Mac, and tasks drawn from them, stay out of agent results | `sona_query_search` (scope `all` or `tasks`; the palette lists task hits its own list missed) | `sona --query T [--scope tasks]`; `work list --search T` also reads comments | `sona_search` (`scope`: `all` or `tasks`), `sona_work_list` (`search`) | both |
| Said lines, on-screen text, in-meeting search | `meeting_said_search`, `meeting_snapshot_text_search`, `meeting_search` | `work said`, `work screen-text`, `work meeting-search` | `sona_said_search`, `sona_screen_text_search`, `sona_meeting_search` | both |
| Dictation history: search, totals | `search_history_entries`, `get_history_stats` | `sona --query T --scope dictations`, `sona --stats` | `sona_search`, `sona_stats` | both |
| Dictation history: list, save, delete | `get_history_entries`, `toggle_history_entry_saved`, `delete_history_entry` | `work dictations [--search T] [--before ID]`, `work dictation-save ID [--unsave]`, `work dictation-delete ID` | `sona_dictations`, `sona_dictation_save` *(opt-in)*, `sona_dictation_delete` *(opt-in, destructive)* | both (reading needs the same External access grant as `--query --scope dictations`) |
| Dictation history: process again, transcribe again | `reprocess_history_entry`, `retry_history_entry_transcription` | — | — | window-bound: both rerun the app's speech and cleanup models on the recording, which the CLI process does not load |
| Evening digest settings; evening card; filing | `change_meeting_digest_enabled_setting`, `change_meeting_digest_minute_of_day_setting`, `home_evening`, `home_evening_write`, `filing_decide` | `work setting evening-digest true\|false`, `work setting evening-digest-time HH:MM`, `work evening`, `work filing ID [--decline]` | `sona_setting`, `sona_evening`, `sona_filing` | both (setting and its time, card, filing; the card leaves out lines, recordings and suggestions from a meeting kept on this Mac); writing the lines now is window-bound: the local AI model runs inside the app |
| Close a task when its sub-tasks are done (off by default) | `change_work_close_parents_setting` (Settings › Agents › Changes to your tasks) | `work setting auto-close-parents true\|false` | `sona_setting` (`auto-close-parents`) | both |
| Weekly project digest in the inbox: each planned, active or paused project's status, latest update, tasks done that week and risks (on by default, Monday 9:00; work from meetings kept on this Mac left out) | Inbox row "Weekly project digest", Return or Space opens it beside the list (`work_inbox`); Settings › Tasks › Weekly digest (`change_work_digest_setting`) | `work digest [--dry-run]`, `work inbox`; `work setting project-digest true\|false`, `work setting project-digest-day monday…sunday` | `sona_work_digest` (`dry_run`) *(opt-in)*, `sona_work_inbox`; `sona_setting` (`project-digest`, `project-digest-day`) | both |
| Recording and meeting retention | `update_recording_retention_period`, `meeting_retention_set` | — | — | UI-only by design: privacy retention |
| Play a turn, listen with context, receipts reel | `conversation_audio_clip`, `conversation_reel_for_loop` | — | — | window-bound: audio output |

## Chat (Talk to Sona)

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Send a turn, stop it, retry | `agent_panel_send_turn`, `agent_panel_cancel_turn` | — (`work ask` answers from local lines) | `sona_work_ask` (local lines) | window-bound: the chat runtime and its relay connection live in the app process |
| New chat, open a past chat | `agent_chat_new`, `agent_chat_open`, `agent_chat_history_list` | — | — | window-bound: chats are held by the app's chat runtime |
| Model and thinking effort | `agent_panel_select_model` | — | — | window-bound: the choice is state of the app's chat runtime, not a setting |
| Apply, undo or dismiss a proposed change or action | `agent_panel_apply_change`, `agent_panel_undo_change`, `agent_panel_apply_action`, `agent_panel_dismiss_action` | — (some actions by hand: `sona --loop-resolve`, `work person-save`) | — | UI-only by design: the owner's yes to a change the AI proposed |
| Review an action that sends outside (email draft, Slack post) | `connections_edit_chat_action` | — | — | UI-only by design: consent to send |
| Allow meeting intelligence (quotes go to the server) | `change_meeting_remote_intelligence_enabled_setting` | — | — | UI-only by design: consent |
| Open a cited meeting or note | `sona_open_link` | `sona --meeting ID`, `sona --note ID` | `sona_meeting`, `sona_note` | both |
| Agent panel public identity | `agent_panel_public_identity` | `sona --agent-panel-public-identity` | — | both |
| Attach a window or screenshot | none until send | — | — | UI-only by design: screen-capture permission |
| Voice conversation, spoken answers | `chat_voice_start`, `chat_voice_stop`, `chat_voice_speak` | — | — | window-bound: microphone and audio output |

## Settings

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Models list | `get_available_models` | `sona --list-models [--json]` | — | both |
| Compute devices | — | `sona --list-devices` | — | CLI only |
| Use, download, delete, rescan a model | `set_active_model`, `download_model`, `delete_model`, `rescan_local_models` | — (`--model` applies to `-f` only) | — | window-bound: the app's model manager downloads and loads models in its process |
| Dictation and sound toggles (push-to-talk, languages, translate to English, English spelling, sounds, custom sound files, mute, quiet speech, command mode and where its answers go, overlay, the pill between dictations and its edge) | `change_ptt_setting`, `change_dictation_languages_setting`, `change_translate_to_english_setting`, `change_english_spelling_setting`, `change_audio_feedback_setting`, `check_custom_sounds`, `change_mute_while_recording_setting`, `change_command_answers_in_chat_setting`, `change_overlay_style_setting`, `set_hud_pill_enabled`, `set_hud_pill_position` | `work settings`; `work setting push-to-talk\|dictation-languages\|translate-to-english\|english-spelling\|audio-feedback\|audio-feedback-volume\|sound-theme\|mute-while-recording\|quiet-speech\|command-mode\|command-answers-in-chat\|overlay\|hud-pill\|hud-pill-position VALUE` | `sona_settings`, `sona_setting` | both (custom sound files are read-only: `work settings` says whether each is there) |
| Microphone and channel | `set_selected_microphone`, `set_selected_channel` | `work settings` lists microphones and the chosen one | `sona_settings` | window-bound (choosing): the app restarts its live audio stream |
| Shortcuts: reset | `reset_binding` | `work setting-reset shortcut ID` | `sona_setting_reset` *(opt-in)* | both |
| Shortcuts: record a new key | `start_handy_keys_recording`, `change_binding` | — | — | window-bound: live keyboard |
| Modes: new, edit, activate, reorder, delete | `upsert_mode`, `set_active_mode`, `reorder_modes`, `delete_mode` | `work mode-save --mode-json`, `work setting mode ID`, `work mode-reorder`, `work mode-delete` | `sona_mode_save`, `sona_setting`, `sona_mode_reorder`, `sona_mode_delete` *(opt-in)* | both |
| Mode rule from the front app or website | `capture_mode_activation_rule`, `capture_mode_website_activation_rule` | — | — | window-bound: reads the frontmost window |
| Mode rule: remove | `remove_mode_activation_rule`, `remove_mode_website_activation_rule` | `work mode-rule-remove MODE --app BUNDLE_ID \| --website HOST [--match exact\|subdomains]` | `sona_mode_rule_remove` *(opt-in, destructive)* | both |
| Cleanup provider, base URL, model | `set_post_process_provider`, `change_post_process_base_url_setting`, `change_post_process_model_setting` | `work setting cleanup\|cleanup-provider\|cleanup-model\|cleanup-base-url VALUE [--provider ID]` | `sona_setting` | both |
| Local cleanup: download, use, remove | `download_local_cleanup`, `select_local_cleanup`, `remove_local_cleanup` | — | — | window-bound: the app's local cleanup manager downloads and loads the model |
| Provider API keys | `set_provider_secret`, `delete_provider_secret`, `verify_stt_provider_secret` | — | — | UI-only by design: secrets |
| Cloud speech or cleanup consent | `accept_cloud_stt_provider_consent`, `accept_post_process_provider_consent` | — | — | UI-only by design: consent |
| Dictation prompts: add, edit, use, delete | `add_post_process_prompt`, `update_post_process_prompt`, `set_post_process_selected_prompt`, `delete_post_process_prompt` | `work prompt-save NAME --prompt T [--id]`, `work setting dictation-prompt ID`, `work prompt-delete ID` | `sona_prompt_save`, `sona_setting`, `sona_prompt_delete` *(opt-in)* | both |
| Saved prompts: save, run, delete | `saved_prompt_save`, `saved_prompt_run`, `saved_prompt_delete` | `work saved-prompts`, `work saved-prompt-save NAME --body --about`, `work saved-prompt-delete ID` | `sona_saved_prompts`, `sona_saved_prompt_save`, `sona_saved_prompt_delete` *(opt-in)* | both (list, save, delete); run is window-bound: the AI runs inside the app |
| Saved prompts: past answers about a meeting, person or series | `saved_prompt_runs` | `work saved-prompt-runs --meeting ID \| --person P \| --series MEETING_ID` | `sona_saved_prompt_runs` | both (a meeting kept on this Mac answers not_found; a person who was in one has their answers left out) |
| Vocabulary, snippets, rewrites, emoji, CSV apply | `list_vocabulary_entries`, `update_vocabulary_entries`, `list_snippets`, `upsert_snippet`, `set_snippet_enabled`, `delete_snippet`, `set_snippets_enabled`, `get_text_replacements`, `save_text_replacements`, `update_text_replacements_enabled`, `reset_text_replacements`, `update_emoji_replacements`, `update_emoji_replacements_enabled`, `update_spoken_edits_enabled`, `apply_vocabulary_csv`, `add_vocabulary_correction` | `work vocabulary [--mode ID]`, `work vocabulary-add`, `work vocabulary-remove SPOKEN [--mode ID]`, `work vocabulary-csv FILE`, `work snippet-save TRIGGER --text T [--id ID] [--off]`, `work snippet-enable ID [--off]`, `work snippet-delete ID`, `work rewrite-save SPOKEN WRITTEN [--off]`, `work rewrite-enable SPOKEN [--off]`, `work rewrite-delete SPOKEN`, `work emoji-save SPOKEN EMOJI`, `work emoji-delete SPOKEN`, `work setting snippets\|text-replacements\|spoken-edits\|emoji`, `work setting-reset replacements` | `sona_vocabulary`, `sona_vocabulary_add`, `sona_vocabulary_remove` *(opt-in)*, `sona_vocabulary_csv`, `sona_snippet_save`, `sona_snippet_enable`, `sona_snippet_delete` *(opt-in)*, `sona_rewrite_save`, `sona_rewrite_enable`, `sona_rewrite_delete` *(opt-in)*, `sona_emoji_save`, `sona_emoji_delete` *(opt-in)*, `sona_setting`, `sona_setting_reset` *(opt-in)* | both |
| Words heard in meetings that the vocabulary lacks | `vocabulary_candidates` | `work heard-words` | `sona_heard_words` | both (meetings kept on this Mac are left out, counts included) |
| Writing samples cleanup matches your voice to | `get_persona_samples`, `save_persona_samples` | `work settings` (`persona_samples`), `work persona-sample-save TEXT [--id ID]`, `work persona-sample-delete ID` | `sona_settings`, `sona_persona_sample_save`, `sona_persona_sample_delete` *(opt-in)* | both |
| Workflows: list, runs, enable | `workflows_list`, `workflow_runs`, `workflow_set_enabled` | `work workflows [--runs]`, `work workflow-enable ID [--off]` | `sona_workflows`, `sona_workflow_enable` | both |
| Workflows: runs per day | `workflow_run_trend` | `work workflows --trend` | `sona_workflows` (`trend`) | both |
| Meeting prep preferences | `meeting_prep_preferences_get`, `meeting_prep_preferences_set` | `work settings`, `work setting meeting-prep JSON` | `sona_settings`, `sona_setting` | both (the mail and web research switches stay UI-only by design: consent) |
| Keyword trackers; series template and digest | `list_keyword_trackers`, `save_keyword_trackers`, `meeting_series_template_set`, `meeting_series_digest_set` | `work setting keyword-trackers JSON`, `work series MEETING_ID`, `work series-template MEETING_ID TEMPLATE`, `work series-digest MEETING_ID [--off]` | `sona_setting`, `sona_series`, `sona_series_template`, `sona_series_digest` | both |
| Learn or forget my voice | `voice_user_learn`, `voice_user_forget` | — | — | UI-only by design: biometric consent |
| Web research key | `meeting_web_research_key_set`, `meeting_web_research_key_remove` | — | — | UI-only by design: secret |
| Agents: external access, mutations, autonomy, folders, permission rules, replies | `set_agent_bridge_master`, `change_work_agent_changes_setting`, `authorize_agent_bridge_project`, `create_agent_bridge_permission_rule`, `confirm_agent_bridge_reply` | — | — | UI-only by design: the gate the CLI itself sits behind |
| Agent panel pairing | `set_agent_panel_pairing`, `clear_agent_panel_pairing` | — | — | UI-only by design: token |
| Agent panel connection test | `agent_panel_test_connection` | — | — | window-bound: tests the app's live relay connection |
| Connections: connect, allow sending, automatic sends, disconnect | `connections_save`, `connections_rule_save`, `connections_disconnect` | — | — | UI-only by design: token and consent to send |
| Connections: list, test, undo a send | `connections_snapshot`, `connections_test`, `connections_undo_send` | `work connections` (no tokens) | `sona_connections` | both (list); test and undo are UI-only by design: they use the connection's token from the keychain |
| Sync: where it stands, the service, each meeting's state | `cloud_sync_overview_get`, `cloud_sync_service_status`, `cloud_sync_meeting_status_list` | `work sync` | `sona_sync` | both (read-only, no keys or tokens; meetings kept on this Mac are left out; an error the running app holds in memory shows only in its window) |
| Sync: set up, pair, approve a device, recover | `cloud_sync_bootstrap`, `cloud_sync_pairing_offer`, `cloud_sync_pairing_approve`, `cloud_sync_recover` | — | — | UI-only by design: vault keys |
| Sync: pause, resume, retry, resolve a conflict | `cloud_sync_pause`, `cloud_sync_resume`, `cloud_sync_retry`, `cloud_sync_conflict_resolve` | — | — | window-bound: the sync engine runs inside the app |
| About and debug: language, theme, window material, update check, what's new, log level, debug mode, experimental features | `change_app_language_setting`, `change_theme_setting`, `change_appearance_material_setting`, `change_update_check_enabled_setting`, `check_for_updates`, `change_show_whats_new_on_update_setting`, `set_log_level`, `change_debug_mode_setting`, `change_experimental_enabled_setting` | `work setting app-language\|theme\|appearance-material\|update-check\|whats-new\|log-level\|debug-mode\|experimental VALUE` (`sona --debug` at launch) | `sona_setting` | both; checking for an update now is window-bound: the updater installs into the running app |
| App data and log folders, portable install | `get_app_dir_path`, `get_log_dir_path`, `is_portable` | `work settings` (`about`) | `sona_settings` | both |
| Open app data, logs, recordings folders | `open_app_data_dir`, `open_log_dir`, `open_recordings_folder` | — | — | window-bound: Finder |
| Launch at login; project root folder | `change_autostart_setting`, `change_dictation_project_root_setting` | — | — | UI-only by design: system login item and folder grant |

## Automations

Automations: saved agent workflows that run the in-app agent with a prompt
when a schedule or event fires. Meeting commitments, called loops, are under
Meetings. The core keeps its loop names (`work_loop*`, `planning_loop*`).

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Open Automations: each automation, on or off, its trigger, next and last run | `work_loops` | `work automation-list` | `sona_automation_list` | both |
| Open an automation: prompt, trigger, conditions, tools, versions, runs | `work_loop` | `work automation-show AUTOMATION` | `sona_automation_show` | both |
| New automation, edit an automation: name, prompt, schedule or event, conditions, tools | `work_loop_save` | `work automation-save NAME [--automation] [--prompt] (--schedule EVERY --first-run "YYYY-MM-DD HH:MM" [--timezone] [--days] \| --on EVENT [--to] [--label]) [--if-label] [--if-project] [--if-status] [--tools]` | `sona_automation_save` | both |
| Run now | `work_loop_run` | `work automation-run AUTOMATION` | `sona_automation_run` *(opt-in)* | both |
| On/off switch | `work_loop_enable` | `work automation-enable AUTOMATION`, `work automation-disable AUTOMATION` | `sona_automation_enable`, `sona_automation_disable` | both |
| Restore an earlier version | `work_loop_restore_version` | `work automation-restore-version AUTOMATION VERSION` | `sona_automation_restore_version` | both |
| Delete an automation | `work_loop_delete` | `work automation-delete AUTOMATION` | `sona_automation_delete` *(opt-in)* | both |
| Tool picker: the tools an automation may use | `work_loops` (`tools`) | `work automation-list` (`tools`); `automation-save --tools` refuses a name Sona does not offer | `sona_automation_list` (`tools`) | both |
| An automation proposes into Triage: with "Propose new tasks for Triage" or "Propose task changes for Triage" ticked in its tools, a run leaves new tasks and field changes waiting in Triage, never applied; the run's output lists them | `work_loop_save` (`tools`), `work_loop` (run output) | `work automation-save --tools sona_work_create,sona_work_update`; `work automation-show` (run output) | `sona_automation_save` (`tools`), `sona_automation_show` | both |

## Rest (Home, Today, rituals, brief, watches)

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Start, stop, cancel dictation | `hud_toggle_recording`, `finish_recording`, `cancel_operation` | `sona --toggle-transcription`, `sona --cancel`, `sona --toggle-post-process` | — | both |
| Transcribe a WAV headless | — | `sona -f WAV [--model --json --diagnose --repeat]` | — | CLI only |
| Export meetings' audio and speakers for analysis | — | `sona --export-meetings DIR` | — | CLI only |
| Record the screen; import audio from Finder | recorder sheet, file picker | — | — | window-bound: screen capture, file picker |
| Today strip (meetings today, unnamed speakers, owed) | `home_today` | `work today` | `sona_work_today` *(opt-in)* | both |
| Morning plan: read focus | `work_day` | `work list`, `work show` print `focus` | `sona_work_list` | both |
| Morning plan: save up to 3 focus tasks | `work_day_plan` | `work focus TASK... [--day]`, `work focus --clear`; read with `work day [DAY]` | `sona_work_day_plan`, `sona_work_day` *(opt-in)* | both |
| Stale task: still stands (Keep) | `work_keep` | `work keep TASK...` | `sona_work_keep` *(opt-in)* | both |
| Stale tasks list (untouched 30 days) | `work_list` (computed in Swift) | `work list --stale` | `sona_work_list` (`stale`) | both |
| Evening: slipped to tomorrow, this week, drop | `work_update` | `work update --due`, `work update --status canceled` | `sona_work_update` | both |
| Person brief and talking points | `work_brief` (person), `work_talking_point_add`, `work_talking_point_done` | `work brief PERSON`, `work talking-point PERSON TEXT`, `work talking-point-done ID [--undo]` | `sona_work_brief`, `sona_work_talking_point_add`, `sona_work_talking_point_done` *(opt-in)* | both |
| Meeting brief for a calendar event | `work_brief` (event) | — | — | window-bound: calendar runtime |
| Watches: list, add, stop | `work_watches`, `work_watch_save`, `work_watch_remove` | `work watches`, `work watch [--dry-run]`, `work unwatch [--dry-run]` | `sona_work_watches`, `sona_work_watch` (`dry_run`) *(opt-in)* | both |
| Watch hit: make a task | `work_watch_task` | `work watch-task WATCH --line N` | `sona_work_watch_task` *(opt-in)* | both |
| Learning suggestions: yes, no thanks | `learning_suggestions`, `learning_decide` | — | — | UI-only by design: the owner's yes to what Sona learned from his own dictation and meetings, which quotes text that never leaves this Mac and rewrites dictation settings |
| Stats trend | `get_history_trend`, `meeting_trend` | `work trend [--range 7\|30\|180]` | `sona_work_trend` *(opt-in)* | both |
| Count words per app | `change_count_words_per_app_setting` | — | — | UI-only by design: a privacy toggle that records which app dictation goes to |
| Capture mode chip | `get_modes`, `set_active_mode` | `work settings` (modes), `work setting mode ID` | `sona_settings`, `sona_setting` | both |

## Missing

None. Checked 2026-11-11: every `work` verb this table names is in
`sona work --help`, and every MCP tool it names is in `TOOLS` in
`tools/sona-mcp/src/tools.ts`. The `missing` rows in the inventories beside
this file are their state when they were written; this table is current.

MCP gaps where the CLI already has the verb: `work snooze` (reachable as
`sona_work_update` `snooze_until`), `work project-update-draft` (draft
without posting), and every top-level `sona` dictation or file flag.
Planning tools for cycles, labels, views, watches, deletes and archive are
opt-in only.

## Count of rows per status

| status | rows |
|---|---|
| both | 218 |
| CLI only | 20 |
| missing | 0 |
| UI-only by design | 28 |
| window-bound | 34 |
| total | 300 |

Rows are grouped: one row can cover several buttons that call the same
method. The inventories beside this file list every button.
