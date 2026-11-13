# Linear issues must-have list vs Sona HEAD

Spec: /Users/aktan/Developer/sona-linear-spec/LinearIssues-report.md, "Must-have for Sona, ranked by value" (rows 1-20). Checked against HEAD 6f89dc7b (branch work/people-projects), code only.

Short paths:
- UI: macos/Sona/*.swift (TDV = TaskDetailView.swift, WK = WorkKeyboard.swift)
- CLI: src-tauri/src/query/external/tracker.rs (TR); verb enum TR:83-170
- MCP: tools/sona-mcp/src/tools.ts (MCP)

Whole-codebase greps for `parent`, `comment`, `attachment`, `remind`, `subscri`, `favorite`, `template` in TR, MCP, src-tauri/src/planning/work/items.rs and types.rs return 0 hits. Sub-issues, comments, file attachments, reminders, subscriptions, favorites and task templates do not exist in the data model, so every surface is missing for them.

| # | Feature | UI | CLI | MCP | What is missing |
|---|---|---|---|---|---|
| 1 | Issue detail page | partial. Full page with back link and Esc (TDV:6-87), K/J stepper (TDV:90-110), inline title (TDV:138), details field (TDV:183-217), property sidebar: status, priority, owner, agent, due, repeat, cycle, time, project, labels, source meeting, created/updated (TDV:755-887), relations (TDV:518), activity (TDV:673). Row click opens it (WK:662-670) | built: `work show` (TR:95, ShowArgs TR:209) prints relations, evidence, activity (TR:1264) | built: `sona_work_get` → `work show --json` (MCP:899-910) | no parent, sub-issue list or comment box. Details are a plain TextField, so there is no Markdown rendering (TDV:190). No side-panel mode |
| 2 | Activity feed with actor and source meeting first | built: DetailActivity sorted by time (TDV:673-718), actor You/Sona/agent name (TDV:951-958), "heard this in a meeting" as the created entry (TDV:964-965), every field kind (TDV:966-1021) | built: `work show` activity (TR:1264, 1311-1314, 3823-3836) | built: via `sona_work_get` (MCP:899) | the actor does not say whether an agent came in through MCP or the CLI (only the agent's name) |
| 3 | Triage queue with keys and AI suggestions | partial. 1/2/3/H (WK:558-561; TriageScreen.swift:186-217), source splits with Tab (WK:482-485), suggestions with reason and quote (TriageScreen.swift:349-352) | partial: `work accept/decline/duplicate/snooze` (TR:105-115) | partial: `sona_work_triage` accept/decline/duplicate (MCP:1082-1150); snooze via `sona_work_update` snooze_until | decline takes no reason (TasksArgs TR:306-312 has none). Suggestion kinds are only owner, due, project, duplicate (WorkTypes.swift:539); related, labels and person-other-than-owner are missing. Per-suggestion accept/dismiss exists only as a whole accept with `--ignore-suggestions`. CLI/MCP have no source filter |
| 4 | Command menu (Cmd K), contextual, full verb list in CLI and MCP | partial. Cmd K palette (SonaApp.swift:42; Panels.swift:1006) with a Task/Tasks section built from the selected tasks (Panels.swift:1195-1198) | partial: get/update/claim/done/create/relate (TR:95-119) | partial: get, update, claim, done, create, relate, unrelate, archive, restore (MCP:899-1155) | set-parent, comment, remind and delete verbs are missing everywhere, because the features are missing |
| 5 | Relations | partial. Add blocks / blocked by / related and remove (TDV:518-573, 624-662); duplicate via triage key 3 (WK:560) | built: `work relate`/`unrelate` (TR:117-119), `work duplicate` (TR:109) | built: `relate`/`unrelate` (MCP:315, 1154-1155), duplicate in triage (MCP:1095) | no M B / M X / M R keys (WK:508-571 has no "m"). No auto-related from `S-12` mentions. A done blocker stops counting (items.rs:814-818) but the link does not turn into related. No duplicate banner on the detail page (only an activity line, TDV:996-997) |
| 6 | Sub-issues | missing | missing | missing | no parent field in the schema or types (grep `parent` = 0 in items.rs, types.rs, TR, MCP). Needs parent column, sub-issue list, progress count, inheritance, set/remove parent everywhere |
| 7 | Comments | missing | missing | missing | no comment storage (grep `comment` = 0). Needs table, Cmd Enter composer, edit/delete/link, threads with resolve, MCP `comment` tool |
| 8 | Bulk selection and bulk edit | built: X toggle (WK:540-543), Cmd A (WK:495-498), Shift ↑/↓ extend (WK:468-475), Shift-click and Cmd-click (WK:662-670), property keys act on the selection (WK:551-563), bulk bar (BulkActionBar.swift:19-33), right-click menu on selection (TasksScreen.swift:426; WK:673-674) | built: verbs take several refs (TasksArgs TR:306-309; Update "several tasks take the same change", TR:99) | built: `sona_work_update` takes several tasks (MCP:974) | nothing for the must-have |
| 9 | Peek (Space) | built: Space toggles peek (WK:544-547; TaskPeek.swift:6) | n/a (`work show`) | n/a (`sona_work_get`) | nothing |
| 10 | Due dates and reminders | partial. D due picker (WK:555; TDV:792-798), Overdue group and reschedule-all (WorkListing.swift:57-86; WorkComponents.swift:422-463), Shift D reschedules overdue (WK:513-515), H snooze (WK:561), due-day notifications open the task (FollowThrough.swift:129-153; core reminders src-tauri/src/planning/work/follow.rs:246-285) | partial: `--due`, `--due overdue` filter (TR:852), `work snooze --until DAY` (TR:318-324) | partial: due and search via list scope flags (MCP:281-283), snooze_until on update | no "Remind me" with a natural-language time, no reminder banner on the task, snooze takes a day not a time in CLI/MCP. Shift D means reschedule-overdue, not the due picker. Red/orange/gray due tone not confirmed in the row code |
| 11 | Attachments and links | partial. Evidence links and quotes (TDV:272-300, 424), source meeting row with timestamp (TDV:868-880), quick entry keeps the front page's link (QuickEntry.swift:117-132) | partial: `work done --evidence URL` (TR:279-282); `create` has no link option | partial: `sona_work_done` evidence (MCP:1051) | no general attachment model (URL, title, subtitle, metadata), no Ctrl L add link, no file drag-and-drop upload, no way to add a link without closing the task |
| 12 | Copy actions | missing. The only task-area pasteboard writes are the delegate prompt (WorkStore.swift:715) and the nudge draft (FollowThrough.swift:394-395); `#N` is selectable text only (TDV:20-23) | partial: `work export --md DIR` writes Markdown files (TR:133, 406-412) | missing | copy ID, copy `sona://` task link, copy as Markdown and pre-filled create link are not built. No `sona://` task route found in TR or the task UI files |
| 13 | Delete with 30-day restore, Archive page | partial. Cmd Delete archives (WK:479-481), Cmd Z undoes (WK:491-494) | partial: `work archive`/`restore` (TR:111-113), `list --archived` (TR:517-519) | partial: archive/restore (MCP:1152-1153), list `archived` (MCP:880) | no delete or 30-day purge (no delete verb for tasks). No Archive page in the UI; restore only through undo. No `#` restore key |
| 14 | Inbox for agent, automation, reminder and due events | missing. Home "Needs you" shows open loops only (OverviewStore.swift:246); due reminders are macOS notifications only | missing | missing | no Inbox store, read/unread, snooze or delete. No event feed for agent changes |
| 15 | Search | partial. Cmd K palette searches meetings and tasks (AppModel.swift:510-513; Panels.swift:1006), task filter search field (TaskFilters.swift:130-142) | partial: `list --search` (TR:503) | partial: `sona_search` for meetings (MCP:352), work list search (MCP:283) | `/` opens the shortcut sheet, not search (WK:520, 566). No `O I` recent tasks. Search does not cover comments (none exist) |
| 16 | Subscribe (Shift S) | missing | missing | missing | no watch-a-task model; existing "watch" is for spoken phrases (TR:144-148), not tasks |
| 17 | Description version history with restore | partial. Activity logs "edited the details" (TDV:985-986) | partial: same activity line in `work show` | partial: same via `sona_work_get` | old text is not kept or shown, no restore. (Notes have versions, MCP:717-732, but tasks do not) |
| 18 | Recurring tasks | partial. Composer reads "every monday" (TaskComposer.swift:251-296); detail shows the rule and kept-of count but cannot edit it (TDV:799-815) | built: `create`/`update --repeat` (TR:546) | built: `repeat` field (MCP:235-238, 262) | UI cannot change or stop a repeat after creation |
| 19 | Favorites in the sidebar | missing | missing | missing | nothing built (grep `favorite` = 0) |
| 20 | Task templates (Option C) | missing (templates exist only for meetings, MeetingTemplatesStore.swift) | missing | missing | no task template model with preset properties and sub-issues |

Not checked as must-haves (the report lists them under "Skip"): teams, sharing, SLAs, triage responsibility, email intake, customer requests, form templates, non-macOS notification channels.

## Build queue (missing or partial only, ranked by the report's value order)

1. Sub-issues: parent column, set/remove parent in UI/CLI/MCP, sub-issue list with progress on the detail page, inherit priority and project. L
2. Comments: storage, Cmd Enter composer on the detail page, edit/delete/copy link, `work comment` and an MCP comment tool for agent progress. Threads and resolve later. L
3. Detail page polish: Markdown rendering of details, duplicate banner. S
4. Triage: decline reason (UI, `work decline --reason`, MCP), more suggestion kinds (related, labels), dismiss one suggestion. M
5. Relations: M B / M X / M R keys, done blocker becomes related, auto-related from `S-N` mentions. M
6. Copy actions: copy ID, `sona://` task link (with a route), copy as Markdown, pre-filled create link. S
7. Reminders: "Remind me" with a time (snooze to a time in CLI/MCP), banner on the task, macOS notification. M
8. Attachments: general link attachments with title and metadata, Ctrl L, `create --link`, file drag-and-drop. M
9. Delete with 30-day restore and an Archive page with restore. M
10. Inbox for agent, reminder and due events with read, snooze, delete. L
11. Search: `/` focuses search, `O I` recent tasks. S
12. Subscribe to a task and notify on agent changes (depends on Inbox). M
13. Details version history with restore. M
14. Edit or stop a repeat from the detail page. S
15. Favorites in the sidebar. M
16. Task templates with preset properties (and sub-issues once item 1 lands). M
